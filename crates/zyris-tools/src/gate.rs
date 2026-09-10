//! Whether tools may run at all.
//!
//! One switch in front of every capability, rather than a flag each one checks: a capability
//! that forgot the check would be a hole nobody could see from the outside, and there will be
//! more capabilities than there are people reading them.
//!
//! # What the switch does not cover
//!
//! [`Gate::check`] runs once, inside `dispatch`. **The switch means "no new calls" — say that,
//! in the code and in the UI, rather than letting a person read it as "nothing is running".**
//! Three specific gaps:
//!
//! - A call already in flight is not stopped.
//! - An already-open stream keeps delivering. `Outgoing::Stream`'s items are drained by the
//!   connection task *after* `dispatch` returned, so a `read_stream` or `open_stream` started
//!   while running keeps producing across `set_paused(true)`.
//! - `exec` has no default timeout — `timeout_ms` is `Option<u64>` and `None` means an unbounded
//!   wait on the child — so a command that never exits pins that dispatch indefinitely, and
//!   pausing will not touch it.
//!
//! The real primitive for "nothing is running" exists: `Node::capabilities()` gives a handle
//! whose `remove(name)` revokes in-flight calls and re-announces. It is deliberately not used
//! here — the announced capability list changing under a live agent needs its own thinking — but
//! it is what to reach for if this switch turns out to be too weak in practice.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use zyris::{ErrorCode, WireError};

/// Shared by every wrapped capability. Cloning gives another handle on the same switch.
#[derive(Clone)]
pub struct Gate {
    paused: Arc<AtomicBool>,
}

impl Gate {
    pub fn running() -> Gate {
        Gate { paused: Arc::new(AtomicBool::new(false)) }
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }

    /// What every wrapped tool calls before doing anything.
    ///
    /// The message matters: an agent that cannot tell "this machine is paused" from "this tool
    /// is broken" will retry the wrong thing, or give up on a machine that is merely waiting.
    ///
    /// `Relaxed` is right for the flag this reads: it is standalone, nothing else depends on
    /// ordering against it, and a call that lands in the same microsecond as a pause may
    /// honestly go either way.
    pub fn check(&self) -> Result<(), WireError> {
        if self.is_paused() {
            // `CapabilityUnavailable` is not retriable, which is what we want: a paused machine
            // may stay paused for hours, and an agent retrying on a timer is worse than one that
            // reports back to its human. `ErrorCode::Other("paused")` would be more precise but
            // is a bespoke code nothing else understands.
            return Err(WireError::new(
                ErrorCode::CapabilityUnavailable,
                "this machine is paused; its owner has stopped tools from running",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_gate_is_running() {
        // Autonomous is the default. The switch exists for a person who wants to stop things,
        // not as a lock they have to open first.
        assert!(!Gate::running().is_paused());
    }

    #[test]
    fn a_running_gate_lets_a_call_through() {
        assert!(Gate::running().check().is_ok());
    }

    #[test]
    fn a_paused_gate_refuses() {
        let gate = Gate::running();

        gate.set_paused(true);

        assert!(gate.check().is_err());
    }

    #[test]
    fn the_refusal_says_it_was_paused_rather_than_that_the_tool_failed() {
        let gate = Gate::running();
        gate.set_paused(true);

        let message = gate.check().unwrap_err().to_string();

        assert!(
            message.to_lowercase().contains("paused"),
            "an agent that reads this has to be able to tell a pause from a broken tool, got: {message}"
        );
    }

    #[test]
    fn resuming_lets_calls_through_again() {
        let gate = Gate::running();
        gate.set_paused(true);

        gate.set_paused(false);

        assert!(gate.check().is_ok());
    }

    #[test]
    fn a_clone_shares_the_switch() {
        // Every wrapped capability holds its own clone. If they did not share, pausing would
        // stop some tools and not others, which is worse than not pausing at all.
        let gate = Gate::running();
        let other = gate.clone();

        gate.set_paused(true);

        assert!(other.is_paused());
    }
}
