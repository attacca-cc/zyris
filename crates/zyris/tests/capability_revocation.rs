//! Protocol §5: `zyris.announce` is full-replacement, so re-announcing without a capability
//! revokes it and fails whatever was still running against it.

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use zyris::proto::method_name;
use zyris::{
    encode_response, encode_streaming, unknown_tool, CapabilityDescriptor, Connection, ErrorCode,
    IncomingCall, Node, NodeKind, Outgoing, Payload, Result, ServeCapability, Streaming,
    ToolDescriptor, Transfer,
};

#[derive(Serialize, Deserialize)]
struct Empty {}

fn unary(name: &str) -> ToolDescriptor {
    ToolDescriptor {
        name: name.into(),
        description: "Test tool.".into(),
        transfer: Transfer::Unary,
        request_schema: serde_json::json!({"type": "object"}),
        response_schema: None,
        item_schema: None,
    }
}

struct Steady;

#[zyris::async_trait]
impl ServeCapability for Steady {
    fn descriptor(&self) -> CapabilityDescriptor {
        CapabilityDescriptor { name: "steady".into(), version: 1, tools: vec![unary("ping")] }
    }

    async fn dispatch(&self, call: IncomingCall) -> Result<Outgoing> {
        match call.tool.as_str() {
            "ping" => encode_response(&Empty {}),
            other => Err(unknown_tool("steady", other)),
        }
    }
}

struct Blocking {
    gate: Arc<Notify>,
}

#[zyris::async_trait]
impl ServeCapability for Blocking {
    fn descriptor(&self) -> CapabilityDescriptor {
        CapabilityDescriptor {
            name: "blocking".into(),
            version: 1,
            tools: vec![
                unary("noop"),
                unary("wait"),
                ToolDescriptor {
                    name: "drip".into(),
                    description: "One item, then nothing.".into(),
                    transfer: Transfer::UniStream,
                    request_schema: serde_json::json!({"type": "object"}),
                    response_schema: None,
                    item_schema: None,
                },
            ],
        }
    }

    async fn dispatch(&self, call: IncomingCall) -> Result<Outgoing> {
        match call.tool.as_str() {
            "noop" => encode_response(&Empty {}),
            "wait" => {
                self.gate.notified().await;
                encode_response(&Empty {})
            }
            "drip" => {
                let items = futures_util::stream::once(async { Ok(Empty {}) })
                    .chain(futures_util::stream::pending());
                encode_streaming(Streaming::new(Empty {}, items), call.serialization)
            }
            other => Err(unknown_tool("blocking", other)),
        }
    }
}

struct Pair {
    client: Connection,
    server: Node,
    _server_conn: Connection,
    gate: Arc<Notify>,
}

async fn pair() -> Pair {
    let gate = Arc::new(Notify::new());
    let dialer = Node::builder().name("client").kind(NodeKind::Cli).build().unwrap();
    let server = Node::builder()
        .name("server")
        .kind(NodeKind::Service)
        .capability(Blocking { gate: gate.clone() })
        .capability(Steady)
        .build()
        .unwrap();
    let (client, server_conn) = zyris::testing::duplex(&dialer, &server).await.unwrap();
    let pair = Pair { client, server, _server_conn: server_conn, gate };
    pair.await_announced(&["blocking", "steady"]).await;
    pair
}

impl Pair {
    fn announced(&self) -> Vec<String> {
        self.client.peer_descriptors().into_iter().map(|d| d.name).collect()
    }

    async fn await_announced(&self, want: &[&str]) {
        for _ in 0..200 {
            if self.announced() == want {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("expected {want:?} to be announced, saw {:?}", self.announced());
    }

    async fn call(&self, capability: &str, tool: &str) -> Result<Payload> {
        self.client.call_raw(&method_name(capability, tool), Payload::default()).await
    }
}

#[tokio::test]
async fn removing_a_capability_unannounces_it() {
    let pair = pair().await;

    assert!(pair.server.capabilities().remove("blocking").await);

    // `remove` awaits the re-announce, so the peer's view is settled by the time it returns.
    assert_eq!(pair.announced(), vec!["steady".to_string()]);
    assert!(!pair.server.capabilities().contains("blocking"));

    let err = pair.call("blocking", "noop").await.unwrap_err();
    assert_eq!(err.code, ErrorCode::CapabilityNotAnnounced);

    pair.call("steady", "ping").await.unwrap();
}

#[tokio::test]
async fn removing_what_was_never_offered_changes_nothing() {
    let pair = pair().await;

    assert!(!pair.server.capabilities().remove("absent").await);
    assert_eq!(pair.announced(), vec!["blocking".to_string(), "steady".to_string()]);
}

#[tokio::test]
async fn revoking_fails_a_unary_call_in_flight() {
    let pair = pair().await;

    let caller = pair.client.clone();
    let pending = tokio::spawn(async move {
        caller.call_raw(&method_name("blocking", "wait"), Payload::default()).await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(pair.server.capabilities().remove("blocking").await);

    let err = pending.await.unwrap().unwrap_err();
    assert_eq!(err.code, ErrorCode::CapabilityUnavailable);
}

#[tokio::test]
async fn revoking_terminates_a_stream_in_flight() {
    let pair = pair().await;

    let (_head, mut items) = pair
        .client
        .call_streaming_raw(&method_name("blocking", "drip"), Payload::default())
        .await
        .unwrap();
    items.next().await.unwrap().unwrap();

    assert!(pair.server.capabilities().remove("blocking").await);

    let err = items.next().await.unwrap().unwrap_err();
    assert_eq!(err.code, ErrorCode::CapabilityUnavailable);
}

#[tokio::test]
async fn revoking_leaves_other_capabilities_running() {
    let pair = pair().await;

    let caller = pair.client.clone();
    let pending = tokio::spawn(async move {
        caller.call_raw(&method_name("blocking", "wait"), Payload::default()).await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(pair.server.capabilities().remove("steady").await);

    pair.gate.notify_waiters();
    pending.await.unwrap().unwrap();
}

#[tokio::test]
async fn adding_a_capability_announces_it() {
    let pair = pair().await;

    assert!(pair.server.capabilities().remove("blocking").await);
    pair.server.capabilities().add(Blocking { gate: pair.gate.clone() }).await.unwrap();

    assert_eq!(pair.announced(), vec!["steady".to_string(), "blocking".to_string()]);
    pair.call("blocking", "noop").await.unwrap();
}

#[tokio::test]
async fn adding_a_duplicate_is_refused_and_changes_nothing() {
    let pair = pair().await;

    let err = pair.server.capabilities().add(Steady).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidParams);
    assert_eq!(pair.announced(), vec!["blocking".to_string(), "steady".to_string()]);
}

#[test]
fn duplicates_are_still_refused_at_build() {
    let built = Node::builder().capability(Steady).capability(Steady).build();
    let err = match built {
        Ok(_) => panic!("expected a duplicate capability to be refused"),
        Err(err) => err,
    };
    assert_eq!(err.code, ErrorCode::InvalidParams);
}
