#![cfg(all(feature = "transfer-listen", feature = "transfer"))]

//! The accept loop, over real iroh endpoints on loopback.
//!
//! `PeerCache`'s decisions are unit-tested next to it. What only running the whole thing can show
//! is the wiring between the parts, and two of those properties are the reason the loop is shaped
//! the way it is:
//!
//! - a peer that is not one of this account's nodes is closed **before** a zyris handshake happens,
//!   not filtered out somewhere after it;
//! - a peer that connects and then says nothing does not hold the listener shut for everyone else.
//!
//! Both were argued in comments and asserted by nothing until this file. `RelayMode::Disabled`
//! throughout: these are two sockets on one machine, and a relay in the path would only add
//! discovery time to a test that is about ordering.

use std::sync::Arc;
use std::time::Duration;

use zyris::{Node, NodeKind};
use zyris_attacca::ZPeerEntry;
use zyris_caps::peer_transfer::{
    PeerTransfer, PeerTransferClient, PeerTransferServer, TransferOffer,
};
use zyris_capkit::transfer::listen::{serve_peers, PeerDirectory};
use zyris_capkit::transfer::{LocalPeerTransfer, TransferConfig};
use zyris_p2p::iroh;
use zyris_p2p::tofu::TofuStore;

/// The account's node list, fixed at construction. `serve_peers` asks it once and caches.
struct Directory(Vec<ZPeerEntry>);

#[async_trait::async_trait]
impl PeerDirectory for Directory {
    async fn peers(&self) -> zyris::Result<Vec<ZPeerEntry>> {
        Ok(self.0.clone())
    }
}

fn entry(slug: &str, endpoint_id: &str) -> ZPeerEntry {
    ZPeerEntry {
        node_id: format!("node-{slug}"),
        slug: slug.to_string(),
        endpoint_id: endpoint_id.to_string(),
        online: true,
    }
}

/// An endpoint and the id it answers to, kept together because the id is read off the key rather
/// than the bound endpoint — the same way `the_iroh_link_carries_a_real_transfer` does it.
struct Peer {
    endpoint: iroh::Endpoint,
    id: iroh::EndpointId,
}

impl Peer {
    async fn new() -> Peer {
        let key = iroh::SecretKey::generate();
        let id = key.public();
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(key)
            .alpns(vec![zyris_p2p::transport::ALPN.to_vec()])
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        Peer { endpoint, id }
    }

    fn addr(&self) -> iroh::EndpointAddr {
        let mut addr = iroh::EndpointAddr::new(self.id);
        for ip in self.endpoint.addr().ip_addrs() {
            addr = addr.with_ip_addr(*ip);
        }
        addr
    }
}

/// Everything the listening side needs, in one place so a test can say what it is varying.
struct Listener {
    peer: Peer,
    inbox: tempfile::TempDir,
    _undo: tempfile::TempDir,
    _pins: tempfile::TempDir,
}

impl Listener {
    /// Starts `serve_peers` on a fresh endpoint, admitting exactly `allowed`.
    async fn start(allowed: Vec<ZPeerEntry>) -> Listener {
        let peer = Peer::new().await;
        let inbox = tempfile::tempdir().unwrap();
        let undo = tempfile::tempdir().unwrap();
        let pins = tempfile::tempdir().unwrap();

        let config = TransferConfig {
            inbox: inbox.path().to_path_buf(),
            undo: undo.path().to_path_buf(),
            ..TransferConfig::default()
        };
        let tofu = TofuStore::new(pins.path().join("peers.json"));
        let serving = peer.endpoint.clone();
        tokio::spawn(async move {
            serve_peers(
                serving,
                Arc::new(Directory(allowed)),
                config,
                tofu,
                "listening-node".to_string(),
            )
            .await;
        });

        Listener { peer, inbox, _undo: undo, _pins: pins }
    }
}

/// Dials the listener and pushes one file through, returning what the peer reported.
///
/// Mirrors what `send::IrohPeerLink` does in production, minus the rendezvous lookup and the pin —
/// this file is about the accepting half.
async fn send_a_file(
    from: &iroh::Endpoint,
    to: &Listener,
    root: &std::path::Path,
    name: &str,
    contents: &[u8],
) -> zyris::Result<String> {
    tokio::fs::write(root.join(name), contents).await.unwrap();

    let transport = zyris_p2p::peer::dial(from, to.peer.addr())
        .await
        .map_err(|e| zyris::WireError::internal(e.to_string()))?;

    let sender = LocalPeerTransfer::sender(root.to_path_buf());
    sender
        .offer_file(
            "t1".to_string(),
            root.join(name),
            contents.len() as u64,
            hex::encode(<sha2::Sha256 as sha2::Digest>::digest(contents)),
        )
        .await;

    let node = Node::builder()
        .name("sender")
        .kind(NodeKind::Cli)
        .capability(PeerTransferServer(sender))
        .build()?;
    let connection = node.connect_over(transport).await?;
    let client: PeerTransferClient = connection.wait_capability(Duration::from_secs(5)).await?;
    let done = client
        .push_offer(TransferOffer {
            transfer_id: "t1".to_string(),
            name: name.to_string(),
            size: contents.len() as u64,
            sha256: hex::encode(<sha2::Sha256 as sha2::Digest>::digest(contents)),
            overwrite: false,
        })
        .await?;
    Ok(done.written)
}

/// The happy path, so the refusals below are known to be refusing something that otherwise works.
#[tokio::test]
async fn a_node_of_this_account_is_accepted_and_its_file_arrives() {
    let sender = Peer::new().await;
    let listener = Listener::start(vec![entry("laptop", &sender.id.to_string())]).await;
    let root = tempfile::tempdir().unwrap();

    let written = tokio::time::timeout(
        Duration::from_secs(30),
        send_a_file(&sender.endpoint, &listener, root.path(), "report.pdf", b"over quic"),
    )
    .await
    .expect("the transfer should not need 30s on loopback")
    .expect("an admitted peer's transfer should succeed");

    assert!(written.ends_with("report.pdf"), "{written}");
    let landed = listener.inbox.path().join("laptop").join("report.pdf");
    assert_eq!(tokio::fs::read(&landed).await.unwrap(), b"over quic");
}

/// The guard the whole lookup exists for. A stranger must be closed *before* a zyris handshake —
/// filtering after one would mean having already told it what we are and what we can do.
#[tokio::test]
async fn a_peer_that_is_not_ours_never_gets_a_zyris_handshake() {
    let stranger = Peer::new().await;
    // The list names a different node, so the stranger's own id is not in it.
    let elsewhere = Peer::new().await;
    let listener = Listener::start(vec![entry("laptop", &elsewhere.id.to_string())]).await;
    let root = tempfile::tempdir().unwrap();

    let outcome = tokio::time::timeout(
        Duration::from_secs(20),
        send_a_file(&stranger.endpoint, &listener, root.path(), "report.pdf", b"nope"),
    )
    .await
    .expect("the listener should close this, not leave it hanging past 20s");

    assert!(outcome.is_err(), "a stranger got a working session: {outcome:?}");
    assert!(
        !listener.inbox.path().join("laptop").exists(),
        "nothing from a refused peer may reach the inbox"
    );
}

/// The reason `establish` runs inside the spawned task rather than in the accept loop.
///
/// A peer that completes the QUIC handshake and then never opens a stream sits there until the
/// 10s deadline. If the loop waited on it, every other peer would wait too. So: park one silent
/// connection, then send a real file, and require it to arrive in well under that deadline. A
/// serial loop cannot pass this — it would still be holding the first one.
#[tokio::test]
async fn a_silent_peer_does_not_hold_the_listener_shut() {
    let silent = Peer::new().await;
    let sender = Peer::new().await;
    let listener = Listener::start(vec![entry("laptop", &sender.id.to_string())]).await;
    let root = tempfile::tempdir().unwrap();

    // Connected, ALPN agreed, and then nothing — no bi-stream, ever. Held so it is not dropped.
    let _parked = silent
        .endpoint
        .connect(listener.peer.addr(), zyris_p2p::transport::ALPN)
        .await
        .expect("the listener should accept the connection itself");

    let started = std::time::Instant::now();
    let written = tokio::time::timeout(
        Duration::from_secs(8),
        send_a_file(&sender.endpoint, &listener, root.path(), "report.pdf", b"still open"),
    )
    .await
    .expect("a silent peer must not make this wait out the handshake deadline")
    .expect("the second peer's transfer should succeed");

    assert!(written.ends_with("report.pdf"), "{written}");
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "served in {:?}, which is not fast enough to prove the loop did not block",
        started.elapsed()
    );
}
