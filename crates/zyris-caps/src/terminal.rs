use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zyris::{Blob, Streaming};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PtyId(pub String);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PtyOpened {
    pub pty: PtyId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PtyChunk {
    pub data: Blob,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExecOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    #[serde(default)]
    pub timed_out: bool,
}

#[zyris::capability(name = "terminal", version = 1)]
pub trait Terminal {
    /// Open an interactive PTY; output chunks arrive on the returned stream.
    #[zyris(uni_stream)]
    async fn open(
        &self,
        shell: Option<String>,
        cols: u16,
        rows: u16,
    ) -> zyris::Result<Streaming<PtyOpened, PtyChunk>>;

    /// Write input bytes to an open PTY.
    async fn write(&self, pty: PtyId, data: Blob) -> zyris::Result<()>;

    /// Resize an open PTY.
    async fn resize(&self, pty: PtyId, cols: u16, rows: u16) -> zyris::Result<()>;

    /// Close an open PTY.
    async fn close(&self, pty: PtyId) -> zyris::Result<()>;

    /// Run a command to completion and capture its output.
    async fn exec(
        &self,
        command: String,
        cwd: Option<String>,
        timeout_ms: Option<u64>,
    ) -> zyris::Result<ExecOutput>;
}
