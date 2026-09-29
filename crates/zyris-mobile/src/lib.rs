//! The Android half of the phone app: a Kotlin plugin (`android/`) for what only the platform can
//! do, and this handle Rust calls it through.
//!
//! - **The screen**, through MediaProjection. Android asks the person once per session; the first
//!   screenshot shows that prompt.
//! - **Touch and keys**, through an accessibility service the person turns on in the system
//!   settings. Nothing else may inject input into other apps.
//! - **Staying connected**, through a foreground service with a notification, so the connection
//!   survives the app leaving the screen.
//! - **Installing an update**, by handing a downloaded APK to the system installer.
//!
//! Every call blocks until Kotlin answers, so it is made off the async runtime.

#[cfg(target_os = "android")]
use serde::{Serialize, de::DeserializeOwned};
#[cfg(target_os = "android")]
use tauri::Manager;
use tauri::Runtime;
use tauri::plugin::{Builder, TauriPlugin};

#[cfg(target_os = "android")]
pub struct Phone<R: Runtime>(tauri::plugin::PluginHandle<R>);

#[cfg(target_os = "android")]
impl<R: Runtime> Phone<R> {
    /// Run one Kotlin command and read its answer.
    pub fn call<T: DeserializeOwned>(&self, command: &str, args: impl Serialize) -> Result<T, String> {
        self.0.run_mobile_plugin(command, args).map_err(|error| error.to_string())
    }
}

#[cfg(target_os = "android")]
pub trait PhoneExt<R: Runtime> {
    fn phone(&self) -> &Phone<R>;
}

#[cfg(target_os = "android")]
impl<R: Runtime, M: Manager<R>> PhoneExt<R> for M {
    fn phone(&self) -> &Phone<R> {
        self.state::<Phone<R>>().inner()
    }
}

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("zyris-mobile")
        .setup(|_app, _api| {
            #[cfg(target_os = "android")]
            _app.manage(Phone(_api.register_android_plugin("cc.attacca.zyris.mobile", "ZyrisPlugin")?));
            Ok(())
        })
        .build()
}
