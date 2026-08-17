//! Wire types and the polling state machine for the OAuth 2.0 Device Authorization Grant
//! (RFC 8628), as a node speaks it.
//!
//! **This module is always compiled and performs no IO.** That is deliberate: the retry semantics
//! below — `slow_down` widening the interval, `authorization_pending` not being an error,
//! `expired_token` meaning "start over" rather than "give up" — are exactly the logic that gets
//! copy-pasted wrong once and stays wrong. Keeping them pure means they can be unit-tested with no
//! server, no network, and no feature flag, which is the only way anyone will actually verify them.
//! It also lets a node with its own HTTP client use these types without taking `reqwest`.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Lower bound on any server-supplied interval. A server that answered `0` would otherwise turn a
/// polite poller into a hot loop.
pub const MIN_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// Upper bound, so a bad or hostile value cannot park a node past its own code's lifetime.
pub const MAX_POLL_INTERVAL: Duration = Duration::from_secs(60);
/// Applied when the server reports `slow_down` without naming a new interval.
pub const SLOW_DOWN_STEP: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Serialize)]
pub struct AuthorizeRequest {
    pub name: String,
    pub platform: String,
    pub scopes: Vec<String>,
    pub client_hint: ClientHint,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHint {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthorizeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: i64,
    pub interval: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: i64,
    #[serde(default)]
    pub scope: String,
    pub node_id: String,
    #[serde(default)]
    pub node_name: String,
    /// The account the node just joined. Printed on success because, over SSH against a URL typed
    /// from memory, this line is the only chance to notice you enrolled into the wrong deployment.
    #[serde(default)]
    pub owner_email: String,
}

/// The RFC 6749 §5.2 error body. `interval` is Attacca's addition, so a node can adopt the
/// server's widened interval instead of guessing.
#[derive(Debug, Clone, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
    #[serde(default)]
    pub error_description: Option<String>,
    #[serde(default)]
    pub interval: Option<i32>,
    /// The HTTP status the body arrived with. **Not part of the wire format** — the transport
    /// stamps it on, because the body alone cannot distinguish the two things that matter here:
    /// the server answering "no" and the server not answering at all. An edge returning 502 with
    /// an HTML page is read as `http_502`, which looks exactly like an unrecognised error code.
    #[serde(skip)]
    pub status: Option<u16>,
}

impl ErrorResponse {
    /// Whether the status says the server could not answer, rather than that it answered.
    ///
    /// 429 is in here with the 5xx family because it means the same thing to a caller deciding
    /// whether to ask again: the request was never judged on its merits.
    pub fn is_transient(&self) -> bool {
        matches!(self.status, Some(status) if status == 429 || (500..600).contains(&status))
    }
}

/// What one poll of the token endpoint means to the node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollOutcome {
    /// Keep waiting; sleep for the enclosed interval first.
    KeepWaiting(Duration),
    /// The code lapsed. Request a new one and print a fresh block — not a fatal condition.
    Expired,
    /// The user said no. Fatal: retrying would be pestering someone who already declined.
    Denied,
    /// Unrecoverable and specific enough to name.
    Fatal(String),
}

/// Tracks a single enrollment attempt's polling cadence. Holding this as a value rather than
/// scattering the arithmetic across the request loop is what makes the whole thing testable.
#[derive(Debug, Clone)]
pub struct PollState {
    interval: Duration,
}

impl PollState {
    /// Start from the server's advertised interval, clamped into a sane range.
    pub fn new(interval_secs: i32) -> PollState {
        PollState { interval: clamp_interval(Duration::from_secs(interval_secs.max(0) as u64)) }
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// Fold one error response into the next action.
    ///
    /// `authorization_pending` is the *expected* answer for almost every poll, and `slow_down` is
    /// a correction rather than a failure — treating either as fatal is the classic way to write
    /// a device-grant client that never completes.
    pub fn on_error(&mut self, response: &ErrorResponse) -> PollOutcome {
        match response.error.as_str() {
            "authorization_pending" => {
                if let Some(interval) = response.interval {
                    self.adopt(interval);
                }
                PollOutcome::KeepWaiting(self.interval)
            }
            "slow_down" => {
                // Prefer the server's number; it knows what it actually enforced. Absent one, back
                // off by the RFC's suggested step rather than keeping the same cadence.
                match response.interval {
                    Some(interval) => self.adopt(interval),
                    None => self.interval = clamp_interval(self.interval + SLOW_DOWN_STEP),
                }
                PollOutcome::KeepWaiting(self.interval)
            }
            "expired_token" => PollOutcome::Expired,
            "access_denied" => PollOutcome::Denied,
            // A 502 from an edge in front of Attacca is not an answer about this grant. The code
            // is still unexpired and the person may not even have reached the screen yet, so
            // giving up throws away a live code and makes them start over — which is what a
            // single 502 mid-poll actually did. Back off and ask again; `expires_in` already
            // bounds how long that can go on.
            _ if response.is_transient() => {
                self.interval = clamp_interval(self.interval + SLOW_DOWN_STEP);
                PollOutcome::KeepWaiting(self.interval)
            }
            other => PollOutcome::Fatal(match &response.error_description {
                Some(description) => format!("{other}: {description}"),
                None => other.to_string(),
            }),
        }
    }

    fn adopt(&mut self, interval_secs: i32) {
        // Only ever widen from a server hint. A server that answered with a *smaller* interval
        // than the node already backed off to would undo a `slow_down` it just issued.
        let proposed = clamp_interval(Duration::from_secs(interval_secs.max(0) as u64));
        self.interval = self.interval.max(proposed);
    }
}

/// What a refused refresh means for the credential that was presented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// The grant chain is dead — the stored credential has to be discarded and the node enrolled
    /// again. Nothing a retry can reach will change this answer.
    Dead(String),
    /// The server could not answer right now. The credential is very probably still good, so it is
    /// kept and the whole thing is tried again later.
    Unavailable(String),
}

/// Fold a refused refresh into the one decision that matters: keep the credential, or throw it away.
///
/// Only `invalid_grant` is fatal. On the refresh endpoint that code has exactly one meaning — the
/// refresh token is unknown, expired, revoked, or was replayed past the reuse grace — and in every
/// one of those cases the server will never honour it again.
///
/// Everything else is transient by default, deliberately: this classification decides whether a
/// file gets deleted, and the two mistakes are not symmetric. Discarding a live credential drags a
/// person back to a terminal to approve a code; keeping a dead one costs a backoff cycle. That
/// asymmetry is also why the catch-all sits on this side — `parse_error` synthesises `http_503` and
/// friends for bodies it could not read, and a proxy returning HTML mid-outage must not be able to
/// unenroll a fleet.
pub fn classify_refresh_error(error: &ErrorResponse) -> RefreshOutcome {
    let described = match &error.error_description {
        Some(description) => format!("{}: {description}", error.error),
        None => error.error.clone(),
    };
    // The status outranks the body. Only `invalid_grant` kills a credential, and a server that
    // could not answer has not told us the grant is dead — even if something in front of it put
    // that word in the body. Both callers answer `Dead` by deleting the credential, so the cost
    // of reading one 500 wrong is a fleet that has to re-enrol by hand.
    if error.is_transient() {
        return RefreshOutcome::Unavailable(described);
    }
    match error.error.as_str() {
        "invalid_grant" => RefreshOutcome::Dead(described),
        _ => RefreshOutcome::Unavailable(described),
    }
}

fn clamp_interval(interval: Duration) -> Duration {
    interval.clamp(MIN_POLL_INTERVAL, MAX_POLL_INTERVAL)
}

/// Render the block a node prints while waiting.
///
/// Pure ASCII, no box drawing: this has to survive an SSH session into a machine with an unhelpful
/// locale. The code is printed with its hyphen but no internal spaces, so a double-click selects
/// the whole thing.
///
/// Two steps, not three: the server's `verification_uri` points at the code-entry screen itself, so
/// there is no button to hunt for in between. It still carries no code — a prefilled link is the one
/// thing RFC 8628 §5.4 names and offers no mitigation for.
pub fn authorization_notice(response: &AuthorizeResponse) -> String {
    let minutes = (response.expires_in + 59) / 60;
    format!(
        "\n\
         --------------------------------------------------------------\n  \
         Authorize this node\n\n  \
         1. Open        {uri}\n  \
         2. Enter code  {code}\n\n  \
         Waiting for approval. This code expires in {minutes} minutes.\n  \
         Press Ctrl-C to cancel.\n\
         --------------------------------------------------------------\n",
        uri = response.verification_uri,
        code = response.user_code,
        minutes = minutes,
    )
}

/// The line printed once a node is live. Names the account, because enrolling into the wrong
/// deployment is otherwise silent and indistinguishable from success.
pub fn authorized_notice(response: &TokenResponse) -> String {
    let scopes = if response.scope.trim().is_empty() {
        "no access to your Attacca account".to_string()
    } else {
        format!("scopes: {}", response.scope)
    };
    format!(
        "Authorized as \"{name}\" in the account {email}. ({scopes})",
        name = response.node_name,
        email = response.owner_email,
        scopes = scopes,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(kind: &str, interval: Option<i32>) -> ErrorResponse {
        ErrorResponse {
            error: kind.to_string(),
            error_description: Some("because".to_string()),
            interval,
            status: None,
        }
    }

    /// The path a real enrollment takes: pending for a while, one correction, then success. If
    /// this ever reads as anything but "keep waiting", nodes stop enrolling.
    #[test]
    fn pending_then_slow_down_then_still_pending() {
        let mut state = PollState::new(5);
        assert_eq!(state.interval(), Duration::from_secs(5));

        assert_eq!(
            state.on_error(&error("authorization_pending", None)),
            PollOutcome::KeepWaiting(Duration::from_secs(5))
        );

        // The server widened its own interval and said so; the node adopts it verbatim.
        assert_eq!(
            state.on_error(&error("slow_down", Some(10))),
            PollOutcome::KeepWaiting(Duration::from_secs(10))
        );
        assert_eq!(
            state.on_error(&error("authorization_pending", Some(10))),
            PollOutcome::KeepWaiting(Duration::from_secs(10))
        );
    }

    /// A `slow_down` with no interval must still back off, or the node keeps making the mistake
    /// the server just complained about.
    #[test]
    fn slow_down_without_a_hint_still_widens() {
        let mut state = PollState::new(5);
        assert_eq!(
            state.on_error(&error("slow_down", None)),
            PollOutcome::KeepWaiting(Duration::from_secs(10))
        );
        assert_eq!(
            state.on_error(&error("slow_down", None)),
            PollOutcome::KeepWaiting(Duration::from_secs(15))
        );
    }

    /// A server hint must never *shrink* the interval — that would let a reply undo the very
    /// `slow_down` that preceded it.
    #[test]
    fn a_smaller_server_hint_never_undoes_a_backoff() {
        let mut state = PollState::new(5);
        state.on_error(&error("slow_down", Some(30)));
        assert_eq!(state.interval(), Duration::from_secs(30));
        state.on_error(&error("authorization_pending", Some(5)));
        assert_eq!(state.interval(), Duration::from_secs(30));
    }

    #[test]
    fn intervals_are_clamped_at_both_ends() {
        assert_eq!(PollState::new(0).interval(), MIN_POLL_INTERVAL, "0 must not become a hot loop");
        assert_eq!(PollState::new(-5).interval(), MIN_POLL_INTERVAL);
        assert_eq!(PollState::new(9999).interval(), MAX_POLL_INTERVAL);

        let mut state = PollState::new(5);
        for _ in 0..100 {
            state.on_error(&error("slow_down", None));
        }
        assert_eq!(state.interval(), MAX_POLL_INTERVAL);
    }

    /// Expiry is recoverable (ask for a new code) and denial is not (someone said no). Confusing
    /// the two either strands a node forever or pesters a user who already declined.
    #[test]
    fn expiry_and_denial_are_distinct() {
        let mut state = PollState::new(5);
        assert_eq!(state.on_error(&error("expired_token", None)), PollOutcome::Expired);
        assert_eq!(state.on_error(&error("access_denied", None)), PollOutcome::Denied);
    }

    #[test]
    fn unknown_errors_are_fatal_and_keep_the_description() {
        let mut state = PollState::new(5);
        let outcome = state.on_error(&error("invalid_grant", None));
        let PollOutcome::Fatal(message) = outcome else { panic!("expected fatal") };
        assert!(message.contains("invalid_grant"));
        assert!(message.contains("because"), "the server's reason must survive");
    }

    /// Observed live: a Windows node polling with ~5 minutes left on its code took one 502 from
    /// the edge and printed "the server rejected this node", ending the attempt. The code was
    /// still good and the person had not yet approved it.
    #[test]
    fn a_transient_5xx_keeps_waiting_instead_of_ending_the_attempt() {
        for status in [429u16, 500, 502, 503, 504] {
            let mut state = PollState::new(5);
            let response = ErrorResponse {
                error: format!("http_{status}"),
                error_description: None,
                interval: None,
                status: Some(status),
            };
            let PollOutcome::KeepWaiting(interval) = state.on_error(&response) else {
                panic!("{status} ended the enrollment; it says nothing about the grant")
            };
            assert_eq!(
                interval,
                Duration::from_secs(10),
                "an unreachable server should be asked less often, not given up on"
            );
        }
    }

    /// The other edge of the same rule. Retrying must not swallow answers the server actually
    /// gave — a 400 naming an error code this client does not know is a real refusal, and polling
    /// on until the code expires would hide it behind a timeout.
    #[test]
    fn a_four_hundred_with_an_unknown_code_is_still_fatal() {
        let mut state = PollState::new(5);
        let response = ErrorResponse {
            error: "unsupported_grant_type".into(),
            error_description: Some("because".into()),
            interval: None,
            status: Some(400),
        };
        let PollOutcome::Fatal(message) = state.on_error(&response) else {
            panic!("a 400 is the server answering, and this client cannot honour that answer")
        };
        assert!(message.contains("unsupported_grant_type"));
    }

    /// The one answer that means "this credential is gone" — and the reason has to survive, because
    /// it is what gets logged immediately before a file is deleted.
    #[test]
    fn only_invalid_grant_kills_a_stored_credential() {
        let outcome = classify_refresh_error(&error("invalid_grant", None));
        let RefreshOutcome::Dead(reason) = outcome else { panic!("expected dead") };
        assert!(reason.contains("invalid_grant"));
        assert!(reason.contains("because"), "the server's reason must survive");
    }

    /// The status outranks the body. Both callers answer `Dead` by deleting the credential, so a
    /// proxy that manages to put `invalid_grant` in front of a 500 — a cached body, a error page
    /// assembled from the wrong template — must not be able to unenroll a fleet.
    #[test]
    fn a_five_hundred_cannot_kill_a_credential_whatever_the_body_says() {
        let response = ErrorResponse {
            error: "invalid_grant".into(),
            error_description: Some("because".into()),
            interval: None,
            status: Some(500),
        };
        assert!(
            matches!(classify_refresh_error(&response), RefreshOutcome::Unavailable(_)),
            "a server that could not answer has not told us the grant is dead"
        );
    }

    /// The asymmetry this classification exists for: an outage must never unenroll a node. Every one
    /// of these is a real answer from the refresh endpoint, and none of them means the grant is dead.
    #[test]
    fn a_server_having_a_bad_day_never_discards_a_credential() {
        for kind in ["temporarily_unavailable", "slow_down", "server_error", "invalid_request"] {
            assert!(
                matches!(classify_refresh_error(&error(kind, None)), RefreshOutcome::Unavailable(_)),
                "{kind} must not be treated as a dead grant"
            );
        }
    }

    /// `parse_error` synthesises this shape when the body is not JSON at all — an HTML error page
    /// from a proxy mid-outage. Unreadable must not mean unenrolled.
    #[test]
    fn an_unreadable_error_body_is_transient_not_fatal() {
        let synthetic =
            ErrorResponse { error: "http_503".into(), error_description: None, interval: None, status: Some(503) };
        let RefreshOutcome::Unavailable(reason) = classify_refresh_error(&synthetic) else {
            panic!("a body we could not read tells us nothing about the grant")
        };
        assert_eq!(reason, "http_503", "with no description the code stands alone");
    }

    /// The notice is the primary UX of this feature and has to survive SSH: ASCII only, and no
    /// internal spaces in the code so a double-click selects it whole.
    #[test]
    fn the_notice_is_plain_ascii_and_selectable() {
        let notice = authorization_notice(&AuthorizeResponse {
            device_code: "zdc_secret".into(),
            user_code: "WXQR-7KBD".into(),
            verification_uri: "https://attacca.example/settings/zyris/device".into(),
            expires_in: 600,
            interval: 5,
        });
        assert!(notice.is_ascii());
        assert!(notice.contains("WXQR-7KBD"));
        assert!(notice.contains("expires in 10 minutes"));
        assert!(!notice.contains("zdc_secret"), "the device code is never displayed");
    }

    #[test]
    fn the_success_line_names_the_account_and_the_grant() {
        let response = TokenResponse {
            access_token: "zna_x".into(),
            refresh_token: "znr_x".into(),
            expires_in: 3600,
            scope: "sessions:read".into(),
            node_id: "n".into(),
            node_name: "hello node".into(),
            owner_email: "allen@example.com".into(),
        };
        let line = authorized_notice(&response);
        assert!(line.contains("hello node"));
        assert!(line.contains("allen@example.com"));
        assert!(line.contains("sessions:read"));
        assert!(!line.contains("zna_x"), "credentials are never printed");

        // The zero-scope case is the common one for a tool-only node and gets plain words rather
        // than an empty list.
        let bare = authorized_notice(&TokenResponse { scope: String::new(), ..response });
        assert!(bare.contains("no access to your Attacca account"));
    }
}
