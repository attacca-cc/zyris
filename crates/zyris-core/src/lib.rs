//! The Zyris runtime core. It runs whether or not anything is displaying it.

pub mod event;
pub mod lifecycle;

pub use event::{CoreEvent, EventBus};
