//! The one place a core event becomes a Tauri event.
//!
//! Everything the window knows arrives through this channel. Keeping it to one event name means
//! the UI has a single subscription and a single switch, and it keeps core state out of Tauri
//! commands — the window asks for nothing but what it missed, and it asks for that exactly once,
//! right after it starts listening.
//!
//! `app.emit` only reaches JS listeners already registered by the time it is called, and
//! `EventBus`'s broadcast channel never replays a send to a subscriber that shows up late. The
//! frontend's `listen()` call has to round-trip over IPC before it counts as registered, so
//! anything published in that gap — which in practice is everything, since the connector starts
//! publishing within microseconds of setup — would otherwise be lost for good. `latest_event`
//! is the way back: the window calls it once its listener is confirmed live, and folds whatever
//! it gets through the same reducer as a normal event.

use std::path::PathBuf;

use anyhow::Context as _;
use tauri::{AppHandle, Emitter, State};
use zyris_autostart::{Autostart, State as AutostartState};
use zyris_runtime::{CoreEvent, EventBus};
use zyris_tools::{Announcement, AuditLog, Entry, Gate, Tools};

/// The single channel. The payload is `CoreEvent`'s tagged JSON.
pub const EVENT_NAME: &str = "core-event";

/// Subscribes to the bus and republishes onto the webview for as long as the app lives.
pub fn forward(app: AppHandle, bus: EventBus, gate: Gate, runtime: &tokio::runtime::Handle) {
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
                    if let Some(event) = bus.latest() {
                        if let Err(error) = app.emit(EVENT_NAME, &event) {
                            tracing::warn!(%error, "could not resend the catch-up event");
                        }
                    }
                    let paused = CoreEvent::Paused { paused: gate.is_paused() };
                    if let Err(error) = app.emit(EVENT_NAME, &paused) {
                        tracing::warn!(%error, "could not resend the switch");
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
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

/// What this machine announces, and the two paths that make the rest of the screen readable.
///
/// A command rather than an event, and it breaks no rule about core state living behind one: the
/// capabilities were built in `main` and handed to the connector long before any window existed,
/// `--headless` announces exactly the same ones without ever calling this, and nothing the core
/// does depends on the answer. It only changes when the app restarts, so a window that asks once
/// when the Tools screen opens is asking at the only moment that matters.
#[tauri::command]
pub fn announced_tools(tools: State<Tools>) -> Announcement {
    tools.announcement()
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
    /// window renders all of them; on Linux with lingering off, that sentence is the difference
    /// between "connected always" and "connected until you log out".
    pub caveats: Vec<String>,
    /// What is, or would be, installed — named so a person can find it without Zyris.
    pub mechanism: Option<String>,
}

/// What the switch reads right now.
pub fn look(autostart: &Autostart) -> anyhow::Result<AutostartView> {
    Ok(AutostartView {
        state: autostart.state()?,
        caveats: autostart.caveats(),
        mechanism: autostart.mechanism(),
    })
}

/// Move the switch, then answer with what the machine says afterwards.
///
/// It returns the state it *ended in* rather than `()`, for the same reason [`apply_paused`]
/// makes the window read the gate: a screen that renders its own request is a screen that can
/// be wrong. Turning autostart on is the case that proves it — on Linux the unit is enabled and
/// lingering can still fail, which is [`AutostartView::caveats`], and nothing about the request
/// would have said so.
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

/// Where autostart stands, for a Settings screen that has just opened.
#[tauri::command]
pub fn autostart_state(autostart: State<Autostart>) -> Result<AutostartView, String> {
    look(&autostart).map_err(|error| error.to_string())
}

/// Start Zyris with this computer, or stop doing that.
///
/// Answers with the state the machine ended in, which is what the window renders.
#[tauri::command]
pub fn set_autostart(
    enabled: bool,
    autostart: State<Autostart>,
) -> Result<AutostartView, String> {
    apply_autostart(&autostart, enabled).map_err(|error| error.to_string())
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
            caveats: vec!["this user does not linger".into()],
            mechanism: Some("a systemd user unit named zyris.service".into()),
        };

        assert_eq!(
            serde_json::to_string(&view).unwrap(),
            r#"{"state":{"unsupported":"no systemd"},"caveats":["this user does not linger"],"mechanism":"a systemd user unit named zyris.service"}"#
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
