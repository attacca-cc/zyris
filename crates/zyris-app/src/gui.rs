//! The windowed runtime: the same core as `headless`, with something watching it.
//!
//! Tauri owns the main thread and runs its own event loop, so this function does not return
//! until the application exits.

use tauri::{RunEvent, WindowEvent};
use zyris_core::{CoreEvent, EventBus};

use crate::tray;

pub fn run(bus: EventBus) -> anyhow::Result<()> {
    bus.publish(CoreEvent::Started);
    tracing::info!("running with a window");

    let app = tauri::Builder::default()
        // Must be registered first: a second launch has to reach the running instance before
        // anything else in this process starts.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            tracing::info!("a second instance was launched; focusing this one");
            tray::show_main_window(app);
        }))
        .manage(bus.clone())
        .setup(|app| {
            tray::build(app.handle())?;
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window must not stop the node. This is the whole point of the app,
            // so it is prevented here rather than left to a window flag someone can change.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
                tracing::info!("window hidden; the node keeps running");
            }
        })
        .build(tauri::generate_context!())?;

    app.run(|_app, event| {
        // `code: None` means the last window closed rather than someone asking to quit.
        // Quitting goes through the tray, which calls `app.exit(0)` and arrives here with a code.
        if let RunEvent::ExitRequested { api, code, .. } = event {
            if code.is_none() {
                api.prevent_exit();
            }
        }
    });

    bus.publish(CoreEvent::ShuttingDown);
    tracing::info!("stopped");
    Ok(())
}
