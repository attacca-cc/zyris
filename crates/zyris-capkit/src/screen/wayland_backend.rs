//! Capture over `wl_output` + `zwlr_screencopy`, the same pair `grim` uses.
//!
//! The dependency is `libwayshot-xcap` rather than upstream `libwayshot` only because `xcap`
//! already pulls that fork in: taking the other one would build a second copy of the whole Wayland
//! stack for the same protocol.

use image::RgbaImage;
use libwayshot::{output::OutputInfo, WayshotConnection};
use zyris_caps::Display;

use super::{internal, no_such_display};

/// Whether this session has a compositor that will hand over pixels.
///
/// GNOME and KDE do not implement `zwlr_screencopy`; they screenshot through a portal, which is
/// what the `xcap` backend already knows how to reach.
pub(super) fn is_available() -> bool {
    match WayshotConnection::new() {
        Ok(conn) => !conn.get_all_outputs().is_empty(),
        Err(_) => false,
    }
}

/// Wayland has no notion of a primary output, so the one covering the origin stands in. That is
/// where a compositor puts the output a user thinks of as first, and it gives `screenshot(None)`
/// a stable answer rather than whichever output the compositor happened to advertise first.
fn covers_origin(output: &OutputInfo) -> bool {
    let region = output.logical_region.inner;
    let (x, y) = (region.position.x, region.position.y);
    x <= 0
        && y <= 0
        && x + region.size.width as i32 > 0
        && y + region.size.height as i32 > 0
}

fn describe(output: &OutputInfo) -> Display {
    let region = output.logical_region.inner;
    Display {
        id: output.name.clone(),
        name: if output.description.is_empty() {
            output.name.clone()
        } else {
            output.description.clone()
        },
        x: region.position.x,
        y: region.position.y,
        width: region.size.width,
        height: region.size.height,
        // `OutputInfo::scale` is private upstream; it is this ratio.
        scale_factor: match region.size.height {
            0 => 1.0,
            logical => output.physical_size.height as f32 / logical as f32,
        },
        primary: covers_origin(output),
    }
}

fn select<'a>(outputs: &'a [OutputInfo], wanted: Option<&str>) -> zyris::Result<&'a OutputInfo> {
    let Some(wanted) = wanted else {
        return outputs
            .iter()
            .find(|o| covers_origin(o))
            .or_else(|| outputs.first())
            .ok_or_else(|| internal("no displays are attached"));
    };
    outputs
        .iter()
        .find(|o| o.name == wanted)
        .or_else(|| outputs.iter().find(|o| o.description == wanted))
        .ok_or_else(|| no_such_display(wanted))
}

pub(super) fn displays() -> zyris::Result<Vec<Display>> {
    let conn = WayshotConnection::new().map_err(internal)?;
    Ok(conn.get_all_outputs().iter().map(describe).collect())
}

pub(super) fn capture(display: Option<&str>) -> zyris::Result<(String, RgbaImage)> {
    let conn = WayshotConnection::new().map_err(internal)?;
    let outputs = conn.get_all_outputs();
    let output = select(outputs, display)?;
    // One whole output. Asking for a sub-region here would go down `capture_output_region`, whose
    // whole-output case is broken on at least Hyprland — cropping happens in the parent module.
    let image = conn.screenshot_single_output(output, false).map_err(internal)?;
    Ok((output.name.clone(), image.into_rgba8()))
}
