mod connection;
pub mod enroll;
mod error;
mod handle;
#[cfg(feature = "hostname")]
mod hostname;
mod node;
#[cfg(feature = "runtime")]
pub mod runtime;
mod serve;
pub mod transport;

pub mod testing;

/// The deployment a node reaches with no configuration at all.
///
/// It lives here rather than in each node so there is exactly one answer, including the `/api`
/// prefix — the enrollment base is derived from this URL by truncating at `/zyris/`, so a node
/// configured with this constant cannot enroll against one deployment while connecting to another.
pub const DEFAULT_SERVER_URL: &str = "wss://attacca.cc/api/zyris/v1/ws";

#[cfg(feature = "hostname")]
pub use hostname::machine_name;

pub use connection::{AcceptOptions, CapabilityClient, Connection, ConnectionInfo, RawIncomingStream};
pub use error::{CloseReason, Error, ErrorCode, Result, TransportError};
pub use handle::CapabilityHandle;
pub use node::{Node, NodeBuilder, NodeKind};
pub use serve::{
    decode_call, encode_response, encode_streaming, unknown_tool, IncomingCall, ItemStream,
    Outgoing, RawItemStream, ServeCapability, Streaming,
};

pub use zyris_proto as proto;
pub use zyris_proto::{
    AttachmentRef, Blob, CapabilityDescriptor, Chunk, Datum, Payload, Serialization,
    ToolDescriptor, Transfer, WireError,
};

pub use async_trait::async_trait;
pub use schemars;
pub use serde;
pub use serde_json;
pub use zyris_macros::capability;

pub mod prelude {
    pub use crate::{
        AcceptOptions, Blob, CapabilityClient, CapabilityHandle, Chunk, Connection, Datum, Error,
        ErrorCode, Node, NodeKind, Result, ServeCapability, Streaming,
    };
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;
    use futures_util::StreamExt;
    use serde::{Deserialize, Serialize};
    use zyris_proto::{CapabilityDescriptor, ToolDescriptor, Transfer};

    use crate::serve::{
        decode_call, encode_response, encode_streaming, IncomingCall, Outgoing, ServeCapability,
        Streaming,
    };
    use crate::{CapabilityClient, CapabilityHandle, Error, ErrorCode, Node, NodeKind, Result};

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct EchoRequest {
        text: String,
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct EchoResponse {
        text: String,
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct CountHead {
        total: u32,
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct CountItem {
        value: u32,
    }

    struct EchoCapability {
        slow: Arc<AtomicBool>,
    }

    fn echo_descriptor() -> CapabilityDescriptor {
        CapabilityDescriptor {
            name: "echo".into(),
            version: 1,
            tools: vec![
                ToolDescriptor {
                    name: "say".into(),
                    description: "Echo text back.".into(),
                    transfer: Transfer::Unary,
                    request_schema: serde_json::json!({"type": "object"}),
                    response_schema: None,
                    item_schema: None,
                },
                ToolDescriptor {
                    name: "count".into(),
                    description: "Stream numbers.".into(),
                    transfer: Transfer::UniStream,
                    request_schema: serde_json::json!({"type": "object"}),
                    response_schema: None,
                    item_schema: None,
                },
            ],
        }
    }

    #[async_trait]
    impl ServeCapability for EchoCapability {
        fn descriptor(&self) -> CapabilityDescriptor {
            echo_descriptor()
        }

        async fn dispatch(&self, call: IncomingCall) -> Result<Outgoing> {
            match call.tool.as_str() {
                "say" => {
                    if self.slow.load(Ordering::SeqCst) {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                    }
                    let req: EchoRequest = decode_call(&call)?;
                    encode_response(&EchoResponse { text: req.text })
                }
                "count" => {
                    let req: CountHead = decode_call(&call)?;
                    let total = req.total;
                    let items = futures_util::stream::iter(
                        (0..total).map(|value| Ok(CountItem { value })),
                    );
                    encode_streaming(
                        Streaming::new(CountHead { total }, items),
                        call.serialization,
                    )
                }
                other => Err(crate::unknown_tool("echo", other)),
            }
        }
    }

    struct EchoClient {
        handle: CapabilityHandle,
    }

    impl CapabilityClient for EchoClient {
        const NAME: &'static str = "echo";
        const VERSION: u32 = 1;

        fn from_handle(handle: CapabilityHandle) -> Self {
            EchoClient { handle }
        }
    }

    impl EchoClient {
        async fn say(&self, text: &str) -> Result<EchoResponse> {
            self.handle.call("say", &EchoRequest { text: text.into() }).await
        }

        async fn count(&self, total: u32) -> Result<Streaming<CountHead, CountItem>> {
            self.handle.call_streaming("count", &CountHead { total }).await
        }
    }

    fn node_with_echo(slow: Arc<AtomicBool>) -> Node {
        Node::builder()
            .name("server")
            .kind(NodeKind::Service)
            .capability(EchoCapability { slow })
            .build()
            .unwrap()
    }

    fn bare_node(name: &str) -> Node {
        Node::builder().name(name).kind(NodeKind::Cli).build().unwrap()
    }

    #[tokio::test]
    async fn unary_roundtrip_over_duplex() {
        let dialer = bare_node("client");
        let acceptor = node_with_echo(Arc::new(AtomicBool::new(false)));
        let (client_conn, _server_conn) = crate::testing::duplex(&dialer, &acceptor).await.unwrap();

        let echo: EchoClient = client_conn
            .wait_capability(Duration::from_secs(2))
            .await
            .unwrap();
        let response = echo.say("hello zyris").await.unwrap();
        assert_eq!(response.text, "hello zyris");
    }

    #[tokio::test]
    async fn dialer_capabilities_are_callable_from_acceptor() {
        let dialer = node_with_echo(Arc::new(AtomicBool::new(false)));
        let acceptor = bare_node("server");
        let (_client_conn, server_conn) = crate::testing::duplex(&dialer, &acceptor).await.unwrap();

        let echo: EchoClient = server_conn
            .wait_capability(Duration::from_secs(2))
            .await
            .unwrap();
        let response = echo.say("reverse direction").await.unwrap();
        assert_eq!(response.text, "reverse direction");
    }

    #[tokio::test]
    async fn uni_stream_delivers_head_and_items() {
        let dialer = bare_node("client");
        let acceptor = node_with_echo(Arc::new(AtomicBool::new(false)));
        let (client_conn, _server) = crate::testing::duplex(&dialer, &acceptor).await.unwrap();

        let echo: EchoClient = client_conn
            .wait_capability(Duration::from_secs(2))
            .await
            .unwrap();
        let mut streaming = echo.count(5).await.unwrap();
        assert_eq!(streaming.head, CountHead { total: 5 });
        let mut seen = Vec::new();
        while let Some(item) = streaming.items.next().await {
            seen.push(item.unwrap().value);
        }
        assert_eq!(seen, vec![0, 1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn unknown_tool_and_capability_errors() {
        let dialer = bare_node("client");
        let acceptor = node_with_echo(Arc::new(AtomicBool::new(false)));
        let (client_conn, _server) = crate::testing::duplex(&dialer, &acceptor).await.unwrap();
        let echo: EchoClient = client_conn
            .wait_capability(Duration::from_secs(2))
            .await
            .unwrap();

        let err = echo
            .handle
            .call::<_, EchoResponse>("nope", &EchoRequest { text: "x".into() })
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::MethodNotFound);

        let err = client_conn
            .call_raw("missing.tool", zyris_proto::Payload::default())
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::CapabilityNotAnnounced);
    }

    #[tokio::test]
    async fn reserved_capability_is_rejected() {
        let dialer = node_with_echo(Arc::new(AtomicBool::new(false)));
        let acceptor = bare_node("server");
        let options = crate::AcceptOptions {
            reserved_capabilities: vec!["echo".into()],
            ..Default::default()
        };
        let (client_conn, server_conn) =
            crate::testing::duplex_with(&dialer, &acceptor, options).await.unwrap();

        let err = match server_conn.wait_capability::<EchoClient>(Duration::from_millis(300)).await
        {
            Ok(_) => panic!("expected rejection"),
            Err(err) => err,
        };
        assert_eq!(err.code, ErrorCode::CapabilityNotAnnounced);
        assert!(!client_conn.is_closed());
    }

    #[tokio::test]
    async fn capability_discovery_lists_descriptors() {
        let dialer = bare_node("client");
        let acceptor = node_with_echo(Arc::new(AtomicBool::new(false)));
        let (client_conn, _server) = crate::testing::duplex(&dialer, &acceptor).await.unwrap();
        let _echo: EchoClient = client_conn
            .wait_capability(Duration::from_secs(2))
            .await
            .unwrap();
        assert!(client_conn.capability::<EchoClient>().is_some());
        let descriptors = client_conn.peer_descriptors();
        assert_eq!(descriptors.len(), 1);
        assert_eq!(descriptors[0].name, "echo");
        assert_eq!(descriptors[0].tools.len(), 2);
    }

    #[tokio::test]
    async fn credit_flow_allows_streams_larger_than_initial_credit() {
        struct BigStream;

        #[async_trait]
        impl ServeCapability for BigStream {
            fn descriptor(&self) -> CapabilityDescriptor {
                CapabilityDescriptor {
                    name: "big".into(),
                    version: 1,
                    tools: vec![ToolDescriptor {
                        name: "pull".into(),
                        description: "Stream 2 MiB of data.".into(),
                        transfer: Transfer::UniStream,
                        request_schema: serde_json::json!({"type": "object"}),
                        response_schema: None,
                        item_schema: None,
                    }],
                }
            }

            async fn dispatch(&self, call: IncomingCall) -> Result<Outgoing> {
                let items = futures_util::stream::iter(
                    (0..32).map(|_| Ok(crate::Chunk::new(vec![7u8; 64 * 1024]))),
                );
                crate::encode_streaming(
                    Streaming::new(CountHead { total: 32 }, items),
                    call.serialization,
                )
            }
        }

        struct BigClient {
            handle: CapabilityHandle,
        }

        impl CapabilityClient for BigClient {
            const NAME: &'static str = "big";
            const VERSION: u32 = 1;

            fn from_handle(handle: CapabilityHandle) -> Self {
                BigClient { handle }
            }
        }

        let dialer = bare_node("client");
        let acceptor = Node::builder()
            .name("server")
            .kind(NodeKind::Service)
            .capability(BigStream)
            .build()
            .unwrap();
        let (client_conn, _server) = crate::testing::duplex(&dialer, &acceptor).await.unwrap();
        let big: BigClient = client_conn
            .wait_capability(Duration::from_secs(2))
            .await
            .unwrap();

        let mut streaming: Streaming<CountHead, crate::Chunk> = big
            .handle
            .call_streaming("pull", &CountHead { total: 32 })
            .await
            .unwrap();
        let mut total = 0usize;
        let mut chunks = 0usize;
        while let Some(item) = streaming.items.next().await {
            let chunk = item.unwrap();
            total += chunk.0.len();
            chunks += 1;
        }
        assert_eq!(chunks, 32);
        assert_eq!(total, 32 * 64 * 1024);
    }

    #[tokio::test]
    async fn close_settles_pending_calls() {
        let slow = Arc::new(AtomicBool::new(true));
        let dialer = bare_node("client");
        let acceptor = node_with_echo(slow);
        let (client_conn, server_conn) = crate::testing::duplex(&dialer, &acceptor).await.unwrap();
        let echo: EchoClient = client_conn
            .wait_capability(Duration::from_secs(2))
            .await
            .unwrap();

        let pending = tokio::spawn(async move { echo.say("never").await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        server_conn.close("test shutdown");

        let result: Result<EchoResponse, Error> = pending.await.unwrap();
        assert_eq!(result.unwrap_err().code, ErrorCode::ConnectionLost);
        let reason = client_conn.closed().await;
        assert!(matches!(
            reason,
            crate::CloseReason::Graceful(_) | crate::CloseReason::Transport(_)
        ));
    }
}
