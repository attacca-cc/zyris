//! `attacca_api` — the capability the server side of the connection announces.
//!
//! Every other capability in this workspace is something a *node* offers. This one runs the other
//! way: [Attacca](https://attacca.cc) reserves the name `attacca_api`, rejects it from any node
//! that tries to announce it, and announces it itself right after the handshake. A node picks the
//! client up on connect and drives agents, projects and sessions over the same websocket it serves
//! its own tools on.
//!
//! It sits in its own crate rather than in `zyris-caps` because `zyris-caps` is the catalogue of
//! things a node implements, and because that crate's default features build a PTY. A node that
//! only wants to call back into Attacca should not compile `portable-pty` to do it.
//!
//! Tools are added within version 1 rather than by bumping it: additive tool changes inside a
//! version are permitted, and consumers discover tools by descriptor. An older node keeps working
//! because it never asks for the new ones; a newer node against an older deployment finds the tool
//! absent from the announcement and gets `capability_not_announced` if it calls anyway.
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

/// What a node's grant may contain — Attacca's scope vocabulary, spelled the way the wire spells
/// it. A node asks for a subset at enrollment (`Runner::request_scopes`, or `$ZYRIS_SCOPES`), the
/// approving user may grant fewer, and `me` reports what was actually granted.
///
/// Every `attacca_api` tool but `me` is scope-checked at call time. The descriptor lists them all
/// regardless of the grant, so a tool being announced is not permission to call it: a node that
/// wants to know what it may do calls `me` and reads [`ZMe::scopes`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum ZScope {
    #[serde(rename = "agents:read")]
    AgentsRead,
    #[serde(rename = "agents:write")]
    AgentsWrite,
    #[serde(rename = "projects:read")]
    ProjectsRead,
    #[serde(rename = "projects:write")]
    ProjectsWrite,
    #[serde(rename = "sessions:read")]
    SessionsRead,
    #[serde(rename = "sessions:write")]
    SessionsWrite,
    #[serde(rename = "jobs:read")]
    JobsRead,
    #[serde(rename = "jobs:write")]
    JobsWrite,
    #[serde(rename = "artifacts:read")]
    ArtifactsRead,
    #[serde(rename = "artifacts:write")]
    ArtifactsWrite,
    #[serde(rename = "kanban:read")]
    KanbanRead,
    #[serde(rename = "kanban:write")]
    KanbanWrite,
    /// The account-wide event stream, including `turn_events`. One scope rather than the union of
    /// every read scope: a partially-scoped stream that silently omits frame kinds is worse than no
    /// stream, because a caller cannot tell "nothing happened" from "you weren't allowed to see it".
    #[serde(rename = "events:read")]
    EventsRead,
}

impl ZScope {
    /// Every scope, in the order Attacca lists them. Useful for a node that wants to ask for
    /// everything and let the approving user cut it down.
    pub const ALL: [ZScope; 13] = [
        ZScope::AgentsRead,
        ZScope::AgentsWrite,
        ZScope::ProjectsRead,
        ZScope::ProjectsWrite,
        ZScope::SessionsRead,
        ZScope::SessionsWrite,
        ZScope::JobsRead,
        ZScope::JobsWrite,
        ZScope::ArtifactsRead,
        ZScope::ArtifactsWrite,
        ZScope::KanbanRead,
        ZScope::KanbanWrite,
        ZScope::EventsRead,
    ];

    /// The wire spelling, which is also what `request_scopes` and `$ZYRIS_SCOPES` take.
    pub fn as_str(self) -> &'static str {
        match self {
            ZScope::AgentsRead => "agents:read",
            ZScope::AgentsWrite => "agents:write",
            ZScope::ProjectsRead => "projects:read",
            ZScope::ProjectsWrite => "projects:write",
            ZScope::SessionsRead => "sessions:read",
            ZScope::SessionsWrite => "sessions:write",
            ZScope::JobsRead => "jobs:read",
            ZScope::JobsWrite => "jobs:write",
            ZScope::ArtifactsRead => "artifacts:read",
            ZScope::ArtifactsWrite => "artifacts:write",
            ZScope::KanbanRead => "kanban:read",
            ZScope::KanbanWrite => "kanban:write",
            ZScope::EventsRead => "events:read",
        }
    }

    /// The inverse of [`ZScope::as_str`]. Returns `None` for a scope this crate does not know,
    /// which is what a *newer* deployment granting a *newer* scope looks like from here.
    pub fn from_str(s: &str) -> Option<ZScope> {
        ZScope::ALL.into_iter().find(|scope| scope.as_str() == s)
    }
}

impl std::fmt::Display for ZScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Who the connection is authorized as, and what it may do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZMe {
    pub user_id: String,
    pub email: String,
    pub display_name: String,
    /// The grant, as the wire spells it. Strings rather than [`ZScope`] because a deployment may
    /// grant a scope this crate predates, and a `me` call that fails to deserialize is a worse
    /// answer than one naming a scope the caller does not recognize. See [`ZMe::known_scopes`].
    #[serde(default)]
    pub scopes: Vec<String>,
}

impl ZMe {
    /// The granted scopes this crate knows, unknown ones dropped.
    pub fn known_scopes(&self) -> Vec<ZScope> {
        self.scopes.iter().filter_map(|s| ZScope::from_str(s)).collect()
    }

    /// Whether the grant contains `scope`. Cheaper and more honest than `known_scopes().contains`:
    /// it compares wire spellings, so it is unaffected by scopes this crate predates.
    pub fn has(&self, scope: ZScope) -> bool {
        self.scopes.iter().any(|s| s == scope.as_str())
    }
}

/// A project: the account-level folder a session, board, or job belongs to. `ZSession.project_id`
/// names one of these.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZProject {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The account's undeletable fallback project. Resources created without a project land here,
    /// and exactly one project per account has this set.
    #[serde(default)]
    pub is_default: bool,
}

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
    /// Identify the account this connection is authorized as, and the scopes it was granted.
    /// Requires no scope of its own, so it answers even for a node granted nothing.
    async fn me(&self) -> zyris::Result<ZMe>;

    /// List the caller's agents.
    async fn list_agents(&self) -> zyris::Result<Vec<ZAgent>>;

    /// Create an agent.
    async fn create_agent(&self, agent: ZNewAgent) -> zyris::Result<ZAgent>;

    /// List the caller's projects: the default first, then the rest oldest-first. The default is
    /// created on demand, so this never comes back empty.
    async fn list_projects(&self) -> zyris::Result<Vec<ZProject>>;

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
