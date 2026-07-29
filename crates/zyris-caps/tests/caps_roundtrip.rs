use std::time::Duration;

use futures_util::StreamExt;
use zyris::{Datum, Node, NodeKind};
use zyris_caps::{
    ExecOutput, FileIo, FileIoClient, FileIoServer, LocalFileIo, PtyTerminal, Terminal,
    TerminalClient, TerminalServer,
};

async fn connect(server: Node) -> zyris::Connection {
    let client_node = Node::builder().name("client").kind(NodeKind::Cli).build().unwrap();
    let (client, _server) = zyris::testing::duplex(&client_node, &server).await.unwrap();
    client
}

#[tokio::test]
async fn file_io_write_read_list_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let server = Node::builder()
        .name("fs-node")
        .kind(NodeKind::Service)
        .capability(FileIoServer(LocalFileIo::rooted(dir.path())))
        .build()
        .unwrap();
    let conn = connect(server).await;
    let fs: FileIoClient = conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let stat = fs
        .write(
            "notes/hello.txt".into(),
            Datum::Text { text: "hello zyris".into(), format: None },
            true,
        )
        .await
        .unwrap();
    assert_eq!(stat.size, 11);

    let entries = fs.list("notes".into()).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "hello.txt");

    let mut streaming = fs.read("notes/hello.txt".into(), None, None).await.unwrap();
    assert_eq!(streaming.head.size, 11);
    let mut bytes = Vec::new();
    while let Some(chunk) = streaming.items.next().await {
        bytes.extend_from_slice(&chunk.unwrap().0);
    }
    assert_eq!(bytes, b"hello zyris");
}

#[tokio::test]
async fn file_io_rejects_path_escape() {
    let dir = tempfile::tempdir().unwrap();
    let server = Node::builder()
        .name("fs-node")
        .kind(NodeKind::Service)
        .capability(FileIoServer(LocalFileIo::rooted(dir.path())))
        .build()
        .unwrap();
    let conn = connect(server).await;
    let fs: FileIoClient = conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let err = fs.stat("../escape".into()).await.unwrap_err();
    assert_eq!(err.code, zyris::ErrorCode::ForbiddenScope);
}

#[tokio::test]
async fn terminal_exec_runs_a_command() {
    let server = Node::builder()
        .name("term-node")
        .kind(NodeKind::Service)
        .capability(TerminalServer(PtyTerminal::default()))
        .build()
        .unwrap();
    let conn = connect(server).await;
    let term: TerminalClient = conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let out: ExecOutput = term.exec("echo hello-zyris".into(), None, Some(5000)).await.unwrap();
    assert_eq!(out.exit_code, 0);
    assert!(out.stdout.contains("hello-zyris"));
    assert!(!out.timed_out);
}
