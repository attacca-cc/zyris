//! The `reqwest` driver for the device grant, plus the startup decision tree a node follows.
//!
//! Dependency weight is smaller than it looks: `tokio-tungstenite` already pulls in `rustls`,
//! `tokio-rustls`, and the native root store, so `reqwest` configured without default features
//! **reuses that TLS stack** — the marginal cost is the hyper/tower layer. It is off by default, so
//! a node using a static `znt_` pays nothing for it.

use std::io::IsTerminal;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::enroll::protocol::{
    authorization_notice, authorized_notice, classify_refresh_error, AuthorizeRequest,
    AuthorizeResponse, ClientHint, ErrorResponse, PollOutcome, PollState, RefreshOutcome,
    TokenResponse,
};
use crate::enroll::store::{CredentialStore, CredentialStoreError, StoredCredential};

/// Access-token lifetime the server issues; used only to decide when to refresh early.
const ACCESS_LIFETIME_SECS: i64 = 3600;
/// How many times a lapsed code is replaced with a fresh one before giving up. Three rounds is
/// roughly half an hour of a human not being at their desk.
const MAX_ENROLLMENT_ROUNDS: usize = 3;
/// Cadence of the "still waiting" line when stdout is not a terminal.
const LOG_PROGRESS_EVERY: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
pub enum EnrollError {
    /// The user declined, or the code expired too many times. Exit code 2, not 1: this needs a
    /// person to do something, and a supervisor should not restart-loop on it forever printing
    /// codes into a log nobody reads.
    #[error("{0}")]
    NeedsOperator(String),
    /// The server will never honour this grant again — it was revoked, it expired, or its chain was
    /// broken. Both refresh paths answer this by clearing the store and enrolling from scratch, so
    /// a merely *unreachable* server must never be reported here. See `classify_refresh_error`.
    #[error("the server rejected this node: {0}")]
    Rejected(String),
    #[error(transparent)]
    Store(#[from] CredentialStoreError),
    #[error("enrollment request failed: {0}")]
    Transport(String),
}

/// Everything a node needs to enroll itself and keep its credentials current.
pub struct Enroller {
    http: reqwest::Client,
    base_url: String,
    store: Arc<dyn CredentialStore>,
    request: AuthorizeRequest,
}

impl Enroller {
    /// `server_url` is the websocket URL the node dials; the HTTP base is derived from it, so a
    /// node is configured with exactly one address and cannot end up enrolling against one
    /// deployment while connecting to another.
    ///
    /// `store` is where the issued credential lives between runs. Most nodes want
    /// [`with_file_store`](Self::with_file_store); pass your own to keep it in a keychain, a
    /// Secret, or nowhere at all.
    pub fn new(
        server_url: &str,
        node_name: String,
        platform: String,
        scopes: Vec<String>,
        store: Arc<dyn CredentialStore>,
    ) -> Result<Enroller, EnrollError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("zyris/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| EnrollError::Transport(e.to_string()))?;

        Ok(Enroller {
            http,
            base_url: http_base(server_url),
            store,
            request: AuthorizeRequest {
                name: node_name,
                platform,
                scopes,
                client_hint: local_client_hint(),
            },
        })
    }

    /// The zero-configuration path: keep the credential in the conventional per-user file for this
    /// `(server_url, profile)` pair.
    #[cfg(feature = "persistence")]
    pub fn with_file_store(
        server_url: &str,
        profile: &str,
        node_name: String,
        platform: String,
        scopes: Vec<String>,
    ) -> Result<Enroller, EnrollError> {
        let store = crate::enroll::FileCredentialStore::for_server(server_url, profile)
            .map_err(CredentialStoreError::from)?;
        Enroller::new(server_url, node_name, platform, scopes, Arc::new(store))
    }

    /// Where this node's credential lives, for the one debug line worth logging at startup. Never
    /// contains a secret.
    pub fn store_description(&self) -> String {
        self.store.describe()
    }

    /// The startup decision tree, in one place.
    ///
    /// A stored credential is refreshed rather than re-enrolled; a refresh the server rejects
    /// outright clears the file and starts over (the node was revoked, or its chain was); and a
    /// refresh that merely failed to *reach* the server is retried, never treated as fatal — a
    /// flaky network must not force a human back to the terminal.
    pub async fn obtain(&self) -> Result<StoredCredential, EnrollError> {
        if let Some(stored) = self.load_forgiving().await? {
            if !stored.should_refresh(now_unix(), ACCESS_LIFETIME_SECS) {
                return Ok(stored);
            }
            match self.refresh(&stored.refresh_token).await {
                Ok(refreshed) => return Ok(refreshed),
                Err(EnrollError::Rejected(reason)) => {
                    // The server will never honour this again; keeping it would loop forever.
                    tracing::warn!(%reason, "stored zyris credential rejected; re-enrolling");
                    self.store.clear().await?;
                }
                Err(EnrollError::Transport(e)) => {
                    // Could not reach the server. The credential may well still be good, and the
                    // access token has ~20% of its life left by construction, so use it.
                    tracing::warn!(error = %e, "could not refresh; continuing with the stored credential");
                    return Ok(stored);
                }
                Err(e) => return Err(e),
            }
        }
        self.enroll().await
    }

    /// Force a rotation, for the 401 at the websocket upgrade — a slept laptop or a clock that
    /// drifted can produce one, and the transport maps 401 to non-retriable, so without this a node
    /// would exit permanently on a condition it could have fixed itself.
    ///
    /// `Ok(None)` means the server disowned the credential outright: the store has already been
    /// cleared, and the caller's next [`obtain`](Self::obtain) will enroll from scratch. That is the
    /// same conclusion `obtain` reaches on the startup path — a node whose grant chain was revoked
    /// while it was connected must not be left presenting the dead token until a human deletes the
    /// file, which is exactly what happens if a rejected rotation merely propagates.
    pub async fn force_refresh(
        &self,
        stored: &StoredCredential,
    ) -> Result<Option<StoredCredential>, EnrollError> {
        match self.refresh(&stored.refresh_token).await {
            Ok(rotated) => Ok(Some(rotated)),
            Err(EnrollError::Rejected(reason)) => {
                tracing::warn!(%reason, "zyris credential rejected on rotation; re-enrolling");
                self.store.clear().await?;
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// A corrupt or unreadable credential is a reason to enroll again, not to die. A *refused* one
    /// is different — a world-readable key file, a backend that failed in a way this crate cannot
    /// classify — and refusing loudly is the whole point of that distinction, so it propagates
    /// rather than answering an exposed secret with a quiet re-enrollment.
    async fn load_forgiving(&self) -> Result<Option<StoredCredential>, EnrollError> {
        match self.store.load().await {
            Ok(stored) => Ok(stored),
            Err(e) if !e.is_discardable() => Err(e.into()),
            Err(e) => {
                tracing::warn!(error = %e, "discarding unusable stored credential");
                self.store.clear().await?;
                Ok(None)
            }
        }
    }

    async fn enroll(&self) -> Result<StoredCredential, EnrollError> {
        for round in 0..MAX_ENROLLMENT_ROUNDS {
            let authorization = self.authorize().await?;
            // `println!`, not `tracing`: someone running with `RUST_LOG=error` must still see the
            // code. This block is the primary UX of the whole feature.
            println!("{}", authorization_notice(&authorization));

            match self.poll_until_resolved(&authorization).await? {
                Some(credential) => {
                    println!("{}", authorized_notice(&credential.0));
                    return Ok(credential.1);
                }
                None if round + 1 < MAX_ENROLLMENT_ROUNDS => {
                    println!("That code expired. Requesting a new one.");
                }
                None => break,
            }
        }
        Err(EnrollError::NeedsOperator(
            "no code was authorized in time; run this again when you can approve it".to_string(),
        ))
    }

    /// Poll until the grant resolves. `Ok(None)` means the code lapsed and a fresh one is worth
    /// requesting; `Err` means stop.
    async fn poll_until_resolved(
        &self,
        authorization: &AuthorizeResponse,
    ) -> Result<Option<(TokenResponse, StoredCredential)>, EnrollError> {
        let mut state = PollState::new(authorization.interval);
        let deadline = std::time::Instant::now() + Duration::from_secs(authorization.expires_in.max(0) as u64);
        let mut progress = Progress::new(deadline);

        loop {
            tokio::time::sleep(state.interval()).await;
            if std::time::Instant::now() >= deadline {
                return Ok(None);
            }
            progress.tick();

            let response = self
                .post("/zyris/v1/device/token", &serde_json::json!({
                    "device_code": authorization.device_code,
                    "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
                }))
                .await?;

            if response.status().is_success() {
                progress.finish();
                let token: TokenResponse =
                    response.json().await.map_err(|e| EnrollError::Transport(e.to_string()))?;
                let credential = self.persist(&token).await?;
                return Ok(Some((token, credential)));
            }

            let error = parse_error(response).await;
            match state.on_error(&error) {
                PollOutcome::KeepWaiting(_) => continue,
                PollOutcome::Expired => {
                    progress.finish();
                    return Ok(None);
                }
                PollOutcome::Denied => {
                    progress.finish();
                    return Err(EnrollError::NeedsOperator(
                        "the request was declined in Attacca".to_string(),
                    ));
                }
                PollOutcome::Fatal(message) => {
                    progress.finish();
                    return Err(EnrollError::Rejected(message));
                }
            }
        }
    }

    async fn authorize(&self) -> Result<AuthorizeResponse, EnrollError> {
        let response = self.post("/zyris/v1/device/authorize", &self.request).await?;
        if !response.status().is_success() {
            let error = parse_error(response).await;
            return Err(EnrollError::Rejected(describe(&error)));
        }
        response.json().await.map_err(|e| EnrollError::Transport(e.to_string()))
    }

    async fn refresh(&self, refresh_token: &str) -> Result<StoredCredential, EnrollError> {
        let response = self
            .post("/zyris/v1/device/refresh", &serde_json::json!({ "refresh_token": refresh_token }))
            .await?;
        if !response.status().is_success() {
            // `Rejected` from here is load-bearing in a way it is not anywhere else: both callers
            // answer it by deleting the credential. So a refusal is classified rather than assumed
            // fatal — a 503 during a deploy would otherwise unenroll every node that dialled through
            // it. See `classify_refresh_error`.
            let error = parse_error(response).await;
            return Err(match classify_refresh_error(&error) {
                RefreshOutcome::Dead(reason) => EnrollError::Rejected(reason),
                RefreshOutcome::Unavailable(reason) => EnrollError::Transport(reason),
            });
        }
        let token: TokenResponse =
            response.json().await.map_err(|e| EnrollError::Transport(e.to_string()))?;
        self.persist(&token).await
    }

    /// Persist **before** the credential is used. A pair written to the store after a successful
    /// dial would be lost if the dial crashed, stranding a node whose old refresh token the server
    /// has already spent.
    async fn persist(&self, token: &TokenResponse) -> Result<StoredCredential, EnrollError> {
        let credential = StoredCredential::new(
            token.access_token.clone(),
            token.refresh_token.clone(),
            token.node_id.clone(),
            token.node_name.clone(),
            token.owner_email.clone(),
            now_unix() + token.expires_in,
        );
        self.store.save(&credential).await?;
        Ok(credential)
    }

    async fn post<T: serde::Serialize + ?Sized>(
        &self,
        path: &str,
        body: &T,
    ) -> Result<reqwest::Response, EnrollError> {
        self.http
            .post(format!("{}{path}", self.base_url))
            .json(body)
            .send()
            .await
            .map_err(|e| EnrollError::Transport(e.to_string()))
    }
}

/// Never a dot per poll: that turns a captured log into confetti and tells the reader nothing. On
/// a terminal this rewrites one line; otherwise it prints at most one line a minute.
struct Progress {
    deadline: std::time::Instant,
    interactive: bool,
    last_logged: std::time::Instant,
}

impl Progress {
    fn new(deadline: std::time::Instant) -> Progress {
        Progress {
            deadline,
            interactive: std::io::stdout().is_terminal(),
            last_logged: std::time::Instant::now(),
        }
    }

    fn tick(&mut self) {
        let remaining = self.deadline.saturating_duration_since(std::time::Instant::now());
        if self.interactive {
            use std::io::Write;
            print!("\r  Waiting for approval... {:>3}s left ", remaining.as_secs());
            let _ = std::io::stdout().flush();
        } else if self.last_logged.elapsed() >= LOG_PROGRESS_EVERY {
            self.last_logged = std::time::Instant::now();
            println!("  Still waiting for approval; {}s left.", remaining.as_secs());
        }
    }

    fn finish(&self) {
        if self.interactive {
            // Clear the rewritten line so the next message does not land on top of it.
            print!("\r{:60}\r", "");
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }
    }
}

async fn parse_error(response: reqwest::Response) -> ErrorResponse {
    let status = response.status();
    response.json::<ErrorResponse>().await.unwrap_or(ErrorResponse {
        error: format!("http_{}", status.as_u16()),
        error_description: None,
        interval: None,
    })
}

fn describe(error: &ErrorResponse) -> String {
    match &error.error_description {
        Some(description) => format!("{}: {description}", error.error),
        None => error.error.clone(),
    }
}

/// `ws://…/zyris/v1/ws` → `http://…`. A node is configured with one address, so the enrollment
/// endpoints are derived from it rather than being a second thing to get wrong.
fn http_base(server_url: &str) -> String {
    let without_scheme = server_url
        .strip_prefix("wss://")
        .map(|rest| format!("https://{rest}"))
        .or_else(|| server_url.strip_prefix("ws://").map(|rest| format!("http://{rest}")))
        .unwrap_or_else(|| server_url.to_string());
    match without_scheme.find("/zyris/") {
        Some(index) => without_scheme[..index].to_string(),
        None => without_scheme.trim_end_matches('/').to_string(),
    }
}

/// What this machine says about itself. Every field is a hint the server never verifies, and the
/// approval screen labels it as such.
fn local_client_hint() -> ClientHint {
    ClientHint {
        hostname: hostname(),
        os: Some(std::env::consts::OS.to_string()),
        agent: Some(concat!("zyris/", env!("CARGO_PKG_VERSION")).to_string()),
    }
}

#[cfg(feature = "hostname")]
fn hostname() -> Option<String> {
    crate::machine_name()
}

#[cfg(not(feature = "hostname"))]
fn hostname() -> Option<String> {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok().map(|v| v.trim().to_string()))
        .filter(|v| !v.is_empty())
}

fn now_unix() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Answer every request on a fresh loopback port with one canned response, and return the
/// websocket URL an [`Enroller`] should be built with to reach it.
///
/// A real socket rather than a faked HTTP client, because the branch under test — the one that
/// deletes a node's credential — keys off a status line and a response body, and stubbing the
/// client would stub out exactly the input that decides it.
#[cfg(test)]
pub(crate) fn canned_responder(status_line: &str, body: &'static str) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("no loopback port");
    let address = listener.local_addr().expect("no local address");
    let status_line = status_line.to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            use std::io::{Read, Write};
            let Ok(mut stream) = stream else { return };
            let _ = stream.read(&mut [0u8; 4096]);
            let response = format!(
                "HTTP/1.1 {status_line}\r\ncontent-type: application/json\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    format!("ws://{address}/zyris/v1/ws")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enroll::MemoryCredentialStore;

    fn credential() -> StoredCredential {
        StoredCredential::new(
            "zna_access".into(),
            "znr_refresh".into(),
            "node-id".into(),
            "hello node".into(),
            "allen@example.com".into(),
            now_unix() + 3600,
        )
    }

    fn enroller_against(url: &str, store: Arc<MemoryCredentialStore>) -> Enroller {
        Enroller::new(url, "hello node".into(), "linux".into(), vec!["agents:read".into()], store)
            .expect("the client builds")
    }

    /// The whole point of the rotation path: a grant the server has disowned must not survive on
    /// disk. Leaving it there is what turns one revoked node into a restart loop that presents the
    /// same dead token forever, since a stored access token with life left is never re-validated.
    #[tokio::test]
    async fn a_disowned_grant_is_deleted_rather_than_presented_again() {
        let url = canned_responder(
            "400 Bad Request",
            r#"{"error":"invalid_grant","error_description":"that code is not valid"}"#,
        );
        let store = Arc::new(MemoryCredentialStore::new());
        store.save(&credential()).await.unwrap();

        let enroller = enroller_against(&url, store.clone());
        let rotated = enroller.force_refresh(&credential()).await.expect("not a hard failure");

        assert!(rotated.is_none(), "a dead grant yields no credential to carry on with");
        assert_eq!(store.load().await.unwrap(), None, "the dead credential must not survive");
    }

    /// The mirror image, and the reason the refusal is classified at all: a server having a bad day
    /// must not be able to unenroll a fleet. The credential stays, and the caller is told to retry.
    #[tokio::test]
    async fn an_outage_during_rotation_leaves_the_credential_alone() {
        let url = canned_responder(
            "503 Service Unavailable",
            r#"{"error":"temporarily_unavailable","error_description":"enrollment is temporarily unavailable"}"#,
        );
        let store = Arc::new(MemoryCredentialStore::new());
        store.save(&credential()).await.unwrap();

        let enroller = enroller_against(&url, store.clone());
        let error = enroller.force_refresh(&credential()).await.expect_err("503 is a failure");

        assert!(matches!(error, EnrollError::Transport(_)), "must be retriable, got {error}");
        assert_eq!(
            store.load().await.unwrap().as_ref(),
            Some(&credential()),
            "an outage must never unenroll a node"
        );
    }

    /// `obtain` shares the classification, so the same outage on the proactive 80%-of-lifetime
    /// refresh keeps the node running on the credential it already holds.
    #[tokio::test]
    async fn a_stored_credential_outlives_an_unreachable_refresh() {
        let url = canned_responder("503 Service Unavailable", r#"{"error":"server_error"}"#);
        let store = Arc::new(MemoryCredentialStore::new());
        // Past the 80% mark, so `obtain` tries to refresh rather than handing it straight back.
        let stale = StoredCredential::new(
            "zna_access".into(),
            "znr_refresh".into(),
            "node-id".into(),
            "hello node".into(),
            "allen@example.com".into(),
            now_unix() + 60,
        );
        store.save(&stale).await.unwrap();

        let obtained = enroller_against(&url, store.clone()).obtain().await.expect("kept going");

        assert_eq!(obtained, stale, "an unreachable server is not a reason to stop");
        assert_eq!(store.load().await.unwrap().as_ref(), Some(&stale));
    }

    /// A node is configured with one address; enrolling against a different deployment than it
    /// connects to is exactly the mistake this derivation exists to make impossible.
    #[test]
    fn http_base_is_derived_from_the_websocket_url() {
        assert_eq!(
            http_base("wss://attacca.example/zyris/v1/ws"),
            "https://attacca.example"
        );
        assert_eq!(http_base("ws://127.0.0.1:8080/zyris/v1/ws"), "http://127.0.0.1:8080");
        assert_eq!(http_base("https://attacca.example/"), "https://attacca.example");
        assert_eq!(
            http_base("wss://attacca.example:8443/zyris/v1/ws"),
            "https://attacca.example:8443"
        );
        // The default deployment sits behind an ingress that strips `/api`, so the prefix is part
        // of the address a node is given and has to survive into the enrollment endpoints.
        assert_eq!(
            http_base(crate::DEFAULT_SERVER_URL),
            "https://attacca.cc/api",
            "the enrollment base must keep the path prefix the websocket URL carries"
        );
    }

    #[test]
    fn the_client_hint_describes_this_machine() {
        let hint = local_client_hint();
        assert_eq!(hint.os.as_deref(), Some(std::env::consts::OS));
        assert!(hint.agent.as_deref().unwrap().starts_with("zyris/"));
    }

    #[test]
    fn an_unparseable_error_body_still_names_the_status() {
        let error = ErrorResponse {
            error: "http_503".into(),
            error_description: None,
            interval: None,
        };
        assert_eq!(describe(&error), "http_503");
    }
}
