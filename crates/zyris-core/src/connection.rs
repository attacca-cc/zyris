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
        let credential = match self.credential().await {
            Some(credential) => credential,
            None => return,
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

        let token = match self.node_token(&account).await {
            Some(token) => token,
            None => return,
        };

        let name = zyris::machine_name().unwrap_or_else(|| "zyris".to_string());
        self.bus.publish(CoreEvent::Connecting);

        let node = match Node::builder().name(name.as_str()).kind(NodeKind::Desktop).build() {
            Ok(node) => node,
            Err(error) => {
                self.bus.publish(CoreEvent::Disconnected { reason: error.to_string() });
                return;
            }
        };

        let link = match node.connect(&self.server, token.as_str()).await {
            Ok(link) => link,
            Err(error) => {
                // A refusal no retry can fix — a revoked token, a rejected node. Saying so is
                // more useful than a spinner that never stops.
                self.bus.publish(CoreEvent::Disconnected { reason: error.to_string() });
                return;
            }
        };

        self.bus.publish(CoreEvent::Connected {
            node_id: link.node_id().to_string(),
            node_name: name,
        });

        // The link reconnects underneath us; this resolves only when it has given up for good.
        let ending = link.wait_closed().await;
        let reason = match ending {
            Ok(()) => "the link was closed".to_string(),
            Err(error) => error.to_string(),
        };
        self.bus.publish(CoreEvent::Disconnected { reason });
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

    /// The stored node token, or one minted once and kept. Minting per launch would fill the
    /// account with nodes, and the per-user cap is real.
    async fn node_token(&self, account: &Account) -> Option<zyris::NodeToken> {
        match self.identity.load() {
            Ok(stored) => {
                if let Some(token) = stored.node_token {
                    tracing::info!("reusing this node's token");
                    return Some(token);
                }
            }
            Err(error) => {
                self.bus.publish(CoreEvent::Disconnected { reason: error.to_string() });
                return None;
            }
        }

        let spec = NodeSpec {
            name: zyris::machine_name().unwrap_or_else(|| "zyris".to_string()),
            platform: Some(std::env::consts::OS.to_string()),
            scopes: NODE_SCOPES.iter().map(|scope| scope.to_string()).collect(),
        };
        match account.register_node(spec).await {
            Ok(token) => {
                if let Err(error) = self.identity.save_node_token(&token) {
                    // Not saving it means the next launch mints another. Say so loudly.
                    self.bus.publish(CoreEvent::Disconnected { reason: error.to_string() });
                    return None;
                }
                tracing::info!("registered this node");
                Some(token)
            }
            Err(error) => {
                self.bus.publish(CoreEvent::Disconnected { reason: error.to_string() });
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

    #[test]
    fn the_node_token_is_not_allowed_to_mint_another_token() {
        assert!(
            !NODE_SCOPES.contains(&"nodes:write"),
            "a static node token that can mint node tokens is an escalation"
        );
        assert!(ACCOUNT_SCOPES.contains(&"nodes:write"));
    }
}
