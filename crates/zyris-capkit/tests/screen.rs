#![cfg(feature = "screen")]

use std::time::Duration;

use zyris::{Blob, Datum, Node, NodeKind};
use zyris_caps::{ImageFormat, ScreenCapture, ScreenCaptureClient, ScreenCaptureServer};
use zyris_capkit::{image, xcap, XcapScreenCapture};

/// CI and headless dev boxes have no compositor to capture. Ask xcap directly rather than letting
/// the capability report a failure we cannot tell apart from a real bug.
fn has_a_display() -> bool {
    match xcap::Monitor::all() {
        Ok(monitors) => !monitors.is_empty(),
        Err(_) => false,
    }
}

async fn connect() -> ScreenCaptureClient {
    let server = Node::builder()
        .name("screen-node")
        .kind(NodeKind::Service)
        .capability(ScreenCaptureServer(XcapScreenCapture::default()))
        .build()
        .unwrap();
    let client_node = Node::builder().name("client").kind(NodeKind::Cli).build().unwrap();
    let (conn, _server) = zyris::testing::duplex(&client_node, &server).await.unwrap();
    conn.wait_capability(Duration::from_secs(2)).await.unwrap()
}

fn inline(datum: &Datum) -> (&str, &[u8]) {
    match datum {
        Datum::Image { media_type, blob: Blob::Inline(bytes), .. } => (media_type, bytes),
        other => panic!("expected an inline image datum, got {other:?}"),
    }
}

#[tokio::test]
async fn lists_displays() {
    if !has_a_display() {
        eprintln!("skipping: no display attached");
        return;
    }
    let screen = connect().await;

    let displays = screen.list_displays().await.unwrap();
    assert!(!displays.is_empty());
    for display in &displays {
        assert!(!display.id.is_empty());
        assert!(display.width > 0 && display.height > 0);
    }
}

#[tokio::test]
async fn screenshots_the_primary_display_as_png() {
    if !has_a_display() {
        eprintln!("skipping: no display attached");
        return;
    }
    let screen = connect().await;

    let datum = screen.screenshot(None, None, None, None).await.unwrap();
    let (media_type, bytes) = inline(&datum);
    assert_eq!(media_type, "image/png");
    assert_eq!(
        image::guess_format(bytes).unwrap(),
        image::ImageFormat::Png
    );
}

#[tokio::test]
async fn max_width_downscales_and_format_selects_jpeg() {
    if !has_a_display() {
        eprintln!("skipping: no display attached");
        return;
    }
    let screen = connect().await;

    let datum = screen
        .screenshot(None, None, Some(ImageFormat::Jpeg), Some(320))
        .await
        .unwrap();
    let (media_type, bytes) = inline(&datum);
    assert_eq!(media_type, "image/jpeg");

    let decoded = image::load_from_memory(bytes).unwrap();
    assert_eq!(decoded.width(), 320);
    assert!(bytes.len() < zyris::proto::INLINE_BLOB_MAX);
}

#[tokio::test]
async fn a_region_crops_and_an_unknown_display_is_rejected() {
    if !has_a_display() {
        eprintln!("skipping: no display attached");
        return;
    }
    let screen = connect().await;
    let displays = screen.list_displays().await.unwrap();
    let display = displays.iter().find(|d| d.primary).unwrap_or(&displays[0]);

    let region = zyris_caps::Region { x: 0, y: 0, width: 64, height: 48 };
    let datum = screen
        .screenshot(Some(display.id.clone()), Some(region), None, None)
        .await
        .unwrap();
    let (_, bytes) = inline(&datum);
    let decoded = image::load_from_memory(bytes).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (64, 48));

    let err = screen
        .screenshot(Some("no-such-display".into()), None, None, None)
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("no-such-display"), "{err}");
}
