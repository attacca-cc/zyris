//! The one place a core event becomes a Tauri event.
//!
//! Everything the core says arrives on one channel. Keeping [`CoreEvent`] to one event name means
//! the UI has a single subscription and a single switch, and it keeps core state out of Tauri
//! commands — the window asks for nothing but what it missed, and it asks for that exactly once,
//! right after it starts listening.
//!
//! There is a second name, [`RESYNC_EVENT_NAME`], and it carries nothing. It is not something the
//! core did; it is this layer saying "you fell behind, ask your questions again", which is the one
//! piece of news a `CoreEvent` cannot be. See [`forward`]'s lag arm.
//!
//! `app.emit` only reaches JS listeners already registered by the time it is called, and
//! `EventBus`'s broadcast channel never replays a send to a subscriber that shows up late. The
//! frontend's `listen()` call has to round-trip over IPC before it counts as registered, so
//! anything published in that gap — which in practice is everything, since the connector starts
//! publishing within microseconds of setup — would otherwise be lost for good. `latest_event`
//! is the way back: the window calls it once its listener is confirmed live, and folds whatever
//! it gets through the same reducer as a normal event.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use tauri::{AppHandle, Emitter, State};
use zyris_autostart::{Autostart, State as AutostartState};
use zyris_runtime::{CoreEvent, EventBus, LiveCapabilities};
use zyris_tools::{
    Announcement, AuditLog, Entry, Gate, InboxEntry, ServerList, ServerView, Servers, Tools,
    Transfers,
};

use crate::confirm::{Pending, Question};
use crate::hotkey::Hotkey;

/// The single channel. The payload is `CoreEvent`'s tagged JSON.
pub const EVENT_NAME: &str = "core-event";

/// "Ask everything again." No payload, because there is nothing to say beyond that.
///
/// **Not a [`CoreEvent`], deliberately.** Every variant of that enum is something the core did,
/// and falling behind is something the *window* did — inventing a core event to describe it would
/// put a fiction in the one vocabulary the tray, the log and the window all read. It is also not
/// something a headless run can experience: there is no webview to fall behind.
///
/// What it is for: the two screens that read their contents through a command and re-read when an
/// event tells them something moved. [`forward`]'s lag arm can name the catch-up value, the switch
/// and a waiting peer question, because each of those is held somewhere it can read. It cannot
/// name the MCP server change it dropped — those are published transiently and nothing keeps the
/// last one — so a window that fell behind would go on showing a dead server as running, and the
/// Tools screen would go on advertising its capability, until somebody clicked away and back.
pub const RESYNC_EVENT_NAME: &str = "core-resync";

/// The one place a waiting question becomes an event.
///
/// Two callers, which is why it is a function rather than a struct literal written twice: `main`
/// builds it when [`crate::confirm::WindowConfirmer`] asks, and [`forward`] rebuilds it for a
/// window that fell behind. A second literal would be a second chance to drop a field, and the
/// field most worth dropping is the one a person is meant to read.
pub fn peer_question_event(question: &Question) -> CoreEvent {
    CoreEvent::NeedsPeerApproval {
        id: question.id,
        label: question.label.clone(),
        fingerprint: question.fingerprint.clone(),
    }
}

/// Subscribes to the bus and republishes onto the webview for as long as the app lives.
pub fn forward(
    app: AppHandle,
    bus: EventBus,
    gate: Gate,
    // For the lag path below, and for nothing else. A question is published transiently, so it is
    // never in the catch-up slot — the only way to name it again is to ask the slot it lives in.
    pending: Pending,
    runtime: &tokio::runtime::Handle,
) {
    let mut events = bus.subscribe();
    runtime.spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    if let Err(error) = app.emit(EVENT_NAME, &event) {
                        tracing::warn!(%error, "could not emit a core event to the window");
                    }
                }
                // Lagged drops a contiguous range of whatever was in the ring, so the window
                // did not merely miss some tool calls — it may have missed the state change that
                // decides which screen it renders. Re-send what we can still name: the catch-up
                // value, and the switch read straight off the gate rather than off the bus,
                // because `Paused` is published transiently and is never in that slot.
                //
                // `reduce` is idempotent for every arm reachable this way, so re-emitting is
                // free. Without it a lost `Connected` leaves the window claiming "Not connected"
                // over a live link, with no command to ask again.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::warn!(missed, "the window fell behind on core events; resyncing");
                    for message in after_falling_behind(&bus, &gate, &pending) {
                        let sent = match &message {
                            Resend::Core(event) => app.emit(EVENT_NAME, event),
                            Resend::AskAgain => app.emit(RESYNC_EVENT_NAME, ()),
                        };
                        if let Err(error) = sent {
                            tracing::warn!(%error, ?message, "could not resync the window");
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// One thing a window that fell behind is told.
#[derive(Debug, PartialEq, Eq)]
pub enum Resend {
    /// A core event, named again on [`EVENT_NAME`].
    Core(CoreEvent),
    /// [`RESYNC_EVENT_NAME`]: everything that could not be named.
    AskAgain,
}

/// Everything to tell a window that missed a stretch of the bus.
///
/// **A list rather than four `emit` calls in a row, because the list is the claim.** What a
/// window that fell behind gets back is the whole of what stops it rendering something untrue,
/// and the one way to be wrong here is to leave something out — which is invisible in a sequence
/// of statements and plain in a value. `forward` emits whatever comes back; the tests below say
/// what has to be in it.
///
/// Each item is read off the thing that owns it rather than off the bus, because the bus is what
/// was just lost: the catch-up value is the only event it still holds, the switch is the gate's,
/// and a waiting peer question is [`Pending`]'s. `reduce` on the other side is idempotent for
/// every arm reachable this way, so re-sending something the window already had costs nothing.
///
/// [`Resend::AskAgain`] is last and is **not** conditional. It is for everything with no owner to
/// read: an MCP server change is published transiently and nothing keeps the last one, so there
/// is no way to say which server moved — only that the window's idea of them is worth nothing.
/// Sending it when nothing in fact changed costs two commands being re-read; not sending it when
/// something did leaves a dead server listed as running, and a capability advertised on the Tools
/// screen that this node has withdrawn, until somebody navigates away and back.
pub fn after_falling_behind(bus: &EventBus, gate: &Gate, pending: &Pending) -> Vec<Resend> {
    let mut messages = Vec::new();
    // Lagged drops a contiguous range of whatever was in the ring, so the window did not merely
    // miss some tool calls — it may have missed the state change that decides which screen it
    // renders. Without this a lost `Connected` leaves the window claiming "Not connected" over a
    // live link, with no command to ask again.
    if let Some(event) = bus.latest() {
        messages.push(Resend::Core(event));
    }
    // Off the gate, not the bus: `Paused` is published transiently and is never in that slot.
    messages.push(Resend::Core(CoreEvent::Paused { paused: gate.is_paused() }));
    // Losing this one costs more than losing a tool call — an agent's `send_to` is blocked on it,
    // and it answers itself with a refusal three quarters of a minute later if nobody is shown
    // it. Absent is the ordinary case and says nothing: `question()` is `None` whenever there is
    // no question, which is almost always.
    if let Some(question) = pending.question() {
        messages.push(Resend::Core(peer_question_event(&question)));
    }
    messages.push(Resend::AskAgain);
    messages
}

/// What the voice session said. Its own channel, carrying `zyris_voice::VoiceEvent`'s tagged JSON.
///
/// **Not a [`CoreEvent`]**, for the reason [`RESYNC_EVENT_NAME`] is not one: every variant of that
/// union is something the node did about its connection to Attacca, and this is a microphone. A
/// build with no audio stack has to be able to name the type either way, which is why
/// `VoiceEvent` lives in `zyris-voice`'s `lib.rs` and not behind its feature.
pub const VOICE_EVENT_NAME: &str = "voice-event";

/// Every step the audio took, for the Debug screen. [`VOICE_EVENT_NAME`]'s noisy neighbour.
pub const VOICE_TRACE_NAME: &str = "voice-trace";

/// Republish the diagnostic stream onto the webview.
///
/// **A second channel rather than more arms on `VoiceEvent`.** The product stream is what the
/// Voice screen renders as a state; this is what a person watching the pipeline reads, and the
/// two have different audiences, different volumes and different rules about wording.
///
/// A lag is reported and nothing is resynced: a diagnostic stream with a gap in it is still
/// worth reading, and there is no stored value that would fill the gap.
pub fn forward_traces(
    app: AppHandle,
    mut traces: tokio::sync::broadcast::Receiver<zyris_voice::Trace>,
    runtime: &tokio::runtime::Handle,
) {
    runtime.spawn(async move {
        loop {
            match traces.recv().await {
                Ok(step) => {
                    if let Err(error) = app.emit(VOICE_TRACE_NAME, &step) {
                        tracing::warn!(%error, "could not forward a voice trace to the window");
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::warn!(missed, "the window fell behind on voice traces");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// Republish what the voice session says onto the webview, for as long as the app lives.
///
/// **No catch-up half, deliberately, and the Voice screen is written to that.** A turn is four
/// events over a second or two and none of them is state: "what was heard three turns ago" is
/// not something a window that has just opened needs, and there is nothing on this side holding
/// it to hand back. What *is* state — whether a microphone is open, and why not — is read through
/// [`voice_state`], which the screen calls on the way in.
///
/// A lag is therefore not resynced either: losing a `Thinking` costs a label that catches up at
/// the next event, and there is no stored value that would make it right in the meantime.
pub fn forward_voice(
    app: AppHandle,
    mut events: tokio::sync::broadcast::Receiver<zyris_voice::VoiceEvent>,
    runtime: &tokio::runtime::Handle,
) {
    runtime.spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    if let Err(error) = app.emit(VOICE_EVENT_NAME, &event) {
                        tracing::warn!(%error, "could not forward a voice event to the window");
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::warn!(missed, "the window fell behind on voice events");
                }
                // A build with no audio stack hands out a stream that has already ended, which
                // is the whole point of it being closed rather than idle: this task leaves
                // rather than waiting forever for speech that can never arrive.
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// Everything the Voice screen draws, read off this machine in one go.
///
/// **One structure rather than two commands**, for the reason [`AutostartView`] gives: the two
/// halves have to agree. "Nothing is listening" and a hotkey answer fetched a round trip later
/// describe two different moments, and this is the screen where a person reads one against the
/// other to work out what to do next.
///
/// The hotkey half is `zyris-app`'s because a global shortcut is a desktop-session concern; the
/// rest is `zyris-voice`'s. Neither knows about the other, and this is the only place they meet.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceScreen {
    /// The microphone, the model on disk and the wake word.
    pub voice: zyris_voice::view::VoiceView,
    /// Whether a push-to-talk key can work on this desktop, and what the person has to do.
    /// Three answers — see [`crate::hotkey::HotkeySupport`] — and the window must not flatten
    /// them: on a Wayland desktop with a GlobalShortcuts portal the key has to be bound by hand,
    /// and on one without a portal there is nothing to bind.
    pub hotkey: crate::hotkey::HotkeySupport,
}

fn voice_screen(view: zyris_voice::view::VoiceView, hotkey: &Arc<dyn Hotkey>) -> VoiceScreen {
    VoiceScreen { voice: view, hotkey: hotkey.describe() }
}

/// What this machine can hear with, right now.
///
/// Read off the disk and the sound system on every call rather than remembered: a microphone can
/// be unplugged and the speech model deleted while this window is open, and both are things the
/// screen would otherwise be confidently wrong about.
#[tauri::command]
pub async fn voice_state(
    voice: State<'_, Arc<zyris_voice::Voice>>,
    hotkey: State<'_, Arc<dyn Hotkey>>,
) -> Result<VoiceScreen, String> {
    Ok(voice_screen(voice.look().await, &hotkey))
}

/// Turn listening on or off, and answer with what that left the machine as.
///
/// **What happened, not what was asked for** — the same rule [`set_mcp_server_enabled`] follows,
/// and the case that proves it is turning it on with no speech model downloaded: the answer comes
/// back with `listening` as `failed` and the reason in it, rather than the `on` that was clicked.
///
/// The answer is also *stored*, whichever way it went. A person who said "listen" has said it for
/// the next launch too; see `zyris_voice`'s `run` module for why that is the shape of this
/// decision and why nothing listens before it is made.
#[tauri::command]
pub async fn set_voice_listening(
    listening: bool,
    voice: State<'_, Arc<zyris_voice::Voice>>,
    hotkey: State<'_, Arc<dyn Hotkey>>,
) -> Result<VoiceScreen, String> {
    Ok(voice_screen(voice.set_listening(listening).await, &hotkey))
}

/// Choose which microphone to open, now and at the next launch.
#[tauri::command]
pub async fn set_voice_device(
    device: zyris_voice::view::Choice,
    voice: State<'_, Arc<zyris_voice::Voice>>,
    hotkey: State<'_, Arc<dyn Hotkey>>,
) -> Result<VoiceScreen, String> {
    Ok(voice_screen(voice.choose(device).await, &hotkey))
}

/// Choose which speaker answers are read through, now and at the next launch.
#[tauri::command]
pub async fn set_voice_speaker(
    speaker: zyris_voice::view::Choice,
    voice: State<'_, Arc<zyris_voice::Voice>>,
    hotkey: State<'_, Arc<dyn Hotkey>>,
) -> Result<VoiceScreen, String> {
    Ok(voice_screen(voice.choose_speaker(speaker).await, &hotkey))
}

/// Choose where speech is transcribed (`cpu`, `gpu:N`) and answers are read (`cpu`, `gpu`).
#[tauri::command]
pub async fn set_voice_compute(
    transcribe: Option<String>,
    speak: Option<String>,
    voice: State<'_, Arc<zyris_voice::Voice>>,
    hotkey: State<'_, Arc<dyn Hotkey>>,
) -> Result<VoiceScreen, String> {
    Ok(voice_screen(voice.choose_compute(transcribe, speak).await, &hotkey))
}

/// Choose how fast answers are read, now and at the next launch.
#[tauri::command]
pub async fn set_speaking_rate(
    rate: f32,
    voice: State<'_, Arc<zyris_voice::Voice>>,
    hotkey: State<'_, Arc<dyn Hotkey>>,
) -> Result<VoiceScreen, String> {
    Ok(voice_screen(voice.choose_speaking_rate(rate).await, &hotkey))
}

/// Choose which speech model listening uses, by id.
#[tauri::command]
pub async fn set_speech_model(
    id: String,
    voice: State<'_, Arc<zyris_voice::Voice>>,
    hotkey: State<'_, Arc<dyn Hotkey>>,
) -> Result<VoiceScreen, String> {
    Ok(voice_screen(voice.choose_model(id).await, &hotkey))
}

/// Download a speech model, by id.
///
/// `Err` is a download that did not produce the model — no network, a proxy's error page, a disk
/// with no room — carrying the sentence `zyris_voice::stt::Fault` gives. It can take minutes, so
/// the screen says what it is doing rather than showing a button that appears to do nothing.
#[tauri::command]
pub async fn fetch_speech_model(
    id: String,
    voice: State<'_, Arc<zyris_voice::Voice>>,
    hotkey: State<'_, Arc<dyn Hotkey>>,
) -> Result<VoiceScreen, String> {
    Ok(voice_screen(voice.fetch_model(id).await?, &hotkey))
}

/// Download the voice that reads answers aloud.
///
/// Sixteen files and about 401 MB, so it takes minutes and the screen says what it is doing.
/// `Err` carries the sentence `zyris_voice` gives, which on a machine where `ZYRIS_TTS_MODELS`
/// is set says that the directory is the operator's and Zyris does not write into it.
#[tauri::command]
pub async fn fetch_voice_model(
    voice: State<'_, Arc<zyris_voice::Voice>>,
    hotkey: State<'_, Arc<dyn Hotkey>>,
) -> Result<VoiceScreen, String> {
    Ok(voice_screen(voice.fetch_voice().await?, &hotkey))
}

/// Delete a downloaded speech model, by id.
///
/// Turns listening off on the way, because the running session holds the model open. It refuses
/// to delete a file `ZYRIS_WHISPER_MODEL` names — that one is an operator's own, and the window
/// does not offer the button for it.
#[tauri::command]
pub async fn forget_speech_model(
    id: String,
    voice: State<'_, Arc<zyris_voice::Voice>>,
    hotkey: State<'_, Arc<dyn Hotkey>>,
) -> Result<VoiceScreen, String> {
    Ok(voice_screen(voice.forget_model(id).await?, &hotkey))
}

/// Record one take of the wake word, from the chosen microphone.
///
/// What the recording is for is said by `zyris_voice::wake`'s `WHAT_THE_TAKES_DO`, carried in
/// the answer rather than written again here, because that
/// constant has a test on each of its claims and a second copy of the sentence would have none.
#[tauri::command]
pub async fn record_wake_take(
    voice: State<'_, Arc<zyris_voice::Voice>>,
    hotkey: State<'_, Arc<dyn Hotkey>>,
) -> Result<VoiceScreen, String> {
    Ok(voice_screen(voice.record_wake_take().await?, &hotkey))
}

/// Forget every take of the wake word.
#[tauri::command]
pub async fn clear_wake_word(
    voice: State<'_, Arc<zyris_voice::Voice>>,
    hotkey: State<'_, Arc<dyn Hotkey>>,
) -> Result<VoiceScreen, String> {
    Ok(voice_screen(voice.clear_wake_word().await?, &hotkey))
}

/// The account's projects, sessions and agents, and which session this machine talks to.
///
/// `Err` is a sentence the Conversation screen shows as it stands: most often that this machine
/// has not connected yet, so there is no account to read.
#[tauri::command]
pub async fn conversation_sessions(
    voice: State<'_, Arc<zyris_voice::Voice>>,
) -> Result<zyris_voice::view::SessionsView, String> {
    voice.sessions().await
}

/// Talk to another session from now on, and at the next launch.
#[tauri::command]
pub async fn choose_conversation_session(
    session: String,
    voice: State<'_, Arc<zyris_voice::Voice>>,
) -> Result<zyris_voice::view::SessionsView, String> {
    voice.choose_session(session).await
}

/// Start a session in a project against an agent, and talk to it from now on. Either may be
/// left out: the default project, and the account's only agent.
#[tauri::command]
pub async fn new_conversation_session(
    project: Option<String>,
    agent: Option<String>,
    voice: State<'_, Arc<zyris_voice::Voice>>,
) -> Result<zyris_voice::view::SessionsView, String> {
    voice.new_session(project, agent).await
}

/// What the core last published, for a window whose listener came up too late to see it live.
/// Meant to be called exactly once, right after `listen()` resolves — see the module doc comment.
#[tauri::command]
pub fn latest_event(bus: State<EventBus>) -> Option<CoreEvent> {
    bus.latest()
}

/// Opens the enrolment URL in the person's own browser. A command rather than a link because the
/// webview must not navigate away from the app.
#[tauri::command]
pub fn open_verification_url(url: String) -> Result<(), String> {
    // Only ever the URL the server issued, and only https. A command that opens whatever it is
    // handed is a command that opens whatever a compromised page hands it.
    if !url.starts_with("https://") {
        return Err("refusing to open a non-https url".to_string());
    }
    open::that(url).map_err(|error| error.to_string())
}

/// Moves the switch and tells everything watching, in that order.
///
/// The one path both the tray and the window go through, so the two can never end up disagreeing
/// about where the switch is.
///
/// Published **transiently**, like a tool call. The ordinary [`EventBus::publish`] also writes
/// the bus's one-slot catch-up value, and a window whose single `latest_event` landed on a
/// `Paused` would fold it into its initial state and render the starting screen — which has no
/// sidebar, so no route to the Tools tab and no way out. Nothing is lost by skipping the slot:
/// the tray and the forwarder still receive this live, and a window that opened later reads the
/// switch through [`is_paused`], which the Tools screen already calls on mount.
///
/// Not a method on `Gate`. The gate is a flag every wrapped capability reads on the request
/// path, and it stays a flag; who is told about a change is this layer's business.
pub fn apply_paused(gate: &Gate, bus: &EventBus, paused: bool) {
    gate.set_paused(paused);
    bus.publish_transient(CoreEvent::Paused { paused });
}

/// Stop or resume tool calls.
///
/// "Paused" means **no new calls**. A call already in flight is not stopped and an already-open
/// stream keeps delivering; `zyris_tools::gate` records exactly what the switch does and does
/// not cover, and the window's copy has to say the same thing.
#[tauri::command]
pub fn set_paused(paused: bool, gate: State<Gate>, bus: State<EventBus>) {
    apply_paused(&gate, &bus, paused);
}

/// Where the switch is right now. For a window that opened long after the last change and so
/// never saw the event that made it.
#[tauri::command]
pub fn is_paused(gate: State<Gate>) -> bool {
    gate.is_paused()
}

/// The peer question waiting for a person, if there is one.
///
/// The catch-up half of a pair, exactly as [`is_paused`] is to `Paused`: the event reaches a
/// window that was already listening, and this reaches one that was not. A window raised *by* the
/// question is the case that makes it necessary rather than tidy — `NeedsPeerApproval` is
/// published the instant `confirm` is called, and on a run started `--minimized` the webview may
/// not have finished registering its listener by then. An event alone would lose the question to
/// exactly the run that most needs it.
///
/// It is also how the window learns a question **ended**, which no event reports. A question
/// stops waiting three ways — answered, expired, or its caller cut off mid-`confirm` and the
/// future dropped — and the last of those runs inside a `Drop`, on cancellation, with no place to
/// publish from. So the screen re-asks while it is open, and `None` is the whole answer: a
/// question that is not waiting is not a question, whichever of the three ended it.
#[tauri::command]
pub fn pending_peer(pending: State<Pending>) -> Option<Question> {
    pending.question()
}

/// A person's answer to the peer question named by `id`.
///
/// **`false` is not a failure and must not be reported as one.** It means that question was no
/// longer waiting — it expired, the machine that asked gave up, or this is the second click on a
/// button that was never redrawn — and that nothing was pinned as a result. The window turns it
/// into a line saying the question has gone, not into an error.
///
/// Nothing is decided here. `Pending::answer` checks the id against the question actually
/// waiting and refuses an answer with nobody left to receive it; this command only carries.
#[tauri::command]
pub fn answer_peer(id: u64, approved: bool, pending: State<Pending>) -> bool {
    let reached = pending.answer(id, approved);
    // Written down because it is a decision a person made about what this machine will do, and
    // the audit log does not cover it — that records what an agent called, and this is the answer
    // underneath one such call. At `info` either way: "nobody was waiting" is as worth having in
    // the log as the answer itself when somebody later asks why a send failed.
    tracing::info!(
        id,
        approved,
        reached,
        "a person answered whether to send to a machine this one has not approved"
    );
    reached
}

/// What this machine announces **right now**, and the two paths that make the rest of the screen
/// readable.
///
/// A command rather than an event, and it breaks no rule about core state living behind one: the
/// capabilities were built in `main` and handed to the connector long before any window existed,
/// `--headless` announces exactly the same ones without ever calling this, and nothing the core
/// does depends on the answer.
///
/// **Read through [`LiveCapabilities`], which is the list every node is built from.** This used to
/// say the answer only changed when the app restarted, so asking once on the way into the Tools
/// screen was asking at the only moment that mattered. That stopped being true the moment a
/// promoted MCP server could be turned off, turned on, or die while this machine was connected:
/// the screen went on telling a person that agents could reach a capability this node had
/// withdrawn. There is now no snapshot to be stale — see [`Tools::announcement`] — and the window
/// asks again whenever the core says a server moved.
#[tauri::command]
pub async fn announced_tools(
    tools: State<'_, Tools>,
    live: State<'_, LiveCapabilities>,
) -> Result<Announcement, String> {
    Ok(tools.announcement(&live).await)
}

/// The durable tail, newest first. Read from the audit file rather than from the bus, so the
/// list is not empty after a restart — live `toolCall` events only cover this run.
///
/// Fallible on purpose. An unreadable log is not an empty one, and a window handed `[]` for it
/// would tell a person nothing has ever run on their machine — the one answer this command must
/// never give. A log that was never written is the exception and really is empty.
#[tauri::command]
pub fn recent_tool_calls(limit: usize, log: State<AuditLog>) -> Result<Vec<Entry>, String> {
    log.recent(limit).map_err(|error| error.to_string())
}

/// What has arrived in this machine's inbox, newest first.
///
/// **Three answers, and the window says something different for each.** `Err` is a read that
/// failed. `Ok(None)` is a machine with no peer identity: `file_transfer` is not announced, no
/// file can arrive, and there is no inbox to read. `Ok(Some(entries))` is the list, and an empty
/// one there is the only case that means nothing has arrived — the same distinction
/// [`recent_tool_calls`] draws, made with an `Option` rather than an empty vector because
/// "nothing can arrive here" and "nothing has" are two different sentences.
///
/// **Read through the capability rather than off the filesystem.** The layout under the inbox is
/// `zyris-transfer`'s — a directory per sending peer, `.part` for a transfer still in flight —
/// and a walker written on this side would be a second copy of those rules for upstream to drift
/// away from. `Transfers::inbox_list` is the same call an agent's `inbox_list` makes.
///
/// `async`, and deliberately **not** through [`off_the_ui_thread`]. Tauri runs a blocking command
/// inline on the IPC handler — the GTK main loop on Linux, the WebView2 UI thread on Windows —
/// and walking a directory there is the window not repainting while the disk is slow. The walk
/// underneath is `tokio::fs`, which yields to the runtime rather than occupying a thread, so this
/// belongs on the async runtime and not in the blocking pool the autostart commands need.
///
/// The clone out of state is for the borrow rather than the cost: `State<'_, T>` hands out a
/// reference tied to one call, and every clone of a `Transfers` is the same wiring anyway.
#[tauri::command]
pub async fn inbox(
    transfers: State<'_, Option<Transfers>>,
) -> Result<Option<Vec<InboxEntry>>, String> {
    let Some(transfers) = transfers.inner().clone() else {
        return Ok(None);
    };

    transfers.inbox_list().await.map(Some).map_err(|error| error.to_string())
}

/// This machine's own peer fingerprint, or `None` when it has no peer identity.
///
/// **The other half of [`pending_peer`], and the window had no way to show it.** The approval
/// screen tells a person to compare eight groups of four against what the *other* machine reports
/// for itself, and until this command existed the only place that value appeared was a
/// `tracing::info!` line at startup — which on an autostarted Windows node goes to a stdout nobody
/// is attached to. Following the instruction dead-ended, and a person who cannot find the other
/// side of a comparison approves blind, which is the one thing this whole module argues against.
///
/// Computed once at `Peering::bind` and held, so this is a clone of a `String` rather than a walk
/// of anything. It cannot fail and it never changes while the process runs: the same key means the
/// same fingerprint, which is the property `Peering` exists for.
///
/// `None` is the machine with no peer identity — the same one [`inbox`] answers `Ok(None)` for,
/// and for the same reason: there is no key, `file_transfer` is not announced, and an empty string
/// would read as a fingerprint made of nothing.
#[tauri::command]
pub fn peer_fingerprint(transfers: State<'_, Option<Transfers>>) -> Option<String> {
    transfers.inner().as_ref().map(|transfers| transfers.peering().fingerprint())
}

/// Every configured MCP server and what it is doing right now.
///
/// **The catch-up half of [`CoreEvent::McpServer`]**, exactly as [`is_paused`] is to `Paused`: the
/// event reaches a window that was already listening, and this reaches one that was not. It is a
/// command rather than state on the bus for the reason the module doc gives — a server change is
/// published transiently, so the bus's one-slot catch-up value never holds one.
///
/// It is also the only place the two withdrawals stay apart. An agent on the other end cannot tell
/// a server somebody turned off from one whose process fell over, and does not need to; the person
/// reading this can and does, through `ServerState`.
///
/// Read straight through the supervisor, which is the same one the core watches with — so what
/// this lists is what the node announces, not a second reader's idea of it.
///
/// **Three answers, and the window says something different for each** — the same rule [`inbox`]
/// follows. `ServerList::problem` set is a server list that could not be read; clear, with no
/// servers, is a machine nobody has configured. Assembled by the supervisor rather than here, so
/// the one thing that knows whether the file was readable is the one thing that says so.
///
/// `Result` for the shape of the boundary rather than for anything this can do: reading the list
/// cannot fail, and the window still has to handle a rejection, because an `invoke` that never
/// reaches here fails on the TypeScript side whatever this signature says.
#[tauri::command]
pub async fn mcp_servers(servers: State<'_, Servers>) -> Result<ServerList, String> {
    Ok(servers.view().await)
}

/// Turn one MCP server on or off, and answer with what that left it as.
///
/// It returns the server's state afterwards rather than `()`, for the same reason
/// [`apply_autostart`] does: a screen that renders its own request is a screen that can be wrong.
/// Turning one on is the case that proves it — the command may not be there any more, and the
/// answer is a `Failed` carrying the reason rather than the `Running` the click asked for.
///
/// So a server that would not start comes back here as `Ok`, not `Err`. `Err` is a request that
/// could not be acted on at all, which is one thing: a name the server list does not have.
///
/// **Nothing is written to the server list on disk, and the window says so.** This switch lasts
/// as long as this run; the file decides what the next one starts. `zyris_tools::Servers::
/// set_enabled` records the three things that decided that, the first of which is that the file is
/// read once at startup — so a write-back would put a stale snapshot over whatever a person has
/// edited since. `ui/src/Mcp.tsx` is where it is said out loud, which is the half that makes it a
/// decision rather than a screen that forgets.
#[tauri::command]
pub async fn set_mcp_server_enabled(
    name: String,
    enabled: bool,
    servers: State<'_, Servers>,
) -> Result<ServerView, String> {
    servers.set_enabled(&name, enabled).await
}

/// Everything the Settings screen needs to draw the autostart switch, read off the machine in
/// one go.
///
/// One structure rather than three commands, because the three answers have to agree. A caveat
/// fetched a round trip after the state it qualifies describes a machine that may have moved in
/// between, and "on, and it will stop when you log out" is exactly the pair that must not come
/// apart.
///
/// The field names and the shape of `state` are what `ui/src/Settings.tsx` transcribes; a test
/// below pins the JSON, and nothing checks the two halves at build time.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutostartView {
    /// Read back off the machine every time, never remembered — someone can remove the unit or
    /// the task by hand while this window is open.
    pub state: AutostartState,
    /// Everything true of this machine that leaves the switch weaker than "on" suggests. The
    /// window renders all of them; on Linux that sentence is the difference between "connected
    /// whenever this computer is on" and "connected whenever somebody is logged in".
    pub caveats: Vec<String>,
    /// What is, or would be, installed — named so a person can find it without Zyris.
    pub mechanism: Option<String>,
}

/// What the switch reads right now.
///
/// **One read of the machine, not two.** `state` is asked once and handed down to `caveats`,
/// which used to go and ask for it again — on Linux that was a second `systemctl --user
/// is-enabled` for every look, and two reads are two answers that can disagree. "On, and
/// nothing is running from it" is exactly the pair that must not come apart.
pub fn look(autostart: &Autostart) -> anyhow::Result<AutostartView> {
    let state = autostart.state()?;

    Ok(AutostartView {
        caveats: autostart.caveats(&state),
        mechanism: autostart.mechanism(),
        state,
    })
}

/// Move the switch, then answer with what the machine says afterwards.
///
/// It returns the state it *ended in* rather than `()`, for the same reason [`apply_paused`]
/// makes the window read the gate: a screen that renders its own request is a screen that can
/// be wrong. Turning autostart on is the case that proves it — on Linux the unit is enabled and
/// still only starts Zyris at a desktop login, which is [`AutostartView::caveats`], and nothing
/// about the request would have said so.
///
/// The one path the CLI flags and the window both take, so `--install-autostart` and the switch
/// cannot install different things.
pub fn apply_autostart(autostart: &Autostart, enabled: bool) -> anyhow::Result<AutostartView> {
    if enabled {
        let exe = installed_executable()?;
        // **Said out loud, at `info`, because this is the one decision here that can rot
        // silently.** From a checkout this is `target/debug/zyris`, and the day someone runs
        // `cargo clean` their machine stops starting Zyris with no message anywhere. Zyris does
        // not detect that and refuse — somebody testing this needs it to work — so the path is
        // printed instead, and this line is what a person has to go on later.
        tracing::info!(
            executable = %exe.display(),
            "installing autostart for this executable; if that path stops existing, so does this",
        );
        autostart.enable(&exe)?;
    } else {
        autostart.disable()?;
    }

    look(autostart)
}

/// The executable a unit file or a task document should name: this one.
///
/// There is no better answer. An installed build's `current_exe()` is where the installer put
/// it, which is exactly right; a development build's is under `target/`, which is right until
/// it is deleted. Guessing at an install location Zyris was not started from would install a
/// path that has never worked, which is worse than one that works today.
fn installed_executable() -> anyhow::Result<PathBuf> {
    std::env::current_exe().context("could not work out where this program is on disk")
}

/// Run a blocking autostart call somewhere other than the thread the window is drawn on.
///
/// **Both commands below are `async fn` for this and nothing else.** Tauri defaults a command to
/// `ExecutionContext::Blocking` and runs it inline on the IPC handler — the GTK main loop on
/// Linux, the WebView2 UI thread on Windows. Everything underneath here is `systemctl` or
/// `schtasks`: process spawns and waits, measured at 400–450 ms for one press of the switch, and
/// for that half-second the window neither repaints nor answers the mouse.
///
/// `spawn_blocking` rather than the one-word `#[tauri::command(async)]`, which would route this
/// through `async_runtime::spawn` and park one of a handful of tokio workers on a subprocess for
/// the whole round trip. Waiting on a child process is what the blocking pool is for.
async fn off_the_ui_thread<T, F>(work: F) -> Result<T, String>
where
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
    T: Send + 'static,
{
    match tauri::async_runtime::spawn_blocking(work).await {
        Ok(answer) => answer.map_err(|error| error.to_string()),
        // The blocking task panicked or was cancelled. Nothing the window can act on, but it is
        // still an answer: a switch that never gets one stays disabled with its spinner on.
        Err(error) => Err(format!("could not ask this machine about autostart: {error}")),
    }
}

/// Where autostart stands, for a Settings screen that has just opened.
///
/// `Arc` rather than the bare `Autostart` in Tauri's state, because the work has to outlive the
/// borrow: `State<'_, T>` hands out a reference tied to the command call, and `spawn_blocking`
/// takes a `'static` closure. See [`off_the_ui_thread`].
#[tauri::command]
pub async fn autostart_state(
    autostart: State<'_, Arc<Autostart>>,
) -> Result<AutostartView, String> {
    let autostart = Arc::clone(&autostart);

    off_the_ui_thread(move || look(&autostart)).await
}

/// Start Zyris with this computer, or stop doing that.
///
/// Answers with the state the machine ended in, which is what the window renders.
#[tauri::command]
pub async fn set_autostart(
    enabled: bool,
    autostart: State<'_, Arc<Autostart>>,
) -> Result<AutostartView, String> {
    let autostart = Arc::clone(&autostart);

    off_the_ui_thread(move || apply_autostart(&autostart, enabled)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_event_name_is_what_ui_src_state_ts_hardcodes() {
        // `ui/src/state.ts` has no way to import this constant across the IPC boundary, so it
        // duplicates the literal instead. Nothing else catches a rename on either side; this is
        // the Rust half of that guard, and the comment beside the TypeScript literal is the
        // other half.
        assert_eq!(EVENT_NAME, "core-event");
    }

    #[test]
    fn the_resync_name_is_what_ui_src_state_ts_hardcodes_and_is_not_the_other_one() {
        // Same agreement, second channel. The inequality is the half worth asserting: emitted
        // under the event name, an empty payload would reach the reducer as an action with no
        // `kind` and be swallowed by its `default` arm — a resync that silently did nothing,
        // which is exactly the failure this channel exists to fix.
        assert_eq!(RESYNC_EVENT_NAME, "core-resync");
        assert_ne!(RESYNC_EVENT_NAME, EVENT_NAME);
    }

    #[test]
    fn a_window_that_fell_behind_is_asked_to_read_again() {
        // **The item with no owner to read it off, and the reason this channel exists.** The
        // other three are recoverable because something holds them: the bus keeps one catch-up
        // value, the gate holds the switch, `Pending` holds a waiting question. An MCP server
        // change is published transiently and nothing keeps the last one, so a window that
        // missed one cannot be told which server moved — only that its idea of them is worth
        // nothing. Leave this out and such a window shows a dead server as running, and lists a
        // capability this node has withdrawn, until somebody navigates away and back.
        let bus = EventBus::new(8);
        let gate = Gate::running();
        let pending = Pending::new();

        let messages = after_falling_behind(&bus, &gate, &pending);

        assert!(
            messages.contains(&Resend::AskAgain),
            "a window that fell behind was told nothing about what could not be named: \
             {messages:?}"
        );
        // Last, so the screens re-read after they have been given everything that could be
        // named rather than in the middle of it.
        assert_eq!(messages.last(), Some(&Resend::AskAgain));
    }

    #[test]
    fn a_window_that_fell_behind_is_told_the_switch_and_whatever_the_bus_still_holds() {
        // The rest of the list, asserted here rather than left to `forward`, which needs a
        // `tauri::AppHandle` and so cannot be reached from a unit test at all. Every one of these
        // is something a window would otherwise render untruthfully: a lost `Connected` reads as
        // "Not connected" over a live link, and a lost `Paused` reads as a machine that is
        // running when it is not.
        let bus = EventBus::new(8);
        bus.publish(CoreEvent::Connected {
            node_id: "n-1".to_string(),
            node_name: "this-machine".to_string(),
        });
        let gate = Gate::running();
        gate.set_paused(true);

        let messages = after_falling_behind(&bus, &gate, &Pending::new());

        assert!(
            messages.iter().any(|m| matches!(m, Resend::Core(CoreEvent::Connected { .. }))),
            "{messages:?}"
        );
        assert!(
            messages.contains(&Resend::Core(CoreEvent::Paused { paused: true })),
            "the switch is read off the gate, not the bus, because `Paused` is transient: \
             {messages:?}"
        );
        // And nothing about a question, because there is not one. A window handed an absence
        // would have nothing to do with it; the reducer has no action that carries one.
        assert!(
            !messages
                .iter()
                .any(|m| matches!(m, Resend::Core(CoreEvent::NeedsPeerApproval { .. }))),
            "{messages:?}"
        );
    }

    #[test]
    fn moving_the_switch_leaves_the_catch_up_slot_alone() {
        // The window reads that slot exactly once, to learn what it missed while its listener
        // was being registered. A `Paused` sitting there folds into the initial state without
        // naming a screen, so the window renders "Starting." — which has no sidebar, and so no
        // route to the Tools tab and no way back out. The bus already keeps tool calls out of
        // the slot for the same reason; the switch belongs out of it too.
        let bus = EventBus::new(8);
        let gate = Gate::running();
        let connected = CoreEvent::Connected { node_id: "n_1".into(), node_name: "laptop".into() };
        bus.publish(connected.clone());

        apply_paused(&gate, &bus, true);

        assert!(gate.is_paused(), "the switch still has to move");
        assert_eq!(
            bus.latest(),
            Some(connected),
            "the switch took the catch-up slot; a cold window would strand on its starting screen"
        );
    }

    #[test]
    fn a_question_reaches_the_window_with_both_strings_untouched() {
        // The one thing this conversion can get wrong, and the way it would be got wrong: an
        // event built field by field in two places, one of which trims or re-cases the string a
        // person is about to compare against another machine's screen. Both surfaces the window
        // has — this event and the `pending_peer` command's `Question` — must hand over the same
        // characters, so both are asserted against the same constant.
        const FINGERPRINT: &str = "9F2A 41C7 0E83 BB15 6D04 A97E 22C1 5FB8";
        let question =
            Question { id: 3, label: "Kitchen-Pi".into(), fingerprint: FINGERPRINT.into() };

        assert_eq!(
            peer_question_event(&question),
            CoreEvent::NeedsPeerApproval {
                id: 3,
                label: "Kitchen-Pi".into(),
                fingerprint: FINGERPRINT.into(),
            }
        );
        assert_eq!(
            serde_json::to_value(&question).unwrap(),
            serde_json::json!({
                "id": 3,
                "label": "Kitchen-Pi",
                "fingerprint": FINGERPRINT,
            }),
            "ui/src/PeerConfirm.tsx transcribes these field names; nothing checks that at build time"
        );
    }

    #[test]
    fn the_inbox_rows_the_tools_screen_reads_are_snake_case_and_absent_is_not_empty() {
        // Two agreements with `ui/src/Tools.tsx`, neither of which anything checks at build time.
        //
        // The field names are the first, and they are the exception on this boundary.
        // `InboxEntry` comes from the protocol stack and derives a plain `Serialize` with no
        // `rename_all`, so the time the window renders is at `received_unix_ms` — snake_case,
        // unlike every structure this workspace writes and serializes camelCase. A screen that
        // reached for `receivedUnixMs` would get `undefined` and render every arrival at the
        // epoch.
        let entry = InboxEntry {
            from: "kitchen-pi".into(),
            name: "notes.txt".into(),
            bytes: 12,
            path: "/home/ada/.local/share/zyris/inbox/kitchen-pi/notes.txt".into(),
            received_unix_ms: 1_757_000_000_000,
        };

        assert_eq!(
            serde_json::to_value(&entry).unwrap(),
            serde_json::json!({
                "from": "kitchen-pi",
                "name": "notes.txt",
                "bytes": 12,
                "path": "/home/ada/.local/share/zyris/inbox/kitchen-pi/notes.txt",
                "received_unix_ms": 1_757_000_000_000u64,
            })
        );

        // The second is that `None` and an empty list do not arrive looking the same. `null` is
        // a machine with no peer identity, where nothing can arrive at all; `[]` is an inbox that
        // was read and is empty. The screen says a different sentence for each, and only the
        // second one is "Nothing has arrived yet."
        let nothing_can_arrive = serde_json::to_value(None::<Vec<InboxEntry>>).unwrap();
        let nothing_has = serde_json::to_value(Some(Vec::<InboxEntry>::new())).unwrap();

        assert_eq!(nothing_can_arrive, serde_json::json!(null));
        assert_eq!(nothing_has, serde_json::json!([]));
        assert_ne!(nothing_can_arrive, nothing_has);
    }

    #[test]
    fn the_path_installed_into_the_unit_is_absolute() {
        // Neither systemd nor Task Scheduler shares a working directory with whoever turned
        // the switch on, so a relative path there resolves somewhere nobody chose — and it
        // fails at logon, where there is nobody to read the error.
        let exe = installed_executable().unwrap();

        assert!(exe.is_absolute(), "{}", exe.display());
    }

    #[test]
    fn the_settings_screen_reads_these_field_names() {
        // `ui/src/Settings.tsx` transcribes this shape rather than importing it — there is no
        // way to share a type across the IPC boundary — so this is the Rust half of the
        // agreement. The externally tagged enum is the part worth pinning: two of the three
        // states are bare strings and the third is an object, and the screen switches on that.
        let view = AutostartView {
            state: AutostartState::Unsupported("no systemd".into()),
            caveats: vec!["it starts at a desktop login".into()],
            mechanism: Some("a systemd user unit named zyris.service".into()),
        };

        assert_eq!(
            serde_json::to_string(&view).unwrap(),
            r#"{"state":{"unsupported":"no systemd"},"caveats":["it starts at a desktop login"],"mechanism":"a systemd user unit named zyris.service"}"#
        );
        assert_eq!(
            serde_json::to_string(&AutostartView {
                state: AutostartState::Enabled,
                caveats: Vec::new(),
                mechanism: None,
            })
            .unwrap(),
            r#"{"state":"enabled","caveats":[],"mechanism":null}"#
        );
    }
}
