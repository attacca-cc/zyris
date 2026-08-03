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
    /// Size in physical pixels: the pixels a `screenshot` of this display is made of, and the ones
    /// `input.move_to` takes. `x` and `y` are in the same space.
    pub width: u32,
    pub height: u32,
    /// Physical pixels per logical pixel. `1.0` on a display that is not scaled.
    ///
    /// Reported for a caller that needs to talk about the display the way its desktop environment
    /// does. It is not something to apply to the fields above — those are already physical.
    #[serde(default)]
    pub scale_factor: f32,
    #[serde(default)]
    pub primary: bool,
}

/// A region of a display, in display-local physical pixels — the same space as [`Display`] and as
/// the coordinates `input.move_to` takes.
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
    /// primary display. `region` is in display-local physical pixels.
    ///
    /// `format` and `max_width` exist because a full-resolution PNG of a 4K display is several
    /// megabytes, and a `Datum::Image` travels inline in the response — `zyris::proto::
    /// INLINE_BLOB_MAX` puts the comfortable ceiling at 4 MiB, which a 4K JPEG (or an ordinary
    /// desktop's 4K PNG) fits at full resolution; a larger capture is detached onto its own stream
    /// and arrives as an attachment instead. `format` defaults to PNG;
    /// `max_width` downscales the capture before encoding, preserving aspect ratio, and is
    /// ignored when it is not smaller than the capture.
    ///
    /// An implementation is free to downscale further on its own to keep the response deliverable,
    /// so the image returned may be smaller than either the display or `max_width`. When it is,
    /// the returned `Datum::Image` describes what happened — its `description` names the display's
    /// real size and the factor to multiply image coordinates by. Read it before feeding a
    /// position from a screenshot into `input.move_to`: that call takes the same `display` and the
    /// same display-local physical pixels, so the downscale factor and the `region` origin are the
    /// whole conversion — the display's own position on the desktop is `move_to`'s to add, not
    /// yours, and [`Display::scale_factor`] plays no part.
    async fn screenshot(
        &self,
        display: Option<String>,
        region: Option<Region>,
        format: Option<ImageFormat>,
        max_width: Option<u32>,
    ) -> zyris::Result<Datum>;
}
