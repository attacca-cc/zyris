//! The sending side: what an agent calls to put a file on another node of the same account.
//!
//! Everything else under [`super`] is the receiving side. This module is the half that says
//! "send", and it is the only place in the crate that talks to the rendezvous
//! (`zyris-attacca`) or to the peer transport (`zyris-p2p`).
//!
//! # What guards what
//!
//! | Guard | Where | What it stops |
//! |---|---|---|
//! | The read jail | [`LocalFileTransfer::resolve_source`] | Sending a file that is not under the node's root |
//! | The answer check | [`LocalFileTransfer::look_up_peer`] | Sending to a machine the caller did not name |
//! | The pin | [`LocalFileTransfer::authorize_peer`] | Sending to a peer whose key is not the one a person confirmed |
//!
//! # Why the peer link is behind a trait
//!
//! [`PeerLink`] exists so the three guards above can be exercised end to end — through the real
//! [`super::peer::LocalPeerTransfer`], the real `push_offer`/`pull` exchange, onto a real file —
//! with no socket in it. [`IrohPeerLink`] is the production implementation and the only part of
//! this module that touches QUIC. That split is the same one the crates themselves already draw:
//! `zyris-capkit` implements capabilities, `zyris-p2p` carries them.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use sha2::{Digest, Sha256};
use zyris::{ErrorCode, Node, NodeKind, Result, WireError};
use zyris_attacca::{AttaccaApi, AttaccaApiClient, ZPeerAddr};
use zyris_caps::file_transfer::{FileTransfer, InboxEntry, SendReceipt};
use zyris_caps::peer_transfer::{
    PeerTransfer, PeerTransferClient, PeerTransferServer, TransferOffer,
};
use zyris_p2p::fingerprint::PeerConfirmer;
use zyris_p2p::iroh;
use zyris_p2p::tofu::{TofuError, TofuStore};

use super::peer::{LocalPeerTransfer, TRANSFER_IN_FLIGHT};

/// How long the whole of `send_to` gets before it answers `pending` instead of finishing.
///
/// Attacca cuts a node call off at 60 seconds with a `Timeout` error. Coming back at 55 with a
/// deliberate `pending: true` beats being cut off: the agent is told to call again in a reply that
/// is not shaped like a failure, and the call it makes resumes rather than restarts.
pub const DEFAULT_WIRE_DEADLINE: Duration = Duration::from_secs(55);

/// How long [`IrohPeerLink`] waits for the peer to announce `peer_transfer` after the zyris
/// handshake. The peer announces during its own handshake, so this only ever absorbs scheduling
/// jitter; a peer that has not announced by now is not going to.
const CAPABILITY_WAIT: Duration = Duration::from_secs(5);

/// How much of the file to hash per read. Same size as the transfer chunk, for the same reason:
/// a fixed buffer instead of loading a file that may be gigabytes into memory to hash it.
const HASH_CHUNK: usize = 64 * 1024;

/// Bytes of `sha256(node_id ‖ name ‖ size ‖ sha256)` kept as the `transfer_id`.
const TRANSFER_ID_BYTES: usize = 16;

/// How far [`LocalFileTransfer::send_inner`] had got when the deadline ran out.
///
/// Only one of the three has anything on the peer's disk to come back to, and the answer has to say
/// which. "Call again and it resumes" is true once bytes are moving; said while the file is still
/// being measured it points the caller at a `.part` that was never created, and the caller — an
/// agent, which will do exactly what the `next` line says — retries forever against a step that
/// starts from zero every time.
mod phase {
    /// Resolving the path and reading the file to size and hash it. Nothing has left this machine,
    /// and nothing has been asked of anyone.
    pub const MEASURING: u8 = 0;
    /// Looking the peer up, settling its key, opening the link. Still nothing sent.
    pub const REACHING: u8 = 1;
    /// `push_offer` is in flight and the peer is pulling. A partial file may exist over there.
    pub const SENDING: u8 = 2;
}

/// One open link to a peer, for the length of one transfer.
pub struct PeerSession {
    /// The handle `push_offer` is called through.
    pub client: PeerTransferClient,
    /// Held for as long as the transfer runs. The peer calls `pull` back over this same
    /// connection while it is answering `push_offer`, so dropping it early would cut the bytes
    /// off mid-flight.
    pub connection: zyris::Connection,
}

/// How `send_to` reaches a peer. Implemented by [`IrohPeerLink`] in production.
#[async_trait::async_trait]
pub trait PeerLink: Send + Sync {
    /// Connects to `addr` and announces `sender` — **`peer_transfer` and nothing else** — on the
    /// resulting link.
    ///
    /// `sender` is taken by value because the link owns it from here on: it is the object the
    /// peer's `pull` calls land on, and it has to stay alive as long as the connection does.
    async fn open(&self, addr: &ZPeerAddr, sender: LocalPeerTransfer) -> Result<PeerSession>;

    /// Whether bytes to `endpoint_id` are travelling a direct path rather than through the relay.
    ///
    /// Asked **after** the transfer, not before: iroh starts a connection over the relay and
    /// upgrades it once hole punching lands, so a reading taken at connect time reports "relayed"
    /// for a transfer that spent almost all of itself direct.
    ///
    /// A link with no way to tell answers `false`, which is the honest reading of "we cannot claim
    /// this was direct" — never a claim that it was.
    async fn is_direct(&self, endpoint_id: &str) -> bool {
        let _ = endpoint_id;
        false
    }
}

/// The production [`PeerLink`]: an iroh QUIC connection carrying zyris.
pub struct IrohPeerLink {
    endpoint: iroh::Endpoint,
}

impl IrohPeerLink {
    pub fn new(endpoint: iroh::Endpoint) -> IrohPeerLink {
        IrohPeerLink { endpoint }
    }

    /// Dials the peer, with the rendezvous's addresses as hints and **iroh's own discovery as the
    /// fallback**.
    ///
    /// The two steps are not redundant, and leaving the second one out is how a transfer that
    /// should work fails. A peer publishes the addresses it can see on itself, and behind NAT — a
    /// container, a laptop on wifi — those are private ones that mean nothing here. Handing iroh an
    /// `EndpointAddr` carrying them is not a hint, it is an assertion: iroh takes it to mean the
    /// caller knows where this peer is, dials only there, and never asks discovery. So a peer that
    /// is perfectly reachable is refused because of the addresses it published about itself.
    ///
    /// Measured against a node in a Kubernetes sandbox on another continent: dialing its published
    /// address failed, and dialing the same endpoint id with no hints at all connected in 1.1
    /// seconds. Both machines were on public n0 relays, and neither had been told anything about
    /// the other's.
    ///
    /// The hints are still tried first, because when they *are* reachable — two machines on one
    /// desk — they are the direct path, and discovery would be a slower route to the same place.
    async fn dial(&self, addr: &ZPeerAddr) -> Result<zyris_p2p::transport::IrohTransport> {
        let hinted = endpoint_addr(addr)?;
        let hinted_at_something = hinted.ip_addrs().next().is_some() || hinted.relay_urls().next().is_some();

        let first = match zyris_p2p::peer::dial(&self.endpoint, hinted).await {
            Ok(transport) => return Ok(transport),
            Err(e) if !hinted_at_something => {
                // Nothing was hinted, so that attempt already was the discovery one.
                return Err(refuse("peer_unreachable", e.to_string()).retriable(true));
            }
            Err(e) => e,
        };

        let id = addr.endpoint_id.parse::<iroh::EndpointId>().map_err(|e| {
            refuse("peer_unreachable", format!("{} is not a usable endpoint id: {e}", addr.endpoint_id))
        })?;
        tracing::debug!(
            endpoint_id = %addr.endpoint_id,
            error = %first,
            "the peer's published addresses did not reach it; asking discovery"
        );
        zyris_p2p::peer::dial(&self.endpoint, iroh::EndpointAddr::new(id)).await.map_err(|second| {
            // Both failures, because they usually say different things: the first is what the
            // published addresses did, the second is what discovery could not find.
            refuse(
                "peer_unreachable",
                format!("its published addresses: {first}; and by discovery: {second}"),
            )
            .retriable(true)
        })
    }
}

#[async_trait::async_trait]
impl PeerLink for IrohPeerLink {
    async fn open(&self, addr: &ZPeerAddr, sender: LocalPeerTransfer) -> Result<PeerSession> {
        let transport = self.dial(addr).await?;
        // One capability on this link and one only. The peer link is not a general-purpose node
        // connection that happens to be filtered down — it is built with nothing else on it.
        let node = Node::builder()
            .name("zyris-transfer")
            .kind(NodeKind::Cli)
            .capability(PeerTransferServer(sender))
            .build()?;
        let connection = node.connect_over(transport).await?;
        let client: PeerTransferClient = connection.wait_capability(CAPABILITY_WAIT).await?;
        Ok(PeerSession { client, connection })
    }

    async fn is_direct(&self, endpoint_id: &str) -> bool {
        let Ok(id) = endpoint_id.parse::<iroh::EndpointId>() else { return false };
        let Some(info) = self.endpoint.remote_info(id).await else { return false };
        // The binding is not redundant, however much it looks it. As the block's tail expression,
        // `addrs()`'s iterator would be a temporary living to the end of the block — dropped *after*
        // `info`, which it borrows, which does not compile (E0597) in a desugared `async_trait`
        // body. Naming the bool ends the borrow on this line.
        let direct = info.addrs().any(|a| {
            a.addr().is_ip() && matches!(a.usage(), iroh::endpoint::TransportAddrUsage::Active)
        });
        direct
    }
}

/// Turns the rendezvous's answer into something iroh can dial.
///
/// Unparseable candidate addresses are dropped rather than failing the dial: the list is a set of
/// hints, and one bad hint among several good ones is not a reason to refuse to connect. The
/// `endpoint_id` is the exception — it is the peer's identity, it is what QUIC authenticates
/// against, and there is nothing to dial without it.
fn endpoint_addr(addr: &ZPeerAddr) -> Result<iroh::EndpointAddr> {
    let id = addr.endpoint_id.parse::<iroh::EndpointId>().map_err(|e| {
        refuse("peer_unreachable", format!("{} is not a usable endpoint id: {e}", addr.endpoint_id))
    })?;
    let mut endpoint_addr = iroh::EndpointAddr::new(id);
    for candidate in &addr.addrs {
        if let Ok(socket) = candidate.parse::<std::net::SocketAddr>() {
            endpoint_addr = endpoint_addr.with_ip_addr(socket);
        }
    }
    if let Some(relay) = &addr.relay_url {
        if let Ok(url) = relay.parse::<iroh::RelayUrl>() {
            endpoint_addr = endpoint_addr.with_relay_url(url);
        }
    }
    Ok(endpoint_addr)
}

#[derive(Debug, Clone)]
pub struct FileTransferConfig {
    /// The only directory `send_to` will read out of. Everything outside is refused.
    pub root: PathBuf,
    /// Where received files land — what `inbox_list` reads. Normally the same path as
    /// [`super::TransferConfig::inbox`] on the same node.
    pub inbox: PathBuf,
    /// This node's own identifier, mixed into `transfer_id` so two nodes sending the same file
    /// under the same name do not collide in the receiver's `.part` slot.
    ///
    /// **A uniqueness salt, not a trust anchor.** Nothing is authorized by it and nothing is
    /// pinned under it — attacca issues it and can issue another one at will.
    pub node_id: String,
    pub wire_deadline: Duration,
}

impl Default for FileTransferConfig {
    fn default() -> FileTransferConfig {
        FileTransferConfig {
            root: PathBuf::from("."),
            inbox: PathBuf::from("."),
            node_id: String::new(),
            wire_deadline: DEFAULT_WIRE_DEADLINE,
        }
    }
}

/// The client this node asks Attacca where a peer is — **replaceable, because the connection it
/// belongs to is**.
///
/// It used to be a `OnceLock`, on the reasoning that a connection has one client and a slot that
/// can be overwritten is a slot that can be swapped. The first half is true and the second is the
/// wrong conclusion: a node's websocket drops and comes back — a laptop sleeps, a server rolls, a
/// network blips — and `Runner` reconnects and calls `set_api` again with a client for the new
/// connection. A write-once slot ignores that call and keeps the client bound to the dead one, so
/// every lookup after the first disconnect fails with `connection lost`, for good, on a node that
/// otherwise looks perfectly healthy. Observed live: a send worked, the socket reset once, and
/// every send after it failed identically until the process was restarted.
///
/// Nothing outside this node can reach `set`. The client always comes from a connection the node
/// itself established, so "could be overwritten" was never a way in — only a way to stay broken.
///
/// Cloning shares the slot, which is what lets one client serve both the send side and
/// [`super::listen::serve_peers`]'s directory.
#[derive(Clone, Default)]
pub struct Rendezvous(Arc<RwLock<Option<Arc<AttaccaApiClient>>>>);


impl Rendezvous {
    /// Replaces the client. Call on every connect.
    pub fn set(&self, api: AttaccaApiClient) {
        // A poisoned lock here would mean a panic while swapping a client, which is not a reason to
        // take the node down with it — the worst case is one stale client, which the next connect
        // replaces anyway.
        if let Ok(mut slot) = self.0.write() {
            *slot = Some(Arc::new(api));
        }
    }

    /// The current client, or `None` before the first connect.
    ///
    /// Returns an owned handle rather than a guard, so no caller can hold the lock across an
    /// `await` and block the next reconnect from installing its client.
    pub fn get(&self) -> Option<Arc<AttaccaApiClient>> {
        self.0.read().ok().and_then(|slot| slot.clone())
    }
}

/// The reference `file_transfer` implementation.
#[derive(Clone)]
pub struct LocalFileTransfer {
    config: FileTransferConfig,
    /// The rendezvous client, which does not exist yet when a node builds its capabilities: it
    /// arrives on the connection to Attacca, after `Runner::run` has been handed everything this
    /// node announces. See [`Rendezvous`] for why it is replaceable rather than write-once.
    api: Rendezvous,
    tofu: TofuStore,
    confirmer: Arc<dyn PeerConfirmer>,
    link: Arc<dyn PeerLink>,
}

impl LocalFileTransfer {
    pub fn new(
        config: FileTransferConfig,
        api: AttaccaApiClient,
        tofu: TofuStore,
        confirmer: Arc<dyn PeerConfirmer>,
        link: Arc<dyn PeerLink>,
    ) -> LocalFileTransfer {
        let transfer = LocalFileTransfer::pending(config, tofu, confirmer, link);
        transfer.set_api(api);
        transfer
    }

    /// The same thing, with the rendezvous client still to come.
    ///
    /// A node announces its capabilities before it connects, and the client is something the
    /// connection hands back — so a node that registers this one has no client to give it yet.
    /// Until [`Self::set_api`] is called, `send_to` refuses rather than pretending: there is no
    /// way to find a peer without something to ask.
    pub fn pending(
        config: FileTransferConfig,
        tofu: TofuStore,
        confirmer: Arc<dyn PeerConfirmer>,
        link: Arc<dyn PeerLink>,
    ) -> LocalFileTransfer {
        LocalFileTransfer { config, api: Rendezvous::default(), tofu, confirmer, link }
    }

    /// Plugs in the rendezvous client for the connection this node is on now.
    ///
    /// Call it on **every** connect, not just the first. See [`Rendezvous`].
    pub fn set_api(&self, api: AttaccaApiClient) {
        self.api.set(api);
    }

    /// The same handle this transfer looks peers up through, for a node that also wants to hand it
    /// to [`super::listen::serve_peers`] as its directory — one client, kept current in one place,
    /// serving both directions.
    pub fn rendezvous(&self) -> Rendezvous {
        self.api.clone()
    }

    /// Resolves a caller-supplied path **and refuses anything outside the root**.
    ///
    /// [`crate::path::resolve_under`] is explicitly not a jail — its own doc says "the root is a
    /// default, not a jail", an absolute path sails straight through it, and it never looks at the
    /// filesystem. Here it is used as one, so it needs the two things it does not do: the result is
    /// canonicalized (which resolves every symlink in the path, so a link planted inside the root
    /// that points out of it is followed to where it really goes) and then checked against the
    /// canonicalized root.
    ///
    /// Order matters. Checking the *un*-canonicalized path would pass a link whose target is
    /// outside, and canonicalizing without re-checking would pass an absolute path outright.
    ///
    /// **A window remains between this check and the reads that follow it.** The file is opened
    /// again to be hashed, and a third time by `pull` when the peer asks for the bytes; anyone who
    /// can write into the root between those points can swap the last component for a link (or a
    /// hard link) pointing somewhere this process can read and the caller cannot. Closing it for
    /// real means carrying one file descriptor from here through to the last read, which `pull`
    /// re-opening by path (`super::peer`) currently makes impossible. What bounds it today is that
    /// write access to the node's own root is already most of the way to owning the node.
    async fn resolve_source(&self, path: &str) -> Result<PathBuf> {
        let root = tokio::fs::canonicalize(&self.config.root).await.map_err(|e| {
            WireError::internal(format!(
                "this node's root {} could not be read: {e}",
                self.config.root.display()
            ))
        })?;
        let requested = crate::path::resolve_under(&root, path);
        let real = tokio::fs::canonicalize(&requested).await.map_err(|e| {
            refuse("file_not_found", format!("{} could not be read: {e}", requested.display()))
        })?;
        if !real.starts_with(&root) {
            return Err(refuse(
                "path_outside_root",
                format!("{path} is outside this node's root and will not be sent"),
            ));
        }
        let info = tokio::fs::metadata(&real)
            .await
            .map_err(|e| refuse("file_not_found", format!("{} could not be read: {e}", real.display())))?;
        if !info.is_file() {
            return Err(refuse("not_a_file", format!("{path} is not a regular file")));
        }
        Ok(real)
    }

    /// Asks the rendezvous which machine the caller meant.
    ///
    /// **A slug is neither unique nor stable, so two live nodes can answer to one name.**
    /// `AttaccaApi::peer_lookup`'s contract is that an implementation finding that must refuse
    /// rather than pick one, and refusing is the only safe answer: sending to an arbitrary one of
    /// them puts the file on a machine the caller did not name, and — because the pin is per-name
    /// — surfaces here as "this peer's key changed", a false alarm at exactly the place a real one
    /// has to be believed.
    ///
    /// **That refusal is not re-implemented here, because this call cannot see the collision.**
    /// The ambiguity is visible in `peer_list`, which returns every node with its `slug`, `node_id`
    /// and `online` flag; `peer_lookup` returns one already-chosen `ZPeerAddr`, so by the time the
    /// answer arrives the choice has been made and there is nothing left to count. Fetching
    /// `peer_list` as well to count it here would put the decision on a *second*, separately-timed
    /// answer from the same server that just gave the first — two answers that can disagree, and a
    /// new rule about which to believe. Against a rendezvous that is lying, neither call helps:
    /// both come from it.
    ///
    /// What is checked here is the one thing this call **can** see: that the answer names the peer
    /// that was asked for. An answer about some other node is refused rather than dialed.
    ///
    /// The comparison ignores ASCII case, because a deployment that canonicalizes slugs may well
    /// answer a lookup for `Laptop` with `laptop` and mean the same machine. **The pin's ledger key
    /// does not fold case** — see [`Self::authorize_peer`]. The two are asking different questions:
    /// this one asks whether the server answered about the peer that was named, and folding is
    /// tolerance; that one asks what the caller actually said, and folding there would quietly
    /// merge two slots into one.
    async fn look_up_peer(&self, node: &str) -> Result<ZPeerAddr> {
        let current = self.api.get();
        let Some(api) = current else {
            return Err(refuse(
                "rendezvous_unavailable",
                "this node is not connected to Attacca yet, so there is nothing to ask where \
                 a peer is".to_string(),
            )
            .retriable(true));
        };
        let addr = api.peer_lookup(node.to_string()).await?;
        if !addr.slug.eq_ignore_ascii_case(node) {
            return Err(refuse(
                "peer_lookup_mismatch",
                format!(
                    "asked for {node} and the rendezvous answered about {} ({}); \
                     refusing to send to a node that was not named",
                    addr.slug, addr.node_id
                ),
            ));
        }
        if !addr.online {
            return Err(refuse("peer_offline", format!("{node} is not connected"))
                .retriable(true));
        }
        Ok(addr)
    }

    /// Settles whether this key is the one a person confirmed for this name.
    ///
    /// `label` is the name **the caller asked for, verbatim** — not [`ZPeerAddr::slug`] and not
    /// [`ZPeerAddr::node_id`]. Both of those are minted by attacca, which is the party the pin
    /// exists to constrain: keying the ledger on either lets a substituted peer arrive under a
    /// freshly-issued identifier, land in a slot nothing has been pinned in, pass, and get pinned
    /// — every time, leaving no trace. The name the caller said is the one string in this call the
    /// server has no channel to reassign, so it is the one the ledger is keyed on. See
    /// `zyris_p2p::tofu`'s module docs and `ZPeerEntry::slug` in `zyris-attacca`.
    ///
    /// This is what makes the case attacca actually supports — freeing a revoked node's slug so a
    /// new node can take it — come out as a refusal here instead of a silent redirection: the new
    /// node answers to the same word the caller said, that word already has a different key pinned
    /// under it, and the mismatch is exactly what a pin is for.
    ///
    /// [`TofuStore::authorize`], not `check` + `pin_preapproved`: those two compose into the flow
    /// where an unknown peer is pinned on its first successful connection with nobody asked, which
    /// is the exact substitution a pin exists to catch. `authorize` puts the person in the loop for
    /// the unknown case and refuses a changed key outright without asking anyone.
    ///
    /// This runs **before** the dial, not after a successful transfer. The anchor is the person who
    /// compared the fingerprint, not the fact that a connection happened to succeed — so there is
    /// nothing about connecting first that makes the pin any more true, and asking first means a
    /// changed key costs nothing to discover.
    async fn authorize_peer(&self, label: &str, endpoint_id: &str) -> Result<()> {
        match self.tofu.authorize(self.confirmer.as_ref(), label, endpoint_id).await {
            Ok(()) => Ok(()),
            Err(e @ TofuError::Changed { .. }) => {
                // Never retriable. A retriable error invites the caller to try again, and the one
                // thing that must not happen here is a second, quieter attempt at the same slot.
                Err(refuse("peer_key_changed", e.to_string()))
            }
            Err(e @ TofuError::Refused { .. }) => Err(refuse("peer_not_confirmed", e.to_string())),
            // A ledger that cannot be read, a key that does not parse, a lock that could not be
            // taken. Kept apart from the two above rather than folded in with them: "a person said
            // no" and "the pin file is corrupt" call for completely different things from whoever
            // reads this, and one code for both would hide which happened.
            Err(e) => Err(refuse("peer_pin_unavailable", e.to_string())),
        }
    }

    /// `at` is moved forward as the work moves, and is read only if the deadline cuts this short.
    /// See [`phase`] for why the answer cannot be the same in all three places.
    async fn send_inner(
        &self,
        node: &str,
        path: &str,
        name: Option<String>,
        overwrite: Option<bool>,
        at: &AtomicU8,
    ) -> Result<SendReceipt> {
        let source = self.resolve_source(path).await?;
        let proposed = match name {
            Some(name) => name,
            // `resolve_source` has already established this is a regular file, which cannot have
            // an empty last component, so the fallback is unreachable in practice.
            None => source.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        };
        let (size, sha256) = hash_file(&source).await?;
        let transfer_id = transfer_id(&self.config.node_id, &proposed, size, &sha256);
        at.store(phase::REACHING, Ordering::SeqCst);

        let addr = self.look_up_peer(node).await?;
        self.authorize_peer(node, &addr.endpoint_id).await?;

        // Registered before the link opens, because the peer calls `pull` back while it is
        // answering `push_offer` — and because `open` takes the sender by value, so this is the
        // last moment it can be reached.
        let sender = LocalPeerTransfer::sender(self.config.root.clone());
        sender.offer_file(transfer_id.clone(), source, size, sha256.clone()).await;

        let session = self.link.open(&addr, sender).await?;
        // From here on the peer can be holding bytes, so a deadline that lands after this line has
        // something for the next call to come back to.
        at.store(phase::SENDING, Ordering::SeqCst);
        let offered = session
            .client
            .push_offer(TransferOffer {
                transfer_id,
                name: proposed,
                size,
                sha256,
                overwrite: overwrite.unwrap_or(false),
            })
            .await;
        let done = match offered {
            Ok(done) => done,
            // **Not a failure — the answer.** The peer is still writing the bytes an earlier call
            // for this same transfer handed it, so it turned this one away rather than letting a
            // second stream append into the same file. That is exactly what `pending` already
            // means here, and the caller was told to call again by the receipt that produced this
            // call; answering with an error would make that instruction a lie.
            Err(e) if e.code == ErrorCode::Other(TRANSFER_IN_FLIGHT.to_string()) => {
                return Ok(SendReceipt {
                    node: node.to_string(),
                    written: String::new(),
                    bytes: 0,
                    sha256: String::new(),
                    replaced: false,
                    undo: None,
                    direct: self.link.is_direct(&addr.endpoint_id).await,
                    pending: true,
                    next: Some(
                        "the peer is still receiving an earlier call for this same file. Call \
                         send_to again with the same arguments once it settles."
                            .to_string(),
                    ),
                })
            }
            Err(e) => return Err(e),
        };

        Ok(SendReceipt {
            node: node.to_string(),
            written: done.written,
            bytes: done.bytes,
            sha256: done.sha256,
            replaced: done.replaced,
            undo: done.undo,
            direct: self.link.is_direct(&addr.endpoint_id).await,
            pending: false,
            next: None,
        })
    }
}

#[async_trait::async_trait]
impl FileTransfer for LocalFileTransfer {
    async fn send_to(
        &self,
        node: String,
        path: String,
        name: Option<String>,
        overwrite: Option<bool>,
    ) -> Result<SendReceipt> {
        let deadline = self.config.wire_deadline;
        let at = AtomicU8::new(phase::MEASURING);
        let running = self.send_inner(&node, &path, name, overwrite, &at);
        let expired = match tokio::time::timeout(deadline, running).await {
            Ok(result) => return result,
            Err(_) => at.load(Ordering::SeqCst),
        };

        // Still reading the file when time ran out. Answering `pending` here would be a promise
        // this cannot keep: nothing has been offered, so there is no partial file on the peer, and
        // the retry the `next` line asks for begins the same read from zero and ends in the same
        // place. A file this node cannot hash inside one call is one it cannot send at all, and
        // saying so once beats an agent retrying until someone notices.
        if expired == phase::MEASURING {
            return Err(refuse(
                "source_too_slow_to_measure",
                format!(
                    "reading and hashing {path} did not finish within the {}s a call gets, so \
                     nothing has been offered to {node}. Calling again starts the same read over; \
                     this file cannot be sent from this node until it can be read faster.",
                    deadline.as_secs()
                ),
            ));
        }

        // Not an error. Once the link is open the bytes already received are on the peer's disk as
        // a `.part`, and the same arguments produce the same `transfer_id`, so calling again picks
        // up from there instead of starting over. Before that there is nothing to resume yet, but
        // the work that remains is a lookup and a dial — both worth retrying — so the shape of the
        // answer is the same and only the sentence differs.
        //
        // Every measured field stays at its zero value: nothing has been confirmed written, and
        // filling `bytes` in with the source file's size here would read as progress that has not
        // happened.
        let next = if expired == phase::SENDING {
            "still sending. Call send_to again with the same arguments to resume."
        } else {
            "the peer had not been reached yet. Call send_to again with the same arguments."
        };
        Ok(SendReceipt {
            node,
            written: String::new(),
            bytes: 0,
            sha256: String::new(),
            replaced: false,
            undo: None,
            direct: false,
            pending: true,
            next: Some(next.to_string()),
        })
    }

    async fn inbox_list(&self) -> Result<Vec<InboxEntry>> {
        let mut entries = Vec::new();
        let mut peers = match tokio::fs::read_dir(&self.config.inbox).await {
            Ok(dir) => dir,
            // An inbox nobody has written to yet does not exist, and that is an empty list rather
            // than a failure.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(entries),
            Err(e) => return Err(WireError::internal(e.to_string())),
        };
        // `while let Ok(Some(..))` would end the walk on a read error as if the directory had
        // simply ended, and hand back a short list as though it were the whole one. A caller asking
        // what arrived is worse served by a confident wrong answer than by a failure.
        loop {
            let peer = match peers.next_entry().await {
                Ok(Some(peer)) => peer,
                Ok(None) => break,
                Err(e) => {
                    return Err(WireError::internal(format!(
                        "{} could not be listed in full: {e}",
                        self.config.inbox.display()
                    )))
                }
            };
            // Received files always sit one level down, under the sending peer's washed name
            // (`Inbox::resolve`). Anything else at the top level is not something this node put
            // there.
            if !peer.path().is_dir() {
                continue;
            }
            let from = peer.file_name().to_string_lossy().into_owned();
            // A sender's directory that has been removed since the walk above listed it is simply
            // gone, and skipping it is right. Any other failure is not "there is nothing here".
            let mut files = match tokio::fs::read_dir(peer.path()).await {
                Ok(files) => files,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    return Err(WireError::internal(format!(
                        "{} could not be listed: {e}",
                        peer.path().display()
                    )))
                }
            };
            loop {
                let file = match files.next_entry().await {
                    Ok(Some(file)) => file,
                    Ok(None) => break,
                    Err(e) => {
                        return Err(WireError::internal(format!(
                            "{} could not be listed in full: {e}",
                            peer.path().display()
                        )))
                    }
                };
                let name = file.file_name().to_string_lossy().into_owned();
                // A transfer still in flight is not something that has arrived. This also hides a
                // genuinely received file whose own name ends in `.part`, which is the safer way
                // round: listing an unfinished transfer as a delivered file is the worse mistake.
                if name.ends_with(".part") {
                    continue;
                }
                let Ok(info) = file.metadata().await else { continue };
                if !info.is_file() {
                    continue;
                }
                entries.push(InboxEntry {
                    from: from.clone(),
                    name,
                    bytes: info.len(),
                    path: file.path().display().to_string(),
                    received_unix_ms: modified_ms(&info),
                });
            }
        }
        // Newest first, and ties broken by path so the order does not depend on what order the
        // filesystem handed the directory back in.
        entries.sort_by(|a, b| {
            b.received_unix_ms.cmp(&a.received_unix_ms).then_with(|| a.path.cmp(&b.path))
        });
        Ok(entries)
    }
}

/// The name of one transfer, derived so that **calling `send_to` again with the same arguments
/// resumes rather than restarts** — the receiving side finds the `.part` it already has under this
/// same id.
///
/// The fields are separated by a byte that cannot occur in any of them rather than concatenated
/// raw. Without a separator, `("ab", "c")` and `("a", "bc")` hash identically, which would let two
/// genuinely different transfers claim one another's partial file. `size` goes in as fixed-width
/// big-endian for the same reason.
///
/// The receiving side's path is deliberately not an ingredient: the destination is the receiver's
/// choice, so the sender cannot know it before asking, and a retry has to produce the same id
/// *before* anything is asked.
fn transfer_id(node_id: &str, name: &str, size: u64, sha256: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(node_id.as_bytes());
    hasher.update([0]);
    hasher.update(name.as_bytes());
    hasher.update([0]);
    hasher.update(size.to_be_bytes());
    hasher.update([0]);
    hasher.update(sha256.as_bytes());
    hex::encode(&hasher.finalize()[..TRANSFER_ID_BYTES])
}

/// Measures the file: its size and its sha256, read in one pass through a fixed buffer.
///
/// The size comes from the bytes actually read, not from `metadata().len()`. Those two can
/// disagree — the file can be appended to or truncated between the two calls — and the one that has
/// to be right is the one the peer will check its own hash against.
async fn hash_file(path: &Path) -> Result<(u64, String)> {
    use tokio::io::AsyncReadExt as _;
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| refuse("file_not_found", format!("{} could not be read: {e}", path.display())))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; HASH_CHUNK];
    let mut size = 0u64;
    loop {
        let read = file.read(&mut buf).await.map_err(|e| WireError::internal(e.to_string()))?;
        if read == 0 {
            break;
        }
        size += read as u64;
        hasher.update(&buf[..read]);
    }
    Ok((size, hex::encode(hasher.finalize())))
}

fn modified_ms(info: &std::fs::Metadata) -> u64 {
    info.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A refusal, spelled with the code from the design's error table. Not retriable unless the caller
/// says otherwise: `ErrorCode::Other`'s own default is already `false`, and every refusal in this
/// module that a caller *should* try again is marked so explicitly at its own site.
fn refuse(code: &str, message: String) -> WireError {
    WireError::new(ErrorCode::Other(code.to_string()), message).retriable(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_arguments_always_name_the_same_transfer() {
        // This is what makes a repeated `send_to` a resume instead of a restart.
        let a = transfer_id("node-1", "report.pdf", 1024, "abc");
        assert_eq!(a, transfer_id("node-1", "report.pdf", 1024, "abc"));
        assert_eq!(a.len(), TRANSFER_ID_BYTES * 2, "{a} should be 16 bytes of hex");
    }

    #[test]
    fn a_different_transfer_never_borrows_another_ones_name() {
        let base = transfer_id("node-1", "report.pdf", 1024, "abc");
        assert_ne!(base, transfer_id("node-2", "report.pdf", 1024, "abc"));
        assert_ne!(base, transfer_id("node-1", "notes.pdf", 1024, "abc"));
        assert_ne!(base, transfer_id("node-1", "report.pdf", 1025, "abc"));
        assert_ne!(base, transfer_id("node-1", "report.pdf", 1024, "abd"));
    }

    #[test]
    fn fields_cannot_be_slid_across_the_boundary_between_them() {
        // Concatenating without a separator makes these two collide, and the receiving side would
        // then hand one transfer the partial file belonging to the other.
        assert_ne!(
            transfer_id("node", "1report.pdf", 1024, "abc"),
            transfer_id("node1", "report.pdf", 1024, "abc")
        );
    }
}
