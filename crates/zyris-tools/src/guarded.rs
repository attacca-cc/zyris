//! The gate and the log, in front of whatever a capability does.
//!
//! One decorator over `ServeCapability` rather than one wrapper per capability trait. The
//! protocol funnels every call through `dispatch`, carrying the capability name and the tool
//! name with it, so this is both the smallest place the two questions can be asked and the only
//! place that cannot be forgotten when a capability is added.

use std::sync::Arc;

use zyris::{CapabilityDescriptor, IncomingCall, Outgoing, ServeCapability};
use zyris_runtime::{CoreEvent, EventBus};

use crate::Gate;
use crate::audit::{self, AuditLog, Entry, Outcome};

/// Any capability, with the gate in front of it and the log behind it.
///
/// No `#[derive(Clone)]` or `#[derive(Debug)]`: `FileIoServer`/`TerminalServer` are generated as
/// bare `pub struct Server<T: Trait>(pub T)` and derive nothing, so neither is derivable through
/// them. [`Tools`](crate::Tools) is the `Clone` type; it clones the gate, the log and the root
/// and builds the servers inside `into_capabilities`.
pub struct Guarded<C> {
    inner: C,
    capability: String,
    gate: Gate,
    log: AuditLog,
    /// Where a call announces itself as it happens, on top of being written down.
    ///
    /// Optional because the log is the record and the bus is only a tail: a `Guarded` built
    /// without one still refuses, still runs and still writes every line to disk. The crate's
    /// own tests use that shape.
    bus: Option<EventBus>,
    /// Whether [`summarize`] runs at all, decided once from the capability's name.
    ///
    /// See [`MCP_CAPABILITY_PREFIX`]. False for a promoted MCP server, whose arguments this
    /// workspace has never seen and must not write down.
    summarize_params: bool,
}

/// The prefix `zyris-mcp` gives every capability it promotes from a local MCP server.
///
/// **A capability whose name starts with this writes no argument detail into the audit log at
/// all**, and that is the whole reason this constant is here rather than in the crate that
/// produces it.
///
/// [`LOGGED_FIELDS`] is an allowlist whose entries were each chosen by reasoning about what a
/// parameter *means* in one of this machine's own five capabilities — `path` is a file this node
/// resolved, `command` is a shell command line, `pty` is a terminal. **None of that reasoning
/// transfers to a server somebody installed.** A promoted tool's arguments are arbitrary JSON
/// written by a third party, and a field spelled `path` or `command` in one of them is a
/// coincidence of spelling and not the same fact — it could as easily be a password. The
/// allowlist matches on spelling alone, so without this rule those two names would be written
/// down for every MCP server on the machine.
///
/// Widening the allowlist to *catch* MCP arguments would be the same mistake pointing the other
/// way, and is ruled out for the same reason. The tool call is still recorded — when, which
/// capability, which tool, and whether the switch let it through — and that is the record. What
/// is not recorded is what was asked, and anything telling a person about this log has to say so
/// rather than let them assume otherwise.
///
/// Matched on the name rather than set by whoever builds the `Guarded`, so it cannot be forgotten
/// at a call site added later.
///
/// **`zyris_mcp::CAPABILITY_PREFIX` itself, not a copy of it.** Two strings that have to agree,
/// in two crates, with nothing that fails when they stop agreeing, is the shape this workspace
/// keeps writing down as the quiet kind of bug: the day somebody renamed the prefix on one side,
/// every promoted tool's arguments would start being written into the audit file and no test
/// anywhere would go red. This alias is a second name for one value, and `announce.rs` names
/// `zyris-mcp` for it.
pub const MCP_CAPABILITY_PREFIX: &str = zyris_mcp::CAPABILITY_PREFIX;

impl<C: ServeCapability> Guarded<C> {
    pub fn new(inner: C, gate: Gate, log: AuditLog) -> Guarded<C> {
        // `descriptor()` is not a field read. The capability macro regenerates every JSON schema
        // in the capability on each call — about a millisecond for `file_io` — so the name is
        // taken once here rather than on the request path.
        let capability = inner.descriptor().name;
        let summarize_params = !capability.starts_with(MCP_CAPABILITY_PREFIX);
        Guarded { inner, capability, gate, log, bus: None, summarize_params }
    }

    /// Also tell everything watching, as each call happens.
    pub fn with_bus(mut self, bus: EventBus) -> Guarded<C> {
        self.bus = Some(bus);
        self
    }

    pub fn into_arc(self) -> Arc<dyn ServeCapability> {
        Arc::new(self)
    }

    fn record(&self, tool: &str, detail: String, outcome: Outcome) {
        if let Some(bus) = &self.bus {
            // **`publish_transient`, never `publish`.** `publish` also writes the bus's one-slot
            // catch-up value, which the window reads exactly once through `latest_event` to
            // learn what it missed while its listener was being registered. Tool calls arrive
            // far more often than connection events, so sharing that slot would mean the window
            // almost always catches up on a `toolCall` — a kind its reducer has no arm for — and
            // never leaves its starting screen.
            //
            // The cost is that a tool call published while nothing is subscribed is gone, and
            // that a burst past the bus's capacity drops the oldest — logged by the forwarder as
            // the window falling behind. Both are acceptable and neither is a bug: the record
            // that has to survive is the audit file, and the window shows a tail, not a ledger.
            bus.publish_transient(CoreEvent::ToolCall {
                capability: self.capability.clone(),
                tool: tool.to_string(),
                detail: detail.clone(),
                outcome: outcome.as_str().to_string(),
            });
        }
        self.log.record(Entry {
            at: audit::now(),
            capability: self.capability.clone(),
            tool: tool.to_string(),
            detail,
            outcome,
        });
    }
}

#[zyris::async_trait]
impl<C: ServeCapability> ServeCapability for Guarded<C> {
    fn descriptor(&self) -> CapabilityDescriptor {
        self.inner.descriptor()
    }

    /// **A line is written when the call finishes, one way or another — and only then.**
    ///
    /// A refusal is written before the capability is reached, and an answer or a failure is
    /// written when `inner.dispatch` returns. A call that never gets to return therefore leaves
    /// nothing: `zyris-core` holds an `AbortHandle` per call in flight and aborts the task when
    /// the peer sends a cancel, when the connection goes down, and when the capability is revoked
    /// (`connection.rs` in that crate, measured at rev 274e9af). An aborted future does not run
    /// its continuation, so the `record` below never happens.
    ///
    /// That is a real hole and it is deliberately left open here, because closing it honestly is
    /// not a change to this function. It would take a second kind of line — one at the start and
    /// one at the end — or a drop guard writing a fourth [`Outcome`] for "began and did not
    /// finish", and either one changes the shape of the file, the audit tail on the Tools screen,
    /// and every sentence anybody has written about what a line means. It is also not new and not
    /// about MCP: `terminal.exec` with no `timeout_ms` has had exactly this property since it was
    /// announced, and [`crate::gate`] names it.
    ///
    /// What is **not** left open is the copy. `ui/src/Mcp.tsx` and `README.md` both describe this
    /// log, and both now say a line is written when a call finishes and that a call cut off
    /// before it finished has none. See
    /// [`a_call_that_never_finishes_leaves_no_line`](tests::a_call_that_never_finishes_leaves_no_line).
    async fn dispatch(&self, call: IncomingCall) -> zyris::Result<Outgoing> {
        // Both of these are read before the call is handed on, because `dispatch` consumes it.
        let tool = call.tool.clone();
        // An empty detail rather than a summary for a promoted MCP capability — see
        // [`MCP_CAPABILITY_PREFIX`]. The line itself is still written.
        let detail = if self.summarize_params { summarize(&call) } else { String::new() };

        if let Err(refusal) = self.gate.check() {
            self.record(&tool, detail, Outcome::Refused);
            return Err(refusal);
        }

        let outcome = self.inner.dispatch(call).await;

        // `Allowed` means the call was accepted and, for a streaming tool, that the stream
        // opened. `Outgoing::Stream` carries a lazy item stream, so a failure partway through it
        // lands after this returns and is not in the log. A clean line is not proof a long
        // `read_stream` finished.
        self.record(
            &tool,
            detail,
            if outcome.is_ok() { Outcome::Allowed } else { Outcome::Failed },
        );
        outcome
    }
}

/// The fields worth writing down. Everything else in a call is either payload or noise.
///
/// Deliberately an allowlist, not a denylist. The secret-bearing arguments of these four
/// capabilities are `file_io.write`'s `data`, `file_io.edit`'s `old_string`/`new_string`,
/// `terminal.write`'s `data`, `terminal.read`/`screen`'s `input` (where a typed password lands),
/// `terminal.exec`'s `stdin` and `env`, and `input.type_text`'s `text` and `input.key`'s `chord`.
/// A denylist grows a hole every time the protocol adds a field. If a future capability needs
/// something named here, add it here on purpose.
///
/// `input.type_text`'s `text` and `input.key`'s `chord` are deliberately absent and must stay
/// absent. Typing is how a password reaches an application, and a sequence of single-character
/// chords reconstructs one a keystroke at a time. Not the values, and not their lengths: this
/// file is meant to be handable to someone helping you. `screen_capture.screenshot` is the same
/// problem in a different shape and is already safe for a different reason — the picture is a
/// return value and this function only ever reads params. Do not "improve" that later.
///
/// What is safe and worth having from the other two is which display, where on it, which button
/// and how far: `display`, `x`, `y` (`input.move_to`), `button` (`input.click`), `dx`, `dy`
/// (`input.scroll`), and `region`, `format`, `max_width` (`screen_capture.screenshot`). A log
/// that cannot say where the pointer was driven answers nothing about what happened to the
/// machine. `screen_capture.list_displays` takes no parameters and so writes an empty detail,
/// which is the right answer.
///
/// `recursive` and `overwrite` are here because they are the difference between two calls the
/// log would otherwise spell identically, and the destructive one is the one that gets lost:
/// `file_io.remove` takes `recursive: Option<bool>` and `zyris-fs` branches on it into
/// `remove_dir_all` rather than `remove_dir`, so without it a line reading `path=/home/me/work`
/// is the same whether a tree went or an empty directory did. `file_io.write`'s `overwrite: bool`
/// is the same question about a file that already existed. Neither weakens the reasoning above:
/// both are bare booleans off the parameter list and neither can carry a payload.
///
/// `file_transfer` adds `node` and `name`, and its two tools are the whole of what it can say:
/// `send_to(node, path, name, overwrite)` names the machine a file was sent to, the file that was
/// read, what it was called on arrival and whether it replaced something there — four facts that
/// identify a transfer and not one byte of one. `inbox_list` takes no parameters, so it writes an
/// empty detail, which is the right answer. Neither name collides: none of the other four
/// capabilities has a parameter called `node` or `name`, and a future one that did would have to
/// be checked here before it was announced.
///
/// **`peer_transfer` is deliberately not accounted for here, because it never reaches this
/// function.** It is announced on the peer link by `zyris-transfer` rather than by
/// [`Tools`](crate::Tools) — see [`crate::transfer`] — so no `push_offer` or `pull` is ever
/// wrapped in a `Guarded`. Its record is `TransferConfig::audit`, written per received file. Were
/// one ever announced here, note where its bytes actually are: `push_offer` carries a
/// `TransferOffer` (a name, a size, a hash) and `pull` a transfer id and an offset, and the file
/// itself travels in `pull`'s reply stream — which this function never reads, the same way a
/// screenshot stays out of the log.
///
/// These are the wire names: the capability macro derives the request struct straight from the
/// trait's parameter list with no `rename_all`, so they stay snake_case. `exec` carries its
/// command line in `command` **or** `argv`, never both, and `pty` identifies the target of every
/// `read`/`screen`/`write`/`resize`/`close`.
///
/// **None of this reasoning reaches a promoted MCP capability, and it must not be made to.** Every
/// entry below is a judgement about what a name *means* in one of this machine's own five
/// capabilities, and a server somebody installed shares none of those meanings — only, sometimes,
/// the spelling. See [`MCP_CAPABILITY_PREFIX`], where that is settled.
///
/// The array's order is the line's order, so `path=` stays first.
const LOGGED_FIELDS: &[&str] = &[
    "path",
    "node",
    "name",
    "command",
    "argv",
    "cwd",
    "shell",
    "pty",
    "recursive",
    "overwrite",
    "display",
    "x",
    "y",
    "button",
    "dx",
    "dy",
    "region",
    "format",
    "max_width",
];

/// How much of one value is worth keeping. One enormous argument must not make the log
/// unreadable, and the log is a summary rather than a transcript.
const VALUE_MAX: usize = 128;

/// A short `key=value` line naming what a call was asked to act on.
///
/// The recorded path is **the caller's string, not the resolved host path**, so `path=notes/x.txt`
/// does not say which root it resolved against; the root is logged once at startup instead.
///
/// Never fails. `Payload::to_json` returns a `Result` — a params object that will not decode is
/// the capability's problem to report, and a summary that cannot be built must not refuse a call.
fn summarize(call: &IncomingCall) -> String {
    let Ok(params) = call.params.to_json() else {
        return String::new();
    };
    let Some(fields) = params.as_object() else {
        return String::new();
    };
    // Walking the allowlist rather than the params keeps the order the one written above rather
    // than whatever order the map happens to iterate in. When a call carries none of these the
    // result is empty, which is the right answer: the tool name alone is still a useful line,
    // and the alternative — dumping the whole params object — is where the payload lives.
    LOGGED_FIELDS
        .iter()
        .filter_map(|name| fields.get(*name).map(|value| format!("{name}={}", render(value))))
        .collect::<Vec<_>>()
        .join(" ")
}

/// One value as the log spells it: a string as itself, anything else — `argv` is an array — as
/// its compact JSON.
fn render(value: &serde_json::Value) -> String {
    let text = match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    truncate(text)
}

fn truncate(text: String) -> String {
    match text.char_indices().nth(VALUE_MAX) {
        // Cutting on a char boundary rather than a byte one: a path or a command line may hold
        // any UTF-8, and slicing through a codepoint panics.
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zyris::caps::{
        Display, FileIoServer, FileTransfer, FileTransferServer, ImageFormat, InboxEntry, Input,
        InputServer, MouseButton, Region, ScreenCapture, ScreenCaptureServer, SendReceipt,
        file_io_capability,
    };
    use zyris::{Datum, IncomingCall, Payload, Serialization};

    fn call(tool: &str, params: serde_json::Value) -> IncomingCall {
        IncomingCall {
            tool: tool.to_string(),
            params: Payload::from_json(params),
            serialization: Serialization::Json,
            meta: Payload::default(),
        }
    }

    fn guarded(
        dir: &std::path::Path,
    ) -> (
        crate::Gate,
        crate::AuditLog,
        Guarded<FileIoServer<zyris_fs::LocalFileIo>>,
    ) {
        let gate = crate::Gate::running();
        let log = crate::AuditLog::new(dir.join("audit.jsonl"));
        let cap = Guarded::new(
            FileIoServer(zyris_fs::LocalFileIo::rooted(dir)),
            gate.clone(),
            log.clone(),
        );
        (gate, log, cap)
    }

    /// `input` and `screen_capture` behind the decorator, with nothing behind *them*.
    ///
    /// The real backends need a display server, and what is under test here is what `summarize`
    /// writes rather than what the platform does. A fake that always succeeds keeps these tests
    /// runnable on a headless machine, and it keeps a redaction assertion honest: the call
    /// reaches the capability, so a detail that does not carry the secret is not merely a call
    /// that never ran.
    struct FakeInput;

    #[zyris::async_trait]
    impl Input for FakeInput {
        async fn type_text(&self, _text: String) -> zyris::Result<()> {
            Ok(())
        }

        async fn key(&self, _chord: String) -> zyris::Result<()> {
            Ok(())
        }

        async fn move_to(&self, _display: String, _x: i32, _y: i32) -> zyris::Result<()> {
            Ok(())
        }

        async fn click(&self, _button: MouseButton) -> zyris::Result<()> {
            Ok(())
        }

        async fn scroll(&self, _dx: i32, _dy: i32) -> zyris::Result<()> {
            Ok(())
        }
    }

    struct FakeScreen;

    #[zyris::async_trait]
    impl ScreenCapture for FakeScreen {
        async fn list_displays(&self) -> zyris::Result<Vec<Display>> {
            Ok(Vec::new())
        }

        async fn screenshot(
            &self,
            _display: Option<String>,
            _region: Option<Region>,
            _format: Option<ImageFormat>,
            _max_width: Option<u32>,
        ) -> zyris::Result<Datum> {
            Ok(Datum::Text { text: String::new(), format: None })
        }
    }

    fn guarded_input(
        dir: &std::path::Path,
    ) -> (crate::Gate, crate::AuditLog, Guarded<InputServer<FakeInput>>) {
        let gate = crate::Gate::running();
        let log = crate::AuditLog::new(dir.join("audit.jsonl"));
        let cap = Guarded::new(InputServer(FakeInput), gate.clone(), log.clone());
        (gate, log, cap)
    }

    fn guarded_screen(
        dir: &std::path::Path,
    ) -> (crate::Gate, crate::AuditLog, Guarded<ScreenCaptureServer<FakeScreen>>) {
        let gate = crate::Gate::running();
        let log = crate::AuditLog::new(dir.join("audit.jsonl"));
        let cap = Guarded::new(ScreenCaptureServer(FakeScreen), gate.clone(), log.clone());
        (gate, log, cap)
    }

    /// `file_transfer` with nothing behind it. The real one needs a bound endpoint and a live
    /// Attacca connection to look a peer up through, and without those every call would come back
    /// `Failed` — which would make a redaction assertion below prove only that a call that never
    /// ran logged nothing.
    struct FakeTransfer;

    /// What the peer says it wrote. A fact about the *other* machine, returned rather than asked
    /// for, and the test below is what keeps it out of a line describing what was asked for.
    const RECEIPT_PATH: &str = "/home/them/inbox/this-machine/renamed.txt";

    #[zyris::async_trait]
    impl FileTransfer for FakeTransfer {
        async fn send_to(
            &self,
            node: String,
            _path: String,
            _name: Option<String>,
            _overwrite: Option<bool>,
        ) -> zyris::Result<SendReceipt> {
            Ok(SendReceipt {
                node,
                written: RECEIPT_PATH.to_string(),
                bytes: 12,
                sha256: "de.ad".to_string(),
                replaced: false,
                undo: None,
                direct: true,
                pending: false,
                next: None,
            })
        }

        async fn inbox_list(&self) -> zyris::Result<Vec<InboxEntry>> {
            Ok(Vec::new())
        }
    }

    fn guarded_transfer(
        dir: &std::path::Path,
    ) -> (crate::Gate, crate::AuditLog, Guarded<FileTransferServer<FakeTransfer>>) {
        let gate = crate::Gate::running();
        let log = crate::AuditLog::new(dir.join("audit.jsonl"));
        let cap = Guarded::new(FileTransferServer(FakeTransfer), gate.clone(), log.clone());
        (gate, log, cap)
    }

    /// A capability built at runtime, under whatever name it is given, with one tool that takes
    /// anything and always succeeds.
    ///
    /// This is the shape `zyris-mcp`'s `Promoted` has: not produced by the capability macro, with
    /// a name chosen from a configuration file, and with a request schema that says nothing.
    /// Written here rather than depended on, because `zyris-tools` does not name `zyris-mcp` yet
    /// and what is under test is this file's rule and not that crate's.
    struct RuntimeCapability(&'static str);

    #[zyris::async_trait]
    impl ServeCapability for RuntimeCapability {
        fn descriptor(&self) -> CapabilityDescriptor {
            CapabilityDescriptor {
                name: self.0.to_string(),
                version: 1,
                tools: vec![zyris::ToolDescriptor {
                    name: "search".to_string(),
                    description: "Somebody else's tool.".to_string(),
                    transfer: zyris::Transfer::Unary,
                    request_schema: serde_json::json!({}),
                    response_schema: None,
                    item_schema: None,
                    call_limit: None,
                }],
            }
        }

        async fn dispatch(&self, _call: IncomingCall) -> zyris::Result<Outgoing> {
            zyris::encode_response(&serde_json::json!({ "ok": true }))
        }
    }

    fn guarded_runtime(
        dir: &std::path::Path,
        name: &'static str,
    ) -> (crate::AuditLog, Guarded<RuntimeCapability>) {
        let log = crate::AuditLog::new(dir.join("audit.jsonl"));
        let cap = Guarded::new(RuntimeCapability(name), crate::Gate::running(), log.clone());
        (log, cap)
    }

    /// Arguments belonging to somebody else's tool, spelled the way this file's allowlist
    /// happens to spell four of its own.
    fn foreign_arguments() -> serde_json::Value {
        serde_json::json!({
            "path": "/etc/shadow",
            "command": "psql -c 'select * from customers'",
            "name": "quarterly numbers",
            "recursive": true,
            "passphrase": "hunter2-do-not-log-me",
            "query": "everything about alice",
        })
    }

    #[tokio::test]
    async fn a_promoted_capability_writes_no_arguments_into_the_log() {
        // The MCP decision, by test rather than by assumption. A promoted tool's arguments are
        // arbitrary JSON from a third party; four of the names below collide with the allowlist
        // by spelling alone, and none of them means what the allowlist's reasoning assumed.
        let dir = tempfile::tempdir().unwrap();
        let (log, cap) = guarded_runtime(dir.path(), "mcp_desk-notes");

        cap.dispatch(call("search", foreign_arguments()))
            .await
            .expect("the call runs");

        let entry = &log.recent(1).unwrap()[0];
        assert_eq!(
            entry.outcome,
            crate::Outcome::Allowed,
            "the call has to have actually run, or this test proves nothing"
        );
        // The line is still written, and it still says what happened. What is missing is what was
        // asked, and anything telling a person about this log has to say so.
        assert_eq!(entry.capability, "mcp_desk-notes");
        assert_eq!(entry.tool, "search");
        assert_eq!(
            entry.detail, "",
            "a promoted tool's arguments reached the audit log: {}",
            entry.detail
        );
    }

    #[tokio::test]
    async fn the_allowlist_matches_on_spelling_alone_which_is_why_promoted_tools_are_exempt() {
        // The hazard the rule above exists to close, pinned so it cannot be rediscovered by
        // accident. The same arguments, under a name outside the promoted space: `summarize`
        // walks names and knows nothing about meaning, so four of them are written down.
        let dir = tempfile::tempdir().unwrap();
        let (log, cap) = guarded_runtime(dir.path(), "notes");

        cap.dispatch(call("search", foreign_arguments()))
            .await
            .expect("the call runs");

        let detail = &log.recent(1).unwrap()[0].detail;
        for spelled in ["path=/etc/shadow", "name=quarterly numbers", "recursive=true"] {
            assert!(
                detail.contains(spelled),
                "expected the allowlist to write `{spelled}`: {detail}"
            );
        }
        assert!(detail.contains("command=psql"), "{detail}");
        // The default is still "log nothing": a field nobody put on the list is not written down,
        // whatever it is called and whichever capability it arrived at.
        assert!(!detail.contains("hunter2"), "an unknown field was written down: {detail}");
        assert!(!detail.contains("alice"), "an unknown field was written down: {detail}");
    }

    #[test]
    fn the_descriptor_passes_through_unchanged() {
        // The decorator must be invisible on the wire: an agent asked for file_io v3 and that is
        // what it has to be told it got.
        let dir = tempfile::tempdir().unwrap();
        let (_gate, _log, cap) = guarded(dir.path());

        let descriptor = cap.descriptor();

        assert_eq!(descriptor.name, file_io_capability().name);
        assert_eq!(descriptor.version, file_io_capability().version);
        assert_eq!(descriptor.tools.len(), file_io_capability().tools.len());
    }

    #[tokio::test]
    async fn a_call_runs_and_is_recorded_when_the_gate_is_open() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "hi").unwrap();
        let (_gate, log, cap) = guarded(dir.path());

        let out = cap
            .dispatch(call("stat", serde_json::json!({ "path": "hello.txt" })))
            .await;

        assert!(out.is_ok());
        let recent = log.recent(1).unwrap();
        assert_eq!(recent[0].capability, "file_io");
        assert_eq!(recent[0].tool, "stat");
        assert_eq!(recent[0].outcome, crate::Outcome::Allowed);
    }

    #[tokio::test]
    async fn a_paused_gate_refuses_before_the_call_reaches_the_capability() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("gone.txt"), "hi").unwrap();
        let (gate, log, cap) = guarded(dir.path());
        gate.set_paused(true);

        let out = cap
            .dispatch(call("remove", serde_json::json!({ "path": "gone.txt" })))
            .await;

        assert!(out.is_err(), "a paused machine must not answer, even when it could");
        assert!(
            dir.path().join("gone.txt").exists(),
            "and must not have done the thing"
        );
        assert_eq!(log.recent(1).unwrap()[0].outcome, crate::Outcome::Refused);
    }

    #[tokio::test]
    async fn a_failing_call_is_recorded_as_failed_not_refused() {
        // A person reading the log has to be able to tell "I stopped it" from "it broke".
        let dir = tempfile::tempdir().unwrap();
        let (_gate, log, cap) = guarded(dir.path());

        let out = cap
            .dispatch(call("stat", serde_json::json!({ "path": "no-such-file" })))
            .await;

        assert!(out.is_err());
        assert_eq!(log.recent(1).unwrap()[0].outcome, crate::Outcome::Failed);
    }

    #[tokio::test]
    async fn the_recorded_detail_names_what_was_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "hi").unwrap();
        let (_gate, log, cap) = guarded(dir.path());

        let _ = cap
            .dispatch(call("stat", serde_json::json!({ "path": "hello.txt" })))
            .await;

        assert!(
            log.recent(1).unwrap()[0].detail.contains("hello.txt"),
            "a log that does not say which file was touched answers nothing"
        );
    }

    #[tokio::test]
    async fn a_recursive_remove_does_not_read_back_as_an_ordinary_one() {
        // The most destructive call this machine announces was the one the log could not
        // describe: `remove` branches on `recursive` into `remove_dir_all`, so a deleted tree and
        // a deleted empty directory wrote byte-identical lines.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("tree")).unwrap();
        std::fs::write(dir.path().join("tree").join("leaf.txt"), "hi").unwrap();
        std::fs::create_dir(dir.path().join("empty")).unwrap();
        let (_gate, log, cap) = guarded(dir.path());

        let tree = cap
            .dispatch(call(
                "remove",
                serde_json::json!({ "path": "tree", "recursive": true }),
            ))
            .await;
        let empty = cap
            .dispatch(call("remove", serde_json::json!({ "path": "empty" })))
            .await;

        assert!(tree.is_ok(), "the tree delete has to have run, or this test proves nothing");
        assert!(empty.is_ok(), "and so does the ordinary one");
        // Newest first, so the plain remove is [0] and the recursive one is [1].
        let recorded = log.recent(2).unwrap();
        assert_ne!(
            recorded[1].detail, recorded[0].detail,
            "a tree delete and an empty-directory delete wrote the same line: {}",
            recorded[0].detail
        );
        assert!(
            recorded[1].detail.contains("recursive=true"),
            "the line does not say a tree went: {}",
            recorded[1].detail
        );
        assert_eq!(recorded[0].detail, "path=empty");
    }

    #[tokio::test]
    async fn the_detail_never_carries_the_payload() {
        // write's params hold the file's contents. Those are exactly what must not be logged:
        // a log nobody dares hand over is not a log.
        let dir = tempfile::tempdir().unwrap();
        let (_gate, log, cap) = guarded(dir.path());

        let _ = cap
            .dispatch(call(
                "write",
                serde_json::json!({
                    "path": "secrets.txt",
                    "data": { "kind": "text", "text": "hunter2-do-not-log-me" },
                    "overwrite": true,
                }),
            ))
            .await;

        let entry = &log.recent(1).unwrap()[0];
        assert_eq!(
            entry.outcome,
            crate::Outcome::Allowed,
            "the write has to have actually run, or this test proves nothing"
        );
        assert!(entry.detail.contains("secrets.txt"));
        assert!(
            entry.detail.contains("overwrite=true"),
            "whether a file that already existed was replaced is part of what was asked for: {}",
            entry.detail
        );
        assert!(
            !entry.detail.contains("hunter2"),
            "the contents leaked into the audit log: {}",
            entry.detail
        );
    }

    #[tokio::test]
    async fn the_detail_never_carries_an_edit_s_strings() {
        // The other payload path. `edit` carries both the old and the new text as plain
        // arguments, which is how a password ends up in a params object.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("conf.txt"), "token = hunter2-do-not-log-me\n").unwrap();
        let (_gate, log, cap) = guarded(dir.path());

        let _ = cap
            .dispatch(call(
                "edit",
                serde_json::json!({
                    "path": "conf.txt",
                    "old_string": "hunter2-do-not-log-me",
                    "new_string": "hunter3-also-secret",
                    "replace_all": false,
                }),
            ))
            .await;

        let entry = &log.recent(1).unwrap()[0];
        assert_eq!(
            entry.outcome,
            crate::Outcome::Allowed,
            "the edit has to have actually run, or this test proves nothing"
        );
        let detail = &entry.detail;
        assert!(detail.contains("conf.txt"));
        assert!(!detail.contains("hunter2"), "an edit's strings leaked: {detail}");
        assert!(!detail.contains("hunter3"), "an edit's strings leaked: {detail}");
    }

    #[tokio::test]
    async fn an_absolute_path_reaches_outside_the_root() {
        // Not a jail, by decision. The root is where relative paths start, nothing more.
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("outside.txt");
        std::fs::write(&target, "hi").unwrap();
        let inside = tempfile::tempdir().unwrap();
        let (_gate, _log, cap) = guarded(inside.path());

        let out = cap
            .dispatch(call(
                "stat",
                serde_json::json!({ "path": target.to_string_lossy() }),
            ))
            .await;

        assert!(out.is_ok(), "absolute paths address the host directly; see the spec");
    }

    #[tokio::test]
    async fn a_call_reaches_the_bus_without_taking_over_the_catch_up_slot() {
        // Two things at once, because they fail together. The window learns about a call from
        // the bus, so a `Guarded` that only writes to disk shows nothing live; and a call that
        // went out through `publish` rather than `publish_transient` would leave a `toolCall`
        // sitting in the slot the window reads once at startup, which its reducer ignores.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "hi").unwrap();
        let bus = EventBus::new(8);
        let mut events = bus.subscribe();
        let cap = Guarded::new(
            FileIoServer(zyris_fs::LocalFileIo::rooted(dir.path())),
            crate::Gate::running(),
            crate::AuditLog::new(dir.path().join("audit.jsonl")),
        )
        .with_bus(bus.clone());

        let _ = cap
            .dispatch(call("stat", serde_json::json!({ "path": "hello.txt" })))
            .await;

        assert_eq!(
            events.recv().await.unwrap(),
            CoreEvent::ToolCall {
                capability: "file_io".to_string(),
                tool: "stat".to_string(),
                detail: "path=hello.txt".to_string(),
                outcome: "allowed".to_string(),
            }
        );
        assert_eq!(
            bus.latest(),
            None,
            "a tool call must not become the thing a late window catches up on"
        );
    }

    #[tokio::test]
    async fn the_detail_never_carries_typed_text() {
        // `type_text` is how a password gets typed. Not the text, and not its length either:
        // this log is meant to be handable to someone helping you.
        let dir = tempfile::tempdir().unwrap();
        let (_gate, log, cap) = guarded_input(dir.path());

        let _ = cap
            .dispatch(call("type_text", serde_json::json!({ "text": "hunter2-do-not-log-me" })))
            .await;

        let entry = &log.recent(1).unwrap()[0];
        assert_eq!(
            entry.outcome,
            crate::Outcome::Allowed,
            "the typing has to have actually run, or this test proves nothing"
        );
        let detail = &entry.detail;
        assert!(!detail.contains("hunter2"), "typed text leaked into the audit log: {detail}");
        assert!(!detail.contains("21"), "the length leaked, which is still a hint: {detail}");
    }

    #[tokio::test]
    async fn the_detail_never_carries_a_chord() {
        // A sequence of single-character chords types a password one keystroke at a time.
        let dir = tempfile::tempdir().unwrap();
        let (_gate, log, cap) = guarded_input(dir.path());

        let _ = cap.dispatch(call("key", serde_json::json!({ "chord": "h" }))).await;

        let recorded = &log.recent(1).unwrap()[0];
        assert_eq!(
            recorded.outcome,
            crate::Outcome::Allowed,
            "the keypress has to have actually run, or this test proves nothing"
        );
        assert_eq!(recorded.tool, "key", "the call still has to be recorded");
        assert!(!recorded.detail.contains("chord"), "the chord leaked: {}", recorded.detail);
    }

    #[tokio::test]
    async fn a_pointer_move_records_where_it_went() {
        // The opposite requirement: a log that cannot say where the pointer was driven answers
        // nothing about what happened to the machine.
        let dir = tempfile::tempdir().unwrap();
        let (_gate, log, cap) = guarded_input(dir.path());

        let _ = cap
            .dispatch(call(
                "move_to",
                serde_json::json!({ "display": "HDMI-1", "x": 640, "y": 480 }),
            ))
            .await;

        let entry = &log.recent(1).unwrap()[0];
        assert_eq!(
            entry.outcome,
            crate::Outcome::Allowed,
            "the move has to have actually run, or this test proves nothing"
        );
        let detail = &entry.detail;
        assert!(detail.contains("HDMI-1"), "which display is missing: {detail}");
        assert!(
            detail.contains("640") && detail.contains("480"),
            "the position is missing: {detail}"
        );
    }

    #[tokio::test]
    async fn a_screenshot_records_which_display_but_not_the_picture() {
        // All four of `screenshot`'s parameters are `Option`, so naming one of them is a
        // complete call. The picture is a return value, and `summarize` only ever reads params —
        // which is what keeps a picture of the screen out of a file meant to be handable.
        let dir = tempfile::tempdir().unwrap();
        let (_gate, log, cap) = guarded_screen(dir.path());

        let _ = cap
            .dispatch(call("screenshot", serde_json::json!({ "display": "HDMI-1" })))
            .await;

        let entry = &log.recent(1).unwrap()[0];
        assert_eq!(
            entry.outcome,
            crate::Outcome::Allowed,
            "the capture has to have actually run, or this test proves nothing"
        );
        assert!(entry.detail.contains("HDMI-1"), "which display is missing: {}", entry.detail);
    }

    #[tokio::test]
    async fn a_send_records_which_machine_got_which_file_under_what_name() {
        // The only line that will ever say a file left this machine. `send_to` is the one
        // announced tool whose whole effect is somewhere else, so a log that cannot name the
        // destination cannot answer the question it exists for.
        let dir = tempfile::tempdir().unwrap();
        let (_gate, log, cap) = guarded_transfer(dir.path());

        let _ = cap
            .dispatch(call(
                "send_to",
                serde_json::json!({
                    "node": "laptop",
                    "path": "notes/x.txt",
                    "name": "renamed.txt",
                    "overwrite": true,
                }),
            ))
            .await;

        let entry = &log.recent(1).unwrap()[0];
        assert_eq!(
            entry.outcome,
            crate::Outcome::Allowed,
            "the send has to have actually run, or this test proves nothing"
        );
        let detail = &entry.detail;
        assert!(detail.contains("node=laptop"), "which machine is missing: {detail}");
        assert!(detail.contains("path=notes/x.txt"), "which file is missing: {detail}");
        assert!(detail.contains("name=renamed.txt"), "what it landed as is missing: {detail}");
        assert!(
            detail.contains("overwrite=true"),
            "whether it replaced a file over there is part of what was asked for: {detail}"
        );
        assert!(
            !detail.contains(RECEIPT_PATH),
            "the receipt leaked into a line about what was asked for: {detail}"
        );
    }

    #[tokio::test]
    async fn reading_the_inbox_is_recorded_even_though_it_has_nothing_to_say() {
        // `inbox_list` takes no parameters, so the detail is empty and that is the right answer:
        // the tool name and the timestamp are still the record that something read what had
        // arrived. The alternative — dumping the params object when the allowlist matches
        // nothing — is where a payload would land.
        let dir = tempfile::tempdir().unwrap();
        let (_gate, log, cap) = guarded_transfer(dir.path());

        let out = cap.dispatch(call("inbox_list", serde_json::json!({}))).await;

        assert!(out.is_ok());
        let entry = &log.recent(1).unwrap()[0];
        assert_eq!(entry.capability, "file_transfer");
        assert_eq!(entry.tool, "inbox_list");
        assert_eq!(entry.detail, "");
    }

    /// A capability that starts and does not come back, so a test can interfere with a call while
    /// it is genuinely running rather than hoping it is.
    struct NeverAnswers {
        started: Arc<tokio::sync::Notify>,
    }

    #[zyris::async_trait]
    impl ServeCapability for NeverAnswers {
        fn descriptor(&self) -> CapabilityDescriptor {
            CapabilityDescriptor {
                name: "mcp_desk-notes".to_string(),
                version: 1,
                tools: vec![zyris::ToolDescriptor {
                    name: "search".to_string(),
                    description: "Somebody else's tool, and it does not answer.".to_string(),
                    transfer: zyris::Transfer::Unary,
                    request_schema: serde_json::json!({}),
                    response_schema: None,
                    item_schema: None,
                    call_limit: None,
                }],
            }
        }

        async fn dispatch(&self, _call: IncomingCall) -> zyris::Result<Outgoing> {
            self.started.notify_waiters();
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn a_call_that_never_finishes_leaves_no_line() {
        // **What the log does not record, pinned so the copy cannot drift back into claiming it
        // does.** A line is written when a call finishes; a call that is cut off before it
        // finishes writes nothing at all, because the task carrying it is aborted and an aborted
        // future does not run its continuation. `zyris-core` does exactly that on a peer's cancel,
        // on the connection going down, and on the capability being revoked — so this is the
        // ordinary way an MCP call ends when an agent gives up on a slow server, not an exotic
        // one.
        //
        // Asserted against a promoted name because that is where the sentence lives, but it is
        // `Guarded`'s behaviour and `terminal.exec` with no `timeout_ms` has it too.
        let dir = tempfile::tempdir().unwrap();
        let log = crate::AuditLog::new(dir.path().join("audit.jsonl"));
        let started = Arc::new(tokio::sync::Notify::new());
        let cap = Arc::new(Guarded::new(
            NeverAnswers { started: started.clone() },
            crate::Gate::running(),
            log.clone(),
        ));

        let waiting = started.notified();
        let calling = {
            let cap = cap.clone();
            tokio::spawn(async move { cap.dispatch(call("search", serde_json::json!({}))).await })
        };
        // Not a sleep: the call has to have reached the capability, or this would be asserting
        // about a call that never started.
        tokio::time::timeout(std::time::Duration::from_secs(5), waiting)
            .await
            .expect("the call reaches the capability");

        calling.abort();
        let ended = calling.await;
        assert!(ended.is_err_and(|why| why.is_cancelled()), "the call was cut off mid-flight");

        assert!(
            log.recent(10).unwrap().is_empty(),
            "a call cut off mid-flight wrote a line; the copy in Mcp.tsx and README.md says it \
             does not, and one of the two is now wrong"
        );
    }
}
