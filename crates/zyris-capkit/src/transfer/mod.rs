//! The receiving side's housekeeping for node-to-node file transfer.
//!
//! There is no human confirmation on the receiving side — an agent calls a tool and a file lands
//! on someone else's machine. **The path jail, the size limit and the audit log are the only
//! defenses there are.** This module splits those between its parts.
//!
//! | Module | What it owns |
//! |---|---|
//! | [`name`] | Proposed name → a single path component. Never touches the filesystem |
//! | [`inbox`] | Choosing the destination and checking the real path. Rejects symlinks |
//! | [`undo`] | Moves the original aside before overwriting |
//! | [`audit`] | Writes one line per transfer |
//! | [`peer`] | Wires the above together into `peer_transfer` |
//!
//! `name` is kept apart from `inbox` on purpose — sanitizing is a pure decision, so dozens of
//! cases run as one table test in an instant. Merged in, every one of those tests would need a
//! `tempfile`.

pub mod audit;
pub mod inbox;
pub mod name;
pub mod peer;
pub mod undo;

pub use audit::{Audit, AuditLine};
pub use inbox::{Inbox, InboxError};
pub use name::safe_name;
pub use peer::{LocalPeerTransfer, TransferConfig, part_path};
pub use undo::UndoStore;
