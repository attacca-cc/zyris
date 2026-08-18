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
            meta: Payload::default(),
        });
        roundtrip(Envelope::Req {
            id: 18,
            method: "terminal.exec".into(),
            params: Payload::from_typed(&serde_json::json!({"command": "ls"})).unwrap(),
            stream: None,
            meta: Payload::from_json(serde_json::json!({"session_id": "s-1"})),
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

    /// `meta` is optional in both directions, and it has to stay that way: a peer built before the
    /// field existed sends a `req` without it, and one built after sends a `req` an older peer must
    /// ignore rather than reject. Requiring it — or serialising it when empty — would make every
    /// call between mismatched versions a malformed frame.
    #[test]
    fn a_req_is_readable_by_a_peer_that_knows_nothing_of_meta() {
        let without = r#"{"t":"req","id":1,"method":"terminal.exec","params":{"command":"ls"}}"#;
        let Envelope::Req { meta, .. } = decode_text(without).unwrap() else { panic!("not a req") };
        assert!(meta.is_nil(), "a req without meta reads as carrying none");

        let empty = Envelope::Req {
            id: 1,
            method: "terminal.exec".into(),
            params: Payload::default(),
            stream: None,
            meta: Payload::default(),
        };
        let WireMessage::Text(text) = encode_control(&empty, Serialization::Json).unwrap() else {
            panic!("expected text")
        };
        assert!(!text.contains("meta"), "an empty meta puts nothing on the wire: {text}");
    }

    /// A peer built before `Hello::kind` sends no such field, and it has to keep connecting.
    ///
    /// This is the whole reason the field is `Option` rather than a defaulted string: absent must
    /// stay distinguishable from any particular kind, so an acceptor that treats one kind
    /// specially treats an old peer as "did not say" instead of as that kind.
    #[test]
    fn a_hello_from_before_the_kind_field_still_parses() {
        let older = r#"{"t":"hello","protocol":{"major":1,"minors_supported":[0]},
            "serialization":["json"],"agent":"zyris/0.1.0 (old; desktop)","features":[]}"#;
        let Envelope::Hello(hello) = decode_text(older).expect("an older hello must parse") else {
            panic!("expected a hello")
        };
        assert_eq!(hello.kind, None, "absent has to read as absent, not as a default kind");
        assert_eq!(hello.agent, "zyris/0.1.0 (old; desktop)");
    }

    /// And the field is left off the wire entirely when there is nothing to say, so a peer that
    /// does not set one costs no bytes and reads the same to an old acceptor.
    #[test]
    fn an_unset_kind_is_absent_from_the_wire() {
        let hello = Envelope::Hello(Hello {
            protocol: HelloProtocol { major: 1, minors_supported: vec![0] },
            serialization: vec![Serialization::Json],
            agent: "zyris-test/0.1".into(),
            kind: None,
            features: vec![],
            resume: None,
        });
        let WireMessage::Text(text) = encode_control(&hello, Serialization::Json).unwrap() else {
            panic!("expected text")
        };
        assert!(!text.contains("kind"), "an unset kind puts nothing on the wire: {text}");
    }

    #[test]
    fn handshake_roundtrips() {
        roundtrip(Envelope::Hello(Hello {
            protocol: HelloProtocol { major: 1, minors_supported: vec![0] },
            serialization: vec![Serialization::Msgpack, Serialization::Json],
            agent: "zyris-test/0.1".into(),
            kind: Some("cli".into()),
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
            meta: Payload::default(),
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
