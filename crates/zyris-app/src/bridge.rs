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
use zyris_tools::{AuditLog, Entry, Gate};

/// The single channel. The payload is `CoreEvent`'s tagged JSON.
pub const EVENT_NAME: &str = "core-event";

/// Subscribes to the bus and republishes onto the webview for as long as the app lives.
pub fn forward(app: AppHandle, bus: EventBus, runtime: &tokio::runtime::Handle) {
    let mut events = bus.subscribe();
    runtime.spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    if let Err(error) = app.emit(EVENT_NAME, &event) {
                        tracing::warn!(%error, "could not emit a core event to the window");
                    }
                }
                // Lagged means a slow window missed events. The next one still arrives, and
                // dropping the oldest is the bus's intended behaviour, so this is not fatal.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::warn!(missed, "the window fell behind on core events");
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
/// about where the switch is. Published with the ordinary [`EventBus::publish`], unlike a tool
/// call: this is real state, and a window whose listener came up late has to catch up on it.
///
/// Not a method on `Gate`. The gate is a flag every wrapped capability reads on the request
/// path, and it stays a flag; who is told about a change is this layer's business.
pub fn apply_paused(gate: &Gate, bus: &EventBus, paused: bool) {
    gate.set_paused(paused);
    bus.publish(CoreEvent::Paused { paused });
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

/// The durable tail, newest first. Read from the audit file rather than from the bus, so the
/// list is not empty after a restart — live `toolCall` events only cover this run.
#[tauri::command]
pub fn recent_tool_calls(limit: usize, log: State<AuditLog>) -> Vec<Entry> {
    log.recent(limit)
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
}
