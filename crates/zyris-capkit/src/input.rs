use std::sync::{Arc, Mutex};

use enigo::{Axis, Coordinate, Direction, Enigo, Keyboard, Mouse, Settings};
use zyris::{ErrorCode, WireError};
use zyris_caps::{Display, Input, MouseButton};

use crate::chord::parse_chord;
use crate::display::{resolve, Displays};

/// [`Input`] backed by [`enigo`] — XTEST on X11, `wlr-virtual-*` on Wayland (with the
/// `input-wayland` feature), `SendInput` on Windows, `CGEvent` on macOS.
///
/// [`Input::move_to`] is display-local, so this needs a [`Displays`] to resolve the display against.
/// With the `screen` feature that is [`HostDisplays`](crate::HostDisplays); otherwise, any
/// `Vec<Display>` the node worked out for itself.
///
/// ```no_run
/// # use zyris_capkit::EnigoInput;
/// # use zyris_caps::{Display, InputServer};
/// # fn main() -> zyris::Result<()> {
/// let displays = vec![Display {
///     id: "DP-1".into(),
///     name: "DP-1".into(),
///     x: 0,
///     y: 0,
///     width: 1920,
///     height: 1080,
///     scale_factor: 1.0,
///     primary: true,
/// }];
/// let server = InputServer(EnigoInput::new(displays)?);
/// # Ok(())
/// # }
/// ```
///
/// On macOS the process needs Accessibility permission; the first call opens the system prompt.
///
/// In a wlroots session, build with `input-wayland` and give it the layout
/// [`ScreenBackend::Wayland`](crate::ScreenBackend::Wayland) reports. [`Input::move_to`] then
/// addresses the output by name and never converts to a desktop coordinate, which is the only way
/// to reach a second monitor there — see [`move_on_output`]. Without that feature enigo drives
/// XTEST through Xwayland, whose flattened layout does not match the compositor's.
pub struct EnigoInput {
    // `Enigo` is `Send` but not `Sync` — it owns a connection to the display server — so the
    // capability's `Sync` bound is met by the mutex rather than by enigo, and every call hops to a
    // blocking thread because none of these methods are async underneath.
    enigo: Arc<Mutex<Enigo>>,
    // Queried per call rather than snapshotted, so a monitor unplugged or rearranged after startup
    // does not silently send the cursor somewhere else.
    displays: Arc<dyn Displays>,
}

impl EnigoInput {
    /// Connect to the display server.
    ///
    /// This fails on a headless host, which is the answer a node wants at startup: if there is no
    /// display, do not announce the `input` capability at all.
    ///
    /// `displays` is what [`Input::move_to`] resolves its `display` argument against —
    /// [`HostDisplays`](crate::HostDisplays) with the `screen` feature, or any [`Displays`].
    pub fn new(displays: impl Displays) -> zyris::Result<Self> {
        Self::with_settings(&Settings::default(), displays)
    }

    pub fn with_settings(settings: &Settings, displays: impl Displays) -> zyris::Result<Self> {
        let enigo = Enigo::new(settings).map_err(|err| {
            WireError::new(
                ErrorCode::Internal,
                format!("cannot connect to the display server: {err}"),
            )
        })?;
        Ok(EnigoInput {
            enigo: Arc::new(Mutex::new(enigo)),
            displays: Arc::new(displays),
        })
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

/// Resolve the display a display-local position falls on.
///
/// `x` and `y` are display-local **physical** pixels — the pixels a `screen_capture.screenshot` of
/// this display is made of, so a position read off a screenshot goes straight in. That is the space
/// [`Display`] is reported in too; the backends normalise there so nothing here has to choose one.
///
/// The bounds check is not pedantry: it is what catches a caller who is still passing whole-desktop
/// coordinates, which would otherwise land silently on the wrong monitor.
fn local<'a>(displays: &'a [Display], wanted: &str, x: i32, y: i32) -> zyris::Result<&'a Display> {
    let display = resolve(displays, wanted)?;
    if x < 0
        || y < 0
        || (x as i64) >= i64::from(display.width)
        || (y as i64) >= i64::from(display.height)
    {
        return Err(WireError::invalid_params(format!(
            "({x}, {y}) is display-local and does not fall on the {}x{} display `{}`",
            display.width, display.height, display.id
        )));
    }
    Ok(display)
}

/// Turn a display-local position into the absolute one [`Coordinate::Abs`] speaks in.
///
/// Adding the origin is a translation rather than a guess because [`local`] has already put both
/// sides in the same space. The result is physical for every enigo backend but one: macOS
/// `CGEvent` positions in Quartz points, so the sum is divided back down — exactly, because
/// `Display::x` on macOS is that display's own point origin multiplied by its own scale.
///
/// What one space cannot fix is a layout enigo has never seen. An X11-only enigo build driven from
/// [`ScreenBackend::Wayland`](crate::ScreenBackend::Wayland) geometry moves through XTEST, which is
/// Xwayland's flattening of the compositor's arrangement — a monitor above the origin has no
/// negative `y` there, so the outputs come out in a row in a different order, and the origin added
/// here names a point on another monitor. That is what `input-wayland` and [`move_on_output`] are
/// for: in a wlroots session there is no whole-desktop coordinate to translate into at all.
fn target(displays: &[Display], wanted: &str, x: i32, y: i32) -> zyris::Result<(i32, i32)> {
    let display = local(displays, wanted, x, y)?;
    let (x, y) = (display.x + x, display.y + y);
    #[cfg(target_os = "macos")]
    let (x, y) = {
        let factor = crate::display::scale(display.scale_factor);
        ((x as f32 / factor).round() as i32, (y as f32 / factor).round() as i32)
    };
    Ok((x, y))
}

/// Move within one wlroots output, addressing it by name.
///
/// `wlr-virtual-pointer` positions the cursor *within* the output its pointer was created against —
/// `motion_absolute` carries the extents to map into, not a desktop coordinate — so a multi-monitor
/// layout is reachable only by binding a pointer per output. That is the one thing the enigo fork
/// adds over upstream, which creates a single unbound pointer and can therefore only ever land on
/// one monitor.
///
/// Display-local is already what the protocol wants, so no origin is added here. The name is the
/// connector — `DP-1` — which is exactly what [`ScreenBackend::Wayland`](crate::ScreenBackend::Wayland)
/// reports as [`Display::id`]; `Display::name`, the monitor's description, is tried second the same
/// way [`resolve`] does.
#[cfg(feature = "input-wayland")]
fn move_on_output(enigo: &mut Enigo, display: &Display, x: i32, y: i32) -> zyris::Result<()> {
    match enigo.move_mouse_on_output(&display.id, x, y) {
        Ok(()) => Ok(()),
        Err(first) => enigo.move_mouse_on_output(&display.name, x, y).map_err(|_| {
            WireError::new(
                ErrorCode::Internal,
                format!(
                    "the compositor has no output `{}`: {first}. The layout `move_to` resolves \
                     against has to be the one the compositor advertises — build the node's \
                     `HostDisplays` with `ScreenBackend::Wayland`.",
                    display.id
                ),
            )
        }),
    }
}

/// The session enigo is driving, not the one the screen backend picked. Compiling the
/// `wlr-virtual-*` path in is half the answer; there also has to be a compositor to talk to, since
/// the same build falls back to XTEST under X11. Same probe
/// [`ScreenBackend::detect`](crate::ScreenBackend::detect) uses.
#[cfg(feature = "input-wayland")]
fn on_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
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

    async fn move_to(&self, display: String, x: i32, y: i32) -> zyris::Result<()> {
        let displays = self.displays.clone();
        self.with(move |enigo| {
            let layout = displays.displays()?;
            #[cfg(feature = "input-wayland")]
            if on_wayland() {
                return move_on_output(enigo, local(&layout, &display, x, y)?, x, y);
            }
            let (x, y) = target(&layout, &display, x, y)?;
            enigo.move_mouse(x, y, Coordinate::Abs).map_err(input_err)
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A second monitor above and to the right of the first. The negative `y` is the arrangement
    /// `ScreenBackend` warns X11 cannot represent, and the one an origin-addition bug survives on a
    /// single-monitor desk.
    fn layout() -> Vec<Display> {
        vec![
            Display {
                id: "1".into(),
                name: "DP-1".into(),
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
                scale_factor: 1.0,
                primary: true,
            },
            Display {
                id: "2".into(),
                name: "HDMI-A-1".into(),
                x: 1920,
                y: -360,
                width: 2560,
                height: 1440,
                scale_factor: 1.0,
                primary: false,
            },
        ]
    }

    #[test]
    fn an_id_resolves_and_the_origin_is_added() {
        assert_eq!(target(&layout(), "1", 100, 200).unwrap(), (100, 200));
        assert_eq!(target(&layout(), "2", 100, 200).unwrap(), (2020, -160));
    }

    #[test]
    fn a_name_resolves_where_no_id_matches() {
        assert_eq!(target(&layout(), "HDMI-A-1", 0, 0).unwrap(), (1920, -360));
    }

    #[test]
    fn an_unknown_display_is_invalid_params() {
        let err = target(&layout(), "DP-9", 0, 0).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidParams);
        assert!(err.message.contains("DP-9"), "{}", err.message);
    }

    #[test]
    fn a_negative_position_is_invalid_params() {
        assert_eq!(
            target(&layout(), "1", -1, 0).unwrap_err().code,
            ErrorCode::InvalidParams
        );
    }

    /// The mistake this catches: a caller passing the whole-desktop coordinate of a point on the
    /// second monitor, which is off the right edge of the first.
    fn out_of_range(x: i32, y: i32) -> WireError {
        target(&layout(), "1", x, y).unwrap_err()
    }

    #[test]
    fn a_position_past_the_edge_is_invalid_params() {
        assert_eq!(out_of_range(2020, 200).code, ErrorCode::InvalidParams);
        assert_eq!(out_of_range(1920, 0).code, ErrorCode::InvalidParams);
        assert_eq!(out_of_range(0, 1080).code, ErrorCode::InvalidParams);
        assert!(target(&layout(), "1", 1919, 1079).is_ok());
    }

    #[test]
    fn an_empty_layout_is_not_invalid_params() {
        // Nothing the caller passed was wrong; the node has no displays to offer.
        assert_eq!(target(&[], "1", 0, 0).unwrap_err().code, ErrorCode::Internal);
    }

    /// A 2x display, reported the way the backends report one: physical throughout, with the scale
    /// alongside as description rather than as something left to apply.
    fn hidpi() -> Vec<Display> {
        vec![Display {
            id: "1".into(),
            name: "eDP-1".into(),
            x: 0,
            y: 0,
            width: 3840,
            height: 2160,
            scale_factor: 2.0,
            primary: true,
        }]
    }

    /// The bug this fixes: aiming at the middle of a scaled panel landed the cursor a quarter of
    /// the way into it, because the layout was logical and enigo's absolute space is not.
    #[test]
    fn the_middle_of_a_scaled_display_is_the_middle() {
        let middle = target(&hidpi(), "1", 1920, 1080).unwrap();
        #[cfg(target_os = "macos")]
        assert_eq!(middle, (960, 540));
        #[cfg(not(target_os = "macos"))]
        assert_eq!(middle, (1920, 1080));
    }

    #[test]
    fn a_scaled_display_is_addressable_to_its_last_physical_pixel() {
        assert!(target(&hidpi(), "1", 3839, 2159).is_ok());
        assert_eq!(
            target(&hidpi(), "1", 3840, 0).unwrap_err().code,
            ErrorCode::InvalidParams
        );
    }

    /// The Wayland path hands the protocol display-local coordinates untouched, so what has to hold
    /// is that the display was resolved and the position bounds-checked against it.
    #[test]
    fn a_display_local_position_resolves_without_an_origin() {
        assert_eq!(local(&layout(), "2", 100, 200).unwrap().id, "2");
        assert_eq!(local(&layout(), "HDMI-A-1", 0, 0).unwrap().id, "2");
        assert_eq!(
            local(&layout(), "2", 2560, 0).unwrap_err().code,
            ErrorCode::InvalidParams
        );
    }
}
