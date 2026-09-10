//! The Zyris runtime core. It runs whether or not anything is displaying it.

pub mod event;
pub mod identity;
pub mod lifecycle;
pub mod secret;

pub use event::{CoreEvent, EventBus};
