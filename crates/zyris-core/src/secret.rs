//! Somewhere to keep a secret that survives a restart.
//!
//! The OS keychain where there is one, a `0600` file where there is not — a headless box with no
//! Secret Service is a normal deployment, not an error, and refusing to run there would be worse
//! than the weaker storage.
//!
//! This module knows nothing about Zyris. It stores a string under a name.

use std::io;
use std::path::PathBuf;

/// Which backend a store actually ended up on. Worth surfacing: a user on a headless box should
/// be able to find out that their credential is in a file rather than a keychain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Keychain,
    File,
}

#[derive(Debug)]
pub enum SecretError {
    /// The backing store refused. Carries the backend's own words rather than flattening them.
    Backend(String),
    Io(io::Error),
}

impl std::fmt::Display for SecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecretError::Backend(message) => write!(f, "secret store: {message}"),
            SecretError::Io(error) => write!(f, "secret store: {error}"),
        }
    }
}

impl std::error::Error for SecretError {}

impl From<io::Error> for SecretError {
    fn from(error: io::Error) -> SecretError {
        SecretError::Io(error)
    }
}

/// Named secrets that outlive the process.
#[derive(Clone)]
pub struct SecretStore {
    service: String,
    /// `None` means "use the keychain". A path means the fallback was chosen — either because
    /// this machine has no keychain, or because a test asked for it.
    file_dir: Option<PathBuf>,
}

impl SecretStore {
    /// Picks the keychain if this machine has a working one, and a file under the user's config
    /// directory if it does not. The probe is a real read: a Secret Service that is installed but
    /// not running answers the same way as one that is absent, and only trying tells them apart.
    pub fn new(service: &str) -> SecretStore {
        let probe = keyring::Entry::new(service, "__probe__").and_then(|entry| match entry.get_password() {
            Ok(_) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error),
        });

        match probe {
            Ok(()) => SecretStore { service: service.to_string(), file_dir: None },
            Err(error) => {
                tracing::warn!(%error, "no usable keychain; keeping secrets in a 0600 file instead");
                SecretStore { service: service.to_string(), file_dir: Some(default_file_dir(service)) }
            }
        }
    }

    /// Pins the file backend at a directory of the caller's choosing. Tests use this so they
    /// never touch the developer's own keyring.
    pub fn with_file_dir(service: &str, dir: PathBuf) -> SecretStore {
        SecretStore { service: service.to_string(), file_dir: Some(dir) }
    }

    pub fn backend(&self) -> Backend {
        if self.file_dir.is_some() { Backend::File } else { Backend::Keychain }
    }

    pub fn get(&self, name: &str) -> Result<Option<String>, SecretError> {
        match &self.file_dir {
            Some(dir) => match std::fs::read_to_string(dir.join(name)) {
                Ok(value) => Ok(Some(value)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error.into()),
            },
            None => match self.entry(name)?.get_password() {
                Ok(value) => Ok(Some(value)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(error) => Err(SecretError::Backend(error.to_string())),
            },
        }
    }

    pub fn set(&self, name: &str, value: &str) -> Result<(), SecretError> {
        match &self.file_dir {
            Some(dir) => {
                std::fs::create_dir_all(dir)?;
                let path = dir.join(name);
                std::fs::write(&path, value)?;
                restrict(&path)?;
                Ok(())
            }
            None => self
                .entry(name)?
                .set_password(value)
                .map_err(|error| SecretError::Backend(error.to_string())),
        }
    }

    /// Absent is already the state the caller wanted, so removing nothing succeeds.
    pub fn delete(&self, name: &str) -> Result<(), SecretError> {
        match &self.file_dir {
            Some(dir) => match std::fs::remove_file(dir.join(name)) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error.into()),
            },
            None => match self.entry(name)?.delete_credential() {
                Ok(()) => Ok(()),
                Err(keyring::Error::NoEntry) => Ok(()),
                Err(error) => Err(SecretError::Backend(error.to_string())),
            },
        }
    }

    fn entry(&self, name: &str) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(&self.service, name).map_err(|error| SecretError::Backend(error.to_string()))
    }
}

fn default_file_dir(service: &str) -> PathBuf {
    directories::ProjectDirs::from("cc", "attacca", service)
        .map(|dirs| dirs.config_dir().join("secrets"))
        .unwrap_or_else(|| PathBuf::from(".").join(".zyris-secrets"))
}

#[cfg(unix)]
fn restrict(path: &std::path::Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

/// On Windows the file inherits the user profile's ACL, which is already owner-only. There is no
/// mode to set, and pretending otherwise by returning an error would be worse than doing nothing.
#[cfg(not(unix))]
fn restrict(_path: &std::path::Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every test pins the file backend explicitly. The keychain is the machine's, and a test
    // that wrote to it would leave entries behind on the developer's own login keyring.
    fn store(dir: &std::path::Path) -> SecretStore {
        SecretStore::with_file_dir("zyris-test", dir.to_path_buf())
    }

    #[test]
    fn a_missing_secret_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();

        assert_eq!(store(dir.path()).get("absent").unwrap(), None);
    }

    #[test]
    fn what_was_set_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());

        store.set("token", "znt_abc").unwrap();

        assert_eq!(store.get("token").unwrap(), Some("znt_abc".to_string()));
    }

    #[test]
    fn setting_twice_replaces_rather_than_appends() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());

        store.set("token", "first").unwrap();
        store.set("token", "second").unwrap();

        assert_eq!(store.get("token").unwrap(), Some("second".to_string()));
    }

    #[test]
    fn a_deleted_secret_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        store.set("token", "znt_abc").unwrap();

        store.delete("token").unwrap();

        assert_eq!(store.get("token").unwrap(), None);
    }

    #[test]
    fn deleting_something_absent_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();

        assert!(store(dir.path()).delete("absent").is_ok());
    }

    #[test]
    fn two_names_do_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());

        store.set("account", "a").unwrap();
        store.set("node", "b").unwrap();

        assert_eq!(store.get("account").unwrap(), Some("a".to_string()));
        assert_eq!(store.get("node").unwrap(), Some("b".to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn the_file_backend_writes_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());

        store.set("token", "znt_abc").unwrap();

        let mode = std::fs::metadata(dir.path().join("token")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "a refresh token in a world-readable file is the failure this prevents");
    }
}
