//! The windowed runtime: the same core as `headless`, with something watching it.
//!
//! Tauri owns the main thread and runs its own event loop, so this function does not return
//! until the application exits.

use zyris_core::{CoreEvent, EventBus};

pub fn run(bus: EventBus) -> anyhow::Result<()> {
    bus.publish(CoreEvent::Started);
    tracing::info!("running with a window");

    tauri::Builder::default()
        // The bus is managed state so a later step's Tauri command can subscribe to it without
        // the command owning any core state of its own.
        .manage(bus)
        .run(tauri::generate_context!())?;

    Ok(())
}
