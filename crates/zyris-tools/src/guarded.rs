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
}

impl<C: ServeCapability> Guarded<C> {
    pub fn new(inner: C, gate: Gate, log: AuditLog) -> Guarded<C> {
        // `descriptor()` is not a field read. The capability macro regenerates every JSON schema
        // in the capability on each call — about a millisecond for `file_io` — so the name is
        // taken once here rather than on the request path.
        let capability = inner.descriptor().name;
        Guarded { inner, capability, gate, log, bus: None }
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

    async fn dispatch(&self, call: IncomingCall) -> zyris::Result<Outgoing> {
        // Both of these are read before the call is handed on, because `dispatch` consumes it.
        let tool = call.tool.clone();
        let detail = summarize(&call);

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
}
