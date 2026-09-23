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
    /// Enrolment ended without a credential — declined, or the request could not be made. This is
    /// terminal: `Connector::run` has already returned, and only a restart tries again.
    #[serde(rename_all = "camelCase")]
    EnrolmentFailed { reason: String },
    /// A dial is in flight. Also the state during every reconnect the link makes on its own —
    /// published right after the `Disconnected` that reports the link going down, whenever that
    /// `Disconnected` says `retrying: true`.
    Connecting,
    /// The link is up. Published again on every reconnect, so a watcher that missed the first one
    /// still learns the node's identity.
    #[serde(rename_all = "camelCase")]
    Connected { node_id: String, node_name: String },
    /// The link went down.
    ///
    /// `retrying` tells apart two situations that look the same from a single event but are not:
    /// `true` means the *link* went down and is backing off to dial again on its own — nothing to
    /// do but wait, and a `Connecting` follows immediately. `false` means the *actor* has stopped
    /// — a refusal no retry can fix, or the link giving up for good after exhausting its own
    /// retries — and the only way back is to restart the process.
    #[serde(rename_all = "camelCase")]
    Disconnected { reason: String, retrying: bool },
    /// Something needed before this node could even attempt to connect failed, terminally: a
    /// stored secret could not be read, or this node could not be registered with Attacca.
    /// Kept apart from `Disconnected` because neither situation involves a link that ever came
    /// up — reusing that channel is what previously sent a storage failure to a "not connected"
    /// status screen. Like `EnrolmentFailed`, this is terminal and needs a restart.
    ///
    /// Published only when no connection has come up yet during the current run — which
    /// includes the automatic recovery a refused credential triggers (`connection.rs`'s
    /// `recover_from_refused_credential`), right up until it succeeds. The same kind of failure *after*
    /// a connection has been live — recovery hitting trouble on a redial that was permanently
    /// refused, say — is reported as `Disconnected { retrying: false }` instead: the person may
    /// already be on the status screen by then, and this event's onboarding screen would wrongly
    /// tell them their account needs reauthorizing. `Connector::report_setup_failure` is what
    /// keeps that split in one place.
    #[serde(rename_all = "camelCase")]
    SetupFailed { reason: String },
    /// The switch moved. Published on every change so the tray and the window agree without
    /// either of them asking.
    Paused { paused: bool },
    /// This machine is about to **send** a file to a peer nobody here has approved, and is
    /// waiting for a person to compare `fingerprint` against what that machine shows on its own
    /// screen. `id` names the question an answer has to name back.
    ///
    /// **Sending only.** Approving makes this computer willing to send to `label`; it is not a
    /// door on files arriving, and nothing on the receiving side consults it. `zyris-app`'s
    /// `confirm` module records why.
    ///
    /// Published through [`EventBus::publish_transient`], never [`EventBus::publish`], for a
    /// reason narrower than the tool call's: a question is state a late window *does* have to
    /// catch up on, but it is state that stops being true the moment it is answered, expires or
    /// its caller gives up. Left in the one-slot catch-up value it would outlive all three and
    /// hand a window that opened an hour later a live-looking Approve button reaching nobody —
    /// and it would displace the `Connected` that window needs to leave its starting screen.
    /// The catch-up path is a command that reads the waiting question itself, which answers
    /// `None` the instant there is not one.
    #[serde(rename_all = "camelCase")]
    NeedsPeerApproval { id: u64, label: String, fingerprint: String },
    /// A tool call happened. The window shows a tail of these; the durable record is the audit
    /// log on disk, which outlives the process.
    ///
    /// Published through [`EventBus::publish_transient`], never [`EventBus::publish`] — see that
    /// method for why this one must stay out of the catch-up slot.
    #[serde(rename_all = "camelCase")]
    ToolCall { capability: String, tool: String, detail: String, outcome: String },
    /// A local MCP server started being announced, or stopped.
    ///
    /// **This event exists because an agent cannot be told the difference and a person has to
    /// be.** On the wire there is one answer for a server somebody turned off and one whose
    /// process fell over: the capability is not announced. That is the right answer for an agent
    /// — there is nothing it could do differently — but it is the wrong answer for whoever is
    /// looking at the window, who can restart the one that crashed and should not be told to
    /// restart the one they switched off themselves. [`McpServerChange`] is that distinction, and
    /// this is the only place the core makes it.
    ///
    /// Published through [`EventBus::publish_transient`], for the same reason a tool call is.
    /// The catch-up slot holds one event and is the window's only way to learn what it missed
    /// while its listener was being registered; a server change sitting in it would displace the
    /// `Connected` a window needs to leave its starting screen. A window that opened afterwards
    /// asks for the whole list instead, exactly as it does for the switch and for a waiting peer
    /// question.
    #[serde(rename_all = "camelCase")]
    McpServer { server: String, change: McpServerChange },
}

/// What happened to one local MCP server, in the terms a person can act on.
///
/// Serialized internally tagged on `change`, so the UI switches on one field and every variant's
/// own detail travels beside it rather than in a free-text sentence nothing can read.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "change", rename_all = "camelCase")]
pub enum McpServerChange {
    /// It is running, and its tools are announced. Published when a server joins a node that is
    /// already up — not at startup, where the whole announcement goes out at once and there is
    /// nothing to report a change against.
    #[serde(rename_all = "camelCase")]
    Announced { capability: String, tools: usize },
    /// Withdrawn because somebody asked for it. The process was stopped on purpose.
    Disabled,
    /// Withdrawn because the process is gone and nobody asked for that. **The one a person is
    /// meant to act on**, and the whole reason this enum is not a boolean.
    Died,
    /// Asked to start, and would not. `reason` is what to check, in the words the failure used.
    Failed { reason: String },
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

    /// Broadcasts without touching the catch-up slot. For events that are worth telling a live
    /// watcher about but are not state a latecomer has to be caught up on.
    ///
    /// The slot holds exactly one event, and `latest()` is the window's only way to learn what it
    /// missed while its listener was being registered. An event published far more often than the
    /// connection changes — a tool call — would own that slot almost all the time, and the window
    /// would catch up on something its reducer has no arm for and stay on its starting screen.
    /// Anything published this way is therefore lost to a subscriber that was not already
    /// listening, which is the trade: for tool calls the durable record is the audit log on disk.
    ///
    /// Returns how many subscribers received it; zero is a normal answer, as for [`Self::publish`].
    pub fn publish_transient(&self, event: CoreEvent) -> usize {
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

    #[test]
    fn started_and_shutting_down_pin_their_wire_shape() {
        assert_eq!(serde_json::to_string(&CoreEvent::Started).unwrap(), r#"{"kind":"started"}"#);
        assert_eq!(
            serde_json::to_string(&CoreEvent::ShuttingDown).unwrap(),
            r#"{"kind":"shuttingDown"}"#
        );
    }

    #[test]
    fn enrolment_failed_pins_its_wire_shape() {
        let json = serde_json::to_string(&CoreEvent::EnrolmentFailed {
            reason: "the request was declined".into(),
        })
        .unwrap();

        assert_eq!(json, r#"{"kind":"enrolmentFailed","reason":"the request was declined"}"#);
    }

    #[test]
    fn connecting_pins_its_wire_shape() {
        assert_eq!(serde_json::to_string(&CoreEvent::Connecting).unwrap(), r#"{"kind":"connecting"}"#);
    }

    #[test]
    fn disconnected_pins_its_wire_shape_including_retrying() {
        let json = serde_json::to_string(&CoreEvent::Disconnected {
            reason: "transport closed: eof".into(),
            retrying: true,
        })
        .unwrap();

        assert_eq!(
            json,
            r#"{"kind":"disconnected","reason":"transport closed: eof","retrying":true}"#
        );
    }

    #[test]
    fn setup_failed_pins_its_wire_shape() {
        let json = serde_json::to_string(&CoreEvent::SetupFailed {
            reason: "secret store: no keyring".into(),
        })
        .unwrap();

        assert_eq!(json, r#"{"kind":"setupFailed","reason":"secret store: no keyring"}"#);
    }

    #[test]
    fn paused_pins_its_wire_shape() {
        let json = serde_json::to_string(&CoreEvent::Paused { paused: true }).unwrap();

        assert_eq!(json, r#"{"kind":"paused","paused":true}"#);
    }

    #[test]
    fn a_peer_question_pins_its_wire_shape() {
        // The fingerprint crosses this wire on its way to a person who will compare it, group by
        // group, against another machine's screen. Pinned as a whole string, spaces and case
        // included: upstream renders 128 bits as eight space-separated groups of four uppercase
        // hex digits, and a serialization that trimmed, split or lowercased it would break that
        // comparison for a reason that has nothing to do with the keys.
        let json = serde_json::to_string(&CoreEvent::NeedsPeerApproval {
            id: 7,
            label: "kitchen-pi".into(),
            fingerprint: "9F2A 41C7 0E83 BB15 6D04 A97E 22C1 5FB8".into(),
        })
        .unwrap();

        assert_eq!(
            json,
            r#"{"kind":"needsPeerApproval","id":7,"label":"kitchen-pi","fingerprint":"9F2A 41C7 0E83 BB15 6D04 A97E 22C1 5FB8"}"#
        );
    }

    #[test]
    fn a_peer_question_stays_out_of_the_catch_up_slot() {
        // The one thing this event must not do. A question answered a minute ago left sitting in
        // the slot is handed to every window that opens afterwards, which would draw an Approve
        // button for a send that has long since finished — and it would push out the `Connected`
        // that window reads to leave its starting screen. The catch-up path for a question is
        // `pending_peer`, which reads the waiting question itself and answers `None` when there
        // is none.
        let bus = EventBus::new(8);
        let connected = CoreEvent::Connected { node_id: "n_1".into(), node_name: "laptop".into() };
        bus.publish(connected.clone());

        bus.publish_transient(CoreEvent::NeedsPeerApproval {
            id: 1,
            label: "kitchen-pi".into(),
            fingerprint: "9F2A 41C7 0E83 BB15 6D04 A97E 22C1 5FB8".into(),
        });

        assert_eq!(
            bus.latest(),
            Some(connected),
            "a question in the catch-up slot outlives the question itself"
        );
    }

    #[test]
    fn a_tool_call_pins_its_wire_shape() {
        let json = serde_json::to_string(&CoreEvent::ToolCall {
            capability: "terminal".into(),
            tool: "exec".into(),
            detail: "ls -la".into(),
            outcome: "allowed".into(),
        })
        .unwrap();

        assert_eq!(
            json,
            r#"{"kind":"toolCall","capability":"terminal","tool":"exec","detail":"ls -la","outcome":"allowed"}"#
        );
    }

    #[test]
    fn a_transient_publish_leaves_the_catch_up_slot_alone() {
        // The window's one-shot `latest_event` is the only thing closing the gap between the core
        // publishing and the webview's listener being registered. Tool calls arrive far more often
        // than connection events, so if they shared that slot the window would almost always catch
        // up on a `toolCall` — which its reducer ignores — and sit on "Starting." for good.
        let bus = EventBus::new(8);
        let connected = CoreEvent::Connected { node_id: "n_1".into(), node_name: "laptop".into() };
        bus.publish(connected.clone());

        bus.publish_transient(CoreEvent::ToolCall {
            capability: "terminal".into(),
            tool: "exec".into(),
            detail: "ls -la".into(),
            outcome: "allowed".into(),
        });

        assert_eq!(
            bus.latest(),
            Some(connected),
            "a transient publish must leave the catch-up slot holding the last real state"
        );
    }

    #[test]
    fn a_server_change_pins_its_wire_shape() {
        // The one event whose whole purpose is a distinction, so the distinction is what is
        // pinned: four shapes, each naming itself, and the two withdrawals spelled differently.
        let announced = serde_json::to_string(&CoreEvent::McpServer {
            server: "desk-notes".into(),
            change: McpServerChange::Announced { capability: "mcp_desk-notes".into(), tools: 4 },
        })
        .unwrap();
        assert_eq!(
            announced,
            r#"{"kind":"mcpServer","server":"desk-notes","change":{"change":"announced","capability":"mcp_desk-notes","tools":4}}"#
        );

        let disabled = serde_json::to_string(&CoreEvent::McpServer {
            server: "desk-notes".into(),
            change: McpServerChange::Disabled,
        })
        .unwrap();
        let died = serde_json::to_string(&CoreEvent::McpServer {
            server: "desk-notes".into(),
            change: McpServerChange::Died,
        })
        .unwrap();
        assert_eq!(
            disabled,
            r#"{"kind":"mcpServer","server":"desk-notes","change":{"change":"disabled"}}"#
        );
        assert_eq!(
            died,
            r#"{"kind":"mcpServer","server":"desk-notes","change":{"change":"died"}}"#
        );
        assert_ne!(
            disabled, died,
            "a server a person turned off and one that fell over must not look the same"
        );

        let failed = serde_json::to_string(&CoreEvent::McpServer {
            server: "desk-notes".into(),
            change: McpServerChange::Failed { reason: "no such command".into() },
        })
        .unwrap();
        assert_eq!(
            failed,
            r#"{"kind":"mcpServer","server":"desk-notes","change":{"change":"failed","reason":"no such command"}}"#
        );
    }

    #[test]
    fn a_server_change_stays_out_of_the_catch_up_slot() {
        // The slot holds one event, and it is what a window folds in to learn which screen to
        // render. A server that fell over an hour ago sitting in it would push out the
        // `Connected` a window needs to leave its starting screen. The catch-up route for this is
        // the command that lists the servers.
        let bus = EventBus::new(8);
        let connected = CoreEvent::Connected { node_id: "n_1".into(), node_name: "laptop".into() };
        bus.publish(connected.clone());

        bus.publish_transient(CoreEvent::McpServer {
            server: "desk-notes".into(),
            change: McpServerChange::Died,
        });

        assert_eq!(bus.latest(), Some(connected));
    }
}
