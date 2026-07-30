#![cfg(feature = "terminal")]

use std::time::Duration;

use zyris::{Node, NodeKind};
use zyris_caps::{ExecOutput, Terminal, TerminalClient, TerminalServer};
use zyris_capkit::PtyTerminal;

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
