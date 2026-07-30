//! A large blob leaves the payload, travels as its own credited stream, and arrives whole.
//!
//! Every test here drives a real connection through `zyris::testing::duplex`, so what is being
//! checked is the wire — the response frame's shape, the chunks that follow it, the credit that
//! paces them — and not a helper's return value.

use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zyris::{
    AcceptOptions, Blob, Datum, Node, NodeKind, Payload, INLINE_BLOB_MAX,
};
use zyris_proto::{attach, Limits, FEATURE_CANCEL};

/// Comfortably over `INLINE_BLOB_MAX`, and not a multiple of any chunk size in play, so a chunking
/// bug shows up as a wrong length rather than a lucky pass.
const BIG: usize = INLINE_BLOB_MAX + 9_999;

#[derive(Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Shot {
    pub taken_at: String,
    pub image: Datum,
}

#[zyris::capability(name = "camera", version = 1)]
pub trait Camera {
    /// Hand back an image of `size` bytes, bare.
    async fn grab(&self, size: u64) -> zyris::Result<Datum>;

    /// The same image, wrapped in a struct — a datum the connection has to find by walking.
    async fn grab_wrapped(&self, size: u64) -> zyris::Result<Shot>;
}

struct FakeCamera;

/// Deterministic and non-uniform, so a chunk delivered out of order or dropped changes the digest.
fn pattern(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
}

fn image_of(size: u64) -> Datum {
    Datum::Image {
        name: "shot.png".into(),
        description: Some("display 0".into()),
        media_type: "image/png".into(),
        blob: Blob::from_bytes(pattern(size as usize)),
    }
}

#[zyris::async_trait]
impl Camera for FakeCamera {
    async fn grab(&self, size: u64) -> zyris::Result<Datum> {
        Ok(image_of(size))
    }

    async fn grab_wrapped(&self, size: u64) -> zyris::Result<Shot> {
        Ok(Shot { taken_at: "now".into(), image: image_of(size) })
    }
}

fn camera_node() -> Node {
    Node::builder()
        .name("cam")
        .kind(NodeKind::Desktop)
        .capability(CameraServer(FakeCamera))
        .build()
        .unwrap()
}

async fn connect_with(options: AcceptOptions) -> (zyris::Connection, zyris::Connection) {
    let dialer = Node::builder().name("consumer").kind(NodeKind::Cli).build().unwrap();
    zyris::testing::duplex_with(&dialer, &camera_node(), options).await.unwrap()
}

async fn connect() -> (zyris::Connection, zyris::Connection) {
    connect_with(AcceptOptions::default()).await
}

async fn camera(conn: &zyris::Connection) -> CameraClient {
    conn.wait_capability(Duration::from_secs(2)).await.unwrap()
}

fn blob_of(datum: &Datum) -> &Blob {
    match datum {
        Datum::Image { blob, .. } | Datum::File { blob, .. } => blob,
        Datum::Text { .. } => panic!("expected a blob-bearing datum"),
    }
}

/// The raw response is what the peer actually put on the wire: a reference, not bytes. Reading it
/// through `call_raw` rather than the generated client is the only way to see that, since the
/// client hydrates before its caller ever holds the value.
#[tokio::test]
async fn a_large_blob_travels_as_a_reference_and_arrives_whole() {
    let (client, _server) = connect().await;
    camera(&client).await;

    let mut raw = client
        .call_raw("camera.grab", Payload::from_typed(&serde_json::json!({ "size": BIG })).unwrap())
        .await
        .unwrap();

    let references = attach::refs(&raw.0);
    assert_eq!(references.len(), 1, "the response should reference one attachment");
    assert_eq!(references[0].size, BIG as u64);
    assert_eq!(references[0].offset, 0);
    assert_eq!(references[0].sha256.as_deref(), Some(attach::digest(&pattern(BIG)).as_str()));

    let carried = raw.to_typed::<Datum>().unwrap();
    assert!(blob_of(&carried).as_inline().is_none(), "the bytes must not ride in the payload");

    client.hydrate(&mut raw, 8 * 1024 * 1024).await.unwrap();
    assert_eq!(raw.to_typed::<Datum>().unwrap(), image_of(BIG as u64));
}

/// The whole mechanism is meant to be invisible from a generated client: the same `Datum` comes
/// back either way, so no capability has to be written twice.
#[tokio::test]
async fn the_generated_client_hydrates_without_being_asked() {
    let (client, _server) = connect().await;
    let cam = camera(&client).await;

    let small = cam.grab(64).await.unwrap();
    assert_eq!(small, image_of(64));

    let big = cam.grab(BIG as u64).await.unwrap();
    assert_eq!(big, image_of(BIG as u64));
    assert_eq!(blob_of(&big).as_inline().unwrap().len(), BIG);
}

/// A datum nested in a tool's own response type is found by the same walk. This is what makes the
/// mechanism work for capabilities the connection has never heard of.
#[tokio::test]
async fn a_nested_datum_is_detached_too() {
    let (client, _server) = connect().await;
    let cam = camera(&client).await;

    let shot = cam.grab_wrapped(BIG as u64).await.unwrap();
    assert_eq!(shot.taken_at, "now");
    assert_eq!(shot.image, image_of(BIG as u64));
}

/// Under the ceiling nothing changes: no stream, no round trip, bytes in the response frame.
#[tokio::test]
async fn a_small_blob_still_rides_inline() {
    let (client, _server) = connect().await;
    camera(&client).await;

    let raw = client
        .call_raw("camera.grab", Payload::from_typed(&serde_json::json!({ "size": 1024 })).unwrap())
        .await
        .unwrap();

    assert!(attach::refs(&raw.0).is_empty());
    let datum = raw.to_typed::<Datum>().unwrap();
    assert_eq!(blob_of(&datum).as_inline().unwrap().len(), 1024);
}

/// §3.2: a peer must not send frames gated behind a feature the other side did not advertise. An
/// acceptor that withholds `attachments` keeps inlining, however large the blob — the far end's
/// size limits are then its own problem, but it never waits on bytes that will not come.
#[tokio::test]
async fn a_peer_that_did_not_advertise_attachments_gets_inline_bytes() {
    let options = AcceptOptions {
        features: vec![FEATURE_CANCEL.into()],
        ..AcceptOptions::default()
    };
    let (client, server) = connect_with(options).await;
    assert!(!client.attachments_enabled());
    assert!(!server.attachments_enabled());
    camera(&client).await;

    let raw = client
        .call_raw("camera.grab", Payload::from_typed(&serde_json::json!({ "size": BIG })).unwrap())
        .await
        .unwrap();

    assert!(attach::refs(&raw.0).is_empty());
    assert_eq!(blob_of(&raw.to_typed::<Datum>().unwrap()).as_inline().unwrap().len(), BIG);
}

/// Credit is the only thing bounding how much a sender can push at a receiver that is not reading.
/// With an initial grant far below the blob, the transfer must block and resume on `s_credit`
/// rather than overrun or deadlock.
#[tokio::test]
async fn a_transfer_larger_than_its_credit_still_completes() {
    let options = AcceptOptions {
        limits: Limits { initial_stream_credit: 4096, max_chunk: 1024, ..Limits::default() },
        ..AcceptOptions::default()
    };
    let (client, _server) = connect_with(options).await;
    let cam = camera(&client).await;

    let big = cam.grab(BIG as u64).await.unwrap();
    assert_eq!(big, image_of(BIG as u64));
}

/// The declared size is checked before the bytes are spent, so a caller that cannot hold the blob
/// finds out immediately rather than after the transfer.
#[tokio::test]
async fn a_blob_over_the_caller_limit_is_refused_up_front() {
    let (client, _server) = connect().await;
    camera(&client).await;

    let mut raw = client
        .call_raw("camera.grab", Payload::from_typed(&serde_json::json!({ "size": BIG })).unwrap())
        .await
        .unwrap();

    let err = client.hydrate(&mut raw, 1024).await.unwrap_err();
    assert!(err.to_string().contains("over the 1024-byte limit"), "got {err}");
}

/// Claiming is one-shot: the second taker gets nothing rather than a stream that will never yield.
#[tokio::test]
async fn an_attachment_can_only_be_claimed_once() {
    let (client, _server) = connect().await;
    camera(&client).await;

    let raw = client
        .call_raw("camera.grab", Payload::from_typed(&serde_json::json!({ "size": BIG })).unwrap())
        .await
        .unwrap();
    let stream = attach::refs(&raw.0)[0].stream;

    let claimed = client.collect_attachment(stream, 8 * 1024 * 1024).await.unwrap();
    assert_eq!(claimed.len(), BIG);
    let err = client.collect_attachment(stream, 8 * 1024 * 1024).await.unwrap_err();
    assert!(err.to_string().contains("already been claimed"), "got {err}");
}
