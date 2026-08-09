//! Pin a peer's `EndpointId` to **the first one we saw** (Trust On First Use).
//!
//! attacca issues node credentials and runs the rendezvous, so a "fake B" introduced to A
//! cannot be ruled out by cryptography alone. Pinning turns that into an attack that works
//! **once, never twice, and leaves a mark**.
//!
//! **There is no automatic way out.** A human has to edit the file. Accepting a changed key
//! quietly would make the pin worth nothing.
//!
//! Two writers can be two `tokio::task`s in this process or two entirely separate
//! processes sharing the same path — both are possible once a node runs more than one
//! connection. An in-process `tokio::sync::Mutex` only ever sees the first kind, so `pin`
//! also takes a `<ledger>.lock` file: the one thing every writer, in any process, actually
//! shares.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum TofuError {
    #[error("this node's key changed. pinned: {pinned}, offered: {offered}")]
    Changed { pinned: String, offered: String },
    #[error("the pin file at {path} could not be read ({reason}); refusing to continue")]
    Malformed { path: String, reason: String },
    #[error("{0}")]
    Io(String),
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

#[derive(Debug, Serialize, Deserialize)]
struct Entry {
    endpoint_id: String,
    /// Never read by this code. It is here for the human who opens the file after a
    /// `Changed` error and needs to know when the pin was taken.
    first_seen_ms: u64,
}

/// How long `pin` waits for another writer holding `<ledger>.lock` before giving up.
const LOCK_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long to sleep between attempts to take the lock file.
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(20);
/// How many names to try if a temp-file name collides with one already on disk.
const TEMP_CREATE_ATTEMPTS: u32 = 8;

/// Clone is cheap and clones share the in-process write lock, so hand out clones instead of
/// rebuilding a store from its path for same-process callers — it keeps them off the lock-file
/// retry loop below entirely. That in-process lock cannot help across processes, though: two
/// separate `TofuStore`s over the same path, cloned or not, in this process or another, still
/// have to serialize through the `<ledger>.lock` file that `pin` takes.
#[derive(Clone)]
pub struct TofuStore {
    path: Arc<PathBuf>,
    write_lock: Arc<tokio::sync::Mutex<()>>,
}

impl TofuStore {
    pub fn new(path: impl Into<PathBuf>) -> TofuStore {
        TofuStore { path: Arc::new(path.into()), write_lock: Arc::new(tokio::sync::Mutex::new(())) }
    }

    /// Checks the offered key against what is pinned. An unknown peer passes — pinning
    /// happens **after** a connection succeeds, not here.
    ///
    /// A ledger we cannot read is an error, not an empty ledger. See the module docs.
    pub async fn check(&self, node_id: &str, endpoint_id: &str) -> Result<(), TofuError> {
        let ledger = self.read().await?;
        match ledger.peers.get(node_id) {
            None => Ok(()),
            Some(entry) if entry.endpoint_id == endpoint_id => Ok(()),
            Some(entry) => Err(TofuError::Changed {
                pinned: entry.endpoint_id.clone(),
                offered: endpoint_id.to_string(),
            }),
        }
    }

    /// Pins the key of the first connection that succeeded. **Never overwrites a pin.**
    pub async fn pin(&self, node_id: &str, endpoint_id: &str) -> Result<(), TofuError> {
        // Keeps same-process tasks that share this instance (via clone) off the lock-file
        // retry loop entirely: they serialize here first, so at most one of them ever
        // touches the lock file at a time.
        let _guard = self.write_lock.lock().await;
        // The mutex above is per-process and, without a clone, per-instance. Two separate
        // `TofuStore`s over the same path do not share it, so the read-modify-write below
        // still races without a lock every writer actually sees: the filesystem.
        let _file_lock = self.acquire_lock_file().await?;
        let mut ledger = self.read().await?;
        if ledger.peers.contains_key(node_id) {
            return Ok(());
        }
        ledger.peers.insert(
            node_id.to_string(),
            Entry { endpoint_id: endpoint_id.to_string(), first_seen_ms: now_ms() },
        );
        self.write(&ledger).await
    }

    /// Waits for exclusive access to the ledger via a `<ledger>.lock` file, created with
    /// `create_new` so two writers can never both believe they hold it. A lock file already
    /// held by someone else is never removed here — stealing it is the exact failure this
    /// exists to prevent — so `pin` either waits it out or fails loudly.
    async fn acquire_lock_file(&self) -> Result<LockFile, TofuError> {
        let lock_path = self.path.with_extension("lock");
        let deadline = tokio::time::Instant::now() + LOCK_ACQUIRE_TIMEOUT;
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
                Ok(_file) => return Ok(LockFile { path: lock_path }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(TofuError::Io(format!(
                            "timed out after {}s waiting for the pin file lock at {} \
                             (held by another writer)",
                            LOCK_ACQUIRE_TIMEOUT.as_secs(),
                            lock_path.display()
                        )));
                    }
                    tokio::time::sleep(LOCK_RETRY_INTERVAL).await;
                }
                Err(e) => return Err(TofuError::Io(e.to_string())),
            }
        }
    }

    async fn read(&self) -> Result<Ledger, TofuError> {
        match tokio::fs::read(self.path.as_path()).await {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| TofuError::Malformed {
                path: self.path.display().to_string(),
                reason: e.to_string(),
            }),
            // No file yet is the honest empty case: nothing has ever been pinned.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Ledger::default()),
            Err(e) => Err(TofuError::Io(e.to_string())),
        }
    }

    async fn write(&self, ledger: &Ledger) -> Result<(), TofuError> {
        if let Some(parent) = self.parent_dir() {
            // `.mode(0o700)` instead of the default umask: the ledger's integrity depends on
            // nobody else being able to write into this directory. `key.rs` makes the same
            // call for the key file itself.
            let mut builder = tokio::fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            builder.mode(0o700);
            builder.create(parent).await.map_err(|e| TofuError::Io(e.to_string()))?;
        }
        let text = serde_json::to_vec_pretty(ledger)
            .map_err(|e| TofuError::Io(format!("could not serialize the pin file: {e}")))?;

        let temp = self.create_temp(&text).await?;
        if let Err(e) = tokio::fs::rename(&temp, self.path.as_path()).await {
            // `create_temp` only ever hands back a path it created itself, so a failed
            // rename here is still ours to clean up.
            let _ = tokio::fs::remove_file(&temp).await;
            return Err(TofuError::Io(e.to_string()));
        }
        // The rename itself has to survive a crash, or the newest pin comes back as
        // "never seen" — a peer we already pinned would be trusted fresh.
        #[cfg(unix)]
        self.fsync_parent_dir().await?;
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
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0);
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

/// Holds the `<ledger>.lock` file for the lifetime of one `pin` call. Removing it on drop is
/// the only cleanup that reliably runs on an early return through `?`, which is why this is
/// sync `std::fs::remove_file` in `Drop` rather than an async method someone has to remember
/// to call.
struct LockFile {
    path: PathBuf,
}

impl Drop for LockFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
