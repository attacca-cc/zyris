//! Pin a peer's `EndpointId` to **the first one we saw** (Trust On First Use).
//!
//! attacca issues node credentials and runs the rendezvous, so a "fake B" introduced to A
//! cannot be ruled out by cryptography alone. Pinning turns that into an attack that works
//! **once, never twice, and leaves a mark** — but only if the ledger is keyed on something
//! attacca itself cannot re-mint.
//!
//! **`TofuStore::authorize` is the entry point.** `check` and `pin_preapproved` are the
//! primitives it is built from — a read-only lookup and a write that never asks anyone — kept
//! `pub` because a handful of callers legitimately need one without the other (see
//! `pin_preapproved`'s own doc for exactly which). Reaching for them together instead of
//! `authorize` silently rebuilds the pre-2.7 flow this module used to have, where an unknown
//! peer got pinned on its first successful connection with no human ever asked. Nothing in this
//! module can stop that at compile time — `check` then `pin_preapproved` typechecks fine — so
//! the defense here is naming: `pin_preapproved` says out loud what the caller is asserting.
//!
//! **The rule for the key `check`/`pin_preapproved`/`authorize` all take (`peer_slug`): it
//! must be a name the *user* chose, and one that no server can re-issue.** That is the actual
//! property — not a list of specific fields to avoid, because any string attacca hands us,
//! however it is spelled, fails it the same way. Two are worth naming anyway, since they are
//! the ones most likely to end up
//! here by a well-meaning mistake: attacca's own `node_id` (see `ConnectionInfo` in
//! `zyris/src/connection.rs`), and `TokenResponse.node_name` (`zyris/src/enroll/protocol.rs`),
//! which gets persisted as `StoredCredential.node_name` (`zyris/src/enroll/store.rs`) and looks
//! enough like a user-facing label to be mistaken for one. It is not: attacca supplies it,
//! unverified, exactly like `node_id`, and reports it fresh for every node — nothing stops a
//! fake B from arriving with a *new* `node_id` and the *same* `node_name` as the real one, or a
//! `node_name` chosen to look however it likes. Whichever server-issued string ends up in this
//! slot, the fake arrives as an unknown peer, passes `check` (nothing is pinned under an
//! identifier nobody has seen before), and gets pinned — every time, with no mark left
//! anywhere. Keying on a name the user picked, that attacca has no channel to silently
//! reassign, is what actually makes the substitution visible: attacca can mint or report as
//! many identifiers as it wants, but it cannot make a second one answer to the slug the user
//! already pinned without `check` catching the mismatch.
//!
//! **There is no automatic way out.** A human has to edit the file. Accepting a changed key
//! quietly would make the pin worth nothing.
//!
//! Two writers can be two `tokio::task`s in this process or two entirely separate
//! processes sharing the same path — both are possible once a node runs more than one
//! connection. An in-process `tokio::sync::Mutex` only ever sees the first kind, so
//! `pin_preapproved` also takes a `<ledger>.lock` file: the one thing every writer, in any
//! process, actually shares.
//!
//! **This defends against a remote peer substituting itself, not against local tampering.**
//! `{"peers":{}}` is a perfectly valid, empty ledger; writing it over the real one erases
//! every pin and no shape check can tell the difference from a node that has genuinely never
//! connected to anyone. Anyone who can write this file can just as easily delete it, or
//! replace the node's own key outright (see `key.rs`) — local write access to our config
//! directory is game over by construction, for this store and for the node's identity both.
//! That is not a gap in this design; it is outside what a TOFU pin can ever claim to cover.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::fingerprint::{fingerprint, PeerConfirmer};

#[derive(Debug, thiserror::Error)]
pub enum TofuError {
    #[error("this node's key changed. pinned: {pinned}, offered: {offered}")]
    Changed { pinned: String, offered: String },
    #[error("the pin file at {path} could not be read ({reason}); refusing to continue")]
    Malformed { path: String, reason: String },
    #[error("{0}")]
    Io(String),
    /// The person asked to confirm `label`'s fingerprint said no. Nothing was pinned —
    /// `TofuStore::authorize` returns this before ever calling `pin_preapproved`, so an
    /// attacker who gets refused once does not get a second, quieter attempt under the same
    /// slug: the peer stays unknown, exactly as if the connection had never happened.
    #[error("peer confirmation for {label} was refused (fingerprint {fingerprint}); not pinned")]
    Refused { label: String, fingerprint: String },
    /// `authorize` could not even compute a fingerprint to show a person, because the offered
    /// `endpoint_id` does not parse as a key at all. Returned before `confirmer` is ever
    /// called — a person must never be asked to eyeball a fingerprint derived from something
    /// that was not a real key in the first place, since there is nothing genuine for it to
    /// match against.
    #[error("{0}")]
    InvalidEndpointId(#[from] crate::fingerprint::InvalidEndpointId),
}

/// The on-disk ledger. `deny_unknown_fields` and requiring `peers` (no `#[serde(default)]`)
/// both matter: without either, `{}`, `[]`, and any object whose `peers` key was renamed or
/// dropped all deserialize to an *empty* ledger instead of failing. Serde's derive accepts a
/// struct in map form (an object) or seq form (an array of positional fields), and a
/// `#[serde(default)]` field is optional in both, so a defaulted `peers` field alone made all
/// three shapes above read as "no pins ever taken" — exactly the fail-open the module docs say
/// must not happen.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    peers: HashMap<String, Entry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    endpoint_id: String,
    /// Never read by this code. It is here for the human who opens the file after a
    /// `Changed` error and needs to know when the pin was taken.
    first_seen_ms: u64,
}

/// Default for how long `pin_preapproved` waits for another writer holding `<ledger>.lock`
/// before giving up. Overridable per store via `TofuStore::with_lock_timeout` (tests only).
const LOCK_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long to sleep between attempts to take the lock file.
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(20);
/// How old a lock file's last modification has to be before it is treated as abandoned. The
/// critical section it guards is milliseconds of I/O, so anything within shouting distance of
/// this means the writer that created it is gone — killed, crashed, OOM-reaped — not slow.
/// Deliberately far above `LOCK_ACQUIRE_TIMEOUT`: a lock still held after 5s of retrying is
/// most likely a live writer taking a while, not a dead one, so `pin_preapproved` gives up
/// loudly there instead of guessing. A lock only crosses this threshold across separate
/// `pin_preapproved` calls, once nothing has retried it in a while.
const LOCK_STALE_THRESHOLD: Duration = Duration::from_secs(60);
/// How many names to try if a temp-file name collides with one already on disk.
const TEMP_CREATE_ATTEMPTS: u32 = 8;

/// Clone is cheap and clones share the in-process write lock, so hand out clones instead of
/// rebuilding a store from its path for same-process callers — it keeps them off the lock-file
/// retry loop below entirely. That in-process lock cannot help across processes, though: two
/// separate `TofuStore`s over the same path, cloned or not, in this process or another, still
/// have to serialize through the `<ledger>.lock` file that `pin_preapproved` takes.
#[derive(Clone)]
pub struct TofuStore {
    path: Arc<PathBuf>,
    write_lock: Arc<tokio::sync::Mutex<()>>,
    lock_acquire_timeout: Duration,
}

impl TofuStore {
    pub fn new(path: impl Into<PathBuf>) -> TofuStore {
        TofuStore {
            path: Arc::new(path.into()),
            write_lock: Arc::new(tokio::sync::Mutex::new(())),
            lock_acquire_timeout: LOCK_ACQUIRE_TIMEOUT,
        }
    }

    /// Overrides how long `pin_preapproved` waits for `<ledger>.lock` before giving up. Exists
    /// for tests that need to exercise the "never gets the lock" path without paying its
    /// real-world cost (the default is 5s) on every run; production callers should leave this
    /// alone.
    #[doc(hidden)]
    pub fn with_lock_timeout(mut self, timeout: Duration) -> TofuStore {
        // Clamped, not passed through as-is: `tokio::time::Instant::now() + timeout` panics
        // for something like `Duration::MAX`. This is `#[doc(hidden)]`, but it is still `pub`
        // — a bad argument from a caller (test or otherwise) should get a store with a very
        // long timeout, not a panic. An hour is far past anything a real wait should need.
        self.lock_acquire_timeout = timeout.min(Duration::from_secs(3600));
        self
    }

    /// Checks the offered key against what is pinned. An unknown peer passes — this method
    /// alone never pins anything, for an unknown peer or otherwise.
    ///
    /// **Most callers want [`Self::authorize`], not this.** `check` is the read-only half of
    /// what `authorize` composes; calling `check` and then [`Self::pin_preapproved`] yourself
    /// reimplements `authorize` minus the one thing that makes it safe — a human confirming an
    /// unknown peer's fingerprint before it gets pinned. `check` on its own is for a caller that
    /// genuinely only wants to *read* the ledger (e.g. before deciding whether to dial at all)
    /// without triggering any confirmation or write.
    ///
    /// `peer_slug` **must be a name the user chose, and one that no server can re-issue** —
    /// that is the property, not a specific field to avoid. It must never be attacca's
    /// `node_id`, `TokenResponse.node_name`, or any other string attacca supplies — see the
    /// module docs for why keying on any of those defeats the whole point of pinning.
    ///
    /// A ledger we cannot read is an error, not an empty ledger. See the module docs.
    ///
    /// `endpoint_id` is parsed and compared in its canonical form — see
    /// `canonical_endpoint_id` — so a peer offering the same key spelled differently than
    /// whatever is on disk (lowercase hex vs. base32) reads as the same key, not a change.
    pub async fn check(&self, peer_slug: &str, endpoint_id: &str) -> Result<(), TofuError> {
        let endpoint_id = canonical_endpoint_id(endpoint_id)?;
        match self.lookup(peer_slug).await? {
            None => Ok(()),
            Some(entry) if entry.endpoint_id == endpoint_id => Ok(()),
            Some(entry) => Err(TofuError::Changed { pinned: entry.endpoint_id, offered: endpoint_id }),
        }
    }

    /// Checks the offered key against a confirmer, pinning it if the confirmer accepts.
    ///
    /// This is `check` plus a human in the loop for the one case `check` cannot resolve on its
    /// own — a peer this ledger has never seen:
    ///
    /// 1. **Look up `peer_slug` without taking any lock.** A known peer is settled right here:
    ///    the same key returns `Ok(())`, a different key returns `Err(Changed)` — and neither
    ///    case calls `confirmer` at all. A key that is already pinned differently is not a
    ///    judgment call for a person to make; it is refused outright, the same way `check`
    ///    already refuses it. Asking "are you sure?" about a substitution would just give an
    ///    attacker a second, quieter attempt at the same slug.
    /// 2. **Only an unknown peer reaches `confirmer.confirm`.** This is deliberately the one
    ///    step in `authorize` that can take longer than a filesystem call — a person has to
    ///    read a fingerprint and decide. Nothing here holds `pin_preapproved`'s file lock (or
    ///    even the in-process one `pin_preapproved` also takes) while that happens: the lock is
    ///    designed to be broken as stale after 60 seconds (see the module docs and
    ///    `LOCK_STALE_THRESHOLD`) because the only legitimate way to hold it that long is a dead
    ///    writer, and a person thinking it over is not dead, just slower than a lock built for
    ///    crash recovery can tell apart from one.
    /// 3. **A refusal returns `Err(Refused)` and pins nothing.** An acceptance calls
    ///    [`Self::pin_preapproved`], which takes the lock and *re-checks* before writing. That
    ///    re-check is what makes step 1's lock-free lookup safe despite the gap a slow human
    ///    opens up: if another connection pinned a different key for this same `peer_slug`
    ///    while this one was waiting on an answer, `pin_preapproved` reports `Changed` instead
    ///    of overwriting it — the exact protection its own doc comment describes, inherited
    ///    here for free rather than re-implemented.
    ///
    /// `peer_slug` carries the same requirement as `check` and `pin_preapproved`: a name the
    /// user chose, never a string attacca minted. See the module docs.
    ///
    /// `endpoint_id` is canonicalized before anything else happens — including before
    /// `confirmer` is ever consulted. An `endpoint_id` that does not even parse as a key
    /// returns `Err(TofuError::InvalidEndpointId)` immediately: a person must never be shown a
    /// fingerprint (or asked to approve a pin) derived from something that was not a real key.
    pub async fn authorize(
        &self,
        confirmer: &dyn PeerConfirmer,
        peer_slug: &str,
        endpoint_id: &str,
    ) -> Result<(), TofuError> {
        let endpoint_id = canonical_endpoint_id(endpoint_id)?;

        match self.lookup(peer_slug).await? {
            Some(entry) if entry.endpoint_id == endpoint_id => return Ok(()),
            Some(entry) => {
                return Err(TofuError::Changed { pinned: entry.endpoint_id, offered: endpoint_id });
            }
            None => {}
        }

        let fp = fingerprint(&endpoint_id)?;
        if !confirmer.confirm(peer_slug, &fp).await {
            return Err(TofuError::Refused { label: peer_slug.to_string(), fingerprint: fp });
        }

        self.pin_preapproved(peer_slug, &endpoint_id).await
    }

    /// Reads the ledger and returns this peer's pinned entry, if any — no lock, since a read
    /// races nothing that matters (only concurrent *writes* need `pin_preapproved`'s lock). The
    /// one piece of lookup logic `check` and `authorize` both need: whether `peer_slug` is
    /// pinned at all, and to what. Kept as its own method rather than inlined in both, since
    /// `authorize` needs to tell "known and matching" apart from "never seen" — something
    /// `check`'s `Result<(), _>` alone cannot express, but the `Option<Entry>` this returns can.
    async fn lookup(&self, peer_slug: &str) -> Result<Option<Entry>, TofuError> {
        let ledger = self.read().await?;
        Ok(ledger.peers.get(peer_slug).cloned())
    }

    /// Pins a key **without asking anyone.** Named `pin_preapproved`, not `pin`, on purpose:
    /// the plain name reads as a safe, obvious thing to reach for, and it is not — calling this
    /// directly for a peer nobody has confirmed is exactly the silent-TOFU behavior this module
    /// no longer has by default (see the module docs). The name is the guard rail, since
    /// nothing in the type system stops `check` then `pin_preapproved` from typechecking as a
    /// substitute for `authorize`. Reach for this directly only when "preapproved" is
    /// genuinely true by construction — e.g. a provisioning step recording a fingerprint an
    /// operator already verified out of band, or a headless node's own `PeerConfirmer`
    /// consulting an allowlist it trusts and calling this itself once satisfied. `authorize` is
    /// what every other caller wants.
    ///
    /// Pins the key of the first connection that succeeded. **Never overwrites a pin** — but a
    /// second call offering a *different* key for an already-pinned `peer_slug` is reported as
    /// `Err(TofuError::Changed)`, not silently swallowed as `Ok`. A caller that pins without
    /// having called `check` first must still learn a substitution was attempted; returning
    /// `Ok` there would hide the exact event this store exists to surface. Only a re-pin of the
    /// identical key — a true no-op — returns `Ok(())`.
    ///
    /// `peer_slug` **must be a name the user chose, and one that no server can re-issue** —
    /// see [`Self::check`] and the module docs.
    ///
    /// `endpoint_id` is canonicalized before it is compared *or stored* — see
    /// `canonical_endpoint_id`. Rejects anything that does not parse as a key at all, rather
    /// than storing it verbatim: a garbage string in the ledger could never be matched back
    /// against a real key offered later, which would make every future connection under this
    /// slug read as `Changed` forever.
    pub async fn pin_preapproved(&self, peer_slug: &str, endpoint_id: &str) -> Result<(), TofuError> {
        let endpoint_id = canonical_endpoint_id(endpoint_id)?;
        // Keeps same-process tasks that share this instance (via clone) off the lock-file
        // retry loop entirely: they serialize here first, so at most one of them ever
        // touches the lock file at a time.
        let _guard = self.write_lock.lock().await;
        // The lock file lives next to the ledger, so the directory has to exist before we
        // can even attempt to create it — this has to run before `acquire_lock_file`, not
        // just before `write`. A node's first ever run, writing into a directory nobody has
        // created yet, is the most common way to hit this, not an edge case.
        self.ensure_parent_dir().await?;
        // The mutex above is per-process and, without a clone, per-instance. Two separate
        // `TofuStore`s over the same path do not share it, so the read-modify-write below
        // still races without a lock every writer actually sees: the filesystem.
        let file_lock = self.acquire_lock_file().await?;
        let mut ledger = self.read().await?;
        if let Some(entry) = ledger.peers.get(peer_slug) {
            return if entry.endpoint_id == endpoint_id {
                Ok(())
            } else {
                Err(TofuError::Changed { pinned: entry.endpoint_id.clone(), offered: endpoint_id })
            };
        }
        ledger.peers.insert(peer_slug.to_string(), Entry { endpoint_id, first_seen_ms: now_ms() });
        self.write(&ledger, &file_lock).await
    }

    /// Removes a pin, and says whether there was one to remove.
    ///
    /// **Forgetting is not a small operation.** The next connection under this slug will be a
    /// first sight again, so whatever key turns up then is what gets pinned — which is precisely
    /// what [`Self::check`] refuses to let happen silently. That is the right behaviour for the
    /// case this exists for (a peer was reinstalled, its key is legitimately new, and a human
    /// established that out of band) and a hole in every other case. So this is deliberately not
    /// reachable from anything automatic: it is for a person who has decided.
    ///
    /// `Ok(false)` for a slug that was not pinned. Not an error, because "make sure this is not
    /// pinned" is a reasonable thing to ask and it is already true.
    pub async fn forget(&self, peer_slug: &str) -> Result<bool, TofuError> {
        let _guard = self.write_lock.lock().await;
        self.ensure_parent_dir().await?;
        let file_lock = self.acquire_lock_file().await?;
        let mut ledger = self.read().await?;
        if ledger.peers.remove(peer_slug).is_none() {
            return Ok(false);
        }
        self.write(&ledger, &file_lock).await?;
        Ok(true)
    }

    /// Every pin, as `(slug, endpoint_id)`, sorted by slug.
    ///
    /// For showing a person what this machine currently trusts. Reads without taking the write
    /// lock: a listing that raced a concurrent pin would show the state from a moment earlier,
    /// which is what any listing does, and blocking a writer to avoid that would be the worse
    /// trade.
    pub async fn pins(&self) -> Result<Vec<(String, String)>, TofuError> {
        let ledger = self.read().await?;
        let mut pins: Vec<(String, String)> =
            ledger.peers.into_iter().map(|(slug, entry)| (slug, entry.endpoint_id)).collect();
        pins.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(pins)
    }

    /// The lock file's path. Appends a suffix rather than replacing the extension
    /// (`with_extension`) — a ledger that itself happened to end in `.lock` would make the
    /// lock and the ledger the same file, the same class of bug Phase 1 hit when a temp file
    /// named via `with_extension("part")` collided with its own destination.
    fn lock_path(&self) -> PathBuf {
        let mut name = self.path.as_os_str().to_owned();
        name.push(".lock");
        PathBuf::from(name)
    }

    /// Waits for exclusive access to the ledger via a `<ledger>.lock` file, created with
    /// `create_new` so two writers can never both believe they hold it. A lock file held by a
    /// live writer is never removed here — stealing it from someone still working would
    /// reintroduce the exact lost-pin bug the lock exists to close — but one whose age crosses
    /// `LOCK_STALE_THRESHOLD` almost certainly has no live owner, so that one gets broken.
    async fn acquire_lock_file(&self) -> Result<LockFile, TofuError> {
        let lock_path = self.lock_path();
        let deadline = tokio::time::Instant::now() + self.lock_acquire_timeout;
        static LOCK_SEQ: AtomicU64 = AtomicU64::new(0);
        loop {
            let opened = {
                #[cfg(unix)]
                {
                    tokio::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(&lock_path)
                        .await
                }
                #[cfg(not(unix))]
                {
                    tokio::fs::OpenOptions::new().write(true).create_new(true).open(&lock_path).await
                }
            };
            match opened {
                Ok(mut file) => {
                    // A nonce identifies *this* acquisition. `write` reads it back right
                    // before publishing: if it does not match, someone else's stale-lock
                    // break raced ours, and the write must not go out under a lock we no
                    // longer hold.
                    // Nanoseconds, not milliseconds: a bare pid + millisecond can repeat
                    // across PID namespaces — two containers sharing a mounted config volume
                    // differ only by the millisecond otherwise. The temp file name already
                    // uses this same resolution for the same reason.
                    let nonce = format!(
                        "{}-{}-{}",
                        std::process::id(),
                        current_nanos(),
                        LOCK_SEQ.fetch_add(1, Ordering::Relaxed)
                    );
                    use tokio::io::AsyncWriteExt;
                    file.write_all(nonce.as_bytes()).await.map_err(|e| TofuError::Io(e.to_string()))?;
                    // `write_all` on an open `tokio::fs::File` can return before the actual
                    // write syscall — dispatched to the blocking pool — has completed; the
                    // next `poll_write` would wait for it, but there is no next one here.
                    // Without this, `verify_lock_still_held`'s later read can race the
                    // in-flight write and see the file still empty, aborting a publish that
                    // had nothing wrong with it. `create_temp` hits the identical fact a few
                    // lines below (see its comment) — it just happens to be covered there by
                    // `sync_all` already waiting for the write internally.
                    //
                    // `flush()`, not `sync_all()`: this file only needs to be *readable* by
                    // the next reader in this run, not to survive a crash — a crash leaves it
                    // stale and `break_if_stale` already handles that. Upgrading this to an
                    // fsync on every single pin would be pure cost for a guarantee nothing
                    // here needs.
                    file.flush().await.map_err(|e| TofuError::Io(e.to_string()))?;
                    return Ok(LockFile { path: lock_path, nonce });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(TofuError::Io(format!(
                            "the pin file lock at {} still exists after {:?} of retrying and \
                             could not be taken. If the process that created it is gone, \
                             delete this file and try again.",
                            lock_path.display(),
                            self.lock_acquire_timeout
                        )));
                    }
                    if self.break_if_stale(&lock_path).await {
                        // Likely just cleared a dead holder's lock: retry `create_new`
                        // immediately rather than sleeping first.
                        continue;
                    }
                    tokio::time::sleep(LOCK_RETRY_INTERVAL).await;
                }
                Err(e) => return Err(TofuError::Io(e.to_string())),
            }
        }
    }

    /// Removes `lock_path` if its last modification is older than `LOCK_STALE_THRESHOLD`.
    /// Returns whether it did. A lock whose age cannot be determined (metadata error, e.g. it
    /// was already removed by whoever else noticed it was stale) is left alone rather than
    /// guessed about; the caller's normal retry loop covers that case either way.
    async fn break_if_stale(&self, lock_path: &Path) -> bool {
        let Ok(meta) = tokio::fs::metadata(lock_path).await else { return false };
        let Ok(modified) = meta.modified() else { return false };
        let Ok(age) = std::time::SystemTime::now().duration_since(modified) else { return false };
        if age < LOCK_STALE_THRESHOLD {
            return false;
        }
        tokio::fs::remove_file(lock_path).await.is_ok()
    }

    /// Re-reads the lock file right before publishing and confirms it still holds the nonce
    /// this call was handed when it acquired the lock. This is what makes breaking a stale
    /// lock safe: two writers can briefly both believe they hold it (one genuine, one that
    /// broke a lock it thought was abandoned), but only the one whose nonce is still there
    /// when it goes to publish is allowed to. Without this check, breaking a stale lock would
    /// just reintroduce the lost-pin race the lock exists to close.
    async fn verify_lock_still_held(&self, lock: &LockFile) -> Result<(), TofuError> {
        let current = tokio::fs::read(&lock.path).await.map_err(|e| {
            TofuError::Io(format!(
                "could not confirm the pin file lock at {} was still ours before publishing \
                 ({e}); refusing to write",
                lock.path.display()
            ))
        })?;
        if current != lock.nonce.as_bytes() {
            return Err(TofuError::Io(format!(
                "the pin file lock at {} no longer holds our nonce — someone else took over \
                 while we were writing, most likely by breaking it as stale; refusing to \
                 publish a write that could race theirs",
                lock.path.display()
            )));
        }
        Ok(())
    }

    async fn read(&self) -> Result<Ledger, TofuError> {
        match tokio::fs::read(self.path.as_path()).await {
            Ok(bytes) => {
                let mut ledger: Ledger =
                    serde_json::from_slice(&bytes).map_err(|e| TofuError::Malformed {
                        path: self.path.display().to_string(),
                        reason: e.to_string(),
                    })?;
                canonicalize_ledger_entries(&mut ledger, &self.path)?;
                Ok(ledger)
            }
            // No file yet is the honest empty case: nothing has ever been pinned.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Ledger::default()),
            Err(e) => Err(TofuError::Io(e.to_string())),
        }
    }

    async fn write(&self, ledger: &Ledger, lock: &LockFile) -> Result<(), TofuError> {
        let text = serde_json::to_vec_pretty(ledger)
            .map_err(|e| TofuError::Io(format!("could not serialize the pin file: {e}")))?;

        let temp = self.create_temp(&text).await?;
        if let Err(e) = self.verify_lock_still_held(lock).await {
            let _ = tokio::fs::remove_file(&temp).await;
            return Err(e);
        }
        if let Err(e) = tokio::fs::rename(&temp, self.path.as_path()).await {
            // `create_temp` only ever hands back a path it created itself, so a failed
            // rename here is still ours to clean up.
            let _ = tokio::fs::remove_file(&temp).await;
            return Err(TofuError::Io(e.to_string()));
        }
        // The rename itself has to survive a crash, or the newest pin comes back as
        // "never seen" — a peer we already pinned would be trusted fresh. But the pin is
        // already durable at this point (the rename above completed), so a failure here is
        // not a failure to pin — it is a failure to guarantee that pin survives a crash
        // before the next successful sync. Reporting it as `Err` would tell a caller "no pin
        // exists" when one plainly does; that is the wrong direction to be wrong in.
        #[cfg(unix)]
        if let Err(e) = self.fsync_parent_dir().await {
            tracing::warn!(
                error = %e,
                path = %self.path.display(),
                "pin landed but syncing its parent directory failed; it may not survive a crash \
                 before the next successful sync"
            );
        }
        Ok(())
    }

    /// Creates a temp file that only this call owns, writes `text` to it, and fsyncs it.
    /// Returns the path only once every step succeeded — a file this function did not
    /// itself create is never touched, including on a name collision (see below).
    async fn create_temp(&self, text: &[u8]) -> Result<PathBuf, TofuError> {
        let io = |e: std::io::Error| TofuError::Io(e.to_string());
        static SEQ: AtomicU64 = AtomicU64::new(0);

        for _ in 0..TEMP_CREATE_ATTEMPTS {
            // pid + a monotonic counter can still collide across process namespaces, where
            // two independent processes both start at a low pid and the same counter value.
            // Nanoseconds on top mean a collision needs a genuine coincidence, and on top of
            // that, a collision here is a *retry with a new name*, never a deletion.
            let nanos = current_nanos();
            let temp = self.path.with_extension(format!(
                "{}.{}.{}.tmp",
                std::process::id(),
                nanos,
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));

            let opened = {
                #[cfg(unix)]
                {
                    tokio::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(&temp)
                        .await
                }
                #[cfg(not(unix))]
                {
                    tokio::fs::OpenOptions::new().write(true).create_new(true).open(&temp).await
                }
            };
            let mut file = match opened {
                Ok(file) => file,
                // Someone else's temp file, not ours: a new name is the fix, not deleting a
                // file we never created — that file might still be mid-write by its owner.
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(io(e)),
            };

            use tokio::io::AsyncWriteExt;
            if let Err(e) = file.write_all(text).await {
                // From here on we own `temp`: `create_new` succeeded, so no one else has
                // this exact path, and cleaning it up on failure is ours to do.
                let _ = tokio::fs::remove_file(&temp).await;
                return Err(io(e));
            }
            // `write_all` returning does not mean the bytes are on disk — tokio hands the
            // real write to a blocking pool and returns. Renaming before this lands puts a
            // zero-length ledger in place, wiping every pin. Task 2.3 measured 474/500.
            if let Err(e) = file.sync_all().await {
                let _ = tokio::fs::remove_file(&temp).await;
                return Err(io(e));
            }
            return Ok(temp);
        }

        Err(TofuError::Io(format!(
            "could not create a temp file for {} after {TEMP_CREATE_ATTEMPTS} attempts \
             (names kept colliding)",
            self.path.display()
        )))
    }

    fn parent_dir(&self) -> Option<&Path> {
        self.path.parent().filter(|p| !p.as_os_str().is_empty())
    }

    /// Creates the ledger's parent directory at `0700` instead of the default umask, if it
    /// does not exist yet. Only covers directories this call actually creates — a
    /// pre-existing parent keeps whatever permissions it already had; chmod'ing a directory
    /// we did not create ourselves could surprise whoever owns it more than it protects us.
    /// `key.rs` makes the same `0700`-on-create call for the key file itself. Has to run
    /// before anything else touches this directory, including the lock file in
    /// `acquire_lock_file`: a node's first run, before this directory has ever been created,
    /// is the common case, not a corner one.
    async fn ensure_parent_dir(&self) -> Result<(), TofuError> {
        if let Some(parent) = self.parent_dir() {
            let mut builder = tokio::fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            builder.mode(0o700);
            builder.create(parent).await.map_err(|e| TofuError::Io(e.to_string()))?;
        }
        Ok(())
    }

    /// fsyncs the ledger's parent directory so the rename in `write` survives a crash.
    /// Errors are propagated, not swallowed: a bare relative path (`"peers.json"`) has a
    /// parent of `Some("")`, which does not open, so that case falls back to `"."`.
    #[cfg(unix)]
    async fn fsync_parent_dir(&self) -> Result<(), TofuError> {
        let parent = self.parent_dir().unwrap_or_else(|| Path::new("."));
        let dir = tokio::fs::File::open(parent).await.map_err(|e| TofuError::Io(e.to_string()))?;
        dir.sync_all().await.map_err(|e| TofuError::Io(e.to_string()))
    }
}

/// Holds the `<ledger>.lock` file for the lifetime of one `pin_preapproved` call. Removing it
/// on drop is the only cleanup that reliably runs on an early return through `?`, which is why
/// this is sync `std::fs::remove_file` in `Drop` rather than an async method someone has to
/// remember to call.
struct LockFile {
    path: PathBuf,
    /// Written into the lock file at creation and re-checked by `verify_lock_still_held`
    /// before publishing. See that method for why this matters.
    nonce: String,
}

impl Drop for LockFile {
    fn drop(&mut self) {
        // Only remove the file if it still holds our nonce. Nothing refreshes a lock file's
        // mtime while it is held, so a holder that merely runs long — not dead, just slow —
        // can cross `LOCK_STALE_THRESHOLD` and have another writer legitimately break its
        // lock and take over. Removing unconditionally here would then delete *that* writer's
        // lock file once we finally get around to dropping ours, letting a third writer in
        // while the second still believes it is exclusive — and the same failure repeats down
        // the chain from there. `verify_lock_still_held` already stops us from publishing in
        // that situation; this stops us from also erasing whoever is holding the lock now.
        //
        // There is a small window between this read and the `remove_file` below where the
        // file could change again (the same lock gets broken a second time). Left as is: closing
        // it needs a second lock to guard the first, which is the kind of scheme this file is
        // trying not to be.
        if std::fs::read(&self.path).ok().as_deref() == Some(self.nonce.as_bytes()) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Parses `endpoint_id` and returns its canonical form — `iroh::EndpointId`'s `Display`,
/// always lowercase hex — never the caller's original spelling. `check`, `authorize`, and
/// `pin_preapproved` all run this before doing anything else with an `endpoint_id`, so every
/// comparison in this module and everything the ledger ever stores is in this one form. That
/// is what makes two accepted spellings of the identical key (lowercase hex, and RFC 4648
/// base32 in either case — see `fingerprint.rs`'s `parse`) compare equal instead of one
/// surfacing as a spurious `Changed` against the other, and it is also the single gate that
/// keeps unparseable garbage out of the ledger in the first place: nothing downstream of this
/// call ever sees the original string again.
fn canonical_endpoint_id(endpoint_id: &str) -> Result<String, TofuError> {
    Ok(crate::fingerprint::parse(endpoint_id)?.to_string())
}

/// Re-canonicalizes every entry's `endpoint_id` in place, right after the ledger is
/// deserialized — chosen over failing on a non-canonical (but validly parseable) spelling
/// because **hand-editing this file is the only documented way to un-pin a peer** (see the
/// module docs). A human recovering from a `Changed` error deletes the stale entry and pastes
/// the real key back in from wherever they copied it, which has no reason to be in this
/// store's own canonical (hex) form. Without this pass, a re-added entry pasted in as valid
/// base32 would compare unequal, byte for byte, against the canonical hex string every future
/// `check`/`authorize` call derives from the *same* key — turning the one recovery path this
/// design offers into a permanent, unexplained `Changed` for anyone who does not happen to
/// paste the exact spelling this code writes internally. Runs on every `read()`, not once at
/// startup, since the file can be hand-edited again between calls.
///
/// Still fails closed on a `peer_slug` whose `endpoint_id` does not parse as a key **at all**
/// — that is corruption, not a spelling choice, and the existing "a ledger we cannot read is
/// an error" rule (see the module docs) covers it the same way a bad top-level JSON shape
/// already does. The error names which slug is at fault, since a human fixing this by hand
/// needs to know where to look.
fn canonicalize_ledger_entries(ledger: &mut Ledger, path: &Path) -> Result<(), TofuError> {
    for (peer_slug, entry) in ledger.peers.iter_mut() {
        let key = crate::fingerprint::parse(&entry.endpoint_id).map_err(|e| TofuError::Malformed {
            path: path.display().to_string(),
            reason: format!("entry {peer_slug:?}: {e}"),
        })?;
        entry.endpoint_id = key.to_string();
    }
    Ok(())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
thread_local! {
    /// Lets `create_temp`'s tests make its candidate name predictable without changing what
    /// a real build does — `current_nanos` only ever consults this under `#[cfg(test)]`, and
    /// only when a test has actually set it.
    static TEST_NANOS_OVERRIDE: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

fn current_nanos() -> u32 {
    // Gated on `#[cfg(test)]`, same as `TEST_NANOS_OVERRIDE`'s declaration above: this branch
    // does not exist in a release build, so outside `cargo test` this function is always the
    // real `SystemTime` read below — including for the lock nonce in `acquire_lock_file`,
    // which calls this same function. There is no seam in the production nonce path.
    #[cfg(test)]
    if let Some(nanos) = TEST_NANOS_OVERRIDE.with(|cell| cell.get()) {
        return nanos;
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `acquire_lock_file` must not return until the nonce it wrote is actually readable back
    /// off disk — `tokio::fs::File::write_all` alone does not guarantee that (see the comment
    /// at its call site).
    ///
    /// **This is a repeated sample, not a deterministic assertion, on purpose.** Any single
    /// iteration only has roughly even odds of catching the missing flush: measured at 30/30
    /// in isolation but 18/30 (60%) under full-suite contention for one earlier version of
    /// this test, and independently reproduced at 29/30 and 16/30 by a reviewer. A *reliably
    /// deterministic* single-shot reproduction would need the blocking pool starved between
    /// the lock file's `open` and its `write` inside `acquire_lock_file` — reachable only
    /// through a production seam (none exists, and none should just for this) or by coupling
    /// this test to tokio's undocumented internal queue behavior. Neither is worth it. Looping
    /// over enough independent attempts instead makes the aggregate catch rate the guarantee:
    /// at even a pessimistic 50% per-iteration rate, missing all of many dozen in a row is
    /// astronomically unlikely, and 0 misses out of many dozen is the expected result once the
    /// flush actually makes the guarantee synchronous instead of probabilistic. Each iteration
    /// uses its own fresh directory and path — a loop over one shared lock file would just
    /// serialize the iterations against each other instead of independently resampling the
    /// same race.
    #[tokio::test]
    async fn the_lock_nonce_is_readable_immediately_after_acquiring() {
        for i in 0..64 {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("peers.json");
            let store = TofuStore::new(&path);
            store.ensure_parent_dir().await.unwrap();

            let lock = store.acquire_lock_file().await.unwrap();
            // A synchronous read on this same thread, not `tokio::fs::read`: dispatching a
            // second task to the blocking pool for the read gives the write's own already
            // in-flight blocking-pool task incidental extra time to finish first, which was
            // masking the race in earlier testing.
            let on_disk = std::fs::read(&lock.path).unwrap();
            assert_eq!(
                on_disk,
                lock.nonce.as_bytes(),
                "iteration {i}: the nonce was not on disk immediately after acquire_lock_file returned"
            );
        }
    }

    /// `LockFile::drop` must only remove the lock file if it still holds this guard's own
    /// nonce. Simulates a stale-break-and-takeover happening while this guard is still alive
    /// (a live holder that merely ran long past `LOCK_STALE_THRESHOLD`, not a dead one): the
    /// file at `lock.path` now belongs to whoever broke it, not to this guard, so dropping
    /// this guard must leave it alone. Getting this wrong is a cascade, not a one-off: an
    /// unconditional remove here would delete the new holder's lock, letting a third writer
    /// acquire one the second still believes it holds.
    #[tokio::test]
    async fn a_dropped_guard_never_removes_a_lock_it_no_longer_holds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("peers.json");
        let store = TofuStore::new(&path);
        store.ensure_parent_dir().await.unwrap();

        let lock = store.acquire_lock_file().await.unwrap();
        let lock_path = lock.path.clone();
        tokio::fs::write(&lock_path, b"someone-elses-nonce").await.unwrap();

        drop(lock);

        let survived = tokio::fs::read(&lock_path).await.unwrap();
        assert_eq!(
            survived,
            b"someone-elses-nonce",
            "drop removed a lock file it no longer held"
        );
    }

    /// Exercises the path that gates `write`'s rename: if the lock file's contents no longer
    /// match the nonce this call was handed, the write must refuse to publish and must not
    /// have renamed anything into place. This is the "someone broke our lock as stale and
    /// took over" case from the module docs, driven by calling `write` directly (private, but
    /// reachable from this inline module) with a pre-corrupted lock, since reproducing it
    /// through the public API racing a real second writer would need a timing seam in
    /// `pin_preapproved` itself. Going through `write` rather than calling
    /// `verify_lock_still_held` in isolation also covers that `write` actually calls it at the
    /// right point, not just that the check itself is correct.
    #[tokio::test]
    async fn a_lock_that_changed_hands_refuses_to_publish() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("peers.json");
        let store = TofuStore::new(&path);
        store.ensure_parent_dir().await.unwrap();

        let lock = store.acquire_lock_file().await.unwrap();
        // Simulate another writer deciding our lock was abandoned, breaking it, and taking
        // over: the file now holds a different nonce than the one we were handed.
        tokio::fs::write(&lock.path, b"someone-elses-nonce").await.unwrap();

        let mut ledger = Ledger::default();
        ledger.peers.insert(
            "kitchen-pi".to_string(),
            Entry { endpoint_id: "key-1".to_string(), first_seen_ms: 0 },
        );
        let result = store.write(&ledger, &lock).await;
        assert!(matches!(result, Err(TofuError::Io(_))), "got {result:?}");
        // Nothing must have been published: a caller must never see this pin as landed when
        // the lock guarding it was not ours anymore by the time we tried to publish.
        assert!(
            !tokio::fs::try_exists(&path).await.unwrap(),
            "the ledger was written despite the stolen lock"
        );
    }

    /// A ledger that happens to already end in `.lock` is the pathological case:
    /// `with_extension("lock")` would leave such a path unchanged, making the lock file and
    /// the ledger the very same file — the same class of bug as `with_extension("part")`
    /// making a temp file collide with its own destination. No existing black-box test
    /// constructs a ledger path shaped like this, so this checks the property directly.
    #[test]
    fn lock_path_never_collides_with_the_ledger_path() {
        let store = TofuStore::new(PathBuf::from("/tmp/example/peers.lock"));
        assert_ne!(store.lock_path().as_path(), store.path.as_path());
    }

    /// `create_temp` never deletes a file it did not create, retrying under a new name
    /// instead. `nanos` is pinned via `TEST_NANOS_OVERRIDE` so the candidate names are
    /// predictable up to the `SEQ` counter's exact starting value — which this test does not
    /// assume is 0, since `SEQ` is shared process-wide with every other call to `create_temp`
    /// in this test binary (including `a_lock_that_changed_hands_refuses_to_publish`, whose
    /// execution order relative to this test is not guaranteed under parallel test
    /// execution). Planting a short run of consecutive candidate names instead of one exact
    /// name means the real collision is still exercised regardless of exactly where `SEQ`
    /// happens to be, as long as it has not drifted past this run — implausible given at most
    /// one other test increments it, by exactly one, before this one might run. This must
    /// stay on the default (single-threaded) `#[tokio::test]` runtime, since the thread-local
    /// override would not be visible if the task migrated to another worker thread.
    #[tokio::test]
    async fn a_temp_name_collision_retries_instead_of_deleting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("peers.json");
        let store = TofuStore::new(&path);

        TEST_NANOS_OVERRIDE.with(|cell| cell.set(Some(0)));
        let planted: Vec<PathBuf> = (0..4)
            .map(|seq| path.with_extension(format!("{}.0.{seq}.tmp", std::process::id())))
            .collect();
        for p in &planted {
            tokio::fs::write(p, b"PLANTED").await.unwrap();
        }

        let key = iroh::SecretKey::generate().public().to_string();
        let result = store.pin_preapproved("kitchen-pi", &key).await;
        TEST_NANOS_OVERRIDE.with(|cell| cell.set(None));
        result.unwrap();

        // Every planted file must survive untouched: a collision has to be resolved by
        // trying a new name, never by deleting a file this call did not create.
        for p in &planted {
            let survived = tokio::fs::read(p).await.unwrap();
            assert_eq!(survived, b"PLANTED", "a planted file was touched by the retry: {p:?}");
        }
        assert!(store.check("kitchen-pi", &key).await.is_ok(), "pin did not survive the collision");
    }
}
