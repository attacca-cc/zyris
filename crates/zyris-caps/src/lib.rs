//! The standard Zyris capability catalogue: declarations only.
//!
//! Each module holds one `#[zyris::capability]` trait and the types it speaks in. The macro turns
//! each into a descriptor, a `…Server<T>` wrapper a node registers, and a `…Client` a peer calls
//! through — so a client crate and a server crate agree on the wire by depending on this and
//! nothing else.
//!
//! There is deliberately no implementation here, and no dependency that touches an operating
//! system. The reference implementations live in `zyris-capkit`.

pub mod browser;
pub mod file_io;
pub mod file_transfer;
pub mod input;
pub mod peer_transfer;
pub mod screen;
pub mod terminal;

pub use browser::{browser_chrome_capability, BrowserChrome, BrowserChromeClient, BrowserChromeServer};
pub use file_io::{
    file_io_capability, DirEntry, FileEdit, FileIo, FileIoClient, FileIoServer, FileRead, FileStat,
};
pub use file_transfer::{
    file_transfer_capability, FileTransfer, FileTransferClient, FileTransferServer, InboxEntry,
    SendReceipt,
};
pub use input::{input_capability, Input, InputClient, InputServer, MouseButton};
pub use peer_transfer::{
    peer_transfer_capability, PeerTransfer, PeerTransferClient, PeerTransferServer, PullHead,
    TransferDone, TransferOffer,
};
pub use screen::{
    screen_capture_capability, Display, ImageFormat, Region, ScreenCapture, ScreenCaptureClient,
    ScreenCaptureServer,
};
pub use terminal::{
    terminal_capability, ExecOutput, PtyChunk, PtyId, PtyOpened, PtyRead, PtyScreen, Settle,
    Terminal, TerminalClient, TerminalServer,
};
