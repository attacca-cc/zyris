use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, MasterPty, NativePtySystem, PtyPair, PtySize, PtySystem};
use zyris::{ErrorCode, WireError};

use super::buffer::OutputBuffer;

pub(crate) const RING_MAX: usize = 1024 * 1024;

/// The interval at which the settle loop and stream subscribers re-check state.
///
/// Why polling instead of `Notify`: the reader is a std thread and the waiter is a tokio task, so
/// a notification would introduce a race — bytes could arrive between the check and the wait. The
/// settle loop has to sleep briefly anyway to re-test whether quiet has expired, so a notification
/// would not buy anything.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(20);

pub(crate) type Sessions = Arc<Mutex<HashMap<String, PtySession>>>;

pub(crate) struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    pub(crate) buf: OutputBuffer,
    pub(crate) parser: vt100::Parser,
    /// The session's read position. **One per session, not one per caller** — an `open_stream`
    /// subscriber carries its own cursor separately, so it does not affect this one.
    pub(crate) read_cursor: u64,
    /// The exit code, once the shell has finished. `None` while it is still alive.
    pub(crate) exited: Option<i32>,
    /// When the PTY last produced a byte. What settle's "quiet" judgment is based on.
    pub(crate) last_byte_at: Instant,
    /// When a tool last touched this session. What the idle sweeper is based on.
    pub(crate) last_touch: Instant,
}

impl PtySession {
    pub(crate) fn touch(&mut self) {
        self.last_touch = Instant::now();
    }

    pub(crate) fn write_input(&mut self, bytes: &[u8]) -> zyris::Result<()> {
        self.writer.write_all(bytes).map_err(super::pty_err)?;
        self.writer.flush().map_err(super::pty_err)
    }

    pub(crate) fn resize(&mut self, cols: u16, rows: u16) -> zyris::Result<()> {
        self.master
            .resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(super::pty_err)?;
        // The screen model has to follow along too, or `screen` keeps rendering at the old width.
        self.parser.screen_mut().set_size(rows, cols);
        Ok(())
    }
}

pub(crate) fn gone() -> WireError {
    WireError::new(ErrorCode::Other("pty_gone".into()), "no such pty")
}

/// Spawns a shell, inserts the session into the map, then starts a thread that copies output into
/// the ring buffer and the screen model.
pub(crate) fn spawn(
    sessions: &Sessions,
    id: String,
    shell: String,
    cwd: &Path,
    cols: u16,
    rows: u16,
) -> zyris::Result<()> {
    let system = NativePtySystem::default();
    let pair = system
        .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
        .map_err(super::pty_err)?;
    let mut cmd = CommandBuilder::new(shell);
    cmd.cwd(cwd);
    let PtyPair { slave, master } = pair;
    let mut child = slave.spawn_command(cmd).map_err(super::pty_err)?;
    let mut reader = master.try_clone_reader().map_err(super::pty_err)?;
    let writer = master.take_writer().map_err(super::pty_err)?;

    // **Drop the slave here.** If the session kept holding onto it, the slave fd would stay open,
    // so even after the shell dies the master would never see EOF and the reader thread would be
    // stuck in `read` forever. That would leave `exited` never populated, so the agent would never
    // find out the shell finished.
    drop(slave);

    let now = Instant::now();
    sessions.lock().unwrap().insert(
        id.clone(),
        PtySession {
            master,
            writer,
            buf: OutputBuffer::new(RING_MAX),
            parser: vt100::Parser::new(rows, cols, 0),
            read_cursor: 0,
            exited: None,
            last_byte_at: now,
            last_touch: now,
        },
    );

    let sessions = sessions.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut guard = sessions.lock().unwrap();
                    // If the session is gone (`close` or the sweeper), there's no reason to keep reading.
                    let Some(s) = guard.get_mut(&id) else { break };
                    s.buf.push(&buf[..n]);
                    s.parser.process(&buf[..n]);
                    s.last_byte_at = Instant::now();
                }
            }
        }
        let code = child.wait().ok().and_then(|st| i32::try_from(st.exit_code()).ok()).unwrap_or(-1);
        // **Do not remove the session.** Removing it would lose the last output the shell produced
        // while dying, for good. Actual removal is left to `close` or the idle sweeper (spec §4).
        if let Some(s) = sessions.lock().unwrap().get_mut(&id) {
            s.exited = Some(code);
        }
    });

    Ok(())
}
