use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use portable_pty::{CommandBuilder, NativePtySystem, PtyPair, PtySize, PtySystem};
use tokio::sync::mpsc;
use zyris::{Blob, ErrorCode, Streaming, WireError};

use crate::path::resolve_under;
use crate::terminal::{ExecOutput, PtyChunk, PtyId, PtyOpened, Terminal};

struct PtySession {
    pair: PtyPair,
    writer: Box<dyn Write + Send>,
}

pub struct PtyTerminal {
    default_shell: String,
    root: PathBuf,
    next_id: AtomicU64,
    sessions: Arc<Mutex<HashMap<String, PtySession>>>,
}

impl PtyTerminal {
    pub fn rooted(root: impl Into<PathBuf>) -> Self {
        PtyTerminal { root: root.into(), ..PtyTerminal::default() }
    }
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

fn pty_err(msg: impl std::fmt::Display) -> WireError {
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
        let system = NativePtySystem::default();
        let pair = system
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(pty_err)?;
        let mut cmd = CommandBuilder::new(shell.unwrap_or_else(|| self.default_shell.clone()));
        cmd.cwd(&self.root);
        let mut child = pair.slave.spawn_command(cmd).map_err(pty_err)?;
        let mut reader = pair.master.try_clone_reader().map_err(pty_err)?;
        let writer = pair.master.take_writer().map_err(pty_err)?;

        let id = format!("pty-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        self.sessions
            .lock()
            .unwrap()
            .insert(id.clone(), PtySession { pair, writer });

        let (tx, rx) = mpsc::channel::<zyris::Result<PtyChunk>>(64);
        let sessions = self.sessions.clone();
        let reader_id = id.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let chunk = PtyChunk {
                            data: Blob::from_bytes(Bytes::copy_from_slice(&buf[..n])),
                        };
                        if tx.blocking_send(Ok(chunk)).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = child.wait();
            sessions.lock().unwrap().remove(&reader_id);
        });

        let items = tokio_stream_from_receiver(rx);
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
        let session = sessions
            .get_mut(&pty.0)
            .ok_or_else(|| WireError::new(ErrorCode::Other("pty_gone".into()), "no such pty"))?;
        session.writer.write_all(&bytes).map_err(pty_err)?;
        session.writer.flush().map_err(pty_err)
    }

    async fn resize(&self, pty: PtyId, cols: u16, rows: u16) -> zyris::Result<()> {
        let sessions = self.sessions.lock().unwrap();
        let session = sessions
            .get(&pty.0)
            .ok_or_else(|| WireError::new(ErrorCode::Other("pty_gone".into()), "no such pty"))?;
        session
            .pair
            .master
            .resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(pty_err)
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

fn tokio_stream_from_receiver(
    rx: mpsc::Receiver<zyris::Result<PtyChunk>>,
) -> impl futures_util::Stream<Item = zyris::Result<PtyChunk>> {
    futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    })
}
