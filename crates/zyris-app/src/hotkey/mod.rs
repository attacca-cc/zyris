//! The push-to-talk key: one trait, the backend this session actually needs, and an honest
//! answer when there is no backend at all.
//!
//! This lives in `zyris-app` rather than in `zyris-voice` because a global shortcut is a
//! desktop-session concern and not an audio one, and because `--headless` has no hotkey: a run
//! with no window is a run with nobody at the keyboard.
//!
//! # Why the backend is chosen from the session and not from `cfg!(target_os)`
//!
//! `tauri-plugin-global-shortcut` is a re-export of `global-hotkey`, whose only Linux
//! implementation is X11. On this machine — Hyprland, `XDG_SESSION_TYPE=wayland`, with Xwayland
//! running so `DISPLAY=:0` is set — every call in that crate **succeeds** and no event ever
//! arrives (measured 2026-09-15): the connection to Xwayland is genuine, the grab lands on the
//! Xwayland root window, and Wayland-native key presses never pass through an X server. There is
//! no error to catch. So the decision is made from [`Desktop::of`], which reads
//! `XDG_SESSION_TYPE` first, and a Wayland session never reaches the X11 grab however set
//! `DISPLAY` is.
//!
//! # Why `global-hotkey` directly, and not the Tauri plugin
//!
//! They are the same code — `tauri-plugin-global-shortcut` is `pub use global_hotkey::{..}` plus
//! four IPC commands. Three things decided against the plugin:
//!
//! - **Its manager is built inside the plugin's `setup`, and a failure there fails
//!   `tauri::Builder::build`.** A machine where the grab cannot be taken would stop starting
//!   Zyris at all, which is the exact opposite of the rule this module exists for.
//! - **Its manager is only reachable through an `AppHandle`**, so [`Hotkey::describe`] — which
//!   has to be answerable before there is a window, and on a run that never shows one — could
//!   not be implemented over it.
//! - It registers `register`, `unregister`, `unregister_all` and `is_registered` as IPC
//!   commands, which would let the webview claim arbitrary global shortcuts. Nothing here wants
//!   that surface.
//!
//! Do not add the plugin alongside this. `GlobalHotKeyEvent::set_event_handler` is one
//! process-global slot and both would want it.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::broadcast;

mod none;
#[cfg(target_os = "linux")]
mod portal;
mod x11;

pub use none::NoHotkey;
#[cfg(target_os = "linux")]
pub use portal::PortalHotkey;
pub use x11::GrabbedHotkey;

/// The shortcut this application registers, everywhere.
///
/// On the portal it is the *name* a compositor points a key at, so it is part of the line a
/// person puts in their configuration and cannot be renamed without breaking every existing
/// install. On X11 and Windows it is only an internal label.
pub const SHORTCUT_ID: &str = "push_to_talk";

/// What the shortcut is for, as the portal shows it to the user.
pub const SHORTCUT_DESCRIPTION: &str = "Hold to talk to Zyris";

/// The key this asks for where it is allowed to ask: Ctrl+Alt+Space.
///
/// Only X11 and Windows honour it. The GlobalShortcuts portal ignores `preferred_trigger`
/// entirely — measured against `xdg-desktop-portal-hyprland` 1.3.12, where the returned
/// `trigger_description` is empty and the string does not appear in the portal binary — which is
/// the whole reason [`HotkeySupport::NeedsAKeyBound`] exists.
pub const TRIGGER: &str = "Ctrl+Alt+Space";

/// How many key events a subscriber may fall behind before it loses the oldest.
///
/// A hold is two events and a person cannot produce many per second, so this is generous by two
/// orders of magnitude. It exists because `broadcast` needs a number, not because the number is
/// a tuning decision.
const EVENT_CAPACITY: usize = 32;

/// What happened to the push-to-talk key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    /// The key went down. One per hold: see [`OnePerHold`].
    Pressed,
    /// The key came up. One per [`HotkeyEvent::Pressed`], never on its own.
    Released,
}

/// Whether a push-to-talk key can work on this desktop, and what a person has to do about it.
///
/// Three answers rather than a boolean, for the reason `zyris-tools`'s `announce.rs` gives about
/// `input` and `screen_capture`: a control that cannot work is worse than an absent one, because
/// nobody can tell it apart from a working one. The window shows whichever of these it is given
/// and must not flatten them.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum HotkeySupport {
    /// A key is registered and the next press arrives here. `trigger` is what to press.
    Working { trigger: String },
    /// Registered with the desktop, but nothing is pressing it yet.
    ///
    /// This is the Wayland portal's normal state: version 1 of the interface has no
    /// `ConfigureShortcuts`, and `preferred_trigger` is ignored, so an application cannot choose
    /// or even offer to choose the key. A person has to.
    ///
    /// **Zyris cannot tell whether they already have.** The compositor does not tell the portal
    /// what it bound, so `trigger_description` stays empty either way; anything rendering this
    /// has to be worded as "if you have not already" rather than as "this is not working".
    #[serde(rename_all = "camelCase")]
    NeedsAKeyBound {
        /// What the compositor has to point a key at.
        shortcut_id: String,
        /// The desktop this was worked out for, so the window can say which one the line below
        /// belongs to instead of presenting it as universal.
        desktop: String,
        /// The exact line, for the desktops whose spelling this project knows. `None` elsewhere
        /// — a guess in this field is a line somebody pastes into a configuration file.
        line: Option<String>,
        /// Said in words as well, always, because `line` can be `None`.
        how: String,
    },
    /// There is no way to register a global hotkey here.
    ///
    /// Not a failure to retry and not something a click can fix. Every desktop that falls back to
    /// `xdg-desktop-portal-gtk` — XFCE, MATE, Cinnamon, LXQt — is this, because that portal
    /// implements no GlobalShortcuts interface at all.
    Unavailable { reason: String },
}

/// A boxed future, because [`Hotkey`] is used behind `dyn` and an `async fn` in a trait is not.
type Closing<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// One push-to-talk key, however this desktop provides one.
///
/// `events` hands out a [`broadcast::Receiver`] rather than the `impl Stream` the plan sketched:
/// the whole point of this module is that the backend is chosen at run time, which needs
/// `dyn Hotkey`, and a trait with an `impl Trait` return is not object-safe. The receiver is the
/// same shape `zyris_runtime::EventBus` already hands out everywhere else in this program.
pub trait Hotkey: Send + Sync + 'static {
    /// Whether this can work, and what the person has to do. Cheap, and safe to call repeatedly
    /// — the window asks on every render.
    fn describe(&self) -> HotkeySupport;

    /// A new subscription to the key. Each caller gets its own; none of them consumes another's.
    fn events(&self) -> broadcast::Receiver<HotkeyEvent>;

    /// Give the registration back before the process ends.
    ///
    /// Only the portal backend has anything to give back — an X11 grab and a Windows
    /// `RegisterHotKey` both die with the process — and even there it is not enough on every
    /// compositor; see [`PortalHotkey::close`].
    fn close(&self) -> Closing<'_>;
}

/// What the session is, as far as a global hotkey is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Desktop {
    /// Windows: `RegisterHotKey`, through `global-hotkey`.
    Windows,
    /// An X11 session: `XGrabKey`, through the same crate.
    X11,
    /// A Wayland session. Only the GlobalShortcuts portal can do this, and not every desktop
    /// has one — which is why choosing this is not the same as having a hotkey.
    Wayland,
    /// No desktop session at all: a TTY, a systemd unit, a container. There is no global
    /// shortcut to register and nothing for a person to press.
    Headless,
}

impl Desktop {
    /// Decide from what the session says it is.
    ///
    /// `XDG_SESSION_TYPE` first and on its own, because it is the only one of the three that
    /// distinguishes the case this module exists for: a Wayland session running Xwayland has
    /// `DISPLAY` set and an X11 grab on it succeeds and never fires.
    ///
    /// The other two are the fallback for a session that never set `XDG_SESSION_TYPE` — a
    /// compositor started by hand from a TTY leaves it at `tty`, or unset — and `WAYLAND_DISPLAY`
    /// comes first there for the same reason.
    pub fn of(env: &Env) -> Desktop {
        // Before any of the three. On Windows none of them is set, and a WSL-style environment
        // that set them anyway would still be a Windows process with a Windows message queue.
        if env.windows {
            return Desktop::Windows;
        }
        match env.session_type.as_deref() {
            Some("wayland") => Desktop::Wayland,
            Some("x11") => Desktop::X11,
            _ if env.wayland_display.is_some() => Desktop::Wayland,
            _ if env.display.is_some() => Desktop::X11,
            _ => Desktop::Headless,
        }
    }
}

/// The readings [`Desktop::of`] decides from, and the name of the desktop for the advice.
///
/// A value rather than four `std::env::var` calls inside the decision, because the decision is
/// the part that has to be tested and no machine is five desktops at once.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Env {
    /// `cfg!(target_os = "windows")` in production. A field so a test can ask what a Windows
    /// session decides without being one.
    pub windows: bool,
    /// `XDG_SESSION_TYPE`.
    pub session_type: Option<String>,
    /// `WAYLAND_DISPLAY`.
    pub wayland_display: Option<String>,
    /// `DISPLAY`.
    pub display: Option<String>,
    /// `XDG_CURRENT_DESKTOP`. Never used to decide a backend — it says what a desktop calls
    /// itself, not what its portal implements — only to word the advice in
    /// [`HotkeySupport::NeedsAKeyBound`].
    pub current_desktop: Option<String>,
}

impl Env {
    pub fn read() -> Env {
        Env {
            windows: cfg!(target_os = "windows"),
            session_type: var("XDG_SESSION_TYPE"),
            wayland_display: var("WAYLAND_DISPLAY"),
            display: var("DISPLAY"),
            current_desktop: var("XDG_CURRENT_DESKTOP"),
        }
    }

    /// What to call this desktop in a sentence a person reads.
    pub fn desktop_name(&self) -> String {
        // `XDG_CURRENT_DESKTOP` is a colon-separated list, most specific first
        // (`Hyprland`, or `pop:GNOME`). The first entry is the one to name.
        self.current_desktop
            .as_deref()
            .and_then(|list| list.split(':').next())
            .filter(|name| !name.is_empty())
            .unwrap_or("this desktop")
            .to_string()
    }
}

/// An environment variable that is set to something. An empty one is not a session: a login
/// manager that exports `DISPLAY=` would otherwise be read as an X11 session.
fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// The line a compositor needs, where this project knows how that one spells it.
///
/// `None` rather than a plausible guess: this string is meant to be pasted into somebody's
/// configuration file, and a wrong one costs them an evening.
///
/// The empty prefix before the colon is the application id, and it is measured rather than
/// assumed: an unsandboxed binary registers with no id, and `hyprctl globalshortcuts` lists the
/// shortcut as `:push_to_talk` (2026-09-15). A Flatpak build would have one and this would have
/// to grow it.
pub fn compositor_line(desktop: &str, shortcut_id: &str) -> Option<String> {
    match desktop.to_ascii_lowercase().as_str() {
        "hyprland" => Some(format!("bind = CTRL ALT, space, global, :{shortcut_id}")),
        _ => None,
    }
}

/// Publishes at most one [`HotkeyEvent::Pressed`] per hold, and at most one
/// [`HotkeyEvent::Released`] per press.
///
/// Every backend goes through this, on every platform, and it is worth saying why rather than
/// treating it as belt and braces:
///
/// - **X11** de-duplicates already, with a `state.pressed` flag inside `global-hotkey`.
/// - **Windows** does not have such a flag, and does not need one either: `register` passes
///   `MOD_NOREPEAT` to `RegisterHotKey`, so the OS sends no repeat while the key is held
///   (read in `global-hotkey` 0.8.0; **not run — there is no Windows machine here**). What it
///   does have is a thread per `WM_HOTKEY` polling `GetAsyncKeyState` every 50 ms, so two quick
///   taps put two threads in the air and both can observe the key up and both send `Released`.
/// - **The portal** promises nothing either way, and the compositor is somebody else's code.
///
/// Three different mechanisms, two of them outside this program, one of them unverified. The
/// invariant a voice session needs — one `Pressed`, then one `Released` — is cheaper to hold
/// here than to trust three times.
///
/// `compare_exchange` rather than a read and a write: on Windows the two release threads above
/// really are concurrent.
pub struct OnePerHold {
    down: AtomicBool,
    events: broadcast::Sender<HotkeyEvent>,
}

impl OnePerHold {
    pub fn new() -> OnePerHold {
        OnePerHold { down: AtomicBool::new(false), events: broadcast::channel(EVENT_CAPACITY).0 }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<HotkeyEvent> {
        self.events.subscribe()
    }

    /// The backend saw the key go down. Returns whether that was news.
    pub fn pressed(&self) -> bool {
        if self.down.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
            return false;
        }
        // `send` fails only when nobody is subscribed, which is the ordinary state of a machine
        // with the voice session not running. Discarded on purpose.
        let _ = self.events.send(HotkeyEvent::Pressed);
        true
    }

    /// The backend saw the key come up. Returns whether that was news.
    pub fn released(&self) -> bool {
        if self.down.compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst).is_err() {
            return false;
        }
        let _ = self.events.send(HotkeyEvent::Released);
        true
    }
}

impl Default for OnePerHold {
    fn default() -> OnePerHold {
        OnePerHold::new()
    }
}

/// Build the backend this session needs.
///
/// Async because asking whether this desktop has a GlobalShortcuts portal is a D-Bus round trip,
/// and asking is the only honest way to know: `XDG_CURRENT_DESKTOP` says what a desktop calls
/// itself, not what its portal implements, and every desktop that falls back to
/// `xdg-desktop-portal-gtk` answers that question with "no".
///
/// Never fails. A machine with no way to register a hotkey gets [`NoHotkey`], which says so —
/// the same shape `announce.rs` uses for a host with no display server, and for the same reason.
pub async fn start(env: &Env) -> Arc<dyn Hotkey> {
    match Desktop::of(env) {
        Desktop::Headless => Arc::new(NoHotkey::because(
            "this process is not in a desktop session, so there is no key for anyone to press",
        )),
        Desktop::Windows | Desktop::X11 => match GrabbedHotkey::grab() {
            Ok(hotkey) => Arc::new(hotkey),
            Err(error) => Arc::new(NoHotkey::because(format!(
                "{TRIGGER} could not be registered with this session: {error}"
            ))),
        },
        #[cfg(target_os = "linux")]
        Desktop::Wayland => match PortalHotkey::open(env).await {
            Ok(hotkey) => Arc::new(hotkey),
            Err(error) => Arc::new(NoHotkey::because(format!(
                "this Wayland desktop has no working GlobalShortcuts portal, so no application \
                 can register a global key here: {error}"
            ))),
        },
        // A Wayland session on something that is not Linux. Nothing in this workspace ships such
        // a build; the arm exists so the match is total and the answer is the honest one.
        #[cfg(not(target_os = "linux"))]
        Desktop::Wayland => Arc::new(NoHotkey::because(
            "a Wayland session, on a build with no portal client compiled in",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wayland_with_xwayland() -> Env {
        Env {
            windows: false,
            session_type: Some("wayland".into()),
            wayland_display: Some("wayland-1".into()),
            display: Some(":0".into()),
            current_desktop: Some("Hyprland".into()),
        }
    }

    /// **The trap this whole module is built around.** This machine is exactly this environment,
    /// and the X11 backend on it registers successfully, reports success, and never fires once.
    #[test]
    fn a_wayland_session_with_display_set_is_not_an_x11_session() {
        assert_eq!(Desktop::of(&wayland_with_xwayland()), Desktop::Wayland);
    }

    #[test]
    fn an_x11_session_takes_the_grab() {
        let env = Env {
            session_type: Some("x11".into()),
            display: Some(":0".into()),
            ..Env::default()
        };
        assert_eq!(Desktop::of(&env), Desktop::X11);
    }

    /// A compositor started by hand from a TTY leaves `XDG_SESSION_TYPE` at `tty` and still has
    /// a `WAYLAND_DISPLAY`. Reading the fallback in the other order would send it to X11.
    #[test]
    fn a_session_type_that_says_tty_is_still_wayland_when_a_wayland_display_is_set() {
        let env = Env {
            session_type: Some("tty".into()),
            wayland_display: Some("wayland-0".into()),
            display: Some(":0".into()),
            ..Env::default()
        };
        assert_eq!(Desktop::of(&env), Desktop::Wayland);
    }

    #[test]
    fn a_session_with_neither_display_has_no_hotkey_at_all() {
        assert_eq!(Desktop::of(&Env::default()), Desktop::Headless);
        let tty = Env { session_type: Some("tty".into()), ..Env::default() };
        assert_eq!(Desktop::of(&tty), Desktop::Headless);
    }

    /// Windows is decided before any of the three are read, so a shell that exported them —
    /// which is an ordinary thing for a cross-compilation or an X server on Windows to do —
    /// cannot send a Windows process down a Linux path.
    #[test]
    fn windows_is_decided_before_the_three_unix_variables() {
        let env = Env {
            windows: true,
            session_type: Some("wayland".into()),
            wayland_display: Some("wayland-1".into()),
            display: Some(":0".into()),
            current_desktop: Some("Hyprland".into()),
        };
        assert_eq!(Desktop::of(&env), Desktop::Windows);
    }

    /// `Env::read` filters empty values, so a login manager that exports `DISPLAY=` does not
    /// make a TTY look like an X11 session. The filter lives in `var`, so this asserts on what
    /// `Desktop::of` does with the value it produces.
    #[test]
    fn an_empty_variable_is_not_a_session() {
        assert_eq!(var("ZYRIS_HOTKEY_TEST_EMPTY"), None);
        unsafe { std::env::set_var("ZYRIS_HOTKEY_TEST_EMPTY", "") };
        assert_eq!(var("ZYRIS_HOTKEY_TEST_EMPTY"), None);
        unsafe { std::env::set_var("ZYRIS_HOTKEY_TEST_EMPTY", "x") };
        assert_eq!(var("ZYRIS_HOTKEY_TEST_EMPTY"), Some("x".into()));
        unsafe { std::env::remove_var("ZYRIS_HOTKEY_TEST_EMPTY") };
    }

    #[test]
    fn the_desktop_named_in_the_advice_is_the_first_of_the_list() {
        let env = Env { current_desktop: Some("pop:GNOME".into()), ..Env::default() };
        assert_eq!(env.desktop_name(), "pop");
        assert_eq!(Env::default().desktop_name(), "this desktop");
        let empty = Env { current_desktop: Some(String::new()), ..Env::default() };
        assert_eq!(empty.desktop_name(), "this desktop");
    }

    /// The line is only ever handed out for a desktop whose spelling was actually checked. A
    /// plausible guess in this field is a line somebody pastes into a configuration file.
    #[test]
    fn a_compositor_line_is_offered_only_where_its_spelling_is_known() {
        let line = compositor_line("Hyprland", SHORTCUT_ID).expect("Hyprland is known");
        assert!(line.contains(&format!(":{SHORTCUT_ID}")), "{line}");
        assert_eq!(compositor_line("hyprland", SHORTCUT_ID), Some(line));
        assert_eq!(compositor_line("XFCE", SHORTCUT_ID), None);
        assert_eq!(compositor_line("sway", SHORTCUT_ID), None);
    }

    /// The line has to name the id the portal was actually given. These are two constants in two
    /// files and nothing else would notice them drifting apart.
    #[test]
    fn the_line_points_at_the_shortcut_this_program_registers() {
        let line = compositor_line("Hyprland", SHORTCUT_ID).unwrap();
        assert_eq!(line, "bind = CTRL ALT, space, global, :push_to_talk");
        assert_eq!(SHORTCUT_ID, "push_to_talk");
    }

    #[test]
    fn a_hold_publishes_one_press_however_many_the_backend_reports() {
        let hold = OnePerHold::new();
        let mut events = hold.subscribe();
        assert!(hold.pressed());
        assert!(!hold.pressed(), "a repeat while the key is down is not a second hold");
        assert!(!hold.pressed());
        assert!(hold.released());
        assert_eq!(events.try_recv(), Ok(HotkeyEvent::Pressed));
        assert_eq!(events.try_recv(), Ok(HotkeyEvent::Released));
        assert!(events.try_recv().is_err(), "nothing else was published");
    }

    /// The Windows shape: two `WM_HOTKEY` in quick succession put two polling threads in the
    /// air, and both can see the key up. The second release must not reach a session that has
    /// already ended its turn.
    #[test]
    fn a_release_that_nobody_pressed_publishes_nothing() {
        let hold = OnePerHold::new();
        let mut events = hold.subscribe();
        assert!(!hold.released());
        assert!(events.try_recv().is_err());
        assert!(hold.pressed());
        assert!(hold.released());
        assert!(!hold.released(), "the second polling thread's answer is not a second release");
        assert_eq!(events.try_recv(), Ok(HotkeyEvent::Pressed));
        assert_eq!(events.try_recv(), Ok(HotkeyEvent::Released));
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn a_second_hold_after_a_release_is_a_second_hold() {
        let hold = OnePerHold::new();
        let mut events = hold.subscribe();
        hold.pressed();
        hold.released();
        assert!(hold.pressed(), "the key can be held again");
        assert!(hold.released());
        let seen: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
        assert_eq!(
            seen,
            vec![
                HotkeyEvent::Pressed,
                HotkeyEvent::Released,
                HotkeyEvent::Pressed,
                HotkeyEvent::Released
            ]
        );
    }

    /// Two subscribers see the same hold. The voice session and the window are both going to
    /// want one.
    #[test]
    fn every_subscriber_sees_the_hold() {
        let hold = OnePerHold::new();
        let mut one = hold.subscribe();
        let mut two = hold.subscribe();
        hold.pressed();
        assert_eq!(one.try_recv(), Ok(HotkeyEvent::Pressed));
        assert_eq!(two.try_recv(), Ok(HotkeyEvent::Pressed));
    }

    /// A desktop with nothing to register says so, rather than handing out a subscription that
    /// will never yield while looking exactly like one that will.
    #[tokio::test]
    async fn a_session_that_is_not_a_desktop_says_so_instead_of_looking_registered() {
        let hotkey = start(&Env::default()).await;
        match hotkey.describe() {
            HotkeySupport::Unavailable { reason } => {
                assert!(reason.contains("desktop session"), "{reason}");
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
        let mut events = hotkey.events();
        assert!(events.try_recv().is_err());
        hotkey.close().await;
    }
}
