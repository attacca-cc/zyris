//! Updating Zyris from inside Zyris: the window asks once at start whether a newer release is
//! published, and installs it when the person says so.
//!
//! What is fetched is `latest.json` on the newest GitHub release, which the release workflow
//! writes, and every download is checked against the public key in `tauri.conf.json` before it is
//! installed. Windows runs the NSIS installer, macOS swaps the `.app`, and Linux installs the new
//! `.deb` or `.rpm` through `pkexec`.
//!
//! **Not from the Nix store.** A package there is read-only and owned by the system's
//! configuration, and `dpkg` does not exist; that copy is updated with the flake or nixpkgs.

use serde::Serialize;
use tauri_plugin_updater::UpdaterExt;

#[derive(Serialize)]
pub struct Available {
    version: String,
    notes: Option<String>,
}

fn managed_elsewhere() -> bool {
    std::env::current_exe().is_ok_and(|exe| exe.starts_with("/nix/store"))
}

/// The newer release, if one is published. `None` also when this copy is not ours to update.
#[tauri::command]
pub async fn check_for_update(app: tauri::AppHandle) -> Result<Option<Available>, String> {
    if managed_elsewhere() {
        return Ok(None);
    }
    let update = app.updater().map_err(|e| e.to_string())?.check().await.map_err(|e| e.to_string())?;
    Ok(update.map(|update| Available { version: update.version, notes: update.body }))
}

/// Download, verify and install the newer release, then start it in place of this process.
#[tauri::command]
pub async fn install_update(app: tauri::AppHandle) -> Result<(), String> {
    let Some(update) = app.updater().map_err(|e| e.to_string())?.check().await.map_err(|e| e.to_string())?
    else {
        return Err("there is no newer release any more".to_string());
    };
    tracing::info!(version = %update.version, "installing an update");
    update.download_and_install(|_, _| {}, || {}).await.map_err(|e| e.to_string())?;
    app.restart()
}
