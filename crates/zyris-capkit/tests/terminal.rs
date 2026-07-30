#![cfg(feature = "terminal")]

use std::time::Duration;

use zyris::{Blob, Node, NodeKind};
use zyris_caps::{ExecOutput, Terminal, TerminalClient, TerminalServer};
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
    let _stream = term.open(Some("/bin/sh".into()), 80, 24).await.unwrap();
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
    let mut stream = term.open(Some("/bin/sh".into()), 80, 24).await.unwrap();
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
