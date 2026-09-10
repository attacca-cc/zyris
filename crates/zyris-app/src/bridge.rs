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
