//! Pin a peer's `EndpointId` to **the first one we saw** (Trust On First Use).
//!
//! attacca issues node credentials and runs the rendezvous, so a "fake B" introduced to A
//! cannot be ruled out by cryptography alone. Pinning turns that into an attack that works
//! **once, never twice, and leaves a mark**.
//!
//! **There is no automatic way out.** A human has to edit the file. Accepting a changed key
//! quietly would make the pin worth nothing.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

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

#[derive(Debug, Default, Serialize, Deserialize)]
struct Ledger {
    #[serde(default)]
    peers: HashMap<String, Entry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Entry {
    endpoint_id: String,
    /// Never read by this code. It is here for the human who opens the file after a
    /// `Changed` error and needs to know when the pin was taken.
    first_seen_ms: u64,
}

/// Clone is cheap and clones share the write lock, so hand out clones instead of
/// rebuilding a store from its path — two stores over one file would each take their own
/// lock and neither would exclude the other.
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
        // Held across read-modify-write. Without it two concurrent pins both read the old
        // ledger and the second write drops the first peer's pin — silently un-pinning it.
        let _guard = self.write_lock.lock().await;
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
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| TofuError::Io(e.to_string()))?;
        }
        let text = serde_json::to_vec_pretty(ledger)
            .map_err(|e| TofuError::Io(format!("could not serialize the pin file: {e}")))?;

        // A fixed temp name would collide between processes sharing this file and each
        // would write into the other's half-written temp.
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let temp = self.path.with_extension(format!(
            "{}.{}.tmp",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));

        let result = self.write_temp(&temp, &text).await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&temp).await;
            return result;
        }
        tokio::fs::rename(&temp, self.path.as_path())
            .await
            .map_err(|e| TofuError::Io(e.to_string()))?;
        // The rename itself has to survive a crash, or the newest pin comes back as
        // "never seen" — a peer we already pinned would be trusted fresh.
        #[cfg(unix)]
        if let Some(parent) = self.path.parent() {
            if let Ok(dir) = tokio::fs::File::open(parent).await {
                let _ = dir.sync_all().await;
            }
        }
        Ok(())
    }

    async fn write_temp(&self, temp: &std::path::Path, text: &[u8]) -> Result<(), TofuError> {
        let io = |e: std::io::Error| TofuError::Io(e.to_string());
        #[cfg(unix)]
        {
            use tokio::io::AsyncWriteExt;
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(temp)
                .await
                .map_err(io)?;
            file.write_all(text).await.map_err(io)?;
            // `write_all` returning does not mean the bytes are on disk — tokio hands the
            // real write to a blocking pool and returns. Renaming before this lands puts a
            // zero-length ledger in place, wiping every pin. Task 2.3 measured 474/500.
            file.sync_all().await.map_err(io)?;
        }
        #[cfg(not(unix))]
        tokio::fs::write(temp, text).await.map_err(io)?;
        Ok(())
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
