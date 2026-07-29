//! Where an enrolled node keeps its credentials between runs.
//!
//! The device grant hands a node a refresh token that outlives the process, so *something* has to
//! hold it. What that something is varies more than the enrollment flow does — a laptop wants a file
//! under `$HOME`, a pod wants a k8s Secret, a desktop app wants the OS keychain, and a test wants
//! nothing at all. That is a trait, not a path, which is why [`CredentialStore`] is the seam and
//! [`FileCredentialStore`](crate::enroll::FileCredentialStore) is merely the default behind it.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Bumped only on an incompatible change. A file from the future is refused rather than guessed
/// at, because guessing wrong here means a node that authenticates as something unintended.
pub(crate) const CREDENTIAL_VERSION: u32 = 1;

/// What went wrong reaching a credential, in the only two shades the caller can act on.
///
/// The distinction is load-bearing: on startup a node *discards* a credential it cannot use and
/// enrolls again, but a credential it must not use is an operator's problem and has to be said out
/// loud. Collapsing the two would turn a leaked secret into a silent re-enrollment.
#[derive(Debug, thiserror::Error)]
pub enum CredentialStoreError {
    /// The credential may exist, but using it would be wrong and someone needs to know — a
    /// world-readable key file, a config directory that cannot be located. Never discarded silently.
    #[error("{0}")]
    Refused(String),
    /// Unreadable, corrupt, or written by a version this build does not understand. Safe to throw
    /// away and enroll again.
    #[error("{0}")]
    Unusable(String),
    /// Anything a third-party backend wants to report. Treated like [`Refused`](Self::Refused) —
    /// a store that fails in a way this crate cannot classify does not get its contents deleted.
    #[error(transparent)]
    Other(Box<dyn std::error::Error + Send + Sync>),
}

impl CredentialStoreError {
    pub fn other(error: impl std::error::Error + Send + Sync + 'static) -> CredentialStoreError {
        CredentialStoreError::Other(Box::new(error))
    }

    /// Whether the caller may respond by clearing the credential and starting over.
    pub fn is_discardable(&self) -> bool {
        matches!(self, CredentialStoreError::Unusable(_))
    }
}

/// A node's long-lived identity: the refresh token it re-presents forever, and the access token it
/// presents at each dial.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredCredential {
    pub version: u32,
    pub access_token: String,
    pub refresh_token: String,
    pub node_id: String,
    #[serde(default)]
    pub node_name: String,
    #[serde(default)]
    pub owner_email: String,
    /// Unix seconds. Stored as an absolute instant rather than the `expires_in` the server sent,
    /// because the value has to survive the process that received it.
    pub access_expires_at: i64,
}

impl StoredCredential {
    pub fn new(
        access_token: String,
        refresh_token: String,
        node_id: String,
        node_name: String,
        owner_email: String,
        access_expires_at: i64,
    ) -> StoredCredential {
        StoredCredential {
            version: CREDENTIAL_VERSION,
            access_token,
            refresh_token,
            node_id,
            node_name,
            owner_email,
            access_expires_at,
        }
    }

    /// The bearer to present at the websocket upgrade, or `None` when the access token is spent.
    ///
    /// Calling this immediately before each dial is the entire answer to mid-connection expiry —
    /// no timer task, no proactive refresh loop. `skew_secs` is subtracted so a token that would
    /// expire during the handshake is refreshed instead of being raced.
    pub fn bearer(&self, now_unix: i64, skew_secs: i64) -> Option<&str> {
        (self.access_expires_at - skew_secs > now_unix).then_some(self.access_token.as_str())
    }

    /// Whether it is worth refreshing ahead of time. At 80% of a one-hour lifetime this fires with
    /// twelve minutes to spare, which is enough to absorb a transient failure and retry.
    pub fn should_refresh(&self, now_unix: i64, lifetime_secs: i64) -> bool {
        let refresh_at = self.access_expires_at - (lifetime_secs / 5);
        now_unix >= refresh_at
    }
}

/// Where a node's credentials live between runs.
///
/// Async because the interesting backends are: a keychain prompts, a secret manager is a network
/// call. The default file backend does blocking IO inside these methods, which is what shipped
/// before the trait existed and is bounded by one small read or write per process lifetime.
#[async_trait]
pub trait CredentialStore: Send + Sync + 'static {
    /// The stored credential, or `None` when this node has never enrolled.
    async fn load(&self) -> Result<Option<StoredCredential>, CredentialStoreError>;
    /// Write, replacing whatever was there. Callers persist *before* using a credential, so a
    /// backend that can be atomic should be.
    async fn save(&self, credential: &StoredCredential) -> Result<(), CredentialStoreError>;
    /// Forget a credential the server will never honour again, so the next start enrolls cleanly
    /// instead of looping on it. Clearing nothing is success, not an error.
    async fn clear(&self) -> Result<(), CredentialStoreError>;
    /// Where this backend keeps things, for the one debug line a node logs at startup. Must never
    /// contain a secret — this is a path or a URL, not a token.
    fn describe(&self) -> String;
}

/// Keeps a credential for exactly as long as the process lives.
///
/// Always compiled, so `enroll` without `persistence` still has something to hand an [`Enroller`],
/// and so a test can exercise the whole flow without touching a filesystem. A node using this
/// re-enrolls on every restart, which is a real choice for a short-lived worker and a mistake for
/// anything else.
///
/// [`Enroller`]: crate::enroll::Enroller
#[derive(Debug, Default)]
pub struct MemoryCredentialStore {
    held: std::sync::Mutex<Option<StoredCredential>>,
}

impl MemoryCredentialStore {
    pub fn new() -> MemoryCredentialStore {
        MemoryCredentialStore::default()
    }
}

#[async_trait]
impl CredentialStore for MemoryCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredential>, CredentialStoreError> {
        Ok(self.held.lock().expect("credential mutex poisoned").clone())
    }

    async fn save(&self, credential: &StoredCredential) -> Result<(), CredentialStoreError> {
        *self.held.lock().expect("credential mutex poisoned") = Some(credential.clone());
        Ok(())
    }

    async fn clear(&self) -> Result<(), CredentialStoreError> {
        *self.held.lock().expect("credential mutex poisoned") = None;
        Ok(())
    }

    fn describe(&self) -> String {
        "memory (this credential is lost on restart)".to_string()
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
    async fn memory_store_round_trips() {
        let store = MemoryCredentialStore::new();
        assert_eq!(store.load().await.unwrap(), None);
        store.save(&credential()).await.unwrap();
        assert_eq!(store.load().await.unwrap().unwrap(), credential());
        store.clear().await.unwrap();
        assert_eq!(store.load().await.unwrap(), None);
        // Clearing nothing is success, so a node that never enrolled can still take the
        // credential-was-rejected branch without dying on the cleanup.
        store.clear().await.unwrap();
    }

    /// The one classification the startup path branches on.
    #[test]
    fn only_unusable_may_be_thrown_away() {
        assert!(CredentialStoreError::Unusable("corrupt".into()).is_discardable());
        assert!(!CredentialStoreError::Refused("mode 0644".into()).is_discardable());
        assert!(!CredentialStoreError::other(std::io::Error::other("keychain locked"))
            .is_discardable());
    }

    /// `bearer` immediately before each dial is the whole mid-connection-expiry story, so the
    /// skew boundary is the thing worth pinning.
    #[test]
    fn bearer_expires_early_by_the_skew_allowance() {
        let credential = credential();
        assert_eq!(credential.bearer(0, 30), Some("zna_access"));
        assert_eq!(credential.bearer(999_969, 30), Some("zna_access"));
        // 30 seconds out with a 30-second allowance: refuse, rather than race the handshake.
        assert_eq!(credential.bearer(999_970, 30), None);
        assert_eq!(credential.bearer(2_000_000, 30), None);
    }

    #[test]
    fn refresh_fires_at_eighty_percent_of_the_lifetime() {
        let credential = credential();
        // A one-hour token expiring at 1_000_000 was issued at 996_400; 80% of the way is 999_280.
        assert!(!credential.should_refresh(999_279, 3600));
        assert!(credential.should_refresh(999_280, 3600));
    }
}
