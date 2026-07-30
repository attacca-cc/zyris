mod buffer;
mod session;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use zyris::{Blob, ErrorCode, Streaming, WireError};

use zyris_caps::{ExecOutput, PtyChunk, PtyId, PtyOpened, Terminal};

use crate::path::resolve_under;
use session::{gone, Sessions, POLL_INTERVAL};

/// `open_stream` 구독자가 한 청크에 싣는 최대 바이트.
const STREAM_CHUNK_MAX: usize = 64 * 1024;

pub struct PtyTerminal {
    default_shell: String,
    root: PathBuf,
    next_id: AtomicU64,
    sessions: Sessions,
}

impl PtyTerminal {
    pub fn rooted(root: impl Into<PathBuf>) -> Self {
        PtyTerminal { root: root.into(), ..PtyTerminal::default() }
    }

    fn new_session(&self, shell: Option<String>, cols: u16, rows: u16) -> zyris::Result<String> {
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
}

/// 링버퍼를 자기 커서로 훑는 `open_stream` 구독자.
///
/// 세션의 `read_cursor`를 건드리지 않으므로, 스트림을 붙여 둔 채 `read`를 불러도
/// 서로 바이트를 빼앗지 않는다.
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
        }
    }
}

pub(crate) fn pty_err(msg: impl std::fmt::Display) -> WireError {
    WireError::new(ErrorCode::Internal, msg.to_string())
}

#[zyris::async_trait]
impl Terminal for PtyTerminal {
    async fn open(
        &self,
        shell: Option<String>,
        cols: u16,
        rows: u16,
    ) -> zyris::Result<Streaming<PtyOpened, PtyChunk>> {
        let id = self.new_session(shell, cols, rows)?;
        let items = subscribe(self.sessions.clone(), id.clone());
        Ok(Streaming::new(PtyOpened { pty: PtyId(id) }, items))
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

    async fn exec(
        &self,
        command: String,
        cwd: Option<String>,
        timeout_ms: Option<u64>,
    ) -> zyris::Result<ExecOutput> {
        let mut cmd = if cfg!(windows) {
            let mut c = tokio::process::Command::new("cmd");
            c.arg("/C").arg(&command);
            c
        } else {
            let mut c = tokio::process::Command::new("/bin/sh");
            c.arg("-c").arg(&command);
            c
        };
        cmd.current_dir(match cwd {
            Some(cwd) => resolve_under(&self.root, &cwd),
            None => self.root.clone(),
        });
        let child = cmd.output();
        let output = match timeout_ms {
            Some(ms) => match tokio::time::timeout(std::time::Duration::from_millis(ms), child)
                .await
            {
                Ok(result) => result.map_err(pty_err)?,
                Err(_) => {
                    return Ok(ExecOutput {
                        exit_code: -1,
                        stdout: String::new(),
                        stderr: "command timed out".into(),
                        timed_out: true,
                    })
                }
            },
            None => child.await.map_err(pty_err)?,
        };
        Ok(ExecOutput {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            timed_out: false,
        })
    }
}
