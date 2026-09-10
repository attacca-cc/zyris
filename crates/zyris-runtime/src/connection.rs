//! Everything between "this process started" and "this node is connected".
//!
//! The library reconnects by itself — `Node::connect` returns a `Link` that backs off and dials
//! again — so nothing here retries. This actor establishes the identity, hands it to the link,
//! and reports what it observes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use zyris::enroll::{EnrollRequest, Progress};
use zyris::{Account, AccountCredential, Node, NodeKind, NodeSpec, RegisterError, RotateError};

use crate::event::{CoreEvent, EventBus};
use crate::identity::Identity;

/// What the account grant asks for.
///
/// It must be a superset of [`NODE_SCOPES`] plus `nodes:write`. The server refuses to mint a
/// node that asks for more than its account holds — `RegisterError::ScopeExceeded`, whose own
/// documentation says the request is clamped to what the grant covers — so an account narrower
/// than the node it is minting cannot mint it at all. `nodes:write` is what lets it mint one.
pub const ACCOUNT_SCOPES: &[&str] = &[
    "agents:read",
    "sessions:read",
    "sessions:write",
    "events:read",
    "peers:write",
    "nodes:write",
];

/// What the node token carries, which is deliberately less. A static token must never be able to
/// mint another one, so `nodes:write` stops at the account layer.
pub const NODE_SCOPES: &[&str] =
    &["agents:read", "sessions:read", "sessions:write", "events:read", "peers:write"];

pub struct Connector {
    identity: Identity,
    bus: EventBus,
    server: String,
    /// Whether a connection has come up at least once during this `run()`. `dial`'s `on_connect`
    /// hook sets this, before publishing `Connected`; `report_setup_failure` reads it to decide
    /// whether a failure belongs on the onboarding screen (nothing has connected yet) or the
    /// status screen the person may already be looking at (something did, before this happened).
    ever_connected: Arc<AtomicBool>,
}

impl Connector {
    pub fn new(identity: Identity, bus: EventBus) -> Connector {
        Connector {
            identity,
            bus,
            server: zyris::DEFAULT_SERVER_URL.to_string(),
            ever_connected: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn with_server(mut self, url: String) -> Connector {
        self.server = url;
        self
    }

    /// Runs until the process ends. Failures are published, not returned: a node that cannot
    /// connect is still a running program with a window to show, and an `Err` here would take
    /// the whole app down with it.
    pub async fn run(self) {
        let token = match self.token().await {
            Some(token) => token,
            None => return,
        };

        let DialEnd::Refused(error) = self.dial(token).await else { return };

        // A refusal `dial` cannot retry its way out of. Recover at most once: discard the dead
        // token and try again with whatever credential is already on disk, minting a fresh token
        // with no browser and no person involved when that credential is still good, and falling
        // all the way back to enrolment only when it is not. A second refusal after that is
        // treated as final rather than recovered from again — a server that refuses every token
        // it is handed would otherwise mint a new node on every pass and fill the account, which
        // is the exact failure the whole token-reuse design exists to prevent.
        let Some(token) = self.recover_from_dead_token(&error).await else { return };

        if let DialEnd::Refused(error) = self.dial(token).await {
            tracing::warn!(
                %error,
                "the freshly registered node's token was also refused; giving up rather than \
                 recovering again"
            );
            self.bus.publish(CoreEvent::Disconnected {
                reason: format!(
                    "{error}, even after registering a new node with Attacca; giving up rather \
                     than retrying forever"
                ),
                retrying: false,
            });
        }
    }

    /// Builds the node, dials once, and stays until the link is down for good.
    ///
    /// The library reconnects by itself underneath this — `on_connect` below reports every
    /// established connection and every ordinary redial — so what this returns is only how the
    /// link's life *ended*: [`DialEnd::Refused`] for the two `ConnectError` shades no retry can
    /// fix (`Revoked`, `Unauthorized`), [`DialEnd::Stopped`] for everything else, which is
    /// already published by the time this returns.
    async fn dial(&self, token: zyris::NodeToken) -> DialEnd {
        let name = zyris::machine_name().unwrap_or_else(|| "zyris".to_string());
        self.bus.publish(CoreEvent::Connecting);

        let bus = self.bus.clone();
        let node_name = name.clone();
        let ever_connected = self.ever_connected.clone();
        let node = match Node::builder()
            .name(name.as_str())
            .kind(NodeKind::Desktop)
            // `Ok(link)` below only means the link is running, not that a connection is up —
            // `Node::connect` returns it even when the first dial merely failed and is retrying
            // in the background. This hook is what actually fires per established connection,
            // the first one and every reconnect, which is the only place `node_id` is real.
            .on_connect(move |conn| {
                let bus = bus.clone();
                let node_name = node_name.clone();
                let ever_connected = ever_connected.clone();
                async move {
                    // Set before publishing: `report_setup_failure`, called from a concurrent
                    // recovery attempt, must never read a stale `false` and send a person who is
                    // already looking at a working connection to the onboarding screen.
                    ever_connected.store(true, Ordering::Relaxed);
                    bus.publish(CoreEvent::Connected {
                        node_id: conn.info().node_id.clone(),
                        node_name,
                    });

                    // `conn` is this hook's own clone of the connection, spawned concurrently
                    // with it — so awaiting its close does not race the link's own bookkeeping,
                    // it just observes the same close. The link always dials again after an
                    // established connection closes (it only stops redialling on a *dial*
                    // refusal no retry can fix, checked before a connection ever came up, or on
                    // being asked to disconnect — this app never asks), so a close seen here is
                    // always followed by another attempt: `retrying: true`, then `Connecting`.
                    let reason = conn.closed().await;
                    bus.publish(CoreEvent::Disconnected {
                        reason: reason.to_string(),
                        retrying: true,
                    });
                    bus.publish(CoreEvent::Connecting);
                }
            })
            .build()
        {
            Ok(node) => node,
            Err(error) => {
                self.bus.publish(CoreEvent::Disconnected {
                    reason: error.to_string(),
                    retrying: false,
                });
                return DialEnd::Stopped;
            }
        };

        let link = match node.connect(&self.server, token.as_str()).await {
            Ok(link) => link,
            Err(error) if is_permanent_refusal(&error) => return DialEnd::Refused(error),
            Err(error) => {
                // A refusal no retry can fix, but not one recovery can do anything about either
                // — a version mismatch or a missing TLS provider needs a different build, not a
                // new token. Saying so is more useful than a spinner that never stops, and there
                // is no link yet for anything to retry on.
                self.bus.publish(CoreEvent::Disconnected {
                    reason: error.to_string(),
                    retrying: false,
                });
                return DialEnd::Stopped;
            }
        };

        // Nothing further is published here: `Connecting` already went out before the dial, and
        // `on_connect` above reports the real thing — a connection actually established, and
        // every disconnect and redial after it — for as long as this link keeps reconnecting.

        // The link reconnects underneath us; this resolves only when it has given up for good,
        // which `on_connect`'s own `Disconnected { retrying: true }` never claims.
        match link.wait_closed().await {
            Ok(()) => {
                self.bus.publish(CoreEvent::Disconnected {
                    reason: "the link was closed".to_string(),
                    retrying: false,
                });
                DialEnd::Stopped
            }
            Err(error) if is_permanent_refusal(&error) => DialEnd::Refused(error),
            Err(error) => {
                self.bus.publish(CoreEvent::Disconnected {
                    reason: error.to_string(),
                    retrying: false,
                });
                DialEnd::Stopped
            }
        }
    }

    /// Recovers from a node token that Attacca refused outright: discards it, then tries to mint
    /// a replacement with whatever account credential is already on disk, before ever asking a
    /// person for anything.
    ///
    /// The common case is that only the node was removed from Attacca, not the account
    /// authorized to run nodes on it — that account credential mints a fresh token with no
    /// browser and no user action at all. Only when the credential itself cannot mint this node
    /// (see [`credential_cannot_mint_this_node`]: revoked, or its grant too narrow for what a
    /// node needs) does this fall back to a full enrolment, at which point `credential()` is
    /// what puts the onboarding screen up. `mint_node_token` makes that same call for whichever
    /// credential ends up being tried, so it is made in exactly one place.
    ///
    /// Called at most once per `run()`; see the comment at its one call site for why. Every
    /// failure here goes through `report_setup_failure` rather than publishing `SetupFailed`
    /// directly — this runs just as readily after a connection was already live (a redial
    /// permanently refused) as before one ever came up, and only the latter is what the
    /// onboarding screen is honest about.
    async fn recover_from_dead_token(&self, error: &zyris::ConnectError) -> Option<zyris::NodeToken> {
        tracing::warn!(%error, "the stored node token was refused; discarding it and recovering");

        if let Err(error) = self.identity.forget_node_token() {
            tracing::warn!(%error, "could not discard the dead node token; recovery cannot proceed");
            self.report_setup_failure(error.to_string());
            return None;
        }

        let stored_credential = match self.identity.load() {
            Ok(stored) => stored.credential,
            Err(error) => {
                tracing::warn!(%error, "could not read stored identity while recovering from a dead node token");
                self.report_setup_failure(error.to_string());
                return None;
            }
        };

        // With nothing stored to try, go straight to a fresh enrolment; otherwise try the
        // credential already on disk first. Either way, `mint_node_token` is what discards a
        // credential that cannot mint this node and falls back to enrolment itself — see its
        // doc comment — so there is nothing left to special-case here.
        let credential = match stored_credential {
            Some(credential) => credential,
            None => self.credential().await?,
        };
        self.mint_node_token(&self.account(credential)).await
    }

    /// Publishes the right event for a failure on the way to a working token: `SetupFailed` when
    /// no connection has come up yet during this `run()` — the onboarding screen is honest there
    /// — or `Disconnected { retrying: false }` once one has, since a connection having been live
    /// means the person may already be on the status screen, and sending them to onboarding
    /// would wrongly say their account needs reauthorizing when the real problem was, say, a
    /// network hiccup during recovery.
    fn report_setup_failure(&self, reason: String) {
        if self.ever_connected.load(Ordering::Relaxed) {
            self.bus.publish(CoreEvent::Disconnected { reason, retrying: false });
        } else {
            self.bus.publish(CoreEvent::SetupFailed { reason });
        }
    }

    /// The stored credential, or a fresh one from an enrolment the person completes.
    async fn credential(&self) -> Option<AccountCredential> {
        // Checked before anything else: if the identity actually lives in a backend this launch
        // cannot reach, `self.identity.load()` below will honestly report nothing stored — and
        // the rest of this method would honestly, wrongly, start a fresh enrolment over it,
        // minting a second node while the first one's identity sits untouched in the backend
        // this launch cannot see. See `Identity::stranded_in`.
        if let Some(backend) = self.identity.stranded_in() {
            let reason = match backend {
                crate::secret::Backend::Keychain => "the keychain holding this node's identity \
                    is not reachable right now; not starting a fresh enrolment, since that would \
                    register a second node for this machine. Restart once the keychain is \
                    available again."
                    .to_string(),
                crate::secret::Backend::File => "this node's identity is stored in a local file \
                    this launch cannot see; not starting a fresh enrolment, since that would \
                    register a second node for this machine. Restart in the same environment \
                    this node was set up in."
                    .to_string(),
            };
            self.report_setup_failure(reason);
            return None;
        }

        let stored = match self.identity.load() {
            Ok(stored) => stored,
            Err(error) => {
                self.bus.publish(CoreEvent::EnrolmentFailed { reason: error.to_string() });
                return None;
            }
        };
        if let Some(credential) = stored.credential {
            return Some(credential);
        }

        self.bus.publish(CoreEvent::NeedsEnrolment);

        let request = EnrollRequest {
            name: zyris::machine_name().unwrap_or_else(|| "zyris".to_string()),
            platform: std::env::consts::OS.to_string(),
            scopes: ACCOUNT_SCOPES.iter().map(|scope| scope.to_string()).collect(),
        };
        let mut enrollment = match zyris::enroll(&self.server, request).await {
            Ok(enrollment) => enrollment,
            Err(error) => {
                self.bus.publish(CoreEvent::EnrolmentFailed { reason: error.to_string() });
                return None;
            }
        };

        self.publish_code(&enrollment);

        loop {
            match enrollment.poll().await {
                // `poll` keeps the server's own interval, so this loop must not sleep.
                Ok(Progress::Waiting { .. }) => {}
                Ok(Progress::Granted(credential)) => {
                    if let Err(error) = self.identity.save_credential(&credential) {
                        self.bus.publish(CoreEvent::EnrolmentFailed { reason: error.to_string() });
                        return None;
                    }
                    return Some(credential);
                }
                Ok(Progress::Lapsed) => match enrollment.renew().await {
                    Ok(()) => self.publish_code(&enrollment),
                    Err(error) => {
                        self.bus.publish(CoreEvent::EnrolmentFailed { reason: error.to_string() });
                        return None;
                    }
                },
                Ok(Progress::Denied) => {
                    self.bus.publish(CoreEvent::EnrolmentFailed {
                        reason: "the request was declined".to_string(),
                    });
                    return None;
                }
                Err(error) => {
                    self.bus.publish(CoreEvent::EnrolmentFailed { reason: error.to_string() });
                    return None;
                }
            }
        }
    }

    fn publish_code(&self, enrollment: &zyris::enroll::Enrollment) {
        let code = enrollment.code();
        self.bus.publish(CoreEvent::EnrolmentCode {
            user_code: code.user_code.clone(),
            verification_uri: code.verification_uri.clone(),
        });
    }

    /// The stored node token, if there is one — dialling needs nothing else. A missing or
    /// unparseable credential is not this actor's problem when a working token is already on
    /// disk; see `identity.rs`, which documents this exact combination and leaves deciding what
    /// to do about it to this module. Only when there is no token does a credential — and the
    /// `Account` built from it — enter the picture at all, to mint one.
    async fn token(&self) -> Option<zyris::NodeToken> {
        match self.identity.load() {
            Ok(stored) => {
                if let Some(token) = stored.node_token {
                    tracing::info!("reusing this node's token");
                    return Some(token);
                }
            }
            Err(error) => {
                // No link exists yet — this is the secret store itself refusing to answer, which
                // has nothing to do with a connection going up or down. `report_setup_failure`
                // publishes `SetupFailed` here, since nothing has connected yet in this `run()`
                // — see its doc comment for the case where the same failure means something
                // else.
                self.report_setup_failure(error.to_string());
                return None;
            }
        }

        let credential = match self.credential().await {
            Some(credential) => credential,
            None => return None,
        };

        self.mint_node_token(&self.account(credential)).await
    }

    /// Wraps a stored credential in the `Account` handle that can mint node tokens and refreshes
    /// its own access token — the same wiring `token` and `recover_from_dead_token` both need,
    /// extracted so the "Ok only after the write lands" rule lives in one place.
    fn account(&self, credential: AccountCredential) -> Account {
        let identity = self.identity.clone();
        Account::restore(&self.server, credential)
            .on_rotate(move |rotated: AccountCredential| {
                let identity = identity.clone();
                async move {
                    // Ok only after the write lands: a refresh token is single-use, and a crash
                    // between "used" and "saved" is how a node gets revoked.
                    identity
                        .save_credential(&rotated)
                        .map_err(|error| RotateError(error.to_string()))
                }
            })
            .build()
    }

    /// Mints a node token against `account` and reports the outcome.
    ///
    /// Ordinarily terminal: there is no further recovery to attempt for either caller — `token`,
    /// on the very first run, or `recover_from_dead_token`'s own fallback. The one exception is a
    /// credential that cannot mint this node at all ([`credential_cannot_mint_this_node`]):
    /// revoked, or granted a scope too narrow for what a node asks for. A stored credential
    /// already in that state, or one a person just granted too narrowly at the approval screen,
    /// would otherwise dead-end forever — every future launch reads back the same unusable
    /// credential, and the only way out today is deleting it by hand. So for exactly those
    /// failures this discards the credential and enrols exactly once more before giving up for
    /// good. That retry is bounded, not a loop: it cannot itself trigger another one, so this
    /// still enrols at most a small, fixed number of times per call, never repeatedly.
    async fn mint_node_token(&self, account: &Account) -> Option<zyris::NodeToken> {
        match self.register(account).await {
            Ok(token) => token,
            Err(error) if credential_cannot_mint_this_node(&error) => {
                tracing::warn!(
                    %error,
                    "the account grant on disk cannot mint this node; discarding it and \
                     enrolling again"
                );
                if let Err(error) = self.identity.forget() {
                    tracing::warn!(
                        %error,
                        "could not discard the unusable credential; recovery cannot proceed"
                    );
                    self.report_setup_failure(error.to_string());
                    return None;
                }
                let credential = self.credential().await?;
                match self.register(&self.account(credential)).await {
                    Ok(token) => token,
                    Err(error) => {
                        self.report_setup_failure(error.to_string());
                        None
                    }
                }
            }
            Err(error) => {
                self.report_setup_failure(error.to_string());
                None
            }
        }
    }

    /// Mints a node token against `account` and stores it.
    ///
    /// Returns the `RegisterError` rather than reporting it, so `mint_node_token` can tell which
    /// failures mean the credential itself cannot mint this node
    /// ([`credential_cannot_mint_this_node`]) apart from every other failure, which no amount of
    /// discarding and re-enrolling can do anything about. `Ok(None)` is the one outcome that
    /// distinction cannot help with: the mint itself succeeded but the token could not be
    /// written to disk, which is already reported below and terminal regardless of who called
    /// this.
    async fn register(&self, account: &Account) -> Result<Option<zyris::NodeToken>, RegisterError> {
        let spec = NodeSpec {
            name: zyris::machine_name().unwrap_or_else(|| "zyris".to_string()),
            platform: Some(std::env::consts::OS.to_string()),
            scopes: NODE_SCOPES.iter().map(|scope| scope.to_string()).collect(),
        };
        let token = account.register_node(spec).await?;
        match self.identity.save_node_token(&token) {
            Ok(()) => {
                tracing::info!("registered this node");
                Ok(Some(token))
            }
            Err(error) => {
                // A node now exists in this person's Attacca account and its token has just
                // been thrown away: nothing on disk remembers it, so the very next launch
                // calls `register_node` again and mints a second node for the same machine —
                // silently, unless this is loud about it. The account-integrity constraint
                // this violates is the whole reason node tokens are read back before minting;
                // say exactly what happened and what to do about it.
                tracing::error!(
                    %error,
                    node_id = %token.node_id,
                    "a node was registered with Attacca but its token could not be stored; \
                     the next launch will register a duplicate node for this machine. Remove \
                     the orphaned node from this account in Attacca, then restart Zyris."
                );
                self.report_setup_failure(format!(
                    "a node was registered but its token could not be saved ({error}); \
                     restarting will register a duplicate. Remove the orphaned node in \
                     Attacca first."
                ));
                Ok(None)
            }
        }
    }
}

/// How [`Connector::dial`] ended.
enum DialEnd {
    /// The link is down for good and there is nothing more this actor can do — already
    /// published.
    Stopped,
    /// The link is down for good because of a refusal that discarding the token and trying
    /// again might fix. Not yet published: the caller decides what to do about it.
    Refused(zyris::ConnectError),
}

/// The two `ConnectError` shades that mean "no retry will fix this" — a dead grant chain, or a
/// token the server flatly refused. Every other variant means something a fresh token cannot
/// help with either (a version mismatch, an unreachable server, a missing TLS provider), so
/// recovery must not trigger for them.
fn is_permanent_refusal(error: &zyris::ConnectError) -> bool {
    matches!(error, zyris::ConnectError::Revoked | zyris::ConnectError::Unauthorized)
}

/// Whether a registration failure means the credential itself cannot mint this node — a dead
/// grant chain (`Revoked`), or a grant too narrow for what a node asks for (`ScopeExceeded`, the
/// server clamping the request to what the account holds, or `Forbidden`, the account never
/// having `nodes:write` at all) — as opposed to something discarding the credential and
/// re-enrolling cannot fix either (the server being unreachable). `mint_node_token` discards the
/// credential and enrols again for exactly these three; every other `RegisterError` is terminal.
fn credential_cannot_mint_this_node(error: &RegisterError) -> bool {
    matches!(
        error,
        RegisterError::Revoked | RegisterError::ScopeExceeded { .. } | RegisterError::Forbidden
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn with_nothing_stored_it_asks_for_enrolment_first() {
        let dir = tempfile::tempdir().unwrap();
        let identity = crate::identity::Identity::new(
            crate::secret::SecretStore::with_file_dir("zyris-test", dir.path().to_path_buf()),
        );
        let bus = EventBus::new(16);
        let mut events = bus.subscribe();

        // A server that cannot be reached: the point is what is published before the network is
        // touched at all, and a real URL would make this test depend on the internet.
        let connector = Connector::new(identity, bus).with_server("wss://127.0.0.1:1/ws".into());
        tokio::spawn(connector.run());

        let first = tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
            .await
            .expect("the connector published nothing within 5s")
            .unwrap();
        assert_eq!(first, CoreEvent::NeedsEnrolment);
    }

    /// The combination `identity.rs` documents and this module is the only place that decides
    /// what to do about: a node token on disk with no credential beside it. Enrolling again would
    /// send someone to a browser for a node that could dial right now.
    #[tokio::test]
    async fn a_stored_node_token_is_dialled_directly_without_a_credential() {
        let dir = tempfile::tempdir().unwrap();
        let identity = crate::identity::Identity::new(
            crate::secret::SecretStore::with_file_dir("zyris-test", dir.path().to_path_buf()),
        );
        let token: zyris::NodeToken =
            serde_json::from_str(r#"{"node_id":"n_1","slug":"laptop","token":"znt_abc"}"#)
                .expect("NodeToken's shape changed; update this fixture");
        identity.save_node_token(&token).unwrap();
        let bus = EventBus::new(16);
        let mut events = bus.subscribe();

        // Same unreachable server as above: what matters is what is published before the token
        // is even handed to the network.
        let connector = Connector::new(identity, bus).with_server("wss://127.0.0.1:1/ws".into());
        tokio::spawn(connector.run());

        let first = tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
            .await
            .expect("the connector published nothing within 5s")
            .unwrap();
        assert_eq!(
            first,
            CoreEvent::Connecting,
            "a stored token must be dialled directly, not sent through enrolment first"
        );
    }

    #[test]
    fn the_node_token_is_not_allowed_to_mint_another_token() {
        assert!(
            !NODE_SCOPES.contains(&"nodes:write"),
            "a static node token that can mint node tokens is an escalation"
        );
        assert!(ACCOUNT_SCOPES.contains(&"nodes:write"));
    }

    #[test]
    fn the_account_grant_covers_everything_a_node_asks_for() {
        // The server refuses to mint a node that requests more than its account holds, so an
        // account narrower than NODE_SCOPES cannot register anything at all. This is not
        // theoretical: the first real enrolment failed with ScopeExceeded because the account
        // asked for two scopes and the node asked for five.
        let missing: Vec<_> =
            NODE_SCOPES.iter().filter(|scope| !ACCOUNT_SCOPES.contains(scope)).collect();

        assert!(
            missing.is_empty(),
            "the account grant is missing scopes its own node will ask for: {missing:?}"
        );
    }

    #[test]
    fn only_a_dead_grant_chain_or_a_refused_token_triggers_recovery() {
        assert!(is_permanent_refusal(&zyris::ConnectError::Revoked));
        assert!(is_permanent_refusal(&zyris::ConnectError::Unauthorized));

        assert!(
            !is_permanent_refusal(&zyris::ConnectError::VersionMismatch {
                ours: "1".to_string(),
                theirs: Some("2".to_string()),
            }),
            "a version mismatch needs a different build, not a new token"
        );
        assert!(
            !is_permanent_refusal(&zyris::ConnectError::Unreachable(
                zyris::TransportError::Closed
            )),
            "an unreachable server is exactly what the link's own retries already handle"
        );
        assert!(
            !is_permanent_refusal(&zyris::ConnectError::NoTlsProvider),
            "a missing TLS provider is a build problem, not a token problem"
        );
    }

    #[test]
    fn only_a_dead_or_too_narrow_grant_is_treated_as_unusable() {
        assert!(credential_cannot_mint_this_node(&RegisterError::Revoked));
        assert!(credential_cannot_mint_this_node(&RegisterError::Forbidden));
        assert!(credential_cannot_mint_this_node(&RegisterError::ScopeExceeded {
            requested: vec!["nodes:write".to_string()],
            granted: vec!["agents:read".to_string()],
        }));

        assert!(
            !credential_cannot_mint_this_node(&RegisterError::Unreachable(zyris::TransportError::Closed)),
            "an unreachable server needs retrying, not a fresh enrolment"
        );
    }

    #[test]
    fn a_setup_style_failure_is_setup_failed_before_any_connection() {
        let dir = tempfile::tempdir().unwrap();
        let identity = crate::identity::Identity::new(
            crate::secret::SecretStore::with_file_dir("zyris-test", dir.path().to_path_buf()),
        );
        let bus = EventBus::new(16);
        let mut events = bus.subscribe();
        let connector = Connector::new(identity, bus);

        connector.report_setup_failure("disk went away".to_string());

        assert_eq!(
            events.try_recv().unwrap(),
            CoreEvent::SetupFailed { reason: "disk went away".to_string() },
            "nothing has connected yet, so onboarding is the honest screen"
        );
    }

    /// The finding this covers: recovery from a dead node token can run after a connection has
    /// already been live (a redial that gets permanently refused), and a `SetupFailed` there
    /// would wrongly send someone who is already connected — or was a moment ago — to the
    /// onboarding screen, telling them their account needs reauthorizing when it does not.
    #[test]
    fn the_same_failure_is_disconnected_once_a_connection_has_been_live() {
        let dir = tempfile::tempdir().unwrap();
        let identity = crate::identity::Identity::new(
            crate::secret::SecretStore::with_file_dir("zyris-test", dir.path().to_path_buf()),
        );
        let bus = EventBus::new(16);
        let mut events = bus.subscribe();
        let connector = Connector::new(identity, bus);
        connector.ever_connected.store(true, std::sync::atomic::Ordering::Relaxed);

        connector.report_setup_failure("disk went away".to_string());

        assert_eq!(
            events.try_recv().unwrap(),
            CoreEvent::Disconnected { reason: "disk went away".to_string(), retrying: false },
            "the status screen the person may already be on is where this belongs, not onboarding"
        );
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

    /// Recovery must not skip straight to enrolment when a perfectly good credential is sitting
    /// on disk — that would send someone to a browser for a node that a stored account could
    /// re-register on its own. This is checked without a real server: the only network call this
    /// path makes is the mint itself, and an unreachable server answers that quickly enough to
    /// prove the credential was tried at all, without needing to prove the mint succeeds.
    #[tokio::test]
    async fn recovery_tries_the_stored_credential_before_asking_a_person() {
        let dir = tempfile::tempdir().unwrap();
        let identity = crate::identity::Identity::new(
            crate::secret::SecretStore::with_file_dir("zyris-test", dir.path().to_path_buf()),
        );
        identity.save_credential(&a_credential()).unwrap();
        let bus = EventBus::new(16);
        let mut events = bus.subscribe();

        let connector = Connector::new(identity.clone(), bus).with_server("wss://127.0.0.1:1/ws".into());
        let token = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            connector.recover_from_dead_token(&zyris::ConnectError::Revoked),
        )
        .await
        .expect("recovery did not finish within 5s");

        assert!(token.is_none(), "an unreachable server cannot mint anything");
        let published = tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
            .await
            .expect("recovery published nothing within 5s")
            .unwrap();
        assert!(
            matches!(published, CoreEvent::SetupFailed { .. }),
            "the credential must be tried — and reported on — before any onboarding event fires, \
             got {published:?}"
        );
        assert_eq!(
            identity.load().unwrap().credential,
            Some(a_credential()),
            "a credential that was merely unreachable, not revoked, must not be discarded"
        );
    }

    /// The other half of the same decision: with no credential to fall back on, recovery must go
    /// straight to enrolment rather than reporting a bare failure and stopping.
    #[tokio::test]
    async fn recovery_with_no_credential_goes_straight_to_enrolment() {
        let dir = tempfile::tempdir().unwrap();
        let identity = crate::identity::Identity::new(
            crate::secret::SecretStore::with_file_dir("zyris-test", dir.path().to_path_buf()),
        );
        let bus = EventBus::new(16);
        let mut events = bus.subscribe();

        let connector = Connector::new(identity, bus).with_server("wss://127.0.0.1:1/ws".into());
        tokio::spawn(async move {
            connector.recover_from_dead_token(&zyris::ConnectError::Unauthorized).await;
        });

        let published = tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
            .await
            .expect("recovery published nothing within 5s")
            .unwrap();
        assert_eq!(
            published,
            CoreEvent::NeedsEnrolment,
            "recovery with nothing to fall back on must ask a person, not just give up"
        );
    }

    /// The finding this covers: a credential already living in the keychain must not be
    /// declared missing just because this particular launch's `SecretStore` resolved to the file
    /// backend instead — that would mint a second node while the first identity sits untouched
    /// in the keychain. A marker naming a backend this launch did not resolve to must stop
    /// `credential()` before it ever gets the chance to say `NeedsEnrolment`.
    #[tokio::test]
    async fn a_stranded_identity_reports_setup_failed_instead_of_asking_to_enrol_again() {
        let dir = tempfile::tempdir().unwrap();
        // `with_file_dir` always resolves to `File`, so a marker naming `Keychain` can never
        // agree with what this store resolves to — exactly the mismatch a real machine would
        // see when the keychain that used to answer stops being reachable.
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(dir.path().join("backend"), "keychain").unwrap();
        let identity = crate::identity::Identity::new(
            crate::secret::SecretStore::with_file_dir("zyris-test", dir.path().to_path_buf()),
        );
        let bus = EventBus::new(16);
        let mut events = bus.subscribe();

        let connector = Connector::new(identity, bus).with_server("wss://127.0.0.1:1/ws".into());
        tokio::spawn(connector.run());

        let first = tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
            .await
            .expect("the connector published nothing within 5s")
            .unwrap();
        match first {
            CoreEvent::SetupFailed { reason } => {
                assert!(reason.contains("keychain"), "the reason should name the stranded backend: {reason}");
            }
            other => panic!("expected SetupFailed, not a fresh enrolment prompt; got {other:?}"),
        }
    }
}
