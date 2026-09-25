//! The windowed runtime: the same core as `headless`, with something watching it.
//!
//! Starting and stopping the core goes through `zyris_runtime::lifecycle`, the same entry point
//! `headless.rs` calls.
//!
//! Tauri owns the main thread and runs its own event loop: `app.run` never returns, on any
//! platform or exit path. Anything that must happen before the process ends — publishing
//! `ShuttingDown`, in particular — runs from inside its callback, on `RunEvent::Exit`, which
//! Tauri delivers right before the process goes away.
//!
//! **And it ends the process with `std::process::exit`, so nothing is ever dropped.** Everything
//! handed to `manage` lives until the process does and then simply stops existing: no destructor
//! runs, on any exit path. For most of what is managed that costs nothing. It costs something for
//! the MCP servers, which are child processes this run started — exiting closes their pipes, which
//! is enough for one that quits on end-of-file and not for one that ignores it, and such a server
//! is then left running and reparented until somebody finds it in a task manager. So
//! [`Servers::stop_all`] is called explicitly, from the two places this process leaves from.
//!
//! The push-to-talk key is the second thing of that kind and the cost is plainer still: on
//! Wayland the registration belongs to `xdg-desktop-portal` rather than to this process and stays
//! listed after it dies. [`close_hotkey`] goes on the same two lines, and the test at the bottom
//! of this file is what stops one of them being forgotten.

use tauri::{AppHandle, Manager, RunEvent, WindowEvent};
use zyris_runtime::connection::Connector;
use zyris_runtime::{lifecycle, CoreEvent, EventBus};
use zyris_runtime::LiveCapabilities;
use zyris_tools::{Servers, Tools, Transfers};

/// Stop the MCP servers this run started, and wait for them, before the process goes away.
///
/// Not a method on anything: it is the one-line bridge between Tauri's two exits and the
/// supervisor's own [`Servers::stop_all`], and it exists so that neither of those exits can be
/// the one somebody forgot. Blocking, because both callers are on the main thread with the event
/// loop already finished or never started — there is nothing left to keep responsive, and the wait
/// is what makes the stop mean anything.
fn stop_mcp_servers(servers: &Servers, runtime: &tokio::runtime::Handle) {
    runtime.block_on(servers.stop_all());
}

/// Give the push-to-talk key back, from the same two exits, before the process goes away.
///
/// The twin of [`stop_mcp_servers`], and it is here for a sharper reason than tidiness: a
/// GlobalShortcuts portal registration is held by `xdg-desktop-portal`, not by this process.
/// Killing Zyris leaves it listed by `hyprctl globalshortcuts` — measured — so there is something
/// to hand back that outlives us, which is not true of an X11 grab or a Windows `RegisterHotKey`.
///
/// Blocking, like its twin, and for the same reason: both callers are on the main thread with the
/// event loop already finished or never started.
fn close_hotkey(hotkey: &std::sync::Arc<dyn Hotkey>, runtime: &tokio::runtime::Handle) {
    runtime.block_on(hotkey.close());
}

use crate::cli::Mode;
use crate::confirm::Pending;
use crate::hotkey::Hotkey;
use crate::{bridge, hotkey, tray};

pub fn run(
    bus: EventBus,
    runtime: tokio::runtime::Handle,
    connector: Connector,
    tools: Tools,
    // What this node announces, and the only authority on it. The window's Tools screen reads it
    // through this handle rather than through a list `main` wrote down once, so a promoted MCP
    // server turned off, turned on, or dead is off that screen as soon as it is off the node.
    live: LiveCapabilities,
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
    // Speech. Built by `main` so that the instance's data directory names the file the answer to
    // "should this listen?" is kept in. It has opened nothing yet: `resume` below is what acts
    // on that stored answer, and it is called here and not in `headless.rs` because a run with
    // no window has no push-to-talk key for anybody to hold.
    voice: std::sync::Arc<zyris_voice::Voice>,
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

    // **Built here, on the main thread, before Tauri takes it over.** On Windows
    // `RegisterHotKey` posts `WM_HOTKEY` to a message-only window and only the thread that owns
    // that window ever dispatches to it; this is that thread, and `app.run` below is what will
    // pump it. Built before the builder rather than inside `setup` so that both of this
    // process's exits can hold a handle taken here — `state::<T>()` for something that was never
    // managed panics, and the last thing this program does is not the place to find that out.
    // Same shape as `exit_servers` just below, and it has the same consequence: a launch that
    // turns out to be the second one has already registered, so that branch has to give it back
    // too.
    //
    // Never fails. A desktop with no way to register a global key gets a `Hotkey` that says so,
    // for the reason `zyris-tools`'s `announce.rs` gives about a machine with no display server:
    // a control that cannot work is worse than an absent one.
    let hotkey = runtime.block_on(hotkey::start(&hotkey::Env::read()));
    tracing::info!(support = ?hotkey.describe(), "push-to-talk");

    let setup_bus = bus.clone();
    let setup_runtime = runtime.clone();
    let exit_hotkey = hotkey.clone();
    let setup_hotkey = hotkey.clone();

    // **The one thing that consumes the key**, and the whole of the wiring between the desktop
    // session and the audio stack. `hotkey::HotkeyEvent` and `zyris_voice::Push` are two types
    // on purpose — a global shortcut is a desktop concern and `--headless` has none — and this
    // is the one line that maps between them. `Push` is declared in `zyris-voice`'s `lib.rs`
    // rather than in its feature-gated session module precisely so that this line needs no
    // `#[cfg]`.
    //
    // Started whether or not anything is listening. A key pressed with the switch off is
    // discarded inside `Voice::push`; the alternative is a subscription that has to be taken and
    // dropped as the switch moves, which is a race with nothing to gain.
    let key_voice = voice.clone();
    let mut keys = hotkey.events();
    runtime.spawn(async move {
        while let Ok(event) = keys.recv().await {
            key_voice.push(match event {
                hotkey::HotkeyEvent::Pressed => zyris_voice::Push::Pressed,
                hotkey::HotkeyEvent::Released => zyris_voice::Push::Released,
            });
        }
    });

    let setup_voice = voice.clone();
    // For the other exit, the ordinary one. A handle taken here rather than looked up out of
    // Tauri's state inside the closure: a `state::<T>()` that was never managed panics, and the
    // last thing this program does is not the place to find that out.
    let exit_servers = servers.clone();
    let exit_runtime = runtime.clone();
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
    // refused by the lock when both point at the same server, which is the case that would
    // enrol twice; two runs pointed at *different* servers are two different instances and being
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
        // because it moves; it is a handle too, holding that same gate and that same log.
        .manage(tools)
        // The list that command actually reads. A handle on the announcement itself, not a copy
        // of it: what the Tools screen lists is what this node is serving at the moment it asks,
        // including the promoted MCP servers that come and go while it runs. Two capabilities
        // need a display server, and this does not re-ask it — the values that decision produced
        // are what is in here. See `zyris_runtime::LiveCapabilities::descriptors`.
        .manage(live)
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
        // The push-to-talk key, as a handle on the one this process registered. What the window
        // needs from it is `describe()`: whether a key can work on this desktop at all, and — on
        // Wayland, where no application is allowed to choose the key — the exact line the person
        // has to add to their compositor configuration. The Voice screen reads it.
        .manage(hotkey)
        // Speech, as a handle on the one this process built. The Voice screen reads everything
        // through it — the device list, the model on disk, whether a microphone is open — and
        // moves the one switch that opens one.
        .manage(voice)
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
            bridge::voice_state,
            bridge::set_voice_listening,
            bridge::set_voice_device,
            bridge::set_voice_speaker,
            bridge::set_speech_model,
            bridge::fetch_speech_model,
            bridge::fetch_voice_model,
            bridge::forget_speech_model,
            bridge::record_wake_take,
            bridge::clear_wake_word,
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
                    // **This run has already started its MCP servers**: `main` starts them before
                    // it builds anything with a window on it, because the first announcement has
                    // to be complete. Leaving here without stopping them is a second copy of every
                    // configured server left running, from a process that did nothing else.
                    stop_mcp_servers(&app.state::<Servers>(), &setup_runtime);
                    // And it has already registered the push-to-talk key, for the same reason:
                    // that happens above, before the builder. On Wayland the registration is the
                    // portal's rather than this process's and outlives it, so a launch that did
                    // nothing else still has one to hand back.
                    close_hotkey(&setup_hotkey, &setup_runtime);
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
                // `recover_from_refused_credential`, when a redial finds the credential revoked.
                // So this watcher can and does raise the window mid-session, hours into a run
                // that was working.
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

            // What the voice session says, on its own channel to the window. Not a `CoreEvent`:
            // every variant of that union is something the node did about its connection to
            // Attacca, and this is a microphone. Subscribed here, beside the bridge above and
            // before `resume` below, for the same reason — `broadcast` never replays a send to
            // a subscriber that shows up late, and a turn that happened before the window was
            // listening is a turn nobody would ever be told about.
            bridge::forward_voice(app.handle().clone(), setup_voice.events(), &setup_runtime);
            bridge::forward_traces(app.handle().clone(), setup_voice.traces(), &setup_runtime);

            // **Acting on an answer a person already gave.** Nothing is opened here unless the
            // stored settings say it was asked for on some earlier run; on a machine nobody has
            // turned this on, `resume` reads the file, finds `listen: false`, and returns.
            //
            // Spawned rather than blocked on: loading the speech model takes long enough to
            // notice, and `setup` is what stands between this process and a window on the
            // screen. The Voice screen reads the answer through `voice_state` whenever it is
            // opened, so nothing is lost by it finishing late.
            let resume_voice = setup_voice.clone();
            setup_runtime.spawn(async move { resume_voice.resume().await });

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
            // Before `shutdown`, which only publishes. These are processes, and this is the last
            // moment anything in this program can reach them.
            stop_mcp_servers(&exit_servers, &exit_runtime);
            // And the key. Nothing in this program is dropped on the way out — `app.run` ends
            // with `std::process::exit` — so a registration held by somebody else's daemon has
            // to be handed back on this line or not at all.
            close_hotkey(&exit_hotkey, &exit_runtime);
            lifecycle::shutdown(&bus);
            tracing::info!("stopped");
            // Tauri would call `std::process::exit` next; this does the same, minus the C++
            // exit handlers that crash a GPU voice build. See `zyris_voice::exit_process`.
            zyris_voice::exit_process(0);
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::hotkey::{HotkeyEvent, HotkeySupport};

    /// A hotkey that only records whether anything asked for it back.
    struct CountingHotkey {
        closed: Arc<AtomicUsize>,
        events: tokio::sync::broadcast::Sender<HotkeyEvent>,
    }

    impl Hotkey for CountingHotkey {
        fn describe(&self) -> HotkeySupport {
            HotkeySupport::Working { trigger: "Ctrl+Alt+Space".into(), release_confirmed: true }
        }

        fn events(&self) -> tokio::sync::broadcast::Receiver<HotkeyEvent> {
            self.events.subscribe()
        }

        fn close(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
            let closed = self.closed.clone();
            Box::pin(async move {
                closed.fetch_add(1, Ordering::SeqCst);
            })
        }
    }

    /// The helper has to *await* the close, not fire it and return: on the portal it is a D-Bus
    /// round trip, and the caller's next statement on both exits is the process ending.
    #[test]
    fn closing_the_hotkey_waits_for_it() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let closed = Arc::new(AtomicUsize::new(0));
        let hotkey: Arc<dyn Hotkey> = Arc::new(CountingHotkey {
            closed: closed.clone(),
            events: tokio::sync::broadcast::channel(4).0,
        });

        close_hotkey(&hotkey, runtime.handle());

        assert_eq!(closed.load(Ordering::SeqCst), 1, "the close ran, and this line waited for it");
    }

    /// **This process leaves from two places and both of them have to give everything back.**
    ///
    /// There is no way to reach either from a test: one is inside Tauri's `setup` and ends in
    /// `std::process::exit`, the other is the event loop's final callback, and `app.run` never
    /// returns on any platform. So this reads the source, which is the same thing `announce.rs`
    /// does to the README and for the same reason — the alternative is nothing at all noticing.
    ///
    /// It is deliberately not a count of call sites. A third exit added later that hands nothing
    /// back is exactly the mistake worth catching, and a count would pass for it.
    #[test]
    fn both_ways_out_of_this_process_hand_everything_back() {
        let source = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/gui.rs"),
        )
        .expect("gui.rs is readable from its own crate");

        // Each exit is read between the line that starts it and the line that ends it, so the
        // window really is that arm and not a neighbourhood that happens to be big enough.
        for (exit, start, end) in [
            (
                "the instance lock turned out to be taken",
                "another Zyris is already running",
                "std::process::exit(0);",
            ),
            ("the event loop's final event", "RunEvent::Exit =>", "lifecycle::shutdown("),
        ] {
            let at = source.find(start).unwrap_or_else(|| {
                panic!("`{start}` is no longer in gui.rs, so this test can no longer see {exit}")
            });
            let length = source[at..].find(end).unwrap_or_else(|| {
                panic!("`{exit}` no longer ends with `{end}`; this test has to be rewritten")
            });
            let arm = &source[at..at + length];
            for owed in ["stop_mcp_servers(", "close_hotkey("] {
                assert!(
                    arm.contains(owed),
                    "{exit} leaves without `{owed}`. Anything this process took from outside \
                     itself — a child process, a registration held by a desktop daemon — has to \
                     be handed back on every path out, because Tauri exits with \
                     `std::process::exit` and drops nothing."
                );
            }
        }
    }
}
