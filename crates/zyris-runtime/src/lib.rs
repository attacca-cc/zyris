//! The Zyris runtime core. It runs whether or not anything is displaying it.

pub mod announcement;
pub mod connection;
pub mod event;
pub mod identity;
pub mod lifecycle;
pub mod lock;
pub mod secret;

pub use announcement::LiveCapabilities;
pub use event::{CoreEvent, EventBus, McpServerChange};
