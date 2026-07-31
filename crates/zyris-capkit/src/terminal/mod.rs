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

/// `open_stream` 구독자가 한 청크에 싣는 최대 바이트.
const STREAM_CHUNK_MAX: usize = 64 * 1024;

/// `read` 한 번이 돌려주는 최대 바이트. `file_io`의 `READ_UNARY_MAX`와 같은 값이다 —
/// Attacca의 `ZYRIS_MAX_RESULT_BYTES`(기본 1,000,000) 예산 통과가 실측으로 확인된 값.
const PTY_READ_MAX: usize = 128 * 1024;

/// 동시에 열어 둘 수 있는 세션 수. 상한이 없으면 루프에 빠진 에이전트가 셸을 무한히 뽑는다.
const MAX_SESSIONS: usize = 8;

/// 아무도 만지지 않는 세션을 닫기까지의 시간.
///
/// 스펙 §5.1이 연결 추적 배선 대신 이것으로 덮기로 한 지점이다 — 와이어만 끊긴 경우
/// 아무도 `read`를 부르지 않으므로 결국 여기서 수거된다. 대가는 끊김 후 최대 이만큼
/// 셸 프로세스가 살아 있다는 것이다.
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

    /// 유휴 타임아웃 주입점. 테스트가 10분을 기다릴 수는 없다.
    pub fn with_idle_timeout(mut self, d: Duration) -> Self {
        self.idle_timeout = d;
        self
    }

    /// 유휴 스위퍼를 처음 `open` 때 건다.
    ///
    /// 생성자에서 걸 수 없다 — `PtyTerminal::default()`는 tokio 런타임 밖에서도 불리고
    /// `tokio::spawn`은 거기서 패닉한다. `open`은 async라 런타임 안이 보장된다.
    ///
    /// 세션 맵을 `Weak`으로 잡는다. 강하게 잡으면 `local.declare` 재선언으로
    /// `PtyTerminal`이 drop돼도 스위퍼가 맵을 붙들고 있어 셸이 살아남는다.
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
                // 세션이 맵에서 빠지면 `PtySession`이 drop되며 master fd가 닫히고,
                // 슬레이브 쪽 셸이 SIGHUP을 받는다.
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

    /// `input`을 쓰고, 출력이 조용해질 때까지 기다린다.
    ///
    /// 반환 조건은 셋 중 먼저 오는 것이다: 조용함이 만족됐고 게이트가 열렸다 /
    /// deadline에 닿았다 / 셸이 끝났고 조용하다.
    ///
    /// **게이트는 `input`이 있을 때만 건다.** 입력이 아무 출력도 유발하지 않는 경우
    /// 조용함은 즉시 만족되는데, 그대로 빈 응답을 돌려주면 에이전트가 "명령이 끝났다"로
    /// 오해한다. deadline까지 기다려 진짜 아무것도 없음을 확인하는 편이 정직하다.
    /// 반대로 `input`이 없는 순수 관찰까지 붙잡으면 폴링 한 번이 timeout_ms를 먹는다.
    ///
    /// **게이트의 한계**: cooked 모드에서는 터미널이 타이핑된 바이트를 즉시 에코하므로
    /// "읽을 게 생겼다"가 곧바로 참이 되어 게이트가 열린다. 명령이 늦게 출력을 시작하면
    /// 에코만 담긴 응답이 먼저 나갈 수 있다 — 그때 `more`는 false고 `exited`는 `None`이라
    /// 에이전트가 "아직 안 끝났다"를 읽어낼 수 있으니, 다시 부르면 이어진다.
    /// 게이트가 값을 하는 곳은 에코가 없는 raw 모드(vim·htop에 키를 넣는 경우)다.
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
                // 셸이 끝났고 조용하면 더 올 것이 없다 — 게이트와 무관하게 나간다.
                if is_quiet && s.exited.is_some() {
                    return Ok(());
                }
                // 게이트는 **이 호출이 시작된 뒤 새로 온 바이트**를 본다. 이미 쌓여
                // 있던 미독 바이트로 열어주면, 앞 호출이 남긴 프롬프트 한 조각 때문에
                // 게이트가 늘 열린 채가 되어 아무것도 막지 못한다.
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
            idle_timeout: IDLE_TIMEOUT,
            sweeper_started: AtomicBool::new(false),
        }
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

        // 잘린 꼬리는 다음 호출로 넘긴다 — 멀티바이트 문자든 이스케이프 시퀀스든.
        // 단 셸이 끝났고 이게 마지막 바이트라면 넘길 다음이 없으므로 그냥 태운다.
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
        // **`read_cursor`를 건드리지 않는다.** 화면은 누적 상태의 렌더이지 소비가 아니다.
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
