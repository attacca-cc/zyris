//! How a node proves who it is, as a hook with the common answers already in the box.
//!
//! Every node has to answer one question before each dial — *what bearer do I present?* — and there
//! are only a few real answers: a token from the environment, a token from a mounted file, or a
//! credential the node enrolled for itself. Those three ship here. Anything else (a vault client, a
//! platform keychain, a token minted per-connection) is a [`Credentials`] impl of your own, and the
//! runtime does not care which it got.

use std::sync::Arc;

use async_trait::async_trait;

/// Why a bearer could not be produced, in the three shades the run loop reacts to differently.
#[derive(Debug, thiserror::Error)]
pub enum CredentialsError {
    /// A person has to do something — approve the node, set a variable, fix a permission. The
    /// process exits 2 rather than restart-looping, so a supervisor does not spin forever on a
    /// condition no amount of retrying will change.
    #[error("{0}")]
    NeedsOperator(String),
    /// Wrong in a way retrying will not fix: a revoked credential, a malformed token. Exits 1.
    #[error("{0}")]
    Fatal(String),
    /// The credential *source* could not be reached — a vault that timed out, a file on a mount
    /// that is not up yet. Retried with backoff, because this is usually a startup race.
    #[error("{0}")]
    Unavailable(String),
}

/// The bearer a node presents at the websocket upgrade.
///
/// [`bearer`](Self::bearer) is called immediately before *every* dial rather than once at startup.
/// That is the entire answer to mid-connection expiry: a token valid now is valid for the
/// handshake, and a connection that outlives its token is handled server-side by the heartbeat. It
/// also means an implementation may return a different token each time without telling anyone.
#[async_trait]
pub trait Credentials: Send + Sync + 'static {
    /// The bearer to present right now.
    async fn bearer(&self) -> Result<String, CredentialsError>;

    /// Called once when a dial is refused as unauthorized, before giving up.
    ///
    /// Return `true` if a different credential is now available and the dial is worth repeating.
    /// The default is `false`: a source that hands back the same token every time has nothing to
    /// contribute here, and answering `true` would loop on a credential that will never work.
    async fn refresh(&self) -> Result<bool, CredentialsError> {
        Ok(false)
    }

    /// Where this credential comes from, for the one line a node logs at startup. Never a secret —
    /// a path, a variable name, or a token prefix at most.
    fn describe(&self) -> String;
}

/// What a static node token starts with. Attacca mints these; the prefix is what makes a mispasted
/// credential of some other kind diagnosable instead of a bare 401.
pub const NODE_TOKEN_PREFIX: &str = "znt_";

/// How much of a token is safe to log. The same prefix length the server stores and shows on the
/// node card, so a log line can be matched to a node in the UI without leaking the secret.
pub const TOKEN_DISPLAY_PREFIX: usize = 12;

/// The leading, non-secret part of a token.
pub fn token_prefix(token: &str) -> &str {
    &token[..token.len().min(TOKEN_DISPLAY_PREFIX)]
}

/// Reject anything that is not a node token, by prefix.
///
/// This exists to catch one specific mistake: pasting an `atk_` API key where a `znt_` node token
/// goes. Both are long opaque strings from the same dashboard, and without this check the only
/// symptom is a 401 at the upgrade with nothing to suggest what is actually wrong.
fn validate_node_token(token: &str, source: &str) -> Result<String, CredentialsError> {
    let token = token.trim();
    if token.is_empty() {
        return Err(CredentialsError::NeedsOperator(format!("{source} is empty")));
    }
    if !token.starts_with(NODE_TOKEN_PREFIX) {
        return Err(CredentialsError::Fatal(format!(
            "{source} does not look like a node token (expected a `znt_` prefix). API keys \
             (`atk_`) will not work here."
        )));
    }
    Ok(token.to_string())
}

/// A token held in memory for the life of the process.
pub struct StaticToken {
    token: String,
    source: String,
}

impl StaticToken {
    /// Trusts the caller: no prefix check, for a node that mints its own credentials or talks to
    /// something other than Attacca. Use [`from_env`] if you want the `atk_` diagnostic.
    pub fn new(token: impl Into<String>) -> StaticToken {
        StaticToken { token: token.into(), source: "a static token".to_string() }
    }

    /// Read `$ZYRIS_NODE_TOKEN`, checking that it is actually a node token.
    pub fn from_env() -> Result<Option<StaticToken>, CredentialsError> {
        let Some(raw) = std::env::var("ZYRIS_NODE_TOKEN").ok().filter(|v| !v.trim().is_empty())
        else {
            return Ok(None);
        };
        Ok(Some(StaticToken {
            token: validate_node_token(&raw, "ZYRIS_NODE_TOKEN")?,
            source: "$ZYRIS_NODE_TOKEN".to_string(),
        }))
    }
}

#[async_trait]
impl Credentials for StaticToken {
    async fn bearer(&self) -> Result<String, CredentialsError> {
        Ok(self.token.clone())
    }

    fn describe(&self) -> String {
        format!("{} ({}…)", self.source, token_prefix(&self.token))
    }
}

/// A token read from a file, fresh on every dial.
///
/// This is the shape Kubernetes Secrets and systemd's `LoadCredential=` mount, and the reason to
/// prefer it over an environment variable is that an env var is visible in `/proc`, inherited by
/// every child process, and printed by any crash reporter that dumps the environment.
///
/// Re-reading per dial rather than caching means a rotated Secret is picked up on the next
/// reconnect with no restart — which is the whole point of mounting one.
pub struct TokenFile {
    path: std::path::PathBuf,
}

impl TokenFile {
    pub fn at(path: impl Into<std::path::PathBuf>) -> TokenFile {
        TokenFile { path: path.into() }
    }

    /// Read `$ZYRIS_NODE_TOKEN_FILE`.
    pub fn from_env() -> Option<TokenFile> {
        std::env::var_os("ZYRIS_NODE_TOKEN_FILE")
            .filter(|v| !v.is_empty())
            .map(TokenFile::at)
    }
}

#[async_trait]
impl Credentials for TokenFile {
    async fn bearer(&self) -> Result<String, CredentialsError> {
        let path = self.path.clone();
        let read = tokio::task::spawn_blocking(move || std::fs::read_to_string(&path))
            .await
            .map_err(|e| CredentialsError::Unavailable(e.to_string()))?;

        match read {
            Ok(contents) => {
                validate_node_token(&contents, &format!("the token in {}", self.path.display()))
            }
            // A mount that is not up yet is the common case at pod start, and it resolves itself.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(
                CredentialsError::Unavailable(format!("{} does not exist yet", self.path.display())),
            ),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                Err(CredentialsError::NeedsOperator(format!(
                    "cannot read {}: {e}",
                    self.path.display()
                )))
            }
            Err(e) => Err(CredentialsError::Unavailable(format!(
                "cannot read {}: {e}",
                self.path.display()
            ))),
        }
    }

    fn describe(&self) -> String {
        self.path.display().to_string()
    }
}

/// Self-registration over the device grant: print a code, wait for a human, then keep the issued
/// credential rotating forever.
///
/// Enrollment is deferred to the first [`bearer`](Credentials::bearer) call rather than done at
/// construction, so a node finishes wiring up its capabilities before it blocks on a person.
#[cfg(feature = "enroll")]
pub struct DeviceGrant {
    enroller: crate::enroll::Enroller,
    held: tokio::sync::Mutex<Option<crate::enroll::StoredCredential>>,
}

#[cfg(feature = "enroll")]
impl DeviceGrant {
    /// Clock-skew allowance when deciding whether a stored access token is still worth presenting.
    const SKEW_SECS: i64 = 30;

    pub fn new(enroller: crate::enroll::Enroller) -> DeviceGrant {
        DeviceGrant { enroller, held: tokio::sync::Mutex::new(None) }
    }
}

#[cfg(feature = "enroll")]
#[async_trait]
impl Credentials for DeviceGrant {
    async fn bearer(&self) -> Result<String, CredentialsError> {
        let mut held = self.held.lock().await;
        // `obtain` is the whole startup decision tree: reuse, refresh, or enroll. Calling it
        // whenever the access token is spent means there is no timer task and no second code path.
        if held.as_ref().and_then(|c| c.bearer(now_unix(), Self::SKEW_SECS)).is_none() {
            *held = Some(self.enroller.obtain().await?);
        }
        held.as_ref()
            .and_then(|c| c.bearer(now_unix(), Self::SKEW_SECS))
            .map(str::to_string)
            .ok_or_else(|| {
                CredentialsError::NeedsOperator(
                    "the credential just issued is already expired; check this machine's clock"
                        .to_string(),
                )
            })
    }

    async fn refresh(&self) -> Result<bool, CredentialsError> {
        let mut held = self.held.lock().await;
        let Some(current) = held.as_ref() else { return Ok(false) };
        *held = Some(self.enroller.force_refresh(current).await?);
        Ok(true)
    }

    fn describe(&self) -> String {
        format!("device enrollment ({})", self.enroller.store_description())
    }
}

#[cfg(feature = "enroll")]
impl From<crate::enroll::EnrollError> for CredentialsError {
    fn from(error: crate::enroll::EnrollError) -> CredentialsError {
        use crate::enroll::EnrollError;
        match error {
            // A credential the store refuses is an operator problem by construction — that is what
            // the `Refused` shade of a store error means.
            e @ (EnrollError::NeedsOperator(_) | EnrollError::Store(_)) => {
                CredentialsError::NeedsOperator(e.to_string())
            }
            e @ EnrollError::Rejected(_) => CredentialsError::Fatal(e.to_string()),
            e @ EnrollError::Transport(_) => CredentialsError::Unavailable(e.to_string()),
        }
    }
}

#[cfg(feature = "enroll")]
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Pick a credential source from the environment, most explicit first.
///
/// `$ZYRIS_NODE_TOKEN` beats `$ZYRIS_NODE_TOKEN_FILE` beats enrolling. Enrolling is last because it
/// is the only option that can block on a human: anything configured explicitly should win without
/// a code ever being printed.
///
/// A token that is set but malformed is a hard error rather than a fall-through to enrollment. That
/// check exists to catch a mispasted `atk_` API key, and quietly enrolling past it would destroy
/// the only diagnostic the user gets.
pub fn from_env(config: &crate::runtime::RunConfig) -> Result<Arc<dyn Credentials>, CredentialsError> {
    if let Some(token) = StaticToken::from_env()? {
        return Ok(Arc::new(token));
    }
    if let Some(file) = TokenFile::from_env() {
        return Ok(Arc::new(file));
    }

    #[cfg(all(feature = "enroll", feature = "persistence"))]
    {
        let enroller = crate::enroll::Enroller::with_file_store(
            &config.url,
            &config.profile,
            config.node_name.clone(),
            config.platform().to_string(),
            config.scopes.clone(),
        )?;
        Ok(Arc::new(DeviceGrant::new(enroller)))
    }
    // Enrollment with nowhere to keep the result would mean printing a fresh code on every restart
    // and leaving a dead node row behind each time. Refusing is the honest answer; the caller can
    // still build a `DeviceGrant` over a `CredentialStore` of their own.
    #[cfg(all(feature = "enroll", not(feature = "persistence")))]
    {
        let _ = config;
        Err(CredentialsError::NeedsOperator(
            "no credential configured, and this build has nowhere to keep an enrolled one: set \
             ZYRIS_NODE_TOKEN or ZYRIS_NODE_TOKEN_FILE, enable zyris's `persistence` feature, or \
             construct a DeviceGrant over your own CredentialStore"
                .to_string(),
        ))
    }
    #[cfg(not(feature = "enroll"))]
    {
        let _ = config;
        Err(CredentialsError::NeedsOperator(
            "no credential configured: set ZYRIS_NODE_TOKEN or ZYRIS_NODE_TOKEN_FILE, or build \
             this node with zyris's `enroll` feature so it can register itself"
                .to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_static_token_is_returned_verbatim_and_never_logged_whole() {
        let creds = StaticToken::new("znt_abcdefghijklmnop");
        assert_eq!(creds.bearer().await.unwrap(), "znt_abcdefghijklmnop");
        assert!(!creds.describe().contains("ijklmnop"), "{}", creds.describe());
        assert!(!creds.refresh().await.unwrap(), "a fixed token has nothing to refresh to");
    }

    /// The one mistake worth a bespoke message: an `atk_` API key pasted where a node token goes.
    #[test]
    fn an_api_key_is_named_rather_than_left_to_401() {
        let error = validate_node_token("atk_looks_about_right", "ZYRIS_NODE_TOKEN").unwrap_err();
        assert!(matches!(error, CredentialsError::Fatal(_)));
        assert!(error.to_string().contains("atk_"), "{error}");
        assert!(validate_node_token("znt_fine", "x").is_ok());
    }

    #[tokio::test]
    async fn a_token_file_is_read_fresh_every_time_so_rotation_needs_no_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        let creds = TokenFile::at(&path);

        // A mount that is not up yet is retriable, not fatal: it is the normal pod-start race.
        assert!(matches!(creds.bearer().await, Err(CredentialsError::Unavailable(_))));

        std::fs::write(&path, "znt_first\n").unwrap();
        assert_eq!(creds.bearer().await.unwrap(), "znt_first", "trailing newline is trimmed");

        std::fs::write(&path, "znt_rotated").unwrap();
        assert_eq!(creds.bearer().await.unwrap(), "znt_rotated");
    }

    #[tokio::test]
    async fn a_token_file_holding_an_api_key_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        std::fs::write(&path, "atk_wrong_kind").unwrap();
        let error = TokenFile::at(&path).bearer().await.unwrap_err();
        assert!(matches!(error, CredentialsError::Fatal(_)), "{error}");
    }
}
