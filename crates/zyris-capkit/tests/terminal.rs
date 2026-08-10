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
        Blob::Attachment(_) => panic!("an attachment should never show up here"),
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

/// The point of argv mode: with no shell in the way, spaces, quotes, `$`, backticks, and `*` all
/// reach the program exactly as given. If a shell had been involved, `$HOME` would have been
/// expanded, backticks executed, and `*` globbed.
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

/// The same shape as PowerShell's `-Command -` pattern — feeding a script through stdin needs no
/// quoting. This verifies the same path on Unix, via `/bin/sh -s`.
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
    assert!(out.stdout.contains("FROM-STDIN"), "got: {:?}", out.stdout);
    assert!(out.stdout.contains("x=a b"), "got: {:?}", out.stdout);
}

/// An `env` entry reaches the child by overriding the node's environment.
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

/// Output is cut off at the cap per stream, and overflow bytes are dropped (`stdout_truncated`).
/// The child still runs to completion, so the exit code comes back normal.
#[cfg(unix)]
#[tokio::test]
async fn exec_caps_output_and_reports_truncation() {
    let term = connect(PtyTerminal::default()).await;

    // stdout that exceeds EXEC_OUTPUT_CAP (1 MiB).
    let out = term
        .exec(Some("yes x | head -c 1500000".into()), None, None, Some(20_000), None, None, None)
        .await
        .unwrap();

    assert_eq!(out.exit_code, 0);
    assert_eq!(out.stdout.len(), 1024 * 1024, "returned more than the cap");
    assert!(out.stdout_truncated, "exceeded the cap but truncated is not set");
    assert!(!out.stderr_truncated);
}

/// On timeout, the **entire process tree** is killed and the call returns with the partial output.
///
/// The previous implementation just dropped the child, leaving `sleep 30` orphaned. Here the
/// shell is made to write its own pid to a file first, and after the timeout it's checked whether
/// that pid died (ESRCH).
#[cfg(unix)]
#[tokio::test]
async fn exec_timeout_kills_the_process_tree() {
    let term = connect(PtyTerminal::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let cmd = format!("echo $$ > {}; sleep 30", pidfile.display());

    let out = term.exec(Some(cmd), None, None, Some(1500), None, None, None).await.unwrap();

    assert!(out.timed_out, "got: {out:?}");
    assert_eq!(out.exit_code, -1);

    let pid: i32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
    // SAFETY: signal 0 only checks — 0 if the process is alive, -1 (ESRCH) if not.
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    assert!(!alive, "process group {pid} is still alive after the timeout kill");
}

/// Giving both `command` and `argv` is ambiguous, so it is rejected.
#[cfg(unix)]
#[tokio::test]
async fn exec_rejects_both_command_and_argv() {
    let term = connect(PtyTerminal::default()).await;

    let err = term
        .exec(Some("echo hi".into()), Some(vec!["echo".into()]), None, Some(5000), None, None, None)
        .await
        .unwrap_err();
    assert!(format!("{err:?}").contains("not both"), "got error: {err:?}");
}

/// If both are missing, there is nothing to run.
#[cfg(unix)]
#[tokio::test]
async fn exec_rejects_neither_command_nor_argv() {
    let term = connect(PtyTerminal::default()).await;

    let err = term.exec(None, None, None, Some(5000), None, None, None).await.unwrap_err();
    assert!(format!("{err:?}").contains("not neither"), "got error: {err:?}");
}

// ── the reader thread is not held hostage by a consumer ─────────────────

/// The PTY has to keep running even while nobody is draining its stream.
///
/// The check is done through the filesystem, **never touching the stream itself**. If the reader
/// were blocked, the shell would block right along with it while writing to the PTY, and would
/// never reach the final `touch`. Judging by reading the stream instead would let a blocked
/// reader get freed the moment it's consumed, which would fail to distinguish anything.
#[cfg(unix)]
#[tokio::test]
async fn the_reader_never_blocks_without_a_consumer() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("done");

    let term = connect(PtyTerminal::default()).await;
    let _stream = term.open_stream(Some("/bin/sh".into()), 80, 24).await.unwrap();
    let pty = _stream.head.pty.clone();

    // Produce output on the PTY that far exceeds mpsc(64) x 8 KiB ~= 512 KiB.
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
    panic!("the PTY stalled with no consumer draining it — the shell never reached its last command");
}

/// Once the shell exits, two things must hold at the same time: the stream **ends**, and the
/// handle **survives**.
///
/// v1 managed neither. The session kept holding onto `pair.slave`, so the master never saw EOF
/// and the reader thread would be stuck in `read` forever; and even if EOF did arrive, the reader
/// would erase the session from the map. Erasing it would mean the agent never gets to see the
/// last output the shell produced while dying (spec §4).
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
            Ok(Some(Err(e))) => panic!("stream error: {e}"),
            Ok(None) => break,
            Err(_) => panic!("the shell exited but the stream never ended — got: {seen:?}"),
        }
    }
    assert!(seen.contains("LAST-WORD"), "missed the last output: {seen:?}");

    // The handle stays valid even after the stream ends — this should get a normal response, not `pty_gone`.
    term.resize(pty.clone(), 100, 30).await.expect("the session should survive the shell exiting");
    term.close(pty).await.unwrap();
}

// ── unary path: open / read / screen ─────────────────────────────────────

/// A short settle. Tests always inject a value so they are never left hanging on the wall clock.
fn quick() -> Option<Settle> {
    Some(Settle { quiet_ms: 60, timeout_ms: 3000 })
}

async fn open_sh(term: &TerminalClient) -> PtyId {
    let opened = term.open(Some("/bin/sh".into()), 80, 24).await.unwrap();
    // Drain the prompt the shell produces on startup. Cutting this off too eagerly would leak
    // those bytes into the next test's `content`.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _ = term
        .read(opened.pty.clone(), None, Some(Settle { quiet_ms: 150, timeout_ms: 3000 }))
        .await
        .unwrap();
    opened.pty
}

/// Something `exec` cannot do — state carries over across calls.
#[cfg(unix)]
#[tokio::test]
async fn state_carries_across_calls() {
    let term = connect(PtyTerminal::default()).await;
    let pty = open_sh(&term).await;

    term.read(pty.clone(), Some("cd /tmp\n".into()), quick()).await.unwrap();
    let out: PtyRead = term.read(pty.clone(), Some("pwd\n".into()), quick()).await.unwrap();

    assert!(out.content.contains("/tmp"), "got: {:?}", out.content);
    assert_eq!(out.exited, None);
    assert_eq!(out.dropped, 0);
}

/// `quiet_ms == 0` does not wait at all.
#[cfg(unix)]
#[tokio::test]
async fn quiet_zero_returns_immediately() {
    let term = connect(PtyTerminal::default()).await;
    let pty = open_sh(&term).await;

    let t0 = std::time::Instant::now();
    term.read(pty, None, Some(Settle { quiet_ms: 0, timeout_ms: 10_000 })).await.unwrap();
    assert!(t0.elapsed() < Duration::from_millis(500), "elapsed: {:?}", t0.elapsed());
}

/// The **quiet** side of settle's two return paths. If output finishes quickly, `timeout_ms` is
/// never waited out.
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

    assert!(out.content.contains("FAST"), "got: {:?}", out.content);
    assert!(t0.elapsed() < Duration::from_secs(3), "held until timeout: {:?}", t0.elapsed());
}

/// A multibyte character stays intact even after passing through the PTY. If it were split across
/// a chunk boundary, a replacement character would leak in. The boundary cannot actually be forced
/// here (the kernel decides where chunks split), so this only checks **round-trip integrity**; the
/// boundary rule itself is covered deterministically by the `trim_incomplete_tail` unit tests in
/// `buffer.rs`.
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

    assert!(out.content.contains("가나다라🎯"), "got: {:?}", out.content);
    assert!(!out.content.contains('\u{FFFD}'), "a replacement character leaked in: {:?}", out.content);
}

/// Input that produces no output must not return an empty response **early** (spec §3.4). An
/// early return would make the agent misread it as "the command finished."
///
/// **Echo has to be off for this gate to actually engage.** In cooked mode the terminal echoes
/// typed bytes back immediately, so "there is something to read" is satisfied right away and the
/// gate opens. Where the gate earns its keep is raw mode with no echo — i.e. exactly the case of
/// feeding keys to a full-screen program like vim or htop and waiting for the screen to react.
#[cfg(unix)]
#[tokio::test]
async fn an_input_with_no_output_waits_for_the_deadline() {
    let term = connect(PtyTerminal::default()).await;
    let pty = open_sh(&term).await;

    // Turn off echo and put a non-readline consumer in the foreground. bash's readline resets the
    // terminal mode every time it draws a prompt, so `stty -echo` alone would not stick.
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

    // `cat` swallows it into /dev/null, so the PTY is completely quiet. Without the gate, an empty response would go out at 50ms.
    assert!(t0.elapsed() >= Duration::from_millis(550), "elapsed: {:?}", t0.elapsed());
    assert!(out.content.is_empty(), "echo was off but something came through: {:?}", out.content);
}

/// Conversely, a pure observation with no `input` is never held — if every poll ate `timeout_ms`, it would be unusable.
#[cfg(unix)]
#[tokio::test]
async fn a_pure_observation_does_not_wait_for_the_deadline() {
    let term = connect(PtyTerminal::default()).await;
    let pty = open_sh(&term).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let t0 = std::time::Instant::now();
    term.read(pty, None, Some(Settle { quiet_ms: 50, timeout_ms: 5000 })).await.unwrap();
    assert!(t0.elapsed() < Duration::from_millis(1000), "elapsed: {:?}", t0.elapsed());
}

/// When it doesn't all fit in one call, `more: true` reports that, and calling again continues **in order**.
#[cfg(unix)]
#[tokio::test]
async fn more_paginates_in_order() {
    let term = connect(PtyTerminal::default()).await;
    let pty = open_sh(&term).await;

    // An amount that exceeds PTY_READ_MAX (128 KiB) but still fits within RING_MAX (1 MiB).
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
        assert_eq!(out.dropped, 0, "should fit within the 1 MiB ring but was lost");
        all.push_str(&out.content);
        saw_more |= out.more;
        if !out.more {
            break;
        }
    }
    assert!(saw_more, "exceeded 128 KiB but `more` never came back true");
    let a = all.find("39998").expect("39998 is missing");
    let b = all.find("39999").expect("39999 is missing");
    assert!(a < b, "the order got reversed");
}

/// Even after the shell exits, the last output and the exit code both come through (spec §4).
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

    assert!(out.content.contains("LAST-WORD"), "got: {:?}", out.content);
    assert_eq!(out.exited, Some(7));
}

/// vt100 control characters render into a grid.
#[cfg(unix)]
#[tokio::test]
async fn screen_renders_cursor_addressing() {
    let term = connect(PtyTerminal::default()).await;
    let pty = open_sh(&term).await;

    // Clear the screen and write starting at row 3, column 5.
    let s: PtyScreen = term
        .screen(
            pty,
            Some("printf '\\033[2J\\033[3;5HMARKER'\n".into()),
            Some(Settle { quiet_ms: 150, timeout_ms: 5000 }),
        )
        .await
        .unwrap();

    assert_eq!(s.lines.len(), 24, "the line count must not differ from rows");
    assert!(s.lines[2].starts_with("    MARKER"), "row 3 was not as expected: {:?}", s.lines[2]);
}

/// `screen` does not advance the read cursor — the screen is a render of accumulated state, so
/// there is no notion of consuming it. Getting this backwards would mean a single `screen` call
/// wipes out an entire build log.
#[cfg(unix)]
#[tokio::test]
async fn screen_does_not_advance_the_read_cursor() {
    let term = connect(PtyTerminal::default()).await;
    let pty = open_sh(&term).await;

    term.read(pty.clone(), Some("echo NEEDLE-IN-BUFFER\n".into()), quick()).await.unwrap();
    // The read above already took it, so produce one more piece of output.
    term.write(pty.clone(), blob("echo SECOND-NEEDLE\n")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;

    term.screen(pty.clone(), None, Some(Settle { quiet_ms: 0, timeout_ms: 1000 })).await.unwrap();

    let out: PtyRead =
        term.read(pty, None, Some(Settle { quiet_ms: 0, timeout_ms: 1000 })).await.unwrap();
    assert!(out.content.contains("SECOND-NEEDLE"), "screen consumed the cursor: {:?}", out.content);
}

// ── lifetime: cap and idle reaping ────────────────────────────────────────

/// Without a cap, an agent stuck in a loop would spawn shells without limit.
#[cfg(unix)]
#[tokio::test]
async fn opening_past_the_cap_fails_with_too_many_ptys() {
    let term = connect(PtyTerminal::default()).await;
    for _ in 0..8 {
        term.open(Some("/bin/sh".into()), 80, 24).await.unwrap();
    }
    let err = term.open(Some("/bin/sh".into()), 80, 24).await.unwrap_err();
    assert!(format!("{err:?}").contains("too_many_ptys"), "got error: {err:?}");
}

/// The idle timeout closes a session. Measured against an injected value instead of the wall
/// clock — a test that waits 10 minutes is not a test.
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
    assert!(format!("{err:?}").contains("pty_gone"), "got error: {err:?}");
}

/// A session that keeps getting touched must stay alive — checks that the sweeper genuinely looks at `last_touch`.
#[cfg(unix)]
#[tokio::test]
async fn a_touched_session_is_not_reaped() {
    let term = connect(PtyTerminal::default().with_idle_timeout(Duration::from_millis(400))).await;
    let opened = term.open(Some("/bin/sh".into()), 80, 24).await.unwrap();

    for _ in 0..6 {
        tokio::time::sleep(Duration::from_millis(150)).await;
        term.read(opened.pty.clone(), None, Some(Settle { quiet_ms: 0, timeout_ms: 500 }))
            .await
            .expect("a session that is actively being touched was reaped anyway");
    }
}

/// Not a single ESC byte may survive in a real shell's output.
///
/// A live measurement (2026-07-31) found Attacca's safety guard outright rejecting tool output
/// that contains U+001B — `"tool output contains disallowed control character U+001B"`. A single
/// bracketed-paste sequence from a shell prompt is enough to trigger it, so if this leaks, `read`
/// is blocked in real use almost every time. Unit tests only ever see synthetic input, so this
/// nails it down once more against a real prompt.
#[cfg(unix)]
#[tokio::test]
async fn a_real_shell_read_carries_no_escape_bytes() {
    let term = connect(PtyTerminal::default()).await;
    let opened = term.open(Some("/bin/sh".into()), 80, 24).await.unwrap();
    let pty = opened.pty;

    // Produce a prompt plus color plus cursor movement all at once.
    let out: PtyRead = term
        .read(
            pty,
            Some("printf '\\033[31mRED\\033[0m\\033[2;3HAT\\n'\n".into()),
            Some(Settle { quiet_ms: 200, timeout_ms: 5000 }),
        )
        .await
        .unwrap();

    assert!(!out.content.contains('\u{1b}'), "ESC survived: {:?}", out.content);
    assert!(out.content.contains("RED"), "the body content is missing: {:?}", out.content);
    for c in out.content.chars() {
        assert!(
            c >= ' ' || c == '\n' || c == '\t',
            "control character {:?} survived: {:?}",
            c,
            out.content
        );
    }
}
