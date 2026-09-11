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

use tauri::{AppHandle, Emitter, State};
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
}
