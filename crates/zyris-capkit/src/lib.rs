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
//! | `screen` | [`XcapScreenCapture`] | [`xcap`] |
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
//! A Wayland compositor does not let a client read the screen, so `xcap` goes through
//! `org.freedesktop.portal.Screenshot` and falls back to the wlroots `zwlr_screencopy` protocol
//! when no portal offers that interface. Give the session a Screenshot portal backend if you can:
//! the fallback is where the rough edges are. On Hyprland with `xdg-desktop-portal-hyprland` and
//! no Screenshot portal exposed, capturing a whole output fails with
//! `wl_shm_pool: Couldn't mmap from fd` — a region one pixel narrower than the output succeeds,
//! which places the bug in the screencopy path rather than here.

pub mod path;
pub use path::resolve_under;

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
pub use screen::XcapScreenCapture;
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
