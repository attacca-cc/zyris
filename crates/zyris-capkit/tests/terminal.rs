#![cfg(feature = "terminal")]

use std::time::Duration;

use zyris::{Blob, Node, NodeKind};
use zyris_caps::{ExecOutput, PtyId, PtyRead, PtyScreen, Settle, Terminal, TerminalClient, TerminalServer};
use zyris_capkit::PtyTerminal;

fn blob(s: &str) -> Blob {
    Blob::from_bytes(bytes::Bytes::copy_from_slice(s.as_bytes()))
}

fn inline(b: Blob) -> Vec<u8> {
    match b {
        Blob::Inline(b) => b.to_vec(),
        Blob::Attachment(_) => panic!("attachment는 여기 오지 않는다"),
    }
}

async fn connect(term: PtyTerminal) -> TerminalClient {
    let server = Node::builder()
        .name("term-node")
        .kind(NodeKind::Service)
        .capability(TerminalServer(term))
        .build()
        .unwrap();
    let client_node = Node::builder().name("client").kind(NodeKind::Cli).build().unwrap();
    let (conn, _server) = zyris::testing::duplex(&client_node, &server).await.unwrap();
    conn.wait_capability(Duration::from_secs(2)).await.unwrap()
}

#[tokio::test]
async fn exec_runs_a_command() {
    let term = connect(PtyTerminal::default()).await;

    let out: ExecOutput = term.exec("echo hello-zyris".into(), None, Some(5000)).await.unwrap();
    assert_eq!(out.exit_code, 0);
    assert!(out.stdout.contains("hello-zyris"));
    assert!(!out.timed_out);
}

#[cfg(unix)]
#[tokio::test]
async fn exec_resolves_cwd() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    std::fs::create_dir(root_path.join("sub")).unwrap();

    let term = connect(PtyTerminal::rooted(&root_path)).await;

    async fn pwd(term: &TerminalClient, cwd: Option<&str>) -> String {
        let out = term.exec("pwd".into(), cwd.map(str::to_string), Some(5000)).await.unwrap();
        assert_eq!(out.exit_code, 0);
        out.stdout.trim().to_string()
    }

    assert_eq!(pwd(&term, None).await, root_path.to_string_lossy());
    assert_eq!(pwd(&term, Some("sub")).await, root_path.join("sub").to_string_lossy());
    assert_eq!(
        pwd(&term, Some("/tmp")).await,
        std::path::Path::new("/tmp").canonicalize().unwrap().to_string_lossy()
    );
}

// ── 리더 스레드가 소비자에 매이지 않는다 ────────────────────────────────

/// 아무도 스트림을 빼가지 않는 동안에도 PTY는 계속 돌아야 한다.
///
/// 판정은 **스트림을 한 번도 건드리지 않고** 파일 시스템으로 한다. 리더가 막히면 셸이
/// PTY에 쓰다 함께 막혀 마지막 `touch`에 영영 도달하지 못한다. 스트림을 읽어서
/// 판정하면 막힌 리더가 소비와 함께 풀려버려 아무것도 구분하지 못한다.
#[cfg(unix)]
#[tokio::test]
async fn the_reader_never_blocks_without_a_consumer() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("done");

    let term = connect(PtyTerminal::default()).await;
    let _stream = term.open_stream(Some("/bin/sh".into()), 80, 24).await.unwrap();
    let pty = _stream.head.pty.clone();

    // mpsc(64) × 8 KiB ≈ 512 KiB를 훌쩍 넘는 출력을 PTY로 낸다.
    term.write(
        pty,
        blob(&format!("yes zyris | head -c 2000000; touch {}\n", marker.display())),
    )
    .await
    .unwrap();

    for _ in 0..100 {
        if marker.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("소비자가 없자 PTY가 멈췄다 — 셸이 마지막 명령에 도달하지 못했다");
}

/// 셸이 끝나면 두 가지가 동시에 성립해야 한다: 스트림이 **끝나고**, 핸들은 **남는다**.
///
/// v1은 둘 다 못 한다. 세션이 `pair.slave`를 계속 쥐고 있어 master가 EOF를 못 보므로
/// 리더 스레드가 영영 `read`에 매달리고, 설령 EOF가 오더라도 리더가 세션을 맵에서
/// 지워버린다. 지우면 셸이 죽으며 낸 마지막 출력을 에이전트가 영영 못 본다 (스펙 §4).
#[cfg(unix)]
#[tokio::test]
async fn a_session_survives_the_shell_exiting() {
    use futures_util::StreamExt;

    let term = connect(PtyTerminal::default()).await;
    let mut stream = term.open_stream(Some("/bin/sh".into()), 80, 24).await.unwrap();
    let pty = stream.head.pty.clone();

    term.write(pty.clone(), blob("echo LAST-WORD; exit 7\n")).await.unwrap();

    let mut seen = String::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(5), stream.items.next()).await {
            Ok(Some(Ok(chunk))) => seen.push_str(&String::from_utf8_lossy(&inline(chunk.data))),
            Ok(Some(Err(e))) => panic!("스트림 오류: {e}"),
            Ok(None) => break,
            Err(_) => panic!("셸이 끝났는데 스트림이 끝나지 않았다 — 본 것: {seen:?}"),
        }
    }
    assert!(seen.contains("LAST-WORD"), "마지막 출력을 놓쳤다: {seen:?}");

    // 스트림이 끝나도 핸들은 유효하다 — `pty_gone`이 아니라 정상 응답이어야 한다.
    term.resize(pty.clone(), 100, 30).await.expect("셸이 끝나도 세션은 남아야 한다");
    term.close(pty).await.unwrap();
}

// ── unary 경로: open / read / screen ────────────────────────────────────

/// 짧은 settle. 벽시계에 매달리지 않도록 테스트는 항상 값을 주입한다.
fn quick() -> Option<Settle> {
    Some(Settle { quiet_ms: 60, timeout_ms: 3000 })
}

async fn open_sh(term: &TerminalClient) -> PtyId {
    let opened = term.open(Some("/bin/sh".into()), 80, 24).await.unwrap();
    // 셸이 뜨며 낸 프롬프트를 흘려보낸다. 여기서 조급하게 끊으면 그 바이트가 다음
    // 테스트의 `content`에 섞여 들어온다.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _ = term
        .read(opened.pty.clone(), None, Some(Settle { quiet_ms: 150, timeout_ms: 3000 }))
        .await
        .unwrap();
    opened.pty
}

/// `exec`으로는 불가능한 것 — 상태가 다음 호출까지 이어진다.
#[cfg(unix)]
#[tokio::test]
async fn state_carries_across_calls() {
    let term = connect(PtyTerminal::default()).await;
    let pty = open_sh(&term).await;

    term.read(pty.clone(), Some("cd /tmp\n".into()), quick()).await.unwrap();
    let out: PtyRead = term.read(pty.clone(), Some("pwd\n".into()), quick()).await.unwrap();

    assert!(out.content.contains("/tmp"), "본 것: {:?}", out.content);
    assert_eq!(out.exited, None);
    assert_eq!(out.dropped, 0);
}

/// `quiet_ms == 0`은 기다리지 않는다.
#[cfg(unix)]
#[tokio::test]
async fn quiet_zero_returns_immediately() {
    let term = connect(PtyTerminal::default()).await;
    let pty = open_sh(&term).await;

    let t0 = std::time::Instant::now();
    term.read(pty, None, Some(Settle { quiet_ms: 0, timeout_ms: 10_000 })).await.unwrap();
    assert!(t0.elapsed() < Duration::from_millis(500), "걸린 시간: {:?}", t0.elapsed());
}

/// settle의 반환 경로 둘 중 **조용함** 쪽. 출력이 금방 끝나면 timeout_ms를 기다리지 않는다.
#[cfg(unix)]
#[tokio::test]
async fn a_quick_command_returns_on_quiet_not_on_timeout() {
    let term = connect(PtyTerminal::default()).await;
    let pty = open_sh(&term).await;

    let t0 = std::time::Instant::now();
    let out: PtyRead = term
        .read(pty, Some("echo FAST\n".into()), Some(Settle { quiet_ms: 80, timeout_ms: 10_000 }))
        .await
        .unwrap();

    assert!(out.content.contains("FAST"), "본 것: {:?}", out.content);
    assert!(t0.elapsed() < Duration::from_secs(3), "timeout까지 붙잡혔다: {:?}", t0.elapsed());
}

/// 멀티바이트 문자가 PTY를 통과해도 온전하다. 청크 경계에서 쪼개졌다면 대체 문자가 섞인다.
/// 경계를 실제로 강제할 수는 없으므로(커널이 청크를 나눈다) 여기서는 **왕복 무결성**만 보고,
/// 경계 규칙 자체는 `buffer.rs`의 `trim_incomplete_tail` 단위 테스트가 결정론적으로 덮는다.
#[cfg(unix)]
#[tokio::test]
async fn multibyte_output_survives_intact() {
    let term = connect(PtyTerminal::default()).await;
    let pty = open_sh(&term).await;

    let out: PtyRead = term
        .read(
            pty,
            Some("printf '가나다라🎯\\n'\n".into()),
            Some(Settle { quiet_ms: 150, timeout_ms: 5000 }),
        )
        .await
        .unwrap();

    assert!(out.content.contains("가나다라🎯"), "본 것: {:?}", out.content);
    assert!(!out.content.contains('\u{FFFD}'), "대체 문자가 섞였다: {:?}", out.content);
}

/// 출력을 내지 않는 입력은 빈 응답을 **조기에** 돌려주면 안 된다 (스펙 §3.4).
/// 조기 반환하면 에이전트가 "명령이 끝났다"로 오해한다.
///
/// **에코를 꺼야 이 게이트가 실제로 걸린다.** cooked 모드에서는 터미널이 타이핑된
/// 바이트를 즉시 되돌려주므로 "읽을 게 생겼다"가 곧바로 만족되어 게이트가 열린다.
/// 게이트가 값을 하는 곳은 에코가 없는 raw 모드 — 즉 vim·htop 같은 풀스크린 프로그램에
/// 키를 넣고 화면이 반응하기를 기다리는, 바로 그 경우다.
#[cfg(unix)]
#[tokio::test]
async fn an_input_with_no_output_waits_for_the_deadline() {
    let term = connect(PtyTerminal::default()).await;
    let pty = open_sh(&term).await;

    // 에코를 끄고 readline이 아닌 소비자를 전면에 둔다. bash는 프롬프트를 그릴 때마다
    // readline이 터미널 모드를 되돌려 놓으므로 `stty -echo`만으로는 유지되지 않는다.
    term.read(
        pty.clone(),
        Some("stty -echo; cat > /dev/null\n".into()),
        Some(Settle { quiet_ms: 200, timeout_ms: 3000 }),
    )
    .await
    .unwrap();

    let t0 = std::time::Instant::now();
    let out: PtyRead = term
        .read(pty, Some("silent-input\n".into()), Some(Settle { quiet_ms: 50, timeout_ms: 600 }))
        .await
        .unwrap();

    // `cat`이 /dev/null로 삼키므로 PTY는 완전히 조용하다. 게이트가 없으면 50ms에 빈 응답이 나간다.
    assert!(t0.elapsed() >= Duration::from_millis(550), "걸린 시간: {:?}", t0.elapsed());
    assert!(out.content.is_empty(), "에코가 꺼졌는데 뭔가 왔다: {:?}", out.content);
}

/// 반면 `input`이 없는 순수 관찰은 붙잡지 않는다 — 매 폴링이 timeout_ms를 먹으면 못 쓴다.
#[cfg(unix)]
#[tokio::test]
async fn a_pure_observation_does_not_wait_for_the_deadline() {
    let term = connect(PtyTerminal::default()).await;
    let pty = open_sh(&term).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let t0 = std::time::Instant::now();
    term.read(pty, None, Some(Settle { quiet_ms: 50, timeout_ms: 5000 })).await.unwrap();
    assert!(t0.elapsed() < Duration::from_millis(1000), "걸린 시간: {:?}", t0.elapsed());
}

/// 한 번에 못 담으면 `more: true`로 알리고, 다시 부르면 **순서대로** 이어진다.
#[cfg(unix)]
#[tokio::test]
async fn more_paginates_in_order() {
    let term = connect(PtyTerminal::default()).await;
    let pty = open_sh(&term).await;

    // PTY_READ_MAX(128 KiB)를 넘기되 RING_MAX(1 MiB) 안에 드는 양.
    term.read(
        pty.clone(),
        Some("seq 1 40000\n".into()),
        Some(Settle { quiet_ms: 300, timeout_ms: 15_000 }),
    )
    .await
    .unwrap();

    let mut all = String::new();
    let mut saw_more = false;
    for _ in 0..40 {
        let out: PtyRead =
            term.read(pty.clone(), None, Some(Settle { quiet_ms: 0, timeout_ms: 1000 })).await.unwrap();
        assert_eq!(out.dropped, 0, "1 MiB 링 안에 드는 양인데 잃었다");
        all.push_str(&out.content);
        saw_more |= out.more;
        if !out.more {
            break;
        }
    }
    assert!(saw_more, "128 KiB를 넘겼는데 more가 한 번도 안 섰다");
    let a = all.find("39998").expect("39998이 없다");
    let b = all.find("39999").expect("39999가 없다");
    assert!(a < b, "순서가 뒤집혔다");
}

/// 셸이 끝난 뒤에도 마지막 출력과 종료 코드를 받는다 (스펙 §4).
#[cfg(unix)]
#[tokio::test]
async fn output_and_exit_code_survive_the_shell_exiting() {
    let term = connect(PtyTerminal::default()).await;
    let pty = open_sh(&term).await;

    let out: PtyRead = term
        .read(
            pty.clone(),
            Some("echo LAST-WORD; exit 7\n".into()),
            Some(Settle { quiet_ms: 100, timeout_ms: 5000 }),
        )
        .await
        .unwrap();

    assert!(out.content.contains("LAST-WORD"), "본 것: {:?}", out.content);
    assert_eq!(out.exited, Some(7));
}

/// vt100 제어문자가 격자로 렌더된다.
#[cfg(unix)]
#[tokio::test]
async fn screen_renders_cursor_addressing() {
    let term = connect(PtyTerminal::default()).await;
    let pty = open_sh(&term).await;

    // 화면을 지우고 3행 5열로 옮겨 쓴다.
    let s: PtyScreen = term
        .screen(
            pty,
            Some("printf '\\033[2J\\033[3;5HMARKER'\n".into()),
            Some(Settle { quiet_ms: 150, timeout_ms: 5000 }),
        )
        .await
        .unwrap();

    assert_eq!(s.lines.len(), 24, "행 수가 rows와 달라선 안 된다");
    assert!(s.lines[2].starts_with("    MARKER"), "3행이 기대와 다르다: {:?}", s.lines[2]);
}

/// `screen`은 읽기 커서를 전진시키지 않는다 — 화면은 누적 상태의 렌더라 소비 개념이 없다.
/// 이걸 반대로 만들면 `screen` 한 번에 빌드 로그가 통째로 사라진다.
#[cfg(unix)]
#[tokio::test]
async fn screen_does_not_advance_the_read_cursor() {
    let term = connect(PtyTerminal::default()).await;
    let pty = open_sh(&term).await;

    term.read(pty.clone(), Some("echo NEEDLE-IN-BUFFER\n".into()), quick()).await.unwrap();
    // 위 read가 이미 가져갔으므로 새 출력을 하나 더 만든다.
    term.write(pty.clone(), blob("echo SECOND-NEEDLE\n")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;

    term.screen(pty.clone(), None, Some(Settle { quiet_ms: 0, timeout_ms: 1000 })).await.unwrap();

    let out: PtyRead =
        term.read(pty, None, Some(Settle { quiet_ms: 0, timeout_ms: 1000 })).await.unwrap();
    assert!(out.content.contains("SECOND-NEEDLE"), "screen이 커서를 먹었다: {:?}", out.content);
}
