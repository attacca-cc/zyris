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

/// Mimics a caller that never declares a stream (which is exactly what Attacca's agent
/// tool-caller does): `call_raw` invoked without `req.stream`.
///
/// `read` is unary, so its content comes back riding along on the response, while `read_stream`
/// is rejected per the protocol (§4 "Caller declares the stream"). This split is the entire
/// reason this capability was broken into two in v2 — leaving it as one would mean a caller that
/// cannot handle streams could never read a file at all.
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
    // Wait for the capability announcement to arrive — that rules out the result being an artifact of it not being announced yet.
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

/// A file past the cap does not error — it comes back as the leading portion plus `truncated`,
/// and continuing to read by feeding back `offset` reconciles byte-for-byte with the original.
/// This is the only path that keeps an agent from getting stuck on a large file.
#[tokio::test]
async fn a_large_file_truncates_and_pages_back_to_the_original() {
    const READ_UNARY_MAX: u64 = 128 * 1024;

    let dir = tempfile::tempdir().unwrap();
    // More than one chunk past the cap — ASCII, so byte count and character count match and the boundary math stays unambiguous.
    let body: String = std::iter::repeat("zyris ").take(30_000).collect();
    assert!(body.len() as u64 > READ_UNARY_MAX);
    std::fs::write(dir.path().join("big.txt"), &body).unwrap();

    let fs = file_io_rooted_at(dir.path()).await;

    let first = fs.read("big.txt".into(), None, None).await.unwrap();
    assert!(first.truncated, "exceeded the cap but truncated is not set");
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

/// `len` only narrows a read and can never raise the cap, and a call that requests exactly up to
/// the file's end reports itself as not truncated — `truncated` is a fact about the file, not
/// about `len`.
#[tokio::test]
async fn len_narrows_the_read_and_the_tail_is_not_truncated() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("hello.txt"), b"hello zyris").unwrap();
    let fs = file_io_rooted_at(dir.path()).await;

    let head = fs.read("hello.txt".into(), None, Some(5)).await.unwrap();
    assert_eq!(head.content, "hello");
    assert_eq!(head.len, 5);
    assert!(head.truncated, "bytes remain after this but truncated is not set");

    let tail = fs.read("hello.txt".into(), Some(6), None).await.unwrap();
    assert_eq!(tail.content, "zyris");
    assert_eq!((tail.offset, tail.len), (6, 5));
    assert!(!tail.truncated, "read to the end of the file but truncated is set");
}

/// Directories cannot be read. The old streaming `read` failed to open here too, but only raised
/// the error after the stream had already started; the unary path cuts it off right at the call
/// with invalid_params.
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

/// `edit` replaces an exact substring and returns the number of replacements plus the new stat.
#[tokio::test]
async fn edit_replaces_the_first_occurrence() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "one two three").unwrap();
    let fs = file_io_rooted_at(dir.path()).await;

    let edited = fs.edit("f.txt".into(), "two".into(), "2".into(), false).await.unwrap();
    assert_eq!(edited.replaced, 1);
    assert_eq!(std::fs::read_to_string(dir.path().join("f.txt")).unwrap(), "one 2 three");
}

/// An `old_string` that occurs more than once is rejected without `replace_all` — the same rule as `computer_file_edit`.
#[tokio::test]
async fn edit_requires_replace_all_for_multiple_occurrences() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "one one one").unwrap();
    let fs = file_io_rooted_at(dir.path()).await;

    let err = fs.edit("f.txt".into(), "one".into(), "1".into(), false).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidParams);
    assert!(err.message.contains("replace_all"), "message: {}", err.message);

    let edited = fs.edit("f.txt".into(), "one".into(), "1".into(), true).await.unwrap();
    assert_eq!(edited.replaced, 3);
    assert_eq!(std::fs::read_to_string(dir.path().join("f.txt")).unwrap(), "1 1 1");
}

/// A string that is not found is an error, and so is an empty `old_string`.
#[tokio::test]
async fn edit_not_found_and_empty_needle_are_errors() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "hello").unwrap();
    let fs = file_io_rooted_at(dir.path()).await;

    let err = fs.edit("f.txt".into(), "zzz".into(), "x".into(), true).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidParams);
    assert!(err.message.contains("not found"), "message: {}", err.message);

    let err = fs.edit("f.txt".into(), "".into(), "x".into(), true).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidParams);
}

/// A binary file is not UTF-8 text, so editing it is rejected — to avoid mangling its content with replacement characters.
#[tokio::test]
async fn edit_rejects_binary_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("bin"), [0xffu8, 0x00, 0xfe, 0x41]).unwrap();
    let fs = file_io_rooted_at(dir.path()).await;

    let err = fs.edit("bin".into(), "A".into(), "B".into(), true).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidParams);
}

/// `recursive: true` deletes an entire directory tree — the same behavior as `computer_file_delete`.
#[tokio::test]
async fn remove_recursively_deletes_a_tree() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("tree/sub")).unwrap();
    std::fs::write(dir.path().join("tree/root.txt"), "r").unwrap();
    std::fs::write(dir.path().join("tree/sub/deep.txt"), "d").unwrap();
    let fs = file_io_rooted_at(dir.path()).await;

    // A non-empty directory is rejected without recursive.
    let err = fs.remove("tree".into(), None).await.unwrap_err();
    assert!(err.to_string().contains("not empty"), "got error: {err:?}");
    assert!(dir.path().join("tree").exists(), "a failed remove deleted the tree anyway");

    let ok = fs.remove("tree".into(), Some(true)).await;
    assert!(ok.is_ok(), "got error: {ok:?}");
    assert!(!dir.path().join("tree").exists());
}

/// A file is deleted regardless of `recursive`.
#[tokio::test]
async fn remove_deletes_a_plain_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "x").unwrap();
    let fs = file_io_rooted_at(dir.path()).await;

    fs.remove("f.txt".into(), None).await.unwrap();
    assert!(!dir.path().join("f.txt").exists());
}
