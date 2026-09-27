//! The one reader of the answer stream.
//!
//! **Why this exists.** The feed subscribes to the session on every connection whether or not a
//! microphone is open, but until this module the only thing that ever read what it published was
//! [`Speaking::run`] — which exists only while listening is on and a speaker opened. So an answer
//! to anything sent with listening off, or on a machine with no voice downloaded, was received and
//! thrown away, and the Conversation screen showed a question with no reply under it.
//!
//! Now this reads every [`TurnEvent`], for the life of the engine, and does two things with each:
//!
//! 1. **Traces what the window shows** — the text as it is written ([`Trace::Delta`]), the start
//!    and end of an answer ([`Trace::Answering`], [`Trace::Answered`]), and an agent run that
//!    failed.
//! 2. **Forwards what the speaker needs** to a [`Speaking`], when one is attached — on a channel
//!    of its own, so the speaker's `run` is unchanged in shape — and holds back anything sayable
//!    while reading aloud is switched off.
//!
//! **Trace first, then forward**, on every event. `Speaking` traces `Fragment` itself when it
//! receives one, so this order is what makes `Answering` reach the window before the answer's
//! first `Fragment` — which is how the window knows the fragment belongs to a new answer.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

use crate::session::Speaking;
use crate::turn::TurnEvent;
use crate::{Trace, VoiceEvent};

/// How far a speaker may fall behind this reader before it loses events. The same depth as the
/// feed's own channel: synthesis is slower than writing, and a long answer is hundreds of events.
const FORWARD_CAPACITY: usize = 512;

/// What reads the answers. Built once per engine; see the module comment.
pub struct Answers {
    traces: broadcast::Sender<Trace>,
    events: broadcast::Sender<VoiceEvent>,
    read_aloud: AtomicBool,
    speaker: Mutex<Option<Attached>>,
}

struct Attached {
    speaking: Arc<Speaking>,
    forward: broadcast::Sender<TurnEvent>,
}

impl Answers {
    /// A reader that traces onto `traces`, reports failures on `events`, and starts with reading
    /// aloud on or off as the settings say.
    pub fn new(
        traces: broadcast::Sender<Trace>,
        events: broadcast::Sender<VoiceEvent>,
        read_aloud: bool,
    ) -> Arc<Answers> {
        Arc::new(Answers {
            traces,
            events,
            read_aloud: AtomicBool::new(read_aloud),
            speaker: Mutex::new(None),
        })
    }

    /// Start forwarding to `speaking`. Answers what its `run` should read.
    ///
    /// A speaker attached earlier is let go: its channel closes and its `run` returns.
    pub fn attach(&self, speaking: Arc<Speaking>) -> broadcast::Receiver<TurnEvent> {
        let (forward, receiver) = broadcast::channel(FORWARD_CAPACITY);
        *self.lock() = Some(Attached { speaking, forward });
        receiver
    }

    /// Stop forwarding. The attached speaker's channel closes, so its `run` returns.
    pub fn detach(&self) {
        *self.lock() = None;
    }

    /// The attached speaker, if there is one.
    pub fn speaking(&self) -> Option<Arc<Speaking>> {
        self.lock().as_ref().map(|attached| attached.speaking.clone())
    }

    /// Switch reading aloud on or off. Answers what it was before.
    ///
    /// Only what is sayable from now on is affected; stopping what is already queued is the
    /// caller's decision (see `Speaking::hush`).
    pub fn set_read_aloud(&self, on: bool) -> bool {
        self.read_aloud.swap(on, Ordering::Relaxed)
    }

    /// Whether answers are being read aloud when a speaker is attached.
    pub fn read_aloud(&self) -> bool {
        self.read_aloud.load(Ordering::Relaxed)
    }

    /// Read `turns` until the feed goes away.
    pub async fn run(self: Arc<Self>, mut turns: broadcast::Receiver<TurnEvent>) {
        loop {
            match turns.recv().await {
                Ok(event) => self.handle(event),
                // What was lost is text nobody will see and nothing will replay: a `Delta` is not
                // durable. Said, rather than left as an answer with a hole in it.
                Err(broadcast::error::RecvError::Lagged(missed)) => self.failed(format!(
                    "the answer came faster than it could be shown and {missed} pieces of it \
                     were lost"
                )),
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    }

    fn handle(&self, event: TurnEvent) {
        match &event {
            // What the agent wrote, reasoning included — the window decides what to show. Never
            // forwarded: the speaker reads fragments, not deltas.
            TurnEvent::Shown { kind, text } => {
                self.trace(Trace::Delta { kind: format!("{kind:?}"), text: text.clone() });
                return;
            }
            TurnEvent::Running(true) => {
                let aloud = self.read_aloud() && self.lock().is_some();
                self.trace(Trace::Answering { aloud });
            }
            TurnEvent::Running(false) => self.trace(Trace::Answered),
            TurnEvent::Say(_) if !self.read_aloud() => return,
            TurnEvent::Say(_) => {}
            // The server writes a failed agent run into the timeline as an `error` event and says
            // nothing else: no delta, often not even a status. Without this a person who asked
            // something saw and heard nothing.
            TurnEvent::Event { event, .. } if event.kind == "error" => {
                let said = event.payload.get("message").and_then(|m| m.as_str());
                self.failed(match said {
                    Some(message) => format!("The agent did not answer: {message}"),
                    None => "The agent did not answer.".to_string(),
                });
                return;
            }
            // Nothing the speaker does anything with.
            TurnEvent::Event { .. } | TurnEvent::Lost { .. } => return,
        }
        if let Some(attached) = self.lock().as_ref() {
            // No receiver means the speaker's `run` has ended; there is nobody to tell.
            let _ = attached.forward.send(event);
        }
    }

    fn failed(&self, reason: String) {
        // Onto the trace as well: the Conversation screen reads only the trace.
        self.trace(Trace::Failed { reason: reason.clone() });
        let _ = self.events.send(VoiceEvent::Failed { reason });
    }

    fn trace(&self, step: Trace) {
        let _ = self.traces.send(step);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Attached>> {
        self.speaker.lock().expect("the attached speaker is not poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Says, Synthesise};
    use crate::split::Fragment;
    use crate::speak::Kind;
    use std::time::Duration;

    const PATIENCE: Duration = Duration::from_secs(10);

    struct Mute;
    impl Synthesise for Mute {
        fn say(&self, _: &str) -> Result<Vec<f32>, String> {
            Ok(vec![0.0; 10])
        }
    }

    struct Nobody;
    #[zyris::async_trait]
    impl Says for Nobody {
        async fn cancel(&self) -> Result<(), String> {
            Ok(())
        }
        async fn say(&self, _: String) -> Result<(), String> {
            Ok(())
        }
    }

    struct Rig {
        answers: Arc<Answers>,
        turns: broadcast::Sender<TurnEvent>,
        traces: broadcast::Receiver<Trace>,
        events: broadcast::Receiver<VoiceEvent>,
        _events_tx: broadcast::Sender<VoiceEvent>,
    }

    fn rig(read_aloud: bool) -> Rig {
        let (traces_tx, traces) = broadcast::channel(64);
        let (events_tx, events) = broadcast::channel(64);
        let answers = Answers::new(traces_tx, events_tx.clone(), read_aloud);
        let (turns, subscription) = broadcast::channel(64);
        tokio::spawn(answers.clone().run(subscription));
        Rig { answers, turns, traces, events, _events_tx: events_tx }
    }

    fn speaking(events: broadcast::Sender<VoiceEvent>) -> Arc<Speaking> {
        let (speaker, _fill, _tap) = crate::playback::offline(441);
        Speaking::new(Arc::new(Mute), Arc::new(speaker), Arc::new(Nobody), events)
    }

    impl Rig {
        fn send(&self, event: TurnEvent) {
            self.turns.send(event).expect("the reader is reading");
        }

        async fn trace(&mut self) -> Trace {
            tokio::time::timeout(PATIENCE, self.traces.recv())
                .await
                .expect("a trace was expected")
                .expect("the trace stream is open")
        }
    }

    fn shown(text: &str) -> TurnEvent {
        TurnEvent::Shown { kind: Kind::Assistant, text: text.to_string() }
    }

    async fn next<T: Clone>(rx: &mut broadcast::Receiver<T>) -> T {
        tokio::time::timeout(PATIENCE, rx.recv()).await.expect("expected one").expect("open")
    }

    /// **The point of the module**: with no speaker at all — listening off, or no voice — the
    /// answer still reaches the window, start to finish.
    #[tokio::test]
    async fn an_answer_is_traced_with_nothing_attached() {
        let mut rig = rig(true);
        rig.send(TurnEvent::Running(true));
        rig.send(shown("Yes."));
        rig.send(TurnEvent::Say(Fragment::spoken("Yes.")));
        rig.send(TurnEvent::Running(false));

        assert_eq!(rig.trace().await, Trace::Answering { aloud: false });
        assert_eq!(rig.trace().await, Trace::Delta { kind: "Assistant".into(), text: "Yes.".into() });
        assert_eq!(rig.trace().await, Trace::Answered);
    }

    /// The window switches on these two spellings; pinned so a rename is a failing test here
    /// rather than an answer the window never notices.
    #[test]
    fn the_answer_steps_keep_their_wire_shape() {
        assert_eq!(
            serde_json::to_string(&Trace::Answering { aloud: true }).unwrap(),
            r#"{"step":"answering","aloud":true}"#
        );
        assert_eq!(serde_json::to_string(&Trace::Answered).unwrap(), r#"{"step":"answered"}"#);
    }

    /// With a speaker, the answer is said to be read aloud, and the speaker is told everything it
    /// needs — after the trace, so the window hears of the answer before its first fragment.
    #[tokio::test]
    async fn an_attached_speaker_is_forwarded_what_it_reads() {
        let mut rig = rig(true);
        let mut forwarded = rig.answers.attach(speaking(rig._events_tx.clone()));
        rig.send(TurnEvent::Running(true));
        rig.send(shown("Yes."));
        rig.send(TurnEvent::Say(Fragment::spoken("Yes.")));
        rig.send(TurnEvent::Running(false));

        assert_eq!(rig.trace().await, Trace::Answering { aloud: true });
        assert_eq!(next(&mut forwarded).await, TurnEvent::Running(true));
        assert_eq!(next(&mut forwarded).await, TurnEvent::Say(Fragment::spoken("Yes.")));
        assert_eq!(next(&mut forwarded).await, TurnEvent::Running(false));
    }

    /// Reading aloud off: the text is shown, nothing sayable reaches the speaker, and the answer
    /// says it is not being read.
    #[tokio::test]
    async fn with_reading_aloud_off_nothing_sayable_is_forwarded() {
        let mut rig = rig(false);
        let mut forwarded = rig.answers.attach(speaking(rig._events_tx.clone()));
        rig.send(TurnEvent::Running(true));
        rig.send(TurnEvent::Say(Fragment::spoken("Yes.")));
        rig.send(TurnEvent::Running(false));

        assert_eq!(rig.trace().await, Trace::Answering { aloud: false });
        assert_eq!(next(&mut forwarded).await, TurnEvent::Running(true));
        assert_eq!(next(&mut forwarded).await, TurnEvent::Running(false), "and no Say between");
    }

    /// The switch moves live, and says what it was.
    #[tokio::test]
    async fn reading_aloud_can_be_switched_while_running() {
        let rig = rig(true);
        let mut forwarded = rig.answers.attach(speaking(rig._events_tx.clone()));
        assert!(rig.answers.set_read_aloud(false), "it was on");
        rig.send(TurnEvent::Say(Fragment::spoken("Held back.")));
        // A marker that is forwarded either way: once it arrives, the fragment before it has
        // been dealt with.
        rig.send(TurnEvent::Running(true));
        assert_eq!(next(&mut forwarded).await, TurnEvent::Running(true), "the fragment was not");
        assert!(!rig.answers.set_read_aloud(true), "it was off");
        rig.send(TurnEvent::Say(Fragment::spoken("Read.")));
        assert_eq!(next(&mut forwarded).await, TurnEvent::Say(Fragment::spoken("Read.")));
    }

    /// Detaching closes the speaker's channel, which is what ends its `run`.
    #[tokio::test]
    async fn detaching_ends_the_speakers_channel() {
        let rig = rig(true);
        let mut forwarded = rig.answers.attach(speaking(rig._events_tx.clone()));
        assert!(rig.answers.speaking().is_some());
        rig.answers.detach();
        assert!(rig.answers.speaking().is_none());
        assert!(matches!(
            tokio::time::timeout(PATIENCE, forwarded.recv()).await.expect("it answers"),
            Err(broadcast::error::RecvError::Closed)
        ));
    }

    /// A failed agent run arrives only as an `error` event in the timeline, and it has to be
    /// said as a failure — this is what production sent on 2026-09-25 while nothing was heard.
    /// Said once, whether or not a speaker is attached.
    #[tokio::test]
    async fn an_agent_run_that_failed_is_a_failure_and_not_silence() {
        let mut rig = rig(true);
        let _forwarded = rig.answers.attach(speaking(rig._events_tx.clone()));
        let event: zyris_attacca::ZSessionEvent = serde_json::from_value(serde_json::json!({
            "seq": 2, "cursor": 2, "kind": "error",
            "payload": { "kind": "error", "message": "The agent run failed. Please try again. (ref c3cd2811)" },
        }))
        .expect("the wire shape");
        rig.send(TurnEvent::Event { cursor: 2, event });

        match next(&mut rig.events).await {
            VoiceEvent::Failed { reason } => assert!(reason.contains("ref c3cd2811"), "{reason}"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(rig.trace().await, Trace::Failed { .. }));
        assert!(rig.events.try_recv().is_err(), "said once");
    }
}
