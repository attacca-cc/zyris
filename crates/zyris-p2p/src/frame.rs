//! QUIC hands us a byte stream, but zyris wants messages. This puts a length prefix on top of
//! a single bi-stream.
//!
//! ```text
//! [u8 kind][u32 BE len][payload …]      kind 0 = Binary, 1 = Text
//! ```
//!
//! **The `len` cap bounds a declared length, not resident memory — it is not the memory
//! defense it looks like.** `vec![0u8; len]` for a length up to `MAX_FRAME` costs only a few
//! KB of RSS up front, not `len` bytes: glibc's `alloc_zeroed` hands back lazy, copy-on-write
//! zero pages, and nothing is actually resident until the receiver writes into them one
//! `read_exact` at a time. So a peer declaring an oversized length does not spike our memory
//! the way this used to claim; what the cap actually buys is rejecting a peer that is not
//! speaking real zyris (`decode_header` refuses the length before any payload byte is read),
//! not bounding allocation cost.
//!
//! **The real exposure is time, not memory.** The protocol's credit accounting only binds what
//! the sender is allowed to send — the receiver keeps no ledger of its own (`CreditViolation`
//! is defined but never used) — so it does nothing to stop a peer that ignores the protocol.
//! A peer that opens a connection and then sends a handful of bytes, or none at all, holds a
//! task, a connection, and a socket open indefinitely: iroh's own keepalives run regardless, so
//! there is no idle timeout to fall back on. That is a deadline problem for whoever drives the
//! read loop on top of this framing, not something a length cap can fix.

use zyris_proto::WireMessage;

pub const MAX_FRAME: usize = 16 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("unknown frame kind: {0}")]
    UnknownKind(u8),
    #[error("{0} bytes exceeds the {MAX_FRAME}-byte cap")]
    TooLarge(usize),
    #[error("text frame is not valid UTF-8")]
    NotUtf8,
}

/// Frame one message for the wire.
///
/// Infallible on purpose: everything it encodes is a message we produced ourselves, so there is
/// no untrusted input to reject here. **`MAX_FRAME` is enforced on the receiving side only** —
/// hand this a payload above the cap and it encodes happily, and the peer refuses it in
/// [`decode_header`]. That asymmetry is deliberate. The cap exists to stop a peer from making us
/// allocate, not to police our own output, and zyris keeps its own frames well under it
/// (`max_control_frame` is 8 MiB, `max_chunk` 256 KiB).
pub fn encode(msg: &WireMessage) -> Vec<u8> {
    let (kind, payload): (u8, &[u8]) = match msg {
        WireMessage::Binary(b) => (0, b),
        WireMessage::Text(t) => (1, t.as_bytes()),
    };
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(kind);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// Reads the fixed 5-byte header off the wire and validates it before the caller reads a
/// single payload byte, so an oversized or malformed declaration is rejected up front instead
/// of after we have already buffered it.
pub fn decode_header(buf: &[u8; 5]) -> Result<(u8, usize), FrameError> {
    let kind = buf[0];
    if kind > 1 {
        return Err(FrameError::UnknownKind(kind));
    }
    let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len));
    }
    Ok((kind, len))
}

/// Turns a validated `(kind, payload)` pair back into the `WireMessage` the caller wanted.
/// Split from [`decode_header`] because the caller reads exactly `len` payload bytes in
/// between: the header tells it how much to read, this turns what it read into a message.
pub fn body(kind: u8, bytes: Vec<u8>) -> Result<WireMessage, FrameError> {
    match kind {
        0 => Ok(WireMessage::Binary(bytes.into())),
        1 => String::from_utf8(bytes).map(WireMessage::Text).map_err(|_| FrameError::NotUtf8),
        other => Err(FrameError::UnknownKind(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zyris_proto::WireMessage;

    #[test]
    fn binary_goes_out_as_kind_0() {
        let encoded = encode(&WireMessage::Binary(bytes::Bytes::from_static(b"abc")));
        assert_eq!(encoded[0], 0);
        assert_eq!(&encoded[1..5], &3u32.to_be_bytes());
        assert_eq!(&encoded[5..], b"abc");
    }

    #[test]
    fn text_goes_out_as_kind_1() {
        let encoded = encode(&WireMessage::Text("가".into()));
        assert_eq!(encoded[0], 1);
        assert_eq!(&encoded[1..5], &3u32.to_be_bytes()); // '가' is 3 bytes in UTF-8
    }

    #[test]
    fn header_round_trips() {
        let encoded = encode(&WireMessage::Binary(bytes::Bytes::from_static(b"abcd")));
        let header: [u8; 5] = encoded[..5].try_into().unwrap();
        assert_eq!(decode_header(&header).unwrap(), (0, 4));
    }

    #[test]
    fn rejects_length_over_max() {
        let mut header = [0u8; 5];
        header[0] = 0;
        header[1..5].copy_from_slice(&((MAX_FRAME + 1) as u32).to_be_bytes());
        // Without this cap, a peer that ignores the protocol could declare a 4 GiB length
        // and push that much straight into our memory before we ever see a payload byte.
        assert!(matches!(decode_header(&header), Err(FrameError::TooLarge(_))));
    }

    #[test]
    fn rejects_unknown_kind() {
        let mut header = [0u8; 5];
        header[0] = 9;
        assert!(matches!(decode_header(&header), Err(FrameError::UnknownKind(9))));
    }

    #[test]
    fn body_reconstructs_binary() {
        let msg = body(0, b"abc".to_vec()).unwrap();
        assert_eq!(msg, WireMessage::Binary(bytes::Bytes::from_static(b"abc")));
    }

    #[test]
    fn body_reconstructs_text() {
        let msg = body(1, "가".as_bytes().to_vec()).unwrap();
        assert_eq!(msg, WireMessage::Text("가".into()));
    }

    #[test]
    fn body_rejects_non_utf8_text() {
        let invalid = vec![0xff, 0xfe, 0xfd];
        assert!(matches!(body(1, invalid), Err(FrameError::NotUtf8)));
    }

    #[test]
    fn body_rejects_unknown_kind() {
        assert!(matches!(body(9, vec![]), Err(FrameError::UnknownKind(9))));
    }
}
