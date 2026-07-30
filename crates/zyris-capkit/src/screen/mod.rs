mod xcap_backend;

#[cfg(target_os = "linux")]
mod wayland_backend;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder, RgbaImage};
use zyris::{Blob, Datum, ErrorCode, WireError};
use zyris_caps::{Display, ImageFormat, Region, ScreenCapture};

use crate::display::Displays;
pub(crate) use crate::display::no_such_display;

/// Which platform API the capture goes through.
///
/// There is a real choice to make on Linux, and it is not the obvious one. `xcap` enumerates
/// monitors over XRandR even in a Wayland session, then captures over `zwlr_screencopy` in
/// Wayland's coordinate space. Those two spaces agree only when Xwayland can express the
/// compositor's layout — a monitor above the origin gives it a negative `y`, which X11 has no way
/// to represent, so Xwayland flattens the whole arrangement into a row and every captured
/// rectangle lands on the wrong output. [`ScreenBackend::Wayland`] talks to `wl_output` directly
/// and never involves X11.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenBackend {
    /// `xcap`: XRandR on X11, Quartz on macOS, GDI on Windows.
    Xcap,
    /// `libwayshot`: `wl_output` plus `zwlr_screencopy`, one whole output at a time.
    #[cfg(target_os = "linux")]
    Wayland,
}

impl ScreenBackend {
    /// Pick a backend for this session.
    ///
    /// In a Wayland session this probes for a usable `zwlr_screencopy` connection and only then
    /// commits to [`ScreenBackend::Wayland`]. A compositor without that protocol — GNOME, KDE —
    /// falls back to `xcap`, which reaches those through their screenshot portal.
    pub fn detect() -> ScreenBackend {
        #[cfg(target_os = "linux")]
        if std::env::var_os("WAYLAND_DISPLAY").is_some() && wayland_backend::is_available() {
            return ScreenBackend::Wayland;
        }
        ScreenBackend::Xcap
    }
}

/// [`ScreenCapture`] for the machine the node runs on.
///
/// ```no_run
/// # use zyris_capkit::HostScreenCapture;
/// # use zyris_caps::{ImageFormat, ScreenCaptureServer};
/// let screen = HostScreenCapture::default()
///     .with_format(ImageFormat::Jpeg)
///     .with_max_width(1280)
///     .with_jpeg_quality(70);
/// let server = ScreenCaptureServer(screen);
/// ```
///
/// The defaults set a ceiling a caller cannot exceed by forgetting to pass one; per-call `format`
/// and `max_width` still win where they are given.
pub struct HostScreenCapture {
    backend: ScreenBackend,
    default_format: ImageFormat,
    default_max_width: Option<u32>,
    jpeg_quality: u8,
    budget: Option<usize>,
}

impl Default for HostScreenCapture {
    fn default() -> Self {
        HostScreenCapture {
            backend: ScreenBackend::detect(),
            default_format: ImageFormat::Png,
            default_max_width: None,
            jpeg_quality: 80,
            budget: Some(zyris::proto::INLINE_BLOB_MAX),
        }
    }
}

impl HostScreenCapture {
    /// Override the backend [`ScreenBackend::detect`] chose.
    pub fn with_backend(mut self, backend: ScreenBackend) -> Self {
        self.backend = backend;
        self
    }

    pub fn backend(&self) -> ScreenBackend {
        self.backend
    }

    pub fn with_format(mut self, format: ImageFormat) -> Self {
        self.default_format = format;
        self
    }

    pub fn with_max_width(mut self, max_width: u32) -> Self {
        self.default_max_width = Some(max_width);
        self
    }

    pub fn with_jpeg_quality(mut self, quality: u8) -> Self {
        self.jpeg_quality = quality.clamp(1, 100);
        self
    }

    /// How many encoded bytes a screenshot may come to before it is shrunk to fit.
    ///
    /// Defaults to [`zyris::proto::INLINE_BLOB_MAX`]. This is a floor on usefulness, not a
    /// preference: a caller that names a `max_width` gets it applied first, and the fit pass only
    /// engages if the result would still be too large to survive the trip.
    pub fn with_budget(mut self, bytes: usize) -> Self {
        self.budget = Some(bytes);
        self
    }

    /// Send whatever the encoder produced, however large. For a node on a local socket whose
    /// caller would rather have every pixel than a reply that definitely arrives.
    pub fn without_budget(mut self) -> Self {
        self.budget = None;
        self
    }
}

pub(crate) fn internal(err: impl std::fmt::Display) -> WireError {
    WireError::new(ErrorCode::Internal, err.to_string())
}

/// Both backends are blocking and hold platform connections that are not worth keeping across an
/// await, so every call opens, captures, and encodes inside one closure and hands back bytes.
async fn blocking<T, F>(f: F) -> zyris::Result<T>
where
    F: FnOnce() -> zyris::Result<T> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(result) => result,
        Err(join) => Err(internal(format!("screen capture task failed: {join}"))),
    }
}

fn displays(backend: ScreenBackend) -> zyris::Result<Vec<Display>> {
    match backend {
        ScreenBackend::Xcap => xcap_backend::displays(),
        #[cfg(target_os = "linux")]
        ScreenBackend::Wayland => wayland_backend::displays(),
    }
}

/// The monitor layout of this machine, for a capability that needs it but does not capture.
///
/// ```no_run
/// # use zyris_capkit::{EnigoInput, HostDisplays};
/// # fn main() -> zyris::Result<()> {
/// let input = EnigoInput::new(HostDisplays::default())?;
/// # Ok(())
/// # }
/// ```
///
/// Use the same backend the node's [`HostScreenCapture`] uses. The two enumerate through different
/// APIs in a Wayland session and, per [`ScreenBackend`], do not always agree on where a monitor is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostDisplays(pub ScreenBackend);

impl Default for HostDisplays {
    fn default() -> Self {
        HostDisplays(ScreenBackend::detect())
    }
}

impl Displays for HostDisplays {
    fn displays(&self) -> zyris::Result<Vec<Display>> {
        displays(self.0)
    }
}

/// Capture a whole display and return it alongside the id it was resolved to.
fn capture(backend: ScreenBackend, display: Option<&str>) -> zyris::Result<(String, RgbaImage)> {
    match backend {
        ScreenBackend::Xcap => xcap_backend::capture(display),
        #[cfg(target_os = "linux")]
        ScreenBackend::Wayland => wayland_backend::capture(display),
    }
}

/// Crop here rather than asking the platform for a sub-rectangle.
///
/// `xcap::Monitor::capture_region` adds the monitor's origin to the coordinates on X11 but passes
/// them through as virtual-desktop coordinates on Wayland, so the same `Region` would name two
/// different rectangles. Cropping the full capture costs a copy and means one rectangle
/// everywhere.
fn crop(full: RgbaImage, region: Option<Region>) -> zyris::Result<RgbaImage> {
    let Some(region) = region else {
        return Ok(full);
    };
    if region.x < 0 || region.y < 0 {
        return Err(WireError::invalid_params(
            "region x and y are display-local and cannot be negative",
        ));
    }
    let (x, y) = (region.x as u32, region.y as u32);
    if region.width == 0
        || region.height == 0
        || x.saturating_add(region.width) > full.width()
        || y.saturating_add(region.height) > full.height()
    {
        return Err(WireError::invalid_params(format!(
            "region {}x{} at ({x}, {y}) does not fit the {}x{} display",
            region.width,
            region.height,
            full.width(),
            full.height()
        )));
    }
    Ok(image::imageops::crop_imm(&full, x, y, region.width, region.height).to_image())
}

fn downscale(image: RgbaImage, max_width: Option<u32>) -> RgbaImage {
    let Some(max_width) = max_width.filter(|w| *w > 0) else {
        return image;
    };
    if image.width() <= max_width {
        return image;
    }
    let height = (u64::from(image.height()) * u64::from(max_width) / u64::from(image.width()))
        .max(1) as u32;
    image::imageops::resize(&image, max_width, height, image::imageops::FilterType::Triangle)
}

fn encode(image: &RgbaImage, format: ImageFormat, jpeg_quality: u8) -> zyris::Result<Vec<u8>> {
    let mut out = Vec::new();
    match format {
        ImageFormat::Png => PngEncoder::new(&mut out)
            .write_image(
                image.as_raw(),
                image.width(),
                image.height(),
                ExtendedColorType::Rgba8,
            )
            .map_err(internal)?,
        // JPEG has no alpha channel, and a screenshot's is opaque anyway. Dropping it by hand
        // rather than through `DynamicImage` matters here: the auto-fit pass re-encodes the same
        // image several times, and this allocates three quarters of a frame instead of copying a
        // whole one first.
        ImageFormat::Jpeg => {
            let mut rgb = Vec::with_capacity(image.len() / 4 * 3);
            for pixel in image.pixels() {
                rgb.extend_from_slice(&pixel.0[..3]);
            }
            JpegEncoder::new_with_quality(&mut out, jpeg_quality)
                .write_image(
                    &rgb,
                    image.width(),
                    image.height(),
                    ExtendedColorType::Rgb8,
                )
                .map_err(internal)?
        }
    }
    Ok(out)
}

/// Never shrink below this. A thumbnail nobody can read is not a better answer than an oversized
/// screenshot, and it guarantees the loop ends.
const MIN_AUTO_WIDTH: u32 = 480;

/// Passes after the first. Two is almost always enough — the square-root estimate below lands
/// close — and the cap bounds the worst case for content that will not compress at any size.
const FIT_PASSES: usize = 3;

/// Encode, and if the result overruns `budget`, shrink and encode again until it fits.
///
/// Returns the bytes alongside the dimensions they were produced at, because the caller has to
/// tell whoever asked: an agent that screenshots and then clicks needs to know the image it is
/// looking at is not 1:1 with the display.
fn fit(
    mut image: RgbaImage,
    format: ImageFormat,
    jpeg_quality: u8,
    budget: Option<usize>,
) -> zyris::Result<(Vec<u8>, u32, u32)> {
    let mut bytes = encode(&image, format, jpeg_quality)?;
    let Some(budget) = budget else {
        return Ok((bytes, image.width(), image.height()));
    };

    for _ in 0..FIT_PASSES {
        if bytes.len() <= budget || image.width() <= MIN_AUTO_WIDTH {
            break;
        }
        // Encoded size tracks pixel count, so the linear scale to aim for is the square root of
        // the overshoot. The 0.95 is slack: landing just over the budget costs a whole extra pass,
        // landing just under costs a few pixels.
        let ratio = (budget as f64 / bytes.len() as f64).sqrt() * 0.95;
        let target = (f64::from(image.width()) * ratio) as u32;
        let target = target.clamp(MIN_AUTO_WIDTH, image.width() - 1);
        image = downscale(image, Some(target));
        bytes = encode(&image, format, jpeg_quality)?;
    }

    Ok((bytes, image.width(), image.height()))
}

/// What the caller is actually looking at, in terms of the display it came from.
///
/// This rides in `Datum::Image::description`, which for a model is the difference between a
/// screenshot it can act on and a picture it can only describe — after an auto-fit pass, image
/// coordinates are no longer display coordinates, and nothing else in the response says so.
fn describe_capture(
    id: &str,
    display_size: (u32, u32),
    region: Option<Region>,
    sent: (u32, u32),
) -> String {
    let (dw, dh) = display_size;
    let (sw, sh) = sent;
    let mut text = format!("display {id}, {dw}x{dh}");

    let (origin_x, origin_y, captured_w, captured_h) = match region {
        Some(r) => (r.x, r.y, r.width, r.height),
        None => (0, 0, dw, dh),
    };
    if region.is_some() {
        text.push_str(&format!(
            "; region {captured_w}x{captured_h} at ({origin_x}, {origin_y})"
        ));
    }

    if (sw, sh) != (captured_w, captured_h) {
        let scale = f64::from(captured_w) / f64::from(sw.max(1));
        text.push_str(&format!(
            "; scaled down to {sw}x{sh} — multiply image coordinates by {scale:.3}"
        ));
    }
    if origin_x != 0 || origin_y != 0 {
        text.push_str(&format!(", then add ({origin_x}, {origin_y})"));
    }
    if (sw, sh) != (captured_w, captured_h) || origin_x != 0 || origin_y != 0 {
        text.push_str(" to get display coordinates");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Incompressible by construction: a gradient would PNG down to nothing and prove nothing
    /// about the fit loop. The generator is a plain LCG so the test is deterministic.
    fn noise(width: u32, height: u32) -> RgbaImage {
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        RgbaImage::from_fn(width, height, |_, _| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let b = (seed >> 33) as u32;
            image::Rgba([b as u8, (b >> 8) as u8, (b >> 16) as u8, 255])
        })
    }

    #[test]
    fn a_small_image_is_left_alone() {
        let image = noise(64, 48);
        let (bytes, w, h) = fit(image, ImageFormat::Png, 80, Some(64 * 1024)).unwrap();
        assert_eq!((w, h), (64, 48));
        assert!(bytes.len() <= 64 * 1024);
    }

    #[test]
    fn an_oversized_image_is_shrunk_until_it_fits() {
        // Noise is the worst case a real screen can present — a 1600x1200 frame of it PNGs to
        // several megabytes — so the budget has to be one that is actually reachable above
        // `MIN_AUTO_WIDTH`. Anything below that is the floor test's job.
        let budget = 1024 * 1024;
        let (bytes, w, h) = fit(noise(1600, 1200), ImageFormat::Png, 80, Some(budget)).unwrap();
        assert!(bytes.len() <= budget, "{} bytes over {budget}", bytes.len());
        assert!(
            (MIN_AUTO_WIDTH..1600).contains(&w),
            "expected a downscale that converged, got {w}x{h}"
        );
        // Aspect ratio survives.
        assert_eq!(h, (u64::from(w) * 1200 / 1600) as u32);
    }

    #[test]
    fn jpeg_fits_where_png_would_bottom_out() {
        let budget = 128 * 1024;
        let (bytes, w, _) = fit(noise(1600, 1200), ImageFormat::Jpeg, 80, Some(budget)).unwrap();
        assert!(bytes.len() <= budget, "{} bytes over {budget}", bytes.len());
        assert!(w >= MIN_AUTO_WIDTH);
    }

    #[test]
    fn shrinking_stops_at_the_minimum_width() {
        // No budget this small is reachable, so the loop must give up rather than spin or
        // produce a one-pixel image.
        let (bytes, w, _) = fit(noise(1600, 1200), ImageFormat::Png, 80, Some(1)).unwrap();
        assert_eq!(w, MIN_AUTO_WIDTH);
        assert!(bytes.len() > 1);
    }

    #[test]
    fn no_budget_means_no_shrinking() {
        let (_, w, h) = fit(noise(1600, 1200), ImageFormat::Png, 80, None).unwrap();
        assert_eq!((w, h), (1600, 1200));
    }

    #[test]
    fn a_full_unscaled_capture_describes_only_the_display() {
        assert_eq!(
            describe_capture("DP-1", (1920, 1080), None, (1920, 1080)),
            "display DP-1, 1920x1080"
        );
    }

    #[test]
    fn a_scaled_capture_carries_the_coordinate_multiplier() {
        assert_eq!(
            describe_capture("DP-1", (1920, 1080), None, (960, 540)),
            "display DP-1, 1920x1080; scaled down to 960x540 \
             — multiply image coordinates by 2.000 to get display coordinates"
        );
    }

    #[test]
    fn a_scaled_region_carries_the_offset_too() {
        let region = Region { x: 100, y: 200, width: 800, height: 600 };
        assert_eq!(
            describe_capture("DP-1", (1920, 1080), Some(region), (400, 300)),
            "display DP-1, 1920x1080; region 800x600 at (100, 200); scaled down to 400x300 \
             — multiply image coordinates by 2.000, then add (100, 200) to get display coordinates"
        );
    }
}

#[zyris::async_trait]
impl ScreenCapture for HostScreenCapture {
    async fn list_displays(&self) -> zyris::Result<Vec<Display>> {
        let backend = self.backend;
        blocking(move || displays(backend)).await
    }

    async fn screenshot(
        &self,
        display: Option<String>,
        region: Option<Region>,
        format: Option<ImageFormat>,
        max_width: Option<u32>,
    ) -> zyris::Result<Datum> {
        let backend = self.backend;
        let format = format.unwrap_or(self.default_format);
        let max_width = max_width.or(self.default_max_width);
        let jpeg_quality = self.jpeg_quality;
        let budget = self.budget;

        blocking(move || {
            let (id, full) = capture(backend, display.as_deref())?;
            let display_size = full.dimensions();

            let image = downscale(crop(full, region)?, max_width);
            let (bytes, width, height) = fit(image, format, jpeg_quality, budget)?;

            if budget.is_some_and(|budget| bytes.len() > budget) {
                // Only reachable at the floor: content that will not compress even at
                // `MIN_AUTO_WIDTH`. Shrinking further would send something unreadable, so send
                // the oversized one and say why.
                tracing::warn!(
                    display = %id,
                    bytes = bytes.len(),
                    budget,
                    width,
                    "screenshot is over budget at the minimum width; try format=jpeg"
                );
            }

            Ok(Datum::Image {
                name: format!("display-{id}.{}", format.extension()),
                description: Some(describe_capture(&id, display_size, region, (width, height))),
                media_type: format.media_type().to_string(),
                blob: Blob::from_bytes(bytes),
            })
        })
        .await
    }
}
