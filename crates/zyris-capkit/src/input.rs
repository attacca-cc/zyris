use std::sync::{Arc, Mutex};

use enigo::{Axis, Coordinate, Direction, Enigo, Keyboard, Mouse, Settings};
use zyris::{ErrorCode, WireError};
use zyris_caps::{Input, MouseButton};

use crate::chord::parse_chord;

/// [`Input`] backed by [`enigo`] — XTEST on X11, `wlr-virtual-*` on Wayland (with the
/// `input-wayland` feature), `SendInput` on Windows, `CGEvent` on macOS.
///
/// ```no_run
/// # use zyris_capkit::EnigoInput;
/// # use zyris_caps::InputServer;
/// # fn main() -> zyris::Result<()> {
/// let server = InputServer(EnigoInput::new()?);
/// # Ok(())
/// # }
/// ```
///
/// On macOS the process needs Accessibility permission; the first call opens the system prompt.
pub struct EnigoInput {
    // `Enigo` is `Send` but not `Sync` — it owns a connection to the display server — so the
    // capability's `Sync` bound is met by the mutex rather than by enigo, and every call hops to a
    // blocking thread because none of these methods are async underneath.
    enigo: Arc<Mutex<Enigo>>,
}

impl EnigoInput {
    /// Connect to the display server.
    ///
    /// This fails on a headless host, which is the answer a node wants at startup: if there is no
    /// display, do not announce the `input` capability at all.
    pub fn new() -> zyris::Result<Self> {
        Self::with_settings(&Settings::default())
    }

    pub fn with_settings(settings: &Settings) -> zyris::Result<Self> {
        let enigo = Enigo::new(settings).map_err(|err| {
            WireError::new(
                ErrorCode::Internal,
                format!("cannot connect to the display server: {err}"),
            )
        })?;
        Ok(EnigoInput { enigo: Arc::new(Mutex::new(enigo)) })
    }

    async fn with<F>(&self, f: F) -> zyris::Result<()>
    where
        F: FnOnce(&mut Enigo) -> zyris::Result<()> + Send + 'static,
    {
        let enigo = self.enigo.clone();
        let task = tokio::task::spawn_blocking(move || {
            let mut enigo = match enigo.lock() {
                Ok(guard) => guard,
                // A previous call panicked mid-simulation. The keys it was holding are unknown,
                // so the connection is no longer something to build on.
                Err(_) => {
                    return Err(WireError::new(
                        ErrorCode::Internal,
                        "input connection is poisoned by an earlier panic",
                    ))
                }
            };
            f(&mut enigo)
        });
        match task.await {
            Ok(result) => result,
            Err(join) => Err(WireError::new(
                ErrorCode::Internal,
                format!("input task failed: {join}"),
            )),
        }
    }
}

fn input_err(err: impl std::fmt::Display) -> WireError {
    WireError::new(ErrorCode::Internal, err.to_string())
}

fn button(button: MouseButton) -> enigo::Button {
    match button {
        MouseButton::Left => enigo::Button::Left,
        MouseButton::Right => enigo::Button::Right,
        MouseButton::Middle => enigo::Button::Middle,
    }
}

#[zyris::async_trait]
impl Input for EnigoInput {
    async fn type_text(&self, text: String) -> zyris::Result<()> {
        self.with(move |enigo| enigo.text(&text).map_err(input_err)).await
    }

    async fn key(&self, chord: String) -> zyris::Result<()> {
        let (modifiers, key) = parse_chord(&chord)?;
        self.with(move |enigo| {
            let mut held = Vec::with_capacity(modifiers.len());
            let mut result = Ok(());
            for modifier in modifiers {
                match enigo.key(modifier, Direction::Press) {
                    Ok(()) => held.push(modifier),
                    Err(err) => {
                        result = Err(input_err(err));
                        break;
                    }
                }
            }
            if result.is_ok() {
                result = enigo.key(key, Direction::Click).map_err(input_err);
            }
            // Release whatever went down, whether or not the key itself made it — a stuck Ctrl
            // outlives this call and breaks every keystroke the user makes afterwards.
            for modifier in held.into_iter().rev() {
                let released = enigo.key(modifier, Direction::Release).map_err(input_err);
                if result.is_ok() {
                    result = released;
                }
            }
            result
        })
        .await
    }

    async fn move_to(&self, x: i32, y: i32) -> zyris::Result<()> {
        self.with(move |enigo| enigo.move_mouse(x, y, Coordinate::Abs).map_err(input_err))
            .await
    }

    async fn click(&self, button_: MouseButton) -> zyris::Result<()> {
        self.with(move |enigo| {
            enigo.button(button(button_), Direction::Click).map_err(input_err)
        })
        .await
    }

    async fn scroll(&self, dx: i32, dy: i32) -> zyris::Result<()> {
        self.with(move |enigo| {
            if dx != 0 {
                enigo.scroll(dx, Axis::Horizontal).map_err(input_err)?;
            }
            if dy != 0 {
                enigo.scroll(dy, Axis::Vertical).map_err(input_err)?;
            }
            Ok(())
        })
        .await
    }
}
