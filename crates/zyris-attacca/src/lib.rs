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
    /// The account's billing plan, as the deployment names it. Absent on a deployment that does not
    /// meter, or one that predates the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// Credit balance, formatted by the deployment rather than parsed here — the unit and the
    /// precision are its business, and a node only ever displays this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits: Option<String>,
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
    /// A session created without a title reads back under Attacca's placeholder, not as absent —
    /// the title agent replaces it once the session has a first message to name it from, so a
    /// title seen here is only final for a session that has already taken a turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default)]
    pub running: bool,
    /// The session's own system instructions, appended to its agent's for every turn. See
    /// [`ZNewSession::preamble`]. Absent on a deployment that predates the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preamble: Option<String>,
}

/// What [`AttaccaApi::create_session_with`] takes: everything [`AttaccaApi::create_session`]'s three
/// arguments carry, plus the options added since. A struct rather than more arguments because the
/// generated request struct has no per-field default — a new *argument* on an existing tool is a
/// decode error for every node built before it, while a new field on a struct whose fields are all
/// `#[serde(default)]` is not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZNewSession {
    pub agent_id: String,
    /// Prefer leaving this unset. Attacca titles a session from its first message, so an untitled
    /// session gets a real name the moment it is used, in the language that message was written in.
    /// Set it only for a session a human will go looking for under a name the node already knows —
    /// a title given here is permanent, and opts the session out of that auto-titling for good.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Omit to file the session under the account's default project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// System instructions for this session alone, appended to the agent's own preamble on every
    /// turn — the agent keeps its identity, tools and skills, and this narrows what it is doing
    /// here. Fixed for the session's lifetime; a node wanting different instructions opens a
    /// different session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preamble: Option<String>,
}

/// The window [`AttaccaApi::session_history`] reads. Every field defaults, so the whole timeline is
/// `ZHistoryQuery::default()`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZHistoryQuery {
    /// Return only entries past this cursor, exclusive — the `cursor` of the last entry already
    /// seen, from either this tool or `turn_events`. Omit for the whole timeline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<i64>,
    /// At most this many entries, taken oldest-first from `after`. Omit for everything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// What one session has cost so far: [`AttaccaApi::session_usage`]'s answer.
///
/// Every field is optional and defaults, so a deployment reports what it actually meters and a node
/// displays what it is given. `Default` is a legitimate response — it means "metered nothing" —
/// which is why absence is spelled per-field rather than by the call failing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZUsage {
    /// The model the session's turns ran on, as the deployment names it. A session that has taken
    /// no turn has none yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Tokens currently in context — what the next turn starts from, not a running total.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<i64>,
    /// Credits this session has consumed. A string for the same reason as [`ZMe::credits`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits_used: Option<String>,
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
    /// RFC 3339. Absent on a deployment that predates the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
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

    /// Create a session. Kept for nodes built before `create_session_with`, which is the same call
    /// with room for the options added since.
    ///
    /// Pass `title` as null unless a human will go looking for this session under a name the node
    /// already knows: Attacca titles a session from its first message, and a title given here is
    /// permanent and suppresses that.
    async fn create_session(
        &self,
        agent_id: String,
        title: Option<String>,
        project_id: Option<String>,
    ) -> zyris::Result<ZSession>;

    /// Create a session, with options — most usefully a `preamble`, which gives this session its own
    /// system instructions on top of its agent's. Leave `title` unset unless the node has a name
    /// worth pinning; Attacca titles the session from its first message otherwise.
    async fn create_session_with(&self, session: ZNewSession) -> zyris::Result<ZSession>;

    /// A session's durable timeline, oldest-first: the same events `turn_events` streams, read back
    /// as a list, so a node that was not connected when they happened can still see them. Mind the
    /// one difference in `after`: omitting it here means the whole history, where in `turn_events`
    /// it means live frames only.
    async fn session_history(
        &self,
        session_id: String,
        query: ZHistoryQuery,
    ) -> zyris::Result<Vec<ZSessionEvent>>;

    /// What a session has cost so far: model, token counts, credits. Requires `sessions:read`.
    ///
    /// Added within version 1, so a node built against this crate may find it absent from an older
    /// deployment's announcement and get `capability_not_announced` back. Treat that as "this
    /// deployment does not meter" and carry on — it is not a connection-level failure.
    async fn session_usage(&self, session_id: String) -> zyris::Result<ZUsage>;

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
