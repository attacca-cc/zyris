//! An MCP server's tool list, as a capability an agent on Attacca can call.
//!
//! One [`Promoted`] per server. Its [`ServeCapability`] implementation is the whole translation:
//! a `tools/list` answer becomes a [`CapabilityDescriptor`], and an [`IncomingCall`] becomes a
//! `tools/call`.
//!
//! Three decisions are made here rather than left to a caller, because each of them has exactly
//! one right answer for the whole machine and a per-caller answer would be a way for two servers
//! to be announced under different rules. They are [the capability's
//! name](#the-name-is-always-prefixed), [what happens to a tool that will not
//! translate](#what-actually-fails-is-names-not-schemas), and [what `isError: true`
//! becomes](#a-tool-that-ran-and-refused-is-an-answer).
//!
//! # The name is always prefixed
//!
//! A server called `notes` becomes the capability `mcp_notes`. Always — not only when the bare
//! name would collide with one of the five this machine announces itself (`terminal`, `file_io`,
//! `input`, `screen_capture`, `file_transfer`).
//!
//! Prefixing only on collision was the alternative, and it fails on the same question twice:
//! **can a capability's name change because of something that has nothing to do with it?**
//!
//! - Two of the five built-ins are conditional. `zyris-tools`'s `announce.rs` announces `input`
//!   and `screen_capture` only when a display server answers, so on a headless host they are not
//!   there to collide with. The same configuration file would then name a server's capability
//!   `input` on that machine and `mcp_input` on a desktop — one config, two protocols, and an
//!   agent that learned one machine cannot address the other.
//! - `file_transfer` is conditional too, on an endpoint that bound.
//! - And a sixth built-in added later would rename an MCP server's capability out from under an
//!   agent that had already learned `notes.search`.
//!
//! Prefixing always is also **structurally** safe rather than safe by vigilance: nothing has to
//! consult a list of built-in names, so nobody has to remember to update that list. The single
//! standing condition is the inverse and it is asserted in this crate's tests — no built-in name
//! may begin with [`CAPABILITY_PREFIX`].
//!
//! Plain concatenation, so it is injective: two different servers can never produce one
//! capability name, and a server that has already guessed the scheme and called itself
//! `mcp_terminal` gets `mcp_mcp_terminal` rather than colliding with a server called `terminal`.
//! A collision would not be a quiet problem — `zyris-core`'s `Served::build` refuses a duplicate
//! `(name, version)` and the whole node then announces *nothing* — which is the other reason not
//! to leave it to a rule with exceptions in it.
//!
//! # What actually fails is names, not schemas
//!
//! The plan expected "a tool whose schema will not translate" to be the case needing a decision.
//! **There is no such case, and this was checked rather than assumed.** `rmcp`'s
//! `Tool::input_schema` is an `Arc<JsonObject>` and `schema_as_json_value()` is
//! `Value::Object(clone)` — infallible, always an object, and a `ToolDescriptor::request_schema`
//! is a `serde_json::Value`, so it goes in untouched. An absent schema is not a special case
//! either: it arrives as `{}`, which is the JSON Schema meaning "anything".
//!
//! A schema that is *not* an object never reaches this file at all. A server that sends
//! `"inputSchema": true` — valid JSON Schema, the one that means "anything" — makes `rmcp` fail
//! to deserialize the whole `tools/list` response, so [`Server::spawn`] fails and the server is
//! absent with its reason logged. One bad tool takes every tool on that server with it; that is
//! upstream's shape, not a decision made here, and `promoted_tools.rs` pins it so nobody looks
//! for the code path here.
//!
//! What can genuinely fail is two things, both about names:
//!
//! - **A server name that will not make a routable capability name.** The protocol's method is
//!   `capability.tool`, and `zyris_proto::split_method` splits at the **first** dot — so a server
//!   called `my.notes` would announce `mcp_my.notes`, and every call to it would be addressed to
//!   a capability called `mcp_my` that does not exist. [`Promoted::new`] refuses it and says so;
//!   the server is not promoted and the others are unaffected. An empty name is refused for the
//!   same reason in a different shape: `mcp_` names nothing a person can talk about.
//! - **The same tool name twice inside one server.** Nothing in MCP forbids it. The second is
//!   unreachable by construction — `CapabilityDescriptor::tool` takes the first, and so does
//!   [`Promoted::dispatch`] — so announcing it would put a schema in front of an agent that no
//!   call can ever reach. The first is kept, the rest are dropped, and each drop is recorded in
//!   [`Promoted::dropped`] so the log and the window can say which and why. **Silence is the one
//!   answer the plan rules out**, and an agent cannot be told mid-announcement, so the person can.
//!
//! Everything else a server name can be — uppercase, spaces, hundreds of characters, a duplicate
//! of another server's — is carried or is not this file's problem. A capability name is an opaque
//! string on the wire; the protocol imposes no charset and no length. A duplicate server name is
//! a configuration question and belongs to whatever reads the configuration, which is the only
//! place that can tell a person which of the two entries to change.
//!
//! # A tool that ran and refused is an answer
//!
//! MCP says a tool that failed is a *successful* response carrying `isError: true` and the reason
//! in `content`; only a call that never happened is a protocol error. That line is kept: a
//! refusal comes back as [`Outgoing::Response`] with the server's own result in it.
//!
//! The reason is that the three outcomes an agent has to tell apart then have three different
//! **shapes**, and none of them needs a string parsed to be recognised:
//!
//! | What happened | What the agent gets |
//! |---|---|
//! | the tool ran and refused | `Ok`, with `isError: true` and the server's reason |
//! | the tool does not exist | `Err`, [`ErrorCode::MethodNotFound`] |
//! | the server is gone | `Err`, [`ErrorCode::CapabilityUnavailable`] |
//!
//! Turning `isError` into a `WireError` would collapse the first row into the other two and throw
//! away the reason, which is the one thing the agent needs to do something else instead.
//!
//! "The tool does not exist" is decided here, against this capability's own descriptor, before
//! the server is asked. That is what makes it a `MethodNotFound` rather than whatever wording the
//! server would have chosen, and it costs no round trip.
//!
//! **The cost, named rather than hidden: `Guarded` records a refusal as `Allowed`.** Its outcome
//! is `Ok`/`Err` from this function, and `Allowed` there means "the switch let the call through",
//! which is exactly what happened. A log that said `Failed` would be reporting on the server's
//! answer, which it does not read.
//!
//! # The audit line carries no arguments
//!
//! `zyris-tools`'s `summarize` writes an allowlist of parameter names — `path`, `command`, `pty`
//! and the rest — chosen by reasoning about what those five capabilities' parameters *mean*. None
//! of that reasoning transfers to a server somebody installed: a promoted tool's arguments are
//! arbitrary JSON, and a field called `path` or `command` in one of them is a coincidence of
//! spelling, not the same fact. So `Guarded` writes no detail at all for a capability whose name
//! begins with [`CAPABILITY_PREFIX`] — see `crates/zyris-tools/src/guarded.rs`, where the rule
//! and its tests live. Widening the allowlist to catch MCP arguments would be the opposite move
//! and is ruled out: a server's arguments can be anything, a password included.

use std::sync::Arc;

use serde_json::{Map, Value, json};
use zyris::{
    CapabilityDescriptor, ErrorCode, IncomingCall, Outgoing, ServeCapability, ToolDescriptor,
    Transfer, WireError, encode_response, unknown_tool,
};

use crate::server::{Server, is_disconnected};

/// What every promoted capability's name begins with.
///
/// See [the module documentation](self#the-name-is-always-prefixed). The one thing that has to
/// stay true of it is that no built-in capability name starts with it; `promoted_tools.rs`
/// asserts that against the list `zyris-tools`'s `announce.rs` produces.
pub const CAPABILITY_PREFIX: &str = "mcp_";

/// The version every promoted capability announces.
///
/// One, and it does not move with the server's own version. `(name, version)` is what
/// `zyris-core` dedupes announcements on and what an agent pins a capability by, so a number that
/// changed when somebody upgraded an MCP server would make the same tools look like a different
/// capability. MCP's `serverInfo.version` is a free-form string and belongs in the window, not
/// here. The number changes when *this* translation changes.
pub const PROMOTED_VERSION: u32 = 1;

/// One tool the server offered and this machine did not announce, with the reason.
///
/// Exists so that dropping is never silent. Nothing on the wire can carry it — an agent sees a
/// tool list, not a list of absences — so it is kept for the log and for the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DroppedTool {
    /// The name the server used.
    pub name: String,
    /// Why it was not announced, in words a person can act on.
    pub reason: String,
}

/// One MCP server, announced as a capability.
///
/// Built once, from a [`Server`] that has already started and reported its tools. The descriptor
/// is assembled here and then only cloned: `zyris-core` asks for it on every announce and
/// `Guarded` asks for it when it is built, and neither should re-walk a tool list to get it.
pub struct Promoted {
    server: Arc<Server>,
    descriptor: CapabilityDescriptor,
    dropped: Vec<DroppedTool>,
}

impl Promoted {
    /// Promote a running server.
    ///
    /// Fails only when the server's name will not make a capability name an agent can address —
    /// see [the module documentation](self#what-actually-fails-is-names-not-schemas). A tool that
    /// cannot be announced does not fail this; it is dropped and reported by [`Self::dropped`].
    pub fn new(server: Arc<Server>) -> anyhow::Result<Promoted> {
        let name = capability_name(server.name())?;
        let (tools, dropped) = translate(server.name(), server.tools());

        if !dropped.is_empty() {
            tracing::warn!(
                server = server.name(),
                capability = name,
                dropped = dropped.len(),
                "some of an MCP server's tools are not announced"
            );
            for tool in &dropped {
                tracing::warn!(
                    server = server.name(),
                    tool = tool.name,
                    reason = tool.reason,
                    "an MCP tool is not announced"
                );
            }
        }
        tracing::info!(
            server = server.name(),
            capability = name,
            tools = tools.len(),
            "an MCP server is promoted"
        );

        Ok(Promoted {
            descriptor: CapabilityDescriptor { name, version: PROMOTED_VERSION, tools },
            dropped,
            server,
        })
    }

    /// The capability name this announces under — the server's name, prefixed.
    pub fn name(&self) -> &str {
        &self.descriptor.name
    }

    /// The server underneath, so whatever owns this can still ask after its health.
    pub fn server(&self) -> &Arc<Server> {
        &self.server
    }

    /// The tools the server offered that this machine did not announce, and why.
    ///
    /// Empty for every well-formed server, which is the ordinary case.
    pub fn dropped(&self) -> &[DroppedTool] {
        &self.dropped
    }
}

/// The capability name and two counts, and nothing else.
///
/// Not derived, for the same reason [`Server`]'s is not: the descriptor holds every tool's JSON
/// schema, and a derived `Debug` would put all of it into any log line or panic message that
/// formats one.
impl std::fmt::Debug for Promoted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Promoted")
            .field("capability", &self.descriptor.name)
            .field("tools", &self.descriptor.tools.len())
            .field("dropped", &self.dropped.len())
            .finish_non_exhaustive()
    }
}

#[zyris::async_trait]
impl ServeCapability for Promoted {
    fn descriptor(&self) -> CapabilityDescriptor {
        self.descriptor.clone()
    }

    async fn dispatch(&self, call: IncomingCall) -> zyris::Result<Outgoing> {
        // Answered from this capability's own descriptor rather than by asking the server, so
        // that "there is no such tool" is the protocol's own `MethodNotFound` and not whatever
        // wording a server would have chosen for it. It also means a tool dropped above is
        // refused the same way as one the server never had, which is the truthful answer: it is
        // not announced, so it does not exist on this connection.
        if self.descriptor.tool(&call.tool).is_none() {
            return Err(unknown_tool(&self.descriptor.name, &call.tool));
        }

        let arguments = arguments(&call)?;
        match self.server.call(&call.tool, arguments).await {
            // The server's `CallToolResult` verbatim, `isError` included. See [the module
            // documentation](self#a-tool-that-ran-and-refused-is-an-answer).
            Ok(result) => encode_response(&result),
            Err(error) => Err(wire_error(self.server.name(), &call.tool, &error)),
        }
    }
}

/// A server's name as a capability name, or why it cannot be one.
fn capability_name(server: &str) -> anyhow::Result<String> {
    if server.is_empty() {
        anyhow::bail!(
            "an MCP server with no name cannot be announced: its capability would be \
             `{CAPABILITY_PREFIX}`, which names nothing"
        );
    }
    if server.contains('.') {
        // Not cosmetic. A method is `capability.tool` split at the first dot, so the capability
        // an agent would address is everything before the server name's own dot — a name nothing
        // announced.
        anyhow::bail!(
            "the MCP server `{server}` cannot be announced: a capability name may not contain a \
             dot, because a call is addressed as `capability.tool` and would be read as a call to \
             `{CAPABILITY_PREFIX}{}`. Rename the server.",
            server.split('.').next().unwrap_or_default()
        );
    }
    Ok(format!("{CAPABILITY_PREFIX}{server}"))
}

/// A tool list as descriptors, and whatever could not be one.
fn translate(server: &str, tools: &[rmcp::model::Tool]) -> (Vec<ToolDescriptor>, Vec<DroppedTool>) {
    let mut descriptors: Vec<ToolDescriptor> = Vec::with_capacity(tools.len());
    let mut dropped = Vec::new();

    for tool in tools {
        let name = tool.name.to_string();
        if descriptors.iter().any(|seen| seen.name == name) {
            dropped.push(DroppedTool {
                name,
                reason: format!(
                    "the MCP server `{server}` offers more than one tool called `{}`, and only \
                     the first can ever be called",
                    tool.name
                ),
            });
            continue;
        }
        descriptors.push(ToolDescriptor {
            name,
            description: description(server, tool),
            transfer: Transfer::Unary,
            request_schema: tool.schema_as_json_value(),
            response_schema: Some(response_schema(tool)),
            // Nothing about MCP streams: `tools/call` is one request and one answer.
            item_schema: None,
            // Not this layer's to set. A per-tool deadline would have to come from the server's
            // own words, and MCP has none; `Server::spawn`'s deadline covers startup, which is
            // the part that otherwise hangs forever.
            call_limit: None,
        });
    }

    (descriptors, dropped)
}

/// What an agent is told a tool is for.
///
/// MCP makes `description` optional and `ToolDescriptor` makes it a `String`, so the gap has to
/// be filled with something. A `title` is the server's own wording and is the better fallback;
/// with neither, the sentence says so, because a blank description reads like a field lost in
/// translation rather than a server that supplied none.
fn description(server: &str, tool: &rmcp::model::Tool) -> String {
    if let Some(description) = &tool.description {
        return description.to_string();
    }
    if let Some(title) = &tool.title {
        return title.clone();
    }
    format!("`{}`, on the MCP server `{server}`, which described it no further.", tool.name)
}

/// What a promoted tool answers with, as JSON Schema.
///
/// Always the MCP result envelope, because that is what [`Server::call`] hands back verbatim.
/// **MCP's `outputSchema` describes `structuredContent`, not the whole result**, so putting it in
/// `response_schema` unwrapped would describe the payload wrongly and an agent validating an
/// answer against it would reject every one. It is nested instead, and a tool that declared none
/// leaves that half unconstrained rather than gaining a constraint invented here.
///
/// The envelope is also how an agent learns where a refusal lives — see [the module
/// documentation](self#a-tool-that-ran-and-refused-is-an-answer).
fn response_schema(tool: &rmcp::model::Tool) -> Value {
    let structured = match &tool.output_schema {
        Some(schema) => Value::Object(schema.as_ref().clone()),
        None => Value::Object(Map::new()),
    };
    json!({
        "type": "object",
        "description": "The result of an MCP tool call.",
        "properties": {
            "content": {
                "type": "array",
                "description": "What the tool said, as MCP content blocks.",
            },
            "structuredContent": structured,
            "isError": {
                "type": "boolean",
                "description":
                    "True when the tool ran and refused. The reason is in `content`. A call that \
                     never happened is an error rather than a result with this set.",
            },
        },
    })
}

/// A call's parameters as MCP arguments.
///
/// `tools/call` carries an object or nothing, and every input schema describes an object. A
/// caller that sent a scalar or an array is told so here rather than having the server say it in
/// its own words, which would be one more wording for a caller to learn.
fn arguments(call: &IncomingCall) -> zyris::Result<Value> {
    // Nil is what `IncomingCall::params` holds when the caller sent nothing at all, which is the
    // ordinary way to call a tool that takes no arguments.
    let params = call.params.to_json()?;
    match params {
        Value::Null | Value::Object(_) => Ok(params),
        other => Err(WireError::invalid_params(format!(
            "arguments for `{}` must be a JSON object, not {}",
            call.tool,
            kind_of(&other)
        ))),
    }
}

/// What went wrong with a call that reached the server, as something the agent can act on.
///
/// Only two answers, deliberately. A disconnection is the one an agent can do something about —
/// stop calling this capability — and everything else is this node's problem to report and
/// nobody else's to classify. The server's own words travel in the message either way, because
/// they are the only description of what happened that anybody wrote.
fn wire_error(server: &str, tool: &str, error: &anyhow::Error) -> WireError {
    let disconnected = error
        .downcast_ref::<rmcp::service::ServiceError>()
        .is_some_and(is_disconnected);
    if disconnected {
        // `CapabilityUnavailable`, not `Internal`: the capability is still announced and the
        // tools are still listed, but nothing behind them is there. Not retriable — the process
        // is gone and will not come back on its own; withdrawing the capability is the answer,
        // and that is Task 4's.
        return WireError::new(
            ErrorCode::CapabilityUnavailable,
            format!(
                "the MCP server `{server}` is no longer running, so `{tool}` cannot be called: \
                 {error:#}"
            ),
        );
    }
    WireError::new(
        ErrorCode::Internal,
        format!("calling `{tool}` on the MCP server `{server}` failed: {error:#}"),
    )
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The name rule, without a process behind it.
    ///
    /// The live half — a server actually called `terminal`, spawned and promoted — is in
    /// `tests/promoted_tools.rs`. This is the part that is about the function and not about a
    /// server: that prefixing is plain concatenation and therefore injective, which is what makes
    /// "two servers can never produce one capability" true rather than likely.
    #[test]
    fn prefixing_is_injective_so_two_servers_can_never_become_one_capability() {
        let names = [
            "notes",
            "terminal",
            "mcp_terminal",
            "mcp_mcp_terminal",
            "NOTES",
            "notes ",
            "a-very-long-name-that-nobody-would-type-but-a-generator-might-well-produce",
        ];
        let promoted: Vec<String> = names
            .iter()
            .map(|name| capability_name(name).expect("none of these is refused"))
            .collect();

        for (index, name) in names.iter().enumerate() {
            assert_eq!(promoted[index], format!("{CAPABILITY_PREFIX}{name}"));
        }
        let mut unique = promoted.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), promoted.len(), "two servers collided: {promoted:?}");
    }

    #[test]
    fn a_name_that_will_not_route_is_refused_rather_than_repaired() {
        // Repairing it — dropping the dot, replacing it — would make the capability's name
        // something nobody configured, and two servers whose names differ only in punctuation
        // would then collide. Refusing names the problem to the one person who can fix it.
        for refused in ["my.notes", "", "a.", ".b"] {
            assert!(
                capability_name(refused).is_err(),
                "`{refused}` should not be promotable"
            );
        }
    }
}
