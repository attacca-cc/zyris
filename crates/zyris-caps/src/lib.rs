pub mod browser;
pub mod file_io;
pub mod input;
pub mod screen;
pub mod terminal;

#[cfg(any(feature = "file-io-impl", feature = "terminal-impl"))]
mod path;

#[cfg(feature = "file-io-impl")]
mod file_io_impl;
#[cfg(feature = "file-io-impl")]
pub use file_io_impl::LocalFileIo;

#[cfg(feature = "terminal-impl")]
mod terminal_impl;
#[cfg(feature = "terminal-impl")]
pub use terminal_impl::PtyTerminal;

pub use browser::{browser_chrome_capability, BrowserChrome, BrowserChromeClient, BrowserChromeServer};
pub use file_io::{
    file_io_capability, DirEntry, FileIo, FileIoClient, FileIoServer, FileStat,
};
pub use input::{input_capability, Input, InputClient, InputServer, MouseButton};
pub use screen::{
    screen_capture_capability, Display, Region, ScreenCapture, ScreenCaptureClient,
    ScreenCaptureServer,
};
pub use terminal::{
    terminal_capability, ExecOutput, PtyChunk, PtyId, PtyOpened, Terminal, TerminalClient,
    TerminalServer,
};
