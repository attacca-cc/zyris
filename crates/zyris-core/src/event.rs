//! The seam between the core and anything watching it.
//!
//! The core publishes; the GUI, the tray and the log each subscribe. Nothing subscribing is a
//! normal state — a headless run has no subscribers at all — so publishing to nobody is not an
//! error, it just returns 0.

use tokio::sync::broadcast;

/// Something the core did. One variant per thing a watcher can act on, never one per log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreEvent {
    /// The core finished starting and is now running.
    Started,
    /// The core is stopping. Subscribers get this before the process goes away.
    ShuttingDown,
}

/// A fan-out channel the core owns and everything else borrows.
///
/// Cloning it is cheap and gives the same underlying channel, so a `Clone` handed to a Tauri
/// state or a spawned task publishes to the same subscribers.
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<CoreEvent>,
}

impl EventBus {
    /// `capacity` is how many events a slow subscriber may fall behind before it starts losing
    /// the oldest ones. Losing them is the intended behaviour: a watcher that cannot keep up
    /// must not be able to stall the core.
    pub fn new(capacity: usize) -> EventBus {
        let (tx, _rx) = broadcast::channel(capacity);
        EventBus { tx }
    }

    /// Returns how many subscribers received it. **Zero is a normal answer** — a headless run
    /// has nobody watching — so this deliberately does not return a `Result`.
    pub fn publish(&self, event: CoreEvent) -> usize {
        self.tx.send(event).unwrap_or(0)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CoreEvent> {
        self.tx.subscribe()
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
}
