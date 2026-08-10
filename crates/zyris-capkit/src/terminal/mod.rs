mod buffer;
mod sanitize;
mod session;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use bytes::Bytes;
use zyris::{Blob, ErrorCode, Streaming, WireError};

use zyris_caps::{ExecOutput, PtyChunk, PtyId, PtyOpened, PtyRead, PtyScreen, Settle, Terminal};

use crate::path::resolve_under;
use buffer::trim_incomplete_tail;
use sanitize::{strip_controls, trim_incomplete_escape};
use session::{gone, Sessions, POLL_INTERVAL};

/// The maximum bytes an `open_stream` subscriber carries in one chunk.
const STREAM_CHUNK_MAX: usize = 64 * 1024;

/// The maximum bytes a single `read` call returns. Matches `file_io`'s `READ_UNARY_MAX` — the
/// value measured to clear Attacca's `ZYRIS_MAX_RESULT_BYTES` budget (default 1,000,000).
const PTY_READ_MAX: usize = 128 * 1024;

/// How many sessions may be open at once. Without a cap, an agent stuck in a loop would spawn
/// shells without limit.
const MAX_SESSIONS: usize = 8;

/// How long to wait before closing a session nobody has touched.
///
/// This is the point where spec §5.1 decided to cover connection-tracking plumbing with this
/// instead — if only the wire got cut, nobody calls `read` anymore, so it eventually gets
/// collected here. The tradeoff is that the shell process stays alive for up to this long after
/// the disconnect.
const IDLE_TIMEOUT: Duration = Duration::from_secs(600);

pub struct PtyTerminal {
    default_shell: String,
    root: PathBuf,
    next_id: AtomicU64,
    sessions: Sessions,
    idle_timeout: Duration,
    sweeper_started: AtomicBool,
}

impl PtyTerminal {
    pub fn rooted(root: impl Into<PathBuf>) -> Self {
        PtyTerminal { root: root.into(), ..PtyTerminal::default() }
    }

    /// Injection point for the idle timeout. A test cannot wait around for 10 minutes.
    pub fn with_idle_timeout(mut self, d: Duration) -> Self {
        self.idle_timeout = d;
        self
    }

    /// Starts the idle sweeper on the first `open`.
    ///
    /// It cannot be started in the constructor — `PtyTerminal::default()` can be called outside a
    /// tokio runtime, and `tokio::spawn` would panic there. `open` is async, so being inside a
    /// runtime is guaranteed.
    ///
    /// Holds the session map as a `Weak`. Holding it strongly would mean that even after
    /// `PtyTerminal` is dropped by a `local.declare` re-declaration, the sweeper would keep the map
    /// alive and the shells would survive with it.
    fn ensure_sweeper(&self) {
        if self.sweeper_started.swap(true, Ordering::Relaxed) {
            return;
        }
        let weak: Weak<Mutex<HashMap<String, session::PtySession>>> = Arc::downgrade(&self.sessions);
        let idle = self.idle_timeout;
        let tick = (idle / 4).max(Duration::from_millis(10));
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tick).await;
                let Some(sessions) = weak.upgrade() else { return };
                // When a session drops out of the map, `PtySession` is dropped, which closes the
                // master fd and delivers SIGHUP to the shell on the slave side.
                sessions.lock().unwrap().retain(|_, s| s.last_touch.elapsed() < idle);
            }
        });
    }

    fn new_session(&self, shell: Option<String>, cols: u16, rows: u16) -> zyris::Result<String> {
        self.ensure_sweeper();
        if self.sessions.lock().unwrap().len() >= MAX_SESSIONS {
            return Err(WireError::new(
                ErrorCode::Other("too_many_ptys".into()),
                format!("at most {MAX_SESSIONS} PTYs at a time; close one first"),
            ));
        }
        let id = format!("pty-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        session::spawn(
            &self.sessions,
            id.clone(),
            shell.unwrap_or_else(|| self.default_shell.clone()),
            &self.root,
            cols,
            rows,
        )?;
        Ok(id)
    }

    /// Writes `input`, then waits until the output goes quiet.
    ///
    /// Returns on whichever of three conditions comes first: quiet is satisfied and the gate is
    /// open / the deadline is reached / the shell has exited and is quiet.
    ///
    /// **The gate is only enforced when `input` is present.** If the input triggers no output at
    /// all, quiet would be satisfied immediately, and returning an empty response right away would
    /// make the agent misread that as "the command is finished." Waiting out the deadline to
    /// confirm there really is nothing is more honest. Conversely, applying the same hold to a
    /// pure observation with no `input` would mean every poll eats the full `timeout_ms`.
    ///
    /// **Limits of the gate**: in cooked mode the terminal echoes typed bytes immediately, so
    /// "there is something to read" becomes true right away and the gate opens. If the command is
    /// slow to start producing output, a response containing only the echo can go out first — but
    /// since `more` is false and `exited` is `None` at that point, the agent can read that as "not
    /// done yet" and calling again picks up where it left off. Where the gate actually earns its
    /// keep is raw mode with no echo (feeding keys to vim or htop).
    async fn settle(
        &self,
        pty: &PtyId,
        input: Option<String>,
        settle: Option<Settle>,
    ) -> zyris::Result<()> {
        let Settle { quiet_ms, timeout_ms } = settle.unwrap_or_default();
        let gated = input.is_some();

        let baseline = {
            let mut sessions = self.sessions.lock().unwrap();
            let s = sessions.get_mut(&pty.0).ok_or_else(gone)?;
            s.touch();
            if let Some(text) = input {
                s.write_input(text.as_bytes())?;
            }
            s.buf.total_written()
        };

        if quiet_ms == 0 {
            return Ok(());
        }

        let quiet = Duration::from_millis(quiet_ms);
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            {
                let sessions = self.sessions.lock().unwrap();
                let s = sessions.get(&pty.0).ok_or_else(gone)?;
                let is_quiet = s.last_byte_at.elapsed() >= quiet;
                // If the shell has exited and is quiet, nothing more is coming — return regardless of the gate.
                if is_quiet && s.exited.is_some() {
                    return Ok(());
                }
                // The gate looks only at **bytes that arrived after this call started**. Letting
                // already-pending unread bytes open it would mean a leftover fragment of the
                // previous call's prompt keeps the gate permanently open, blocking nothing at all.
                let gate_open = !gated || s.buf.total_written() > baseline;
                if is_quiet && gate_open {
                    return Ok(());
                }
            }
            if Instant::now() >= deadline {
                return Ok(());
            }
        }
    }
}

/// An `open_stream` subscriber that walks the ring buffer with its own cursor.
///
/// It never touches the session's `read_cursor`, so calling `read` while a stream is still
/// attached does not steal bytes from either side.
fn subscribe(
    sessions: Sessions,
    id: String,
) -> impl futures_util::Stream<Item = zyris::Result<PtyChunk>> {
    futures_util::stream::unfold((sessions, id, 0u64), |(sessions, id, mut cursor)| async move {
        loop {
            {
                let guard = sessions.lock().unwrap();
                let s = guard.get(&id)?;
                let (bytes, _dropped) = s.buf.read_at(&mut cursor, STREAM_CHUNK_MAX);
                if !bytes.is_empty() {
                    let chunk = PtyChunk { data: Blob::from_bytes(Bytes::from(bytes)) };
                    drop(guard);
                    return Some((Ok(chunk), (sessions, id, cursor)));
                }
                if s.exited.is_some() {
                    return None;
                }
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
}

impl Default for PtyTerminal {
    fn default() -> Self {
        let default_shell = if cfg!(windows) {
            "powershell.exe".to_string()
        } else {
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
        };
        PtyTerminal {
            default_shell,
            root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            next_id: AtomicU64::new(1),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            idle_timeout: IDLE_TIMEOUT,
            sweeper_started: AtomicBool::new(false),
        }
    }
}

/// The maximum bytes `exec` allows for one output stream. Bytes past this are dropped, and
/// `ExecOutput::stdout_truncated` / `stderr_truncated` report that it happened.
///
/// The point is to stay under Attacca's result budget (`ZYRIS_MAX_RESULT_BYTES`) — 1 MiB is the
/// safe side, for the same reason as `PTY_READ_MAX`.
const EXEC_OUTPUT_CAP: usize = 1024 * 1024;

#[derive(Default)]
struct OutputAcc {
    buf: Vec<u8>,
    truncated: bool,
}

/// Collects a pipe up to `cap`, and keeps reading even past that, discarding whatever overflows.
///
/// Stopping the read could make the child block once its pipe fills up. Bytes past the cap are
/// left out of the result, but the child is still allowed to run to completion.
async fn drain_capped<R>(mut reader: R, acc: Arc<Mutex<OutputAcc>>, cap: usize)
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;

    let mut chunk = [0u8; 8192];
    loop {
        let n = match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let mut acc = acc.lock().unwrap();
        let room = cap.saturating_sub(acc.buf.len());
        if room > 0 {
            acc.buf.extend_from_slice(&chunk[..room.min(n)]);
        }
        if n > room {
            acc.truncated = true;
        }
    }
}

/// Kills the process tree.
///
/// Unix: since `exec` made `process_group(0)` set the child up as the leader of a new group,
/// passing that leader's pid as a negative number kills every grandchild the shell left behind in
/// one shot. Retries briefly, until the group is gone, to cover the setpgid race window (before
/// the child has created its group yet).
#[cfg(unix)]
fn kill_tree(pid: Option<u32>) {
    use std::thread;
    use std::time::Duration;

    let Some(pid) = pid else { return };
    let target = -(pid as i32);
    for _ in 0..50 {
        // SAFETY: a negative pid addresses a process group. This is the group we just created,
        // and our own process is not in it.
        if unsafe { libc::kill(target, libc::SIGKILL) } == 0 {
            return; // SIGKILL was delivered to the whole group
        }
        // If the leader (the child itself) is already dead, the group will never come into being.
        if unsafe { libc::kill(pid as i32, 0) } != 0 {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// Windows: `taskkill /T` kills the child tree. Killing just cmd.exe would leave grandchildren
/// orphaned, so a tree kill is needed.
#[cfg(not(unix))]
fn kill_tree(pid: Option<u32>) {
    if let Some(pid) = pid {
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .output();
    }
}

pub(crate) fn pty_err(msg: impl std::fmt::Display) -> WireError {
    WireError::new(ErrorCode::Internal, msg.to_string())
}

#[zyris::async_trait]
impl Terminal for PtyTerminal {
    async fn open(&self, shell: Option<String>, cols: u16, rows: u16) -> zyris::Result<PtyOpened> {
        Ok(PtyOpened { pty: PtyId(self.new_session(shell, cols, rows)?) })
    }

    async fn open_stream(
        &self,
        shell: Option<String>,
        cols: u16,
        rows: u16,
    ) -> zyris::Result<Streaming<PtyOpened, PtyChunk>> {
        let id = self.new_session(shell, cols, rows)?;
        let items = subscribe(self.sessions.clone(), id.clone());
        Ok(Streaming::new(PtyOpened { pty: PtyId(id) }, items))
    }

    async fn read(
        &self,
        pty: PtyId,
        input: Option<String>,
        settle: Option<Settle>,
    ) -> zyris::Result<PtyRead> {
        self.settle(&pty, input, settle).await?;

        let mut sessions = self.sessions.lock().unwrap();
        let s = sessions.get_mut(&pty.0).ok_or_else(gone)?;
        let mut cursor = s.read_cursor;
        let (bytes, dropped) = s.buf.read_at(&mut cursor, PTY_READ_MAX);

        // Carry a truncated tail over to the next call — whether it's a multibyte character or an
        // escape sequence. Except if the shell has exited and this is the last of the bytes, there
        // is no next call to carry it to, so it's just burned.
        let at_end = s.exited.is_some() && cursor == s.buf.total_written();
        let keep = if at_end {
            bytes.len()
        } else {
            trim_incomplete_tail(&bytes).min(trim_incomplete_escape(&bytes))
        };
        s.read_cursor = cursor - (bytes.len() - keep) as u64;

        Ok(PtyRead {
            content: strip_controls(&String::from_utf8_lossy(&bytes[..keep])),
            more: s.read_cursor < s.buf.total_written(),
            dropped,
            exited: s.exited,
        })
    }

    async fn screen(
        &self,
        pty: PtyId,
        input: Option<String>,
        settle: Option<Settle>,
    ) -> zyris::Result<PtyScreen> {
        self.settle(&pty, input, settle).await?;

        let sessions = self.sessions.lock().unwrap();
        let s = sessions.get(&pty.0).ok_or_else(gone)?;
        // **`read_cursor` is left untouched.** The screen is a render of accumulated state, not a consumption of it.
        let screen = s.parser.screen();
        let (_, cols) = screen.size();
        let (cursor_row, cursor_col) = screen.cursor_position();
        Ok(PtyScreen {
            lines: screen.rows(0, cols).collect(),
            cursor_row,
            cursor_col,
            exited: s.exited,
        })
    }

    async fn write(&self, pty: PtyId, data: Blob) -> zyris::Result<()> {
        let bytes = match data {
            Blob::Inline(b) => b,
            Blob::Attachment(_) => {
                return Err(WireError::invalid_params("attachment blobs unsupported for pty write"))
            }
        };
        let mut sessions = self.sessions.lock().unwrap();
        let s = sessions.get_mut(&pty.0).ok_or_else(gone)?;
        s.touch();
        s.write_input(&bytes)
    }

    async fn resize(&self, pty: PtyId, cols: u16, rows: u16) -> zyris::Result<()> {
        let mut sessions = self.sessions.lock().unwrap();
        let s = sessions.get_mut(&pty.0).ok_or_else(gone)?;
        s.touch();
        s.resize(cols, rows)
    }

    async fn close(&self, pty: PtyId) -> zyris::Result<()> {
        self.sessions.lock().unwrap().remove(&pty.0);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn exec(
        &self,
        command: Option<String>,
        argv: Option<Vec<String>>,
        cwd: Option<String>,
        timeout_ms: Option<u64>,
        stdin: Option<String>,
        env: Option<HashMap<String, String>>,
        shell: Option<String>,
    ) -> zyris::Result<ExecOutput> {
        use std::process::Stdio;
        use tokio::io::AsyncWriteExt;
        use tokio::process::Command;

        let argv = match argv {
            Some(v) if v.is_empty() => {
                return Err(WireError::invalid_params("argv must not be empty"))
            }
            other => other,
        };
        let has_command = matches!(command.as_deref(), Some(c) if !c.trim().is_empty());
        match (has_command, argv.is_some()) {
            (false, false) => return Err(WireError::invalid_params("give `command` or `argv`, not neither")),
            (true, true) => return Err(WireError::invalid_params("give `command` or `argv`, not both")),
            _ => {}
        }
        // Windows command mode is always cmd /C, so shell goes unused (PowerShell is chosen via argv).
        #[cfg(not(unix))]
        let _ = &shell;

        let mut cmd = match argv {
            // argv mode: no shell is involved. Arguments go straight to the program, so no
            // escaping is needed — this is what makes PowerShell's gnarly quoting rules moot.
            Some(argv) => {
                let mut c = Command::new(&argv[0]);
                c.args(&argv[1..]);
                c
            }
            None => {
                #[cfg(unix)]
                {
                    let mut c = Command::new(shell.unwrap_or_else(|| "/bin/sh".to_string()));
                    c.arg("-c").arg(command.unwrap_or_default());
                    c
                }
                #[cfg(not(unix))]
                {
                    // cmd /C has the worst quoting rules, so PowerShell is right to be called via argv instead.
                    let mut c = Command::new("cmd");
                    c.arg("/C").arg(command.unwrap_or_default());
                    c
                }
            }
        };

        cmd.current_dir(match cwd {
            Some(cwd) => resolve_under(&self.root, &cwd),
            None => self.root.clone(),
        });
        if let Some(env) = env {
            cmd.envs(env);
        }
        cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        // On Unix, make the child the leader of a new process group — so on timeout the whole
        // tree can be killed (including any grandchildren the shell left behind).
        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = cmd.spawn().map_err(pty_err)?;
        let pid = child.id();

        if let Some(input) = stdin {
            if let Some(mut sin) = child.stdin.take() {
                tokio::spawn(async move {
                    let _ = sin.write_all(input.as_bytes()).await;
                    let _ = sin.shutdown().await;
                });
            }
        }

        // A separate task collects each output stream up to cap. Even past the cap it **keeps
        // reading and draining it** — stopping would let the child's pipe fill up and block forever.
        let stdout = child.stdout.take().expect("stdout was launched as piped");
        let stderr = child.stderr.take().expect("stderr was launched as piped");
        let out_acc = Arc::new(Mutex::new(OutputAcc::default()));
        let err_acc = Arc::new(Mutex::new(OutputAcc::default()));
        let out_task = tokio::spawn(drain_capped(stdout, out_acc.clone(), EXEC_OUTPUT_CAP));
        let err_task = tokio::spawn(drain_capped(stderr, err_acc.clone(), EXEC_OUTPUT_CAP));

        let (exit_code, timed_out) = match timeout_ms {
            Some(ms) => match tokio::time::timeout(Duration::from_millis(ms), child.wait()).await {
                Ok(status) => (status.map_err(pty_err)?.code().unwrap_or(-1), false),
                // Timeout: kill and reap the whole tree. The previous implementation just dropped
                // the child, which left zombies and orphans behind.
                Err(_) => {
                    kill_tree(pid);
                    let _ = child.wait().await;
                    (-1, true)
                }
            },
            None => (child.wait().await.map_err(pty_err)?.code().unwrap_or(-1), false),
        };

        // The pipes close, and the reader tasks with them, only once the child dies or exits.
        let _ = out_task.await;
        let _ = err_task.await;

        let (stdout, stdout_truncated) = {
            let acc = out_acc.lock().unwrap();
            (String::from_utf8_lossy(&acc.buf).into_owned(), acc.truncated)
        };
        let (stderr, stderr_truncated) = {
            let acc = err_acc.lock().unwrap();
            (String::from_utf8_lossy(&acc.buf).into_owned(), acc.truncated)
        };

        Ok(ExecOutput { exit_code, stdout, stderr, timed_out, stdout_truncated, stderr_truncated })
    }
}
