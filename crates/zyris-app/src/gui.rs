//! The windowed runtime: the same core as `headless`, with something watching it.
//!
//! Starting and stopping the core goes through `zyris_runtime::lifecycle`, the same entry point
//! `headless.rs` calls.
//!
//! Tauri owns the main thread and runs its own event loop: `app.run` never returns, on any
//! platform or exit path. Anything that must happen before the process ends — publishing
//! `ShuttingDown`, in particular — runs from inside its callback, on `RunEvent::Exit`, which
//! Tauri delivers right before the process goes away.

use tauri::{Manager, RunEvent, WindowEvent};
use zyris_runtime::connection::Connector;
use zyris_runtime::{lifecycle, EventBus};
use zyris_tools::Tools;

use crate::{bridge, tray};

pub fn run(
    bus: EventBus,
    runtime: tokio::runtime::Handle,
    connector: Connector,
    tools: Tools,
    // What this run calls itself: `main`'s `instance_name`, the same string the keychain and the
    // audit log are named by. Passed in rather than recomputed, because the lock taken below has
    // to name the same instance those two do.
    instance: String,
) -> anyhow::Result<()> {
    tracing::info!("running with a window");

    let setup_bus = bus.clone();
    let setup_runtime = runtime.clone();
    let app = tauri::Builder::default()
        // Must be registered first: a second launch has to reach the running instance before
        // anything else in this process starts.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            tracing::info!("a second instance was launched; focusing this one");
            tray::show_main_window(app);
        }))
        .manage(bus.clone())
        .manage(runtime)
        // Clones, not the `Tools` itself: both are handles on the state `main` built, so the
        // switch the window moves is the one every announced capability reads, and the log it
        // reads is the one they write. Managed here rather than passed into `tray::build` and
        // the commands separately, because Tauri's state is the only thing both a command and a
        // menu handler can reach.
        .manage(tools.gate().clone())
        .manage(tools.log().clone())
        // And the `Tools` itself, for the one command that asks what is announced. Managed last
        // because it moves; it is a handle too, holding that same gate and that same log, and it
        // reads the capability descriptors on demand rather than from a startup snapshot, so the
        // Tools screen cannot list something the connector did not actually announce.
        .manage(tools)
        .invoke_handler(tauri::generate_handler![
            bridge::open_verification_url,
            bridge::latest_event,
            bridge::set_paused,
            bridge::is_paused,
            bridge::recent_tool_calls,
            bridge::announced_tools,
        ])
        .setup(move |app| {
            // Taken here, after the single-instance plugin above has already had first refusal:
            // a second GUI launch has to reach that plugin — which focuses the running window
            // and lets this process exit — rather than being turned away before Tauri even
            // starts. `main.rs` takes the very same lock, by the same name, for the headless
            // branch, where there is no plugin to reach first; see its comment.
            //
            // The name is the instance's rather than the product's, so a `--server` window can
            // run beside a production one instead of being refused by its lock.
            //
            // `manage`d rather than kept as a local: a local here would drop, and release the
            // lock, the moment this closure returns — the guard has to live for the app's whole
            // run, not just its setup.
            match zyris_runtime::lock::InstanceLock::acquire(&instance) {
                Ok(Some(lock)) => {
                    app.manage(lock);
                }
                Ok(None) => {
                    tracing::info!("another Zyris is already running on this machine; exiting");
                    std::process::exit(0);
                }
                Err(error) => {
                    tracing::warn!(%error, "could not take the instance lock; continuing anyway");
                }
            }

            // After the `manage` calls above, which is what lets the tray reach the gate and the
            // bus: `tray::build` reads both out of Tauri's state to label its pause item and to
            // keep that label following the switch.
            tray::build(app.handle(), &setup_runtime)?;
            // The bridge must be subscribed before the connector can publish anything, and
            // before `lifecycle::start` below — for the same reason `headless.rs` subscribes
            // before either fires: the connector publishes `NeedsEnrolment` (or dials straight
            // away) within microseconds of being spawned, and `broadcast` never replays a send
            // to a subscriber that shows up late. `latest_event` covers the window's own late
            // `listen()`, but only if the bridge itself was already forwarding by the time these
            // events fired.
            bridge::forward(app.handle().clone(), setup_bus.clone(), &setup_runtime);
            // Published only now that the bridge above is already subscribed — publishing
            // earlier is a silent no-op, since `broadcast` never replays a send to a later
            // subscriber. Do not move this back above the bridge.
            lifecycle::start(&setup_bus);
            // The GUI has no async context of its own; this is what the shared runtime handle
            // from step 1 exists for.
            setup_runtime.spawn(connector.run());
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
