use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zyris::Datum;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Display {
    /// Stable identifier for this display, as reported by the platform.
    pub id: String,
    /// Human-readable name. Accepted by `screenshot` wherever `id` is.
    #[serde(default)]
    pub name: String,
    /// Origin of this display within the virtual desktop, so a caller holding a global
    /// coordinate can work out which display it falls on and subtract to get a `Region`.
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Physical pixels per logical pixel. `1.0` on a display that is not scaled.
    #[serde(default)]
    pub scale_factor: f32,
    #[serde(default)]
    pub primary: bool,
}

/// A region of a display, in display-local pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Region {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImageFormat {
    Png,
    Jpeg,
}

impl ImageFormat {
    pub fn media_type(self) -> &'static str {
        match self {
            ImageFormat::Png => "image/png",
            ImageFormat::Jpeg => "image/jpeg",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
        }
    }
}

#[zyris::capability(name = "screen_capture", version = 1)]
pub trait ScreenCapture {
    /// List available displays.
    async fn list_displays(&self) -> zyris::Result<Vec<Display>>;

    /// Capture a still image of a display (optionally cropped to a region).
    ///
    /// `display` matches a [`Display::id`] first and a [`Display::name`] second; `None` picks the
    /// primary display. `region` is in display-local pixels.
    ///
    /// `format` and `max_width` exist because a full-resolution PNG of a 4K display is several
    /// megabytes, and a `Datum::Image` travels inline in the response — `zyris::proto::
    /// INLINE_BLOB_MAX` puts the comfortable ceiling at 128 KiB. `format` defaults to PNG;
    /// `max_width` downscales the capture before encoding, preserving aspect ratio, and is
    /// ignored when it is not smaller than the capture.
    async fn screenshot(
        &self,
        display: Option<String>,
        region: Option<Region>,
        format: Option<ImageFormat>,
        max_width: Option<u32>,
    ) -> zyris::Result<Datum>;
}
