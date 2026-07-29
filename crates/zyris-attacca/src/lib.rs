//! `attacca_api` — the capability the server side of the connection announces.
//!
//! Every other capability in this workspace is something a *node* offers. This one runs the other
//! way: [Attacca](https://attacca.cc) reserves the name `attacca_api`, rejects it from any node
//! that tries to announce it, and announces it itself right after the handshake — filtered to the
//! tools the node's scopes permit. A node picks the client up on connect and drives agents and
//! sessions over the same websocket it serves its own tools on.
//!
//! It sits in its own crate rather than in `zyris-caps` because `zyris-caps` is the catalogue of
//! things a node implements, and because that crate's default features build a PTY. A node that
//! only wants to call back into Attacca should not compile `portable-pty` to do it.
//!
//! `zyris-hello` consumes it this way, in two lines of imports. Depending on the crate is still a
//! convenience rather than a requirement: matching is by `(name, version)` and the announced tool
//! list is never compared, so a node may instead declare just the slice it calls with its own
//! `#[zyris::capability]` trait — which is what consuming any capability nobody has published a
//! crate for looks like.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zyris::{Datum, Streaming};

/// The capability name Attacca reserves to itself on every connection.
pub const ATTACCA_API_CAPABILITY: &str = "attacca_api";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZAgent {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZNewAgent {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZSession {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default)]
    pub running: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZSessionFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZTurnStatus {
    pub session_id: String,
    pub running: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_cursor: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ZDeltaKind {
    Assistant,
    Reasoning,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZSessionEvent {
    pub seq: i64,
    pub cursor: i64,
    pub kind: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ZTurnFrame {
    Event { cursor: i64, event: ZSessionEvent },
    Delta { kind: ZDeltaKind, text: String },
    Status { running: bool },
}

#[zyris::capability(name = "attacca_api", version = 1)]
pub trait AttaccaApi {
    /// List the caller's agents.
    async fn list_agents(&self) -> zyris::Result<Vec<ZAgent>>;

    /// Create an agent.
    async fn create_agent(&self, agent: ZNewAgent) -> zyris::Result<ZAgent>;

    /// List the caller's sessions.
    async fn list_sessions(&self, filter: ZSessionFilter) -> zyris::Result<Vec<ZSession>>;

    /// Create a session.
    async fn create_session(
        &self,
        agent_id: String,
        title: Option<String>,
        project_id: Option<String>,
    ) -> zyris::Result<ZSession>;

    /// Post a message, starting a turn. Stream results via `turn_events`.
    async fn send_message(
        &self,
        session_id: String,
        message: String,
        data: Vec<Datum>,
    ) -> zyris::Result<()>;

    /// Stop the running turn on a session.
    async fn cancel_turn(&self, session_id: String) -> zyris::Result<()>;

    /// Live turn feed with cursor resume: the head carries the current running flag and
    /// last cursor; items mirror LiveFrame (durable events with cursor, deltas, status).
    #[zyris(uni_stream)]
    async fn turn_events(
        &self,
        session_id: String,
        after: Option<i64>,
    ) -> zyris::Result<Streaming<ZTurnStatus, ZTurnFrame>>;
}
