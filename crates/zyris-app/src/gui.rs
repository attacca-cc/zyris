//! The windowed runtime: the same core as `headless`, with something watching it.
//!
//! Starting and stopping the core goes through `zyris_runtime::lifecycle`, the same entry point
//! `headless.rs` calls.
//!
//! Tauri owns the main thread and runs its own event loop: `app.run` never returns, on any
//! platform or exit path. Anything that must happen before the process ends — publishing
//! `ShuttingDown`, in particular — runs from inside its callback, on `RunEvent::Exit`, which
//! Tauri delivers right before the process goes away.

use tauri::{AppHandle, Manager, RunEvent, WindowEvent};
use zyris_runtime::connection::Connector;
use zyris_runtime::{lifecycle, CoreEvent, EventBus};
use zyris_tools::Tools;

use crate::cli::Mode;
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
    // Whether this window goes on the screen. Autostart installs `--minimized`, which builds
    // everything and shows nothing but the tray icon.
    mode: Mode,
) -> anyhow::Result<()> {
    tracing::info!(hidden = !mode.shows_a_window(), "running with a window");

    let setup_bus = bus.clone();
    let setup_runtime = runtime.clone();
    let setup_gate = tools.gate().clone();
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
        // carries the snapshot `main` recorded when it handed the capabilities to the node, so
        // the Tools screen reports what was actually announced rather than what a fresh look
        // would say now. Two of the four need a display server, so those are not the same
        // question.
        .manage(tools)
        // Built here rather than in `main`: it holds nothing, remembers nothing and reads the
        // machine on every call, so there is no state for the two runtimes to share. Headless
        // has no switch to move, and the CLI flags build their own — before the instance lock,
        // where this process does not exist yet.
        //
        // In an `Arc` because the two commands that read it are `async` and hand the work to
        // `spawn_blocking`, which needs something that outlives the borrow Tauri's state gives
        // out for one call. See `bridge::off_the_ui_thread`.
        .manage(std::sync::Arc::new(zyris_autostart::Autostart::for_this_machine()))
        .invoke_handler(tauri::generate_handler![
            bridge::open_verification_url,
            bridge::latest_event,
            bridge::set_paused,
            bridge::is_paused,
            bridge::recent_tool_calls,
            bridge::announced_tools,
            bridge::autostart_state,
            bridge::set_autostart,
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
            //
            // Built in both modes, and that is the point of `--minimized`: a process started by
            // autostart with no tray is a process nobody can open Zyris on.
            tray::build(app.handle(), &setup_runtime)?;

            // `tauri.conf.json` declares this window `"visible": false`, so this call is what
            // puts it on the screen. It has to be this way round: Tauri builds a
            // config-declared window *before* this closure runs, so a visible window hidden
            // here would appear and then vanish at every sign-in.
            //
            // Below the lock check on purpose. A second launch exits from that arm having shown
            // nothing, and the first process's `tauri_plugin_single_instance` callback is what
            // brings the running window forward.
            if mode.shows_a_window() {
                tray::show_main_window(app.handle());
            } else {
                // Onboarding is the one case a hidden window cannot be left hidden through: the
                // enrolment code is the single screen a person *must* see, and a tray icon that
                // has never been mentioned is not a way of telling them it is there.
                //
                // Driven off the event rather than off a second look at the keychain, so it
                // says what the core actually decided instead of guessing at it — and so the
                // keychain is read once, by the one thing that owns it.
                //
                // `NeedsEnrolment` is published once, before the first dial, and every other
                // step of onboarding follows it, `EnrolmentFailed` included. So this covers the
                // whole path without ever raising a window over somebody's work mid-session.
                show_when_enrolment_needs_a_person(
                    app.handle().clone(),
                    setup_bus.clone(),
                    &setup_runtime,
                );
            }

            // The bridge must be subscribed before the connector can publish anything, and
            // before `lifecycle::start` below — for the same reason `headless.rs` subscribes
            // before either fires: the connector publishes `NeedsEnrolment` (or dials straight
            // away) within microseconds of being spawned, and `broadcast` never replays a send
            // to a subscriber that shows up late. `latest_event` covers the window's own late
            // `listen()`, but only if the bridge itself was already forwarding by the time these
            // events fired.
            bridge::forward(
                app.handle().clone(),
                setup_bus.clone(),
                setup_gate.clone(),
                &setup_runtime,
            );
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

/// Put the window on the screen the moment the core says this machine is not enrolled yet.
///
/// Only registered for a run that started hidden. A first run under `--minimized` would
/// otherwise keep the one screen a person has no way around — a short code and a link, which
/// expire — behind a tray icon nobody told them about. Every later run has a credential, says
/// nothing here, and stays hidden exactly as it was asked to.
///
/// One shot: the task ends as soon as it has shown the window, so nothing here can raise a
/// window twice or fight with somebody who closed it.
fn show_when_enrolment_needs_a_person(
    app: AppHandle,
    bus: EventBus,
    runtime: &tokio::runtime::Handle,
) {
    // Subscribed here, synchronously, rather than inside the task: the connector publishes
    // `NeedsEnrolment` within microseconds of being spawned and `broadcast` never replays a
    // send to a subscriber that arrived after it. The caller runs this before
    // `lifecycle::start` and before the connector is spawned, for that reason.
    let mut events = bus.subscribe();

    runtime.spawn(async move {
        loop {
            match events.recv().await {
                Ok(CoreEvent::NeedsEnrolment) => {
                    tracing::info!(
                        "this computer is not enrolled yet, so the window opens even though \
                         this run was asked to start hidden",
                    );
                    tray::show_main_window(&app);
                    return;
                }
                Ok(_) => {}
                // Nothing can outrun this subscriber before enrolment — the only events before
                // a connection are the handful onboarding publishes, and tool calls cannot
                // happen until there is a link. Handled anyway, off the catch-up slot, because
                // the cost of being wrong is a person staring at a machine that will never
                // connect and never says why.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::warn!(missed, "fell behind while watching for enrolment");
                    if matches!(bus.latest(), Some(CoreEvent::NeedsEnrolment)) {
                        tray::show_main_window(&app);
                        return;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });
}
