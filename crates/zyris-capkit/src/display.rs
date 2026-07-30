//! Where a capability that needs the monitor layout gets it.
//!
//! `screen_capture` enumerates displays itself, but `input` does not: the `input` feature pulls in
//! `enigo` and nothing else, while the enumerators — `xcap`, `libwayshot` — sit behind `screen`
//! and drag in the whole display stack. So [`EnigoInput`](crate::EnigoInput) takes a [`Displays`]
//! from whoever builds it. A node with both features hands it [`HostDisplays`](crate::HostDisplays);
//! a node with only `input` hands it a `Vec<Display>` it worked out some other way.

use zyris::WireError;
use zyris_caps::Display;

/// Something that can report the monitor layout.
///
/// Blocking on purpose — every caller is already inside `spawn_blocking`, and the platform APIs
/// underneath are blocking anyway.
pub trait Displays: Send + Sync + 'static {
    fn displays(&self) -> zyris::Result<Vec<Display>>;
}

/// A layout that does not change: tests, and nodes that know their monitors up front.
impl Displays for Vec<Display> {
    fn displays(&self) -> zyris::Result<Vec<Display>> {
        Ok(self.clone())
    }
}

pub(crate) fn no_such_display(wanted: &str) -> WireError {
    WireError::invalid_params(format!("no display matches `{wanted}`"))
}

/// Id first, name second — the rule `screen_capture.screenshot` documents, applied to a layout
/// that has already been enumerated rather than to live platform handles. The `screen` backends do
/// their own matching against live `Monitor`/`OutputInfo` handles instead.
#[cfg(feature = "input")]
pub(crate) fn resolve<'a>(displays: &'a [Display], wanted: &str) -> zyris::Result<&'a Display> {
    if displays.is_empty() {
        return Err(WireError::new(
            zyris::ErrorCode::Internal,
            "no displays are attached",
        ));
    }
    displays
        .iter()
        .find(|d| d.id == wanted)
        .or_else(|| displays.iter().find(|d| d.name == wanted))
        .ok_or_else(|| no_such_display(wanted))
}
