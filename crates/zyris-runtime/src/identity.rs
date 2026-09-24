//! The one secret this app keeps between runs, and the rule for reading it back.
//!
//! Since zyris-protocol#43 that is a single `zc_` [`Credential`]: issued once by an enrollment,
//! never rotated, never expiring, and dialled with directly. The account credential and node token
//! this used to keep apart are gone from the protocol, and every server that speaks it refuses
//! them, so a machine that still has them on disk simply enrols again.

use zyris::Credential;

use crate::secret::{Backend, SecretError, SecretStore};

/// The name the credential lives under. Changing it strands whatever is already stored, so it is
/// a constant rather than a literal scattered through the file.
///
/// **Not `account-credential`**, which is what the account layer wrote: that JSON does not parse
/// as a [`Credential`] (the protocol's own test pins that it fails on a missing field), and a
/// name of its own keeps "nothing stored yet" from ever depending on that.
const CREDENTIAL: &str = "credential";

/// What the account layer stored, which no server honours any more. Deleted whenever this
/// identity is written or forgotten, so an upgraded machine does not keep a dead bearer on disk.
const RETIRED: &[&str] = &["account-credential", "node-token"];

/// Not a secret — just the name of whichever backend last held one. Lives beside the file
/// backend's own secrets (see `SecretStore::file_dir`) so it does not move depending on which
/// backend a given launch happens to probe its way onto.
const BACKEND_MARKER: &str = "backend";

#[derive(Clone)]
pub struct Identity {
    store: SecretStore,
}

impl Identity {
    pub fn new(store: SecretStore) -> Identity {
        Identity { store }
    }

    /// Unparseable stored JSON reads as absent. The only recovery from a corrupt secret is to
    /// enrol again, and that path starts by loading — so failing here would leave no way out.
    pub fn load(&self) -> Result<Option<Credential>, SecretError> {
        self.read(CREDENTIAL)
    }

    pub fn save(&self, credential: &Credential) -> Result<(), SecretError> {
        self.write(CREDENTIAL, credential)?;
        self.forget_retired();
        Ok(())
    }

    /// Clears the credential and forgets which backend held it — a genuinely cleared machine must
    /// enrol cleanly, with nothing left behind to disagree with whatever backend the next launch
    /// happens to resolve to.
    pub fn forget(&self) -> Result<(), SecretError> {
        self.store.delete(CREDENTIAL)?;
        self.forget_retired();
        if let Err(error) = self.clear_backend_marker() {
            // The secret itself is gone, which is what callers actually depend on; losing the
            // marker only means a stale backend name might outlive it, not that anything this
            // method promises has failed.
            tracing::warn!(%error, "could not clear the backend marker while forgetting this identity");
        }
        Ok(())
    }

    /// Best effort: a retired secret that will not delete is dead weight, not a failure.
    fn forget_retired(&self) {
        for name in RETIRED {
            if let Err(error) = self.store.delete(name) {
                tracing::warn!(%error, secret = name, "could not delete a retired secret");
            }
        }
    }

    /// Which backend this launch cannot reach the identity in, if any.
    ///
    /// `Some(backend)` means an earlier launch recorded the identity as living in `backend`, and
    /// this launch's `SecretStore` resolved to a *different* one — the state that must never be
    /// papered over by quietly starting a fresh enrolment, since that issues a second credential
    /// while the first one sits untouched in the backend this launch cannot see. `None` covers
    /// every safe case alike: nothing has ever been saved, this predates the marker, or the
    /// recorded backend and this launch's backend already agree.
    pub fn stranded_in(&self) -> Option<Backend> {
        match self.recorded_backend() {
            Some(recorded) if recorded != self.store.backend() => Some(recorded),
            _ => None,
        }
    }

    fn recorded_backend(&self) -> Option<Backend> {
        let path = self.store.file_dir().join(BACKEND_MARKER);
        let contents = std::fs::read_to_string(path).ok()?;
        parse_backend(contents.trim())
    }

    fn record_backend(&self, backend: Backend) -> Result<(), SecretError> {
        let dir = self.store.file_dir();
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join(BACKEND_MARKER), backend_name(backend))?;
        Ok(())
    }

    fn clear_backend_marker(&self) -> Result<(), SecretError> {
        match std::fs::remove_file(self.store.file_dir().join(BACKEND_MARKER)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn read<T: serde::de::DeserializeOwned>(&self, name: &str) -> Result<Option<T>, SecretError> {
        let Some(raw) = self.store.get(name)? else { return Ok(None) };
        match serde_json::from_str(&raw) {
            Ok(value) => Ok(Some(value)),
            Err(error) => {
                tracing::warn!(%error, secret = name, "stored secret could not be read; treating it as absent");
                Ok(None)
            }
        }
    }

    fn write<T: serde::Serialize>(&self, name: &str, value: &T) -> Result<(), SecretError> {
        let raw = serde_json::to_string(value).map_err(|error| SecretError::Backend(error.to_string()))?;
        self.store.set(name, &raw)?;
        if let Err(error) = self.record_backend(self.store.backend()) {
            // The secret is safely stored, which is the part that must not be undone by a
            // marker-write hiccup.
            tracing::warn!(%error, "could not record which backend holds this node's identity");
        }
        Ok(())
    }
}

fn backend_name(backend: Backend) -> &'static str {
    match backend {
        Backend::Keychain => "keychain",
        Backend::File => "file",
    }
}

fn parse_backend(text: &str) -> Option<Backend> {
    match text {
        "keychain" => Some(Backend::Keychain),
        "file" => Some(Backend::File),
        _ => None,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn identity(dir: &std::path::Path) -> Identity {
        Identity::new(crate::secret::SecretStore::with_file_dir("zyris-test", dir.to_path_buf()))
    }

    pub(crate) fn a_credential() -> Credential {
        serde_json::from_value(serde_json::json!({
            "version": 2,
            "secret": "zc_abc",
            "system": {"id": "sys-1", "name": "Laptop", "slug": "laptop"},
            "program": {"id": "cred-1", "name": "zyris", "slug": "zyris"},
            "scopes": ["agents:read"],
            "owner_email": "person@example.com"
        }))
        .expect("Credential's shape changed; update this fixture")
    }

    #[test]
    fn nothing_stored_reads_as_a_clean_slate() {
        let dir = tempfile::tempdir().unwrap();

        assert!(identity(dir.path()).load().unwrap().is_none());
    }

    #[test]
    fn a_saved_credential_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let identity = identity(dir.path());

        identity.save(&a_credential()).unwrap();

        assert_eq!(identity.load().unwrap(), Some(a_credential()));
    }

    #[test]
    fn unreadable_stored_json_reads_as_absent_rather_than_failing() {
        // A truncated write or a format change must not brick the app: the recovery from "I
        // cannot read this" is to enrol again, which requires load() to succeed.
        let dir = tempfile::tempdir().unwrap();
        let identity = identity(dir.path());
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(dir.path().join("credential"), "{ this is not json").unwrap();

        assert!(identity.load().unwrap().is_none());
    }

    /// A machine upgraded from the account layer has its `zna_`/`znt_` pair on disk. Neither is a
    /// credential, so it enrols — and the dead bearers must not outlive that.
    #[test]
    fn what_the_account_layer_stored_is_not_a_credential_and_is_cleared_on_save() {
        let dir = tempfile::tempdir().unwrap();
        let identity = identity(dir.path());
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(
            dir.path().join("account-credential"),
            r#"{"version":1,"access_token":"zna_old","refresh_token":"znr_old"}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("node-token"), r#"{"token":"znt_old"}"#).unwrap();

        assert!(identity.load().unwrap().is_none(), "an old account grant is not a credential");

        identity.save(&a_credential()).unwrap();

        assert!(!dir.path().join("account-credential").exists());
        assert!(!dir.path().join("node-token").exists());
    }

    #[test]
    fn forget_clears_the_credential() {
        let dir = tempfile::tempdir().unwrap();
        let identity = identity(dir.path());
        identity.save(&a_credential()).unwrap();

        identity.forget().unwrap();

        assert!(identity.load().unwrap().is_none());
    }

    #[test]
    fn nothing_saved_yet_is_not_stranded() {
        let dir = tempfile::tempdir().unwrap();

        assert_eq!(identity(dir.path()).stranded_in(), None);
    }

    #[test]
    fn saving_records_the_backend_actually_used() {
        let dir = tempfile::tempdir().unwrap();
        let identity = identity(dir.path());

        identity.save(&a_credential()).unwrap();

        assert_eq!(identity.stranded_in(), None);
        assert_eq!(std::fs::read_to_string(dir.path().join("backend")).unwrap(), "file");
    }

    #[test]
    fn a_marker_naming_a_different_backend_than_this_launch_resolved_reads_as_stranded() {
        // An earlier launch stored the identity in the keychain, and this launch's `SecretStore` —
        // pinned to the file backend here, the way an unavailable keychain would resolve for real
        // — cannot see it.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(dir.path().join("backend"), "keychain").unwrap();

        assert_eq!(identity(dir.path()).stranded_in(), Some(crate::secret::Backend::Keychain));
    }

    #[test]
    fn an_unreadable_marker_is_not_stranded_either() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(dir.path().join("backend"), "not-a-real-backend").unwrap();

        assert_eq!(identity(dir.path()).stranded_in(), None);
    }

    #[test]
    fn forget_clears_the_backend_marker_too() {
        let dir = tempfile::tempdir().unwrap();
        let identity = identity(dir.path());
        identity.save(&a_credential()).unwrap();
        assert!(dir.path().join("backend").exists(), "the marker must exist before it can be cleared");

        identity.forget().unwrap();

        assert!(!dir.path().join("backend").exists());
    }
}
