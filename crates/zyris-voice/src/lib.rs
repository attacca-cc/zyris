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

/// End the process, the way `std::process::exit` does — except in a build that reads answers
/// on the GPU, where it skips the C++ exit handlers.
///
/// ONNX Runtime's global environment is a C++ static, and with the WebGPU provider its destructor
/// releases the Dawn instance after Dawn has already torn itself down, which aborts with "pure
/// virtual method called" and a core dump on every quit (`dawn::native::NativeInstanceRelease`
/// from `OrtEnv::~OrtEnv`, measured 2026-09-25). Nothing is lost by skipping it: Tauri already
/// ends with `std::process::exit`, which drops nothing on the Rust side either, so this only
/// flushes what stdio holds and leaves.
pub fn exit_process(code: i32) -> ! {
    #[cfg(feature = "gpu-tts")]
    {
        use std::io::Write;
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        unsafe extern "C" {
            fn _exit(code: i32) -> !;
        }
        // SAFETY: `_exit` takes an int and never returns; it is in every C runtime this builds
        // against.
        unsafe { _exit(code) }
    }
    #[cfg(not(feature = "gpu-tts"))]
    std::process::exit(code)
}

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

// Every model file this crate downloads, and the one `.part`-hash-rename shape that puts it
// on disk. Shared by `stt` and `tts`, which is the whole reason it is not inside either.
#[cfg(feature = "voice")]
pub mod model;

// Whisper: the model on disk, the one parameter that makes it fast enough to talk to, and
// the settings that are decisions rather than defaults.
#[cfg(feature = "voice")]
pub mod stt;

// Supertonic 3: the files it needs, the four graphs, and the normalisation without which most
// of the world’s text is silently unsayable.
#[cfg(feature = "voice")]
pub mod tts;

// What is read aloud and what is not, and the two texts that are not the same text.
#[cfg(feature = "voice")]
pub mod speak;

// Where one fragment ends and the next begins, argued from what synthesis actually costs.
#[cfg(feature = "voice")]
pub mod split;

// The speaker: the output stream, the queue in front of it, and the tap that keeps what was
// played so the echo canceller can be told what to subtract.
#[cfg(feature = "voice")]
pub mod playback;

// The live turn feed: `turn_events`, the cursor a reconnect resumes from, and the deltas on
// their way to the filter. The only module here that speaks to Attacca.
#[cfg(feature = "voice")]
pub mod turn;

// The recordings the tests measure against, and the one reader for them.
#[cfg(all(test, feature = "voice"))]
mod fixture;

// The state machine: Idle -> Listening -> Thinking, and what a push-to-talk key does to it.
// Everything above is a piece; this is the only thing that publishes a `VoiceEvent`.
#[cfg(feature = "voice")]
pub mod session;

// The Windows echo canceller: the operating system's own Voice Capture DSP. **Behind `voice`
// rather than `aec`**, and the module documentation argues why: `aec` exists because nothing
// that ships can build `webrtc-audio-processing`, and this needs nothing built at all — the
// canceller is a DLL that is already on every Windows machine and `windows-rs` is generated
// Rust. So unlike `aec`, this is compiled and unit-tested by CI, on `windows-latest`.
#[cfg(all(windows, feature = "voice"))]
pub mod win_aec;

// Recording a wake word, and what it is matched as.
#[cfg(feature = "voice")]
pub mod wake;

// The microphone that is open, the settings that say whether there should be one, and the
// wake word recorder. Everything above is a piece; this is what puts them together.
#[cfg(feature = "voice")]
mod run;

// What the Voice screen renders. **Not behind the feature**, because `zyris-app` may contain no
// `#[cfg(feature = "voice")]` and therefore has to be able to name the answer on either build.
pub mod view;

/// Why a build with no `voice` feature will never hear anything.
///
/// Worded for a person reading the window, not for a developer reading a log: whoever installed
/// a build like this did not choose the feature flags.
pub const NOT_COMPILED_IN: &str =
    "this build of Zyris was made without the audio stack, so it cannot listen";


/// Something the voice session did. The only thing that leaves this crate.
///
/// Serialized as a tagged union in camelCase, like `zyris_runtime::CoreEvent`, because the far
/// side of that wire is TypeScript. It is deliberately *not* a `CoreEvent` variant: the core's
/// bus is about the node's connection to Attacca, this is about a microphone, and a build with
/// no audio stack must still be able to name the type.
///
/// The spec's state machine, as far as it is built: `Idle -> Listening -> Thinking` from step 7,
/// and `Speaking` with its barge-in arrow back to `Listening` from step 8. `AwaitingInput` needs
/// a second entry into a turn that nothing has yet.
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
    /// The answer is being read aloud. Published when the first fragment of it reaches the
    /// speaker, which is seconds after the first words of it reach the screen.
    Speaking,
    /// The answer finished being read aloud.
    ///
    /// **Not "the answer finished"** — the agent stops writing well before the speaker stops
    /// talking, because synthesis on this machine runs at 1.2 to 1.9 times real time. This is
    /// the one that says the room is quiet again, and a window without it would show `Speaking`
    /// over a speaker that stopped a minute ago.
    Spoke,
    /// Speech was stopped part way because the person started a turn.
    ///
    /// Its own variant rather than [`VoiceEvent::Spoke`]: they read differently to a person, and
    /// exactly one of them means the answer was not all heard.
    Interrupted,
}

/// Every step the audio takes, for somebody watching it work.
///
/// **A second stream rather than more arms on [`VoiceEvent`], and the split is the point.**
/// `VoiceEvent` is the product: four or five things a person needs to be told, each of which a
/// screen renders as a state. This is the trace — noisy, detailed, and about the *machine*
/// rather than about the conversation. Folding them together would make every consumer of the
/// product stream filter out the diagnostics, and would make the diagnostics something the
/// product's copy has to be careful about.
///
/// It is published unconditionally and costs nothing when nobody is looking: `broadcast::send`
/// on a channel with no receivers returns an error that is discarded, and every field here is
/// already computed for another reason.
///
/// Like [`VoiceEvent`] it is declared outside the `voice` feature, because `zyris-app` forwards
/// it to the window and may contain no `#[cfg(feature = "voice")]`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "step", rename_all = "camelCase")]
pub enum Trace {
    /// The push-to-talk key **arrived at Zyris**. The first thing to check when nothing
    /// happens at all: on Wayland the compositor has to be told to send it, and until it is,
    /// no `down` ever arrives.
    ///
    /// Published from [`Voice::push`], which runs whether or not anything is listening —
    /// **not from the session**, and that distinction is the whole reason this arm exists
    /// separately from [`Trace::Recording`]. A key that reaches the program and a key that
    /// reaches a running session are two different facts, and the second is false on every
    /// machine whose switch is off or whose model has not been downloaded. Reporting only the
    /// second made "the key is not bound" and "nothing is listening" look identical, which is
    /// the first question this stream is asked.
    #[serde(rename_all = "camelCase")]
    Key { down: bool },
    /// The phrase was heard, and a turn was opened because of it. `heard` is what whisper made
    /// of the utterance, so a wake nobody meant shows what it was mistaken for.
    #[serde(rename_all = "camelCase")]
    Woke { heard: String },
    /// Something was said while the phrase was being listened for, and it was not the phrase.
    /// What whisper heard travels, so a phrase that keeps being spelled some other way shows as
    /// that spelling.
    #[serde(rename_all = "camelCase")]
    Unmatched { heard: String },
    /// The key reached a running session, and a turn began or ended because of it.
    ///
    /// Always preceded by a [`Trace::Key`]. One without the other means the key arrived and
    /// nothing was listening to it.
    #[serde(rename_all = "camelCase")]
    Recording { started: bool },
    /// A turn's recording ended, and what the silence rule made of it.
    ///
    /// `kept` is false when there was less than [`vad`]'s floor of speech in it — the turn is
    /// discarded and whisper never sees it, which is the case that otherwise looks like a
    /// transcription that returned nothing.
    #[serde(rename_all = "camelCase")]
    Recorded { seconds: f32, speech_seconds: f32, kept: bool },
    /// The recording was handed to whisper. `seconds` is after trimming, so it is smaller than
    /// [`Trace::Recorded`]'s.
    #[serde(rename_all = "camelCase")]
    Transcribing { seconds: f32 },
    /// Whisper answered. An empty `text` is audio it found no speech in.
    #[serde(rename_all = "camelCase")]
    Transcribed { text: String, took_ms: u64 },
    /// What whisper makes of the turn **so far**, while the key is still down.
    ///
    /// **Whisper is not a streaming recogniser**, so this is not a growing transcript: it is
    /// the whole recording re-read from the start, and the words already shown can change when
    /// the next one lands. That is a property of the model and not a bug to smooth over — a
    /// screen that only ever appended would show a sentence the model has since revised.
    ///
    /// Display only. Nothing is sent to the agent until the key comes up and
    /// [`Trace::Transcribed`] says what the turn actually was.
    #[serde(rename_all = "camelCase")]
    Hearing { text: String, seconds: f32 },
    /// The transcript was posted into the Attacca session. The step the spec calls
    /// `send_message`, and the one that joins the listening half to the speaking half.
    #[serde(rename_all = "camelCase")]
    Sent { text: String },
    /// It was not posted, and this is why.
    #[serde(rename_all = "camelCase")]
    SendFailed { reason: String },
    /// A delta from the agent, as the screen would have it. Every delta, reasoning included.
    #[serde(rename_all = "camelCase")]
    Delta { kind: String, text: String },
    /// What the splitter cut out of the deltas to be spoken. **Not the same text as
    /// [`Trace::Delta`]**: the filter drops code fences, asides, URLs and markdown, so a
    /// fragment is what is left after all of that.
    #[serde(rename_all = "camelCase")]
    Fragment { text: String },
    /// Supertonic turned a fragment into audio.
    ///
    /// Carries the text rather than a length, so a reader can pair it with the
    /// [`Trace::Fragment`] it belongs to **without counting**. Pairing by order would be right
    /// today — synthesis is one at a time, deliberately — and would silently mis-attribute
    /// every later sentence the first time a [`Trace::Dropped`] appeared between them.
    #[serde(rename_all = "camelCase")]
    Synthesised { text: String, seconds: f32, took_ms: u64 },
    /// The audio reached the speaker's queue, and where in the stream it sits.
    ///
    /// `at_sample` and `samples` are at [`crate::tts::SAMPLE_RATE`] and are what
    /// [`Trace::Playing`] is read against: a fragment is sounding when the cursor is inside
    /// its range, and the fraction of the way through is exact rather than timed.
    #[serde(rename_all = "camelCase")]
    Queued { text: String, at_sample: u64, samples: u64 },
    /// How far the speaker has actually got, while anything is queued.
    ///
    /// **Samples written to the device, not a clock.** The queue is ahead of the loudspeaker by
    /// whatever the device buffers — 42.67 ms here — and that is the whole of the uncertainty.
    /// A position estimated from a timer would drift against it and would keep counting after
    /// an interruption threw the queue away.
    #[serde(rename_all = "camelCase")]
    Playing { at_sample: u64 },
    /// A fragment was refused by the speaker, which is what an interruption between synthesis
    /// and the queue looks like — or arrived from a turn that had already been interrupted.
    Dropped,
    /// The agent started a new answer. What follows is a turn of its own, even with nothing said
    /// on this machine in between — an answer to a message typed somewhere else, say.
    Answering,
    /// The speaker ran out of things to play.
    Spoke,
    /// Speech was cut off by the key, and how much of the answer had been heard.
    #[serde(rename_all = "camelCase")]
    Interrupted { heard: usize, unheard: usize },
    /// Something failed, said in the same words the product stream uses.
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
    /// **This is what [`start`] answers on a `voice` build with a microphone**, as of task 7:
    /// it is `capture::support()`, which asks the host for a default input configuration and
    /// reports what it said. An earlier version of this comment said `start` still answered
    /// [`VoiceSupport::Unavailable`] on both builds — true while nothing turned audio into
    /// events, and false since task 6's session and task 7's switch.
    ///
    /// It says this machine **could** be listened to, not that anything is: whether a
    /// microphone is open is [`view::ListeningState`], and the Voice screen shows both because
    /// a machine that can and is not has to read differently from one that never could.
    Ready,
    /// Nothing will ever arrive on the stream, and this is why.
    Unavailable { reason: String },
}

/// One voice session: a stream of [`VoiceEvent`], an honest answer about whether anything will
/// ever come out of it, and the switches a window moves.
///
/// **One set of method signatures in both feature states**, the accommodation `apm::Apm` makes:
/// only the bodies read the feature, so `zyris-app` calls the same methods on either build and
/// gets a different answer rather than a different program.
pub struct Voice {
    /// `None` is a voice that will never publish. It is not "a sender nobody sends on": a
    /// subscriber to one of those waits forever, and waiting forever is indistinguishable from
    /// a microphone that has not been spoken into yet. A closed stream *ends*, which is what a
    /// `while let Ok(event) = rx.recv().await` loop needs in order to stop.
    events: Option<broadcast::Sender<VoiceEvent>>,
    /// The diagnostic stream. `None` for the same reason `events` is: a subscriber to a voice
    /// that will never publish has to be able to *end*, not wait.
    traces: Option<broadcast::Sender<Trace>>,
    support: VoiceSupport,
    /// What is, or could be, listening. `None` on a [`Voice::disabled`]; always `Some` on one
    /// [`start`] built, because `run::Engine::new` cannot fail and opens nothing.
    #[cfg(feature = "voice")]
    engine: Option<std::sync::Arc<run::Engine>>,
}

impl Voice {
    /// A voice that will never produce an event, and says why.
    ///
    /// `reason` is shown to a person, so it is a sentence rather than an error code.
    pub fn disabled(reason: impl Into<String>) -> Voice {
        Voice {
            events: None,
            traces: None,
            support: VoiceSupport::Unavailable { reason: reason.into() },
            #[cfg(feature = "voice")]
            engine: None,
        }
    }

    /// A new subscription. Each caller gets its own; none of them consumes another's.
    ///
    /// On a disabled voice the receiver is already closed — `try_recv` and `recv` both answer
    /// "ended" immediately rather than blocking.
    pub fn events(&self) -> broadcast::Receiver<VoiceEvent> {
        #[cfg(feature = "voice")]
        if let Some(engine) = &self.engine {
            return engine.events();
        }
        match &self.events {
            Some(tx) => tx.subscribe(),
            // The sender is dropped at the end of this expression, which closes the channel.
            None => broadcast::channel(1).1,
        }
    }

    /// A new subscription to the diagnostic stream. [`Voice::events`]'s neighbour, and closed
    /// on a disabled voice for the same reason.
    pub fn traces(&self) -> broadcast::Receiver<Trace> {
        #[cfg(feature = "voice")]
        if let Some(engine) = &self.engine {
            return engine.traces();
        }
        match &self.traces {
            Some(tx) => tx.subscribe(),
            None => broadcast::channel(1).1,
        }
    }

    /// Whether this can work **at all** — a build with an audio stack, on a machine with a
    /// microphone that answers. Cheap, and safe to call repeatedly.
    ///
    /// Not the same question as "is anything listening": that is
    /// [`view::VoiceView::listening`], and the two are separate because a machine that *can*
    /// listen and is not doing so has to read differently from one that never could.
    pub fn describe(&self) -> VoiceSupport {
        #[cfg(feature = "voice")]
        if let Some(engine) = &self.engine {
            return engine.support();
        }
        self.support.clone()
    }

    /// The push-to-talk key went down or came up.
    ///
    /// The one thing that goes *inward*, and the reason [`Push`] is declared in this file: the
    /// key lives in `zyris-app`, which may contain no `#[cfg(feature = "voice")]`, so the type
    /// it maps to has to be nameable on both builds. A key pressed on a build that cannot
    /// listen is discarded here rather than refused; nobody pressed it expecting an error.
    pub fn push(&self, push: Push) {
        #[cfg(feature = "voice")]
        if let Some(engine) = &self.engine {
            engine.push(push);
        }
        let _ = push;
    }

    /// A connection to Attacca has come up. Listen to the turn on it.
    ///
    /// **The one method here that takes a protocol type, and the reason `zyris` is not an
    /// optional dependency of this crate.** `zyris-app` may contain no
    /// `#[cfg(feature = "voice")]` anywhere — a test enforces it — so the connect hook it
    /// installs cannot name `turn::Feed`, which exists only with the feature. The seam has to be
    /// a method on `Voice` with one signature in both builds, like [`Voice::describe`], and this
    /// is it.
    ///
    /// Runs on **every** connection, including every redial: the turn stream dies with the
    /// socket, and `after: None` does not replay what was missed, so re-subscribing is the whole
    /// of what keeps a voice working across a reconnect.
    ///
    /// On a build with no audio stack this is a no-op, and on one with it but nothing listening
    /// it is still worth doing: the subscription belongs to the connection, not to the switch.
    pub async fn on_connect(&self, connection: zyris::Connection) {
        #[cfg(feature = "voice")]
        if let Some(engine) = &self.engine {
            engine.on_connect(connection.clone()).await;
        }
        let _ = connection;
    }

    /// Start listening if a person has already said to, on some earlier run.
    ///
    /// Called by the windowed branch and **not** by `--headless`, which has no push-to-talk key
    /// and so has nothing that could start a turn.
    pub async fn resume(&self) {
        #[cfg(feature = "voice")]
        if let Some(engine) = &self.engine {
            engine.resume().await;
        }
    }

    /// Everything the Voice screen renders, read off this machine in one go.
    pub async fn look(&self) -> view::VoiceView {
        #[cfg(feature = "voice")]
        if let Some(engine) = &self.engine {
            return engine.look().await;
        }
        view::VoiceView::unavailable(match &self.support {
            VoiceSupport::Unavailable { reason } => reason.clone(),
            VoiceSupport::Ready => NOT_COMPILED_IN.to_string(),
        })
    }

    /// Turn listening on or off, and answer with what that left the machine as.
    ///
    /// **What happened, not what was asked for.** Turning it on with no model downloaded leaves
    /// [`view::ListeningState::Failed`] carrying the reason, and the answer says so.
    pub async fn set_listening(&self, on: bool) -> view::VoiceView {
        #[cfg(feature = "voice")]
        if let Some(engine) = &self.engine {
            engine.set_listening(on).await;
        }
        let _ = on;
        self.look().await
    }

    /// Choose which microphone to open, now and at the next launch.
    pub async fn choose(&self, device: view::Choice) -> view::VoiceView {
        #[cfg(feature = "voice")]
        if let Some(engine) = &self.engine {
            engine.choose(device.clone()).await;
        }
        let _ = device;
        self.look().await
    }

    /// Choose where speech is transcribed and where answers are read; `None` leaves one alone.
    pub async fn choose_compute(
        &self,
        transcribe: Option<String>,
        speak: Option<String>,
    ) -> view::VoiceView {
        #[cfg(feature = "voice")]
        if let Some(engine) = &self.engine {
            engine.choose_compute(transcribe.clone(), speak.clone()).await;
        }
        let _ = (transcribe, speak);
        self.look().await
    }

    /// Choose how fast answers are read, now and at the next launch.
    pub async fn choose_speaking_rate(&self, rate: f32) -> view::VoiceView {
        #[cfg(feature = "voice")]
        if let Some(engine) = &self.engine {
            engine.choose_speaking_rate(rate).await;
        }
        let _ = rate;
        self.look().await
    }

    /// Choose which speaker answers are read through, now and at the next launch.
    pub async fn choose_speaker(&self, speaker: view::Choice) -> view::VoiceView {
        #[cfg(feature = "voice")]
        if let Some(engine) = &self.engine {
            engine.choose_speaker(speaker.clone()).await;
        }
        let _ = speaker;
        self.look().await
    }

    /// Choose which speech model listening uses, by id, now and at the next launch.
    pub async fn choose_model(&self, id: String) -> view::VoiceView {
        #[cfg(feature = "voice")]
        if let Some(engine) = &self.engine {
            engine.choose_model(id.clone()).await;
        }
        let _ = id;
        self.look().await
    }

    /// Download a speech model, by id. Answers `Err` with a sentence when it could not be had.
    pub async fn fetch_model(&self, id: String) -> Result<view::VoiceView, String> {
        #[cfg(feature = "voice")]
        if let Some(engine) = &self.engine {
            engine.fetch_model(id).await?;
            return Ok(self.look().await);
        }
        let _ = id;
        Err(NOT_COMPILED_IN.to_string())
    }

    /// Download the voice the answers are read in. Answers `Err` with a sentence when it
    /// could not be had — including when `ZYRIS_TTS_MODELS` names a directory of somebody's own.
    pub async fn fetch_voice(&self) -> Result<view::VoiceView, String> {
        #[cfg(feature = "voice")]
        if let Some(engine) = &self.engine {
            engine.fetch_voice().await?;
            return Ok(self.look().await);
        }
        Err(NOT_COMPILED_IN.to_string())
    }

    /// Delete the downloaded speech model, and any wreckage a killed download left beside it.
    ///
    /// Turns listening off first: the running session holds the model, and a switch left on
    /// over a model that is gone is a screen claiming something it cannot do.
    pub async fn forget_model(&self, id: String) -> Result<view::VoiceView, String> {
        #[cfg(feature = "voice")]
        if let Some(engine) = &self.engine {
            engine.forget_model(id).await?;
            return Ok(self.look().await);
        }
        let _ = id;
        Err(NOT_COMPILED_IN.to_string())
    }

    /// Record one take of the wake word from the chosen microphone, and keep it.
    pub async fn record_wake_take(&self) -> Result<view::VoiceView, String> {
        #[cfg(feature = "voice")]
        if let Some(engine) = &self.engine {
            engine.record_wake_take().await?;
            return Ok(self.look().await);
        }
        Err(NOT_COMPILED_IN.to_string())
    }

    /// Forget every take of the wake word.
    pub async fn clear_wake_word(&self) -> Result<view::VoiceView, String> {
        #[cfg(feature = "voice")]
        if let Some(engine) = &self.engine {
            engine.clear_wake_word().await?;
            return Ok(self.look().await);
        }
        Err(NOT_COMPILED_IN.to_string())
    }
}

/// Start the voice session this build and this machine can have.
///
/// **Never fails**, on any platform, in either feature state — the same shape `hotkey::start`
/// and `announce.rs` use. A machine that cannot listen gets a [`Voice`] that says so, because
/// the alternative is an application that refuses to start over a microphone. **It opens
/// nothing**: see `run`'s module documentation for why a microphone is not opened at launch.
///
/// `dir` is the instance's data directory, where the answer to "should this listen?" is kept.
/// `None` is a machine that names no such directory: everything still works for this run and
/// nothing is remembered for the next one.
///
/// This is the whole of what `zyris-app` calls. The two arms below are the only place in the
/// workspace that reads the feature.
pub fn start(dir: Option<&std::path::Path>) -> Voice {
    #[cfg(not(feature = "voice"))]
    {
        let _ = dir;
        Voice::disabled(NOT_COMPILED_IN)
    }
    #[cfg(feature = "voice")]
    {
        let events = broadcast::channel(EVENT_CAPACITY).0;
        let engine = std::sync::Arc::new(run::Engine::new(dir, events.clone()));
        Voice { events: None, traces: None, support: engine.support(), engine: Some(engine) }
    }
}

/// How many [`VoiceEvent`]s a subscriber may fall behind before it loses the oldest.
///
/// A turn produces four at most — `Listening`, `Thinking`, and one of `Heard`, `HeardNothing`
/// or `Failed` — and the window is the only subscriber. Generous by two orders of magnitude,
/// like `zyris-app`'s `hotkey::EVENT_CAPACITY`, and for the same reason: `broadcast` needs a
/// number, not a tuning decision.
#[cfg(feature = "voice")]
const EVENT_CAPACITY: usize = 64;

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
        let voice = Voice {
            events: Some(tx),
            traces: None,
            support: VoiceSupport::Ready,
            #[cfg(feature = "voice")]
            engine: None,
        };

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

    /// [`start`] answers in both feature states, and **opens nothing while doing it**.
    ///
    /// The off build is pinned exactly — the sentence matters, because it is what a person
    /// reads. The on build is not pinned to a variant: it is the machine's own answer, and a
    /// machine with no microphone is a legitimate `Unavailable` there. What both must agree on
    /// is that nothing is listening, which is the decision this task took.
    #[test]
    fn starting_answers_on_every_build_and_opens_nothing() {
        let voice = start(None);

        if cfg!(not(feature = "voice")) {
            assert_eq!(
                voice.describe(),
                VoiceSupport::Unavailable { reason: NOT_COMPILED_IN.into() },
                "a build with no audio stack has to say so, in the sentence a person reads"
            );
        }

        let view = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime")
            .block_on(voice.look());
        assert_eq!(
            view.listening,
            view::ListeningState::Off,
            "`start` must not open a microphone: nothing listens until somebody asks"
        );
    }

    /// The answer a person acts on is two answers, and the screen shows both. A machine that
    /// **can** listen and is not doing so must not read like one that never could.
    #[test]
    fn being_able_to_listen_and_listening_are_two_different_answers() {
        let voice = Voice::disabled("no microphone on this machine");

        let view = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime")
            .block_on(voice.look());

        assert_eq!(
            view.support,
            VoiceSupport::Unavailable { reason: "no microphone on this machine".into() }
        );
        assert_eq!(view.listening, view::ListeningState::Off);
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
