//! The dial/reconnect loop every node runs, so no node has to write it.
//!
//! Nothing here is clever, and that is the point: backoff with jitter, a healthy connection resets
//! it, one forced credential rotation on a 401, graceful shutdown on `Ctrl-C`, and exit codes a
//! supervisor can act on. Each of those is a small decision that is easy to get subtly wrong once
//! and then carry forever — a node pinned at the backoff ceiling after a nightly server restart, a
//! restart loop printing enrollment codes into a log nobody reads.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;

use crate::capabilities::{Capabilities, CapabilitySet};
use crate::error::WireError;
use crate::runtime::credentials::{token_prefix, Credentials, CredentialsError};
use crate::{Connection, Node, NodeKind, ServeCapability};

/// A connection that stayed up this long counts as healthy, so its eventual drop restarts the
/// backoff from the bottom. Without this a nightly server restart would leave every node pinned at
/// the ceiling forever.
const DEFAULT_STABLE_AFTER: Duration = Duration::from_secs(30);
const DEFAULT_BACKOFF_MIN: Duration = Duration::from_secs(1);
const DEFAULT_BACKOFF_MAX: Duration = Duration::from_secs(30);
/// Long enough for a closing frame to reach the server, so it retires the node promptly instead of
/// waiting for the heartbeat to lapse.
const CLOSE_GRACE: Duration = Duration::from_millis(200);

/// Everything a node needs to know before it dials.
#[derive(Debug, Clone)]
pub struct RunConfig {
    /// The websocket URL. The enrollment endpoints are derived from it, so this is the only address
    /// a node is configured with.
    pub url: String,
    pub node_name: String,
    pub kind: NodeKind,
    /// Names the credential file, so one machine can hold separate identities against the same
    /// deployment without them clobbering each other.
    pub profile: String,
    /// What this node *asks* for at enrollment. The user narrows it at approval time and may grant
    /// none of it, which is the common case for a tool-only node.
    pub scopes: Vec<String>,
    /// Set when `$ZYRIS_SCOPES` was present. An operator who said what a node may ask for outranks
    /// the node's own compiled-in default, so [`Runner::request_scopes`] steps aside.
    scopes_pinned: bool,
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    pub stable_after: Duration,
}

impl Default for RunConfig {
    fn default() -> RunConfig {
        RunConfig {
            url: crate::DEFAULT_SERVER_URL.to_string(),
            #[cfg(feature = "hostname")]
            node_name: crate::machine_name().unwrap_or_else(|| "zyris-node".to_string()),
            #[cfg(not(feature = "hostname"))]
            node_name: "zyris-node".to_string(),
            kind: NodeKind::Service,
            profile: "default".to_string(),
            // Empty on purpose. A node that announces tools needs no access to its owner's account,
            // and a library default that asks for some would have every node asking by accident.
            scopes: Vec::new(),
            scopes_pinned: false,
            backoff_min: DEFAULT_BACKOFF_MIN,
            backoff_max: DEFAULT_BACKOFF_MAX,
            stable_after: DEFAULT_STABLE_AFTER,
        }
    }
}

impl RunConfig {
    /// Read the `ZYRIS_*` variables, falling back to [`RunConfig::default`] for each.
    ///
    /// | Variable | Falls back to |
    /// |---|---|
    /// | `ZYRIS_SERVER_URL` | [`crate::DEFAULT_SERVER_URL`] |
    /// | `ZYRIS_NODE_NAME` | this machine's hostname |
    /// | `ZYRIS_PROFILE` | `default` |
    /// | `ZYRIS_SCOPES` | whatever the node asks for in code |
    pub fn from_env() -> RunConfig {
        let default = RunConfig::default();
        let scopes = std::env::var("ZYRIS_SCOPES").ok().map(|raw| {
            raw.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
        });
        RunConfig {
            url: env_or("ZYRIS_SERVER_URL", default.url),
            node_name: env_or("ZYRIS_NODE_NAME", default.node_name),
            profile: env_or("ZYRIS_PROFILE", default.profile),
            scopes_pinned: scopes.is_some(),
            scopes: scopes.unwrap_or(default.scopes),
            ..default
        }
    }

    /// What this machine reports itself as at enrollment. A hint the server never verifies.
    pub fn platform(&self) -> &'static str {
        match std::env::consts::OS {
            "linux" => "linux",
            "macos" => "macos",
            "windows" => "windows",
            _ => "other",
        }
    }
}

fn env_or(key: &str, fallback: String) -> String {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty()).unwrap_or(fallback)
}

/// A node could not be started, or could not keep running.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("{0}")]
    Credentials(#[from] CredentialsError),
    #[error("could not build the node: {0}")]
    Build(#[from] crate::Error),
    /// The server refused this node in a way retrying will not fix — a revoked credential, an
    /// unsupported protocol version.
    #[error("the server refused this node: {0}")]
    Refused(String),
}

impl RunError {
    /// Exit 2 when a human has to do something, 1 otherwise.
    ///
    /// The distinction is what stops a supervisor from restart-looping on a condition no restart
    /// can fix, printing a fresh enrollment code into a log nobody reads each time around.
    pub fn exit_code(&self) -> ExitCode {
        match self {
            RunError::Credentials(CredentialsError::NeedsOperator(_)) => ExitCode::from(2),
            RunError::Build(_) => ExitCode::from(2),
            _ => ExitCode::from(1),
        }
    }
}

type ConnectHook = Arc<dyn Fn(Connection) -> BoxFuture<'static, ()> + Send + Sync>;

/// Where the bearer will come from.
///
/// `FromEnv` is resolved at [`Runner::run`] rather than at construction, and that ordering is
/// load-bearing: a device grant captures the scopes it will ask for when the `Enroller` is built,
/// so resolving eagerly would freeze them before [`Runner::request_scopes`] had a chance to speak.
enum CredentialSource {
    Given(Arc<dyn Credentials>),
    FromEnv,
}

/// A node, its credentials, and the loop that keeps them connected.
///
/// ```no_run
/// # use std::process::ExitCode;
/// # use zyris::{NodeKind, runtime::Runner};
/// # async fn example() -> ExitCode {
/// Runner::from_env().kind(NodeKind::Service).run().await
/// # }
/// ```
pub struct Runner {
    config: RunConfig,
    credentials: CredentialSource,
    capabilities: Arc<CapabilitySet>,
    /// A duplicate handed to the builder. Reported by `try_run` rather than at the call that made
    /// it, because the builder methods return `Self` so a node can be described in one expression.
    capability_error: Option<WireError>,
    on_connect: Option<ConnectHook>,
}

impl Runner {
    /// Configure everything from the environment: address, name, profile, and whichever credential
    /// source is set. See [`RunConfig::from_env`] and [`credentials::from_env`].
    ///
    /// Infallible, and nothing has been contacted yet — a malformed token or a missing credential
    /// is reported by [`run`](Self::run), along with everything else that can go wrong. Enrollment
    /// in particular waits until the first dial, so a node finishes wiring up its capabilities
    /// before it blocks on a person reading a code.
    ///
    /// [`credentials::from_env`]: crate::runtime::credentials::from_env
    pub fn from_env() -> Runner {
        Runner {
            config: RunConfig::from_env(),
            credentials: CredentialSource::FromEnv,
            capabilities: CapabilitySet::new(),
            capability_error: None,
            on_connect: None,
        }
    }

    /// A node with a credential source of your own — anything implementing [`Credentials`].
    pub fn new(config: RunConfig, credentials: Arc<dyn Credentials>) -> Runner {
        Runner {
            config,
            credentials: CredentialSource::Given(credentials),
            capabilities: CapabilitySet::new(),
            capability_error: None,
            on_connect: None,
        }
    }

    /// The name this node will announce, available before `run` so a capability can be built with
    /// it — a node that reports which machine answered needs to know that first.
    pub fn node_name(&self) -> &str {
        &self.config.node_name
    }

    pub fn config(&self) -> &RunConfig {
        &self.config
    }

    pub fn kind(mut self, kind: NodeKind) -> Self {
        self.config.kind = kind;
        self
    }

    pub fn capability(self, capability: impl ServeCapability) -> Self {
        self.capability_arc(Arc::new(capability))
    }

    pub fn capability_arc(mut self, capability: Arc<dyn ServeCapability>) -> Self {
        if let Err(e) = self.capabilities.push(capability) {
            self.capability_error.get_or_insert(e);
        }
        self
    }

    /// What this node offers, available before [`run`](Self::run) so an application can hold onto
    /// it and change the set while the node is connected. See [`Capabilities`].
    pub fn capabilities(&self) -> Capabilities {
        Capabilities::new(self.capabilities.clone())
    }

    /// What this node asks for at enrollment, unless `$ZYRIS_SCOPES` already said.
    ///
    /// The operator's answer wins: someone who set the variable is deciding what this node may ask
    /// for, and a compiled-in default silently overriding that would make the variable a lie.
    pub fn request_scopes<S: Into<String>>(mut self, scopes: impl IntoIterator<Item = S>) -> Self {
        if !self.config.scopes_pinned {
            self.config.scopes = scopes.into_iter().map(Into::into).collect();
        }
        self
    }

    /// Run once per established connection, concurrently with the connection itself.
    ///
    /// This is the *consume* half of a node: the server announces its own capabilities on the same
    /// websocket, so a node is not only a tool provider. The hook is spawned and its outcome is
    /// ignored — a node whose token lacks a scope should still serve the tools it announced.
    pub fn on_connect<F, Fut>(mut self, hook: F) -> Self
    where
        F: Fn(Connection) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        self.on_connect = Some(Arc::new(move |conn| Box::pin(hook(conn))));
        self
    }

    /// Connect, stay connected, and translate whatever happens into an exit code.
    ///
    /// Returns only when the node is shut down deliberately (`Ctrl-C`, success) or hits something
    /// no retry can fix. Every failure along the way is logged as it happens, so the caller has
    /// nothing left to report.
    pub async fn run(self) -> ExitCode {
        match self.try_run().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                tracing::error!(%error, "zyris node stopped");
                error.exit_code()
            }
        }
    }

    /// [`run`](Self::run) without the exit-code translation, for a caller that has its own idea of
    /// what to do when a node gives up.
    pub async fn try_run(mut self) -> Result<(), RunError> {
        if let Some(error) = self.capability_error.take() {
            return Err(error.into());
        }
        // Built once, outside the loop. A `Node` is reusable across connections, and the
        // capability impls are owned by it, so rebuilding per attempt would hand every reconnect a
        // freshly initialised capability that had forgotten whatever the last one knew. The set is
        // shared rather than copied, so a capability dropped while connected stays dropped across
        // the reconnect too.
        let node = Node::with_capabilities(
            self.config.node_name.clone(),
            self.config.kind.clone(),
            self.capabilities.clone(),
        );

        // Resolved here rather than at construction, so the scopes a device grant will ask for are
        // whatever the builder finally settled on.
        let credentials = match self.credentials {
            CredentialSource::Given(ref credentials) => credentials.clone(),
            CredentialSource::FromEnv => crate::runtime::credentials::from_env(&self.config)?,
        };

        tracing::info!(
            node = %self.config.node_name,
            url = %self.config.url,
            credentials = %credentials.describe(),
            scopes = ?self.config.scopes,
            capabilities = self.capabilities.len(),
            "starting zyris node"
        );

        let mut backoff = self.config.backoff_min;
        // Tracks whether a refusal has already been answered with a forced rotation, so a genuinely
        // dead credential still terminates instead of refreshing forever.
        let mut rotated_after_refusal = false;

        loop {
            // Freshness is decided immediately before each dial rather than by a timer task: a
            // token that is valid now is valid for the handshake, and a connection that outlives
            // its token is handled server-side by the heartbeat.
            let bearer = match credentials.bearer().await {
                Ok(bearer) => bearer,
                // The credential *source* is unreachable — a Secret not mounted yet, a vault
                // restarting. That is a startup race, not a dead node, so it backs off like any
                // other transient failure instead of killing the process.
                Err(e @ CredentialsError::Unavailable(_)) => {
                    tracing::warn!(error = %e, "credential source unavailable");
                    backoff = self.wait_then_widen(backoff).await;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };

            tracing::info!(token = %token_prefix(&bearer), "connecting");
            match node.connect(&self.config.url, &bearer).await {
                // A refusal here can be a slept laptop or a clock that drifted — recoverable, but
                // the transport marks 401 non-retriable, so without one forced rotation a node
                // would exit permanently on a condition it could fix itself.
                Err(e) if !e.retriable && !rotated_after_refusal => {
                    tracing::warn!(error = %e, "credential refused; rotating once before giving up");
                    rotated_after_refusal = true;
                    match credentials.refresh().await {
                        Ok(true) => continue,
                        Ok(false) => return Err(RunError::Refused(e.to_string())),
                        Err(refresh_error) => return Err(refresh_error.into()),
                    }
                }
                Err(e) if !e.retriable => return Err(RunError::Refused(e.to_string())),
                Err(e) => tracing::warn!(error = %e, "connect failed"),
                Ok(conn) => {
                    rotated_after_refusal = false;
                    let up = Instant::now();
                    tracing::info!(
                        node_id = %conn.info().node_id,
                        conn_id = %conn.info().conn_id,
                        "connected"
                    );
                    if let Some(hook) = &self.on_connect {
                        tokio::spawn(hook(conn.clone()));
                    }
                    if self.serve_until_closed(&conn).await {
                        return Ok(());
                    }
                    if up.elapsed() >= self.config.stable_after {
                        backoff = self.config.backoff_min;
                    }
                }
            }

            backoff = self.wait_then_widen(backoff).await;
        }
    }

    /// Hold a live connection until it drops or the operator interrupts. `true` means shut down.
    async fn serve_until_closed(&self, conn: &Connection) -> bool {
        tokio::select! {
            reason = conn.closed() => {
                tracing::warn!(%reason, "disconnected");
                false
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("shutting down");
                conn.close("node shutting down");
                tokio::time::sleep(CLOSE_GRACE).await;
                true
            }
        }
    }

    async fn wait_then_widen(&self, backoff: Duration) -> Duration {
        let wait = jitter(backoff);
        tracing::info!(seconds = wait.as_secs_f64(), "reconnecting");
        tokio::time::sleep(wait).await;
        (backoff * 2).min(self.config.backoff_max)
    }
}

/// ±20%, so a server restart does not bring every node back in the same instant.
///
/// `RandomState` rather than an `rand` dependency: a fresh one hashes differently on every call,
/// which is the entire requirement here. Backoff jitter does not need a real RNG, and adding one to
/// the default feature set would make every node pay for it.
fn jitter(base: Duration) -> Duration {
    use std::hash::{BuildHasher, Hasher, RandomState};
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u8(0);
    let factor = 0.8 + (hasher.finish() % 400) as f64 / 1000.0;
    base.mul_f64(factor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_stays_within_twenty_percent_and_actually_varies() {
        let base = Duration::from_secs(10);
        let samples: Vec<Duration> = (0..64).map(|_| jitter(base)).collect();
        for sample in &samples {
            assert!(*sample >= base.mul_f64(0.8) && *sample <= base.mul_f64(1.2), "{sample:?}");
        }
        assert!(
            samples.iter().any(|s| *s != samples[0]),
            "every node backing off by the same amount is the thundering herd this exists to avoid"
        );
    }

    #[test]
    fn defaults_point_at_the_default_deployment() {
        let config = RunConfig::default();
        assert_eq!(config.url, crate::DEFAULT_SERVER_URL);
        assert!(config.scopes.is_empty(), "a library default must not ask for account access");
        assert!(!config.node_name.is_empty());
    }

    /// An operator who set `$ZYRIS_SCOPES` is deciding what this node may ask for; a compiled-in
    /// default silently overriding that would make the variable a lie.
    fn runner(config: RunConfig) -> Runner {
        Runner::new(config, Arc::new(crate::runtime::StaticToken::new("znt_x")))
    }

    /// An operator who set `$ZYRIS_SCOPES` is deciding what this node may ask for; a compiled-in
    /// default silently overriding that would make the variable a lie.
    #[test]
    fn an_explicit_scope_setting_outranks_the_nodes_own_default() {
        let pinned = RunConfig { scopes_pinned: true, ..RunConfig::default() };
        assert!(runner(pinned).request_scopes(["agents:read"]).config().scopes.is_empty());

        let asked = runner(RunConfig::default()).request_scopes(["agents:read"]);
        assert_eq!(asked.config().scopes, vec!["agents:read".to_string()]);
    }

    /// The ordering bug this exists to prevent: a device grant captures its scopes when the
    /// `Enroller` is built, so resolving credentials at `from_env` time would freeze them before
    /// `request_scopes` ran and the node would enroll asking for nothing.
    #[test]
    fn credentials_stay_unresolved_until_the_builder_is_done() {
        let runner = Runner::from_env().kind(NodeKind::Cli);
        assert!(matches!(runner.credentials, CredentialSource::FromEnv));
        assert_eq!(runner.config().kind, NodeKind::Cli);
    }
}
