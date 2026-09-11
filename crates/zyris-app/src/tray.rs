//! The tray icon: the only surface this app has while its window is closed.
//!
//! It carries just what cannot wait for the window to open. Anything that needs explaining, or
//! that has more than two states, belongs in the window instead.

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};
use zyris_runtime::{CoreEvent, EventBus};
use zyris_tools::Gate;

use crate::bridge;

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

/// What the pause item says, which is the action it will perform rather than the state it is in.
///
/// A menu item is a verb — a person reads "Resume" and knows both that tools are stopped and
/// what clicking will do. Labelling it with the state instead ("Paused") says the first and
/// leaves the second to be guessed.
fn pause_label(paused: bool) -> &'static str {
    if paused { "Resume" } else { "Pause" }
}

pub fn build(app: &AppHandle, runtime: &tokio::runtime::Handle) -> tauri::Result<()> {
    // Read rather than assumed to be running: `Gate::running()` is what `main` builds today, but
    // a tray that hardcodes its starting label is a tray that lies the first time that changes.
    let gate = app.state::<Gate>();
    let open = MenuItem::with_id(app, "open", "Open Zyris", true, None::<&str>)?;
    let pause = MenuItem::with_id(app, "pause", pause_label(gate.is_paused()), true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &pause, &quit])?;

    // The label follows the switch rather than the click. `set_paused` from the Tools tab moves
    // the same gate, and a tray that only relabelled itself when its own item was clicked would
    // read "Pause" on a machine the window had already paused.
    //
    // `MenuItem::set_text` exists in Tauri 2.11 and takes `&self`, so nothing here has to rebuild
    // the menu; it dispatches the change to the main thread on its own, which is why this may run
    // on a tokio worker.
    let label = pause.clone();
    let resync = app.state::<Gate>().inner().clone();
    let mut events = app.state::<EventBus>().subscribe();
    runtime.spawn(async move {
        loop {
            match events.recv().await {
                Ok(CoreEvent::Paused { paused }) => {
                    if let Err(error) = label.set_text(pause_label(paused)) {
                        tracing::warn!(%error, "could not relabel the tray's pause item");
                    }
                }
                Ok(_) => {}
                // Tool calls can outrun a subscriber, and the bus drops the oldest when they
                // do — a contiguous range of *everything* in the ring, `Paused` included. The
                // label is written in exactly two places and never polled, so a dropped switch
                // would leave it stale for the rest of the run; and since the click handler
                // toggles against the gate, an item still reading "Pause" on a paused machine
                // would resume it. With the window closed this is the only surface, so re-read
                // the gate rather than trusting the stream.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::warn!(missed, "the tray fell behind; resyncing the label from the gate");
                    if let Err(error) = label.set_text(pause_label(resync.is_paused())) {
                        tracing::warn!(%error, "could not resync the tray's pause item");
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    TrayIconBuilder::with_id("main")
        .icon(
            app.default_window_icon()
                .expect("tauri.conf.json declares a bundle icon")
                .clone(),
        )
        .tooltip("Zyris")
        .menu(&menu)
        // On Windows and macOS this keeps the menu off the left click, so left click can open
        // the window instead. Tauri documents this as unsupported on Linux, where the menu may
        // appear on any click regardless — there, "Open Zyris" in the menu is the reliable path.
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main_window(app),
            // Toggled against the gate itself rather than against a copy kept here: the window
            // moves the same switch, and a second opinion about where it is would be one more
            // thing to keep in step. The relabelling happens above, off the published event.
            "pause" => {
                let gate = app.state::<Gate>();
                let bus = app.state::<EventBus>();
                bridge::apply_paused(&gate, &bus, !gate.is_paused());
            }
            // The only path that actually ends the process. Everything else is prevented in
            // `gui::run`, which is what makes closing the window safe.
            "quit" => app.exit(0),
            other => tracing::warn!(id = other, "unknown tray menu item"),
        })
        .build(app)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pause_item_names_the_action_rather_than_the_state() {
        // A person reading the menu has to learn both where the switch is and what clicking will
        // do. "Pause" on a running machine says both; "Running" would say only the first.
        assert_eq!(pause_label(false), "Pause");
        assert_eq!(pause_label(true), "Resume");
    }
}
