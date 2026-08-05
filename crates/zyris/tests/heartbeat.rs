//! The negotiated heartbeat end to end: a peer that goes silently dead (half-open connection) is
//! detected and the connection closes, so the node's reconnect loop can run; a healthy idle
//! connection stays up with heartbeats flowing; a peer that never agreed the feature is never
//! lapse-closed; and a dead writer shuts the connection down instead of wedging it.

use std::sync::atomic::Ordering;
use std::time::Duration;

use zyris::proto::{HeartbeatConfig, FEATURE_ATTACHMENTS, FEATURE_CANCEL};
use zyris::testing::gated_duplex;
use zyris::{AcceptOptions, CloseReason, Node, NodeKind, Payload};

/// Production defaults are 20s/45s; these are shrunk so a lapse happens within a couple of seconds.
const FAST: HeartbeatConfig = HeartbeatConfig { interval_s: 1, timeout_s: 2 };

fn server() -> Node {
    Node::builder().name("server").kind(NodeKind::Server).build().unwrap()
}

fn client() -> Node {
    Node::builder().name("client").kind(NodeKind::Cli).build().unwrap()
}

#[tokio::test]
async fn a_silent_peer_is_detected_and_the_connection_closes() {
    let (client_conn, server_conn, gates) =
        gated_duplex(&client(), &server(), AcceptOptions { heartbeat: FAST, ..Default::default() }).await;

    // Healthy: both sides exchange heartbeats, so nothing closes even past the timeout.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(!server_conn.is_closed(), "a healthy connection must not lapse-close");
    assert!(!client_conn.is_closed());

    // The client goes silent: its frames stop arriving, the channel underneath never errors.
    gates.dialer_to_acceptor.store(false, Ordering::Relaxed);

    let reason = tokio::time::timeout(Duration::from_secs(6), server_conn.closed())
        .await
        .expect("the server must lapse-close a silent peer");
    assert!(matches!(reason, CloseReason::Heartbeat(_)), "expected heartbeat lapse, got {reason}");
}

#[tokio::test]
async fn a_healthy_idle_connection_stays_up_and_heartbeats_flow() {
    let (_client_conn, server_conn, gates) =
        gated_duplex(&client(), &server(), AcceptOptions { heartbeat: FAST, ..Default::default() }).await;

    let before = gates.delivered.load(Ordering::Relaxed);
    tokio::time::sleep(Duration::from_millis(2300)).await;
    assert!(!server_conn.is_closed());

    let after = gates.delivered.load(Ordering::Relaxed);
    assert!(after > before, "an idle connection must still exchange frames (heartbeats)");
}

#[tokio::test]
async fn a_peer_that_never_agreed_the_feature_is_not_lapse_closed() {
    // The acceptor withholds FEATURE_HEARTBEAT — an older server. Neither side may enforce
    // liveness, so the silence below must go undetected.
    let options = AcceptOptions {
        heartbeat: FAST,
        features: vec![FEATURE_CANCEL.to_string(), FEATURE_ATTACHMENTS.to_string()],
        ..Default::default()
    };
    let (client_conn, server_conn, gates) = gated_duplex(&client(), &server(), options).await;

    gates.dialer_to_acceptor.store(false, Ordering::Relaxed);
    tokio::time::sleep(Duration::from_millis(3500)).await;

    assert!(!server_conn.is_closed(), "no heartbeat agreement → no lapse enforcement");
    assert!(!client_conn.is_closed());
}

#[tokio::test]
async fn a_dead_writer_shuts_the_connection_down() {
    let (client_conn, _server_conn, gates) = gated_duplex(&client(), &server(), AcceptOptions::default()).await;

    gates.dialer_sink_fails.store(true, Ordering::Relaxed);
    // Trigger a write; it hits the failing sink and the writer must propagate the failure instead
    // of silently dying while the reader keeps waiting on a socket that never errors.
    let _ = client_conn.notify("test", Payload::default());

    let reason = tokio::time::timeout(Duration::from_secs(3), client_conn.closed())
        .await
        .expect("a dead writer must close the connection");
    assert!(matches!(reason, CloseReason::Transport(_)), "expected transport close, got {reason}");
}
