//! The node's ed25519 keypair. **The private key never leaves this machine.**
//!
//! Same rule as a credential store — reject anything that isn't `0600`. `FileCredentialStore`
//! checks mode for the same reason: a key someone else can read is not a key.

use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("key file permissions are {0:o}, must be 0600")]
    Permissions(u32),
    #[error("{0}")]
    Io(String),
    #[error("could not read key file")]
    Malformed,
}

pub async fn load_or_create(path: &Path) -> Result<iroh::SecretKey, KeyError> {
    match tokio::fs::read(path).await {
        Ok(bytes) => {
            check_permissions(path).await?;
            let array: [u8; 32] = bytes.as_slice().try_into().map_err(|_| KeyError::Malformed)?;
            Ok(iroh::SecretKey::from_bytes(&array))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => create(path).await,
        Err(e) => Err(KeyError::Io(e.to_string())),
    }
}

/// Creates a fresh key and writes it to `path`.
///
/// **Regenerating on every call would defeat TOFU pinning** — a peer that already pinned our
/// public key from a previous connection would see a stranger every time and refuse us. Writing
/// the key to disk and reading it back on subsequent calls is what makes us the same node run to
/// run.
async fn create(path: &Path) -> Result<iroh::SecretKey, KeyError> {
    let key = iroh::SecretKey::generate();
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| KeyError::Io(e.to_string()))?;
    }
    #[cfg(unix)]
    {
        // Open with mode 0600 from the very first byte written. Creating the file with the
        // default mode and narrowing permissions afterward would leave a window — however
        // short — during which another local user could read the private key before we lock
        // it down.
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .await
            .map_err(|e| KeyError::Io(e.to_string()))?;
        use tokio::io::AsyncWriteExt;
        file.write_all(&key.to_bytes()).await.map_err(|e| KeyError::Io(e.to_string()))?;
    }
    #[cfg(not(unix))]
    {
        tokio::fs::write(path, key.to_bytes()).await.map_err(|e| KeyError::Io(e.to_string()))?;
    }
    Ok(key)
}

/// Rejects a key file that anyone but the owner can read or write.
///
/// This is deliberately separate from [`create`]: a file can start life at `0600` and still end
/// up looser later (an admin script, a careless `chmod -R`, a restore from a backup that dropped
/// modes). Checking on every load catches that drift instead of trusting whatever made the file.
async fn check_permissions(path: &Path) -> Result<(), KeyError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = tokio::fs::metadata(path).await.map_err(|e| KeyError::Io(e.to_string()))?;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(KeyError::Permissions(mode));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
