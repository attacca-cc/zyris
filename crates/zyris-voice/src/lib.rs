//! Speech into this machine, and exactly one thing outward: a stream of [`VoiceEvent`].
//!
//! # What "off" has to mean here
//!
//! The workspace is `members = ["crates/*"]`, so this crate is compiled by
//! `cargo test --workspace` whether or not the `voice` feature is on. "Off" therefore cannot
//! mean "not built". It means every heavy dependency is `optional = true` and **this crate
//! still exports [`VoiceEvent`] and [`Voice::events`] either way** — empty rather than absent.
//!
//! That is what lets `zyris-app` contain no `#[cfg(feature = "voice")]` anywhere: it calls
//! [`start`] on every build and reads [`Voice::describe`] for what to say about it. The two
//! builds take the same code path through the application, which is the only reason the on
//! build cannot rot unnoticed while everybody develops against the off one.
//! `crates/zyris-app/tests/the_app_never_asks_whether_voice_is_compiled_in.rs` fails the moment
//! that stops being true, and `tests/nothing_turns_the_feature_on_by_itself.rs` fails if any
//! manifest in the workspace quietly turns the feature on.
//!
//! The cost the flag buys back is measured: `cargo build -p zyris-voice --features voice` cold
//! is **2m 07s at 291% CPU with a 717 MB peak** on this machine, against a 1.4 s warm
//! `cargo build --release`.
//!
//! # What else is public, and why it is not a second data path
//!
//! `capture` (present only with the `voice` feature) is public for the same reason [`Voice::describe`] is, below: a window has to be
//! able to list the microphones a person can choose between and say which one is in use, and an
//! event stream cannot say it. Nothing in it publishes a [`VoiceEvent`] — task 6's session is
//! what turns audio into events, and it is the only caller of `capture::Capture::open`.
//!
//! # Why `describe()` exists beside the stream
//!
//! The design says this crate exposes one thing outward, and the *events* really are one
//! thing. [`Voice::describe`] is not a second data path: it is the same accommodation
//! `zyris-app`'s `Hotkey::describe` makes, and for the same reason — a window has to be able to
//! say "this cannot work here, and this is why" before anything has happened, and a stream that
//! is simply silent cannot say it. A control that cannot work must not look like one that can.

use tokio::sync::broadcast;

// What happens to a frame between the microphone and everything that reads it. Present in
// both feature states with one set of signatures; only the bodies read `aec`. See the module.
#[cfg(feature = "voice")]
pub mod apm;

// The microphone. Behind the feature because every line of it is `cpal` or `rubato`, and the
// off build has neither — see this module's own documentation for what that costs.
#[cfg(feature = "voice")]
pub mod capture;

// Knowing when somebody stopped talking. `earshot`, and the rule that ends Listening.
#[cfg(feature = "voice")]
pub mod vad;

// Whisper: the model on disk, the one parameter that makes it fast enough to talk to, and
// the settings that are decisions rather than defaults.
#[cfg(feature = "voice")]
pub mod stt;

// The state machine: Idle -> Listening -> Thinking, and what a push-to-talk key does to it.
// Everything above is a piece; this is the only thing that publishes a `VoiceEvent`.
#[cfg(feature = "voice")]
pub mod session;

// Recording a wake word, and keeping it. Nothing matches it -- see the module.
#[cfg(feature = "voice")]
pub mod wake;

/// Why a build with no `voice` feature will never hear anything.
///
/// Worded for a person reading the window, not for a developer reading a log: whoever installed
/// a build like this did not choose the feature flags.
pub const NOT_COMPILED_IN: &str =
    "this build of Zyris was made without the audio stack, so it cannot listen";

/// Why a build that *has* the audio stack still hears nothing today.
///
/// Temporary, and owed to step 7's later tasks. As of task 3 the microphone is there —
/// `capture::Capture::open` delivers 16 kHz mono chunks — but nothing turns them into a
/// [`VoiceEvent`] yet, and that is what this sentence is about. [`start`] stops returning it
/// when task 6 gives it a session to start.
///
/// Public for the same reason [`NOT_COMPILED_IN`] is: it is a sentence the window renders, and
/// the two builds have to be able to say different things about the same silence.
pub const NOTHING_WIRED_YET: &str =
    "the audio stack is compiled into this build, but nothing opens a microphone yet";

/// Something the voice session did. The only thing that leaves this crate.
///
/// Serialized as a tagged union in camelCase, like `zyris_runtime::CoreEvent`, because the far
/// side of that wire is TypeScript. It is deliberately *not* a `CoreEvent` variant: the core's
/// bus is about the node's connection to Attacca, this is about a microphone, and a build with
/// no audio stack must still be able to name the type.
///
/// These are the step 7 half of the spec's state machine — `Idle -> Listening -> Thinking`.
/// Step 8 owns `Speaking` and barge-in and will add to this; nothing here predicts their shape.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum VoiceEvent {
    /// The microphone is open and what is said is being kept.
    Listening,
    /// Recording ended and the utterance is being transcribed. On this machine a 3-second
    /// utterance takes about a third of a second, so this state is short but it is not nothing.
    Thinking,
    /// What was said, as text.
    #[serde(rename_all = "camelCase")]
    Heard { text: String },
    /// The turn ended with no speech in it — the key was tapped, or nobody said anything.
    ///
    /// Its own variant rather than [`VoiceEvent::Heard`] with an empty string: a window shows
    /// these two differently, and an empty transcript is also what a broken microphone produces.
    HeardNothing,
    /// The turn ended because something failed, and this is what to tell the person.
    #[serde(rename_all = "camelCase")]
    Failed { reason: String },
}

/// What the push-to-talk key did.
///
/// The same two things `zyris-app`.s `hotkey::HotkeyEvent` carries, and deliberately a second
/// type rather than a shared one: a global shortcut is a desktop-session concern, it lives in
/// `zyris-app` because `--headless` has none, and `zyris-app` is the crate that depends on this
/// one. `zyris-app` maps between them in one line.
///
/// **Declared here rather than in [`session`], which is where it is used, and that is the
/// point.** `session` is behind the `voice` feature and `zyris-app` may contain no
/// `#[cfg(feature = "voice")]` anywhere — so a hotkey event has to be nameable in both builds
/// or the mapping could not be written at all. Same accommodation [`VoiceEvent`] makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Push {
    /// The key went down.
    Pressed,
    /// The key came up.
    Released,
}

/// Whether speech can work here at all, and what to say when it cannot.
///
/// Two answers rather than a boolean for the reason `zyris-tools`'s `announce.rs` gives about
/// `input` and `screen_capture`: a control that always fails is worse than an absent one,
/// because nobody can tell the two apart.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum VoiceSupport {
    /// The audio stack is compiled in and this machine can be listened to. Events arrive on
    /// [`Voice::events`].
    ///
    /// **Not yet what [`start`] answers.** Capture exists as of task 3 of step 7 and
    /// `capture::support()` returns this on a machine with a microphone — but a session that
    /// turns audio into [`VoiceEvent`]s is task 6, and until there is one, [`start`] would be
    /// claiming a stream that nothing publishes to. So it still answers
    /// [`VoiceSupport::Unavailable`] on both builds, with different reasons.
    Ready,
    /// Nothing will ever arrive on the stream, and this is why.
    Unavailable { reason: String },
}

/// One voice session: a stream of [`VoiceEvent`], and an honest answer about whether anything
/// will ever come out of it.
pub struct Voice {
    /// `None` is a voice that will never publish. It is not "a sender nobody sends on": a
    /// subscriber to one of those waits forever, and waiting forever is indistinguishable from
    /// a microphone that has not been spoken into yet. A closed stream *ends*, which is what a
    /// `while let Ok(event) = rx.recv().await` loop needs in order to stop.
    events: Option<broadcast::Sender<VoiceEvent>>,
    support: VoiceSupport,
}

impl Voice {
    /// A voice that will never produce an event, and says why.
    ///
    /// `reason` is shown to a person, so it is a sentence rather than an error code.
    pub fn disabled(reason: impl Into<String>) -> Voice {
        Voice { events: None, support: VoiceSupport::Unavailable { reason: reason.into() } }
    }

    /// A new subscription. Each caller gets its own; none of them consumes another's.
    ///
    /// On a disabled voice the receiver is already closed — `try_recv` and `recv` both answer
    /// "ended" immediately rather than blocking.
    pub fn events(&self) -> broadcast::Receiver<VoiceEvent> {
        match &self.events {
            Some(tx) => tx.subscribe(),
            // The sender is dropped at the end of this expression, which closes the channel.
            None => broadcast::channel(1).1,
        }
    }

    /// Whether this can work, and what the person has to do about it. Cheap, and safe to call
    /// repeatedly — the window asks on every render.
    pub fn describe(&self) -> VoiceSupport {
        self.support.clone()
    }
}

/// Start the voice session this build and this machine can have.
///
/// **Never fails**, on any platform, in either feature state — the same shape `hotkey::start`
/// and `announce.rs` use. A machine that cannot listen gets a [`Voice`] that says so, because
/// the alternative is an application that refuses to start over a microphone.
///
/// This is the whole of what `zyris-app` calls. The two arms below are the only place in the
/// workspace that reads the feature.
pub fn start() -> Voice {
    #[cfg(not(feature = "voice"))]
    {
        Voice::disabled(NOT_COMPILED_IN)
    }
    #[cfg(feature = "voice")]
    {
        Voice::disabled(NOTHING_WIRED_YET)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The test the whole feature layout rests on.** It is written so that it passes in both
    /// feature states on purpose: the point is not that voice is off, it is that a consumer of
    /// this crate never has to know which build it got.
    #[test]
    fn a_build_without_voice_still_has_the_stream() {
        let voice = Voice::disabled("nothing here can listen");

        let mut events = voice.events();

        // Ended, rather than empty-and-waiting. The distinction is the point: a caller looping
        // on this has to be able to leave the loop.
        assert!(
            matches!(events.try_recv(), Err(broadcast::error::TryRecvError::Closed)),
            "a disabled voice must hand out a stream that has already ended, so that a consumer \
             stops rather than waiting for speech that can never arrive"
        );
    }

    /// The same thing the way a real consumer reads it — awaiting, not polling.
    ///
    /// **The deadline is the assertion.** A `Voice` that kept a sender nobody ever sends on
    /// would leave `recv()` pending forever, and `#[tokio::test]` has no timeout of its own: the
    /// test would not fail, it would hang, and the suite with it. That is exactly what happened
    /// when this mutation was tried, which is why the timeout is here rather than left to CI to
    /// notice twenty minutes later.
    #[tokio::test]
    async fn awaiting_a_disabled_stream_ends_rather_than_blocking() {
        let voice = Voice::disabled("nothing here can listen");

        let mut events = voice.events();

        let ended = tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
            .await
            .expect("a disabled stream ends at once; waiting on it is the bug this catches");

        assert!(matches!(ended, Err(broadcast::error::RecvError::Closed)));
    }

    /// Two subscribers, because `events()` is called once by the window and once by whatever
    /// drives the session, and a receiver that consumed the other's would lose events.
    #[test]
    fn every_subscriber_gets_its_own_stream() {
        let (tx, _) = broadcast::channel(4);
        let voice = Voice { events: Some(tx), support: VoiceSupport::Ready };

        let mut first = voice.events();
        let mut second = voice.events();
        voice.events.as_ref().expect("just built with a sender").send(VoiceEvent::Listening).ok();

        assert_eq!(first.try_recv(), Ok(VoiceEvent::Listening));
        assert_eq!(second.try_recv(), Ok(VoiceEvent::Listening));
    }

    /// A disabled voice has to be able to say why, or the window has nothing to render but
    /// silence — which reads as a microphone that is about to work.
    #[test]
    fn a_disabled_voice_says_why() {
        let voice = Voice::disabled("no microphone on this machine");

        assert_eq!(
            voice.describe(),
            VoiceSupport::Unavailable { reason: "no microphone on this machine".into() }
        );
    }

    /// [`start`] answers in both feature states, and the answer names the right situation.
    ///
    /// The assertion is on the reason rather than only on the variant, because the two arms of
    /// `start` differ in exactly that and a test that ignored it would pass for an off build
    /// that claimed the stack was compiled in.
    #[test]
    fn starting_always_answers_and_says_which_build_this_is() {
        let expected =
            if cfg!(feature = "voice") { NOTHING_WIRED_YET } else { NOT_COMPILED_IN };

        assert_eq!(
            start().describe(),
            VoiceSupport::Unavailable { reason: expected.into() },
            "`start` must answer on every build; only the reason differs"
        );
    }

    /// The wire shape the window switches on. Pinned here rather than discovered in TypeScript.
    #[test]
    fn an_event_serializes_as_a_tagged_union_in_camel_case() {
        let heard = VoiceEvent::Heard { text: "turn the lights off".into() };

        let json = serde_json::to_string(&heard).expect("VoiceEvent is serializable");

        assert_eq!(json, r#"{"kind":"heard","text":"turn the lights off"}"#);
        assert_eq!(
            serde_json::to_string(&VoiceEvent::HeardNothing).expect("serializable"),
            r#"{"kind":"heardNothing"}"#
        );
    }
}
