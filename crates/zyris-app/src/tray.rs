//! The tray icon: the only surface this app has while its window is closed.
//!
//! It carries just what cannot wait for the window to open. Anything that needs explaining, or
//! that has more than two states, belongs in the window instead.

use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager};

/// Brings the window back, whether it was hidden or merely behind something.
pub fn show_main_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        tracing::warn!("no main window to show");
        return;
    };
    let _ = window.show();
    let _ = window.unminimize();
    let _ = window.set_focus();
}

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open Zyris", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;

    TrayIconBuilder::with_id("main")
        .icon(
            app.default_window_icon()
                .expect("tauri.conf.json declares a bundle icon")
                .clone(),
        )
        .tooltip("Zyris")
        .menu(&menu)
        // Left click opens the window; the menu is the right-click surface. Showing the menu on
        // both leaves no way to open the window with one click.
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main_window(app),
            // The only path that actually ends the process. Everything else is prevented in
            // `gui::run`, which is what makes closing the window safe.
            "quit" => app.exit(0),
            other => tracing::warn!(id = other, "unknown tray menu item"),
        })
        .build(app)?;

    Ok(())
}
