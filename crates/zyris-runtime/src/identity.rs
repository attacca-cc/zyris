//! The two secrets this app keeps between runs, and the rule for reading them back.
//!
//! They are stored apart rather than as one blob because they are written at different moments:
//! the credential rotates on its own schedule, and rewriting the node token every time it did
//! would risk losing a token that never changes.

use zyris::{AccountCredential, NodeToken};

use crate::secret::{SecretError, SecretStore};

/// The names the two secrets live under. Changing one strands whatever is already stored, so
/// they are constants rather than literals scattered through the file.
const CREDENTIAL: &str = "account-credential";
const NODE_TOKEN: &str = "node-token";

/// What was found on disk. Either may be absent, and the combinations mean different things —
/// see `connection.rs`, which is the only place that decides what to do about them.
#[derive(Debug, Default)]
pub struct Stored {
    pub credential: Option<AccountCredential>,
    pub node_token: Option<NodeToken>,
}

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
    pub fn load(&self) -> Result<Stored, SecretError> {
        Ok(Stored {
            credential: self.read(CREDENTIAL)?,
            node_token: self.read(NODE_TOKEN)?,
        })
    }

    pub fn save_credential(&self, credential: &AccountCredential) -> Result<(), SecretError> {
        self.write(CREDENTIAL, credential)
    }

    pub fn save_node_token(&self, token: &NodeToken) -> Result<(), SecretError> {
        self.write(NODE_TOKEN, token)
    }

    pub fn forget(&self) -> Result<(), SecretError> {
        self.store.delete(CREDENTIAL)?;
        self.store.delete(NODE_TOKEN)
    }

    /// Discards only the node token, keeping the credential beside it.
    ///
    /// What recovering from a dead token needs: the node itself can be gone from Attacca while
    /// the account that minted it is still good, and re-minting a replacement only needs that
    /// credential to still be on disk.
    pub fn forget_node_token(&self) -> Result<(), SecretError> {
        self.store.delete(NODE_TOKEN)
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
        self.store.set(name, &raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(dir: &std::path::Path) -> Identity {
        Identity::new(crate::secret::SecretStore::with_file_dir("zyris-test", dir.to_path_buf()))
    }

    fn a_token() -> NodeToken {
        serde_json::from_str(r#"{"node_id":"n_1","slug":"laptop","token":"znt_abc"}"#)
            .expect("NodeToken's shape changed; update this fixture")
    }

    fn a_credential() -> AccountCredential {
        AccountCredential::new(
            "at_abc".to_string(),
            "rt_abc".to_string(),
            "n_1".to_string(),
            "laptop".to_string(),
            "person@example.com".to_string(),
            4_102_444_800,
        )
    }

    #[test]
    fn nothing_stored_reads_as_a_clean_slate() {
        let dir = tempfile::tempdir().unwrap();

        let stored = identity(dir.path()).load().unwrap();

        assert!(stored.credential.is_none());
        assert!(stored.node_token.is_none());
    }

    #[test]
    fn a_saved_node_token_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let identity = identity(dir.path());

        identity.save_node_token(&a_token()).unwrap();

        let stored = identity.load().unwrap();
        assert_eq!(stored.node_token.unwrap().as_str(), "znt_abc");
    }

    #[test]
    fn a_node_token_without_a_credential_still_loads() {
        // This is the state after a credential is revoked but the node token is not. The node
        // can still dial with the token it has, so refusing to load it would strand a node that
        // is in fact still able to work.
        let dir = tempfile::tempdir().unwrap();
        let identity = identity(dir.path());

        identity.save_node_token(&a_token()).unwrap();

        let stored = identity.load().unwrap();
        assert!(stored.node_token.is_some());
        assert!(stored.credential.is_none());
    }

    #[test]
    fn a_saved_credential_reads_back() {
        // The credential is the secret that *rotates*: `on_rotate` in `connection.rs` saves a
        // fresh one on every refresh, and a failed save there revokes the node. It deserves the
        // same round-trip coverage the node token already has.
        let dir = tempfile::tempdir().unwrap();
        let identity = identity(dir.path());

        identity.save_credential(&a_credential()).unwrap();

        let stored = identity.load().unwrap();
        assert_eq!(stored.credential, Some(a_credential()));
    }

    #[test]
    fn unreadable_stored_credential_json_reads_as_absent_rather_than_failing() {
        // Mirrors `unreadable_stored_json_reads_as_absent_rather_than_failing` below, but for the
        // credential file rather than the node token — the same corruption can happen to either.
        let dir = tempfile::tempdir().unwrap();
        let identity = identity(dir.path());
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(dir.path().join("account-credential"), "{ this is not json").unwrap();

        let stored = identity.load().unwrap();

        assert!(stored.credential.is_none());
    }

    #[test]
    fn unreadable_stored_json_reads_as_absent_rather_than_failing() {
        // A truncated write or a format change must not brick the app: the recovery from "I
        // cannot read this" is to enrol again, which requires load() to succeed.
        let dir = tempfile::tempdir().unwrap();
        let identity = identity(dir.path());
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(dir.path().join("node-token"), "{ this is not json").unwrap();

        let stored = identity.load().unwrap();

        assert!(stored.node_token.is_none());
    }

    #[test]
    fn forget_clears_both() {
        let dir = tempfile::tempdir().unwrap();
        let identity = identity(dir.path());
        identity.save_node_token(&a_token()).unwrap();

        identity.forget().unwrap();

        assert!(identity.load().unwrap().node_token.is_none());
    }

    #[test]
    fn forget_node_token_discards_only_the_token() {
        // The whole point of having this apart from `forget`: recovering from a node Attacca
        // refused must not also throw away a credential that is still good, or every recovery
        // would send someone to a browser instead of quietly minting a replacement node.
        let dir = tempfile::tempdir().unwrap();
        let identity = identity(dir.path());
        identity.save_node_token(&a_token()).unwrap();
        identity.save_credential(&a_credential()).unwrap();

        identity.forget_node_token().unwrap();

        let stored = identity.load().unwrap();
        assert!(stored.node_token.is_none(), "the dead token must be gone");
        assert_eq!(
            stored.credential,
            Some(a_credential()),
            "the credential must survive discarding the token"
        );
    }
}
