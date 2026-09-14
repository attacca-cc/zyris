//! The backend for an X11 session and for Windows: one key, grabbed from the session.
//!
//! Named for X11 because that is the half this project can reach — but it is the Windows backend
//! too, and deliberately the same code: `global-hotkey` is one crate with a `platform_impl` per
//! system, and having two wrappers over it would be two places for the de-duplication rule to be
//! forgotten.
//!
//! **On Windows this must be built on the thread that runs the message loop.** `RegisterHotKey`
//! posts `WM_HOTKEY` to a message-only window created by `GlobalHotKeyManager::new`, and only the
//! thread that owns that window ever dispatches to it. In this program that is the main thread,
//! before `tauri::App::run` takes it over — see `gui::run`. Built anywhere else the registration
//! succeeds and no event arrives, which is the same silent failure this module exists to avoid.

use std::sync::Arc;

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use tokio::sync::broadcast;

use super::{Closing, Hotkey, HotkeyEvent, HotkeySupport, OnePerHold, TRIGGER};

/// The manager, and the one platform where it needs help crossing a thread.
///
/// On Linux it is a `crossbeam` sender to its own X11 thread and is `Send + Sync` on its own. On
/// Windows it is a raw `HWND`, which is neither.
///
/// SAFETY (Windows only): the window is created once, on the main thread, and this value is then
/// only read — `unregister` in [`GrabbedHotkey::close`], which `gui.rs` calls from the main
/// thread at `RunEvent::Exit`. `Drop` would call `DestroyWindow` from wherever the last `Arc`
/// died, and cross-thread `DestroyWindow` fails rather than corrupting anything; it also never
/// runs, because Tauri ends the process with `std::process::exit`. This is the same assertion
/// `tauri-plugin-global-shortcut` makes about the same value.
struct Manager(GlobalHotKeyManager);

#[cfg(any(target_os = "windows", target_os = "macos"))]
unsafe impl Send for Manager {}
#[cfg(any(target_os = "windows", target_os = "macos"))]
unsafe impl Sync for Manager {}

/// A key grabbed from an X11 session or from Windows.
pub struct GrabbedHotkey {
    manager: Manager,
    hotkey: HotKey,
    /// An `Arc` because the process-global event handler below outlives this value: nothing in
    /// `global-hotkey` takes a handler back, so the closure has to be able to survive the
    /// `GrabbedHotkey` that installed it.
    events: Arc<OnePerHold>,
}

impl GrabbedHotkey {
    /// Take the key.
    ///
    /// Fails where the session will not give it: no X server, or a combination another
    /// application already holds (`ERROR_HOTKEY_ALREADY_REGISTERED` on Windows,
    /// `BadAccess` on X11). Both are real answers and the caller turns them into
    /// [`super::NoHotkey`] — a key another program owns is not a key this one has.
    ///
    /// **It does not fail on a Wayland session with Xwayland running**, which is why nothing
    /// here is allowed to decide the backend. See the module documentation on `hotkey/mod.rs`.
    pub fn grab() -> anyhow::Result<GrabbedHotkey> {
        let manager = GlobalHotKeyManager::new()?;
        let hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::Space);
        manager.register(hotkey)?;

        let events = Arc::new(OnePerHold::new());
        let published = events.clone();
        let id = hotkey.id();
        // One process-global slot, claimed here. This is why the Tauri plugin must not also be
        // registered: it claims the same slot in its own `setup` and whichever ran last wins,
        // silently.
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            if event.id != id {
                return;
            }
            match event.state {
                HotKeyState::Pressed => published.pressed(),
                HotKeyState::Released => published.released(),
            };
        }));

        tracing::info!(trigger = TRIGGER, "push-to-talk key registered with the session");
        Ok(GrabbedHotkey { manager: Manager(manager), hotkey, events })
    }
}

impl Hotkey for GrabbedHotkey {
    fn describe(&self) -> HotkeySupport {
        HotkeySupport::Working { trigger: TRIGGER.to_string() }
    }

    fn events(&self) -> broadcast::Receiver<HotkeyEvent> {
        self.events.subscribe()
    }

    /// Tidiness rather than necessity, and the difference from the portal is the point: an X11
    /// grab and a Windows `RegisterHotKey` both belong to a connection and a window that die with
    /// this process, so nothing is left behind whether this runs or not. The portal's
    /// registration outlives the process, which is why [`Hotkey::close`] exists at all.
    fn close(&self) -> Closing<'_> {
        if let Err(error) = self.manager.0.unregister(self.hotkey) {
            tracing::debug!(%error, "could not hand the push-to-talk key back; it dies with this process anyway");
        }
        Box::pin(std::future::ready(()))
    }
}

/// **The Windows auto-repeat question, and the test that answers it there.**
///
/// What is established by reading `global-hotkey` 0.8.0 and **not** by running it — there is no
/// Windows machine in this project:
///
/// - `register` passes `MOD_NOREPEAT` to `RegisterHotKey`, so Windows should send no repeated
///   `WM_HOTKEY` while the key stays down. That contradicts the note this task started from,
///   which said the Windows path has no de-duplication; it has none *in Rust*, and asks the OS
///   for it instead.
/// - There is no `state.pressed` flag on that path, unlike X11's. Release is reported by a thread
///   spawned per `WM_HOTKEY` polling `GetAsyncKeyState` every 50 ms — so two quick taps really do
///   put two threads in the air.
///
/// So the count this test prints is the measurement: how many `Pressed` one hold produces, and
/// how many `Released` follow it.
///
/// **It reads `global-hotkey`'s own channel rather than [`GrabbedHotkey`]**, on purpose.
/// [`super::OnePerHold`] collapses a repeat whatever the backend does, so a count taken through
/// this crate's own type would read `1` on a machine that repeated a hundred times and prove
/// nothing about the platform.
///
/// **It has never been compiled.** This machine is Linux, so `cargo test` here does not typecheck
/// the body; run `cargo test -p zyris-app -- --ignored a_held_key` on Windows.
#[cfg(target_os = "windows")]
#[cfg(test)]
mod windows_tests {
    use super::*;

    /// A hand-run test needs the message pump a Tauri app would otherwise provide: `WM_HOTKEY`
    /// reaches `global-hotkey`'s window only through `DispatchMessage`, and a `#[test]` has no
    /// event loop of its own.
    fn pump() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
        };
        let mut message: MSG = MSG::default();
        while unsafe { PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }

    #[test]
    #[ignore = "needs Windows and a person to hold Ctrl+Alt+Space"]
    fn a_held_key_reports_one_press_and_one_release() {
        let manager = GlobalHotKeyManager::new().expect("a message-only window on this thread");
        let hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::Space);
        manager.register(hotkey).expect("Ctrl+Alt+Space is not already taken");
        let events = GlobalHotKeyEvent::receiver();

        eprintln!("Hold Ctrl+Alt+Space down for three seconds, then let go. Listening for ten.");
        let (mut pressed, mut released) = (0usize, 0usize);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            pump();
            while let Ok(event) = events.try_recv() {
                match event.state {
                    HotKeyState::Pressed => pressed += 1,
                    HotKeyState::Released => released += 1,
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        eprintln!("one hold produced: pressed={pressed} released={released}");

        assert_eq!(
            pressed, 1,
            "`MOD_NOREPEAT` was supposed to stop Windows repeating `WM_HOTKEY` while the key is \
             held. It did not, and `global-hotkey` spawns a polling thread per press, so a long \
             hold is also a pile of threads. Record the number here and in CLAUDE.md."
        );
        assert_eq!(released, 1, "one hold, one release");
    }
}
