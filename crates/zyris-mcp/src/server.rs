//! One MCP server: the process, the tools it said it has, and what happens when it stops.
//!
//! # Everything here is bounded
//!
//! A server is a command a person typed into a configuration file, so all three of the ways it
//! can be wrong have to end in an error rather than in waiting:
//!
//! - **The command is not there.** `Command::spawn` fails with `NotFound` before a byte is
//!   written, so this one is free. It is also the one people expect to be the dangerous case, and
//!   it is the least dangerous of the three.
//! - **The command is there and says nothing.** The wrong binary, a wrapper script that prints a
//!   usage message to stderr and sits waiting on stdin, an interpreter with no script. Nothing in
//!   `rmcp` bounds the initialize handshake, so without [`STARTUP_DEADLINE`] this would wait for
//!   as long as the process lives — and a caller cannot tell "still starting" from "never going
//!   to start". This is the case the deadline exists for.
//! - **The command is there, speaks, and then dies.** See "Death" below.
//!
//! # Death
//!
//! Established against a server that exits mid-request, not assumed:
//!
//! `rmcp`'s service loop reads the child's stdout; when that returns end-of-file the loop quits
//! with `QuitReason::Closed` and drops every pending responder. The call that was in flight
//! therefore fails with [`ServiceError::TransportClosed`], and so does every call made
//! afterwards. There is no notification and no callback, so [`is_disconnected`] is how the answer
//! to a call is told apart from a tool that merely refused.
//!
//! **But a death does not have to be waited for until somebody calls.** [`Server::is_running`] is
//! a read of a flag, and it is the one `rmcp` primitive that tells the truth here. The two
//! neighbouring candidates do not, and the difference between the three is not guessable from
//! their names:
//!
//! | asked of a child that has exited | answer |
//! |---|---|
//! | `RunningService::is_closed()` | `false` — **wrong**, and stays wrong |
//! | `RunningService::waiting()` | correct, but consumes the service |
//! | `Peer::is_transport_closed()` | `true`, within milliseconds, with nothing asked of the server |
//!
//! **`is_closed()` is not a health check, and no wrapper around it can be.** Measured against a
//! server that exits mid-call (2026-09-14): it stays `false` before the death, immediately after
//! it, half a second later, and after a second call has already failed with `TransportClosed`. It
//! reports *cancellation* — this end deciding to stop — and the service loop does not cancel its
//! token when the child goes away.
//!
//! **`is_transport_closed()` is**, and it was measured the same way rather than read (2026-09-14,
//! against `rmcp` 3.3.0). It is `Peer`'s, reached through `RunningService`'s `Deref`, so it takes
//! `&self` and works through an `Arc` — nothing is consumed and nothing is spawned. Its body is
//! `self.tx.is_closed()`: `tx` sends into the service loop, the loop owns the receiver, and the
//! loop drops it on the way out of `QuitReason::Closed`. Four readings around a child that exits
//! during a call: `false` before, `true` immediately after, `true` half a second later, `true`
//! after a second call had already failed. And the reading that actually matters, because a
//! signal only a call can produce is no use to a health check — a child that exits on a timer
//! with **no call ever made**, before or after: `true` about six milliseconds later.
//!
//! The five-second drain `rmcp` puts between `QuitReason::Closed` and the end of its loop does
//! not delay this for a client: the drain waits on handler tasks, and a client that serves `()`
//! has none.
//!
//! The child itself is reaped without anybody asking: dropping the [`Server`] cancels the
//! service, which closes the transport, which waits briefly for the child and then kills it.

use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use rmcp::model::{CallToolRequestParams, CallToolResponse, Tool};
use rmcp::service::{RoleClient, RunningService, ServiceError};
use rmcp::transport::TokioChildProcess;
use rmcp::{ServiceExt, model::JsonObject};
use serde_json::Value;

/// How long [`Server::spawn`] will wait for a command to finish the MCP handshake and answer
/// `tools/list` before giving up on it.
///
/// Ten seconds, which is generous rather than tight: the common real server is `npx something`,
/// which on a cold cache on Windows genuinely takes seconds to reach its first byte. The number
/// is not a performance budget — it is the line past which "starting" stops being a useful
/// description of what the process is doing.
pub const STARTUP_DEADLINE: Duration = Duration::from_secs(10);

/// One local MCP server, running, with the tools it reported at startup.
///
/// The tool list is read once, during [`spawn`](Server::spawn), and kept. MCP servers can change
/// their tools mid-session and announce it with `notifications/tools/list_changed`; nothing here
/// listens for that yet, and a re-announcement is the step that will need it.
pub struct Server {
    name: String,
    tools: Vec<Tool>,
    service: RunningService<RoleClient, ()>,
}

impl Server {
    /// Start `command` with `args` and ask it what it has.
    ///
    /// `name` is this server's name in the configuration; it is carried so that a failure, a log
    /// line or a promoted capability can say which server it is talking about. Nothing about it
    /// reaches the server.
    ///
    /// Returns an error rather than waiting if the command is missing, exits, or fails to finish
    /// the handshake and report its tools within [`STARTUP_DEADLINE`].
    pub async fn spawn(name: &str, command: &str, args: &[String]) -> anyhow::Result<Self> {
        let mut process = tokio::process::Command::new(command);
        process.args(args);

        // The server's stderr is its own diagnostic channel — usage messages, stack traces, the
        // reason it is about to exit — and inheriting it puts that in front of whoever is reading
        // Zyris's output. It must not be `piped()` with nobody draining it: a server that logs
        // enough fills the pipe buffer and blocks on its own write, which looks exactly like a
        // hang and is one of the few ways this code could cause one.
        let transport = TokioChildProcess::builder(process)
            .stderr(Stdio::inherit())
            .spawn()
            .map(|(transport, _no_stderr_handle)| transport)
            .with_context(|| format!("starting MCP server `{name}` (`{command}`)"))?;

        // **One deadline over both steps, not one each.** From the outside they are the same
        // thing: a server that answered `initialize` and then went quiet on `tools/list` is no
        // more usable than one that never answered at all, and a per-step deadline is a second
        // timeout that only the rarer failure exercises — so the one the tests do not reach is
        // the one that rots.
        let startup = async {
            let service = ()
                .serve(transport)
                .await
                .with_context(|| format!("MCP handshake with server `{name}`"))?;
            // Following `nextCursor` rather than taking the first page: a server with more tools
            // than fit in one response would otherwise lose the rest silently, and a tool an
            // agent cannot see is indistinguishable from one the server does not have.
            let tools = service
                .list_all_tools()
                .await
                .with_context(|| format!("listing the tools of MCP server `{name}`"))?;
            anyhow::Ok((service, tools))
        };
        let (service, tools) = tokio::time::timeout(STARTUP_DEADLINE, startup)
            .await
            .map_err(|_| {
                anyhow!(
                    "MCP server `{name}` did not finish the handshake and list its tools within \
                     {STARTUP_DEADLINE:?}"
                )
            })??;

        tracing::info!(server = name, tools = tools.len(), "MCP server started");

        Ok(Self {
            name: name.to_owned(),
            tools,
            service,
        })
    }

    /// This server's name in the configuration.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether the process behind this is still there.
    ///
    /// **Cheap enough to ask on a timer, and that is the whole point.** It reads a flag on the
    /// channel into `rmcp`'s service loop: no round trip, nothing written to the child, nothing
    /// spawned, no `&mut self` and no consuming. Asking it a thousand times costs what asking it
    /// once costs, so whoever polls it is choosing how stale an announcement may be and nothing
    /// else.
    ///
    /// `true` for a server that is merely idle — see [the module documentation](self#death) for
    /// the four readings this was established with, and for why `rmcp`'s own `is_closed()` cannot
    /// answer this.
    ///
    /// **It reports the transport, which is the process.** A server that is alive but wedged —
    /// answering nothing, or answering nonsense — still reads as running, and correctly: it is
    /// there, and withdrawing it would say it is not. What that costs is that the only way to
    /// learn a server has stopped being *useful* is still to call it.
    pub fn is_running(&self) -> bool {
        !self.service.is_transport_closed()
    }

    /// Stop the server: end the session, close the pipes, and reap the child.
    ///
    /// **Explicit, rather than left to the last [`Arc`](std::sync::Arc) going away.** Dropping a
    /// `Server` does reap the child — `ChildWithCleanup::drop` kills it — but a `Server` that has
    /// been promoted and announced is held in several places at once, and "the process stops when
    /// the last of them lets go" is a rule that quietly stops being true the moment anything keeps
    /// a handle. What that looks like is a server somebody switched off that is still running: no
    /// longer announced, so nothing can reach it and nothing lists it, and still there.
    ///
    /// Takes `&self`, which is what makes it usable through an `Arc` at all. `rmcp`'s
    /// `RunningService::cancellation_token()` hands out a token from `&self`; cancelling it ends
    /// the service loop, which closes the transport, which closes the child's stdin and waits
    /// briefly for it to exit before killing it.
    ///
    /// Returns immediately. The teardown runs on `rmcp`'s own task, so
    /// [`is_running`](Self::is_running) goes false a moment later rather than on this line.
    /// Calling it on a server that has already died is harmless and does nothing.
    pub fn stop(&self) {
        tracing::info!(server = self.name, "stopping an MCP server");
        self.service.cancellation_token().cancel();
    }

    /// What the server said it has, as it said it.
    ///
    /// [`Tool`] is `rmcp`'s own description and is passed through untouched: a `name`, an optional
    /// `description`, and an `input_schema` that is JSON Schema. Every field a server sends
    /// survives to whatever promotes it.
    pub fn tools(&self) -> &[Tool] {
        &self.tools
    }

    /// Call one of this server's tools and hand back what it answered.
    ///
    /// `arguments` must be a JSON object or `null`; that is what MCP's `tools/call` carries and
    /// what every tool's input schema describes.
    ///
    /// The answer is the server's `CallToolResult` verbatim — `content`, and `structuredContent`
    /// and `isError` when the server sent them. **`isError: true` is not an `Err` here.** MCP
    /// draws the line between a tool that ran and failed (a successful response saying so, with
    /// the reason in `content`) and a call that never happened (a JSON-RPC error), and flattening
    /// the two would throw away the reason. Deciding what an agent sees is the promotion layer's
    /// job; this one reports.
    ///
    /// **No deadline, and [`STARTUP_DEADLINE`] does not supply one** — that bounds the handshake
    /// and the first `tools/list`, and nothing after. `call_tool_once` goes out with
    /// `PeerRequestOptions::default()`, whose `timeout` is `None`, so a server that accepts a
    /// request and never answers waits here until the caller gives up or the connection goes
    /// down. The reasoning for leaving it that way, and what it costs the audit log, is on
    /// `promote.rs`'s `call_limit`.
    pub async fn call(&self, tool: &str, arguments: Value) -> anyhow::Result<Value> {
        let arguments: Option<JsonObject> = match arguments {
            Value::Null => None,
            Value::Object(map) => Some(map),
            other => bail!(
                "arguments for `{tool}` on MCP server `{}` must be a JSON object, not {}",
                self.name,
                kind_of(&other)
            ),
        };

        let mut params = CallToolRequestParams::new(tool.to_owned());
        params.arguments = arguments;

        let response = self
            .service
            .call_tool_once(params)
            .await
            .with_context(|| format!("calling `{tool}` on MCP server `{}`", self.name))?;

        match response {
            CallToolResponse::Complete(result) => Ok(serde_json::to_value(result)?),
            // Neither of these is a partial answer that could be waited out: both say the call
            // does not finish on this request, and this crate offers no way to carry one on.
            // Saying so beats returning something shaped like a result.
            CallToolResponse::InputRequired(_) => bail!(
                "`{tool}` on MCP server `{}` asked for more input mid-call; \
                 Zyris calls a tool once and takes the answer",
                self.name
            ),
            CallToolResponse::Task(_) => bail!(
                "`{tool}` on MCP server `{}` answered with a long-running task; \
                 Zyris calls a tool once and takes the answer",
                self.name
            ),
            // `CallToolResponse` is `#[non_exhaustive]`, and every way it can grow is another way
            // for one call not to be the whole answer. Refusing an unrecognised one by name is
            // the only safe default: the alternative is a caller handed something shaped like a
            // result that is not one.
            other => bail!(
                "`{tool}` on MCP server `{}` answered with {other:?}, which this version of \
                 Zyris does not know how to carry on",
                self.name
            ),
        }
    }
}

/// The name and how many tools, and nothing else.
///
/// Not derived: a `Server` holds the whole tool list, schemas included, and a derived `Debug`
/// would put all of it into any log line or panic message that formats one.
impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("name", &self.name)
            .field("tools", &self.tools.len())
            .finish_non_exhaustive()
    }
}

/// Whether an error from `rmcp` means the connection is gone rather than the call being refused.
///
/// Kept next to [`Server`] because it is the one piece of `rmcp`'s error shape the layers above
/// need: withdrawing a capability is the right answer to a dead server and the wrong answer to a
/// tool that returned an error.
pub fn is_disconnected(error: &ServiceError) -> bool {
    matches!(
        error,
        ServiceError::TransportClosed | ServiceError::TransportSend(_)
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

    /// Where `cargo test` leaves the `probe_server` example.
    ///
    /// Derived rather than handed over: there is no `CARGO_BIN_EXE_` for an example. Asserted,
    /// because a missing file here has one cause — a target selection narrow enough that cargo
    /// skipped examples — and saying so beats a spawn failure that reads like broken code.
    fn probe_server() -> String {
        let mut directory = std::env::current_exe().expect("the test binary knows its own path");
        directory.pop();
        if directory.ends_with("deps") {
            directory.pop();
        }
        let path = directory
            .join("examples")
            .join(format!("probe_server{}", std::env::consts::EXE_SUFFIX));
        assert!(
            path.is_file(),
            "the `probe_server` example is not at {}; `cargo test -p zyris-mcp` builds it",
            path.display()
        );
        path.into_os_string().into_string().expect("a path cargo produced is UTF-8")
    }

    /// The four readings [`Server::is_running`] is built on, taken here rather than trusted.
    ///
    /// The sibling of `rmcp_does_not_report_a_dead_child_as_closed` below and deliberately the
    /// same shape: the two differ only in which `rmcp` primitive they ask, and that is the whole
    /// finding. If a later `rmcp` moves either answer, exactly one of these two fails and names
    /// what changed.
    #[tokio::test]
    async fn a_dead_child_stops_reporting_itself_as_running() {
        let server = Server::spawn("probe", &probe_server(), &[])
            .await
            .expect("the probe server starts");

        // Before.
        assert!(server.is_running(), "a server that just started is running");

        let died = server
            .call("die", serde_json::json!({}))
            .await
            .expect_err("the server exits during the call");
        assert!(
            died.downcast_ref::<ServiceError>().is_some_and(is_disconnected),
            "expected a disconnection, got {died:#}"
        );

        // Immediately after, half a second later, and after a second call has already failed.
        // All three, because `is_closed()` passes the first of them and fails the rest, and a
        // test that took only one reading would not have caught it.
        assert!(!server.is_running(), "immediately after the death");
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!server.is_running(), "half a second after the death");
        let _ = server.call("echo", serde_json::json!({ "text": "anybody" })).await;
        assert!(!server.is_running(), "after a second call had already failed");
    }

    /// The claim in this module's "Death" section, kept honest.
    ///
    /// It lives here rather than in `tests/one_server.rs` because it is about a private field: no
    /// public method reports it, deliberately, and this test is why. If a later `rmcp` starts
    /// cancelling the token when the child goes away, this fails — and then a liveness flag
    /// becomes possible and the module doc has to change.
    #[tokio::test]
    async fn rmcp_does_not_report_a_dead_child_as_closed() {
        let server = Server::spawn("probe", &probe_server(), &[])
            .await
            .expect("the probe server starts");

        let died = server
            .call("die", serde_json::json!({}))
            .await
            .expect_err("the server exits during the call");
        assert!(
            died.downcast_ref::<ServiceError>()
                .is_some_and(is_disconnected),
            "expected a disconnection, got {died:#}"
        );

        assert!(
            !server.service.is_closed(),
            "`rmcp` now reports a dead child as closed — a liveness flag is possible, and this \
             module's \"Death\" section says it is not"
        );
    }
}
