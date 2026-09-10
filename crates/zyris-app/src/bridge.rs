//! The one place a core event becomes a Tauri event.
//!
//! Everything the window knows arrives through this channel. Keeping it to one event name means
//! the UI has a single subscription and a single switch, and it keeps core state out of Tauri
//! commands — the window asks for nothing, it is told.

use tauri::{AppHandle, Emitter};
use zyris_core::EventBus;

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
