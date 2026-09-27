//! The phone app's entry point, for Tauri's Android and iOS builds. The desktop app is the
//! `zyris` binary (`main.rs`); on a desktop build this library is empty.

#[cfg(mobile)]
mod mobile;

#[cfg(mobile)]
pub use mobile::run;
