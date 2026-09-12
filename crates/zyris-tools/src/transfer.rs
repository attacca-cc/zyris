//! This machine's peer identity: the endpoint it is reachable on, the key that makes it the
//! same machine tomorrow, and the ledger of peers it has pinned.
//!
//! Nothing here is announced. [`Peering`] is the state the two transfer capabilities are built
//! on, and it is separate from `announce.rs` because it is the one piece of this product that
//! has to **outlive the process**. A capability is made fresh on every dial; a peer identity
//! that were made fresh on every dial would not be an identity.
//!
//! The failure this exists to prevent hides well. A node that generates a new key each launch
//! still *receives* files perfectly — the accept loop never consults a pin — so the only symptom
//! is that every peer which pinned this machine refuses it from then on, and only when sending.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, anyhow};
use zyris::p2p::fingerprint::{PeerConfirmer, fingerprint};
use zyris::p2p::tofu::TofuStore;
use zyris::p2p::transport::ALPN;
use zyris::p2p::{iroh, key};

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
}
