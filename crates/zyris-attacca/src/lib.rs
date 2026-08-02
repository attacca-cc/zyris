//! `attacca_api` — the capability the server side of the connection announces.
//!
//! Every other capability in this workspace is something a *node* offers. This one runs the other
//! way: [Attacca](https://attacca.cc) reserves the name `attacca_api`, rejects it from any node
//! that tries to announce it, and announces it itself right after the handshake. A node picks the
//! client up on connect and drives agents, projects and sessions over the same websocket it serves
//! its own tools on.
//!
//! It sits in its own crate rather than in `zyris-caps` because `zyris-caps` is the catalogue of
//! things a node implements, and this is the one surface a node only ever calls.
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
    #[serde(rename = "files:read")]
    FilesRead,
    #[serde(rename = "files:write")]
    FilesWrite,
    #[serde(rename = "jobs:read")]
    JobsRead,
    #[serde(rename = "jobs:write")]
    JobsWrite,
    /// Works are scoped apart from jobs rather than folded in with them: a work owns a git
    /// integration branch and a graph of task worktrees, so granting a node the run of them is a
    /// materially larger thing to hand over than letting it queue a job.
    #[serde(rename = "works:read")]
    WorksRead,
    #[serde(rename = "works:write")]
    WorksWrite,
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
    /// Register and destroy the account's *temporary* zyris nodes — the session-scoped nodes a
    /// program like zyris-code creates per coding agent. Ephemeral only: the server clamps their
    /// lifetime and idle TTL and sweeps them, so this scope cannot mint a permanent node.
    #[serde(rename = "nodes:write")]
    NodesWrite,
}

impl ZScope {
    /// Every scope, in the order Attacca lists them. Useful for a node that wants to ask for
    /// everything and let the approving user cut it down.
    pub const ALL: [ZScope; 18] = [
        ZScope::AgentsRead,
        ZScope::AgentsWrite,
        ZScope::ProjectsRead,
        ZScope::ProjectsWrite,
        ZScope::SessionsRead,
        ZScope::SessionsWrite,
        ZScope::FilesRead,
        ZScope::FilesWrite,
        ZScope::JobsRead,
        ZScope::JobsWrite,
        ZScope::WorksRead,
        ZScope::WorksWrite,
        ZScope::ArtifactsRead,
        ZScope::ArtifactsWrite,
        ZScope::KanbanRead,
        ZScope::KanbanWrite,
        ZScope::EventsRead,
        ZScope::NodesWrite,
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
            ZScope::FilesRead => "files:read",
            ZScope::FilesWrite => "files:write",
            ZScope::JobsRead => "jobs:read",
            ZScope::JobsWrite => "jobs:write",
            ZScope::WorksRead => "works:read",
            ZScope::WorksWrite => "works:write",
            ZScope::ArtifactsRead => "artifacts:read",
            ZScope::ArtifactsWrite => "artifacts:write",
            ZScope::KanbanRead => "kanban:read",
            ZScope::KanbanWrite => "kanban:write",
            ZScope::EventsRead => "events:read",
            ZScope::NodesWrite => "nodes:write",
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

/// What [`AttaccaApi::create_project`] takes. A project is a folder, not a workspace: nothing runs
/// in it, so there is nothing to configure beyond its name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZNewProject {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A partial project edit: an omitted field is left alone.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZProjectUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Three-valued, which is why it is doubly wrapped: omitted leaves the description as it is, an
    /// explicit `null` clears it, and a string replaces it. A single `Option` would collapse the
    /// first two together and leave a node no way to remove a description it once set.
    #[serde(default, deserialize_with = "deserialize_some", skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
}

/// Forces an explicit JSON `null` to deserialize as `Some(None)` rather than collapsing into the
/// same `None` a missing key produces.
fn deserialize_some<'de, T, D>(deserializer: D) -> Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    T::deserialize(deserializer).map(Some)
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

/// Where a job is in its life. Derived by Attacca from the job's session rather than set by the
/// caller, so this is a thing to read and not a thing to write — [`ZJobUpdate`] has no state field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ZJobState {
    Backlog,
    Processing,
    Done,
    /// The agent asked a question and is waiting on an answer — send one with `send_message` to the
    /// job's `session_id`.
    Ambiguous,
    Failed,
    Recurring,
    /// A plan-mode job that has produced a plan and is waiting for it to be approved.
    AwaitingPlan,
}

/// One item of autonomous work: a message handed to an agent that runs it to completion on its own
/// session, reporting long results as artifacts. The unit a node reaches for when it wants something
/// done rather than a conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZJob {
    pub id: String,
    /// Titled in the background from the opening message, so a job read back immediately still
    /// carries Attacca's placeholder.
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub state: ZJobState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// The session driving the job. Pass it to `send_message` to answer a question, or to
    /// `turn_events` to watch the work happen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// A one-line "what the agent is doing right now", so a node can show progress without opening
    /// the event stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity: Option<String>,
    /// Why the job ended, set only in [`ZJobState::Failed`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    /// A planning job: it drafts a plan and turns it into a [`ZWork`] rather than doing the work
    /// itself.
    #[serde(default)]
    pub planning: bool,
    /// RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// What [`AttaccaApi::create_job`] takes. Creating a job starts it: the first turn is enqueued
/// before this call returns, so the job comes back already `Processing`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZNewJob {
    /// What the agent is being asked to do. Also what the job is titled from.
    pub message: String,
    /// Omit to run the job on the account's Main Agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Omit to file the job under the account's default project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// IANA name, e.g. `Europe/Berlin`. What the agent answers "what time is it" with; omitting it
    /// leaves the deployment's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// Draft a plan and turn it into a [`ZWork`] instead of doing the work here.
    #[serde(default)]
    pub planning: bool,
    /// Investigate, hand back a plan, and wait for approval before executing — all within this one
    /// job. Distinct from `planning`, which hands off to a work.
    #[serde(default)]
    pub plan_mode: bool,
    /// Attachments for the opening message, the same way `send_message` takes them. Text datums are
    /// folded into `message`; files land in the account's workspace.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data: Vec<Datum>,
}

/// A partial job edit: an omitted field is left alone. State is derived and cannot be set here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZJobUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZJobFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Where a work is in its life. Like [`ZJobState`], derived rather than set — but two of these are
/// gates rather than progress: [`ZWorkState::AwaitingGoalApproval`] and
/// [`ZWorkState::AwaitingPlanApproval`] are where the loop stops and waits to be let through, with
/// `approve_work_goal` and `approve_work_plan`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ZWorkState {
    Draft,
    CheckingRequirements,
    /// Gate 1: the goal is drafted and needs approving before planning starts.
    AwaitingGoalApproval,
    Planning,
    /// Gate 2: the task graph is drafted and needs approving before anything executes.
    AwaitingPlanApproval,
    Executing,
    /// A phase finished that was set to stop for review. `continue_work` resumes.
    Halted,
    Verifying,
    Done,
    Failed,
    Cancelled,
}

/// A larger unit than a job: a goal that is planned into a graph of tasks, each executed in its own
/// git worktree and merged into a shared integration branch. A work pauses twice for approval, which
/// is why creating one is not enough to make it run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZWork {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub state: ZWorkState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// The planning conversation. This is an ordinary session id: `send_message` to argue with the
    /// plan, `turn_events` to watch it being drawn up, `work_message` to do the former without
    /// having to read this field first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planner_session_id: Option<String>,
    /// The measurable success criterion, settled at gate 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_goal: Option<String>,
    /// Blockers found up front — a missing toolchain, a login the agent cannot perform.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requirements_report: Option<String>,
    /// The git branch the task worktrees merge into. Set once execution starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integration_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity: Option<String>,
    /// RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// What [`AttaccaApi::create_work`] takes. Creating a work starts its planning turns; it then stops
/// at `awaiting_goal_approval` and stays there until `approve_work_goal` is called.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZNewWork {
    /// The goal, in prose. The work's title is derived from its first line.
    pub message: String,
    /// Omit to plan the work with the account's Main Agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Omit to file the work under the account's default project. A work's tasks run against the
    /// project's checkout, so this decides what the work is allowed to change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

/// A partial work edit: an omitted field is left alone. State is derived and cannot be set here —
/// use the approval and stop/continue tools to move a work along.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZWorkUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZWorkFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Where one task in a work's graph has got to. `creating`/`verifying`/`recording` name the agent
/// role currently holding it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ZTaskState {
    Pending,
    /// An upstream task failed, so this one cannot start.
    Blocked,
    /// The agent asked a question and is waiting on an answer in the task's own session.
    AwaitingInput,
    Creating,
    SelfVerifying,
    Verifying,
    Recording,
    Merging,
    Done,
    Failed,
    /// The merge into the integration branch hit a conflict.
    Conflicted,
}

/// One node of a work's task graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZTask {
    pub id: String,
    pub work_id: String,
    pub title: String,
    pub description: String,
    pub state: ZTaskState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_id: Option<String>,
    /// The session doing the work. An ordinary session id, so `turn_events` follows one task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_session_id: Option<String>,
    /// The task's own git branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// `[{ "text": String, "met": bool }]` — the checks the task has to satisfy. Passed through as
    /// JSON rather than typed, because it is the planner's shape and not the protocol's.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub micro_measurables: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity: Option<String>,
    /// RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// An edge: `downstream_task_id` may not start until `upstream_task_id` is done. Two tasks with no
/// edge between them run in parallel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZTaskDep {
    pub upstream_task_id: String,
    pub downstream_task_id: String,
}

/// A work's plan, as a graph: [`AttaccaApi::work_tasks`]'s answer. Empty until the work has been
/// through `planning`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZWorkTasks {
    #[serde(default)]
    pub tasks: Vec<ZTask>,
    #[serde(default)]
    pub deps: Vec<ZTaskDep>,
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

/// What a node or API key asks for when registering a temporary node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZNewTempNode {
    /// Display name; the server slugs it for the tool namespace like any node's.
    pub name: String,
    /// Platform label; `linux` when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// Scopes the temp node may carry. The server clamps these to the caller's own grant.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    /// Seconds the node may go without activity before the server destroys it. Clamped server-side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_ttl_secs: Option<u64>,
    /// Absolute lifetime cap in seconds. Clamped server-side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_lifetime_secs: Option<u64>,
}

/// A registered temporary node, as the server reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZTempNode {
    pub node_id: String,
    pub name: String,
    pub slug: String,
    /// The node that registered this one, when it came from `create_temp_node` over `attacca_api`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_node_id: Option<String>,
    /// One-time plaintext node token. Present only on the create response — `list_temp_nodes`
    /// never carries it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// RFC 3339 absolute lifetime cap; the server destroys the node past this.
    pub expires_at: String,
    /// Seconds of allowed inactivity before the server destroys the node.
    pub idle_ttl_secs: u64,
    pub connected: bool,
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

    /// Read one project.
    async fn get_project(&self, project_id: String) -> zyris::Result<ZProject>;

    /// Create a project.
    async fn create_project(&self, project: ZNewProject) -> zyris::Result<ZProject>;

    /// Edit a project's name or description. Omitted fields are left alone.
    async fn update_project(
        &self,
        project_id: String,
        update: ZProjectUpdate,
    ) -> zyris::Result<ZProject>;

    /// Delete a project. Refused while it still holds sessions, boards or jobs, and refused outright
    /// for the account's default project — move or delete the contents first.
    async fn delete_project(&self, project_id: String) -> zyris::Result<()>;

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

    /// List the caller's jobs, newest first.
    async fn list_jobs(&self, filter: ZJobFilter) -> zyris::Result<Vec<ZJob>>;

    /// Read one job.
    async fn get_job(&self, job_id: String) -> zyris::Result<ZJob>;

    /// Queue a job and start it. The returned job is already running; watch it with `turn_events` on
    /// its `session_id`, or poll `get_job` for its state.
    async fn create_job(&self, job: ZNewJob) -> zyris::Result<ZJob>;

    /// Edit a job's title, description or project. Omitted fields are left alone.
    async fn update_job(&self, job_id: String, update: ZJobUpdate) -> zyris::Result<ZJob>;

    /// Delete a job.
    async fn delete_job(&self, job_id: String) -> zyris::Result<()>;

    /// List the caller's works, newest first.
    async fn list_works(&self, filter: ZWorkFilter) -> zyris::Result<Vec<ZWork>>;

    /// Read one work.
    async fn get_work(&self, work_id: String) -> zyris::Result<ZWork>;

    /// Create a work and start planning it. It will settle at `awaiting_goal_approval` and go no
    /// further on its own — a node that creates a work and walks away has created something that
    /// never runs. See `approve_work_goal`.
    async fn create_work(&self, work: ZNewWork) -> zyris::Result<ZWork>;

    /// Edit a work's title, description or project. Omitted fields are left alone.
    async fn update_work(&self, work_id: String, update: ZWorkUpdate) -> zyris::Result<ZWork>;

    /// Delete a work.
    async fn delete_work(&self, work_id: String) -> zyris::Result<()>;

    /// Let a work through gate 1: accept the goal it drafted and start planning the task graph.
    /// Read `final_goal` and `requirements_report` off the work first — that is what is being
    /// approved, and `work_message` is how to ask for changes instead.
    async fn approve_work_goal(&self, work_id: String) -> zyris::Result<ZWork>;

    /// Let a work through gate 2: accept the task graph and begin executing it. Read it first with
    /// `work_tasks`.
    async fn approve_work_plan(&self, work_id: String) -> zyris::Result<ZWork>;

    /// A work's task graph. Empty before planning has run.
    async fn work_tasks(&self, work_id: String) -> zyris::Result<ZWorkTasks>;

    /// Stop a running work, and every task under it.
    async fn stop_work(&self, work_id: String) -> zyris::Result<()>;

    /// Resume a stopped, halted or failed work from where it left off: the tasks that already
    /// finished are kept, the rest are re-run.
    async fn continue_work(&self, work_id: String) -> zyris::Result<ZWork>;

    /// Say something to a work's planner — to steer the goal at gate 1 or the plan at gate 2.
    /// Exactly `send_message` against the work's `planner_session_id`, spelled so a node does not
    /// have to read the work first, and streamed back the same way.
    async fn work_message(
        &self,
        work_id: String,
        message: String,
        data: Vec<Datum>,
    ) -> zyris::Result<()>;

    /// Register a dedicated temporary node under this account — one coding agent of a
    /// session-scoped program like zyris-code. The node dials like any static-token node, but the
    /// server destroys it once it has been idle for `idle_ttl_secs` or after `max_lifetime_secs`,
    /// whichever comes first; the returned token is shown once and never retrievable again.
    ///
    /// Requires the `nodes:write` scope, and the temp node's scopes are clamped to this node's own
    /// grant — it cannot be minted with more power than its creator holds.
    async fn create_temp_node(&self, request: ZNewTempNode) -> zyris::Result<ZTempNode>;

    /// Destroy a temporary node this account created — the explicit end-of-session cleanup, which
    /// closes its socket immediately rather than waiting for its idle TTL. Requires `nodes:write`.
    async fn destroy_temp_node(&self, node_id: String) -> zyris::Result<()>;

    /// List the account's temporary nodes. Requires `nodes:write`.
    async fn list_temp_nodes(&self) -> zyris::Result<Vec<ZTempNode>>;

    /// Live turn feed with cursor resume: the head carries the current running flag and
    /// last cursor; items mirror LiveFrame (durable events with cursor, deltas, status).
    #[zyris(uni_stream)]
    async fn turn_events(
        &self,
        session_id: String,
        after: Option<i64>,
    ) -> zyris::Result<Streaming<ZTurnStatus, ZTurnFrame>>;
}
