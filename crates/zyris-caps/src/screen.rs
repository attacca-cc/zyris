use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zyris::Datum;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Display {
    pub id: String,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub primary: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Region {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[zyris::capability(name = "screen_capture", version = 1)]
pub trait ScreenCapture {
    /// List available displays.
    async fn list_displays(&self) -> zyris::Result<Vec<Display>>;

    /// Capture a still image of a display (optionally cropped to a region).
    async fn screenshot(
        &self,
        display: Option<String>,
        region: Option<Region>,
    ) -> zyris::Result<Datum>;
}
