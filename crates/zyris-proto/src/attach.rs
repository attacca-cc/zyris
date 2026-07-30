//! Moving blob bytes out of a payload and onto a stream, and back again.
//!
//! A [`Datum`](crate::Datum) carries its bytes inline, which is the right shape until the bytes are
//! large: a payload travels in one control frame, so a 40 MB file would arrive as one 40 MB frame
//! that no receiver can flow-control and every intermediate hop has to buffer whole. The attachment
//! form replaces the bytes with [`AttachmentRef`] naming a stream, and the bytes follow as
//! STREAM_DATA chunks the receiver can credit-limit like any other stream.
//!
//! Both halves of that live here as plain tree walks over `rmpv::Value`, because [`Payload`] is
//! already a decoded value — the connection never has to re-parse a frame to find out whether it
//! carries bytes.
//!
//! The walk matches on the datum's *wire shape* (`kind` plus a `blob`) rather than deserializing to
//! `Datum`, so a datum nested anywhere — a bare one, a list, a field of some tool's own response
//! struct — is found without the connection knowing that tool's types.

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::datum::AttachmentRef;

/// What an attachment's `s_end` carries. §4.2 allows a trailer on any stream and names `sha256` as
/// the end-to-end integrity check; for an attachment it is always present, because the receiver has
/// no other way to tell a truncated blob from a short one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentTrailer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

/// Bytes lifted out of a payload, waiting to be sent on the stream the payload now names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detached {
    pub stream: u32,
    pub bytes: Bytes,
    /// Digest of `bytes`, computed once here so the sender can put it in the `s_end` trailer
    /// without a second pass.
    pub sha256: String,
}

/// Replace every inline blob larger than `threshold` with an attachment reference, returning the
/// bytes to send on the streams `alloc` handed out.
///
/// Only blobs over the threshold move: a small image still rides inline, where it costs one frame
/// and no round trip. `alloc` is called once per blob that does move, in walk order.
pub fn detach(
    value: &mut rmpv::Value,
    threshold: usize,
    alloc: &mut impl FnMut() -> u32,
) -> Vec<Detached> {
    let mut out = Vec::new();
    walk_detach(value, threshold, alloc, &mut out);
    out
}

fn walk_detach(
    value: &mut rmpv::Value,
    threshold: usize,
    alloc: &mut impl FnMut() -> u32,
    out: &mut Vec<Detached>,
) {
    match value {
        rmpv::Value::Map(entries) => {
            if is_datum(entries) {
                if let Some(slot) = entry_mut(entries, "blob") {
                    if let rmpv::Value::Binary(bytes) = slot {
                        if bytes.len() > threshold {
                            let bytes = Bytes::from(std::mem::take(bytes));
                            let stream = alloc();
                            let sha256 = digest(&bytes);
                            *slot = attachment_value(&AttachmentRef {
                                stream,
                                size: bytes.len() as u64,
                                sha256: Some(sha256.clone()),
                                offset: 0,
                            });
                            out.push(Detached { stream, bytes, sha256 });
                            return;
                        }
                    }
                }
            }
            for (_, nested) in entries.iter_mut() {
                walk_detach(nested, threshold, alloc, out);
            }
        }
        rmpv::Value::Array(items) => {
            for item in items.iter_mut() {
                walk_detach(item, threshold, alloc, out);
            }
        }
        _ => {}
    }
}

/// Every attachment this payload references, in walk order.
///
/// The receiver calls this the moment a frame is decoded — before the payload reaches whoever
/// asked for it — because the chunks are already on their way and a stream nobody is listening for
/// is dropped on the floor.
pub fn refs(value: &rmpv::Value) -> Vec<AttachmentRef> {
    let mut out = Vec::new();
    walk_refs(value, &mut out);
    out
}

fn walk_refs(value: &rmpv::Value, out: &mut Vec<AttachmentRef>) {
    match value {
        rmpv::Value::Map(entries) => {
            if is_datum(entries) {
                if let Some(blob) = entry(entries, "blob") {
                    if let Some(found) = parse_attachment(blob) {
                        out.push(found);
                        return;
                    }
                }
            }
            for (_, nested) in entries.iter() {
                walk_refs(nested, out);
            }
        }
        rmpv::Value::Array(items) => {
            for item in items.iter() {
                walk_refs(item, out);
            }
        }
        _ => {}
    }
}

/// Put received bytes back where the reference was, turning the payload back into the one the
/// sender started with. Returns whether a reference naming `stream` was found.
pub fn splice(value: &mut rmpv::Value, stream: u32, bytes: &Bytes) -> bool {
    match value {
        rmpv::Value::Map(entries) => {
            if is_datum(entries) {
                if let Some(slot) = entry_mut(entries, "blob") {
                    if parse_attachment(slot).is_some_and(|a| a.stream == stream) {
                        *slot = rmpv::Value::Binary(bytes.to_vec());
                        return true;
                    }
                }
            }
            entries.iter_mut().any(|(_, nested)| splice(nested, stream, bytes))
        }
        rmpv::Value::Array(items) => items.iter_mut().any(|item| splice(item, stream, bytes)),
        _ => false,
    }
}

pub fn digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// A map is a datum if it is tagged as one. `text` datums carry no blob and are skipped here
/// rather than in the caller, so an unrelated map that happens to have a `blob` key is never
/// rewritten.
fn is_datum(entries: &[(rmpv::Value, rmpv::Value)]) -> bool {
    matches!(entry(entries, "kind").and_then(as_str), Some("image" | "file"))
}

fn entry<'a>(entries: &'a [(rmpv::Value, rmpv::Value)], key: &str) -> Option<&'a rmpv::Value> {
    entries.iter().find(|(k, _)| as_str(k) == Some(key)).map(|(_, v)| v)
}

fn entry_mut<'a>(
    entries: &'a mut [(rmpv::Value, rmpv::Value)],
    key: &str,
) -> Option<&'a mut rmpv::Value> {
    entries.iter_mut().find(|(k, _)| as_str(k) == Some(key)).map(|(_, v)| v)
}

fn as_str(value: &rmpv::Value) -> Option<&str> {
    match value {
        rmpv::Value::String(s) => s.as_str(),
        _ => None,
    }
}

fn as_u64(value: &rmpv::Value) -> Option<u64> {
    match value {
        rmpv::Value::Integer(i) => i.as_u64(),
        _ => None,
    }
}

/// Read the `{"attachment": {..}}` envelope [`Blob`](crate::Blob) serializes an attachment as. Hand
/// written rather than routed through serde because the walk sees `rmpv::Value` and this must not
/// allocate a deserializer per node visited.
fn parse_attachment(value: &rmpv::Value) -> Option<AttachmentRef> {
    let rmpv::Value::Map(outer) = value else { return None };
    let rmpv::Value::Map(inner) = entry(outer, "attachment")? else { return None };
    Some(AttachmentRef {
        stream: as_u64(entry(inner, "stream")?)? as u32,
        size: as_u64(entry(inner, "size")?)?,
        sha256: entry(inner, "sha256").and_then(as_str).map(str::to_string),
        offset: entry(inner, "offset").and_then(as_u64).unwrap_or(0),
    })
}

fn attachment_value(reference: &AttachmentRef) -> rmpv::Value {
    let mut inner = vec![
        (rmpv::Value::from("stream"), rmpv::Value::from(reference.stream)),
        (rmpv::Value::from("size"), rmpv::Value::from(reference.size)),
        (rmpv::Value::from("offset"), rmpv::Value::from(reference.offset)),
    ];
    if let Some(sha256) = &reference.sha256 {
        inner.push((rmpv::Value::from("sha256"), rmpv::Value::from(sha256.as_str())));
    }
    rmpv::Value::Map(vec![(rmpv::Value::from("attachment"), rmpv::Value::Map(inner))])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datum::{Blob, Datum};
    use crate::payload::Payload;

    fn image(bytes: &[u8]) -> Datum {
        Datum::Image {
            name: "shot.png".into(),
            description: Some("display 0".into()),
            media_type: "image/png".into(),
            blob: Blob::from_bytes(bytes.to_vec()),
        }
    }

    fn payload_of<T: serde::Serialize>(value: &T) -> rmpv::Value {
        Payload::from_typed(value).unwrap().0
    }

    fn ids() -> impl FnMut() -> u32 {
        let mut next = 2;
        move || {
            let id = next;
            next += 2;
            id
        }
    }

    #[test]
    fn a_big_blob_leaves_and_a_small_one_stays() {
        let mut value = payload_of(&image(&[7u8; 100]));
        assert!(detach(&mut value, 128, &mut ids()).is_empty());

        let mut value = payload_of(&image(&[7u8; 300]));
        let detached = detach(&mut value, 128, &mut ids());
        assert_eq!(detached.len(), 1);
        assert_eq!(detached[0].stream, 2);
        assert_eq!(detached[0].bytes.len(), 300);
        assert_eq!(refs(&value), vec![AttachmentRef {
            stream: 2,
            size: 300,
            sha256: Some(detached[0].sha256.clone()),
            offset: 0,
        }]);
    }

    /// The point of walking the wire shape: the connection finds datums in a response type it has
    /// never heard of, at whatever depth that type happens to put them.
    #[test]
    fn datums_are_found_at_any_depth() {
        let mut value = payload_of(&serde_json::json!({ "captured_at": "now" }));
        let rmpv::Value::Map(entries) = &mut value else { panic!() };
        entries.push((rmpv::Value::from("shots"), rmpv::Value::Array(vec![
            payload_of(&image(&[1u8; 300])),
            payload_of(&image(&[2u8; 300])),
        ])));

        let detached = detach(&mut value, 128, &mut ids());
        assert_eq!(detached.len(), 2);
        assert_eq!(detached.iter().map(|d| d.stream).collect::<Vec<_>>(), vec![2, 4]);
        assert_eq!(refs(&value).len(), 2);
    }

    #[test]
    fn detach_then_splice_is_the_identity() {
        let original = image(&[9u8; 4096]);
        let mut value = payload_of(&original);
        let detached = detach(&mut value, 128, &mut ids());

        assert!(splice(&mut value, detached[0].stream, &detached[0].bytes));
        assert_eq!(Payload(value).to_typed::<Datum>().unwrap(), original);
    }

    #[test]
    fn splice_reports_a_stream_that_is_not_there() {
        let mut value = payload_of(&image(&[9u8; 300]));
        detach(&mut value, 128, &mut ids());
        assert!(!splice(&mut value, 999, &Bytes::from_static(b"x")));
    }

    /// A text datum has no blob, and a map that merely owns a `blob` key is not a datum. Rewriting
    /// either would corrupt a tool's own response.
    #[test]
    fn only_blob_bearing_datums_are_touched() {
        let mut value = payload_of(&serde_json::json!({
            "notes": { "kind": "text", "text": "hello" },
            "decoy": { "blob": "not a datum" },
        }));
        let before = value.clone();
        assert!(detach(&mut value, 0, &mut ids()).is_empty());
        assert_eq!(value, before);
        assert!(refs(&value).is_empty());
    }

    /// What the sender puts in the `s_end` trailer has to be what the receiver computes over the
    /// bytes it actually got, or the integrity check is theatre.
    #[test]
    fn the_digest_is_of_the_bytes_that_travel() {
        let mut value = payload_of(&image(&[3u8; 300]));
        let detached = detach(&mut value, 128, &mut ids());
        assert_eq!(detached[0].sha256, digest(&[3u8; 300]));
        assert_eq!(detached[0].sha256.len(), 64);
    }

    /// The reference the walk writes must be the one `Blob`'s own deserializer reads, or a typed
    /// client sees a malformed datum instead of an attachment.
    #[test]
    fn the_written_reference_deserializes_as_a_blob() {
        let mut value = payload_of(&image(&[5u8; 300]));
        detach(&mut value, 128, &mut ids());
        let Datum::Image { blob, .. } = Payload(value).to_typed::<Datum>().unwrap() else {
            panic!("expected an image")
        };
        assert_eq!(blob.attachment().map(|a| a.stream), Some(2));
        assert_eq!(blob.len(), 300);
    }
}
