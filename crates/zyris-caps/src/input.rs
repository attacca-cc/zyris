use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[zyris::capability(name = "input", version = 1)]
pub trait Input {
    /// Type a string of text.
    async fn type_text(&self, text: String) -> zyris::Result<()>;

    /// Press a key chord such as `ctrl+c` or `Enter`.
    async fn key(&self, chord: String) -> zyris::Result<()>;

    /// Move the cursor to a position on a display.
    ///
    /// `display` matches a [`Display::id`](crate::Display::id) first and a
    /// [`Display::name`](crate::Display::name) second, the same way `screen_capture.screenshot`
    /// resolves it. `x` and `y` are display-local physical pixels — the same pixels
    /// [`Display::width`](crate::Display::width) counts — so a coordinate read off a screenshot of
    /// that display goes in unchanged, and [`Display::scale_factor`](crate::Display::scale_factor)
    /// is not something to apply on the way.
    async fn move_to(&self, display: String, x: i32, y: i32) -> zyris::Result<()>;

    /// Click a mouse button at the current position.
    async fn click(&self, button: MouseButton) -> zyris::Result<()>;

    /// Scroll by a delta.
    async fn scroll(&self, dx: i32, dy: i32) -> zyris::Result<()>;
}
