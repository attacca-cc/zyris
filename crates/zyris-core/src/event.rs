//! The seam between the core and anything watching it.
//!
//! The core publishes; the GUI, the tray and the log each subscribe. Nothing subscribing is a
//! normal state — a headless run has no subscribers at all — so publishing to nobody is not an
//! error, it just returns 0.

use tokio::sync::{broadcast, watch};

/// Something the core did. One variant per thing a watcher can act on, never one per log line.
///
/// Serialized as a tagged union so the UI can switch on `kind` without a decoder of its own, and
/// in camelCase because the other side of that wire is TypeScript.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CoreEvent {
    /// The core finished starting and is now running.
    Started,
    /// The core is stopping. Subscribers get this before the process goes away.
    ShuttingDown,
    /// No credential is stored. The window shows onboarding; a headless run can do nothing but
    /// say so, which is why this is an event rather than an error.
    NeedsEnrolment,
    /// Show these to the person and wait. The code expires; a fresh one replaces it with another
    /// event of this kind.
    #[serde(rename_all = "camelCase")]
    EnrolmentCode { user_code: String, verification_uri: String },
    /// Enrolment ended without a credential — declined, or the request could not be made.
    #[serde(rename_all = "camelCase")]
    EnrolmentFailed { reason: String },
    /// A dial is in flight. Also the state during every reconnect the link makes on its own.
    Connecting,
    /// The link is up. Published again on every reconnect, so a watcher that missed the first one
    /// still learns the node's identity.
    #[serde(rename_all = "camelCase")]
    Connected { node_id: String, node_name: String },
    /// The link went down. The library reconnects on its own; this is not a request to retry.
    #[serde(rename_all = "camelCase")]
    Disconnected { reason: String },
}

/// A fan-out channel the core owns and everything else borrows.
///
/// Cloning it is cheap and gives the same underlying channel, so a `Clone` handed to a Tauri
/// state or a spawned task publishes to the same subscribers.
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<CoreEvent>,
    /// The most recently published event, kept beside the broadcast channel rather than only on
    /// it. `broadcast::Receiver::subscribe` only sees sends that happen after it is created, so a
    /// subscriber that starts listening late — the window, whose JS `listen()` call has to
    /// round-trip over IPC before it is registered — has otherwise already missed everything.
    /// `watch` keeps just the newest value, which is exactly what a latecomer needs to catch up:
    /// see `EventBus::latest`.
    latest: watch::Sender<Option<CoreEvent>>,
}

impl EventBus {
    /// `capacity` is how many events a slow subscriber may fall behind before it starts losing
    /// the oldest ones. Losing them is the intended behaviour: a watcher that cannot keep up
    /// must not be able to stall the core.
    pub fn new(capacity: usize) -> EventBus {
        let (tx, _rx) = broadcast::channel(capacity);
        let (latest, _rx) = watch::channel(None);
        EventBus { tx, latest }
    }

    /// Returns how many subscribers received it. **Zero is a normal answer** — a headless run
    /// has nobody watching — so this deliberately does not return a `Result`.
    pub fn publish(&self, event: CoreEvent) -> usize {
        // Recorded before the broadcast send so `latest()` never answers with something older
        // than what a concurrent `subscribe()` might already be about to receive.
        // `send_replace`, not `send`: `watch::Sender::send` silently no-ops when there are zero
        // receivers, and this channel is never subscribed to — only `borrow`ed through
        // `latest()` — so it would always have zero. `send_replace` updates the stored value
        // unconditionally, which is the one thing this channel exists for.
        self.latest.send_replace(Some(event.clone()));
        self.tx.send(event).unwrap_or(0)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CoreEvent> {
        self.tx.subscribe()
    }

    /// The last event published, or `None` if nothing has been published yet. What a subscriber
    /// that started listening late asks for once, to catch up on whatever it missed.
    pub fn latest(&self) -> Option<CoreEvent> {
        self.latest.borrow().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn subscriber_receives_a_published_event() {
        let bus = EventBus::new(8);
        let mut rx = bus.subscribe();

        bus.publish(CoreEvent::Started);

        assert_eq!(rx.recv().await.unwrap(), CoreEvent::Started);
    }

    #[tokio::test]
    async fn every_subscriber_receives_the_same_event() {
        let bus = EventBus::new(8);
        let mut first = bus.subscribe();
        let mut second = bus.subscribe();

        assert_eq!(bus.publish(CoreEvent::ShuttingDown), 2);

        assert_eq!(first.recv().await.unwrap(), CoreEvent::ShuttingDown);
        assert_eq!(second.recv().await.unwrap(), CoreEvent::ShuttingDown);
    }

    #[test]
    fn publishing_with_no_subscribers_is_not_an_error() {
        let bus = EventBus::new(8);

        assert_eq!(bus.publish(CoreEvent::Started), 0);
    }

    #[test]
    fn latest_is_none_before_anything_is_published() {
        let bus = EventBus::new(8);

        assert_eq!(bus.latest(), None);
    }

    #[test]
    fn latest_holds_the_most_recent_event_even_with_no_subscribers() {
        let bus = EventBus::new(8);

        bus.publish(CoreEvent::Started);
        assert_eq!(bus.latest(), Some(CoreEvent::Started));

        bus.publish(CoreEvent::NeedsEnrolment);
        assert_eq!(
            bus.latest(),
            Some(CoreEvent::NeedsEnrolment),
            "latest must track the newest publish, not just the first"
        );
    }

    #[test]
    fn a_late_subscriber_can_still_read_latest() {
        // The whole point: a subscriber created after the publish still sees what it missed,
        // which `subscribe()` alone cannot give it — `broadcast` never replays a send.
        let bus = EventBus::new(8);
        bus.publish(CoreEvent::Connecting);

        let _late = bus.subscribe();

        assert_eq!(bus.latest(), Some(CoreEvent::Connecting));
    }

    #[test]
    fn events_serialize_as_a_tagged_union_the_ui_can_switch_on() {
        let json = serde_json::to_string(&CoreEvent::Connected {
            node_id: "n_1".into(),
            node_name: "laptop".into(),
        })
        .unwrap();

        assert_eq!(json, r#"{"kind":"connected","nodeId":"n_1","nodeName":"laptop"}"#);
    }

    #[test]
    fn a_unit_event_still_carries_its_kind() {
        let json = serde_json::to_string(&CoreEvent::NeedsEnrolment).unwrap();

        assert_eq!(json, r#"{"kind":"needsEnrolment"}"#);
    }

    #[test]
    fn the_enrolment_code_survives_the_round_trip() {
        let event = CoreEvent::EnrolmentCode {
            user_code: "WXQR-7KBD".into(),
            verification_uri: "https://attacca.cc/settings/zyris/device".into(),
        };

        let json = serde_json::to_string(&event).unwrap();

        assert_eq!(serde_json::from_str::<CoreEvent>(&json).unwrap(), event);
    }
}
