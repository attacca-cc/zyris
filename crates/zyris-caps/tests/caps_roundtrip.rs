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

async fn file_io_rooted_at(root: &std::path::Path) -> FileIoClient {
    let server = Node::builder()
        .name("fs-node")
        .kind(NodeKind::Service)
        .capability(FileIoServer(LocalFileIo::rooted(root)))
        .build()
        .unwrap();
    let conn = connect(server).await;
    conn.wait_capability(Duration::from_secs(2)).await.unwrap()
}

async fn read_all(fs: &FileIoClient, path: &str) -> Vec<u8> {
    let mut streaming = fs.read(path.into(), None, None).await.unwrap();
    let mut bytes = Vec::new();
    while let Some(chunk) = streaming.items.next().await {
        bytes.extend_from_slice(&chunk.unwrap().0);
    }
    bytes
}

#[tokio::test]
async fn file_io_resolves_absolute_and_parent() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("outside.txt"), b"outside the root").unwrap();

    let fs = file_io_rooted_at(root.path()).await;

    let absolute = outside.path().join("outside.txt").to_string_lossy().to_string();
    let stat = fs.stat(absolute.clone()).await.unwrap();
    assert_eq!(stat.path, absolute);
    assert_eq!(stat.size, 16);
    assert_eq!(read_all(&fs, &absolute).await, b"outside the root");

    let sibling = outside.path().file_name().unwrap().to_string_lossy().to_string();
    let via_parent = format!("../{sibling}/outside.txt");
    assert_eq!(read_all(&fs, &via_parent).await, b"outside the root");
}

#[tokio::test]
async fn file_io_writes_to_an_absolute_path() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let fs = file_io_rooted_at(root.path()).await;

    let target = outside.path().join("nested/out.txt");
    let absolute = target.to_string_lossy().to_string();
    let stat = fs
        .write(
            absolute.clone(),
            Datum::Text { text: "written absolutely".into(), format: None },
            true,
        )
        .await
        .unwrap();

    assert_eq!(stat.path, absolute);
    assert_eq!(std::fs::read(&target).unwrap(), b"written absolutely");
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

#[cfg(unix)]
#[tokio::test]
async fn terminal_exec_resolves_cwd() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    std::fs::create_dir(root_path.join("sub")).unwrap();

    let server = Node::builder()
        .name("term-node")
        .kind(NodeKind::Service)
        .capability(TerminalServer(PtyTerminal::rooted(&root_path)))
        .build()
        .unwrap();
    let conn = connect(server).await;
    let term: TerminalClient = conn.wait_capability(Duration::from_secs(2)).await.unwrap();

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
