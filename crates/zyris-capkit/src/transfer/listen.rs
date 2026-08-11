//! Taking incoming peer connections. **Not from anyone who asks.**
//!
//! An arriving connection is identified by one thing only: the `EndpointId` QUIC authenticated it
//! against. To turn that into "one of my own machines" this asks the rendezvous for the account's
//! node list and looks the id up in it. Anything not on that list is closed without a zyris
//! handshake ever happening.
//!
//! # Why nothing is pinned here
//!
//! The sending side pins, because it has something worth keying a pin on: the name the *caller*
//! typed, which attacca has no channel to reassign. This side has no such string. It knows an
//! `EndpointId` and whatever name the rendezvous attaches to it — and `ZPeerEntry::slug`'s own doc
//! is explicit that the slug is not a trust anchor: it is derived from a node `name` whose default
//! is the enrolling device's unverified self-report, it is neither unique nor stable, and freeing a
//! revoked node's slug so a *different* node can take it is deliberate, tested behaviour.
//!
//! So a pin written here would be keyed on a string the server chooses. That is the exact shape
//! `zyris_p2p::tofu`'s module doc calls out: a substituted peer arrives under a name nothing is
//! pinned under, passes, and gets pinned — every time, leaving no mark. Asking a person first does
//! not fix it, because the person would be confirming a name attacca picked.
//!
//! What this side does instead is **check and never write**. A slug the user has already pinned by
//! sending to it refuses a changed key, which is the protection that was ever real. A slug nobody
//! has pinned is let through and stays unpinned, so the ledger only ever holds names a person
//! actually said. The cost is that a peer we only receive from is never pinned; the alternative was
//! pinning it under a name that could not carry the weight.
//!
//! # Two things that would otherwise stall or amplify
//!
//! The accept loop never waits on a peer. `accept_next` returns as soon as there is a connection to
//! work with, and the handshake — which a peer can simply decline to finish — runs under a deadline
//! inside the spawned task. Waiting here would let one silent peer hold the whole listener shut.
//!
//! The node list is cached, and a lookup miss does **not** get to force a refresh: an unknown peer
//! that could make us re-query on every connection would be an amplifier pointed at the rendezvous
//! by anyone who can open a socket. Refreshes are rate-limited independently of what asks for them.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use zyris::{AcceptOptions, Node, NodeKind, Result};
use zyris_attacca::{AttaccaApi, AttaccaApiClient, ZPeerEntry};
use zyris_caps::peer_transfer::PeerTransferServer;
use zyris_p2p::iroh;
use zyris_p2p::tofu::TofuStore;

use super::peer::{LocalPeerTransfer, TransferConfig};

/// How long a fetched node list is treated as current.
pub const DEFAULT_DIRECTORY_TTL: Duration = Duration::from_secs(60);
/// The shortest gap between two fetches, whatever asks for them.
pub const DEFAULT_REFRESH_INTERVAL: Duration = Duration::from_secs(10);
/// How long a peer gets to finish the QUIC handshake and open its stream.
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);
/// How long to wait for the peer to announce `peer_transfer` once connected.
const CAPABILITY_WAIT: Duration = Duration::from_secs(5);

/// Where the account's node list comes from.
///
/// A trait rather than the client directly so the cache's decisions — is this id ours, and may we
/// fetch again yet — can be tested for what they are, without a rendezvous to talk to and without
/// the test having to be about timing.
#[async_trait::async_trait]
pub trait PeerDirectory: Send + Sync {
    async fn peers(&self) -> Result<Vec<ZPeerEntry>>;
}

#[async_trait::async_trait]
impl PeerDirectory for AttaccaApiClient {
    async fn peers(&self) -> Result<Vec<ZPeerEntry>> {
        AttaccaApi::peer_list(self).await
    }
}

/// The account's nodes, by `EndpointId`, with both limits the module doc describes.
pub struct PeerCache {
    directory: Arc<dyn PeerDirectory>,
    ttl: Duration,
    refresh_interval: Duration,
    by_endpoint: HashMap<String, ZPeerEntry>,
    /// When the list last came back. `None` until the first successful fetch.
    fetched: Option<Instant>,
    /// When a fetch was last *attempted*, successful or not. This is what the interval bounds —
    /// counting only successes would let a rendezvous that is down be retried as fast as peers
    /// arrive.
    attempted: Option<Instant>,
}

impl PeerCache {
    pub fn new(
        directory: Arc<dyn PeerDirectory>,
        ttl: Duration,
        refresh_interval: Duration,
    ) -> PeerCache {
        PeerCache {
            directory,
            ttl,
            refresh_interval,
            by_endpoint: HashMap::new(),
            fetched: None,
            attempted: None,
        }
    }

    /// The entry for `endpoint_id`, or `None` if this account has no such node.
    ///
    /// A hit on a list still inside its TTL answers immediately. A miss is the interesting case:
    /// the id may be genuinely unknown, or the list may just be older than the node. Refreshing on
    /// every miss is what turns an unknown peer into an amplifier, so a miss only refreshes when
    /// [`Self::refresh_interval`] has elapsed since the last attempt — and answers from the list it
    /// has otherwise. Being briefly wrong about a node that enrolled seconds ago costs a retry;
    /// being an open amplifier costs the rendezvous.
    pub async fn find(&mut self, endpoint_id: &str) -> Option<ZPeerEntry> {
        let fresh = self.fetched.is_some_and(|at| at.elapsed() < self.ttl);
        if fresh {
            if let Some(entry) = self.by_endpoint.get(endpoint_id) {
                return Some(entry.clone());
            }
        }
        if self.may_attempt() {
            self.refresh().await;
        }
        self.by_endpoint.get(endpoint_id).cloned()
    }

    fn may_attempt(&self) -> bool {
        self.attempted.is_none_or(|at| at.elapsed() >= self.refresh_interval)
    }

    /// Replaces the list wholesale, so a node that was revoked stops being found. A failed fetch
    /// leaves the previous list in place: the rendezvous being unreachable is not evidence that
    /// anyone's nodes went away, and dropping the list there would refuse every peer until it came
    /// back.
    async fn refresh(&mut self) {
        self.attempted = Some(Instant::now());
        match self.directory.peers().await {
            Ok(entries) => {
                self.by_endpoint = entries
                    .into_iter()
                    .map(|entry| (entry.endpoint_id.clone(), entry))
                    .collect();
                self.fetched = Some(Instant::now());
            }
            Err(error) => {
                tracing::warn!(%error, "could not refresh the node list; keeping the one we have");
            }
        }
    }
}

/// Accepts peer connections until `endpoint` stops producing them.
///
/// `node_id` is this node's own identifier, announced to the peer so it knows who answered.
/// It is a label on the wire and nothing here is authorized by it.
pub async fn serve_peers(
    endpoint: iroh::Endpoint,
    api: AttaccaApiClient,
    config: TransferConfig,
    tofu: TofuStore,
    node_id: String,
) {
    let directory = Arc::new(Mutex::new(PeerCache::new(
        Arc::new(api),
        DEFAULT_DIRECTORY_TTL,
        DEFAULT_REFRESH_INTERVAL,
    )));

    while let Some(accepting) = zyris_p2p::peer::accept_next(&endpoint).await {
        let config = config.clone();
        let directory = directory.clone();
        // Cloned, not rebuilt from the path: clones share the write lock, so two connections
        // pinning at once cannot overwrite one another (Task 2.4).
        let tofu = tofu.clone();
        let node_id = node_id.clone();
        tokio::spawn(async move {
            serve_one(accepting, config, directory, tofu, node_id).await;
        });
    }
}

async fn serve_one(
    accepting: zyris_p2p::peer::PendingConnection,
    config: TransferConfig,
    directory: Arc<Mutex<PeerCache>>,
    tofu: TofuStore,
    node_id: String,
) {
    let (peer, transport) =
        match zyris_p2p::peer::establish(accepting, HANDSHAKE_DEADLINE).await {
            Ok(established) => established,
            Err(error) => {
                tracing::warn!(%error, "the handshake did not finish; closing");
                return;
            }
        };
    let peer = peer.to_string();

    let Some(entry) = directory.lock().await.find(&peer).await else {
        tracing::warn!(%peer, "not a node of this account; closing");
        // Dropping `transport` closes the connection.
        return;
    };

    // The slug names which peer this is, and that is all it is used for here — the inbox
    // subdirectory it files under, which `Inbox::resolve` washes into a single safe component.
    // Nothing is authorized by it. See this module's docs for why it is not pinned under either.
    let slug = entry.slug.clone();

    // Read-only. A slug the sending side pinned refuses a changed key here too; a slug nobody has
    // pinned passes and stays unpinned.
    if let Err(error) = tofu.check(&slug, &peer).await {
        tracing::error!(%peer, %slug, %error, "the peer's key is not the one pinned for this name; refusing");
        return;
    }

    let receiving = LocalPeerTransfer::receiver_pending(config, slug.clone());
    let node = match Node::builder()
        .name("peer")
        .kind(NodeKind::Cli)
        // The receiving side offers one capability and one only, the same way the sending side
        // does. A peer link is not a node connection that happens to be filtered down.
        .capability(PeerTransferServer(receiving.clone()))
        .build()
    {
        Ok(node) => node,
        Err(error) => {
            tracing::error!(%error, "could not build the peer-side node");
            return;
        }
    };

    let options = AcceptOptions { node_id, ..AcceptOptions::default() };
    let connection = match node.accept(transport, options).await {
        Ok(connection) => connection,
        Err(error) => {
            tracing::warn!(%peer, %error, "the zyris handshake failed; closing");
            return;
        }
    };

    // The handle `push_offer` pulls the bytes back through. Without it `push_offer` refuses, which
    // is the right failure — but it is a failure, so say why it happened.
    match connection.wait_capability(CAPABILITY_WAIT).await {
        Ok(client) => receiving.set_peer(client),
        Err(error) => {
            tracing::warn!(%peer, %slug, %error, "the peer never announced peer_transfer; it cannot send to us");
        }
    }

    connection.closed().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn entry(slug: &str, endpoint_id: &str) -> ZPeerEntry {
        ZPeerEntry {
            node_id: format!("node-{slug}"),
            slug: slug.to_string(),
            endpoint_id: endpoint_id.to_string(),
            online: true,
        }
    }

    /// Answers with whatever it is holding and counts how often it was asked.
    struct Directory {
        entries: Mutex<Vec<ZPeerEntry>>,
        calls: AtomicUsize,
    }

    impl Directory {
        fn new(entries: Vec<ZPeerEntry>) -> Arc<Directory> {
            Arc::new(Directory { entries: Mutex::new(entries), calls: AtomicUsize::new(0) })
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl PeerDirectory for Directory {
        async fn peers(&self) -> Result<Vec<ZPeerEntry>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.entries.lock().await.clone())
        }
    }

    #[tokio::test]
    async fn a_node_of_this_account_is_found_and_the_list_is_fetched_once() {
        let directory = Directory::new(vec![entry("laptop", "abc")]);
        let mut cache =
            PeerCache::new(directory.clone(), Duration::from_secs(60), Duration::from_secs(10));

        assert_eq!(cache.find("abc").await.unwrap().slug, "laptop");
        assert_eq!(cache.find("abc").await.unwrap().slug, "laptop");
        assert_eq!(directory.calls(), 1, "a hit inside the TTL must not re-ask");
    }

    /// The amplification guard. Without it, anyone able to open a socket could make this node
    /// re-query the rendezvous once per connection just by not being on the list.
    #[tokio::test]
    async fn an_unknown_peer_cannot_force_a_fetch_per_connection() {
        let directory = Directory::new(vec![entry("laptop", "abc")]);
        let mut cache =
            PeerCache::new(directory.clone(), Duration::from_secs(60), Duration::from_secs(10));

        for _ in 0..20 {
            assert!(cache.find("stranger").await.is_none());
        }
        assert_eq!(directory.calls(), 1, "twenty unknown peers, one fetch");
    }

    /// A node that enrolled after the list was fetched is still reachable — the first miss is
    /// allowed to refresh, because nothing had been fetched yet.
    #[tokio::test]
    async fn a_node_that_appears_after_the_first_fetch_is_found_once_the_interval_passes() {
        let directory = Directory::new(vec![entry("laptop", "abc")]);
        let mut cache =
            PeerCache::new(directory.clone(), Duration::from_secs(60), Duration::ZERO);

        // Looked up by endpoint id — the only thing an arriving connection carries. The slug is
        // what comes back, not what goes in.
        assert!(cache.find("def").await.is_none());
        directory.entries.lock().await.push(entry("desk", "def"));

        assert_eq!(cache.find("def").await.unwrap().slug, "desk");
    }

    /// A revoked node has to stop being found, so a refresh replaces the list rather than merging
    /// into it.
    #[tokio::test]
    async fn a_revoked_node_stops_being_found_after_a_refresh() {
        let directory = Directory::new(vec![entry("laptop", "abc"), entry("desk", "def")]);
        let mut cache = PeerCache::new(directory.clone(), Duration::ZERO, Duration::ZERO);

        assert!(cache.find("def").await.is_some());
        directory.entries.lock().await.retain(|e| e.endpoint_id != "def");

        assert!(cache.find("def").await.is_none(), "a revoked node must not survive in the cache");
    }

    /// A rendezvous that is down is not evidence that anyone's nodes went away.
    #[tokio::test]
    async fn a_failed_refresh_keeps_the_list_it_already_had() {
        struct Failing {
            calls: AtomicUsize,
        }
        #[async_trait::async_trait]
        impl PeerDirectory for Failing {
            async fn peers(&self) -> Result<Vec<ZPeerEntry>> {
                if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    Ok(vec![entry("laptop", "abc")])
                } else {
                    Err(zyris::WireError::internal("the rendezvous is unreachable".to_string()))
                }
            }
        }

        let directory = Arc::new(Failing { calls: AtomicUsize::new(0) });
        let mut cache = PeerCache::new(directory, Duration::ZERO, Duration::ZERO);

        assert!(cache.find("abc").await.is_some());
        assert!(cache.find("abc").await.is_some(), "a failed fetch must not empty the list");
    }
}
