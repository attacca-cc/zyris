//! What an Android phone offers its agent: its screen, touch and its files, served under the same
//! capability names a desktop announces (`screen_capture`, `input`, `file_io`), through the Kotlin
//! half in `tauri-plugin-zyris-mobile`.
//!
//! The desktop's input vocabulary is a pointer: `move_to`, then `click`. A phone has no pointer,
//! so `move_to` remembers where, `click` taps there and `scroll` swipes from there. Coordinates
//! are the screen's pixels, the same space `list_displays` reports.

use std::sync::{Arc, Mutex};

use base64::Engine;
use serde::Deserialize;
use serde_json::json;
use tauri::{AppHandle, Wry};
use tauri_plugin_zyris_mobile::PhoneExt;
use zyris::caps::{Display, ImageFormat, Input, InputServer, MouseButton, Region, ScreenCapture, ScreenCaptureServer};
use zyris::proto::{Blob, Datum, INLINE_BLOB_MAX};
use zyris::{ErrorCode, ServeCapability, WireError};

/// The id `list_displays` gives the phone's one screen.
const SCREEN: &str = "phone";

/// Run a Kotlin command off the async runtime: every one blocks until the phone answers.
async fn call<T: for<'de> Deserialize<'de> + Send + 'static>(
    app: &AppHandle,
    command: &'static str,
    args: serde_json::Value,
) -> zyris::Result<T> {
    let app = app.clone();
    tokio::task::spawn_blocking(move || app.phone().call::<T>(command, args))
        .await
        .map_err(|error| WireError::new(ErrorCode::Internal, error.to_string()))?
        .map_err(|reason| WireError::new(ErrorCode::CapabilityUnavailable, reason))
}

/// A command whose answer is only that it worked.
async fn act(app: &AppHandle, command: &'static str, args: serde_json::Value) -> zyris::Result<()> {
    call::<serde_json::Value>(app, command, args).await.map(drop)
}

#[derive(Deserialize)]
struct Size {
    width: u32,
    height: u32,
    scale: f32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Shot {
    data: String,
    width: u32,
    height: u32,
    source_width: u32,
    source_height: u32,
}

pub struct PhoneScreen(AppHandle);

#[zyris::async_trait]
impl ScreenCapture for PhoneScreen {
    async fn list_displays(&self) -> zyris::Result<Vec<Display>> {
        let size: Size = call(&self.0, "display", json!({})).await?;
        Ok(vec![Display {
            id: SCREEN.to_string(),
            name: "Phone screen".to_string(),
            x: 0,
            y: 0,
            width: size.width,
            height: size.height,
            scale_factor: size.scale,
            primary: true,
        }])
    }

    async fn screenshot(
        &self,
        _display: Option<String>,
        region: Option<Region>,
        format: Option<ImageFormat>,
        max_width: Option<u32>,
    ) -> zyris::Result<Datum> {
        if region.is_some() {
            return Err(WireError::invalid_params("a phone takes the whole screen; leave out the region"));
        }
        let mut format = format.unwrap_or(ImageFormat::Png);
        let mut shot: Shot = call(&self.0, "screenshot", shot_args(format, max_width)).await?;
        // A PNG of a busy screen can pass what one message carries; JPEG always fits.
        if shot.data.len() / 4 * 3 > INLINE_BLOB_MAX && matches!(format, ImageFormat::Png) {
            format = ImageFormat::Jpeg;
            shot = call(&self.0, "screenshot", shot_args(format, max_width)).await?;
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(shot.data)
            .map_err(|error| WireError::new(ErrorCode::Internal, error.to_string()))?;
        let mut description = format!("display {SCREEN}, {}x{}", shot.source_width, shot.source_height);
        if shot.width != shot.source_width {
            let scale = f64::from(shot.source_width) / f64::from(shot.width.max(1));
            description.push_str(&format!(
                "; scaled down to {}x{} — multiply image coordinates by {scale:.3} to get display coordinates",
                shot.width, shot.height
            ));
        }
        Ok(Datum::Image {
            name: format!("display-{SCREEN}.{}", format.extension()),
            description: Some(description),
            media_type: format.media_type().to_string(),
            blob: Blob::from_bytes(bytes),
        })
    }
}

fn shot_args(format: ImageFormat, max_width: Option<u32>) -> serde_json::Value {
    json!({ "maxWidth": max_width, "jpeg": matches!(format, ImageFormat::Jpeg) })
}

pub struct PhoneInput {
    app: AppHandle,
    /// Where the last `move_to` pointed: what `click` taps and `scroll` swipes from.
    at: Mutex<(f32, f32)>,
}

#[zyris::async_trait]
impl Input for PhoneInput {
    async fn type_text(&self, text: String) -> zyris::Result<()> {
        act(&self.app, "typeText", json!({ "text": text })).await
    }

    async fn key(&self, chord: String) -> zyris::Result<()> {
        act(&self.app, "key", json!({ "name": chord })).await
    }

    async fn move_to(&self, _display: String, x: i32, y: i32) -> zyris::Result<()> {
        *self.at.lock().expect("not poisoned") = (x as f32, y as f32);
        Ok(())
    }

    async fn click(&self, _button: MouseButton) -> zyris::Result<()> {
        let (x, y) = *self.at.lock().expect("not poisoned");
        act(&self.app, "tap", json!({ "x": x, "y": y })).await
    }

    /// A swipe from the pointer, the way a finger scrolls: content moves with it, so scrolling
    /// down (positive `dy`) drags upwards.
    async fn scroll(&self, dx: i32, dy: i32) -> zyris::Result<()> {
        let (x, y) = *self.at.lock().expect("not poisoned");
        let args = json!({ "x1": x, "y1": y, "x2": x - dx as f32, "y2": y - dy as f32, "durationMs": 300 });
        act(&self.app, "swipe", args).await
    }
}

/// Everything the phone announces. Files start in the app's own directory; with "All files
/// access" granted, absolute paths under the shared storage work too.
pub fn capabilities(app: &AppHandle<Wry>, data: std::path::PathBuf) -> Vec<Arc<dyn ServeCapability>> {
    vec![
        Arc::new(zyris::caps::FileIoServer(zyris_fs::LocalFileIo::rooted(data))),
        Arc::new(ScreenCaptureServer(PhoneScreen(app.clone()))),
        Arc::new(InputServer(PhoneInput { app: app.clone(), at: Mutex::new((0.0, 0.0)) })),
    ]
}

// ---- Updates ------------------------------------------------------------------------------------

/// The newest release on GitHub, if it is newer than this app and carries an APK.
pub struct Release {
    pub version: String,
    pub notes: Option<String>,
    apk: String,
}

const LATEST: &str = "https://api.github.com/repos/attacca-cc/zyris/releases/latest";

fn http() -> Result<reqwest::Client, String> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder().user_agent("Zyris").build().map_err(|error| error.to_string())
}

/// `0.2.0` → `(0, 2, 0)`; anything after a `-` is ignored, which puts a pre-release beside its
/// release rather than above it.
fn numbers(version: &str) -> Vec<u64> {
    let version = version.trim_start_matches('v');
    let version = version.split('-').next().unwrap_or(version);
    version.split('.').map(|part| part.parse().unwrap_or(0)).collect()
}

pub async fn newer_release(current: &str) -> Result<Option<Release>, String> {
    #[derive(Deserialize)]
    struct Asset {
        name: String,
        browser_download_url: String,
    }
    #[derive(Deserialize)]
    struct Latest {
        tag_name: String,
        body: Option<String>,
        assets: Vec<Asset>,
    }
    let latest: Latest = http()?
        .get(LATEST)
        .send()
        .await
        .and_then(|response| response.error_for_status())
        .map_err(|error| error.to_string())?
        .json()
        .await
        .map_err(|error| error.to_string())?;
    if numbers(&latest.tag_name) <= numbers(current) {
        return Ok(None);
    }
    let Some(apk) = latest.assets.into_iter().find(|asset| asset.name.ends_with("_arm64.apk")) else {
        return Ok(None);
    };
    Ok(Some(Release {
        version: latest.tag_name.trim_start_matches('v').to_string(),
        notes: latest.body,
        apk: apk.browser_download_url,
    }))
}

/// Download the APK into the directory the FileProvider shares, and hand it to the installer.
/// Android checks that it is signed with the same key as this app before replacing it.
pub async fn install(app: &AppHandle, release: Release, cache: std::path::PathBuf) -> Result<(), String> {
    let bytes = http()?
        .get(&release.apk)
        .send()
        .await
        .and_then(|response| response.error_for_status())
        .map_err(|error| error.to_string())?
        .bytes()
        .await
        .map_err(|error| error.to_string())?;
    let dir = cache.join("updates");
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let file = dir.join("zyris-update.apk");
    std::fs::write(&file, &bytes).map_err(|error| error.to_string())?;
    act(app, "installApk", json!({ "path": file })).await.map_err(|error| error.message)
}

#[cfg(test)]
mod tests {
    use super::numbers;

    #[test]
    fn versions_compare_as_numbers() {
        assert!(numbers("v0.10.0") > numbers("0.9.9"));
        assert!(numbers("v0.2.0") > numbers("0.1.2"));
        assert!(numbers("0.2.0") <= numbers("0.2.0"));
        assert!(numbers("v0.2.0-rc.1") <= numbers("0.2.0"));
    }
}
