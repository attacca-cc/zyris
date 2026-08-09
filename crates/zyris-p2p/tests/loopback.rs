//! Boots two iroh endpoints in one process and connects them **for real**. No relay involved —
//! it dials over loopback, so this runs in CI.

use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zyris::{AcceptOptions, Node, NodeKind};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Echoed {
    pub said: String,
}

#[zyris::capability(name = "echo", version = 1)]
pub trait Echo {
    async fn say(&self, text: String) -> zyris::Result<Echoed>;
}

struct EchoImpl;

#[async_trait::async_trait]
impl Echo for EchoImpl {
    async fn say(&self, text: String) -> zyris::Result<Echoed> {
        Ok(Echoed { said: text })
    }
}

/// Both tests share this wiring: bind two endpoints, have B `accept_next` and serve `Echo`,
/// have A `dial` and `connect_over`. Returns `(a_conn, b_conn, b_endpoint_id, a_ep, b_ep)` so a
/// caller that needs the peer id A saw does not have to duplicate the setup.
///
/// The trailing `a_ep`/`b_ep` are not decorative: `iroh::Endpoint` is `Arc`-backed, and dropping
/// its last clone aborts the underlying socket ungracefully out from under every `Connection` it
/// produced — even ones still in active use. `IrohTransport` now keeps its own clone alive for
/// as long as the connection itself lives (see `transport.rs`), which is the production fix, but
/// the caller's own endpoint handle is a separate value with its own lifetime, and this test was
/// dropping it: `b_ep` used to be moved into `b_task`'s closure and die the moment that task
/// returned, well before the test finished using the connection it produced. Returning both
/// endpoints keeps them alive for the caller's whole test, which is the test-level half of the
/// fix.
async fn connect_pair(
) -> (zyris::Connection, zyris::Connection, iroh::EndpointId, iroh::Endpoint, iroh::Endpoint) {
    let a_key = iroh::SecretKey::generate();
    let b_key = iroh::SecretKey::generate();
    let a_id = a_key.public();

    let b_ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(b_key.clone())
        .alpns(vec![zyris_p2p::transport::ALPN.to_vec()])
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .unwrap();
    let b_addr = b_ep.addr();
    let b_ep_keep = b_ep.clone();

    // B: accept and stand up the acceptor-side Connection.
    let b_task = tokio::spawn(async move {
        let (peer, transport) = zyris_p2p::peer::accept_next(&b_ep).await.unwrap().unwrap();
        let node = Node::builder()
            .name("b")
            .kind(NodeKind::Cli)
            .capability(EchoServer(EchoImpl))
            .build()
            .unwrap();
        // Do not use `AcceptOptions::default()` — see the warning in the task-2.5 brief's
        // "constraints" section. Leaving it at the default here would let a wiring mistake go
        // undetected until it hit live traffic.
        let opts = AcceptOptions { node_id: "b".into(), ..AcceptOptions::default() };
        let conn = node.accept(transport, opts).await.unwrap();
        (peer, conn)
    });

    let a_ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(a_key)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .unwrap();
    let transport = zyris_p2p::peer::dial(&a_ep, b_addr).await.unwrap();
    let a_node = Node::builder().name("a").kind(NodeKind::Cli).build().unwrap();

    // Run both sides' handshake concurrently rather than awaiting A first: `accept_next`'s
    // peer id and B's half of `establish` do not depend on A ever finishing `connect_over`,
    // so joining lets that assertion run (and a mutation there get caught) independently of
    // whichever side, if either, fails.
    let (a_result, b_result) = tokio::join!(a_node.connect_over(transport), b_task);
    let (peer_seen, b_conn) = b_result.unwrap();
    assert_eq!(peer_seen, a_id, "B must learn A's EndpointId");
    let a_conn = a_result.unwrap();

    (a_conn, b_conn, peer_seen, a_ep, b_ep_keep)
}

#[tokio::test]
async fn two_endpoints_connect_and_speak_zyris() {
    let (a_conn, _b_conn, _peer, _a_ep, _b_ep) = connect_pair().await;

    let echo: EchoClient = a_conn.wait_capability(Duration::from_secs(5)).await.unwrap();
    let reply = echo.say("hello".into()).await.unwrap();
    assert_eq!(reply.said, "hello");
}

#[tokio::test]
async fn a_large_message_round_trips_whole() {
    // Checks that framing does not break across a chunk boundary. QUIC is a byte stream, so a
    // single `read_exact` does not necessarily get everything at once — the header can arrive
    // split across reads, and the body almost certainly will for a message this size.
    let (a_conn, _b_conn, _peer, _a_ep, _b_ep) = connect_pair().await;
    let echo: EchoClient = a_conn.wait_capability(Duration::from_secs(5)).await.unwrap();

    let long_text = "가".repeat(300_000); // 900,000 bytes in UTF-8
    assert!(long_text.len() > 512 * 1024, "must exceed one QUIC read");
    let reply = echo.say(long_text.clone()).await.unwrap();
    assert_eq!(reply.said.len(), long_text.len(), "length must survive the round trip");
    assert_eq!(reply.said, long_text, "content must survive the round trip");
}
