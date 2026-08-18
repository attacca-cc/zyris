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
//! | [`send`] | The other direction — the `file_transfer` surface an agent calls |
//! | [`listen`] | Accepting peer connections, and deciding whose they are |
//!
//! `name` is kept apart from `inbox` on purpose — sanitizing is a pure decision, so dozens of
//! cases run as one table test in an instant. Merged in, every one of those tests would need a
//! `tempfile`.
//!
//! [`send`] and [`listen`] are the two halves that touch the network; everything else here stops
//! at the filesystem, which is why the plain `transfer` feature costs no transport dependencies.
//! Those two are behind `transfer-send` and `transfer-listen`, which add the rendezvous
//! (`zyris-attacca`) and the transport (`zyris-p2p`, and through it iroh). They are separate
//! features because the two directions are separately useful: a node can accept deliveries without
//! carrying the tool surface that makes them.

pub mod audit;
pub mod inbox;
#[cfg(feature = "transfer-listen")]
pub mod listen;
pub mod name;
pub mod peer;
#[cfg(feature = "transfer-send")]
pub mod send;
pub mod undo;

pub use audit::{Audit, AuditLine};
pub use inbox::{Inbox, InboxError};
#[cfg(feature = "transfer-listen")]
pub use listen::{
    serve_peers, PeerCache, PeerDirectory, DEFAULT_DIRECTORY_TTL, DEFAULT_REFRESH_INTERVAL,
};
pub use name::safe_name;
pub use peer::{InFlight, LocalPeerTransfer, TRANSFER_IN_FLIGHT, TransferConfig, part_path};
#[cfg(feature = "transfer-send")]
pub use send::{
    FileTransferConfig, IrohPeerLink, LocalFileTransfer, PeerLink, PeerSession,
    DEFAULT_WIRE_DEADLINE,
};
pub use undo::UndoStore;
