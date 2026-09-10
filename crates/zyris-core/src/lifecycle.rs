//! The one place that says what "the core is running" and "the core is stopping" mean.
//!
//! Both `zyris-app` runtimes — headless and windowed — call these two functions instead of
//! publishing the events themselves, so the claim that nothing which matters lives only in one
//! runtime is true in code, not just in a comment. Step 2 gets a single place to add "connect on
//! start, clean up on stop".

use crate::event::{CoreEvent, EventBus};

/// Marks the core as started: publishes [`CoreEvent::Started`] and logs it.
///
/// Call this only once a subscriber that needed to see it could already exist — publishing
/// earlier is a silent no-op, since `broadcast` never replays a send to a later subscriber.
pub fn start(bus: &EventBus) {
    bus.publish(CoreEvent::Started);
    tracing::info!("core started");
}

/// Marks the core as stopping: publishes [`CoreEvent::ShuttingDown`] and logs it.
pub fn shutdown(bus: &EventBus) {
    bus.publish(CoreEvent::ShuttingDown);
    tracing::info!("core stopping");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_publishes_started() {
        let bus = EventBus::new(8);
        let mut rx = bus.subscribe();

        start(&bus);

        assert_eq!(rx.try_recv().unwrap(), CoreEvent::Started);
    }

    #[test]
    fn shutdown_publishes_shutting_down() {
        let bus = EventBus::new(8);
        let mut rx = bus.subscribe();

        shutdown(&bus);

        assert_eq!(rx.try_recv().unwrap(), CoreEvent::ShuttingDown);
    }
}
