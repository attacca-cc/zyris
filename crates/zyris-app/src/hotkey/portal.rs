//! The backend for a Wayland session: the GlobalShortcuts portal, through `ashpd`.
//!
//! This is the only way to register a global key on Wayland. There is no X11 fallback — see the
//! module documentation on `hotkey/mod.rs` for why the one that appears to work does not — and
//! not every Wayland desktop has one: Hyprland and GNOME do, KDE is unverified, and everything
//! that falls back to `xdg-desktop-portal-gtk` (XFCE, MATE, Cinnamon, LXQt) implements no
//! GlobalShortcuts interface at all. [`PortalHotkey::open`] failing is that answer, and the
//! caller turns it into [`super::NoHotkey`].
//!
//! Three things about this portal, each measured against `xdg-desktop-portal-hyprland` 1.3.12 on
//! 2026-09-15 and each of which shapes the code below:
//!
//! - **`preferred_trigger` is ignored.** The returned `trigger_description` is empty and the
//!   strings are not in the portal binary. The shortcut registers with no key bound to it.
//! - **The interface is version 1 here, and `ConfigureShortcuts` needs version 2**, so this
//!   application cannot even open a dialog to let somebody assign the key. A line in their
//!   compositor configuration is the only route, which is what
//!   [`super::HotkeySupport::NeedsAKeyBound`] carries.
//! - **The registration outlives the process.** Killing it leaves the shortcut listed by
//!   `hyprctl globalshortcuts`, and so — measured, and not what was expected — does closing the
//!   session properly. See [`PortalHotkey::close`].

use std::sync::Arc;
use std::sync::Mutex;

use ashpd::desktop::Session;
use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
use futures_util::StreamExt;
use tokio::sync::{broadcast, oneshot};

use super::{
    Closing, Env, Hotkey, HotkeyEvent, HotkeySupport, OnePerHold, SHORTCUT_DESCRIPTION,
    SHORTCUT_ID, TRIGGER, compositor_line,
};

/// What this asks the portal for, in the portal's own spelling. Ignored by every implementation
/// measured so far; sent anyway, because one that honours it would save the user the line below.
const PREFERRED_TRIGGER: &str = "CTRL+ALT+space";

pub struct PortalHotkey {
    /// Answered from what the portal said at registration, once. Nothing re-asks: the compositor
    /// never tells the portal what key it bound, so this cannot become more accurate by being
    /// looked at again.
    support: HotkeySupport,
    events: Arc<OnePerHold>,
    session: Session<GlobalShortcuts>,
    /// Ends the listening task. `Option` because it is spent on the first
    /// [`Hotkey::close`]; `Mutex` because `close` takes `&self`, which is what a `dyn Hotkey`
    /// behind an `Arc` can offer.
    stop: Mutex<Option<oneshot::Sender<()>>>,
}

impl PortalHotkey {
    /// Register with the portal, and start listening.
    ///
    /// Must be called from inside a tokio runtime: the listening task is spawned here, because
    /// the two signal streams have to be subscribed **before** `BindShortcuts` returns. A signal
    /// that fired between the bind and a later subscribe would be lost, and the first thing a
    /// person does after adding the compositor line is press the key.
    pub async fn open(env: &Env) -> ashpd::Result<PortalHotkey> {
        let portal = GlobalShortcuts::new().await?;
        let version = portal.version();
        let session = portal.create_session(Default::default()).await?;

        let mut activated = portal.receive_activated().await?;
        let mut deactivated = portal.receive_deactivated().await?;

        let shortcut = NewShortcut::new(SHORTCUT_ID, SHORTCUT_DESCRIPTION)
            .preferred_trigger(PREFERRED_TRIGGER);
        let bound = portal
            .bind_shortcuts(&session, &[shortcut], None, Default::default())
            .await?
            .response()?;

        // Empty on every implementation measured so far, which is what puts this on the
        // `NeedsAKeyBound` branch. A portal that filled it in — version 2, or one that honours
        // the preferred trigger — lands on `Working` with no other change.
        let trigger = bound
            .shortcuts()
            .iter()
            .find(|shortcut| shortcut.id() == SHORTCUT_ID)
            .map(|shortcut| shortcut.trigger_description().to_string())
            .unwrap_or_default();

        let desktop = env.desktop_name();
        let support = if trigger.is_empty() {
            let line = compositor_line(&desktop, SHORTCUT_ID);
            HotkeySupport::NeedsAKeyBound {
                how: match &line {
                    Some(_) => format!(
                        "{desktop} does not let an application choose the key, so Zyris cannot \
                         bind {TRIGGER} for you. Add this line to your compositor configuration \
                         if you have not already, and reload it."
                    ),
                    None => format!(
                        "{desktop} does not let an application choose the key. Bind one to the \
                         global shortcut named {SHORTCUT_ID} in your desktop's keyboard \
                         settings; Zyris cannot tell whether you have."
                    ),
                },
                shortcut_id: SHORTCUT_ID.to_string(),
                desktop,
                line,
            }
        } else {
            // `false`: the portal's `Deactivated` has never been seen to arrive, and a trigger
            // being reported says nothing about it. See `HotkeySupport::Working`.
            HotkeySupport::Working { trigger, release_confirmed: false }
        };

        let events = Arc::new(OnePerHold::new());
        let published = events.clone();
        let (stop, mut stopped) = oneshot::channel();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut stopped => return,
                    // **Filtered by shortcut id, not by session.** `ashpd` subscribes to the
                    // signal on the portal's own object, and its `session_handle` accessor is
                    // the only way to tell one session's activation from another's — but
                    // `Session::path` is `pub(crate)`, so this end cannot name its own handle to
                    // compare against. The id is the filter that is reachable. In practice the
                    // portal directs these signals at the client that owns the session; the
                    // worst this can cost is a stray press from another application that chose
                    // the same string, which `OnePerHold` then turns into at most one hold.
                    Some(event) = activated.next() => {
                        if event.shortcut_id() == SHORTCUT_ID {
                            published.pressed();
                        }
                    }
                    Some(event) = deactivated.next() => {
                        if event.shortcut_id() == SHORTCUT_ID {
                            published.released();
                        }
                    }
                    else => return,
                }
            }
        });

        tracing::info!(
            version,
            shortcut = SHORTCUT_ID,
            bound = !matches!(support, HotkeySupport::NeedsAKeyBound { .. }),
            "registered the push-to-talk shortcut with the GlobalShortcuts portal"
        );
        Ok(PortalHotkey { support, events, session, stop: Mutex::new(Some(stop)) })
    }
}

impl Hotkey for PortalHotkey {
    fn describe(&self) -> HotkeySupport {
        self.support.clone()
    }

    fn events(&self) -> broadcast::Receiver<HotkeyEvent> {
        self.events.subscribe()
    }

    /// Stop listening and close the portal session.
    ///
    /// **This is why [`Hotkey::close`] exists**, and it is the one backend where the registration
    /// is not this process's to lose: killing Zyris leaves the shortcut listed by
    /// `hyprctl globalshortcuts`, held by the portal rather than by us.
    ///
    /// **Closing does not take it off that list either** — measured on
    /// `xdg-desktop-portal-hyprland` 1.3.12 with Hyprland 0.55.4 on 2026-09-15: `Close` returns
    /// `Ok`, and half a second, two and a half seconds, and a process exit later the id is still
    /// there. Dropping the proxy does not do it either. Only restarting the portal clears the
    /// list. What was also measured is the part that makes this bounded rather than a leak:
    /// **registering the same id again is idempotent** — a later run gets exactly one entry, the
    /// compositor's `bind` line goes on pointing at the same name, and the key works. So the cost
    /// is one stale line in that listing per distinct shortcut id, which is one, forever.
    ///
    /// Close is still called, because it is what the protocol asks for and because a portal that
    /// does honour it should get the chance to.
    fn close(&self) -> Closing<'_> {
        if let Some(stop) = self.stop.lock().expect("hotkey stop slot").take() {
            let _ = stop.send(());
        }
        Box::pin(async move {
            match self.session.close().await {
                Ok(()) => tracing::info!("closed the global shortcuts portal session"),
                Err(error) => {
                    tracing::warn!(%error, "could not close the global shortcuts portal session")
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole portal path, against whatever portal this machine is running.
    ///
    /// `#[ignore]` because it needs a live session bus with an `xdg-desktop-portal` on it: CI has
    /// neither, and a test that quietly passed on a machine with no portal would be worse than
    /// no test. Run it with `cargo test -p zyris-app -- --ignored the_portal_path`.
    ///
    /// It asserts the shape rather than a particular answer, because the answer is the desktop's:
    /// a portal that assigned a key lands on `Working`, and one that did not lands on
    /// `NeedsAKeyBound` naming the id a compositor has to point at. What it rules out is the
    /// third — a registration that succeeded and then described itself as unavailable.
    ///
    /// **It registers `push_to_talk` for real, and that registration outlives this process** —
    /// see [`PortalHotkey::close`]. Registering the same id again is idempotent, so running this
    /// repeatedly costs one line in `hyprctl globalshortcuts` and nothing more.
    #[tokio::test]
    #[ignore = "needs a live xdg-desktop-portal on the session bus"]
    async fn the_portal_path_registers_and_says_what_is_owed() {
        let env = Env::read();
        let hotkey = PortalHotkey::open(&env).await.expect("a GlobalShortcuts portal");
        match hotkey.describe() {
            HotkeySupport::Working { trigger, release_confirmed } => {
                assert!(!trigger.is_empty(), "a working hotkey names the key to press");
                assert!(
                    !release_confirmed,
                    "nobody has seen the portal deliver a key release; a backend claiming \
                     otherwise has to be measured first"
                );
            }
            HotkeySupport::NeedsAKeyBound { shortcut_id, how, .. } => {
                assert_eq!(shortcut_id, SHORTCUT_ID);
                assert!(!how.is_empty(), "there is always a sentence, even with no line");
            }
            HotkeySupport::Unavailable { reason } => {
                panic!("registered, then called itself unavailable: {reason}")
            }
        }
        // Nothing is expected on it — nobody pressed anything — but the subscription has to be
        // live rather than closed, which is what the listening task being alive means.
        let mut events = hotkey.events();
        assert!(matches!(events.try_recv(), Err(broadcast::error::TryRecvError::Empty)));
        hotkey.close().await;
    }
}
