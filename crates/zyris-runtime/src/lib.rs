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

/// The address a node dials when nobody said otherwise.
///
/// Named here as well as in the protocol crate so that a caller which only wants to *say* where
/// this machine is pointed — `zyris status`, and the state file a running node writes — does not
/// have to depend on the protocol stack to find out. The value is the protocol crate's; this is a
/// re-export rather than a second copy of the URL.
pub use zyris::DEFAULT_SERVER_URL;
