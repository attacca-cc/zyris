use image::RgbaImage;
use xcap::{Monitor, XCapError};
use zyris::WireError;
use zyris_caps::Display;

use super::{internal, no_such_display};

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

fn describe(monitor: &Monitor, index: usize) -> Display {
    Display {
        id: monitor_id(monitor, index),
        name: monitor_name(monitor),
        x: monitor.x().unwrap_or(0),
        y: monitor.y().unwrap_or(0),
        width: monitor.width().unwrap_or(0),
        height: monitor.height().unwrap_or(0),
        scale_factor: monitor.scale_factor().unwrap_or(1.0),
        primary: monitor.is_primary().unwrap_or(false),
    }
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
