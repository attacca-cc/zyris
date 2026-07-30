use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, MasterPty, NativePtySystem, PtyPair, PtySize, PtySystem};
use zyris::{ErrorCode, WireError};

use super::buffer::OutputBuffer;

pub(crate) const RING_MAX: usize = 1024 * 1024;

/// settle 루프와 스트림 구독자가 상태를 다시 보는 주기.
///
/// `Notify`가 아니라 폴링인 이유: 리더는 std 스레드고 대기자는 tokio 태스크라
/// 알림에는 "확인과 대기 사이에 도착한 바이트"라는 경합이 생긴다. settle 루프는
/// 어차피 조용함 만료를 재보려면 짧게 자야 하므로, 알림이 사줄 것이 없다.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(20);

pub(crate) type Sessions = Arc<Mutex<HashMap<String, PtySession>>>;

pub(crate) struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    pub(crate) buf: OutputBuffer,
    pub(crate) parser: vt100::Parser,
    /// 세션의 읽기 위치. **호출자당이 아니라 세션당 하나다** — `open_stream` 구독자는
    /// 자기 커서를 따로 들고 다니므로 여기에 영향을 주지 않는다.
    pub(crate) read_cursor: u64,
    /// 셸이 끝났으면 종료 코드. 살아 있으면 `None`.
    pub(crate) exited: Option<i32>,
    /// 마지막으로 PTY에서 바이트가 온 시각. settle의 "조용함" 판정 기준.
    pub(crate) last_byte_at: Instant,
    /// 마지막 도구 접촉 시각. 유휴 스위퍼의 기준.
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
        // 화면 모델도 같이 따라가야 `screen`이 새 폭으로 렌더된다.
        self.parser.screen_mut().set_size(rows, cols);
        Ok(())
    }
}

pub(crate) fn gone() -> WireError {
    WireError::new(ErrorCode::Other("pty_gone".into()), "no such pty")
}

/// 셸을 띄우고 세션을 맵에 넣은 뒤, 출력을 링버퍼·화면 모델로 옮기는 스레드를 건다.
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

    // **슬레이브를 여기서 놓는다.** 세션이 계속 쥐고 있으면 슬레이브 fd가 열린 채라
    // 셸이 죽어도 master가 EOF를 보지 못하고, 리더 스레드가 영영 `read`에 매달린다.
    // 그러면 `exited`가 끝내 채워지지 않아 에이전트는 셸이 끝난 줄을 모른다.
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
                    // 세션이 사라졌으면(`close`·스위퍼) 더 읽을 이유가 없다.
                    let Some(s) = guard.get_mut(&id) else { break };
                    s.buf.push(&buf[..n]);
                    s.parser.process(&buf[..n]);
                    s.last_byte_at = Instant::now();
                }
            }
        }
        let code = child.wait().ok().and_then(|st| i32::try_from(st.exit_code()).ok()).unwrap_or(-1);
        // **세션을 지우지 않는다.** 지우면 셸이 죽으며 낸 마지막 출력을 영영 못 본다.
        // 실제 제거는 `close` 또는 유휴 스위퍼의 몫이다 (스펙 §4).
        if let Some(s) = sessions.lock().unwrap().get_mut(&id) {
            s.exited = Some(code);
        }
    });

    Ok(())
}
