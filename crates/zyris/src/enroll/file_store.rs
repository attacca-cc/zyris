//! The default [`CredentialStore`]: one private file per deployment under the user's config dir.
//!
//! Be honest about the threat model: a file on disk cannot be protected from anyone who can
//! `sudo -u` the account that owns it. For a shared service account, the right answer is a static
//! `znt_` from your secret manager, which is exactly why static tokens remain supported. What this
//! backend *can* do is stop a credential from leaking through a permissive umask or a restored
//! tarball, and it does.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::enroll::store::{
    CredentialStore, CredentialStoreError, StoredCredential, CREDENTIAL_VERSION,
};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("could not determine a config directory; set ZYRIS_CONFIG_DIR")]
    NoConfigDir,
    #[error("credential file {path} is readable by other users (mode {mode:04o}); run: chmod 600 {path}")]
    Permissive { path: String, mode: u32 },
    #[error("credential file was written by a newer version ({found}, expected {CREDENTIAL_VERSION})")]
    UnknownVersion { found: u32 },
    #[error("credential file is corrupt: {0}")]
    Corrupt(serde_json::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl From<StoreError> for CredentialStoreError {
    fn from(error: StoreError) -> CredentialStoreError {
        match error {
            // Both need a human: one is an exposed secret, the other is a machine with nowhere to
            // put one. Neither is a reason to quietly enroll again.
            e @ (StoreError::Permissive { .. } | StoreError::NoConfigDir) => {
                CredentialStoreError::Refused(e.to_string())
            }
            // A file this build cannot read is a reason to enroll again, not to die. `Io` is here
            // rather than under `Other` deliberately: `NotFound` is already reported as "no
            // credential", so what remains is a file that exists and cannot be read, and looping on
            // it forever helps nobody.
            e @ (StoreError::UnknownVersion { .. } | StoreError::Corrupt(_) | StoreError::Io(_)) => {
                CredentialStoreError::Unusable(e.to_string())
            }
        }
    }
}

/// A credential kept in one file, private to the user running the node.
pub struct FileCredentialStore {
    path: PathBuf,
}

impl FileCredentialStore {
    /// The conventional location for a given deployment URL and profile.
    ///
    /// One file per `(deployment, profile)` so two nodes started concurrently against different
    /// servers cannot clobber each other, and no locking is needed to make that true.
    pub fn for_server(server_url: &str, profile: &str) -> Result<FileCredentialStore, StoreError> {
        Ok(FileCredentialStore {
            path: config_dir()?.join(format!("{}-{}.json", slugify(server_url), slugify(profile))),
        })
    }

    /// An exact path, for a node whose location is decided by something other than this crate.
    pub fn at(path: impl Into<PathBuf>) -> FileCredentialStore {
        FileCredentialStore { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[async_trait]
impl CredentialStore for FileCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredential>, CredentialStoreError> {
        Ok(load(&self.path)?)
    }

    async fn save(&self, credential: &StoredCredential) -> Result<(), CredentialStoreError> {
        Ok(save(&self.path, credential)?)
    }

    async fn clear(&self) -> Result<(), CredentialStoreError> {
        Ok(clear(&self.path)?)
    }

    fn describe(&self) -> String {
        self.path.display().to_string()
    }
}

/// `$ZYRIS_CONFIG_DIR`, else the platform's per-user config location.
///
/// Under `systemd` with `ProtectHome=yes` there is no usable `$HOME`. That case fails loudly and
/// tells the operator which variable to set, rather than silently writing a credential into the
/// working directory — which is how secrets end up committed.
pub fn config_dir() -> Result<PathBuf, StoreError> {
    if let Some(dir) = std::env::var_os("ZYRIS_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"));
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));

    base.map(|base| base.join("zyris")).ok_or(StoreError::NoConfigDir)
}

/// Read a stored credential, or `None` when there is none yet.
///
/// A file with group or other bits set is **refused**, the way `ssh` refuses a world-readable
/// private key. This is the cheapest possible mitigation for a credential restored from a tarball
/// or created under a permissive umask, and refusing is safer than silently repairing: the
/// operator should know it was exposed.
fn load(path: &Path) -> Result<Option<StoredCredential>, StoreError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path)?.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(StoreError::Permissive { path: path.display().to_string(), mode });
        }
    }

    let credential: StoredCredential =
        serde_json::from_slice(&bytes).map_err(StoreError::Corrupt)?;
    if credential.version != CREDENTIAL_VERSION {
        return Err(StoreError::UnknownVersion { found: credential.version });
    }
    Ok(Some(credential))
}

/// Write atomically: temp file, `sync_all`, rename. A node killed mid-write must not come back to
/// a half-written credential, because that is indistinguishable from a corrupt one and would
/// force a re-enrollment that needed a human.
fn save(path: &Path, credential: &StoredCredential) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
    }

    let temp = path.with_extension("tmp");
    let bytes = serde_json::to_vec_pretty(credential).map_err(StoreError::Corrupt)?;
    {
        use std::io::Write;
        let mut file = fs::File::create(&temp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Set before writing, so the secret is never briefly on disk under the umask's mode.
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    fs::rename(&temp, path)?;
    Ok(())
}

/// Forget a credential the server no longer honours, so the next start enrolls cleanly instead of
/// looping on a token that will never work again.
fn clear(path: &Path) -> Result<(), StoreError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Reduce a URL or profile name to something safe as a filename component.
fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_dash = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.extend(ch.to_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
        if out.len() >= 48 {
            break;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "default".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credential() -> StoredCredential {
        StoredCredential::new(
            "zna_access".into(),
            "znr_refresh".into(),
            "node-id".into(),
            "hello node".into(),
            "allen@example.com".into(),
            1_000_000,
        )
    }

    #[tokio::test]
    async fn save_then_load_round_trips_through_the_trait() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::at(dir.path().join("nested").join("creds.json"));
        assert_eq!(store.load().await.unwrap(), None, "a missing file is not an error");

        store.save(&credential()).await.unwrap();
        assert_eq!(store.load().await.unwrap().unwrap(), credential());

        store.clear().await.unwrap();
        assert_eq!(store.load().await.unwrap(), None);
        store.clear().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_file_is_private_and_a_permissive_one_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("creds.json");
        let store = FileCredentialStore::at(&path);
        store.save(&credential()).await.unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777, 0o700);

        // A credential restored from a tarball, or written under a lax umask, must be refused
        // rather than silently used — and refused in the shade that never gets thrown away, or the
        // node would answer an exposed secret by quietly enrolling again.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let error = store.load().await.unwrap_err();
        assert!(matches!(error, CredentialStoreError::Refused(_)));
        assert!(!error.is_discardable());
    }

    #[tokio::test]
    async fn a_file_from_the_future_is_refused_rather_than_guessed_at() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("creds.json");
        let mut future = credential();
        future.version = CREDENTIAL_VERSION + 1;
        save(&path, &future).unwrap();

        let error = FileCredentialStore::at(&path).load().await.unwrap_err();
        assert!(error.is_discardable(), "a file this build cannot read is re-enrollable");
    }

    #[tokio::test]
    async fn corrupt_json_is_discardable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("creds.json");
        fs::write(&path, b"{not json").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert!(FileCredentialStore::at(&path).load().await.unwrap_err().is_discardable());
    }

    /// One file per (deployment, profile), so concurrent starts against different servers cannot
    /// clobber each other and no locking is needed.
    #[test]
    fn paths_are_per_deployment_and_per_profile() {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: single-threaded test process; the var is read only through `config_dir`.
        unsafe { std::env::set_var("ZYRIS_CONFIG_DIR", dir.path()) };

        let prod = FileCredentialStore::for_server("wss://attacca.example/zyris/v1/ws", "default")
            .unwrap();
        let staging =
            FileCredentialStore::for_server("wss://staging.attacca.example/zyris/v1/ws", "default")
                .unwrap();
        let other =
            FileCredentialStore::for_server("wss://attacca.example/zyris/v1/ws", "builder").unwrap();
        assert_ne!(prod.path(), staging.path());
        assert_ne!(prod.path(), other.path());
        assert!(prod.path().starts_with(dir.path()));
        assert!(prod.describe().ends_with(".json"));

        unsafe { std::env::remove_var("ZYRIS_CONFIG_DIR") };
    }

    #[test]
    fn slugify_is_filename_safe() {
        assert_eq!(slugify("wss://attacca.example/zyris/v1/ws"), "wss-attacca-example-zyris-v1-ws");
        assert_eq!(slugify("///"), "default");
        assert_eq!(slugify(""), "default");
        assert!(!slugify("a/../../etc/passwd").contains('/'));
    }
}
