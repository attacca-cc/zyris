//! The windowed runtime: the same core as `headless`, with something watching it.
//!
//! Starting and stopping the core goes through `zyris_core::lifecycle`, the same entry point
//! `headless.rs` calls.
//!
//! Tauri owns the main thread and runs its own event loop: `app.run` never returns, on any
//! platform or exit path. Anything that must happen before the process ends — publishing
//! `ShuttingDown`, in particular — runs from inside its callback, on `RunEvent::Exit`, which
//! Tauri delivers right before the process goes away.

use tauri::{RunEvent, WindowEvent};
use zyris_core::{lifecycle, EventBus};

use crate::tray;

pub fn run(bus: EventBus, runtime: tokio::runtime::Handle) -> anyhow::Result<()> {
    tracing::info!("running with a window");

    let setup_bus = bus.clone();
    let app = tauri::Builder::default()
        // Must be registered first: a second launch has to reach the running instance before
        // anything else in this process starts.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            tracing::info!("a second instance was launched; focusing this one");
            tray::show_main_window(app);
        }))
        .manage(bus.clone())
        .manage(runtime)
        .setup(move |app| {
            tray::build(app.handle())?;
            // Published here, after the tray (and anything else `setup` does) is built, rather
            // than before the builder: `broadcast` never replays a send, so a subscriber wired
            // up during setup — the Status tab, from step 2 on — has to already exist when this
            // fires. Publishing earlier would return 0 and nobody would ever learn the core
            // started. Do not move this back above `setup`.
            lifecycle::start(&setup_bus);
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

    app.run(move |_app, event| match event {
        // `code: None` means the last window closed rather than someone asking to quit.
        // Quitting goes through the tray, which calls `app.exit(0)` and arrives here with a code.
        RunEvent::ExitRequested { api, code, .. } => {
            if code.is_none() {
                api.prevent_exit();
            }
        }
        // The event loop's final event, on every platform and every exit path — including the
        // tray's Quit. This is the only place in this function that runs after `app.run` starts,
        // since `app.run` itself never returns.
        RunEvent::Exit => {
            lifecycle::shutdown(&bus);
            tracing::info!("stopped");
        }
        _ => {}
    });

    Ok(())
}
