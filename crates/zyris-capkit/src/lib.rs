//! Reference implementations of the capabilities declared in [`zyris_caps`].
//!
//! `zyris-caps` is the contract — traits, request types, generated clients and server wrappers,
//! and nothing that touches an operating system. This crate is the other half: the implementations
//! a node reaches for when it wants the standard behaviour rather than its own.
//!
//! Every implementation is behind a feature, and only the two that build anywhere are on by
//! default:
//!
//! | Feature | Type | Backend |
//! |---|---|---|
//! | `file-io` (default) | [`LocalFileIo`] | `tokio::fs` |
//! | `terminal` (default) | [`PtyTerminal`] | `portable-pty` |
//! | `screen` | [`HostScreenCapture`] | [`xcap`], or `libwayshot` in a wlroots Wayland session |
//! | `input` | [`EnigoInput`] | [`enigo`] |
//!
//! `desktop` turns on both of the last two.
//!
//! # Building the desktop features on Linux
//!
//! `xcap` links against the display stack, so the `screen` feature needs development packages that
//! a headless build does not:
//!
//! ```text
//! # Debian/Ubuntu
//! apt-get install pkg-config libclang-dev libxcb1-dev libxrandr-dev \
//!     libdbus-1-dev libpipewire-0.3-dev libwayland-dev libegl-dev
//! ```
//!
//! `enigo` defaults to a pure-Rust X11 client and needs nothing. Add `input-wayland` to also
//! compile its `wlr-virtual-keyboard` / `wlr-virtual-pointer` path, which only a wlroots-based
//! compositor exposes.
//!
//! # Screen capture on Wayland
//!
//! A Wayland compositor does not let a client read the screen, so there are two ways in and
//! [`ScreenBackend::detect`] picks between them at construction.
//!
//! On a wlroots-based compositor — Hyprland, Sway, river, Wayfire — [`ScreenBackend::Wayland`]
//! talks `wl_output` and `zwlr_screencopy` directly, the same pair `grim` uses. This is not the
//! same as letting `xcap` do it: `xcap` enumerates monitors over XRandR even in a Wayland session
//! and then captures in Wayland's coordinate space, and the two only agree while Xwayland can
//! express the compositor's layout. Put a monitor above the origin and its `y` goes negative,
//! which X11 cannot represent, so Xwayland flattens the arrangement into a row and every captured
//! rectangle lands on the wrong output.
//!
//! Everywhere else — GNOME, KDE, and any compositor without `zwlr_screencopy` — the probe fails
//! and [`ScreenBackend::Xcap`] takes over, reaching the screen through
//! `org.freedesktop.portal.Screenshot`. That interface has to actually be offered: a session whose
//! portal backend does not register it has no way to capture at all.

pub mod path;
pub use path::resolve_under;

#[cfg(any(feature = "input", feature = "screen"))]
mod display;
#[cfg(any(feature = "input", feature = "screen"))]
pub use display::Displays;

#[cfg(feature = "file-io")]
mod file_io;
#[cfg(feature = "file-io")]
pub use file_io::LocalFileIo;

#[cfg(feature = "terminal")]
mod terminal;
#[cfg(feature = "terminal")]
pub use terminal::PtyTerminal;

#[cfg(feature = "screen")]
mod screen;
#[cfg(feature = "screen")]
pub use screen::{HostDisplays, HostScreenCapture, ScreenBackend};
#[cfg(feature = "screen")]
pub use xcap::{self, image};

#[cfg(feature = "input")]
mod chord;
#[cfg(feature = "input")]
mod input;
#[cfg(feature = "input")]
pub use enigo;
#[cfg(feature = "input")]
pub use input::EnigoInput;
