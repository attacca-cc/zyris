pub mod attach;
mod capability;
mod datum;
mod envelope;
mod error;
mod frame;
mod payload;

pub use attach::{AttachmentTrailer, Detached};
pub use capability::{
    method_name, split_method, AnnounceParams, AnnounceResult, CapabilityDescriptor,
    ClosingParams, RejectedCapability, ToolDescriptor, Transfer,
};
pub use datum::{AttachmentRef, Blob, Chunk, Datum, INLINE_BLOB_MAX};
pub use envelope::{
    AckProtocol, Envelope, HeartbeatConfig, Hello, HelloAck, HelloProtocol, Limits, ResumeInfo,
    Serialization, StreamDecl, CLOSE_FLOW_VIOLATION, CLOSE_MALFORMED_FRAME, CLOSE_NORMAL,
    CLOSE_UNAUTHORIZED, CLOSE_UNSUPPORTED_VERSION, FEATURE_ATTACHMENTS, FEATURE_CANCEL,
    FEATURE_HEARTBEAT, METHOD_ANNOUNCE, METHOD_CLOSING, METHOD_HEARTBEAT, METHOD_WEBRTC_CLOSE,
    METHOD_WEBRTC_SIGNAL, PROTOCOL_MAJOR, PROTOCOL_MINOR,
};
pub use error::{ErrorCode, WireError};
pub use frame::{
    decode_binary, decode_text, encode_control, encode_stream_data, CodecError, IncomingFrame,
    WireMessage, FRAME_CONTROL, FRAME_STREAM_DATA,
};
pub use payload::{decode_item, encode_item, json_to_rmpv, rmpv_to_json, Payload};

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    fn roundtrip(envelope: Envelope) {
        let bin = encode_control(&envelope, Serialization::Msgpack).unwrap();
        let WireMessage::Binary(bytes) = bin else { panic!("expected binary") };
        let decoded = decode_binary(bytes).unwrap();
        assert_eq!(decoded, IncomingFrame::Control(envelope.clone()));

        let text = encode_control(&envelope, Serialization::Json).unwrap();
        let WireMessage::Text(text) = text else { panic!("expected text") };
        let decoded = decode_text(&text).unwrap();
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn envelope_roundtrips() {
        roundtrip(Envelope::Req {
            id: 17,
            method: "terminal.exec".into(),
            params: Payload::from_typed(&serde_json::json!({"command": "ls", "n": 3})).unwrap(),
            stream: Some(StreamDecl { id: 42 }),
        });
        roundtrip(Envelope::Res { id: 17, result: Payload::default() });
        roundtrip(Envelope::Err {
            id: 3,
            error: WireError::new(ErrorCode::InvalidParams, "bad args"),
        });
        roundtrip(Envelope::Note { method: "zyris.closing".into(), params: Payload::default() });
        roundtrip(Envelope::Cancel { id: 9 });
        roundtrip(Envelope::SCredit { stream: 4, bytes: 65536 });
        roundtrip(Envelope::SEnd { stream: 4, trailer: Payload::default() });
        roundtrip(Envelope::SErr {
            stream: 4,
            error: WireError::new(ErrorCode::StreamLagged, "gap"),
        });
        roundtrip(Envelope::SCancel { stream: 4 });
    }

    #[test]
    fn handshake_roundtrips() {
        roundtrip(Envelope::Hello(Hello {
            protocol: HelloProtocol { major: 1, minors_supported: vec![0] },
            serialization: vec![Serialization::Msgpack, Serialization::Json],
            agent: "zyris-test/0.1".into(),
            features: vec!["cancel".into()],
            resume: None,
        }));
        roundtrip(Envelope::HelloAck(HelloAck {
            protocol: AckProtocol { major: 1, minor: 0 },
            serialization: Serialization::Msgpack,
            conn_id: "c1".into(),
            resume_token: "tok".into(),
            node_id: "n1".into(),
            heartbeat: HeartbeatConfig::default(),
            limits: Limits::default(),
            resumed: false,
            features: vec![FEATURE_ATTACHMENTS.into()],
        }));
    }

    #[test]
    fn error_code_unknown_string_survives() {
        let err = WireError::new(ErrorCode::Other("terminal_gone".into()), "x");
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("\"terminal_gone\""));
        let back: WireError = serde_json::from_str(&json).unwrap();
        assert_eq!(back.code, ErrorCode::Other("terminal_gone".into()));

        let known: WireError =
            serde_json::from_str(r#"{"code":"timeout","message":"m","retriable":true}"#).unwrap();
        assert_eq!(known.code, ErrorCode::Timeout);
    }

    #[test]
    fn inline_blob_is_msgpack_bin_and_json_base64() {
        let datum = Datum::Image {
            name: "shot.png".into(),
            description: None,
            media_type: "image/png".into(),
            blob: Blob::from_bytes(vec![0u8, 159, 146, 150]),
        };
        let env = Envelope::Res { id: 1, result: Payload::from_typed(&datum).unwrap() };

        let WireMessage::Binary(bin) = encode_control(&env, Serialization::Msgpack).unwrap()
        else {
            panic!()
        };
        let IncomingFrame::Control(Envelope::Res { result, .. }) = decode_binary(bin).unwrap()
        else {
            panic!()
        };
        assert_eq!(result.to_typed::<Datum>().unwrap(), datum);

        let WireMessage::Text(text) = encode_control(&env, Serialization::Json).unwrap() else {
            panic!()
        };
        assert!(text.contains("AJ+Slg=="));
        let Envelope::Res { result, .. } = decode_text(&text).unwrap() else { panic!() };
        assert_eq!(result.to_typed::<Datum>().unwrap(), datum);
    }

    #[test]
    fn attachment_blob_roundtrips() {
        let datum = Datum::File {
            filename: "big.bin".into(),
            description: Some("large".into()),
            media_type: None,
            blob: Blob::Attachment(AttachmentRef {
                stream: 42,
                size: 1 << 30,
                sha256: Some("ab".into()),
                offset: 0,
            }),
        };
        let env = Envelope::Res { id: 1, result: Payload::from_typed(&datum).unwrap() };
        for s in [Serialization::Msgpack, Serialization::Json] {
            let decoded = match encode_control(&env, s).unwrap() {
                WireMessage::Binary(b) => {
                    let IncomingFrame::Control(e) = decode_binary(b).unwrap() else { panic!() };
                    e
                }
                WireMessage::Text(t) => decode_text(&t).unwrap(),
            };
            let Envelope::Res { result, .. } = decoded else { panic!() };
            assert_eq!(result.to_typed::<Datum>().unwrap(), datum);
        }
    }

    #[test]
    fn stream_data_frame_roundtrips() {
        let frame = encode_stream_data(7, 3, b"hello");
        let decoded = decode_binary(frame).unwrap();
        assert_eq!(
            decoded,
            IncomingFrame::StreamData { stream: 7, seq: 3, payload: Bytes::from_static(b"hello") }
        );
    }

    #[test]
    fn announce_params_roundtrip_through_payload() {
        let params = AnnounceParams {
            capabilities: vec![CapabilityDescriptor {
                name: "terminal".into(),
                version: 1,
                tools: vec![ToolDescriptor {
                    name: "exec".into(),
                    description: "Run a command.".into(),
                    transfer: Transfer::Unary,
                    request_schema: serde_json::json!({
                        "type": "object",
                        "properties": {"command": {"type": "string"}}
                    }),
                    response_schema: Some(serde_json::json!({"type": "object"})),
                    item_schema: None,
                }],
            }],
        };
        let env = Envelope::Req {
            id: 1,
            method: METHOD_ANNOUNCE.into(),
            params: Payload::from_typed(&params).unwrap(),
            stream: None,
        };
        let WireMessage::Binary(bin) = encode_control(&env, Serialization::Msgpack).unwrap()
        else {
            panic!()
        };
        let IncomingFrame::Control(Envelope::Req { params: decoded, .. }) =
            decode_binary(bin).unwrap()
        else {
            panic!()
        };
        assert_eq!(decoded.to_typed::<AnnounceParams>().unwrap(), params);
    }
}
