//! Everything between "this process started" and "this node is connected".
//!
//! The library reconnects by itself — `Node::connect` returns a `Link` that backs off and dials
//! again — so nothing here retries. This actor establishes the identity, hands it to the link,
//! and reports what it observes.

use zyris::enroll::{EnrollRequest, Progress};
use zyris::{Account, AccountCredential, Node, NodeKind, NodeSpec, RotateError};

use crate::event::{CoreEvent, EventBus};
use crate::identity::Identity;

/// What the account grant asks for. `nodes:write` is the one that matters: without it
/// `register_node` comes back forbidden and this node has no token to dial with.
pub const ACCOUNT_SCOPES: &[&str] = &["agents:read", "nodes:write"];

/// What the node token carries, which is deliberately less. A static token must never be able to
/// mint another one, so `nodes:write` stops at the account layer.
pub const NODE_SCOPES: &[&str] =
    &["agents:read", "sessions:read", "sessions:write", "events:read", "peers:write"];

pub struct Connector {
    identity: Identity,
    bus: EventBus,
    server: String,
}

impl Connector {
    pub fn new(identity: Identity, bus: EventBus) -> Connector {
        Connector { identity, bus, server: zyris::DEFAULT_SERVER_URL.to_string() }
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

        let name = zyris::machine_name().unwrap_or_else(|| "zyris".to_string());
        self.bus.publish(CoreEvent::Connecting);

        let bus = self.bus.clone();
        let node_name = name.clone();
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
                async move {
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
                return;
            }
        };

        let link = match node.connect(&self.server, token.as_str()).await {
            Ok(link) => link,
            Err(error) => {
                // A refusal no retry can fix — a revoked token, a rejected node. Saying so is
                // more useful than a spinner that never stops, and there is no link yet for
                // anything to retry on.
                self.bus.publish(CoreEvent::Disconnected {
                    reason: error.to_string(),
                    retrying: false,
                });
                return;
            }
        };

        // Nothing further is published here: `Connecting` already went out before the dial, and
        // `on_connect` above reports the real thing — a connection actually established, and
        // every disconnect and redial after it — for as long as this link keeps reconnecting.

        // The link reconnects underneath us; this resolves only when it has given up for good,
        // which `on_connect`'s own `Disconnected { retrying: true }` never claims.
        let ending = link.wait_closed().await;
        let reason = match ending {
            Ok(()) => "the link was closed".to_string(),
            Err(error) => error.to_string(),
        };
        self.bus.publish(CoreEvent::Disconnected { reason, retrying: false });
    }

    /// The stored credential, or a fresh one from an enrolment the person completes.
    async fn credential(&self) -> Option<AccountCredential> {
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
                // has nothing to do with a connection going up or down. Publishing this as
                // `Disconnected` would send the window to a "not connected" status screen for a
                // failure that happened before a dial was ever attempted; `SetupFailed` is the
                // channel the UI can tell apart from an ordinary connection problem.
                self.bus.publish(CoreEvent::SetupFailed { reason: error.to_string() });
                return None;
            }
        }

        let credential = match self.credential().await {
            Some(credential) => credential,
            None => return None,
        };

        let identity = self.identity.clone();
        let account = Account::restore(&self.server, credential)
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
            .build();

        self.mint_node_token(&account).await
    }

    /// Mints a node token against this account and stores it. Reached only when nothing was
    /// already on disk — minting on every launch would fill the account with nodes, and the
    /// per-user cap is real.
    async fn mint_node_token(&self, account: &Account) -> Option<zyris::NodeToken> {
        let spec = NodeSpec {
            name: zyris::machine_name().unwrap_or_else(|| "zyris".to_string()),
            platform: Some(std::env::consts::OS.to_string()),
            scopes: NODE_SCOPES.iter().map(|scope| scope.to_string()).collect(),
        };
        match account.register_node(spec).await {
            Ok(token) => {
                if let Err(error) = self.identity.save_node_token(&token) {
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
                    self.bus.publish(CoreEvent::SetupFailed {
                        reason: format!(
                            "a node was registered but its token could not be saved ({error}); \
                             restarting will register a duplicate. Remove the orphaned node in \
                             Attacca first."
                        ),
                    });
                    return None;
                }
                tracing::info!("registered this node");
                Some(token)
            }
            Err(error) => {
                self.bus.publish(CoreEvent::SetupFailed { reason: error.to_string() });
                None
            }
        }
    }
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
}
