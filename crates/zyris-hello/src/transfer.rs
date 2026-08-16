//! Node-to-node file transfer, wired up.
//!
//! Everything this uses already existed; what was missing was anything calling it. Three things
//! have to happen, and only the first is obvious:
//!
//! 1. announce `file_transfer`, so an agent has a tool to call;
//! 2. **publish this node's endpoint**, or a peer looking us up finds nothing — until this,
//!    `AttaccaApi::peer_publish` had no caller anywhere outside a test, which meant `peer_lookup`
//!    had nothing to answer with and no transfer could have started;
//! 3. accept incoming connections, or transfers only ever go one way.
//!
//! Two and three need the connection to Attacca, which does not exist when a node builds the
//! capabilities it announces — hence `LocalFileTransfer::pending` and `set_api`, and hence the
//! on-connect hook here.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use zyris::Connection;
use zyris_attacca::{AttaccaApi, AttaccaApiClient};
use zyris_caps::FileTransferServer;
use zyris_capkit::transfer::listen::serve_peers;
use zyris_capkit::transfer::{
    FileTransferConfig, IrohPeerLink, LocalFileTransfer, TransferConfig,
};
use zyris_p2p::fingerprint::{fingerprint, PeerConfirmer};
use zyris_p2p::iroh;
use zyris_p2p::tofu::TofuStore;

/// Everything the transfer half of this node holds on to between connections.
pub struct Transfer {
    transfer: LocalFileTransfer,
    endpoint: iroh::Endpoint,
    config: TransferConfig,
    tofu: TofuStore,
    node_id: String,
    /// `on_connect` runs again on every reconnect. Publishing again is right — addresses move.
    /// Starting a second accept loop on the same endpoint is not.
    listening: AtomicBool,
}

impl Transfer {
    /// Binds this node's peer endpoint and builds the capability, with no rendezvous yet.
    ///
    /// Paths come from the environment so a node can be pointed at a directory it is willing to
    /// serve. `ZYRIS_TRANSFER_ROOT` bounds what `send_to` will read — it is the read jail, so a
    /// node that sets it to `/` has turned the jail off and should mean to.
    pub async fn bind(node_name: &str) -> anyhow::Result<Transfer> {
        let root = dir("ZYRIS_TRANSFER_ROOT", "transfers")?;
        let inbox = dir("ZYRIS_TRANSFER_INBOX", "transfers/inbox")?;
        let undo = dir("ZYRIS_TRANSFER_UNDO", "transfers/undo")?;
        let pins = dir("ZYRIS_TRANSFER_PINS", "transfers")?;
        let key_path = pins.join("peer_key");

        // `N0` brings relays and discovery, which is what makes a node behind NAT reachable at all
        // — but its relays are n0's public ones. They carry the connection, not its contents (the
        // QUIC session is end to end), yet they still see who talks to whom and when. A deployment
        // running its own relay says so with `ZYRIS_RELAY_URL`, and this logs which it ended up on
        // rather than leaving that to be discovered from a packet capture.
        // **Handed a key, not left to generate one.** `zyris_p2p::key` writes it `0600` and reads
        // the same one back next time, which is what makes this node the same node run to run.
        // Without it iroh mints a fresh identity at every start, and a peer that pinned this one
        // after comparing fingerprints would meet a stranger the next time it came up — the pin
        // would expire with the process, which is not a pin. It hides well, too: the accept loop
        // never consults a pin, so receiving keeps working and only sending breaks.
        let secret = zyris_p2p::key::load_or_create(&key_path).await?;

        let builder = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
            .secret_key(secret)
            .alpns(vec![zyris_p2p::transport::ALPN.to_vec()]);
        let builder = match std::env::var("ZYRIS_RELAY_URL").ok().filter(|u| !u.trim().is_empty()) {
            Some(url) => {
                let parsed: iroh::RelayUrl = url.parse()?;
                tracing::info!(relay = %parsed, "using this deployment's own relay");
                builder.relay_mode(iroh::RelayMode::Custom(parsed.into()))
            }
            None => {
                tracing::info!("no ZYRIS_RELAY_URL; falling back on the public relays");
                builder
            }
        };
        let endpoint = builder.bind().await?;
        let node_id = endpoint.id().to_string();
        tracing::info!(
            endpoint_id = %node_id,
            fingerprint = %fingerprint(&node_id).unwrap_or_default(),
            "peer endpoint bound"
        );

        let tofu = TofuStore::new(pins.join("peers.json"));
        let config = TransferConfig { inbox, undo, ..TransferConfig::default() };
        let transfer = LocalFileTransfer::pending(
            FileTransferConfig {
                root,
                inbox: config.inbox.clone(),
                node_id: node_id.clone(),
                ..FileTransferConfig::default()
            },
            tofu.clone(),
            Arc::new(AskTheTerminal { node_name: node_name.to_string() }),
            Arc::new(IrohPeerLink::new(endpoint.clone())),
        );

        Ok(Transfer {
            transfer,
            endpoint,
            config,
            tofu,
            node_id,
            listening: AtomicBool::new(false),
        })
    }

    pub fn capability(&self) -> FileTransferServer<LocalFileTransfer> {
        FileTransferServer(self.transfer.clone())
    }

    /// Called on every connect. Publishes where this node can be reached, and starts the accept
    /// loop the first time.
    pub async fn on_connect(&self, conn: &Connection) {
        let Ok(api) = conn.wait_capability::<AttaccaApiClient>(super::CONSUME_WAIT).await else {
            tracing::warn!("no attacca_api on this connection; file transfer stays offline");
            return;
        };
        self.transfer.set_api(api);

        // Read after the endpoint has had a moment to learn its own addresses. An empty list is
        // still worth publishing: the endpoint id alone is dialable through the relay.
        let addrs: Vec<String> =
            self.endpoint.addr().ip_addrs().map(|a| a.to_string()).collect();
        match conn.wait_capability::<AttaccaApiClient>(super::CONSUME_WAIT).await {
            Ok(api) => match api.peer_publish(self.node_id.clone(), addrs.clone()).await {
                Ok(()) => tracing::info!(
                    endpoint_id = %self.node_id,
                    addrs = addrs.len(),
                    "published this node's peer address"
                ),
                Err(error) => tracing::warn!(%error, "could not publish; peers cannot find us"),
            },
            Err(error) => tracing::warn!(%error, "attacca_api went away before publishing"),
        }

        if self.listening.swap(true, Ordering::SeqCst) {
            return;
        }
        // The loop below starts once and outlives this connection, so it is given the transfer's
        // own rendezvous handle rather than this connection's client. `set_api` above keeps that
        // handle current; a client captured here would be dead after the first reconnect and the
        // cache behind it would never refresh again.
        let directory = self.transfer.rendezvous();
        let endpoint = self.endpoint.clone();
        let config = self.config.clone();
        let tofu = self.tofu.clone();
        let node_id = self.node_id.clone();
        tokio::spawn(async move {
            tracing::info!("accepting peer connections");
            serve_peers(endpoint, Arc::new(directory), config, tofu, node_id).await;
            tracing::warn!("the accept loop ended");
        });
    }
}

fn dir(key: &str, default: &str) -> anyhow::Result<std::path::PathBuf> {
    let path = std::env::var_os(key)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(default));
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

/// Asks whoever is running this node, on its terminal.
///
/// `zyris-p2p` ships `DenyUnknown` and deliberately nothing else — what counts as confirmed is the
/// node's decision, not the transport's. For a reference node started by hand, the honest answer is
/// the person who started it: print the fingerprint, and let them compare it against the one the
/// other machine printed at startup.
///
/// With no terminal there is nobody to ask, and the answer is no. That is `DenyUnknown`'s reasoning
/// and it does not change here: an unknown peer must not be trusted merely because nobody was
/// around to refuse it.
struct AskTheTerminal {
    node_name: String,
}

#[async_trait::async_trait]
impl PeerConfirmer for AskTheTerminal {
    async fn confirm(&self, label: &str, endpoint_id: &str) -> bool {
        let shown = fingerprint(endpoint_id).unwrap_or_else(|_| endpoint_id.to_string());
        if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            tracing::warn!(
                peer = %label,
                fingerprint = %shown,
                "no terminal to confirm this peer on; refusing"
            );
            return false;
        }
        // `println!`, not `tracing`: this is a question for a person, and it has to appear even
        // when the log filter is turned down.
        println!("\n{}: is this {label}'s fingerprint?\n    {shown}", self.node_name);
        println!("Compare it with what that machine printed when it started. [y/N]");

        let answer = tokio::task::spawn_blocking(|| {
            let mut line = String::new();
            std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut line).ok()?;
            Some(line)
        })
        .await
        .ok()
        .flatten()
        .unwrap_or_default();

        let yes = matches!(answer.trim(), "y" | "Y" | "yes");
        if yes {
            tracing::info!(peer = %label, "confirmed; pinning this key");
        } else {
            tracing::warn!(peer = %label, "not confirmed; refusing");
        }
        yes
    }
}
