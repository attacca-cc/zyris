#![cfg(feature = "file-io")]

use std::time::Duration;

use futures_util::StreamExt;
use zyris::serde_json::json;
use zyris::{Datum, ErrorCode, Node, NodeKind, Payload};
use zyris_caps::{FileIo, FileIoClient, FileIoServer, FileRead};
use zyris_capkit::LocalFileIo;

async fn file_io_rooted_at(root: &std::path::Path) -> FileIoClient {
    let server = Node::builder()
        .name("fs-node")
        .kind(NodeKind::Service)
        .capability(FileIoServer(LocalFileIo::rooted(root)))
        .build()
        .unwrap();
    let client_node = Node::builder().name("client").kind(NodeKind::Cli).build().unwrap();
    let (conn, _server) = zyris::testing::duplex(&client_node, &server).await.unwrap();
    conn.wait_capability(Duration::from_secs(2)).await.unwrap()
}

async fn read_all(fs: &FileIoClient, path: &str) -> Vec<u8> {
    let mut streaming = fs.read_stream(path.into(), None, None).await.unwrap();
    let mut bytes = Vec::new();
    while let Some(chunk) = streaming.items.next().await {
        bytes.extend_from_slice(&chunk.unwrap().0);
    }
    bytes
}

#[tokio::test]
async fn write_read_list_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let fs = file_io_rooted_at(dir.path()).await;

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

    let mut streaming = fs.read_stream("notes/hello.txt".into(), None, None).await.unwrap();
    assert_eq!(streaming.head.size, 11);
    let mut bytes = Vec::new();
    while let Some(chunk) = streaming.items.next().await {
        bytes.extend_from_slice(&chunk.unwrap().0);
    }
    assert_eq!(bytes, b"hello zyris");

    let read = fs.read("notes/hello.txt".into(), None, None).await.unwrap();
    assert_eq!(read.content, "hello zyris");
    assert_eq!(read.stat.size, 11);
    assert_eq!((read.offset, read.len), (0, 11));
    assert!(!read.truncated);
}

/// 스트림을 선언하지 않는 호출자(Attacca의 에이전트 tool-caller가 그렇다)를 그대로 흉내낸다:
/// `req.stream` 없이 부르는 `call_raw`다.
///
/// `read`는 unary라 내용이 응답에 실려 돌아오고, `read_stream`은 프로토콜(§4 "Caller declares
/// the stream")대로 거부된다. 이 분기가 이 capability가 v2에서 둘로 갈라진 이유 전부다 —
/// 하나로 합쳐 두면 스트림을 못 다루는 호출자는 파일을 영영 읽지 못한다.
#[tokio::test]
async fn a_caller_without_streams_can_read_but_not_read_stream() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("hello.txt"), b"hello zyris").unwrap();

    let server = Node::builder()
        .name("fs-node")
        .kind(NodeKind::Service)
        .capability(FileIoServer(LocalFileIo::rooted(dir.path())))
        .build()
        .unwrap();
    let client_node = Node::builder().name("client").kind(NodeKind::Cli).build().unwrap();
    let (conn, _server) = zyris::testing::duplex(&client_node, &server).await.unwrap();
    // 공시가 도착할 때까지 기다린다 — 그래야 결과가 미공시 때문이 아님이 확실하다.
    let _: FileIoClient = conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let params = Payload::from_json(json!({ "path": "hello.txt" }));
    let read: FileRead = conn.call_raw("file_io.read", params.clone()).await.unwrap().to_typed().unwrap();
    assert_eq!(read.content, "hello zyris");
    assert!(!read.truncated);

    let err = conn.call_raw("file_io.read_stream", params).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidParams);
    assert!(
        err.message.contains("streaming tool requires a stream declaration"),
        "unexpected message: {}",
        err.message
    );
}

/// 상한을 넘는 파일은 오류가 아니라 앞부분 + `truncated`로 돌아오고, `offset`을 물려 이어
/// 읽으면 원본과 바이트 단위로 같아진다. 에이전트가 큰 파일에서 막히지 않는 유일한 경로다.
#[tokio::test]
async fn a_large_file_truncates_and_pages_back_to_the_original() {
    const READ_UNARY_MAX: u64 = 128 * 1024;

    let dir = tempfile::tempdir().unwrap();
    // 상한보다 한 청크 넘게 — ASCII라 바이트 수와 문자 수가 같아 경계 계산이 명확하다.
    let body: String = std::iter::repeat("zyris ").take(30_000).collect();
    assert!(body.len() as u64 > READ_UNARY_MAX);
    std::fs::write(dir.path().join("big.txt"), &body).unwrap();

    let fs = file_io_rooted_at(dir.path()).await;

    let first = fs.read("big.txt".into(), None, None).await.unwrap();
    assert!(first.truncated, "상한을 넘는데 truncated가 아니다");
    assert_eq!(first.len, READ_UNARY_MAX);
    assert_eq!(first.offset, 0);
    assert_eq!(first.stat.size, body.len() as u64);

    let mut seen = first.content.clone();
    let mut at = first.offset + first.len;
    let mut page = first;
    while page.truncated {
        page = fs.read("big.txt".into(), Some(at), None).await.unwrap();
        assert_eq!(page.offset, at);
        seen.push_str(&page.content);
        at += page.len;
    }
    assert_eq!(seen, body);
    assert_eq!(at, body.len() as u64);
}

/// `len`은 읽기를 좁힐 뿐 상한을 올리지 못하고, 파일의 끝까지를 정확히 요청한 호출은
/// 잘리지 않았다고 보고한다 — `truncated`는 `len`이 아니라 파일에 대한 사실이다.
#[tokio::test]
async fn len_narrows_the_read_and_the_tail_is_not_truncated() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("hello.txt"), b"hello zyris").unwrap();
    let fs = file_io_rooted_at(dir.path()).await;

    let head = fs.read("hello.txt".into(), None, Some(5)).await.unwrap();
    assert_eq!(head.content, "hello");
    assert_eq!(head.len, 5);
    assert!(head.truncated, "뒤에 바이트가 남았는데 truncated가 아니다");

    let tail = fs.read("hello.txt".into(), Some(6), None).await.unwrap();
    assert_eq!(tail.content, "zyris");
    assert_eq!((tail.offset, tail.len), (6, 5));
    assert!(!tail.truncated, "파일 끝까지 읽었는데 truncated다");
}

/// 디렉터리는 읽히지 않는다. 예전 스트리밍 `read`는 여기서 열기에 실패해 스트림이 시작된 뒤
/// 오류를 냈지만, unary 경로는 호출 그 자리에서 invalid_params로 끊는다.
#[tokio::test]
async fn reading_a_directory_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    let fs = file_io_rooted_at(dir.path()).await;

    let err = fs.read("sub".into(), None, None).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidParams);
}

#[tokio::test]
async fn resolves_absolute_and_parent() {
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
async fn writes_to_an_absolute_path() {
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

// ── v3: edit / recursive remove ─────────────────────────────────────────

/// `edit`은 정확한 부분 문자열을 바꾸고, 바꾼 횟수와 새 stat을 돌려준다.
#[tokio::test]
async fn edit_replaces_the_first_occurrence() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "one two three").unwrap();
    let fs = file_io_rooted_at(dir.path()).await;

    let edited = fs.edit("f.txt".into(), "two".into(), "2".into(), false).await.unwrap();
    assert_eq!(edited.replaced, 1);
    assert_eq!(std::fs::read_to_string(dir.path().join("f.txt")).unwrap(), "one 2 three");
}

/// 여러 번 나오는 `old_string`은 `replace_all` 없이는 거부한다 — 컴퓨터_file_edit과 같은 규칙.
#[tokio::test]
async fn edit_requires_replace_all_for_multiple_occurrences() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "one one one").unwrap();
    let fs = file_io_rooted_at(dir.path()).await;

    let err = fs.edit("f.txt".into(), "one".into(), "1".into(), false).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidParams);
    assert!(err.message.contains("replace_all"), "본 메시지: {}", err.message);

    let edited = fs.edit("f.txt".into(), "one".into(), "1".into(), true).await.unwrap();
    assert_eq!(edited.replaced, 3);
    assert_eq!(std::fs::read_to_string(dir.path().join("f.txt")).unwrap(), "1 1 1");
}

/// 없는 문자열은 오류, 빈 `old_string`도 오류.
#[tokio::test]
async fn edit_not_found_and_empty_needle_are_errors() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "hello").unwrap();
    let fs = file_io_rooted_at(dir.path()).await;

    let err = fs.edit("f.txt".into(), "zzz".into(), "x".into(), true).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidParams);
    assert!(err.message.contains("not found"), "본 메시지: {}", err.message);

    let err = fs.edit("f.txt".into(), "".into(), "x".into(), true).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidParams);
}

/// 바이너리 파일은 UTF-8 텍스트가 아니므로 편집을 거부한다 — 대체 문자로 내용을 망치지 않도록.
#[tokio::test]
async fn edit_rejects_binary_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("bin"), [0xffu8, 0x00, 0xfe, 0x41]).unwrap();
    let fs = file_io_rooted_at(dir.path()).await;

    let err = fs.edit("bin".into(), "A".into(), "B".into(), true).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidParams);
}

/// `recursive: true`는 디렉터리 트리 전체를 지운다 — `computer_file_delete`와 같은 동작.
#[tokio::test]
async fn remove_recursively_deletes_a_tree() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("tree/sub")).unwrap();
    std::fs::write(dir.path().join("tree/root.txt"), "r").unwrap();
    std::fs::write(dir.path().join("tree/sub/deep.txt"), "d").unwrap();
    let fs = file_io_rooted_at(dir.path()).await;

    // 비어 있지 않은 디렉터리는 recursive 없이 거부된다.
    let err = fs.remove("tree".into(), None).await.unwrap_err();
    assert!(err.to_string().contains("not empty"), "본 오류: {err:?}");
    assert!(dir.path().join("tree").exists(), "실패한 remove가 트리를 지웠다");

    let ok = fs.remove("tree".into(), Some(true)).await;
    assert!(ok.is_ok(), "본 오류: {ok:?}");
    assert!(!dir.path().join("tree").exists());
}

/// 파일은 `recursive`와 무관하게 지워진다.
#[tokio::test]
async fn remove_deletes_a_plain_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "x").unwrap();
    let fs = file_io_rooted_at(dir.path()).await;

    fs.remove("f.txt".into(), None).await.unwrap();
    assert!(!dir.path().join("f.txt").exists());
}
