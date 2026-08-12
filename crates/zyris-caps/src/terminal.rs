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
    /// True when stdout hit the per-stream cap and surplus bytes were discarded.
    #[serde(default)]
    pub stdout_truncated: bool,
    /// True when stderr hit the per-stream cap and surplus bytes were discarded.
    #[serde(default)]
    pub stderr_truncated: bool,
}

/// What bounds a `read` or `screen` call: how long the PTY must be quiet before the reply goes
/// out, and how long to wait at most.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Settle {
    /// Return once output has been quiet this long. 0 snapshots immediately without waiting.
    pub quiet_ms: u64,
    /// Return here even if output never goes quiet. Not an error — `more` says whether anything
    /// is left behind.
    pub timeout_ms: u64,
}

impl Default for Settle {
    fn default() -> Self {
        Settle { quiet_ms: 300, timeout_ms: 5000 }
    }
}

/// What a [`Terminal::read`] returns.
///
/// `more`, `dropped` and `exited` each state a different fact, and collapsing them into one
/// "truncated" flag would leave a caller unable to answer the only question that matters: is
/// calling again going to get me the rest?
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PtyRead {
    /// Output since the last `read`, decoded as UTF-8 and cut on a character boundary. A
    /// multi-byte character split across a chunk boundary carries over to the next call.
    ///
    /// Terminal control sequences are stripped — only `\n` and `\t` survive. Escape codes are
    /// not something a reader can act on, and a tool result carrying a raw `U+001B` is rejected
    /// outright by at least one agent runtime. When the *effect* of those codes is what matters,
    /// [`Terminal::screen`] renders it. `open_stream` still carries the bytes untouched.
    pub content: String,
    /// Bytes are still buffered. **Call again and you get them.**
    pub more: bool,
    /// Bytes lost for good because the ring buffer overflowed. **Calling again will not bring
    /// them back.**
    pub dropped: u64,
    /// The shell's exit code once it has finished; `null` while it is still running. Output
    /// produced before it exited is still delivered.
    pub exited: Option<i32>,
}

/// What a [`Terminal::screen`] returns: the display as a text grid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PtyScreen {
    /// One entry per screen row, trailing blanks trimmed. Named `lines` rather than `rows`
    /// because `open` and `resize` already use `rows` for a row *count*.
    pub lines: Vec<String>,
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub exited: Option<i32>,
}

#[zyris::capability(name = "terminal", version = 3)]
pub trait Terminal {
    /// Open an interactive PTY and return its handle. Output is collected in the node and
    /// fetched with `read` (raw bytes since the last call) or `screen` (the rendered display).
    ///
    /// The shell starts in the node's root directory, which is also what relative paths in
    /// `exec` resolve against. Close it with `close` when done; a session nobody touches is
    /// reaped after an idle timeout.
    async fn open(&self, shell: Option<String>, cols: u16, rows: u16) -> zyris::Result<PtyOpened>;

    /// Open an interactive PTY whose output arrives as a stream.
    ///
    /// This costs a caller that can declare a stream (§4 of the protocol: the caller allocates
    /// the id and puts it in `req.stream`). A caller that cannot — notably an agent tool-caller
    /// invoking tools unary — wants [`Terminal::open`] plus [`Terminal::read`] instead.
    #[zyris(uni_stream)]
    async fn open_stream(
        &self,
        shell: Option<String>,
        cols: u16,
        rows: u16,
    ) -> zyris::Result<Streaming<PtyOpened, PtyChunk>>;

    /// Read a PTY's output since the last `read`. With `input`, those bytes are written first and
    /// the settle window opens after them, so the reply covers what the input produced.
    ///
    /// Returns once output has been quiet for `quiet_ms`, or at `timeout_ms`. Hitting the timeout
    /// is not an error: whatever arrived comes back with `more` set.
    async fn read(
        &self,
        pty: PtyId,
        input: Option<String>,
        settle: Option<Settle>,
    ) -> zyris::Result<PtyRead>;

    /// Render a PTY's current display as a text grid, under the same settle rules as `read`.
    ///
    /// This is the path for full-screen programs (vim, htop, less), where what matters is the
    /// state of the display rather than the byte stream that produced it. It consumes nothing,
    /// so a `read` after a `screen` still returns every byte.
    async fn screen(
        &self,
        pty: PtyId,
        input: Option<String>,
        settle: Option<Settle>,
    ) -> zyris::Result<PtyScreen>;

    /// Write input bytes to an open PTY.
    async fn write(&self, pty: PtyId, data: Blob) -> zyris::Result<()>;

    /// Resize an open PTY.
    async fn resize(&self, pty: PtyId, cols: u16, rows: u16) -> zyris::Result<()>;

    /// Close an open PTY.
    async fn close(&self, pty: PtyId) -> zyris::Result<()>;

    /// Run a command to completion and capture its stdout, stderr and exit code.
    ///
    /// Say what to run with **exactly one** of `command` or `argv`:
    ///
    /// - `command`: a shell one-liner, run through `/bin/sh -c` on Unix (honouring `shell`, which
    ///   defaults to `/bin/sh`) or `cmd /C` on Windows (`shell` is ignored there — use `argv` to
    ///   pick a Windows program). Use this only when you actually need shell features (pipes,
    ///   redirection, globbing). On Unix the whole string is handed to the shell as **one argument**,
    ///   so embedded single- or double-quotes reach the shell exactly as written — no escaping, no
    ///   base64 encoding. `echo "hello"` and `echo 'hello'` run directly.
    /// - `argv`: a program and its argument list, run **without any shell**. Nothing is re-quoted
    ///   or re-interpreted: spaces, quotes, `$`, backticks and `%` inside an element reach the
    ///   program exactly as given. This is the escaping-free path — prefer it for anything longer
    ///   than a trivial one-liner.
    ///
    /// For a PowerShell script on a Windows node the reliable, quoting-free pattern is:
    ///
    /// ```text
    /// argv: ["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", "-"], stdin: <script>
    /// ```
    ///
    /// `-Command -` makes PowerShell read the script from stdin, so the script travels as plain
    /// text — no escaping, no base64 encoding. Short one-liners can go straight into `argv`
    /// (`["powershell.exe", "-NoProfile", "-Command", "Get-Date"]`).
    ///
    /// `stdin` is written to the child and closed before the output is gathered; omit it to run
    /// with an empty stdin. `env` entries override the node's own environment. `cwd` may be
    /// relative or absolute: a relative path (`crates/zyris-caps`) resolves against the node's root
    /// directory, a leading `/` is used as given, and `.` / `..` are normalized; omit it to run in
    /// the root itself.
    ///
    /// `timeout_ms` bounds the whole run; on expiry the **whole process tree** is killed, partial
    /// output is returned with `timed_out` set, and the exit code is -1. Each output stream is
    /// capped; when a cap is hit the surplus is discarded and `stdout_truncated` / `stderr_truncated`
    /// report it (the child keeps running to completion either way).
    #[allow(clippy::too_many_arguments)]
    async fn exec(
        &self,
        command: Option<String>,
        argv: Option<Vec<String>>,
        cwd: Option<String>,
        timeout_ms: Option<u64>,
        stdin: Option<String>,
        env: Option<std::collections::HashMap<String, String>>,
        shell: Option<String>,
    ) -> zyris::Result<ExecOutput>;
}
