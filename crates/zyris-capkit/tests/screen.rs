#![cfg(feature = "screen")]

use std::time::Duration;

use zyris::{Blob, Datum, Node, NodeKind};
use zyris_caps::{ImageFormat, ScreenCapture, ScreenCaptureClient, ScreenCaptureServer};
use zyris_capkit::{image, HostScreenCapture, ScreenBackend};

/// CI and headless dev boxes have no compositor to capture. Ask the backend directly rather than
/// letting the capability report a failure we cannot tell apart from a real bug.
fn has_a_display() -> bool {
    match ScreenBackend::detect() {
        #[cfg(target_os = "linux")]
        ScreenBackend::Wayland => true,
        ScreenBackend::Xcap => matches!(zyris_capkit::xcap::Monitor::all(), Ok(m) if !m.is_empty()),
    }
}

async fn serve(screen: HostScreenCapture) -> ScreenCaptureClient {
    eprintln!("screen backend: {:?}", screen.backend());
    let server = Node::builder()
        .name("screen-node")
        .kind(NodeKind::Service)
        .capability(ScreenCaptureServer(screen))
        .build()
        .unwrap();
    let client_node = Node::builder().name("client").kind(NodeKind::Cli).build().unwrap();
    let (conn, _server) = zyris::testing::duplex(&client_node, &server).await.unwrap();
    conn.wait_capability(Duration::from_secs(2)).await.unwrap()
}

async fn connect() -> ScreenCaptureClient {
    serve(HostScreenCapture::default()).await
}

fn inline(datum: &Datum) -> (&str, &[u8]) {
    match datum {
        Datum::Image { media_type, blob: Blob::Inline(bytes), .. } => (media_type, bytes),
        other => panic!("expected an inline image datum, got {other:?}"),
    }
}

fn description(datum: &Datum) -> &str {
    match datum {
        Datum::Image { description: Some(text), .. } => text,
        other => panic!("expected an image datum with a description, got {other:?}"),
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

/// Every display must capture at the size it advertises. A backend that enumerates in one
/// coordinate space and captures in another passes `lists_displays` and still returns the wrong
/// monitor's pixels; a size mismatch is the cheapest way to catch that.
///
/// The budget is off here so the assertion stays about the backend. Auto-fit is what
/// `the_default_budget_holds_on_a_real_screen` is for.
#[tokio::test]
async fn each_display_captures_at_its_advertised_size() {
    if !has_a_display() {
        eprintln!("skipping: no display attached");
        return;
    }
    let screen = serve(HostScreenCapture::default().without_budget()).await;

    for display in screen.list_displays().await.unwrap() {
        let datum = screen
            .screenshot(Some(display.id.clone()), None, None, None)
            .await
            .unwrap();
        let (_, bytes) = inline(&datum);
        let decoded = image::load_from_memory(bytes).unwrap();
        assert_eq!(
            (decoded.width(), decoded.height()),
            (display.width, display.height),
            "display {} ({}) at ({}, {})",
            display.id,
            display.name,
            display.x,
            display.y
        );
    }
}

/// The point of the auto-fit pass: a caller that passes nothing still gets something that will
/// survive the trip, and is told what the scaling did to its coordinates.
#[tokio::test]
async fn the_default_budget_holds_on_a_real_screen() {
    if !has_a_display() {
        eprintln!("skipping: no display attached");
        return;
    }
    let screen = connect().await;

    for display in screen.list_displays().await.unwrap() {
        let datum = screen
            .screenshot(Some(display.id.clone()), None, None, None)
            .await
            .unwrap();
        let (_, bytes) = inline(&datum);
        assert!(
            bytes.len() <= zyris::proto::INLINE_BLOB_MAX,
            "{} is {} bytes, over the {} budget",
            display.id,
            bytes.len(),
            zyris::proto::INLINE_BLOB_MAX
        );

        let decoded = image::load_from_memory(bytes).unwrap();
        let text = description(&datum);
        assert!(text.contains(&display.id), "{text}");
        assert!(
            text.contains(&format!("{}x{}", display.width, display.height)),
            "description must name the display's real size: {text}"
        );
        if decoded.width() != display.width {
            assert!(
                text.contains("multiply image coordinates by"),
                "a scaled capture must say so: {text}"
            );
        }
        eprintln!(
            "{}: {}x{} -> {}x{} in {} bytes",
            display.id,
            display.width,
            display.height,
            decoded.width(),
            decoded.height(),
            bytes.len()
        );
    }
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
