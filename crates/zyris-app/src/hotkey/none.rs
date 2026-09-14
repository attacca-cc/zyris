//! The answer for a desktop that cannot have a global hotkey.
//!
//! This is not a stub and it is not a placeholder. It is the fourth answer the plan asked for:
//! a machine where no application can register a global key — every desktop that falls back to
//! `xdg-desktop-portal-gtk` (XFCE, MATE, Cinnamon, LXQt) has no GlobalShortcuts interface at all,
//! and a headless run has nobody at a keyboard. Handing those a hotkey that silently never fires
//! is the failure mode this whole module exists to avoid.

use tokio::sync::broadcast;

use super::{Closing, Hotkey, HotkeyEvent, HotkeySupport, OnePerHold};

/// A hotkey that will never fire, and says so.
pub struct NoHotkey {
    reason: String,
    /// Kept only so [`Hotkey::events`] can hand out a real, live receiver rather than a closed
    /// one. A caller that subscribed to this and got an immediate `Closed` would read it as "the
    /// key went away", which is a different thing from "there was never a key"; the description
    /// is where the difference is said.
    events: OnePerHold,
}

impl NoHotkey {
    pub fn because(reason: impl Into<String>) -> NoHotkey {
        let reason = reason.into();
        // `info!`, not `warn!`, for the reason `announce.rs` gives about a machine with no
        // display server: a desktop that will never have a global shortcut is an ordinary
        // machine, not a fault, and a warning on every launch teaches people to ignore warnings.
        tracing::info!(%reason, "no push-to-talk key on this desktop");
        NoHotkey { reason, events: OnePerHold::new() }
    }
}

impl Hotkey for NoHotkey {
    fn describe(&self) -> HotkeySupport {
        HotkeySupport::Unavailable { reason: self.reason.clone() }
    }

    fn events(&self) -> broadcast::Receiver<HotkeyEvent> {
        self.events.subscribe()
    }

    fn close(&self) -> Closing<'_> {
        Box::pin(std::future::ready(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn it_says_why_and_then_never_fires() {
        let hotkey = NoHotkey::because("XFCE's portal has no GlobalShortcuts interface");
        assert_eq!(
            hotkey.describe(),
            HotkeySupport::Unavailable {
                reason: "XFCE's portal has no GlobalShortcuts interface".into()
            }
        );
        let mut events = hotkey.events();
        assert!(
            matches!(events.try_recv(), Err(broadcast::error::TryRecvError::Empty)),
            "an open subscription with nothing on it, not a closed one"
        );
        hotkey.close().await;
    }
}
