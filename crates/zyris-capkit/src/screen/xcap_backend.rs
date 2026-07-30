use image::RgbaImage;
use xcap::{Monitor, XCapError};
use zyris::WireError;
use zyris_caps::Display;

use super::{internal, no_such_display};
use crate::display::scale;

/// A display's identity survives one call, not a reboot: `id()` is what the platform reports, and
/// the index is only there so a monitor whose id lookup fails is still addressable.
fn monitor_id(monitor: &Monitor, index: usize) -> String {
    match monitor.id() {
        Ok(id) => id.to_string(),
        Err(_) => format!("display-{index}"),
    }
}

fn monitor_name(monitor: &Monitor) -> String {
    monitor
        .friendly_name()
        .or_else(|_| monitor.name())
        .unwrap_or_default()
}

/// `xcap` does not report one coordinate space, and the one it picks is never the one it captures
/// in. On X11 it divides the RandR geometry by the scale it derives from `Xft.dpi`; on macOS it
/// reports Quartz points. Both capture in physical pixels, and so does `input.move_to` — see
/// [`target`](crate::EnigoInput) — so undo the division here rather than leaving two spaces loose
/// in the crate. Windows reports `dmPelsWidth` and a `dmPosition` that are physical already.
#[cfg(not(target_os = "windows"))]
fn to_physical(display: Display) -> Display {
    let factor = scale(display.scale_factor);
    Display {
        x: (display.x as f32 * factor).round() as i32,
        y: (display.y as f32 * factor).round() as i32,
        width: (display.width as f32 * factor).round() as u32,
        height: (display.height as f32 * factor).round() as u32,
        scale_factor: factor,
        ..display
    }
}

#[cfg(target_os = "windows")]
fn to_physical(display: Display) -> Display {
    Display { scale_factor: scale(display.scale_factor), ..display }
}

fn describe(monitor: &Monitor, index: usize) -> Display {
    to_physical(Display {
        id: monitor_id(monitor, index),
        name: monitor_name(monitor),
        x: monitor.x().unwrap_or(0),
        y: monitor.y().unwrap_or(0),
        width: monitor.width().unwrap_or(0),
        height: monitor.height().unwrap_or(0),
        scale_factor: monitor.scale_factor().unwrap_or(1.0),
        primary: monitor.is_primary().unwrap_or(false),
    })
}

fn select<'a>(monitors: &'a [Monitor], wanted: Option<&str>) -> zyris::Result<(&'a Monitor, usize)> {
    if let Some(wanted) = wanted {
        let found = monitors
            .iter()
            .enumerate()
            .find(|(index, m)| monitor_id(m, *index) == wanted)
            .or_else(|| {
                monitors
                    .iter()
                    .enumerate()
                    .find(|(_, m)| monitor_name(m) == wanted)
            });
        return match found {
            Some((index, monitor)) => Ok((monitor, index)),
            None => Err(no_such_display(wanted)),
        };
    }
    let index = monitors
        .iter()
        .position(|m| m.is_primary().unwrap_or(false))
        .unwrap_or(0);
    Ok((&monitors[index], index))
}

pub(super) fn displays() -> zyris::Result<Vec<Display>> {
    let monitors = Monitor::all().map_err(internal)?;
    Ok(monitors
        .iter()
        .enumerate()
        .map(|(index, monitor)| describe(monitor, index))
        .collect())
}

pub(super) fn capture(display: Option<&str>) -> zyris::Result<(String, RgbaImage)> {
    let monitors = Monitor::all().map_err(internal)?;
    if monitors.is_empty() {
        return Err(internal("no displays are attached"));
    }
    let (monitor, index) = select(&monitors, display)?;
    let id = monitor_id(monitor, index);
    let image = monitor.capture_image().map_err(|err| match err {
        XCapError::InvalidCaptureRegion(msg) => WireError::invalid_params(msg),
        other => internal(other),
    })?;
    Ok((id, image))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scaled(scale_factor: f32) -> Display {
        Display {
            id: "1".into(),
            name: "DP-1".into(),
            x: 1920,
            y: -360,
            width: 1920,
            height: 1080,
            scale_factor,
            primary: false,
        }
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn a_scaled_display_is_reported_in_the_pixels_it_captures_in() {
        let display = to_physical(scaled(2.0));
        assert_eq!((display.width, display.height), (3840, 2160));
        assert_eq!((display.x, display.y), (3840, -720));
        assert_eq!(display.scale_factor, 2.0);
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn windows_geometry_is_already_physical() {
        let display = to_physical(scaled(2.0));
        assert_eq!((display.width, display.height), (1920, 1080));
        assert_eq!((display.x, display.y), (1920, -360));
    }

    #[test]
    fn an_unscaled_display_is_left_where_it_is() {
        assert_eq!(to_physical(scaled(1.0)), scaled(1.0));
    }

    /// `xcap` hands back `0.0` where it cannot work a scale out. Multiplying by it would put every
    /// display at the origin with no size at all.
    #[test]
    fn a_scale_of_zero_is_a_scale_of_one() {
        assert_eq!(to_physical(scaled(0.0)), scaled(1.0));
        assert_eq!(to_physical(scaled(f32::NAN)), scaled(1.0));
    }
}
