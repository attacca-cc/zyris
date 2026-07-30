use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{DynamicImage, ExtendedColorType, ImageEncoder, RgbaImage};
use xcap::{Monitor, XCapError};
use zyris::{Blob, Datum, ErrorCode, WireError};
use zyris_caps::{Display, ImageFormat, Region, ScreenCapture};

/// [`ScreenCapture`] backed by [`xcap`] — X11 and Wayland on Linux, Quartz on macOS, DXGI/GDI on
/// Windows.
///
/// The defaults set the ceiling a caller cannot exceed by forgetting to pass one. A node on a
/// metered link can pin every capture to a small JPEG:
///
/// ```no_run
/// # use zyris_capkit::XcapScreenCapture;
/// # use zyris_caps::{ImageFormat, ScreenCaptureServer};
/// let screen = XcapScreenCapture::default()
///     .with_format(ImageFormat::Jpeg)
///     .with_max_width(1280)
///     .with_jpeg_quality(70);
/// let server = ScreenCaptureServer(screen);
/// ```
///
/// Per-call `format` and `max_width` still win where they are given; the defaults only fill in the
/// gaps.
pub struct XcapScreenCapture {
    default_format: ImageFormat,
    default_max_width: Option<u32>,
    jpeg_quality: u8,
}

impl Default for XcapScreenCapture {
    fn default() -> Self {
        XcapScreenCapture {
            default_format: ImageFormat::Png,
            default_max_width: None,
            jpeg_quality: 80,
        }
    }
}

impl XcapScreenCapture {
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
}

fn internal(err: impl std::fmt::Display) -> WireError {
    WireError::new(ErrorCode::Internal, err.to_string())
}

/// `xcap` is entirely blocking and its `Monitor` handles are platform connections, so every call
/// enumerates, captures, and encodes inside one closure and hands back only the finished bytes.
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
            None => Err(WireError::invalid_params(format!(
                "no display matches `{wanted}`"
            ))),
        };
    }
    let index = monitors
        .iter()
        .position(|m| m.is_primary().unwrap_or(false))
        .unwrap_or(0);
    Ok((&monitors[index], index))
}

/// Capture the whole display and crop here rather than calling `Monitor::capture_region`.
///
/// `capture_region` is not the same operation on every platform: xcap adds the monitor's origin to
/// the coordinates on X11 but passes them through as virtual-desktop coordinates on Wayland, so the
/// same `Region` would name two different rectangles. Cropping the full capture costs a copy and
/// means one rectangle everywhere.
fn capture(monitor: &Monitor, region: Option<Region>) -> zyris::Result<RgbaImage> {
    let full = monitor.capture_image().map_err(|err| match err {
        XCapError::InvalidCaptureRegion(msg) => WireError::invalid_params(msg),
        other => internal(other),
    })?;
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

fn encode(image: RgbaImage, format: ImageFormat, jpeg_quality: u8) -> zyris::Result<Vec<u8>> {
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
        // JPEG has no alpha channel, and a screenshot's is opaque anyway.
        ImageFormat::Jpeg => {
            let rgb = DynamicImage::ImageRgba8(image).into_rgb8();
            JpegEncoder::new_with_quality(&mut out, jpeg_quality)
                .write_image(
                    rgb.as_raw(),
                    rgb.width(),
                    rgb.height(),
                    ExtendedColorType::Rgb8,
                )
                .map_err(internal)?
        }
    }
    Ok(out)
}

#[zyris::async_trait]
impl ScreenCapture for XcapScreenCapture {
    async fn list_displays(&self) -> zyris::Result<Vec<Display>> {
        blocking(|| {
            let monitors = Monitor::all().map_err(internal)?;
            Ok(monitors
                .iter()
                .enumerate()
                .map(|(index, monitor)| describe(monitor, index))
                .collect())
        })
        .await
    }

    async fn screenshot(
        &self,
        display: Option<String>,
        region: Option<Region>,
        format: Option<ImageFormat>,
        max_width: Option<u32>,
    ) -> zyris::Result<Datum> {
        let format = format.unwrap_or(self.default_format);
        let max_width = max_width.or(self.default_max_width);
        let jpeg_quality = self.jpeg_quality;

        blocking(move || {
            let monitors = Monitor::all().map_err(internal)?;
            if monitors.is_empty() {
                return Err(internal("no displays are attached"));
            }
            let (monitor, index) = select(&monitors, display.as_deref())?;
            let id = monitor_id(monitor, index);

            let image = downscale(capture(monitor, region)?, max_width);
            let bytes = encode(image, format, jpeg_quality)?;

            if bytes.len() > zyris::proto::INLINE_BLOB_MAX {
                tracing::warn!(
                    display = %id,
                    bytes = bytes.len(),
                    limit = zyris::proto::INLINE_BLOB_MAX,
                    "screenshot exceeds the inline blob limit; pass max_width or format=jpeg"
                );
            }

            Ok(Datum::Image {
                name: format!("display-{id}.{}", format.extension()),
                description: None,
                media_type: format.media_type().to_string(),
                blob: Blob::from_bytes(bytes),
            })
        })
        .await
    }
}
