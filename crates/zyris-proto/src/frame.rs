use bytes::{BufMut, Bytes, BytesMut};

use crate::envelope::{Envelope, Serialization};

pub const FRAME_CONTROL: u8 = 0x00;
pub const FRAME_STREAM_DATA: u8 = 0x01;

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("empty frame")]
    Empty,
    #[error("unknown frame kind {0:#04x}")]
    UnknownKind(u8),
    #[error("truncated stream data frame")]
    Truncated,
    #[error("msgpack encode: {0}")]
    MsgpackEncode(#[from] rmp_serde::encode::Error),
    #[error("msgpack decode: {0}")]
    MsgpackDecode(#[from] rmp_serde::decode::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq)]
pub enum WireMessage {
    Binary(Bytes),
    Text(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum IncomingFrame {
    Control(Envelope),
    StreamData { stream: u32, seq: u32, payload: Bytes },
}

pub fn encode_control(envelope: &Envelope, serialization: Serialization) -> Result<WireMessage, CodecError> {
    match serialization {
        Serialization::Msgpack => {
            let body = rmp_serde::to_vec_named(envelope)?;
            let mut buf = BytesMut::with_capacity(body.len() + 1);
            buf.put_u8(FRAME_CONTROL);
            buf.put_slice(&body);
            Ok(WireMessage::Binary(buf.freeze()))
        }
        Serialization::Json => Ok(WireMessage::Text(serde_json::to_string(envelope)?)),
    }
}

pub fn encode_stream_data(stream: u32, seq: u32, payload: &[u8]) -> Bytes {
    let mut buf = BytesMut::with_capacity(payload.len() + 9);
    buf.put_u8(FRAME_STREAM_DATA);
    buf.put_u32(stream);
    buf.put_u32(seq);
    buf.put_slice(payload);
    buf.freeze()
}

pub fn decode_binary(frame: Bytes) -> Result<IncomingFrame, CodecError> {
    let Some(&kind) = frame.first() else {
        return Err(CodecError::Empty);
    };
    match kind {
        FRAME_CONTROL => {
            let envelope: Envelope = rmp_serde::from_slice(&frame[1..])?;
            Ok(IncomingFrame::Control(envelope))
        }
        FRAME_STREAM_DATA => {
            if frame.len() < 9 {
                return Err(CodecError::Truncated);
            }
            let stream = u32::from_be_bytes(frame[1..5].try_into().unwrap());
            let seq = u32::from_be_bytes(frame[5..9].try_into().unwrap());
            Ok(IncomingFrame::StreamData { stream, seq, payload: frame.slice(9..) })
        }
        other => Err(CodecError::UnknownKind(other)),
    }
}

pub fn decode_text(text: &str) -> Result<Envelope, CodecError> {
    Ok(serde_json::from_str(text)?)
}
