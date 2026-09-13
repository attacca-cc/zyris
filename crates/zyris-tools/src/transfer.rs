//! Moving a file between two of this account's machines, and the peer identity that makes it
//! possible: the endpoint this machine is reachable on, the key that makes it the same machine
//! tomorrow, and the ledger of peers it has pinned.
//!
//! [`Peering`] is that identity and [`Transfers`] is everything built on it. Both live here
//! rather than in `announce.rs` because they are the one piece of this product that has to
//! **outlive the process**. A capability is made fresh on every dial; a peer identity that were
//! made fresh on every dial would not be an identity.
//!
//! The failure this exists to prevent hides well. A node that generates a new key each launch
//! still *receives* files perfectly — the accept loop never consults a pin — so the only symptom
//! is that every peer which pinned this machine refuses it from then on, and only when sending.
//!
//! # One capability is announced here, not two
//!
//! `file_transfer` is the surface an agent calls — `send_to` and `inbox_list` — and it goes to
//! Attacca with the other four. **`peer_transfer` does not, and must not.** It is the wire
//! *between* two machines, and `zyris_caps::peer_transfer`'s own first line says it is "announced
//! only on the peer link". Its `push_offer` can only reach an inbox through a
//! `PeerTransferClient` taken from the peer's own connection, so one announced on the Attacca
//! link would refuse every call it ever received — a tool an agent cannot tell apart from a
//! broken one, which is the trap this repository avoids everywhere else. Both ends of that wire
//! are announced inside `zyris-transfer` instead: `serve_peers` builds one per accepted peer
//! connection, and `IrohPeerLink::open` builds the other for the length of one send.
//!
//! The consequence worth knowing is that an arriving file is **not** behind
//! [`Gate`](crate::Gate): the pause switch stops what an agent asks *of* this machine, and a peer
//! delivering a file asks this machine's agent surface for nothing. What records those instead is
//! `TransferConfig::audit`, one line per received file, written beside the audit log.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, anyhow};
use zyris::p2p::fingerprint::fingerprint;
use zyris::p2p::tofu::TofuStore;
use zyris::p2p::transport::ALPN;
use zyris::p2p::{iroh, key};
use zyris_attacca::{AttaccaApi, AttaccaApiClient};
use zyris_transfer::{
    DEFAULT_WIRE_DEADLINE, FileTransferConfig, IrohPeerLink, LocalFileTransfer, PeerDirectory,
    TransferConfig, serve_peers,
};

/// Who is asked about a peer this machine has never seen, and the answer a node with nobody to
/// ask gives. Re-exported so `main` can name the confirmer it installs without taking a
/// dependency on the protocol stack of its own.
pub use zyris::p2p::fingerprint::{DenyUnknown, PeerConfirmer};

/// Points this machine at a relay of its own instead of the public ones.
///
/// A relay cannot read a transfer — it carries an encrypted QUIC stream — but it does see which
/// two endpoints are talking and when. Unset, this rides n0's public relays, which is the right
/// default for something that has to work behind NAT on a first run with nothing configured.
pub const RELAY_URL_ENV: &str = "ZYRIS_RELAY_URL";

/// What the key is called inside the per-user data directory.
const KEY_FILE: &str = "iroh-secret.key";
/// What the pinned-peer ledger is called. The name upstream's own tests use, so a person
/// reading about `peers.json` in `zyris-p2p` finds the same file here.
const LEDGER_FILE: &str = "peers.json";
/// Where a peer's files land, one subdirectory per sending peer.
const INBOX_DIR: &str = "inbox";
/// Where a file an arriving one replaced is stashed. Beside the inbox, never inside it.
const UNDO_DIR: &str = "undo";
/// One line per received file. Not the audit log — that records what an agent asked of this
/// machine, and a delivery is not something an agent asked of it.
const TRANSFER_LOG: &str = "transfers.jsonl";

/// How long a connection gets to announce `attacca_api` before file transfer gives up on it and
/// waits for the next one.
///
/// Generous because it costs nothing: the wait ends early when the connection closes, and it runs
/// beside the close report rather than in front of it.
const API_WAIT: Duration = Duration::from_secs(30);

/// The endpoint, the key behind it, and the peers this machine has pinned.
///
/// One value rather than three loose ones because the three are only correct together: the
/// endpoint is derived from the key, and a pin in the ledger is a statement about a key — swap
/// either and every pin in the ledger silently stops meaning anything.
///
/// The confirmer travels with them because it is the answer to the one question the ledger
/// cannot answer on its own: what to do about a peer it has never seen. In `--headless` that is
/// `DenyUnknown`, and nothing else can be right there — nobody being around to ask is not
/// consent.
pub struct Peering {
    endpoint: iroh::Endpoint,
    tofu: TofuStore,
    confirmer: Arc<dyn PeerConfirmer>,
    /// Computed once, at `bind`. The endpoint's own id always parses — it came from iroh — so
    /// rendering it here keeps [`Self::fingerprint`] infallible rather than making every caller
    /// handle an error that cannot happen.
    fingerprint: String,
}

impl Peering {
    /// Loads (or, on a first run, creates) this machine's key from `dir`, binds an endpoint on
    /// it, and opens the pinned-peer ledger beside it.
    ///
    /// `dir` is the per-user data directory the audit log already lives in. It is passed in
    /// rather than computed here for the reason the audit log's path is: a `--server` run is a
    /// different node and must not read the production machine's identity as its own, and only
    /// the caller knows which run this is.
    ///
    /// Binding opens a real UDP socket and starts iroh's background work. It does not wait for a
    /// relay or for discovery, so a machine with no network still gets an endpoint — it simply
    /// has nowhere to reach.
    pub async fn bind(dir: &Path, confirmer: Arc<dyn PeerConfirmer>) -> anyhow::Result<Peering> {
        let key_path = key_path(dir);
        // Never `SecretKey::generate()`. `load_or_create` is the whole of the persistence this
        // needs, and it creates the file with `create_new(true).mode(0o600)` and fsyncs it, so
        // the key is never even momentarily readable by another local user and never half
        // written. It also *rejects* a key file that has since drifted looser than `0600`.
        let secret = key::load_or_create(&key_path)
            .await
            .with_context(|| format!("could not load this machine's peer key from {}", key_path.display()))?;

        let mut builder = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
            .secret_key(secret)
            // Without this the endpoint accepts nothing: `Endpoint::accept` only matches ALPNs
            // the endpoint was configured with, so an accept loop on an endpoint that never
            // named `zyris/1` waits forever on connections iroh has already turned away.
            .alpns(vec![ALPN.to_vec()]);
        // Left alone when nothing asked for a relay of its own, rather than re-stating the
        // default: the preset's choice already honours iroh's own staging override, and setting
        // it here unconditionally would quietly take that away.
        if let Some(relay) = custom_relay(std::env::var(RELAY_URL_ENV).ok().as_deref())? {
            builder = builder.relay_mode(relay);
        }
        let endpoint = builder
            .bind()
            .await
            .map_err(|error| anyhow!("could not bind this machine's peer endpoint: {error}"))?;

        let id = endpoint.id().to_string();
        let fingerprint = fingerprint(&id)
            .map_err(|error| anyhow!("iroh handed back an endpoint id that does not parse: {error}"))?;
        tracing::info!(endpoint_id = %id, %fingerprint, key = %key_path.display(), "peer identity ready");

        Ok(Peering {
            endpoint,
            tofu: TofuStore::new(ledger_path(dir)),
            confirmer,
            fingerprint,
        })
    }

    /// This machine's own fingerprint, as a person reads it aloud to the person at the other
    /// machine — `9F2A 41C7 0E83 BB15 6D04 A97E 22C1 5FB8`.
    ///
    /// The same string across restarts, because the key is. That is the property the whole
    /// module exists for, and it is what the other machine is comparing against.
    pub fn fingerprint(&self) -> String {
        self.fingerprint.clone()
    }

    /// The bound endpoint, for the dial side and the accept loop.
    pub fn endpoint(&self) -> &iroh::Endpoint {
        &self.endpoint
    }

    /// The pinned-peer ledger. Cheap to clone, and clones share the write lock, so hand one of
    /// these out rather than rebuilding a store from the same path.
    pub fn tofu(&self) -> &TofuStore {
        &self.tofu
    }

    /// Who decides about a peer the ledger has never seen.
    pub fn confirmer(&self) -> Arc<dyn PeerConfirmer> {
        self.confirmer.clone()
    }
}

/// File transfer, wired onto this machine's peer identity.
///
/// One value owns all of it, because the pieces are only correct together: the `file_transfer`
/// capability an agent calls, the accept loop that takes deliveries, and the rendezvous client
/// both of them ask where a peer is — which is **one** client, replaced on every connect.
///
/// Cheap to clone, and every clone is the same wiring rather than a copy of it: the endpoint, the
/// ledger, the rendezvous slot and the in-flight set inside [`TransferConfig`] are all shared
/// handles. `main` keeps one and the connect hook keeps another.
#[derive(Clone)]
pub struct Transfers {
    peering: Arc<Peering>,
    /// What is announced, and what holds the rendezvous client. The clone [`Tools`] announces
    /// shares this one's slot, so [`Self::on_connect`] calling `set_api` here reaches the
    /// capability the node is actually serving.
    ///
    /// [`Tools`]: crate::Tools
    file_transfer: LocalFileTransfer,
    /// Where an arriving peer's files land, and what is done about one that would replace a file
    /// already there.
    ///
    /// **Cloned when the accept loop starts, never rebuilt.** `in_flight` is shared between
    /// clones of this config and is what stops two deliveries of one transfer id from racing; a
    /// second config built from the same paths would share nothing with this one and that guard
    /// would silently stop being a guard.
    receiving: TransferConfig,
    /// Kept for the window and for the line logged at startup: a person has to be able to find
    /// what arrived.
    inbox: PathBuf,
    /// This machine's iroh endpoint id, which is three things at once and is the same string in
    /// all three: what `peer_publish` publishes, the salt `FileTransferConfig` mixes into a
    /// `transfer_id`, and the label `serve_peers` announces to a peer so it knows who answered.
    ///
    /// The salt and the label are documented upstream as identifiers and explicitly **not** trust
    /// anchors, so what they need is to be unique to this machine and stable across restarts.
    /// The endpoint id is exactly that and — unlike the node id Attacca issues — it exists before
    /// this process has a connection, which is when the capability is built.
    endpoint_id: String,
    /// Whether the accept loop has been started. See [`Self::start_accepting`].
    accepting: Arc<AtomicBool>,
}

impl Transfers {
    /// Binds this machine's peer identity and builds everything that stands on it.
    ///
    /// `dir` is the per-user data directory, the one the audit log lives in: the key, the pinned-
    /// peer ledger, the inbox, the undo stash and the transfer log are all this node's own state
    /// and a `--server` run must not inherit the production machine's.
    ///
    /// `root` is where a caller's relative paths start — the same root `file_io` and `terminal`
    /// use — and for `send_to` it is a real boundary rather than a default: `zyris-transfer`
    /// canonicalizes the source path and refuses anything that lands outside it.
    ///
    /// Fails when the key will not load or the socket will not bind. That is not a fault of this
    /// machine and the caller is expected to carry on without transfer; see `Tools::screen_pair`
    /// for the same shape.
    pub async fn bind(
        dir: &Path,
        root: PathBuf,
        confirmer: Arc<dyn PeerConfirmer>,
    ) -> anyhow::Result<Transfers> {
        let peering = Peering::bind(dir, confirmer).await?;
        let endpoint_id = peering.endpoint().id().to_string();
        let inbox = dir.join(INBOX_DIR);

        // Every field is named on both configs. Both default their roots to `"."`, which is `/`
        // under a systemd unit and whatever a desktop launcher happened to set otherwise — the
        // trap this repository avoids everywhere else, and the one place it would land a
        // stranger's file rather than merely read the wrong one.
        let receiving = TransferConfig {
            inbox: inbox.clone(),
            // Beside the inbox and never inside it: `inbox_list` reads the inbox's top level as
            // one directory per sending peer, so a stash kept in there would be listed as a
            // delivery from a peer called `undo`.
            undo: dir.join(UNDO_DIR),
            // The only record that a file arrived. Nothing behind [`Guarded`](crate::Guarded)
            // sees an incoming transfer — it does not come through the Attacca connection at all
            // — and this line is what answers "what landed on this machine, from whom, and did it
            // replace anything" afterwards. It carries the file's name, size and hash and not one
            // byte of the file, which is the same rule the audit log follows.
            audit: Some(dir.join(TRANSFER_LOG)),
            // The two limits are upstream's: 8 GiB for one file, 32 GiB for the whole inbox.
            ..TransferConfig::default()
        };

        let sending = FileTransferConfig {
            root,
            inbox: inbox.clone(),
            node_id: endpoint_id.clone(),
            wire_deadline: DEFAULT_WIRE_DEADLINE,
        };
        let file_transfer = LocalFileTransfer::pending(
            sending,
            peering.tofu().clone(),
            peering.confirmer(),
            Arc::new(IrohPeerLink::new(peering.endpoint().clone())),
        );

        tracing::info!(
            inbox = %inbox.display(),
            transfer_log = %dir.join(TRANSFER_LOG).display(),
            "file transfer is ready; what a peer sends lands in the inbox and is written down here"
        );
        Ok(Transfers {
            peering: Arc::new(peering),
            file_transfer,
            receiving,
            inbox,
            endpoint_id,
            accepting: Arc::new(AtomicBool::new(false)),
        })
    }

    /// This machine's identity as a peer: its fingerprint, and the peers it has pinned.
    pub fn peering(&self) -> &Peering {
        &self.peering
    }

    /// Where received files land.
    pub fn inbox(&self) -> &Path {
        &self.inbox
    }

    /// The capability [`Tools`](crate::Tools) announces. The clone shares this one's rendezvous
    /// slot, which is what makes announcing it and keeping it current two views of one thing.
    pub(crate) fn capability(&self) -> LocalFileTransfer {
        self.file_transfer.clone()
    }

    /// The per-connection half of file transfer: the client, the address, and — once — the
    /// accept loop.
    ///
    /// **Runs on every established connection, including every redial.** `Rendezvous`'s own
    /// documentation records what a write-once version costs: a socket reset left the client
    /// bound to the dead connection and every send afterwards failed with `connection lost` on a
    /// node that otherwise looked healthy. So the client is *replaced* here, never kept.
    ///
    /// Publishing repeats for a different reason: addresses move. A laptop that changed network
    /// since the last connect is reachable at somewhere it has not said yet, and nothing else in
    /// this program would ever say it.
    ///
    /// Nothing here returns an error. A connection that never announces `attacca_api`, or a
    /// publish the server refuses, leaves this machine connected and able to serve every other
    /// capability — and the next connection tries again.
    pub async fn on_connect(&self, connection: zyris::Connection) {
        let api = match connection.wait_capability::<AttaccaApiClient>(API_WAIT).await {
            Ok(api) => api,
            Err(error) => {
                tracing::warn!(
                    %error,
                    "this connection never announced attacca_api, so there is nothing to ask where a peer is; file transfer waits for the next one"
                );
                return;
            }
        };

        // Installed before anything that waits. Publishing is a network round trip, and
        // `send_to` refuses outright while this slot is empty — so the slot is filled first and
        // the client is read back out of it, rather than the other way round.
        self.file_transfer.set_api(api);
        let Some(api) = self.file_transfer.rendezvous().get() else {
            // Unreachable: the line above just filled it. Written as a branch because the slot
            // is a lock and the honest reading of a poisoned one is "no client", not a panic.
            return;
        };

        self.publish(api.as_ref()).await;
        self.start_accepting();
    }

    /// Says where this machine can be reached, so another node of the same account can dial it.
    async fn publish(&self, api: &AttaccaApiClient) {
        let addrs = candidate_addrs(&self.peering.endpoint().addr());
        match api.peer_publish(self.endpoint_id.clone(), addrs.clone()).await {
            Ok(()) => tracing::info!(
                endpoint_id = %self.endpoint_id,
                addrs = %addrs.join(", "),
                "published where this machine can be reached"
            ),
            // Not fatal and not retried here. Another node can still find this one through
            // iroh's own discovery — measured upstream as the path that worked when published
            // addresses did not — and the next connection publishes again.
            Err(error) => tracing::warn!(
                %error,
                "could not publish where this machine can be reached; another node will have to find it by discovery"
            ),
        }
    }

    /// Starts the accept loop, **once for the life of the process**, and says whether this call
    /// was the one that did it.
    ///
    /// [`Self::on_connect`] runs on every connection, so "once" cannot be the caller's
    /// discipline; `swap` is what makes it this function's. A second loop on one endpoint is the
    /// bug that works in testing and fails under load: both loops call `accept_next` on the same
    /// endpoint, each connection goes to whichever won the race, and the two disagree about what
    /// is in flight.
    ///
    /// The loop is not restarted after a disconnect and must not be: it holds a [`Rendezvous`],
    /// which is the same slot [`Self::on_connect`] refreshes, so it keeps working across
    /// reconnects with no attention. `serve_peers` never returns while the endpoint lives.
    ///
    /// [`Rendezvous`]: zyris_transfer::Rendezvous
    fn start_accepting(&self) -> bool {
        if self.accepting.swap(true, Ordering::SeqCst) {
            return false;
        }
        tracing::info!("accepting peer connections");
        tokio::spawn(serve_peers(
            self.peering.endpoint().clone(),
            // The rendezvous rather than the client: the loop outlives the connection that
            // client came from, and a loop holding a dead client judges every arriving peer
            // against whatever the account looked like at startup.
            Arc::new(self.file_transfer.rendezvous()) as Arc<dyn PeerDirectory>,
            self.receiving.clone(),
            self.peering.tofu().clone(),
            self.endpoint_id.clone(),
        ));
        true
    }
}

/// What [`AttaccaApi::peer_publish`] is given: the socket addresses this endpoint believes it can
/// be reached at, as strings.
///
/// The relay is deliberately not among them — `peer_publish` takes no relay field, because the
/// relay belongs to the deployment rather than to a node, and it comes back down `ZPeerAddr` from
/// the server.
///
/// **Not waited for.** `Endpoint::online()` resolves when a relay answers and never resolves when
/// none does, so awaiting it here would hang the connect hook on exactly the machines that have
/// the least to publish. Direct addresses are known the moment the socket is bound, which is
/// before the first connection; the relay path is iroh's own discovery, which a dialer falls back
/// to anyway.
fn candidate_addrs(addr: &iroh::EndpointAddr) -> Vec<String> {
    addr.ip_addrs().map(|socket| socket.to_string()).collect()
}

fn key_path(dir: &Path) -> PathBuf {
    dir.join(KEY_FILE)
}

fn ledger_path(dir: &Path) -> PathBuf {
    dir.join(LEDGER_FILE)
}

/// Turns whatever [`RELAY_URL_ENV`] was set to into a relay mode, or `None` when nothing asked.
///
/// Unset and blank both mean "nothing asked" — a variable exported as the empty string is how a
/// shell or a unit file spells "not set", and treating that as a relay URL would fail the bind
/// on every machine that does it. Anything else that is not a URL is an **error**, not a
/// fallback to the public relays: a person who set this to point at their own relay and got the
/// public ones anyway would have no way to tell, and the whole reason to set it is not to use
/// them.
fn custom_relay(value: Option<&str>) -> anyhow::Result<Option<iroh::RelayMode>> {
    let Some(url) = value.map(str::trim).filter(|url| !url.is_empty()) else {
        return Ok(None);
    };
    let url = url
        .parse::<iroh::RelayUrl>()
        .with_context(|| format!("{RELAY_URL_ENV} is set to {url:?}, which is not a relay URL"))?;
    Ok(Some(iroh::RelayMode::custom([url])))
}

#[cfg(test)]
mod tests {
    use super::*;
    use zyris::p2p::fingerprint::DenyUnknown;
    use zyris::p2p::tofu::TofuError;

    /// A real, parseable endpoint id that is nobody this machine has met. Built from a fresh
    /// key rather than written out as a literal, because `authorize` canonicalizes and
    /// fingerprints whatever it is given before it ever consults a confirmer — a string that
    /// merely looks like an id is rejected for *that* reason and never reaches the decision the
    /// test is about.
    fn a_stranger() -> String {
        iroh::SecretKey::generate().public().to_string()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_key_survives_a_restart() {
        // The whole reason this is persisted. Without it this is a different node after every
        // launch, and a pin another peer made expires with the process — which is not a pin.
        // The failure hides: the accept loop never consults a pin, so receiving keeps working
        // and only sending breaks.
        let dir = tempfile::tempdir().unwrap();

        let first = Peering::bind(dir.path(), Arc::new(DenyUnknown)).await.unwrap();
        let before = first.fingerprint();
        drop(first);
        let second = Peering::bind(dir.path(), Arc::new(DenyUnknown)).await.unwrap();

        assert_eq!(before, second.fingerprint(), "this machine became a different peer");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn two_directories_are_two_peers() {
        // The converse, so the test above cannot pass by returning a constant.
        let one = tempfile::tempdir().unwrap();
        let two = tempfile::tempdir().unwrap();

        let a = Peering::bind(one.path(), Arc::new(DenyUnknown)).await.unwrap();
        let b = Peering::bind(two.path(), Arc::new(DenyUnknown)).await.unwrap();

        assert_ne!(a.fingerprint(), b.fingerprint());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_fingerprint_comes_from_the_key_file_and_not_from_anywhere_else() {
        // `the_key_survives_a_restart` proves the answer is stable and `two_directories_are_two_peers`
        // proves it is not a constant, but both would still hold if the identity were derived
        // from, say, the directory's name. Deleting the one file and binding the same directory
        // again is what pins it to the key: the same path, and a different peer.
        let dir = tempfile::tempdir().unwrap();
        let before = Peering::bind(dir.path(), Arc::new(DenyUnknown)).await.unwrap().fingerprint();

        std::fs::remove_file(dir.path().join(KEY_FILE)).unwrap();
        let after = Peering::bind(dir.path(), Arc::new(DenyUnknown)).await.unwrap().fingerprint();

        assert_ne!(before, after, "the identity did not come from the key file");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn the_key_file_is_owner_only() {
        // `key::load_or_create` is what enforces this, and it is checked here rather than taken
        // on trust: it is a private key, and the day an upstream refactor loses the mode there
        // is nothing else in this repository that would notice.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();

        Peering::bind(dir.path(), Arc::new(DenyUnknown)).await.unwrap();

        let mode = std::fs::metadata(dir.path().join(KEY_FILE)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "this machine's private key is readable by someone else");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unpinned_peer_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let peering = Peering::bind(dir.path(), Arc::new(DenyUnknown)).await.unwrap();

        let refusal = peering
            .tofu()
            .authorize(peering.confirmer().as_ref(), "nobody", &a_stranger())
            .await;

        // Named, not just `is_err`. Every other `TofuError` would also be an error here, and
        // one of them — `InvalidEndpointId` — is what a test that hands `authorize` a string
        // that is not a key gets instead, while proving nothing about whether an unknown peer
        // is turned away.
        assert!(
            matches!(refusal, Err(TofuError::Refused { .. })),
            "an unknown peer must be refused, got {refusal:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_refused_peer_is_not_pinned() {
        // The refusal has to leave the ledger as it found it. A refusal that pinned anyway
        // would read as a working `DenyUnknown` — the call still failed — right up until the
        // same peer came back and sailed through as already known.
        let dir = tempfile::tempdir().unwrap();
        let peering = Peering::bind(dir.path(), Arc::new(DenyUnknown)).await.unwrap();

        let _ = peering.tofu().authorize(peering.confirmer().as_ref(), "nobody", &a_stranger()).await;

        assert!(peering.tofu().pins().await.unwrap().is_empty());
    }

    #[test]
    fn no_relay_url_leaves_the_public_relays_alone() {
        assert!(custom_relay(None).unwrap().is_none());
        // An exported-but-empty variable is how a shell and a systemd unit both spell "unset".
        assert!(custom_relay(Some("")).unwrap().is_none());
        assert!(custom_relay(Some("   ")).unwrap().is_none());
    }

    #[test]
    fn a_relay_url_becomes_the_only_relay() {
        let mode = custom_relay(Some("https://relay.example.test")).unwrap().unwrap();

        let map = mode.relay_map();
        assert_eq!(map.len(), 1, "a self-hosted relay must replace the public ones, not join them");
        assert!(map.contains(&"https://relay.example.test".parse().unwrap()));
    }

    #[test]
    fn a_relay_url_that_is_not_a_url_is_refused_rather_than_ignored() {
        // Falling back to the public relays here would send this machine's traffic through
        // exactly the servers the person was trying to avoid, and say nothing about it.
        assert!(custom_relay(Some("this is not a url")).is_err());
    }

    async fn transfers(dir: &Path) -> Transfers {
        Transfers::bind(dir, dir.join("root"), Arc::new(DenyUnknown)).await.unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_accept_loop_starts_once_however_often_a_connection_comes_up() {
        // The spec's rule, and the reason it is a rule: `on_connect` runs on every connection —
        // first dial, every redial, a laptop waking up — and two accept loops on one endpoint
        // race for each arriving connection. That works in testing, where connections arrive one
        // at a time, and fails under load.
        let dir = tempfile::tempdir().unwrap();
        let transfers = transfers(dir.path()).await;

        assert!(transfers.start_accepting(), "the first connection has to start it");
        for _ in 0..5 {
            assert!(!transfers.start_accepting(), "a redial must not start a second one");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_clone_shares_the_guard_rather_than_carrying_its_own() {
        // `main` keeps one of these and the connect hook keeps a clone, so a guard that cloned
        // as `false` would be no guard at all — it would simply move the second accept loop into
        // the second handle.
        let dir = tempfile::tempdir().unwrap();
        let transfers = transfers(dir.path()).await;

        assert!(transfers.clone().start_accepting());

        assert!(!transfers.start_accepting(), "the clone's start did not count as a start");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_undo_stash_is_not_inside_the_inbox() {
        // `inbox_list` reads the inbox's top level as one directory per sending peer, so a stash
        // kept in there would be reported as a delivery from a peer called `undo` — and the
        // files under it as files that peer sent.
        let dir = tempfile::tempdir().unwrap();
        let transfers = transfers(dir.path()).await;

        assert!(!transfers.receiving.undo.starts_with(transfers.inbox()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn every_transfer_path_is_absolute_rather_than_relative_to_wherever_this_started() {
        // Both upstream configs default their roots to `"."`. Under a systemd unit that is `/`,
        // and for the inbox it is the one path in this program that a stranger's file is written
        // to rather than merely read from.
        let dir = tempfile::tempdir().unwrap();
        let transfers = transfers(dir.path()).await;

        assert!(transfers.inbox().is_absolute(), "{}", transfers.inbox().display());
        assert!(transfers.receiving.undo.is_absolute());
        let audit = transfers.receiving.audit.clone().expect("a received file must be written down");
        assert!(audit.is_absolute(), "{}", audit.display());
    }

    #[test]
    fn the_published_addresses_are_socket_addresses_and_never_the_relay() {
        // `peer_publish` takes no relay field: the relay belongs to the deployment and comes back
        // down `ZPeerAddr`. The dialing side parses each of these as a `SocketAddr` and drops
        // whatever does not — so a relay URL published here would not be a hint, it would be a
        // hint silently thrown away.
        let addr = iroh::EndpointAddr::from_parts(
            iroh::SecretKey::generate().public(),
            [
                iroh::TransportAddr::Ip("198.51.100.9:4433".parse().unwrap()),
                iroh::TransportAddr::Relay("https://relay.example.test".parse().unwrap()),
            ],
        );

        let published = candidate_addrs(&addr);

        assert_eq!(published, vec!["198.51.100.9:4433".to_string()]);
        for candidate in &published {
            assert!(
                candidate.parse::<std::net::SocketAddr>().is_ok(),
                "a dialer drops anything that is not a socket address: {candidate}"
            );
        }
    }
}
