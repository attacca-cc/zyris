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
use zyris_tools::{Servers, Tools, Transfers};

use crate::cli::Mode;
use crate::confirm::Pending;
use crate::{bridge, tray};

pub fn run(
    bus: EventBus,
    runtime: tokio::runtime::Handle,
    connector: Connector,
    tools: Tools,
    // Where a question about an unapproved peer waits. The same handle `main` gave the confirmer,
    // so what the window reads and answers is the question an agent's `send_to` is blocked on.
    pending: Pending,
    // File transfer, for the one thing the window does with it: listing what has arrived. A
    // handle on the same wiring the announced capability is, not a second one — see `main`. `None`
    // is a machine with no peer identity, which announces no `file_transfer` and has no inbox.
    transfers: Option<Transfers>,
    // The local MCP servers: what each is doing, and the switch that turns one on or off. The
    // same supervisor the core is already watching for deaths with, so the window and the node
    // cannot disagree about which servers are announced.
    servers: Servers,
    // What this run calls itself: `main`'s `instance_name`, the same string the keychain and the
    // audit log are named by. Passed in rather than recomputed, because the lock taken below has
    // to name the same instance those two do.
    instance: String,
    // Whether this window goes on the screen. Autostart installs `--minimized`, which builds
    // everything and shows nothing but the tray icon.
    mode: Mode,
    // Whether `--server` pointed this run at a development server, which is the one case two
    // Zyris windows are wanted on one machine at once. See the plugin registration below.
    dev_server: bool,
) -> anyhow::Result<()> {
    tracing::info!(hidden = !mode.shows_a_window(), "running with a window");

    let setup_bus = bus.clone();
    let setup_runtime = runtime.clone();
    let setup_gate = tools.gate().clone();
    let setup_pending = pending.clone();

    let mut builder = tauri::Builder::default();

    // **Skipped for a `--server` run, and that is what makes the flag work.** The plugin keys
    // on `app.config().identifier` and nothing else — one constant for every build of Zyris, on
    // both platforms, with no per-instance knob in 2.4.4. So a development window launched
    // beside a production one was caught by production's copy of the plugin, brought *its*
    // window forward, and exited without ever appearing. The instance lock taken in `setup`
    // below is already named per instance and has always let the two coexist; this was the one
    // thing still refusing them.
    //
    // What it costs: two `--server` windows are no longer refused by the plugin. They are still
    // refused by the lock when both point at the same server, which is the case that would mint
    // two nodes; two runs pointed at *different* servers are two different instances and being
    // able to have both is the point.
    //
    // Registered first when it is registered at all: a second launch has to reach the running
    // instance before anything else in this process starts.
    if dev_server {
        tracing::info!(
            "not registering the single-instance plugin: it keys on the bundle identifier, so a \
             --server window would be swallowed by a production one",
        );
    } else {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            tracing::info!("a second instance was launched; focusing this one");
            tray::show_main_window(app);
        }));
    }

    let app = builder
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
        // A handle on the slot, like the gate and the log above: what `pending_peer` reads and
        // `answer_peer` writes is the question the confirmer is waiting on, not a copy of it.
        .manage(pending)
        // The `Option` is managed as it is rather than only when it is `Some`, because the two
        // answers are not the same answer and Tauri has no way to ask whether a type was
        // registered: a machine with no peer identity has no inbox to read, which the window has
        // to say differently from an inbox nothing has arrived in. See `bridge::inbox`.
        .manage(transfers)
        // The MCP servers. A handle on the same supervisor the death watcher holds, for the same
        // reason the gate is: a window that read a second copy would show servers this node is
        // not announcing, and its switch would move something nothing else could see.
        .manage(servers)
        .invoke_handler(tauri::generate_handler![
            bridge::open_verification_url,
            bridge::latest_event,
            bridge::set_paused,
            bridge::is_paused,
            bridge::recent_tool_calls,
            bridge::announced_tools,
            bridge::autostart_state,
            bridge::set_autostart,
            bridge::pending_peer,
            bridge::answer_peer,
            bridge::inbox,
            bridge::peer_fingerprint,
            bridge::mcp_servers,
            bridge::set_mcp_server_enabled,
        ])
        .setup(move |app| {
            // Taken here, after the single-instance plugin above has already had first refusal:
            // a second GUI launch has to reach that plugin — which focuses the running window
            // and lets this process exit — rather than being turned away before Tauri even
            // starts. `main.rs` takes the very same lock, by the same name, for the headless
            // branch, where there is no plugin to reach first; see its comment.
            //
            // The name is the instance's rather than the product's, so a `--server` window can
            // run beside a production one instead of being refused by its lock. That is half
            // of what side-by-side needs; the other half is the single-instance plugin above,
            // which keys on the bundle identifier and so is skipped for such a run.
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
                // `NeedsEnrolment` comes from `connection.rs`'s `credential()`, which runs
                // before the first dial — and runs again long after one, through
                // `recover_from_dead_token` → `mint_node_token` → the arm for a credential
                // that cannot mint this node. So this watcher can and does raise the window
                // mid-session, hours into a run that was working.
                //
                // **That is the intended behaviour, not an oversight.** Bounding this to the
                // first dial would leave a short enrolment code — one that expires while
                // nobody looks at it — behind a tray icon that has never been mentioned, on a
                // machine that will not reconnect and does not say why. A window somebody has
                // to dismiss is the cheaper of the two surprises. Every other step of
                // onboarding follows `NeedsEnrolment`, `EnrolmentFailed` included, so the one
                // subscription covers the whole path either way.
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
                setup_pending.clone(),
                &setup_runtime,
            );

            // Registered in **both** modes, unlike the enrolment watcher above, and for a reason
            // that is not about `--minimized`: closing the window hides it rather than quitting
            // (see `on_window_event` below), so a run that started with a window on the screen
            // spends most of its life with no window on the screen. A question nobody is shown
            // refuses itself three quarters of a minute later, and the agent is told only that
            // the peer was not approved.
            raise_the_window_for_a_peer_question(
                app.handle().clone(),
                setup_bus.clone(),
                setup_pending.clone(),
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
/// expire — behind a tray icon nobody told them about. A later run has a credential and stays
/// hidden exactly as it was asked to, right up until the day that credential stops working:
/// re-enrolment publishes the same event mid-session and this raises the window then too, on
/// purpose. The caller's comment says why.
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

/// Put the window on the screen whenever a peer is waiting to be approved.
///
/// **Not one shot, unlike [`show_when_enrolment_needs_a_person`].** Enrolment happens once and is
/// then over; this happens every time an agent sends to a machine this one has not pinned, which
/// is once per new machine and again whenever a ledger is moved or rebuilt. The task lives for the
/// app's run.
///
/// **Called from a tokio worker, which is where this had to be established rather than assumed.**
/// `show`, `unminimize` and `set_focus` each go through `tauri-runtime-wry`'s `send_user_message`,
/// which compares the calling thread against the event loop's: on the main thread it handles the
/// message inline, and off it — here — it posts through the tao event loop proxy. So all three are
/// legal from here and are applied in the order they were sent. What changes off the main thread
/// is that they are *queued* rather than done: this function returns before the window is up, and
/// the proxy's only failure is the event loop being gone, which `show_main_window` already
/// discards.
///
/// **What `--minimized` costs is focus, not the window.** A run started hidden has a real window
/// all along — `tauri.conf.json` declares it `"visible": false` — so `show` maps it exactly as it
/// maps one that was closed into the tray. `set_focus` is the part that can quietly do nothing:
/// tao guards it on the window already being visible (`window.get_visible()` on Linux, the
/// `VISIBLE` flag on Windows), and on Linux `set_visible` only *queues* a request onto the GTK
/// main context, so the focus call that follows it in the same batch can still see a window that
/// has not been mapped yet. On Windows the flag is set inline on the event loop thread and the
/// focus goes through. Either way the window appears, which is what a fingerprint needs; on Linux
/// whether it comes to the front is the window manager's usual new-window policy.
///
/// That is why there is no desktop notification here and no `tauri-plugin-notification`: the
/// window is the only surface that can show 32 hex digits to compare, showing it works from here,
/// and a plugin plus its permissions would buy a second way of saying what this already says.
fn raise_the_window_for_a_peer_question(
    app: AppHandle,
    bus: EventBus,
    pending: crate::confirm::Pending,
    runtime: &tokio::runtime::Handle,
) {
    // Subscribed synchronously, before the connector is spawned, for the reason the caller's
    // comment gives: `broadcast` never replays a send to a subscriber that arrived after it.
    let mut events = bus.subscribe();

    runtime.spawn(async move {
        loop {
            match events.recv().await {
                Ok(CoreEvent::NeedsPeerApproval { label, .. }) => {
                    tracing::info!(
                        %label,
                        "opening the window: a machine this one has not approved is waiting"
                    );
                    tray::show_main_window(&app);
                }
                Ok(_) => {}
                // Tool calls can outrun this subscriber, and the bus drops a contiguous range of
                // the ring when they do — a question among them. It is published transiently, so
                // `bus.latest()` never holds it; the slot it actually lives in is the one to ask,
                // and it answers `None` unless a question is waiting this very moment. Worth the
                // four lines: the alternative is an agent blocked for three quarters of a minute
                // behind a window that was never raised.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::warn!(missed, "fell behind while watching for a peer question");
                    if pending.question().is_some() {
                        tray::show_main_window(&app);
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });
}
