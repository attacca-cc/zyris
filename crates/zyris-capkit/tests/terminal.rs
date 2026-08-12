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

    let out: ExecOutput = term
        .exec(Some("echo hello-zyris".into()), None, None, Some(5000), None, None, None)
        .await
        .unwrap();
    assert_eq!(out.exit_code, 0);
    assert!(out.stdout.contains("hello-zyris"));
    assert!(!out.timed_out);
}

/// `command` 모드(셸 one-liner)는 한 문자열을 그대로 `sh -c`에 넘기므로, 안에 든
/// 단일/이중 따옴표는 그대로 셸에 닿아야 한다 — Attacca가 따옴표 때문에 base64로
/// 우회하지 않아도 되는 이유다.
#[cfg(unix)]
#[tokio::test]
async fn exec_command_mode_passes_embedded_quotes_to_the_shell() {
    let term = connect(PtyTerminal::default()).await;

    let out = term
        .exec(Some("echo \"hello dq\"".into()), None, None, Some(5000), None, None, None)
        .await
        .unwrap();
    assert_eq!(out.exit_code, 0);
    assert_eq!(out.stdout.trim(), "hello dq");

    let out = term
        .exec(Some("echo 'hello sq'".into()), None, None, Some(5000), None, None, None)
        .await
        .unwrap();
    assert_eq!(out.exit_code, 0);
    assert_eq!(out.stdout.trim(), "hello sq");
}

#[cfg(unix)]
#[tokio::test]
async fn exec_resolves_cwd() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    std::fs::create_dir(root_path.join("sub")).unwrap();

    let term = connect(PtyTerminal::rooted(&root_path)).await;

    async fn pwd(term: &TerminalClient, cwd: Option<&str>) -> String {
        let out = term
            .exec(Some("pwd".into()), None, cwd.map(str::to_string), Some(5000), None, None, None)
            .await
            .unwrap();
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

// ── exec v3: argv / stdin / env / cap / timeout ──────────────────────────

/// argv 모드의 요점: 셸을 거치지 않으므로 공백·따옴표·`$`·백틱·`*`가 전부 그대로
/// 프로그램에 닿는다. 셸이 개입했다면 `$HOME`은 확장되고 백틱은 실행되고 `*`는 글롭된다.
#[cfg(unix)]
#[tokio::test]
async fn exec_argv_passes_arguments_unmangled() {
    let term = connect(PtyTerminal::default()).await;

    let out = term
        .exec(
            None,
            Some(vec![
                "/bin/echo".into(),
                "a b".into(),
                "c\"d".into(),
                "$HOME".into(),
                "`ls`".into(),
                "*".into(),
            ]),
            None,
            Some(5000),
            None,
            None,
            None,
        )
        .await
        .unwrap();

    assert_eq!(out.exit_code, 0);
    assert_eq!(out.stdout, "a b c\"d $HOME `ls` *\n");
    assert!(!out.timed_out);
}

/// PowerShell의 `-Command -` 패턴과 같은 모양 — 스크립트를 stdin으로 넣으면 인용이
/// 필요 없다. 여기서는 유닉스에서 `/bin/sh -s`로 같은 길을 검증한다.
#[cfg(unix)]
#[tokio::test]
async fn exec_reads_a_script_from_stdin() {
    let term = connect(PtyTerminal::default()).await;

    let script = "echo FROM-STDIN\nprintf 'x=%s\\n' 'a b'\n";
    let out = term
        .exec(
            None,
            Some(vec!["/bin/sh".into(), "-s".into()]),
            None,
            Some(5000),
            Some(script.into()),
            None,
            None,
        )
        .await
        .unwrap();

    assert_eq!(out.exit_code, 0);
    assert!(out.stdout.contains("FROM-STDIN"), "본 것: {:?}", out.stdout);
    assert!(out.stdout.contains("x=a b"), "본 것: {:?}", out.stdout);
}

/// `env` 항목은 노드 환경 위에 덮어써서 자식에 닿는다.
#[cfg(unix)]
#[tokio::test]
async fn exec_env_overrides_reach_the_child() {
    let term = connect(PtyTerminal::default()).await;

    let out = term
        .exec(
            Some("printf '%s' \"$ZYRIS_TEST_VAR\"".into()),
            None,
            None,
            Some(5000),
            None,
            Some(std::collections::HashMap::from([(
                "ZYRIS_TEST_VAR".into(),
                "from-env".into(),
            )])),
            None,
        )
        .await
        .unwrap();

    assert_eq!(out.exit_code, 0);
    assert_eq!(out.stdout, "from-env");
}

/// 출력은 스트림당 cap에서 잘리고, 넘친 바이트는 버려진다(`stdout_truncated`).
/// 자식은 끝까지 돌므로 exit code는 정상이다.
#[cfg(unix)]
#[tokio::test]
async fn exec_caps_output_and_reports_truncation() {
    let term = connect(PtyTerminal::default()).await;

    // EXEC_OUTPUT_CAP(1 MiB)를 넘는 stdout.
    let out = term
        .exec(Some("yes x | head -c 1500000".into()), None, None, Some(20_000), None, None, None)
        .await
        .unwrap();

    assert_eq!(out.exit_code, 0);
    assert_eq!(out.stdout.len(), 1024 * 1024, "cap보다 많이 돌려줬다");
    assert!(out.stdout_truncated, "cap을 넘겼는데 truncated가 아니다");
    assert!(!out.stderr_truncated);
}

/// 타임아웃이면 **프로세스 트리 전체**를 죽이고 부분 출력과 함께 돌아온다.
///
/// 예전 구현은 자식을 drop만 해서 `sleep 30`이 고아로 남았다. 여기서는 셸이 자기 pid를
/// 파일에 남기게 한 뒤, 타임아웃 후 그 pid가 죽었는지(ESRCH) 확인한다.
#[cfg(unix)]
#[tokio::test]
async fn exec_timeout_kills_the_process_tree() {
    let term = connect(PtyTerminal::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let cmd = format!("echo $$ > {}; sleep 30", pidfile.display());

    let out = term.exec(Some(cmd), None, None, Some(1500), None, None, None).await.unwrap();

    assert!(out.timed_out, "본 것: {out:?}");
    assert_eq!(out.exit_code, -1);

    let pid: i32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
    // SAFETY: 신호 0은 검사만 한다 — 프로세스가 살아 있으면 0, 없으면 -1(ESRCH).
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    assert!(!alive, "프로세스 그룹 {pid}이 타임아웃 킬 뒤에도 살아 있다");
}

/// `command`와 `argv`를 둘 다 주면 모호하므로 거부한다.
#[cfg(unix)]
#[tokio::test]
async fn exec_rejects_both_command_and_argv() {
    let term = connect(PtyTerminal::default()).await;

    let err = term
        .exec(Some("echo hi".into()), Some(vec!["echo".into()]), None, Some(5000), None, None, None)
        .await
        .unwrap_err();
    assert!(format!("{err:?}").contains("not both"), "본 오류: {err:?}");
}

/// 둘 다 빠지면 실행할 것이 없다.
#[cfg(unix)]
#[tokio::test]
async fn exec_rejects_neither_command_nor_argv() {
    let term = connect(PtyTerminal::default()).await;

    let err = term.exec(None, None, None, Some(5000), None, None, None).await.unwrap_err();
    assert!(format!("{err:?}").contains("not neither"), "본 오류: {err:?}");
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

// ── 수명: 상한과 유휴 수거 ──────────────────────────────────────────────

/// 상한이 없으면 루프에 빠진 에이전트가 셸을 무한히 뽑는다.
#[cfg(unix)]
#[tokio::test]
async fn opening_past_the_cap_fails_with_too_many_ptys() {
    let term = connect(PtyTerminal::default()).await;
    for _ in 0..8 {
        term.open(Some("/bin/sh".into()), 80, 24).await.unwrap();
    }
    let err = term.open(Some("/bin/sh".into()), 80, 24).await.unwrap_err();
    assert!(format!("{err:?}").contains("too_many_ptys"), "본 오류: {err:?}");
}

/// 유휴 타임아웃이 세션을 닫는다. 벽시계 대신 주입한 값으로 잰다 —
/// 10분을 기다리는 테스트는 테스트가 아니다.
#[cfg(unix)]
#[tokio::test]
async fn an_idle_session_is_reaped() {
    let term = connect(PtyTerminal::default().with_idle_timeout(Duration::from_millis(300))).await;
    let opened = term.open(Some("/bin/sh".into()), 80, 24).await.unwrap();

    tokio::time::sleep(Duration::from_millis(1500)).await;

    let err = term
        .read(opened.pty, None, Some(Settle { quiet_ms: 0, timeout_ms: 500 }))
        .await
        .unwrap_err();
    assert!(format!("{err:?}").contains("pty_gone"), "본 오류: {err:?}");
}

/// 계속 만지는 세션은 살아 있어야 한다 — 스위퍼가 `last_touch`를 진짜로 보는지.
#[cfg(unix)]
#[tokio::test]
async fn a_touched_session_is_not_reaped() {
    let term = connect(PtyTerminal::default().with_idle_timeout(Duration::from_millis(400))).await;
    let opened = term.open(Some("/bin/sh".into()), 80, 24).await.unwrap();

    for _ in 0..6 {
        tokio::time::sleep(Duration::from_millis(150)).await;
        term.read(opened.pty.clone(), None, Some(Settle { quiet_ms: 0, timeout_ms: 500 }))
            .await
            .expect("만지고 있는 세션이 수거됐다");
    }
}

/// 진짜 셸의 출력에 ESC가 한 바이트도 남아선 안 된다.
///
/// 라이브 실측(2026-07-31)에서 Attacca의 안전 가드가 U+001B가 든 tool 출력을 통째로
/// 거부했다 — `"tool output contains disallowed control character U+001B"`. 셸 프롬프트의
/// bracketed-paste 시퀀스 하나로 걸리므로, 이게 새면 `read`는 실제 사용에서 거의 항상
/// 차단된다. 단위 테스트는 합성 입력만 보므로 진짜 프롬프트로 한 번 더 못 박는다.
#[cfg(unix)]
#[tokio::test]
async fn a_real_shell_read_carries_no_escape_bytes() {
    let term = connect(PtyTerminal::default()).await;
    let opened = term.open(Some("/bin/sh".into()), 80, 24).await.unwrap();
    let pty = opened.pty;

    // 프롬프트 + 색 + 커서 이동을 한꺼번에 낸다.
    let out: PtyRead = term
        .read(
            pty,
            Some("printf '\\033[31mRED\\033[0m\\033[2;3HAT\\n'\n".into()),
            Some(Settle { quiet_ms: 200, timeout_ms: 5000 }),
        )
        .await
        .unwrap();

    assert!(!out.content.contains('\u{1b}'), "ESC가 남았다: {:?}", out.content);
    assert!(out.content.contains("RED"), "본문이 사라졌다: {:?}", out.content);
    for c in out.content.chars() {
        assert!(
            c >= ' ' || c == '\n' || c == '\t',
            "제어 문자 {:?}가 남았다: {:?}",
            c,
            out.content
        );
    }
}
