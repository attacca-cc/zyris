//! The state machine: `Idle -> Listening -> Thinking`, and stop.
//!
//! This is the half of the spec's machine step 7 can honestly build. The spec draws
//! `Idle -> Listening -> Thinking -> Speaking -> Idle` with a barge-in arrow back to
//! `Listening` and a second entry at `AwaitingInput`; every one of those needs a turn stream
//! and a loudspeaker, and this step has neither. Nothing here predicts their shape.
//!
//! [`crate::VoiceEvent`] is the only thing that leaves. A caller subscribes to the stream and
//! learns what happened; there is no state to read back, because a state a window could read
//! and a stream a window could follow are two copies of the same fact and the second one goes
//! stale in a way nothing goes red about.
//!
//! # It is driven by a key, not by silence
//!
//! Step 7 is push-to-talk. The spec's wake-word and `AwaitingInput` entries both need something
//! step 7 does not have — a matcher, and a turn stream — so the only way into `Listening` here
//! is [`Push::Pressed`], and the way out is [`Push::Released`].
//!
//! That decides what the silence rule is for. [`crate::vad`] argues its hangover from a
//! recorded sentence and then says, in as many words, that **in hold-to-talk it is never paid
//! at all, because the key release ends the turn**. So [`push_to_talk_rule`] sets the hangover
//! to [`MAX_TURN`], which no held key can outlast, and the turn ends when the key comes up.
//! Honouring the hangover instead would cut a person who paused to think into two turns while
//! they were still holding the key down — the error [`crate::vad`] says is the unrepairable
//! one, since the first half has already gone to an agent that may act on it.
//!
//! What is still paid is everything else the endpointer knows: [`crate::vad::Rule::min_speech`]
//! is what makes a tapped key [`crate::VoiceEvent::HeardNothing`] instead of forty milliseconds
//! handed to a speech model, and [`crate::vad::Rule::margin`] is what trims the silence off
//! both ends so that [`crate::stt::audio_ctx`] has less to walk.
//!
//! # The two floors agree rather than each defending separately
//!
//! `whisper_full_with_state` answers audio under 100 ms with a warning and zero segments, and
//! [`crate::stt::Stt::transcribe`] refuses it up front so that "nobody spoke" and "never looked
//! at" are not the same answer. This session never reaches that guard: the endpointer's
//! `min_speech` is 300 ms of *speech*, and a turn carrying that much speech is at least that
//! many samples long. `the_two_floors_agree_rather_than_each_defending_separately` is the test
//! that says so, and it fails if either constant is moved to where the other stops covering it.
//!
//! # The maximum turn length, and what happens when it is hit
//!
//! [`MAX_TURN`] is 30 seconds, and a turn that reaches it is **discarded** with
//! [`crate::VoiceEvent::Failed`]. Both halves of that need arguing.
//!
//! *Why there is a cap at all.* `CLAUDE.md` gives the reason as the measured one: `earshot`
//! reads a 440 Hz tone as a voice in 121 frames of 125, so an alarm or a held note at speaking
//! pitch holds a turn open indefinitely. That is real, and it is **not** what makes the cap
//! load-bearing in step 7 — with the hangover disabled above, what holds a turn open here is
//! the key, not the room. The live cause is the one thing in this step nobody has been able to
//! check: **whether the portal's `Deactivated` (key release) arrives on Wayland is still
//! unverified.** If it does not, every hold is a `Pressed` that is never followed by anything,
//! and without a cap that is a `Vec<f32>` that grows until the machine dies. The cap turns the
//! worst case of an unanswered question into a bounded, visible failure. The tone becomes the
//! live cause again the moment step 8 adds a path into `Listening` that no key holds open.
//!
//! *Why 30 seconds.* It is [`crate::stt::FULL_WINDOW`], the whisper encoder's own window: the
//! longest turn that is still one pass, and the point past which [`crate::stt::audio_ctx`] has
//! nothing left to scale. Deriving it rather than typing `30` keeps it one number.
//!
//! *Why the audio is thrown away rather than transcribed.* This is the half that is not
//! obvious, and it is decided by what reaching the cap means. Both of the ways to get there —
//! a key whose release was lost, and a turn a tone is holding open — produce audio that is
//! either truncated mid-sentence or not speech at all. `crate::vad` prices the first: ending
//! early "truncates somebody mid-sentence and nothing downstream can repair it", because the
//! machine cannot join two turns and the first half has already been acted on. `crate::stt`
//! prices the second: whisper "invents fluent sentences out of noise, and the invention goes
//! to an agent that can act on it". Transcribing is the worse error in both branches.
//! Discarding costs a person who really did talk for half a minute one repetition — and they
//! can see it happen, which is the same asymmetry `min_speech` is argued from.
//!
//! *The buffer cap is the same cap.* The recording is bounded by [`MAX_TURN`] by construction
//! — 30 s of 16 kHz `f32` is 1.92 MB — so there is no second number that can disagree with the
//! first. [`crate::vad::Endpointer`] holds no audio and reports frame indices; this is what
//! keeps the buffer, and this is where its size is decided.
//!
//! # A press and a release are not promised to be well formed
//!
//! `zyris-app`'s `hotkey::OnePerHold` already guarantees one `Pressed` per hold and one
//! `Released` per press, and nothing here depends on that being true. Three different
//! mechanisms produce these — an X11 grab, a `RegisterHotKey` with a polling thread per press,
//! and a compositor — two of them are outside this program and one of them has never been run.
//! So:
//!
//! - a [`Push::Released`] with no [`Push::Pressed`] is ignored, and publishes nothing;
//! - a second [`Push::Pressed`] inside a hold is ignored, and **does not restart the turn** —
//!   restarting would silently throw away everything said before the repeat;
//! - a [`Push::Pressed`] that is never followed by anything ends at [`MAX_TURN`], above.
//!
//! # What audio already in the channel belongs to
//!
//! The key and the microphone arrive on two channels with no ordering between them, and audio
//! sitting in the channel when a key event is read was captured **before** it. So on a press it
//! belongs to nobody — the room somebody walked in from must not decide whether their turn was
//! long enough to be one — and on a release it belongs to the turn, because cutting the tail off
//! a sentence for a scheduling reason is the error `crate::vad` says cannot be repaired.
//!
//! Both of those are [`Session::drain`], called at the top of the press and the release. They
//! were first left to the `select!` being `biased` on the audio, which produces the same
//! behaviour and **cannot be tested**: a bias is a scheduling preference, and the mutation
//! deleting it survived a pass because the scheduler happened to agree with it. The `select!` is
//! now biased the other way, on the key, so that the two drains are the only thing deciding
//! either rule and a test can take them away.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc};

use crate::VoiceEvent;
use crate::apm::Apm;
use crate::capture::{APM_FRAME, Captured, Chunker, Recovery, VAD_FRAME};
use crate::stt;
use crate::vad::{Ended, Endpointer, Listening, Rule, frames_in};

/// The longest one turn may be. See the module: this is the buffer's cap as well, and a turn
/// that reaches it is discarded rather than truncated.
///
/// [`crate::stt::FULL_WINDOW`] rather than a `30` of its own — the whisper encoder's window is
/// the longest turn that is still one pass.
pub const MAX_TURN: Duration = stt::FULL_WINDOW;

/// What to tell a person when a turn was discarded for running past [`MAX_TURN`].
///
/// A function rather than a constant because the number belongs in the sentence and
/// [`MAX_TURN`] is the only place it is decided.
pub fn turn_too_long() -> String {
    format!(
        "the push-to-talk key was held for more than {} seconds without coming up, so the \
         recording was stopped and discarded rather than half a sentence being sent on",
        MAX_TURN.as_secs()
    )
}

/// What to tell a person when key events were lost before they got here.
pub const KEY_EVENTS_LOST: &str =
    "some push-to-talk key events were lost, so the recording was stopped rather than run on \
     without knowing whether the key had come up";

/// What to tell a person when a turn ended while two others were still being transcribed.
pub const FALLING_BEHIND: &str =
    "speech recognition is still working through what was said before this, so this recording \
     was discarded rather than queued behind it";

/// How many frames of [`crate::capture::VAD_FRAME`] samples one turn may hold.
pub fn max_turn_frames() -> usize {
    frames_in(MAX_TURN)
}

/// The endpointer's rule while a key is what holds the turn open.
///
/// Everything is [`Rule::default`] except the hangover, which is set to [`MAX_TURN`] so that it
/// cannot end a turn before the cap does — see the module for why the hangover is the wrong
/// ending for a held key, and why step 8's always-on path is what wants the default back.
pub fn push_to_talk_rule() -> Rule {
    Rule { hangover: MAX_TURN, ..Rule::default() }
}

/// What the push-to-talk key did. Declared in the crate root; see [`crate::Push`].
pub use crate::Push;

/// Why [`Session::run`] returned.
///
/// Both of these are shutdown, not failure: the loop ends when one of the two things driving it
/// goes away, and neither can be replaced from in here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// The key stream closed. This is the ordinary way the session ends.
    KeyGone,
    /// The microphone stream closed.
    MicrophoneGone,
}

/// Turning an utterance into text.
///
/// A trait rather than [`crate::stt::Stt`] directly, for one reason that is worth the
/// indirection: **every ending this module has to get right is an ending whisper is not part
/// of**, and a test that had to load 141 MB of model to find out what happens when a key is
/// tapped would be a test nobody runs. `crate::stt::Stt` implements it; the tests below use a
/// double that records what it was handed.
///
/// `transcribe` blocks — it is the signature [`crate::stt::Stt::transcribe`] already has — and
/// this module is what keeps it off the runtime.
pub trait Transcribe: Send + Sync + 'static {
    /// One utterance of 16 kHz mono samples, as text. Blocking.
    fn transcribe(&self, audio: &[f32]) -> Result<String, stt::Fault>;
}

impl Transcribe for stt::Stt {
    fn transcribe(&self, audio: &[f32]) -> Result<String, stt::Fault> {
        stt::Stt::transcribe(self, audio)
    }
}

/// What is being recorded right now.
struct Turn {
    /// Conditioned 16 kHz mono samples, a whole number of [`VAD_FRAME`] frames, so that a frame
    /// index from the endpointer is a sample offset here multiplied by [`VAD_FRAME`].
    buffer: Vec<f32>,
    /// The endpointer's frame number when this turn began. Its indices count from the last
    /// [`Endpointer::reset`] and not from the turn, so this is what makes them an offset.
    first_frame: usize,
}

/// Which of the things being waited on happened. See the module on why the `select!` arms
/// produce a value rather than doing the work: the futures are still borrowed inside them.
enum Woke {
    Audio(Option<Captured>),
    Key(Result<Push, broadcast::error::RecvError>),
    Transcribed(Result<Result<String, stt::Fault>, tokio::task::JoinError>),
    /// A look at the turn so far, for the screen. Never the turn's answer.
    Hearing(Result<Result<String, stt::Fault>, tokio::task::JoinError>, usize),
}

/// Listening for the enrolled phrase while no turn is open.
///
/// **Its own endpointer and its own buffer, not the session's.** The session's is deliberately
/// not advanced while nothing is being recorded — that is what keeps a turn's first frame equal
/// to its `turn_start` — so a watch that borrowed it would break the turn it exists to start.
struct Watch {
    features: crate::mfcc::Features,
    phrase: crate::spot::Phrase,
    ends: Endpointer,
    /// The audio `ends` is indexing, from its last reset.
    heard: Vec<f32>,
}

impl Watch {
    /// Forget what has been said so far. Called after every verdict and whenever the buffer
    /// has grown past anything the phrase could be.
    fn forget(&mut self) {
        self.ends.reset();
        self.heard.clear();
    }
}

/// The longest the watch will hold before giving up on the utterance in progress.
///
///
/// A room the detector never hears silence in — a fan, a television, a conversation across it
/// — would otherwise grow this buffer for as long as the machine is on. Anything longer than
/// a take could have been is not the phrase, so there is nothing to lose by forgetting it.
fn watch_cap() -> usize {
    crate::stt::samples_in(crate::wake::MAX_TAKE) * 2
}

/// One voice session, driven by a key and a microphone.
///
/// **It does not own the microphone**, only the receiver the microphone sends on.
/// `capture::Capture` is a `cpal::Stream` and not `Send` on every platform, so a session that
/// held one could not be moved onto a task; the caller keeps it alive for as long as it wants
/// audio, and dropping it closes this receiver, which is [`Stopped::MicrophoneGone`].
pub struct Session {
    audio: mpsc::UnboundedReceiver<Captured>,
    keys: broadcast::Receiver<Push>,
    apm: Arc<Apm>,
    stt: Arc<dyn Transcribe>,
    events: broadcast::Sender<VoiceEvent>,
    /// The diagnostic stream. Never `Option`: a sender with no subscribers costs an atomic
    /// load per send, and a `None` arm here would be a branch on every step of the pipeline.
    traces: broadcast::Sender<crate::Trace>,
    /// When the transcription in flight was handed over, so the trace can say what it cost.
    since: Option<std::time::Instant>,
    /// A look at the turn so far, running while the key is still down.
    ///
    /// **Separate from [`Session::pending`] and never allowed to delay it.** This one is for a
    /// screen; that one is the turn's answer and the only thing that reaches the agent. One at
    /// a time, and abandoned rather than awaited when the turn ends — a partial that lands
    /// after the real transcript would overwrite it with something older.
    partial: Option<tokio::task::JoinHandle<Result<String, stt::Fault>>>,
    /// How long the recording was when the partial in flight was started.
    partial_from: usize,
    /// How long the recording will be before another is worth starting.
    partial_next: usize,

    /// Whatever length the device chose, re-cut to what the processor accepts. The processor
    /// **panics** rather than erroring on a wrong count, so nothing may reach it unmeasured.
    to_apm: Chunker,
    /// And again, to what the detector accepts. Neither of the two sizes divides the other.
    to_vad: Chunker,
    endpointer: Endpointer,
    /// Frames handed to the endpointer since it was built. It is not advanced while nothing is
    /// being recorded, which is what makes a turn's first frame equal to its `turn_start`.
    frames: usize,
    turn: Option<Turn>,

    /// The transcription in flight, if any. One at a time: two whisper calls on a four-thread
    /// machine is slower than one after the other, and the pool would take a third and a fourth.
    pending: Option<tokio::task::JoinHandle<Result<String, stt::Fault>>>,
    /// One turn may wait behind it. A third is [`FALLING_BEHIND`] rather than a queue that
    /// grows: whisper runs several times faster than real time, so two turns behind is a
    /// machine that has stopped keeping up, not a person talking, and a growing queue of
    /// 30-second recordings is how a 3.6 GB machine dies quietly.
    queued: Option<Vec<f32>>,

    /// Reused across callbacks so the steady state allocates nothing.
    staged: Vec<f32>,
    ready: Vec<f32>,

    /// Listening for the enrolled phrase. `None` on a machine with no wake word recorded,
    /// which is every machine until somebody records one.
    watch: Option<Watch>,

    /// What is reading the answer aloud, if anything is. `None` is a session with no speaker —
    /// which is every one step 7 built, and every one on a machine whose output device would not
    /// open. A key pressed then starts a turn and interrupts nothing.
    speaking: Option<Arc<Speaking>>,

    /// Where a transcript goes. `None` is a machine with no Attacca session named, which hears
    /// and transcribes and has nowhere to send it.
    ///
    /// **Separate from [`Session::speaking`] although both end at the same `Feed`**, because
    /// the two open on different conditions: reading an answer aloud needs a speaker and 401 MB
    /// of voice, and sending what somebody said needs neither. Folding this into `speaking`
    /// would make a machine with no voice models deaf to the agent *and* silent to it, which is
    /// two failures out of one missing download.
    conversation: Option<Arc<dyn Says>>,
}

impl Session {
    /// Build one. Nothing happens until [`Session::run`] is awaited.
    ///
    /// `audio` is what `capture::Capture::open` returned; the chunk size it was opened with does
    /// not matter here, because this re-cuts whatever arrives.
    pub fn new(
        audio: mpsc::UnboundedReceiver<Captured>,
        keys: broadcast::Receiver<Push>,
        apm: Arc<Apm>,
        stt: Arc<dyn Transcribe>,
        events: broadcast::Sender<VoiceEvent>,
    ) -> Session {
        Session {
            audio,
            keys,
            apm,
            stt,
            events,
            // Replaced by [`Session::tracing`] when anything is watching. A channel nobody
            // subscribed to is the ordinary case and sending on it is a discarded error.
            traces: broadcast::channel(1).0,
            since: None,
            partial: None,
            partial_from: 0,
            partial_next: PARTIAL_EVERY,
            // Replaced on every press, which is where the sizes are decided and where a
            // mutation of them is caught; these two are what a session holds before the first
            // one, and nothing reads them then.
            to_apm: Chunker::new(APM_FRAME),
            to_vad: Chunker::new(VAD_FRAME),
            endpointer: Endpointer::with_rule(push_to_talk_rule()),
            frames: 0,
            turn: None,
            pending: None,
            queued: None,
            staged: Vec::new(),
            ready: Vec::new(),
            watch: None,
            speaking: None,
            conversation: None,
        }
    }

    /// Give this session something to interrupt.
    ///
    /// **A builder rather than a seventh argument to [`Session::new`]**, because a speaker is
    /// genuinely optional: a machine with no output device, or a build whose text-to-speech
    /// models have not been downloaded, listens exactly as well without one. Every ending this
    /// module already had is unchanged by its absence.
    pub fn speaking(mut self, speaking: Arc<Speaking>) -> Session {
        self.speaking = Some(speaking);
        self
    }

    /// Listen for the enrolled phrase whenever no turn is open.
    ///
    /// **A builder, and absent by default**, because a machine with no takes recorded has
    /// nothing to listen for and must not pay for the attempt. `spot::Phrase` with no usable
    /// takes answers `Cannot` to everything, so passing one is safe; not passing one is cheaper.
    pub fn listening_for(mut self, phrase: crate::spot::Phrase) -> Session {
        self.watch = Some(Watch {
            features: crate::mfcc::Features::new(),
            phrase,
            ends: Endpointer::new(),
            heard: Vec::new(),
        });
        self
    }

    /// Give this session somewhere to send what it hears.
    ///
    /// The spec's conversation loop is *get a session, send an utterance, receive the reply*,
    /// and this is the second step. Before it existed a transcript was published as
    /// [`VoiceEvent::Heard`] and went nowhere else: the listening half and the speaking half
    /// were each wired to Attacca and nothing joined them, so this computer could read an answer
    /// aloud to a question asked from somebody's phone and not to one asked out loud in front
    /// of it.
    pub fn conversation(mut self, conversation: Arc<dyn Says>) -> Session {
        self.conversation = Some(conversation);
        self
    }

    /// Publish every step onto this stream as well.
    pub fn tracing(mut self, traces: broadcast::Sender<crate::Trace>) -> Session {
        self.traces = traces;
        self
    }

    /// Run until the key stream or the microphone goes away.
    pub async fn run(mut self) -> Stopped {
        loop {
            let woke = {
                let pending = &mut self.pending;
                let partial = &mut self.partial;
                let from = self.partial_from;
                tokio::select! {
                    // The key first, deliberately: what audio already in the channel belongs to
                    // is decided by `drain` and not by this order. See the module.
                    biased;
                    key = self.keys.recv() => Woke::Key(key),
                    audio = self.audio.recv() => Woke::Audio(audio),
                    done = async {
                        match pending {
                            Some(handle) => handle.await,
                            None => std::future::pending().await,
                        }
                    } => Woke::Transcribed(done),
                    seen = async {
                        match partial {
                            Some(handle) => handle.await,
                            None => std::future::pending().await,
                        }
                    } => Woke::Hearing(seen, from),
                }
            };

            match woke {
                Woke::Audio(Some(captured)) => self.captured(captured),
                // The microphone is gone. Whatever was being recorded cannot be finished, and
                // there is nothing left to record with.
                Woke::Audio(None) => {
                    self.abort_with_device_gone();
                    return Stopped::MicrophoneGone;
                }
                Woke::Key(Ok(Push::Pressed)) => {
                    // **Put the key's own rule back.** A turn the phrase opened switched the
                    // endpointer to the ordinary one so that silence could end it; a turn the
                    // key opens must not end on silence, or somebody pausing to think mid-
                    // sentence is cut into two turns while still holding the key down.
                    self.endpointer.use_rule(push_to_talk_rule());
                    self.pressed();
                }
                Woke::Key(Ok(Push::Released)) => self.released(),
                // Events were dropped, so whether the key came up is not knowable. Ending the
                // turn is the honest answer; carrying on would be a recording with no end.
                Woke::Key(Err(broadcast::error::RecvError::Lagged(_))) => {
                    self.abort(KEY_EVENTS_LOST.to_string());
                }
                // Shutdown. Nothing is published: a turn cut off by the process ending is not
                // news anybody is still there to read.
                Woke::Key(Err(broadcast::error::RecvError::Closed)) => return Stopped::KeyGone,
                Woke::Transcribed(done) => self.transcribed(done),
                Woke::Hearing(seen, from) => self.hearing(seen, from),
            }
        }
    }

    /// One buffer from the device: condition it, cut it into detector frames, and record it.
    /// Take another look at the turn so far, if it has grown enough and nothing is looking.
    ///
    /// **Never while the final transcription is in flight.** The turn's answer is what reaches
    /// the agent and it runs on the same one-at-a-time blocking pool; a partial started beside
    /// it would take a core off the thing somebody is actually waiting for.
    fn look_again(&mut self) {
        let Some(turn) = &self.turn else { return };
        // **Nothing is watching, so there is nothing to compute.** A partial exists only to be
        // shown: it is never sent to the agent and never becomes the turn's answer. Whisper
        // re-reads the whole recording each time, so a hold of ten seconds with no subscriber
        // would spend six passes over growing audio to publish into a channel that drops it.
        //
        // This is what keeps `--headless` free of the cost entirely, and what keeps every test
        // that counts transcriptions counting only the answers. In the windowed app
        // `bridge::forward_traces` subscribes for the life of the process, so it is on whenever
        // there is a window — **not only when the Conversation tab is open**. If that turns out
        // to cost too much on a slow machine, the next move is a switch rather than a smaller
        // interval: the passes are the cost and the interval only spreads them.
        if self.traces.receiver_count() == 0 {
            return;
        }
        if self.partial.is_some() || self.pending.is_some() {
            return;
        }
        let length = turn.buffer.len();
        if length < self.partial_next || length < stt::samples_in(stt::MIN_AUDIO) {
            return;
        }
        // The next look is measured from *now* rather than from a running multiple, so a slow
        // machine takes fewer looks instead of falling behind and then taking several at once.
        self.partial_next = length + PARTIAL_EVERY;
        self.partial_from = length;
        self.partial = Some(self.spawn(turn.buffer.clone()));
    }

    /// What whisper made of the turn so far. **Published and otherwise thrown away.**
    fn hearing(
        &mut self,
        seen: Result<Result<String, stt::Fault>, tokio::task::JoinError>,
        from: usize,
    ) {
        self.partial = None;
        // A turn that has already ended owns its own answer. A partial landing after it would
        // put older words over the real transcript, which is the one thing this must not do.
        if self.turn.is_none() {
            return;
        }
        if let Ok(Ok(text)) = seen {
            if !text.is_empty() {
                self.trace(crate::Trace::Hearing { text, seconds: seconds(from) });
            }
        }
        // A fault is not reported here. The turn's own transcription is about to run over the
        // same audio and will say so properly; two messages about one failure is worse.
    }

    fn heard(&mut self, samples: &[f32]) {
        if self.turn.is_none() && !self.should_watch() {
            // Neither recording nor listening for the phrase. The audio is dropped rather than
            // buffered, and the session.s endpointer is not advanced — which is what keeps a
            // turn.s first frame equal to its `turn_start`.
            //
            // **A mutation deleting this survives**, and it stays anyway: the loop below breaks
            // on the same condition, so nothing observable changes, and what this saves is the
            // processor running a hundred times a second over audio nobody asked for.
            return;
        }

        // Out of `self` so that the per-frame work can be an ordinary `&mut self` method. Both
        // buffers go back at the end, so the steady state still allocates nothing.
        let mut staged = std::mem::take(&mut self.staged);
        staged.clear();
        staged.extend(self.to_apm.push(samples).flatten().copied());
        // The chunker.s own size, not `APM_FRAME` again. Asking twice lets the two disagree,
        // and a disagreement here is a short last chunk that the stage below silently drops.
        for frame in staged.chunks_mut(self.to_apm.frames()) {
            if let Err(fault) = self.apm.process_capture(frame) {
                self.staged = staged;
                self.abort(fault.to_string());
                return;
            }
        }

        let mut ready = std::mem::take(&mut self.ready);
        ready.clear();
        ready.extend(self.to_vad.push(&staged).flatten().copied());
        self.staged = staged;

        for frame in ready.chunks(self.to_vad.frames()) {
            if self.turn.is_some() {
                self.frame(frame);
            } else if self.should_watch() {
                self.watching(frame);
            } else {
                // The cap ended the turn part way through this buffer, and nothing is
                // listening for the phrase. The rest belongs to nothing.
                break;
            }
        }
        self.ready = ready;
        // Once per buffer of audio rather than once per detector frame: the check is cheap and
        // the answer cannot change more than once inside one callback anyway.
        self.look_again();
    }

    /// Whether the phrase is worth listening for right now.
    ///
    /// **Not while the speaker is going, and that is not an optimisation.** A build without
    /// the `aec` feature — which is every build that ships — has an echo canceller that
    /// cancels nothing, so the microphone hears the loudspeaker. A watch running then would
    /// match the machine.s own voice reading an answer aloud and open a turn on it, over and
    /// over, on every answer. This is the same reasoning that made barge-in the key rather
    /// than the microphone, and it has the same shape: the detector cannot be trusted over a
    /// speaker until the canceller is real.
    ///
    /// It costs a little on a build where the canceller *is* real: the phrase cannot be used
    /// to interrupt. The key can, and it is the thing this whole module already interrupts on.
    fn should_watch(&self) -> bool {
        if self.watch.is_none() {
            return false;
        }
        match &self.speaking {
            Some(speaking) => !speaking.is_speaking(),
            None => true,
        }
    }

    /// One detector frame while no turn is open: is this the phrase?
    fn watching(&mut self, frame: &[f32]) {
        let Some(watch) = &mut self.watch else { return };
        let listening = match watch.ends.push(frame) {
            Ok(listening) => listening,
            // Unreachable — `to_vad` hands out exactly `VAD_FRAME` — and answered by forgetting
            // rather than by ending anything, because there is no turn here to end.
            Err(_) => {
                watch.forget();
                return;
            }
        };
        watch.heard.extend_from_slice(frame);

        match listening {
            Listening::Ended(Ended::Utterance { first, last, .. }) => {
                let from = first * crate::capture::VAD_FRAME;
                let to = ((last + 1) * crate::capture::VAD_FRAME).min(watch.heard.len());
                let said = watch.heard[from.min(to)..to].to_vec();
                watch.forget();
                self.judge(&said);
            }
            // Somebody made a noise that was not long enough to be anything. Forgetting is what
            // keeps the next utterance from being scored with this one stuck on the front.
            Listening::Ended(Ended::TooShort { .. }) => watch.forget(),
            _ => {
                // A room the detector never hears silence in would grow this forever.
                if watch.heard.len() > watch_cap() {
                    watch.forget();
                }
            }
        }
    }

    /// Was that the phrase? If so, start a turn for whatever comes next.
    fn judge(&mut self, said: &[f32]) {
        let Some(watch) = &self.watch else { return };
        let verdict = watch.phrase.matches(&watch.features, said);
        match verdict {
            crate::spot::Match::Yes { distance, threshold } => {
                self.trace(crate::Trace::Woke { distance, threshold });
                // **The ordinary rule, not the key.s.** Nothing is going to let go of anything:
                // a turn opened by the phrase has to end when the person stops talking, and
                // `push_to_talk_rule` deliberately makes that impossible.
                self.endpointer.use_rule(Rule::default());
                self.pressed();
            }
            crate::spot::Match::No { .. } => {}
            // Said once per utterance and not per frame, so a machine that can never match —
            // no usable takes — says so as often as somebody speaks rather than silently.
            crate::spot::Match::Cannot { reason } => {
                tracing::debug!(reason, "the wake word could not be compared");
            }
        }
    }

    /// One detector frame: keep it, score it, and stop if the turn has run too long.
    fn frame(&mut self, frame: &[f32]) {
        match self.endpointer.push(frame) {
            Ok(listening) => {
                if let Some(turn) = &mut self.turn {
                    turn.buffer.extend_from_slice(frame);
                }
                self.frames += 1;
                // Unreachable while the rule is [`push_to_talk_rule`] — a hangover of
                // [`MAX_TURN`] needs more silence than the cap allows turn — and handled rather
                // than asserted away, because the rule is a value and step 8 will pass another.
                if let Listening::Ended(ended) = listening {
                    self.end(ended);
                    return;
                }
            }
            // Not audio, so neither speech nor silence: the endpointer refuses to advance on it
            // and neither does the buffer. `to_vad` hands out exactly `VAD_FRAME`, so this is
            // unreachable unless that stops being true — **and the same is true of the
            // processor's fault twenty lines above, which ends the turn.** Two unreachable
            // faults of one kind, handled two ways, with nothing able to tell either apart from
            // the other: whichever is right, they should agree. They agree now. A frame the
            // detector cannot read is a turn nothing can honestly finish, and ending it says so
            // where returning quietly would leave a turn that never ends and never speaks.
            Err(fault) => {
                self.abort(fault.to_string());
                return;
            }
        }

        if self.turn.as_ref().is_some_and(|turn| turn.buffer.len() >= max_turn_frames() * VAD_FRAME)
        {
            self.abort(turn_too_long());
        }
    }

    /// Take everything the microphone has already handed over and deal with it now.
    ///
    /// Audio sitting in the channel was captured **before** the key event about to be handled,
    /// so on a press it belongs to nobody and on a release it belongs to the turn — and both of
    /// those fall out of draining here, before the turn opens or closes.
    ///
    /// **Doing it explicitly rather than leaning on the `select!` being `biased` is what makes
    /// either of them provable.** A bias is a scheduling preference: a test cannot ask for one,
    /// and a mutation deleting `biased` survived a pass because the scheduler happened to agree
    /// with it. With this, the bias decides only fairness and the rule is in the code.
    fn drain(&mut self) {
        loop {
            let Ok(captured) = self.audio.try_recv() else { return };
            self.captured(captured);
        }
    }

    /// One thing the microphone said, however it got here.
    ///
    /// **One site, reached from both the `select!` and [`Self::drain`].** It was two, and a
    /// mutation flipping the copy inside `drain` survived: every test that decided the rule went
    /// through the other one. Two branches that have to agree, with nothing that goes red when
    /// they stop, is the shape this workspace keeps finding.
    fn captured(&mut self, captured: Captured) {
        let problem = match captured {
            Captured::Audio(samples) => return self.heard(&samples),
            Captured::Problem(problem) => problem,
        };

        // **Every device problem is written down, and the level is what the recovery says.**
        // This used to discard the ones it could carry on through, which left the one case this
        // module cannot otherwise explain — a microphone that goes quiet and stays quiet —
        // with no evidence anywhere. `Continue` is `debug` rather than `warn` because an
        // underrun under load arrives in bursts and would bury everything else; it is still
        // reachable with `RUST_LOG=zyris_voice=debug`, which is the difference between quiet
        // and gone. Anything that ends the turn is visible without asking.
        match problem.recovery {
            Recovery::Continue => tracing::debug!(
                reason = problem.reason,
                "the microphone reported something it can carry on through"
            ),
            Recovery::Retry | Recovery::Rebuild => tracing::warn!(
                reason = problem.reason,
                recovery = ?problem.recovery,
                "the microphone stopped delivering audio and this turn is over"
            ),
            Recovery::Stop => tracing::error!(
                reason = problem.reason,
                settings = problem.settings.as_deref(),
                "the microphone cannot be used and nothing here will retry it"
            ),
        }

        // A rerouted default stream reports and keeps running; ending the turn on it would
        // end every turn a person started while plugging in a headset.
        if problem.recovery != Recovery::Continue {
            self.abort(problem.reason);
        }
    }

    /// The key went down.
    fn pressed(&mut self) {
        // Before anything else: what is already in the channel is not part of this turn.
        self.drain();
        // **Barge-in, and it happens before the repeat guard.** A second press inside a hold
        // must not restart the recording, but it also cannot un-interrupt anything: by then the
        // queue is already gone. Putting it here rather than after the guard costs a lock on a
        // repeat and keeps the stopping unconditional, which is the property that matters —
        // there is no key press that leaves the speaker talking over the person.
        if let Some(speaking) = &self.speaking {
            speaking.interrupt();
        }
        if self.turn.is_some() {
            // A repeat inside one hold. Starting again here would discard everything said
            // before it, which is the one thing a recorder may not do.
            return;
        }
        // The chunkers hold up to 159 and 255 samples of the *previous* turn between calls —
        // 26 ms of somebody else.s sentence that would otherwise lead this one. They are cheap
        // and they are cut here rather than carried.
        self.to_apm = Chunker::new(APM_FRAME);
        self.to_vad = Chunker::new(VAD_FRAME);
        self.turn = Some(Turn { buffer: Vec::new(), first_frame: self.frames });
        // A partial still running belongs to the turn that just ended, and `hearing` drops it
        // on arrival. The schedule starts again from nothing.
        self.partial_next = PARTIAL_EVERY;
        self.trace(crate::Trace::Recording { started: true });
        self.publish(VoiceEvent::Listening);
    }

    /// The key came up. A release nobody pressed is not an ending.
    ///
    /// There is no guard here and there was one: `if self.turn.is_none() { return }` was written,
    /// a mutation deleting it survived, and it is gone for the reason `crate::vad` gives about
    /// the `.max(1)` it used to carry. Nothing can tell it from its absence, because both halves
    /// already fall out: [`Self::end`] finds no turn and publishes nothing, and
    /// [`Endpointer::finish`] on an endpointer that has not been advanced since the last turn
    /// ended only sets a turn start that is already where it would put it — audio is dropped
    /// while nothing is being recorded, so the frame number cannot have moved.
    fn released(&mut self) {
        self.trace(crate::Trace::Recording { started: false });
        // Everything already captured is part of the turn the key is ending.
        self.drain();
        let ended = self.endpointer.finish();
        self.end(ended);
    }

    /// Finish the turn the endpointer has just ended.
    fn end(&mut self, ended: Ended) {
        let Some(turn) = self.turn.take() else { return };
        match ended {
            // Not enough was said for it to be a turn. Nothing is sent to whisper: the floor
            // here is the one that keeps "nobody spoke" from arriving as an invented sentence.
            Ended::TooShort { speech, .. } => {
                self.trace(crate::Trace::Recorded {
                    seconds: seconds(turn.buffer.len()),
                    speech_seconds: speech.as_secs_f32(),
                    kept: false,
                });
                self.publish(VoiceEvent::HeardNothing)
            }
            Ended::Utterance { first, last, speech } => {
                self.trace(crate::Trace::Recorded {
                    seconds: seconds(turn.buffer.len()),
                    speech_seconds: speech.as_secs_f32(),
                    kept: true,
                });
                let from = first.saturating_sub(turn.first_frame) * VAD_FRAME;
                // The clamp is provably dead today and stays: the buffer grows by one frame
                // exactly when the endpointer.s index does, so `last` can never name a frame
                // past it. **A mutation removing it survives.** What it costs is nothing and
                // what it stands between is a future change to the buffering and a panic on a
                // slice index, inside an audio session — the same reading `crate::stt` gives
                // its `sync_all`.
                let to = ((last + 1).saturating_sub(turn.first_frame) * VAD_FRAME)
                    .min(turn.buffer.len());
                let audio = turn.buffer[from.min(to)..to].to_vec();
                self.start_transcribing(audio);
            }
        }
    }

    /// End the turn without transcribing it, and say why.
    fn abort(&mut self, reason: String) {
        // The endpointer's turn state goes with it, so the next press starts clean.
        let _ = self.endpointer.finish();
        if self.turn.take().is_some() {
            self.publish(VoiceEvent::Failed { reason });
        }
    }

    /// The microphone stream closed. Only news if something was being recorded.
    fn abort_with_device_gone(&mut self) {
        self.abort(
            "the microphone stopped delivering audio, so the recording was discarded".to_string(),
        );
    }

    fn start_transcribing(&mut self, audio: Vec<f32>) {
        self.trace(crate::Trace::Transcribing { seconds: seconds(audio.len()) });
        if self.pending.is_none() {
            self.since = Some(std::time::Instant::now());
            self.publish(VoiceEvent::Thinking);
            self.pending = Some(self.spawn(audio));
        } else if self.queued.is_none() {
            self.publish(VoiceEvent::Thinking);
            self.queued = Some(audio);
        } else {
            self.publish(VoiceEvent::Failed { reason: FALLING_BEHIND.to_string() });
        }
    }

    /// `spawn_blocking` rather than [`crate::stt::off_the_runtime`], because this loop has to be
    /// able to go on reading the microphone while whisper works and therefore needs a handle to
    /// wait on beside the other two, not a future to await.
    fn spawn(&self, audio: Vec<f32>) -> tokio::task::JoinHandle<Result<String, stt::Fault>> {
        let stt = self.stt.clone();
        tokio::task::spawn_blocking(move || stt.transcribe(&audio))
    }

    fn transcribed(
        &mut self,
        done: Result<Result<String, stt::Fault>, tokio::task::JoinError>,
    ) {
        self.pending = None;
        let took = self.since.take().map_or(0, |at| at.elapsed().as_millis() as u64);
        match done {
            // An empty transcript is not an empty sentence. `stt::clean` turns whisper's own
            // annotations for audio it found no speech in — `[BLANK_AUDIO]`, `(silence)` —
            // into exactly this, and a window shows "nobody spoke" differently from "".
            Ok(Ok(text)) if text.is_empty() => {
                self.trace(crate::Trace::Transcribed { text: String::new(), took_ms: took });
                self.publish(VoiceEvent::HeardNothing)
            }
            Ok(Ok(text)) => {
                self.trace(crate::Trace::Transcribed { text: text.clone(), took_ms: took });
                self.publish(VoiceEvent::Heard { text: text.clone() });
                self.send(text);
            }
            Ok(Err(fault)) => self.publish(VoiceEvent::Failed { reason: fault.to_string() }),
            Err(_) => {
                self.publish(VoiceEvent::Failed { reason: stt::Fault::Lost.to_string() })
            }
        }
        if let Some(next) = self.queued.take() {
            self.since = Some(std::time::Instant::now());
            self.pending = Some(self.spawn(next));
        }
    }

    /// `send` fails only when nobody is subscribed, which is the ordinary state of a machine
    /// whose window is closed. Discarded on purpose.
    fn publish(&self, event: VoiceEvent) {
        let _ = self.events.send(event);
    }

    /// Send a transcript to the agent, on a task of its own.
    ///
    /// **Spawned rather than awaited**, because this runs inside the select loop that is also
    /// reading the microphone: awaiting a round trip to Attacca here would stop capturing audio
    /// for the length of it, and the next thing somebody says would be lost. The cost is that
    /// two utterances in quick succession could arrive out of order — which is the same order
    /// they would arrive in if the person had typed them into two windows, and far cheaper than
    /// a deaf microphone.
    ///
    /// A machine with no session named has nowhere to send it. That is not an error and not
    /// silent either: the Voice screen says so before anybody speaks.
    fn send(&self, text: String) {
        let Some(conversation) = self.conversation.clone() else { return };
        let events = self.events.clone();
        let traces = self.traces.clone();
        tokio::spawn(async move {
            let sent = text.clone();
            if let Err(reason) = conversation.say(text).await {
                let _ = traces.send(crate::Trace::SendFailed { reason: reason.clone() });
                // Not swallowed. A transcript that did not reach the agent looks exactly like
                // one that did until the answer never comes, and this is the only place that
                // knows the difference.
                let _ = events.send(VoiceEvent::Failed {
                    reason: format!("what you said did not reach Attacca: {reason}"),
                });
            } else {
                let _ = traces.send(crate::Trace::Sent { text: sent });
            }
        });
    }

    /// One step onto the diagnostic stream. Discarded when nobody is watching, like
    /// [`Session::publish`].
    fn trace(&self, step: crate::Trace) {
        let _ = self.traces.send(step);
    }
}

/// Samples at the capture rate, as seconds. The trace's only arithmetic, in one place.
fn seconds(samples: usize) -> f32 {
    samples as f32 / crate::capture::SAMPLE_RATE as f32
}

// ---------------------------------------------------------------------------------------------
// Speaking, and stopping
// ---------------------------------------------------------------------------------------------

/// Turning one fragment into audio. Blocking, for seconds.
///
/// A trait rather than [`crate::tts::Tts`] for the reason [`Transcribe`] is one: every ending
/// this half has to get right — a fragment cut off, a queue thrown away, a message that says
/// where speech stopped — is an ending the model is not part of, and a test that had to load
/// 401 MB of graphs to reach one would be a test nobody runs.
pub trait Synthesise: Send + Sync + 'static {
    /// 44.1 kHz mono, in `[-1, 1]`. `&self`, so this is behind a lock: there is exactly one
    /// model and one thread may use it at a time.
    fn say(&self, text: &str) -> Result<Vec<f32>, String>;
}

impl Synthesise for std::sync::Mutex<crate::tts::Tts> {
    fn say(&self, text: &str) -> Result<Vec<f32>, String> {
        let mut tts = self.lock().map_err(|_| "the voice is not usable".to_string())?;
        tts.say(text).map(|said| said.samples).map_err(|fault| fault.to_string())
    }
}

/// Handing audio to a speaker, and taking back what it has not played yet.
///
/// The half of [`crate::playback::Playback`] that barge-in uses, as a trait so that the rules
/// below can be decided without a sound card. Everything in it is a counter the real callback
/// keeps; none of it is a clock.
pub trait Play: Send + Sync + 'static {
    /// Queue one fragment; answer where in the stream it starts, or `None` if the stream is gone.
    fn speak(&self, samples: Vec<f32>) -> Option<u64>;
    /// Samples written to the device, ever. **How far playback got.**
    fn played(&self) -> u64;
    /// Samples queued and not yet written to the device.
    fn pending(&self) -> u64;
    /// Throw away everything queued and not yet written.
    fn silence(&self);
}

impl Play for crate::playback::Speaker {
    fn speak(&self, samples: Vec<f32>) -> Option<u64> {
        crate::playback::Speaker::speak(self, samples)
    }
    fn played(&self) -> u64 {
        crate::playback::Speaker::played(self)
    }
    fn pending(&self) -> u64 {
        crate::playback::Speaker::pending(self)
    }
    fn silence(&self) {
        crate::playback::Speaker::silence(self)
    }
}

/// The two things barge-in asks of the conversation: stop generating, and record what happened.
///
/// Implemented for [`crate::turn::Feed`]; a trait here so that the order of the two calls is
/// decidable by a test, which is the only thing about them that can be got wrong silently.
#[zyris::async_trait]
pub trait Says: Send + Sync + 'static {
    /// Stop the turn that is running.
    async fn cancel(&self) -> Result<(), String>;
    /// Post a message, starting a new turn.
    async fn say(&self, message: String) -> Result<(), String>;
}

#[zyris::async_trait]
impl Says for crate::turn::Feed {
    async fn cancel(&self) -> Result<(), String> {
        crate::turn::Feed::cancel(self).await.map_err(|error| error.to_string())
    }
    async fn say(&self, message: String) -> Result<(), String> {
        crate::turn::Feed::say(self, message).await.map_err(|error| error.to_string())
    }
}

/// How often the drain watch looks, once a turn has stopped producing fragments.
///
/// Only ever reached when there is audio still queued, so the cost is one atomic read every
/// 50 ms of somebody being spoken to. It is a poll rather than a notification because the thing
/// being waited on is an audio callback, which may not signal anything.
/// How much new audio is worth another look at a turn in progress.
///
/// **This is a cost, not a frame rate.** Whisper re-reads the *whole* recording each time —
/// there is no streaming decoder here — so a hold of ten seconds at this interval costs six
/// passes over an ever-longer clip. 1.5 s keeps that to well under one core on the machine
/// this was measured on, where a three-second clip is 0.9 s in release and 1.3 s under
/// `cargo test`.
///
/// It is deliberately not smaller. The words visibly change as the model revises them, and a
/// screen that flickered twice a second would be harder to read than one that settles.
const PARTIAL_EVERY: usize = crate::capture::SAMPLE_RATE as usize * 3 / 2;

const DRAIN_POLL: Duration = Duration::from_millis(50);

/// How often the playback cursor is published while an answer is being read aloud.
///
/// Ten a second: fast enough that a sentence of a second or two visibly fills, slow enough that
/// a window doing nothing else is not the reason the machine is busy. It is a poll for
/// [`DRAIN_POLL`]'s reason — the thing being watched is an audio callback, which signals
/// nothing.
const PLAYBACK_POLL: Duration = Duration::from_millis(100);

/// One fragment that was queued, and where in the stream it is.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Queued {
    text: String,
    /// Samples handed to the speaker before this fragment. [`Play::speak`]'s own answer, never a
    /// second running total — see its documentation.
    start: u64,
    samples: u64,
}

/// What was queued for this turn, in order, so that an interruption can say where it stopped.
#[derive(Debug, Default)]
struct Ledger {
    queued: Vec<Queued>,
}

impl Ledger {
    fn add(&mut self, text: &str, start: u64, samples: u64) {
        self.queued.push(Queued { text: text.to_string(), start, samples });
    }

    fn is_empty(&self) -> bool {
        self.queued.is_empty()
    }

    fn clear(&mut self) {
        self.queued.clear();
    }

    /// Read the ledger against how far the speaker got.
    ///
    /// `played` is samples **written to the device**, which is not the same as samples a person
    /// has heard: the device holds another `stream_delay` — 42.67 ms on this machine — that it
    /// will go on to emit whatever happens here. So this is the upper bound, by a fraction of
    /// one frame, and it is a measurement rather than an estimate from a clock.
    fn at(&self, played: u64) -> Interruption {
        let mut interruption = Interruption::default();
        for entry in &self.queued {
            let end = entry.start + entry.samples;
            if played >= end {
                interruption.heard.push(entry.text.clone());
            } else if played > entry.start {
                interruption.cut = Some(Cut {
                    text: entry.text.clone(),
                    at: played_for(played - entry.start),
                    of: played_for(entry.samples),
                });
            } else {
                interruption.unheard.push(entry.text.clone());
            }
        }
        interruption
    }
}

/// The silence put between one fragment and the next, in samples of the speaker.s stream.
///
/// **A subtraction rather than a number**, which is task 2.s decision and not this one.s: the
/// model already leaves a lead-in and a tail on every fragment, together 677 ms to 1.19 ms wide,
/// and [`crate::split::GAP`] is what is *missing* from the pause they make between them. It is
/// zero today and stops being zero the moment those pads are trimmed before a fragment is
/// queued — which is worth doing and is not done here.
///
/// A function rather than a constant so that the arithmetic has somewhere to be wrong and a
/// test has something to decide.
fn gap_samples() -> usize {
    (crate::split::GAP.as_secs_f64() * f64::from(crate::tts::SAMPLE_RATE)).round() as usize
}

/// How long a number of samples of the speaker.s stream lasts.
fn played_for(samples: u64) -> Duration {
    Duration::from_secs_f64(samples as f64 / f64::from(crate::tts::SAMPLE_RATE))
}

/// The fragment speech stopped in the middle of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cut {
    /// The fragment, as it was sent to the voice.
    pub text: String,
    /// How much of it reached the speaker.
    pub at: Duration,
    /// How long the whole of it was.
    pub of: Duration,
}

/// Where a spoken answer was cut off.
///
/// # What this node knows, and what it cannot tell anybody
///
/// It knows this **precisely**: it has the samples it handed to the device and the timestamp the
/// backend attached to each callback. What it has no way to say is any of it to the server —
/// `cancel_turn` takes a session id and nothing else, and there is no `Cancelled` frame, so from
/// the stream alone a cancel and an ordinary finish are the same event. The spec's "record only
/// what actually reached the speaker" is therefore not implementable as written.
///
/// So the decision taken is to **post a message saying where the speech was cut off**, which is
/// [`Interruption::message`], and to ask upstream for `cancel_turn` to take a delivery point.
///
/// **The agent's own record is not truncated by any of this**, and the copy may not say it is.
/// Generation runs ahead of speech — synthesis on this machine is 1.2 to 1.9 times slower than
/// real time — so by the time somebody interrupts, most of the answer has usually been written
/// already. What was cut short is the reading aloud.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Interruption {
    /// Fragments the speaker played to the end, in order.
    pub heard: Vec<String>,
    /// The fragment it stopped in the middle of, if it stopped in the middle of one.
    pub cut: Option<Cut>,
    /// Fragments that were queued and never started.
    pub unheard: Vec<String>,
}

impl Interruption {
    /// Whether any of what was queued went unheard. `false` is a speaker that had finished.
    pub fn anything_missed(&self) -> bool {
        self.cut.is_some() || !self.unheard.is_empty()
    }

    /// The message posted into the session, written for the agent reading it.
    ///
    /// Bracketed and named, because it is a message this node wrote and not something the person
    /// said — an agent that could not tell the two apart would answer it as if it had been asked
    /// something.
    pub fn message(&self) -> String {
        let quoted = |texts: &[String]| {
            texts.iter().map(|text| format!("\u{201c}{text}\u{201d}")).collect::<Vec<_>>().join(" ")
        };
        let mut lines = vec![
            "[Zyris: the person started speaking, so reading this answer aloud was stopped part \
             way through and the turn was cancelled.]"
                .to_string(),
        ];
        if !self.heard.is_empty() {
            lines.push(format!("Heard in full: {}", quoted(&self.heard)));
        }
        if let Some(cut) = &self.cut {
            lines.push(format!(
                "Heard {:.1}s of {:.1}s: \u{201c}{}\u{201d}",
                cut.at.as_secs_f64(),
                cut.of.as_secs_f64(),
                cut.text
            ));
        }
        if !self.unheard.is_empty() {
            lines.push(format!("Not heard at all: {}", quoted(&self.unheard)));
        }
        if self.heard.is_empty() && self.cut.is_none() {
            lines.push("None of it was heard.".to_string());
        }
        lines.push(
            "Your own record of that answer is complete — only the speaking was cut short. \
             Speech runs behind writing here, so most of what you wrote had already been written \
             before anything was stopped."
                .to_string(),
        );
        lines.join("\n")
    }
}

/// Reading an answer aloud, and stopping when the person starts a turn.
///
/// # What barge-in is, here
///
/// It is the push-to-talk key going down, and **not** the microphone hearing a voice. That is a
/// decision with a reason on each side:
///
/// - Wake-word matching is deferred to its own spike, so there is no other way into a turn: the
///   only way a person speaks to this machine is by reaching for the key. "Stops the moment you
///   speak" and "stops the moment you press" are the same moment.
/// - A build **without the `aec` feature** — which is every build that ships, see
///   `crates/zyris-voice/Cargo.toml` — has an echo canceller that cancels nothing. A session
///   that barged in on detected speech there would hear its own loudspeaker and cut itself off
///   after its first word, on every answer. The detector cannot be trusted over a speaker until
///   [`crate::apm::Apm::erle_db`] says the canceller is real, and nothing in CI can compile the
///   code that would make it real.
///
/// # What stopping does, in order
///
/// Throw the queue away, read how far the speaker got, cancel the turn, and post a message
/// saying where it was cut off. The order matters twice: the queue is discarded **before**
/// `played` is read, or the answer would include audio that never reached the device; and the
/// turn is cancelled **before** the message is posted, or the message would arrive into a turn
/// that is still generating.
pub struct Speaking {
    tts: Arc<dyn Synthesise>,
    out: Arc<dyn Play>,
    turn: Arc<dyn Says>,
    events: broadcast::Sender<VoiceEvent>,
    /// The diagnostic stream, for the half of the pipeline that runs after the agent answers.
    /// **Everything it needs is already here**: `run` sees `TurnEvent::Shown` — what the agent
    /// wrote — beside `TurnEvent::Say` — what the filter and splitter made of it — so the two
    /// texts can be shown against each other without `turn.rs` knowing about tracing at all.
    traces: broadcast::Sender<crate::Trace>,
    state: std::sync::Mutex<SpeakingState>,
}

#[derive(Default)]
struct SpeakingState {
    ledger: Ledger,
    /// How many times speech has been stopped. A synthesis that finishes after a barge-in
    /// carries the number it started with and is thrown away rather than queued behind the
    /// person who just interrupted.
    generation: u64,
}

impl Speaking {
    /// The voice, the speaker, the conversation, and where events go.
    pub fn new(
        tts: Arc<dyn Synthesise>,
        out: Arc<dyn Play>,
        turn: Arc<dyn Says>,
        events: broadcast::Sender<VoiceEvent>,
    ) -> Arc<Speaking> {
        Arc::new(Speaking {
            tts,
            out,
            turn,
            events,
            traces: broadcast::channel(1).0,
            state: std::sync::Mutex::new(Default::default()),
        })
    }

    /// Publish every step onto this stream as well. [`Session::tracing`]'s opposite number.
    ///
    /// Takes `Arc<Self>` apart rather than `&mut self` because [`Speaking::new`] answers an
    /// `Arc` — a builder here would have to unwrap it, and every caller holds only the one.
    pub fn tracing(self: Arc<Self>, traces: broadcast::Sender<crate::Trace>) -> Arc<Speaking> {
        Arc::new(Speaking {
            tts: self.tts.clone(),
            out: self.out.clone(),
            turn: self.turn.clone(),
            events: self.events.clone(),
            traces,
            state: std::sync::Mutex::new(Default::default()),
        })
    }

    /// Synthesise and queue everything the feed says to, until the feed goes away.
    ///
    /// **One fragment at a time, deliberately.** There is one model, it is 451 MB resident, and
    /// synthesis is slower than speech on this machine — a second one in flight would take a
    /// core off the first and make the first sentence later, which is the only latency anybody
    /// hears.
    pub async fn run(self: Arc<Self>, mut turns: broadcast::Receiver<crate::turn::TurnEvent>) {
        loop {
            match turns.recv().await {
                Ok(crate::turn::TurnEvent::Say(fragment)) => {
                    self.trace(crate::Trace::Fragment { text: fragment.text().to_string() });
                    self.synthesise(fragment.text()).await;
                }
                // Carried to the trace and nowhere else. This is what the agent wrote, before
                // the filter took the code fences and asides out of it, and seeing the two
                // beside each other is the only way to tell "the filter ate it" from "the
                // agent never said it".
                Ok(crate::turn::TurnEvent::Shown { kind, text }) => {
                    self.trace(crate::Trace::Delta { kind: format!("{kind:?}"), text });
                }
                // The end of a turn. Everything sayable has been said; what is left is waiting
                // for the speaker to get through it.
                Ok(crate::turn::TurnEvent::Running(false)) => self.drained().await,
                Ok(_) => {}
                // A fragment was dropped before it was read, which is a sentence that will never
                // be spoken. Nothing can recover it — a `Delta` is not durable and nothing
                // replays one — so it is said out loud rather than swallowed.
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    self.publish(VoiceEvent::Failed {
                        reason: format!(
                            "speech fell too far behind the answer and {missed} pieces of it \
                             were lost, so part of it was not read aloud"
                        ),
                    });
                }
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    }

    /// One fragment: say it, and queue it if nobody interrupted while it was being said.
    async fn synthesise(self: &Arc<Self>, text: &str) {
        let generation = self.generation();
        let tts = self.tts.clone();
        let owned = text.to_string();
        let started = std::time::Instant::now();
        let said = tokio::task::spawn_blocking(move || tts.say(&owned)).await;

        // **Whether anybody interrupted while this was being made is decided in
        // [`Speaking::queue`], under the lock, and nowhere else.** A check here as well would be
        // a second copy of the rule that no test could tell from its absence — this module
        // already carries two clauses like that from step 7 and does not want a third. A
        // synthesis that *failed* is still reported either way: the voice really did stop
        // working, and whoever interrupted is not the reason.
        match said {
            Ok(Ok(samples)) => {
                self.trace(crate::Trace::Synthesised {
                    text: text.to_string(),
                    seconds: samples.len() as f32 / crate::tts::SAMPLE_RATE as f32,
                    took_ms: started.elapsed().as_millis() as u64,
                });
                self.queue(text, samples, generation)
            }
            Ok(Err(reason)) => self.publish(VoiceEvent::Failed { reason }),
            Err(_) => self.publish(VoiceEvent::Failed {
                reason: "making the answer into speech stopped before it finished".to_string(),
            }),
        }
    }

    /// Put one fragment.s audio on the speaker.s queue and write it into the ledger.
    ///
    /// **The generation is checked again here, under the lock**, and the check in
    /// [`Speaking::synthesise`] is the cheap early-out rather than the rule. Between that check
    /// and this line is a window — microseconds, but a real one — in which a key press would
    /// discard the queue and then have this put a sentence back onto it, unledgered, to be
    /// played over whoever pressed the key.
    fn queue(self: &Arc<Self>, text: &str, samples: Vec<f32>, generation: u64) {
        let mut state = self.state.lock().expect("the speaking state is not poisoned");
        if state.generation != generation {
            // Somebody pressed the key between the synthesis starting and this line. The audio
            // is real and nobody will hear it.
            self.trace(crate::Trace::Dropped);
            return;
        }
        let first = state.ledger.is_empty();
        if !first && gap_samples() > 0 {
            self.out.speak(vec![0.0; gap_samples()]);
        }
        let length = samples.len() as u64;
        match self.out.speak(samples) {
            Some(at) => {
                state.ledger.add(text, at, length);
                self.trace(crate::Trace::Queued {
                    text: text.to_string(),
                    at_sample: at,
                    samples: length,
                });
                if first {
                    drop(state);
                    self.publish(VoiceEvent::Speaking);
                    self.clone().watch_playback(generation);
                }
            }
            None => self.publish(VoiceEvent::Failed {
                reason: "the speaker stopped accepting audio, so the answer was not read aloud"
                    .to_string(),
            }),
        }
    }

    /// Wait for the speaker to finish what it was given, then say so.
    ///
    /// Reached when a turn stops producing text. **Not the same as "the answer is finished"** —
    /// generation ends well before speech does, which is the whole reason barge-in has anything
    /// to say.
    async fn drained(&self) {
        let generation = self.generation();
        while self.out.pending() > 0 {
            tokio::time::sleep(DRAIN_POLL).await;
            if self.generation() != generation {
                // Somebody interrupted; they own the ending, not this.
                return;
            }
        }
        let mut state = self.state.lock().expect("the speaking state is not poisoned");
        if state.generation != generation || state.ledger.is_empty() {
            return;
        }
        state.ledger.clear();
        drop(state);
        self.trace(crate::Trace::Spoke);
        self.publish(VoiceEvent::Spoke);
    }

    /// Publish where the speaker has actually got to, until it has nothing left.
    ///
    /// **Only while something is queued**, and started by the first fragment of an answer
    /// rather than run for the life of the session: a machine that is not speaking would
    /// otherwise put ten messages a second onto the stream saying the same number.
    ///
    /// It reads [`Play::played`] — samples written to the device — and not a clock. A position
    /// counted from a timer would drift against whatever the device buffers and, worse, would
    /// keep counting after an interruption threw the queue away.
    fn watch_playback(self: Arc<Self>, generation: u64) {
        tokio::spawn(async move {
            loop {
                if self.generation() != generation {
                    // Interrupted. The ending belongs to whoever pressed the key.
                    return;
                }
                self.trace(crate::Trace::Playing { at_sample: self.out.played() });
                if self.out.pending() == 0 {
                    return;
                }
                tokio::time::sleep(PLAYBACK_POLL).await;
            }
        });
    }

    /// The person started a turn. Stop speaking, and answer with what they did not hear.
    ///
    /// `None` is a speaker that had already finished — every queued fragment written to the
    /// device — which is the ordinary case for a key pressed between answers. **Derived from the
    /// ledger and the device's own counter rather than from a flag**: a flag saying "still
    /// speaking" is a second copy of that fact, and the copy is what goes stale.
    pub fn stop(&self) -> Option<Interruption> {
        let mut state = self.state.lock().expect("the speaking state is not poisoned");
        state.generation += 1;
        // Discard first: `played` must not include audio that was still on the queue.
        self.out.silence();
        let interruption = state.ledger.at(self.out.played());
        state.ledger.clear();
        interruption.anything_missed().then_some(interruption)
    }

    /// Cancel the turn and record where the speech stopped, in that order.
    ///
    /// Separate from [`Speaking::stop`] because the two halves belong to different places: the
    /// stopping is synchronous and has to happen inside the key press, and this talks to a server
    /// and may take as long as a round trip.
    pub async fn record(&self, interruption: Interruption) {
        if let Err(error) = self.turn.cancel().await {
            tracing::warn!(%error, "the turn could not be cancelled after speech was interrupted");
        }
        if let Err(error) = self.turn.say(interruption.message()).await {
            tracing::warn!(
                %error,
                "the session was not told where the spoken answer was cut off, so its record of \
                 the answer does not say that only part of it was heard"
            );
        }
    }

    /// Whether anything is queued or being played.
    ///
    /// **The wake word may not listen while this is true on a build with no echo canceller**,
    /// which is every build that ships. See `Session::watching`.
    pub fn is_speaking(&self) -> bool {
        self.out.pending() > 0
    }

    /// Both halves, as one call for a caller that is not async. Nothing if nothing was missed.
    pub fn interrupt(self: &Arc<Self>) {
        let Some(interruption) = self.stop() else { return };
        self.trace(crate::Trace::Interrupted {
            heard: interruption.heard.len(),
            unheard: interruption.unheard.len() + usize::from(interruption.cut.is_some()),
        });
        self.publish(VoiceEvent::Interrupted);
        let speaking = self.clone();
        tokio::spawn(async move { speaking.record(interruption).await });
    }

    fn generation(&self) -> u64 {
        self.state.lock().expect("the speaking state is not poisoned").generation
    }

    /// One step onto the diagnostic stream.
    fn trace(&self, step: crate::Trace) {
        let _ = self.traces.send(step);
    }

    fn publish(&self, event: VoiceEvent) {
        let _ = self.events.send(event);
    }
}

/// What reached Attacca, in order.
///
/// **Shared by both test modules rather than written twice.** The listening half asserts that a
/// transcript arrives at all and the speaking half asserts the order of a cancel against the
/// message after it; two doubles for one trait is the duplication this file keeps arguing
/// against everywhere else.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
enum Told {
    Cancel,
    Said(String),
}

#[cfg(test)]
#[derive(Default)]
struct Conversation {
    told: std::sync::Mutex<Vec<Told>>,
    /// Answer every call with an error. What a node that is offline, or whose session has gone,
    /// does to a transcript.
    refuse: bool,
}

#[cfg(test)]
impl Conversation {
    fn told(&self) -> Vec<Told> {
        self.told.lock().expect("not poisoned").clone()
    }

    /// The one message posted, or a panic naming what was posted instead.
    fn message(&self) -> String {
        match self.told().into_iter().find_map(|told| match told {
            Told::Said(message) => Some(message),
            Told::Cancel => None,
        }) {
            Some(message) => message,
            None => panic!("nothing was posted into the session: {:?}", self.told()),
        }
    }
}

#[cfg(test)]
#[zyris::async_trait]
impl Says for Conversation {
    async fn cancel(&self) -> Result<(), String> {
        self.told.lock().expect("not poisoned").push(Told::Cancel);
        if self.refuse { return Err("the connection is gone".into()) }
        Ok(())
    }
    async fn say(&self, message: String) -> Result<(), String> {
        self.told.lock().expect("not poisoned").push(Told::Said(message));
        if self.refuse { return Err("the connection is gone".into()) }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use crate::capture::{DeviceProblem, SAMPLE_RATE};

    // -----------------------------------------------------------------------------------------
    // Material
    // -----------------------------------------------------------------------------------------

    /// Long enough that a machine under load does not fail a test, short enough that a state
    /// machine which never leaves a state is a failure rather than a suite that has to be
    /// killed by hand. **The deadline is the assertion** in every test that waits: a
    /// `#[tokio::test]` has no timeout of its own, and this crate has already lost twenty-four
    /// minutes to that once.
    const PATIENCE: Duration = Duration::from_secs(10);

    /// How long "and then nothing happened" is given to be wrong.
    const QUIET: Duration = Duration::from_millis(150);

    fn seconds(count: f32) -> usize {
        (SAMPLE_RATE as f32 * count) as usize
    }

    fn silence(secs: f32) -> Vec<f32> {
        vec![0.0; seconds(secs)]
    }

    /// A 440 Hz tone. Used where the point is that *something* fills a turn, not that a sine is
    /// speech — `crate::vad` is where that measurement lives.
    fn tone(secs: f32) -> Vec<f32> {
        (0..seconds(secs))
            .map(|n| 0.5 * (std::f32::consts::TAU * 440.0 * n as f32 / SAMPLE_RATE as f32).sin())
            .collect()
    }

    /// A full-band burst: a door, a cough, a hand on a desk. Broadband, so the high-pass keeps
    /// it, and short, so the noise suppressor has no time to decide it is noise.
    fn burst(secs: f32) -> Vec<f32> {
        let mut seed = 12345u32;
        (0..seconds(secs))
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                ((seed >> 8) as f32 / 8388608.0 - 1.0) * 0.8
            })
            .collect()
    }

    /// `tests/audio/jfk.wav`: 11.00 s, 16 kHz, mono, one sentence, the recording every claim
    /// about the silence rule is measured against.
    ///
    /// Real speech rather than a tone, and not only for realism: with the `aec` feature on, the
    /// processor's noise suppression runs over everything before the detector sees it, and a
    /// steady sine is exactly what a stationary-noise suppressor exists to remove. A test built
    /// on one would pass in the build CI compiles and fail in the build that has the library.
    ///
    /// Read with a short RIFF reader for the reason `crate::vad`'s tests give: `hound` would be
    /// a `[dev-dependencies]` entry on a crate whose whole feature layout exists to keep the
    /// dependency graph small.
    fn recorded() -> Vec<f32> {
        crate::fixture::wav("jfk.wav")
    }

    /// A slice of the recording that is speech rather than the silence it opens with. The first
    /// word starts at 336 ms; half a second in is inside it.
    fn utterance(secs: f32) -> Vec<f32> {
        let all = recorded();
        let from = seconds(0.5);
        all[from..(from + seconds(secs)).min(all.len())].to_vec()
    }

    // -----------------------------------------------------------------------------------------
    // Doubles
    // -----------------------------------------------------------------------------------------

    /// A transcriber that writes down what it was handed.
    ///
    /// Every ending this module has to get right — a tap, a lost release, a device that goes
    /// away — is an ending whisper is not part of, and the two that whisper *is* part of care
    /// about what it answered rather than about what it is. Neither needs 141 MB on disk.
    pub(super) struct Scribe {
        heard: Mutex<Vec<Vec<f32>>>,
        answers: Mutex<VecDeque<Result<String, stt::Fault>>>,
        /// When set, every call blocks until a token is put on it. This is how "a key pressed
        /// again while a transcription is still running" is made a sequence rather than a race.
        gate: Option<Mutex<std::sync::mpsc::Receiver<()>>>,
    }

    impl Scribe {
        pub(super) fn saying(
            answers: impl IntoIterator<Item = Result<String, stt::Fault>>,
        ) -> Arc<Scribe> {
            Arc::new(Scribe {
                heard: Mutex::new(Vec::new()),
                answers: Mutex::new(answers.into_iter().collect()),
                gate: None,
            })
        }

        pub(super) fn always(text: &str) -> Arc<Scribe> {
            Scribe::saying(std::iter::repeat_n(Ok(text.to_string()), 8))
        }

        fn gated(text: &str) -> (Arc<Scribe>, std::sync::mpsc::Sender<()>) {
            let (open, gate) = std::sync::mpsc::channel();
            let scribe = Arc::new(Scribe {
                heard: Mutex::new(Vec::new()),
                answers: Mutex::new(std::iter::repeat_n(Ok(text.to_string()), 8).collect()),
                gate: Some(Mutex::new(gate)),
            });
            (scribe, open)
        }

        fn calls(&self) -> usize {
            self.heard.lock().expect("not poisoned").len()
        }

        fn audio(&self, nth: usize) -> Vec<f32> {
            self.heard.lock().expect("not poisoned")[nth].clone()
        }
    }

    impl Transcribe for Scribe {
        fn transcribe(&self, audio: &[f32]) -> Result<String, stt::Fault> {
            // Written down before the gate, so a test can see that the call started.
            self.heard.lock().expect("not poisoned").push(audio.to_vec());
            if let Some(gate) = &self.gate {
                let _ = gate.lock().expect("not poisoned").recv();
            }
            self.answers
                .lock()
                .expect("not poisoned")
                .pop_front()
                .unwrap_or(Ok(String::new()))
        }
    }

    // -----------------------------------------------------------------------------------------
    // The harness
    // -----------------------------------------------------------------------------------------

    struct Harness {
        /// An `Option` so that a test can unplug the microphone without moving the harness out
        /// from under the methods that read the event stream.
        audio: Option<mpsc::UnboundedSender<Captured>>,
        keys: broadcast::Sender<Push>,
        events: broadcast::Receiver<VoiceEvent>,
        scribe: Arc<Scribe>,
        session: tokio::task::JoinHandle<Stopped>,
    }

    fn running(scribe: Arc<Scribe>) -> Harness {
        let (audio, audio_rx) = mpsc::unbounded_channel();
        let (keys, keys_rx) = broadcast::channel(32);
        let (events, events_rx) = broadcast::channel(64);
        let apm = Arc::new(Apm::new().expect("a processor this machine can build"));
        let session = Session::new(audio_rx, keys_rx, apm, scribe.clone(), events);
        Harness {
            audio: Some(audio),
            keys,
            events: events_rx,
            scribe,
            session: tokio::spawn(session.run()),
        }
    }

    /// A phrase made of two tones, and recordings of it that differ the way a person's do.
    ///
    /// Synthetic, and the tests below are about the *wiring* rather than about whether the
    /// matcher works on a voice — that is measured in `spot`, on real takes, and the number
    /// is in `spot::CEILING`'s documentation. What these decide is that audio reaches the
    /// watch, that a match opens a turn, that a non-match does not, and that the turn it opens
    /// ends the way a turn with no key has to.
    fn two_tones(first: f32, second: f32, seconds: f32) -> Vec<f32> {
        said(first, second, seconds, 0)
    }

    /// One saying of the phrase, with `voice` deciding how this one differs from the others.
    ///
    /// **Five identical recordings are not five takes.** The threshold is calibrated from how
    /// much the takes disagree with each other, so pure tones — which disagree by almost
    /// nothing — produce a threshold of about 1 where five real recordings of a voice produced
    /// 16.3. A test built on identical takes therefore demands an identical candidate and
    /// fails on any honest one, which is exactly what happened: 0.26 s of trailing silence,
    /// which the endpointer's own margin puts there, scored 15.3 against a threshold of 1.06.
    ///
    /// So each take carries its own noise and its own small shifts in level and pitch, which
    /// is what a person saying one phrase five times sounds like to this front end.
    fn said(first: f32, second: f32, seconds: f32, voice: u32) -> Vec<f32> {
        // splitmix64, the same thirty lines `tts` uses to be deterministic without `rand`.
        let mut state = 0x9E3779B97F4A7C15u64.wrapping_mul(voice as u64 + 1);
        let mut next = move || {
            state = state.wrapping_add(0x9E3779B97F4A7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
            ((z ^ (z >> 31)) >> 40) as f32 / 16777216.0 - 0.5
        };
        let drift = 1.0 + voice as f32 * 0.01;
        let level = 0.4 - voice as f32 * 0.02;
        let tone = |hz: f32, seconds: f32, next: &mut dyn FnMut() -> f32| {
            let n = (crate::capture::SAMPLE_RATE as f32 * seconds) as usize;
            (0..n)
                .map(|at| {
                    let t = at as f32 / crate::capture::SAMPLE_RATE as f32;
                    level * (2.0 * std::f32::consts::PI * hz * drift * t).sin()
                        + 0.02 * next()
                })
                .collect::<Vec<f32>>()
        };
        let mut samples = tone(first, seconds / 2.0, &mut next);
        samples.extend(tone(second, seconds / 2.0, &mut next));
        samples
    }

    /// A take, conditioned the way the live path conditions what it compares.
    ///
    /// **The processor is part of the comparison, not a detail of the capture.** With `aec`
    /// on, noise suppression and the high-pass filter change the audio before the watch ever
    /// sees it — so templates built from raw samples and a candidate that went through the
    /// processor are two different recordings of one phrase, and the distance measures the
    /// processing. Both of these tests passed under plain `voice` and failed under `aec` for
    /// exactly that reason, which is the same asymmetry `wake` records `Conditioning` for.
    fn conditioned(samples: &[f32]) -> Vec<f32> {
        let apm = Apm::new().expect("a processor this machine can build");
        let mut out = Vec::with_capacity(samples.len());
        for frame in samples.chunks(crate::capture::APM_FRAME) {
            if frame.len() != crate::capture::APM_FRAME {
                break;
            }
            let mut frame = frame.to_vec();
            apm.process_capture(&mut frame).expect("a frame of the right length");
            out.extend_from_slice(&frame);
        }
        out
    }

    fn the_phrase() -> crate::spot::Phrase {
        let features = crate::mfcc::Features::new();
        // **With the endpointer's margin on each end, because a real take has one.** A take
        // is recorded by `run::record_one`, which ends it at the endpointer's verdict, and
        // `enrolled_phrase` trims it to the same verdict — so both carry `vad::MARGIN`. The
        // candidate the watch hands over carries it too. Templates built without it are the
        // one side of the comparison that differs from production, and the difference is not
        // small: measured, 0.26 s of silence on one side only moved the distance from 4.6 to
        // 12.9 against a threshold of 10.6 — a phrase that would be recognised, refused.
        let margin = vec![0.0f32; crate::stt::samples_in(crate::vad::MARGIN)];
        let takes: Vec<Vec<f32>> = [1.0, 1.1, 1.2, 0.95, 1.05]
            .iter()
            .enumerate()
            .map(|(voice, seconds)| {
                let mut take = margin.clone();
                take.extend(said(300.0, 900.0, *seconds, voice as u32 + 1));
                take.extend(margin.iter().copied());
                conditioned(&take)
            })
            .collect();
        crate::spot::Phrase::from_takes(&features, &takes)
    }

    /// A session listening for [`the_phrase`], and watching its own diagnostic stream.
    fn running_and_listening(scribe: Arc<Scribe>) -> (Harness, broadcast::Receiver<crate::Trace>) {
        let (audio, audio_rx) = mpsc::unbounded_channel();
        let (keys, keys_rx) = broadcast::channel(32);
        let (events, events_rx) = broadcast::channel(64);
        let (traces, traces_rx) = broadcast::channel(512);
        let apm = Arc::new(Apm::new().expect("a processor this machine can build"));
        let session = Session::new(audio_rx, keys_rx, apm, scribe.clone(), events)
            .tracing(traces)
            .listening_for(the_phrase());
        let harness = Harness {
            audio: Some(audio),
            keys,
            events: events_rx,
            scribe,
            session: tokio::spawn(session.run()),
        };
        (harness, traces_rx)
    }

    /// A speaker with a second of audio still to go, and a voice that is never asked for any.
    ///
    /// Only `pending` matters here: it is the whole of what `should_watch` asks.
    struct Busy;
    impl Play for Busy {
        fn speak(&self, _: Vec<f32>) -> Option<u64> {
            Some(0)
        }
        fn played(&self) -> u64 {
            0
        }
        fn pending(&self) -> u64 {
            crate::tts::SAMPLE_RATE as u64
        }
        fn silence(&self) {}
    }

    struct Mute;
    impl Synthesise for Mute {
        fn say(&self, _: &str) -> Result<Vec<f32>, String> {
            Ok(Vec::new())
        }
    }

    /// Listening for the phrase, with an answer already being read aloud.
    fn running_and_speaking(scribe: Arc<Scribe>) -> (Harness, broadcast::Receiver<crate::Trace>) {
        let (audio, audio_rx) = mpsc::unbounded_channel();
        let (keys, keys_rx) = broadcast::channel(32);
        let (events, events_rx) = broadcast::channel(64);
        let (traces, traces_rx) = broadcast::channel(512);
        let apm = Arc::new(Apm::new().expect("a processor this machine can build"));
        let speaking = Speaking::new(
            Arc::new(Mute),
            Arc::new(Busy),
            Arc::new(Conversation::default()),
            events.clone(),
        );
        let session = Session::new(audio_rx, keys_rx, apm, scribe.clone(), events)
            .tracing(traces)
            .listening_for(the_phrase())
            .speaking(speaking);
        let harness = Harness {
            audio: Some(audio),
            keys,
            events: events_rx,
            scribe,
            session: tokio::spawn(session.run()),
        };
        (harness, traces_rx)
    }

    /// Digital silence, long enough for the watch's hangover to end an utterance.
    fn quiet(seconds: f32) -> Vec<f32> {
        vec![0.0; (crate::capture::SAMPLE_RATE as f32 * seconds) as usize]
    }

    /// The same, with somebody subscribed to the diagnostic stream.
    ///
    /// Partials are computed only when something is watching, so a harness that wants them
    /// has to hold the receiver — dropping it would switch them off half way through a test.
    fn running_watched(scribe: Arc<Scribe>) -> (Harness, broadcast::Receiver<crate::Trace>) {
        let (audio, audio_rx) = mpsc::unbounded_channel();
        let (keys, keys_rx) = broadcast::channel(32);
        let (events, events_rx) = broadcast::channel(64);
        let (traces, traces_rx) = broadcast::channel(256);
        let apm = Arc::new(Apm::new().expect("a processor this machine can build"));
        let session = Session::new(audio_rx, keys_rx, apm, scribe.clone(), events).tracing(traces);
        let harness = Harness {
            audio: Some(audio),
            keys,
            events: events_rx,
            scribe,
            session: tokio::spawn(session.run()),
        };
        (harness, traces_rx)
    }

    /// Every step the stream carried, for a test that has stopped the session.
    fn steps(traces: &mut broadcast::Receiver<crate::Trace>) -> Vec<crate::Trace> {
        let mut seen = Vec::new();
        while let Ok(step) = traces.try_recv() {
            seen.push(step);
        }
        seen
    }

    /// Wait for one step the predicate accepts.
    ///
    /// **Not `settle` and then a drain.** A look at the turn runs on a blocking thread, so it
    /// is not finished after any number of yields on a current-thread runtime, and a test that
    /// drained would be asserting on how fast this machine is. The deadline is the assertion.
    async fn step_where(
        traces: &mut broadcast::Receiver<crate::Trace>,
        wanted: impl Fn(&crate::Trace) -> bool,
    ) -> crate::Trace {
        tokio::time::timeout(PATIENCE, async {
            loop {
                let step = traces.recv().await.expect("the trace stream must stay open");
                if wanted(&step) {
                    return step;
                }
            }
        })
        .await
        .expect("the step this test is about never arrived")
    }

    /// The same, with somewhere for the transcript to go.
    fn running_in_a_conversation(scribe: Arc<Scribe>) -> (Harness, Arc<Conversation>) {
        let (audio, audio_rx) = mpsc::unbounded_channel();
        let (keys, keys_rx) = broadcast::channel(32);
        let (events, events_rx) = broadcast::channel(64);
        let apm = Arc::new(Apm::new().expect("a processor this machine can build"));
        let conversation = Arc::new(Conversation::default());
        let session = Session::new(audio_rx, keys_rx, apm, scribe.clone(), events)
            .conversation(conversation.clone());
        let harness = Harness {
            audio: Some(audio),
            keys,
            events: events_rx,
            scribe,
            session: tokio::spawn(session.run()),
        };
        (harness, conversation)
    }

    /// Let the session catch up with everything it has been handed.
    ///
    /// Needed because the `select!` is `biased` on the audio, which is the right production
    /// order — audio already in the channel when a key event is read was captured *before* it,
    /// so it belongs to the turn on a release and to nobody on a press — and the wrong order for
    /// a test that hands over a press and two seconds of audio in the same instant. On a
    /// current-thread runtime the session runs until both its channels are empty, so one yield
    /// is enough; it is done several times so that a task which parks and is woken again does
    /// not race the assertion.
    async fn settle() {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    }

    impl Harness {
        async fn press(&self) {
            self.press_now();
            settle().await;
        }

        async fn release(&self) {
            self.release_now();
            settle().await;
        }

        /// Hand a key event over **without** letting the session run.
        ///
        /// The pair below are what put audio and a key event in their channels at the same
        /// instant, which is the only way to test what `Session::drain` decides: every other
        /// method here settles, and a session that has already emptied the audio channel has
        /// nothing for a drain to do.
        fn press_now(&self) {
            let _ = self.keys.send(Push::Pressed);
        }

        fn release_now(&self) {
            let _ = self.keys.send(Push::Released);
        }

        fn feed_now(&self, samples: &[f32]) {
            if let Some(audio) = &self.audio {
                let _ = audio.send(Captured::Audio(samples.to_vec()));
            }
        }

        fn problem_now(&self, recovery: Recovery, reason: &str) {
            if let Some(audio) = &self.audio {
                let _ = audio.send(Captured::Problem(DeviceProblem {
                    recovery,
                    reason: reason.to_string(),
                    settings: None,
                }));
            }
        }

        /// One buffer, whatever length it is. A device picks the length, not this crate.
        async fn feed(&self, samples: &[f32]) {
            if let Some(audio) = &self.audio {
                let _ = audio.send(Captured::Audio(samples.to_vec()));
            }
            settle().await;
        }

        async fn problem(&self, recovery: Recovery, reason: &str) {
            if let Some(audio) = &self.audio {
                let _ = audio.send(Captured::Problem(DeviceProblem {
                    recovery,
                    reason: reason.to_string(),
                    settings: None,
                }));
            }
            settle().await;
        }

        /// The microphone goes away: the stream is closed and nothing replaces it.
        async fn unplug(&mut self) {
            self.audio = None;
            settle().await;
        }

        async fn next(&mut self) -> VoiceEvent {
            tokio::time::timeout(PATIENCE, self.events.recv())
                .await
                .expect(
                    "the session has to publish something; a machine that never leaves a state \
                     is the failure this deadline exists to catch",
                )
                .expect("the event stream must not close while the session is running")
        }

        async fn says_nothing(&mut self) {
            let spoke = tokio::time::timeout(QUIET, self.events.recv()).await;
            assert!(spoke.is_err(), "expected silence, got {spoke:?}");
        }

        async fn stops(self) -> Stopped {
            drop(self.keys);
            drop(self.audio);
            tokio::time::timeout(PATIENCE, self.session)
                .await
                .expect("the session has to end when nothing is driving it")
                .expect("the session task must not panic")
        }
    }

    // -----------------------------------------------------------------------------------------
    // Idle -> Listening -> Thinking
    // -----------------------------------------------------------------------------------------

    #[tokio::test]
    async fn pressing_the_key_says_it_is_listening() {
        let mut zyris = running(Scribe::always("hello"));

        zyris.press().await;

        assert_eq!(zyris.next().await, VoiceEvent::Listening);
        zyris.stops().await;
    }

    /// The plan's first ending: the key went down and came up and nobody said anything.
    #[tokio::test]
    async fn a_key_released_before_anybody_spoke_hears_nothing() {
        let mut zyris = running(Scribe::always("something invented"));

        zyris.press().await;
        zyris.feed(&silence(1.0)).await;
        zyris.release().await;

        assert_eq!(zyris.next().await, VoiceEvent::Listening);
        assert_eq!(zyris.next().await, VoiceEvent::HeardNothing);
        assert_eq!(
            zyris.scribe.calls(),
            0,
            "a turn with no speech in it must never reach the model: whisper answers noise with \
             a fluent sentence, and the sentence goes to an agent that can act on it"
        );
        zyris.stops().await;
    }

    /// The plan's second ending: the key was held through a whole utterance.
    #[tokio::test]
    async fn a_key_held_through_an_utterance_is_transcribed() {
        let mut zyris = running(Scribe::always("and so my fellow americans"));

        zyris.press().await;
        zyris.feed(&utterance(3.0)).await;
        zyris.release().await;

        assert_eq!(zyris.next().await, VoiceEvent::Listening);
        assert_eq!(zyris.next().await, VoiceEvent::Thinking);
        assert_eq!(
            zyris.next().await,
            VoiceEvent::Heard { text: "and so my fellow americans".into() }
        );
        assert_eq!(zyris.scribe.calls(), 1);
        zyris.stops().await;
    }

    /// **What was said reaches the agent, and that is the step the spec calls `send_message`.**
    /// It was missing: a transcript was published as [`VoiceEvent::Heard`] and went nowhere,
    /// so this computer could read out an answer to a question asked from somewhere else and
    /// not one asked out loud in front of it. Nothing in the suite noticed, because every test
    /// of the listening half asserted on the event stream and every test of the speaking half
    /// started from a turn that was already running.
    #[tokio::test]
    async fn what_was_heard_is_sent_to_the_agent() {
        let (mut zyris, attacca) = running_in_a_conversation(Scribe::always("what is the time"));

        zyris.press().await;
        zyris.feed(&utterance(3.0)).await;
        zyris.release().await;

        assert_eq!(zyris.next().await, VoiceEvent::Listening);
        assert_eq!(zyris.next().await, VoiceEvent::Thinking);
        assert_eq!(zyris.next().await, VoiceEvent::Heard { text: "what is the time".into() });
        // Posted on a task of its own, so it is not there the instant the event is.
        settle().await;
        assert_eq!(attacca.told(), vec![Told::Said("what is the time".into())]);
        zyris.stops().await;
    }

    /// A turn with nothing in it is not a message. An agent asked an empty question answers
    /// something, and the whole of what `min_speech` and `stt::clean` are for is that a tapped
    /// key and a quiet room do not become a sentence somebody has to undo.
    #[tokio::test]
    async fn a_turn_nobody_spoke_in_sends_nothing() {
        let (mut zyris, attacca) = running_in_a_conversation(Scribe::always(""));

        zyris.press().await;
        zyris.feed(&utterance(3.0)).await;
        zyris.release().await;

        assert_eq!(zyris.next().await, VoiceEvent::Listening);
        assert_eq!(zyris.next().await, VoiceEvent::Thinking);
        assert_eq!(zyris.next().await, VoiceEvent::HeardNothing);
        settle().await;
        assert!(attacca.told().is_empty(), "an empty transcript was posted: {:?}", attacca.told());
        zyris.stops().await;
    }

    /// A transcript that did not arrive is said out loud rather than swallowed. Without this
    /// it looks exactly like one that did, until the answer never comes — and on a machine with
    /// no voice downloaded there is no answer to wait for either, so nothing would ever say it.
    #[tokio::test]
    async fn a_transcript_that_did_not_reach_attacca_is_reported() {
        let (mut zyris, _attacca) = {
            let (audio, audio_rx) = mpsc::unbounded_channel();
            let (keys, keys_rx) = broadcast::channel(32);
            let (events, events_rx) = broadcast::channel(64);
            let apm = Arc::new(Apm::new().expect("a processor this machine can build"));
            let refuses = Arc::new(Conversation { refuse: true, ..Conversation::default() });
            let scribe = Scribe::always("hello");
            let session = Session::new(audio_rx, keys_rx, apm, scribe.clone(), events)
                .conversation(refuses.clone());
            (
                Harness {
                    audio: Some(audio),
                    keys,
                    events: events_rx,
                    scribe,
                    session: tokio::spawn(session.run()),
                },
                refuses,
            )
        };

        zyris.press().await;
        zyris.feed(&utterance(3.0)).await;
        zyris.release().await;

        assert_eq!(zyris.next().await, VoiceEvent::Listening);
        assert_eq!(zyris.next().await, VoiceEvent::Thinking);
        assert_eq!(zyris.next().await, VoiceEvent::Heard { text: "hello".into() });
        match zyris.next().await {
            VoiceEvent::Failed { reason } => {
                assert!(reason.contains("did not reach Attacca"), "{reason}");
            }
            other => panic!("a refused post was not reported: {other:?}"),
        }
        zyris.stops().await;
    }

    /// **A turn in progress is read back while the key is still down**, which is the whole of
    /// what makes a conversation screen able to show anything before somebody lets go.
    ///
    /// Whisper is not a streaming recogniser, so this is the recording re-read from the start
    /// rather than a growing transcript. The words may change; the test asserts only that a
    /// look happened and that it did not become the turn's answer.
    #[tokio::test]
    async fn a_turn_is_read_back_while_the_key_is_still_down() {
        let (mut zyris, mut traces) = running_watched(Scribe::always("and so my"));

        zyris.press().await;
        // More than `PARTIAL_EVERY`, so one look is due.
        zyris.feed(&utterance(3.0)).await;

        let seen = step_where(&mut traces, |step| matches!(step, crate::Trace::Hearing { .. }))
            .await;
        match seen {
            crate::Trace::Hearing { text, seconds } => {
                assert_eq!(text, "and so my");
                assert!(seconds > 0.0, "a look has to say how much it looked at");
            }
            other => panic!("not a look at the turn: {other:?}"),
        }

        // And nothing has been published as the turn's answer, because the key is still down.
        assert_eq!(zyris.next().await, VoiceEvent::Listening);
        zyris.says_nothing().await;
        zyris.release().await;
        zyris.stops().await;
    }

    /// **Nothing is watching, so nothing is computed.** A partial is never sent to the agent
    /// and never becomes an answer; it exists to be shown. Whisper re-reads the whole
    /// recording each time, so computing one for a channel that drops it is the cost of the
    /// feature with none of the point of it — and it is what keeps `--headless` free of it.
    #[tokio::test]
    async fn a_turn_nobody_is_watching_is_not_read_back() {
        let mut zyris = running(Scribe::always("and so my"));

        zyris.press().await;
        zyris.feed(&utterance(3.0)).await;
        settle().await;

        assert_eq!(
            zyris.scribe.calls(),
            0,
            "the model was asked about a turn no window could have shown"
        );
        zyris.release().await;
        zyris.stops().await;
    }

    /// A look still running when the key comes up is thrown away when it lands.
    ///
    /// It was started on less audio than the turn ended with, so letting it through would put
    /// older words over the real transcript — the one thing a display-only path must not do.
    ///
    /// **The scribe is gated so the look is genuinely still in flight at the release.** With an
    /// instant one it finishes during the hold, nothing is in flight when the key comes up, and
    /// the test passes whether or not the rule is there: a mutation deleting the guard survived
    /// exactly that arrangement.
    #[tokio::test]
    async fn a_look_still_running_when_the_key_came_up_is_dropped() {
        let (scribe, open) = Scribe::gated("half a sentence");
        let (mut zyris, mut traces) = running_watched(scribe);

        zyris.press().await;
        // Enough for one look, which now blocks inside the scribe.
        zyris.feed(&utterance(3.0)).await;
        zyris.release().await;

        // Let the look finish first, then the turn's own transcription behind it.
        open.send(()).expect("the look is waiting on the gate");
        open.send(()).expect("the transcription is waiting on the gate");

        assert_eq!(zyris.next().await, VoiceEvent::Listening);
        assert_eq!(zyris.next().await, VoiceEvent::Thinking);
        assert_eq!(
            zyris.next().await,
            VoiceEvent::Heard { text: "half a sentence".into() }
        );
        tokio::time::sleep(QUIET).await;

        let looks = steps(&mut traces)
            .into_iter()
            .filter(|step| matches!(step, crate::Trace::Hearing { .. }))
            .count();
        assert_eq!(looks, 0, "a look that outlived its turn was published");
        zyris.stops().await;
    }

    /// **Saying the phrase opens a turn, with no key touched.** The whole of what the wake
    /// word is for, and until this existed the takes on disk were read by nothing.
    #[tokio::test]
    async fn saying_the_phrase_opens_a_turn() {
        let (mut zyris, mut traces) = running_and_listening(Scribe::always("what is the time"));

        // The phrase, then enough silence for the watch to decide the utterance is over.
        zyris.feed(&two_tones(300.0, 900.0, 1.1)).await;
        zyris.feed(&quiet(1.6)).await;

        let woke = step_where(&mut traces, |step| matches!(step, crate::Trace::Woke { .. })).await;
        match woke {
            crate::Trace::Woke { distance, threshold } => {
                assert!(distance <= threshold, "{distance} is not under {threshold}");
            }
            other => panic!("not a wake: {other:?}"),
        }
        assert_eq!(zyris.next().await, VoiceEvent::Listening);
        zyris.stops().await;
    }

    /// Something else said in the room does not open one. The cost of getting this wrong is a
    /// turn nobody asked for going to an agent that can act on it, which is the asymmetry
    /// `spot::ROOM` is argued from.
    #[tokio::test]
    async fn saying_something_else_does_not() {
        let (mut zyris, mut traces) = running_and_listening(Scribe::always("what is the time"));

        zyris.feed(&two_tones(1500.0, 400.0, 1.1)).await;
        zyris.feed(&quiet(1.6)).await;
        settle().await;

        let woke = steps(&mut traces)
            .into_iter()
            .any(|step| matches!(step, crate::Trace::Woke { .. }));
        assert!(!woke, "a different phrase opened a turn");
        zyris.says_nothing().await;
        zyris.stops().await;
    }

    /// **A turn the phrase opened ends on silence**, because there is no key to let go of.
    /// `push_to_talk_rule` sets the hangover to the whole cap so that a hold is never cut in
    /// two; leaving that in force here would make a wake turn run to the cap and be discarded
    /// every time, which is the shape of failure step 7 already shipped once.
    #[tokio::test]
    async fn a_turn_the_phrase_opened_ends_when_the_talking_stops() {
        let (mut zyris, _traces) = running_and_listening(Scribe::always("what is the time"));

        zyris.feed(&two_tones(300.0, 900.0, 1.1)).await;
        zyris.feed(&quiet(1.6)).await;
        assert_eq!(zyris.next().await, VoiceEvent::Listening);

        // Now somebody speaks, and then stops. Nothing touches a key.
        zyris.feed(&utterance(2.0)).await;
        zyris.feed(&quiet(1.6)).await;

        assert_eq!(zyris.next().await, VoiceEvent::Thinking);
        assert_eq!(
            zyris.next().await,
            VoiceEvent::Heard { text: "what is the time".into() }
        );
        zyris.stops().await;
    }

    /// **The machine does not wake itself up.** A build without the `aec` feature — which is
    /// every build that ships — has an echo canceller that cancels nothing, so the microphone
    /// hears the loudspeaker. Left listening while an answer is being read aloud, the watch
    /// would score the machine's own voice against the phrase and open a turn on it, on every
    /// answer, for as long as it kept talking.
    ///
    /// This is the same reasoning that made barge-in the key rather than the microphone, and
    /// it costs the same thing: the phrase cannot interrupt. The key can.
    #[tokio::test]
    async fn the_phrase_is_not_listened_for_while_the_answer_is_being_read_aloud() {
        let (mut zyris, mut traces) = running_and_speaking(Scribe::always("what is the time"));

        // Exactly the audio that wakes it when nothing is speaking.
        zyris.feed(&two_tones(300.0, 900.0, 1.1)).await;
        zyris.feed(&quiet(1.6)).await;
        settle().await;

        let woke = steps(&mut traces)
            .into_iter()
            .any(|step| matches!(step, crate::Trace::Woke { .. }));
        assert!(!woke, "the machine woke itself up on its own voice");
        zyris.says_nothing().await;
        zyris.stops().await;
    }

    /// The plan's third ending: the key is still down and the microphone goes away.
    #[tokio::test]
    async fn the_device_disappearing_mid_hold_ends_the_turn_and_says_why() {
        let mut zyris = running(Scribe::always("half a sentence"));

        zyris.press().await;
        zyris.feed(&utterance(2.0)).await;
        zyris.problem(Recovery::Rebuild, "the microphone was unplugged").await;

        assert_eq!(zyris.next().await, VoiceEvent::Listening);
        assert_eq!(
            zyris.next().await,
            VoiceEvent::Failed { reason: "the microphone was unplugged".into() }
        );
        assert_eq!(
            zyris.scribe.calls(),
            0,
            "the recording stopped part way through a sentence; sending the half that arrived \
             is the one thing nothing downstream can repair"
        );
        zyris.stops().await;
    }

    /// And the discriminating half of it: not every report from the device is an ending. A
    /// default-device stream that the backend reroutes says [`Recovery::Continue`] and goes on
    /// delivering audio, and a session that ended the turn on it would end every turn somebody
    /// started while plugging in a headset. Once through the `select!` and once through
    /// `Session::drain`, because those were two copies of the rule before they were one.
    /// Everything the session hears from the device is written down, and the one it carries on
    /// through is written down too.
    ///
    /// **This is the only test in the crate that reads a log**, and it exists because the
    /// alternative failure has no other evidence: a microphone that reroutes onto a monitor or
    /// a `null` source keeps delivering, keeps being silent, and every turn after it is
    /// `HeardNothing`. Before this the `Continue` arm was `=> {}` — the one piece of evidence
    /// discarded at the one moment it was worth having.
    ///
    /// A `debug` line rather than a `warn` for that arm, because an underrun under load arrives
    /// in bursts; the subscriber here asks for it explicitly, which is what a person diagnosing
    /// one would do.
    #[tokio::test]
    async fn every_device_problem_reaches_the_log() {
        use std::io::Write;
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct Shared(Arc<Mutex<Vec<u8>>>);
        impl Write for Shared {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().expect("the log is not poisoned").extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Shared {
            type Writer = Shared;
            fn make_writer(&'a self) -> Shared {
                self.clone()
            }
        }

        let written = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(Shared(written.clone()))
            .with_ansi(false)
            .finish();

        // `set_default` rather than `with_default`: the body awaits, and a closure cannot.
        // `#[tokio::test]` is a current-thread runtime, so the thread-local the guard sets is
        // the same one every await comes back to.
        let guard = tracing::subscriber::set_default(subscriber);
        let mut zyris = running(Scribe::always("never mind"));
        zyris.problem_now(Recovery::Continue, "the default device changed");
        zyris.problem_now(Recovery::Rebuild, "the microphone was unplugged");
        zyris.problem_now(Recovery::Stop, "the microphone is not allowed");
        settle().await;
        zyris.stops().await;
        drop(guard);

        let said = String::from_utf8(written.lock().expect("the log is not poisoned").clone())
            .expect("the log is text");

        for reason in [
            "the default device changed",
            "the microphone was unplugged",
            "the microphone is not allowed",
        ] {
            assert!(said.contains(reason), "{reason:?} is not in the log:\n{said}");
        }

        // And the level is the recovery's, not one level for all three: an underrun that is
        // carried through must not read the same as a microphone that is gone.
        let line = |needle: &str| {
            said.lines()
                .find(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("{needle:?} is not in the log:\n{said}"))
                .to_string()
        };
        assert!(line("the default device changed").contains("DEBUG"));
        assert!(line("the microphone was unplugged").contains("WARN"));
        assert!(line("the microphone is not allowed").contains("ERROR"));
    }

    #[tokio::test]
    async fn a_rerouted_default_stream_reported_as_the_key_comes_up_does_not_end_the_turn() {
        let mut zyris = running(Scribe::always("the whole sentence"));

        zyris.press().await;
        assert_eq!(zyris.next().await, VoiceEvent::Listening);

        zyris.feed_now(&utterance(2.0));
        zyris.problem_now(Recovery::Continue, "the default device changed");
        zyris.release_now();
        settle().await;

        assert_eq!(zyris.next().await, VoiceEvent::Thinking);
        assert_eq!(zyris.next().await, VoiceEvent::Heard { text: "the whole sentence".into() });
        zyris.stops().await;
    }

    #[tokio::test]
    async fn a_rerouted_default_stream_does_not_end_the_turn() {
        let mut zyris = running(Scribe::always("the whole sentence"));

        zyris.press().await;
        zyris.feed(&utterance(1.5)).await;
        zyris.problem(Recovery::Continue, "the default device changed").await;
        zyris.feed(&utterance(1.5)).await;
        zyris.release().await;

        assert_eq!(zyris.next().await, VoiceEvent::Listening);
        assert_eq!(zyris.next().await, VoiceEvent::Thinking);
        assert_eq!(zyris.next().await, VoiceEvent::Heard { text: "the whole sentence".into() });
        assert!(
            zyris.scribe.audio(0).len() > seconds(2.0),
            "the audio from both sides of the report has to be in the turn"
        );
        zyris.stops().await;
    }

    /// Audio that was already in the channel when the key went down was captured **before** it,
    /// and belongs to nobody. Keeping it would let the room somebody walked in from decide
    /// whether their turn was long enough to be one.
    ///
    /// Handed over **unsettled**, so that the press and two seconds of audio really are waiting
    /// at the same instant and `Session::drain` is the only thing that can separate them.
    #[tokio::test]
    async fn audio_from_before_the_key_went_down_is_not_part_of_the_turn() {
        let mut zyris = running(Scribe::always("invented"));

        zyris.feed_now(&utterance(2.0));
        zyris.press_now();
        settle().await;
        zyris.feed(&utterance(0.1)).await;
        zyris.release().await;

        assert_eq!(zyris.next().await, VoiceEvent::Listening);
        assert_eq!(
            zyris.next().await,
            VoiceEvent::HeardNothing,
            "the two seconds before the press must not count towards the turn being long enough"
        );
        assert_eq!(zyris.scribe.calls(), 0);
        zyris.stops().await;
    }

    /// And the other half of the same rule: audio already captured when the key came **up** is
    /// part of the turn the key is ending. Dropping it cuts the tail off a sentence for a
    /// scheduling reason, which `crate::vad` prices as the error nothing downstream can repair.
    #[tokio::test]
    async fn audio_already_captured_when_the_key_came_up_is_part_of_the_turn() {
        let mut zyris = running(Scribe::always("what was said"));

        zyris.press().await;
        assert_eq!(zyris.next().await, VoiceEvent::Listening);

        zyris.feed_now(&utterance(2.0));
        zyris.release_now();
        settle().await;

        assert_eq!(zyris.next().await, VoiceEvent::Thinking);
        assert_eq!(zyris.next().await, VoiceEvent::Heard { text: "what was said".into() });
        assert!(zyris.scribe.audio(0).len() > seconds(1.0));
        zyris.stops().await;
    }

    /// A device that says something while nothing is being recorded has not ended a turn, and
    /// `VoiceEvent::Failed` means a turn ended. What a window needs in order to say the
    /// microphone is gone is `capture`.s own reporting, which is task 7.s.
    #[tokio::test]
    async fn a_device_problem_with_no_turn_in_progress_is_not_a_failed_turn() {
        let mut zyris = running(Scribe::always("invented"));

        zyris.problem(Recovery::Stop, "the microphone was unplugged").await;

        zyris.says_nothing().await;
        zyris.stops().await;
    }

    /// The chunkers hold a partial frame between calls, so the last 26 ms of one turn would
    /// otherwise lead the next one. A quarter of a second of somebody else.s sentence is not
    /// much and it is still the previous turn.
    #[tokio::test]
    async fn the_tail_of_one_turn_does_not_lead_the_next() {
        let mut zyris = running(Scribe::always("said"));

        // A first hold whose length is deliberately not a whole number of frames, so that
        // something is left in the chunker when it ends.
        zyris.press().await;
        let mut first = utterance(2.0);
        first.extend(std::iter::repeat_n(0.0, 100));
        zyris.feed(&first).await;
        zyris.release().await;
        assert_eq!(zyris.next().await, VoiceEvent::Listening);
        assert_eq!(zyris.next().await, VoiceEvent::Thinking);
        assert_eq!(zyris.next().await, VoiceEvent::Heard { text: "said".into() });

        // A second hold that opens on something unmistakable, because the processor attenuates
        // the quiet onset of a word to within 3% of silence and the amplitudes would then not
        // separate the two cases in the `aec` build (measured 2026-09-15: 0.0045 against
        // 0.0043). A 6 ms full-band burst is past the high-pass and faster than the noise
        // suppressor can adapt to.
        zyris.press().await;
        let mut second_hold = burst(0.006);
        second_hold.extend(utterance(1.0));
        zyris.feed(&second_hold).await;
        zyris.release().await;
        assert_eq!(zyris.next().await, VoiceEvent::Listening);
        assert_eq!(zyris.next().await, VoiceEvent::Thinking);
        assert_eq!(zyris.next().await, VoiceEvent::Heard { text: "said".into() });

        // **Asserted only where the processor is not in the way.** With `aec` on, noise
        // suppression is what a 6 ms full-band burst exists to be removed by: measured on this
        // machine on 2026-09-15 it comes out at 0.0134, against 0.0045 for the quiet onset of a
        // word, and no threshold separates a turn that begins with itself from one that begins
        // with the last 26 ms of the turn before. The property is about buffering and holds in
        // both builds; the plain `voice` build is the one that can see it, and it is the build
        // CI compiles on both runners.
        #[cfg(not(feature = "aec"))]
        {
            let second = zyris.scribe.audio(1);
            let loudest = second[..100].iter().fold(0.0f32, |so_far, s| so_far.max(s.abs()));
            assert!(
                loudest > 0.1,
                "the second turn has to begin with the second turn: it begins with {loudest}, \
                 which is the last of the turn before it, left behind in the chunker"
            );
        }
        zyris.stops().await;
    }

    // -----------------------------------------------------------------------------------------
    // The two floors
    // -----------------------------------------------------------------------------------------

    /// A tapped key is `TooShort`, not forty milliseconds handed to a model.
    #[tokio::test]
    async fn a_tapped_key_never_reaches_the_model() {
        let mut zyris = running(Scribe::always("invented"));

        zyris.press().await;
        zyris.feed(&utterance(0.1)).await;
        zyris.release().await;

        assert_eq!(zyris.next().await, VoiceEvent::Listening);
        assert_eq!(zyris.next().await, VoiceEvent::HeardNothing);
        assert_eq!(zyris.scribe.calls(), 0);
        zyris.stops().await;
    }

    /// **The two floors agree rather than each defending separately.**
    ///
    /// `stt::Stt::transcribe` refuses audio under `stt::MIN_AUDIO` because whisper answers it
    /// with a warning and zero segments. `vad::MIN_SPEECH` is the floor this session applies.
    /// If the second ever stopped covering the first, a turn this session accepted would arrive
    /// at whisper's own guard and come back as an empty transcript — indistinguishable from a
    /// silent room. Both halves are asserted: the arithmetic, and a real turn.
    #[tokio::test]
    async fn the_two_floors_agree_rather_than_each_defending_separately() {
        assert!(
            stt::samples_in(crate::vad::MIN_SPEECH) >= stt::samples_in(stt::MIN_AUDIO),
            "the endpointer's floor has to be the higher one, or this session hands whisper \
             audio whisper refuses"
        );

        let mut zyris = running(Scribe::always("said"));
        zyris.press().await;
        zyris.feed(&utterance(0.6)).await;
        zyris.release().await;

        assert_eq!(zyris.next().await, VoiceEvent::Listening);
        assert_eq!(zyris.next().await, VoiceEvent::Thinking);
        assert_eq!(zyris.next().await, VoiceEvent::Heard { text: "said".into() });
        assert!(
            zyris.scribe.audio(0).len() >= stt::samples_in(stt::MIN_AUDIO),
            "the shortest turn this session accepts still has to be longer than the shortest \
             clip whisper will look at"
        );
        zyris.stops().await;
    }

    // -----------------------------------------------------------------------------------------
    // The silence rule, under a key
    // -----------------------------------------------------------------------------------------

    /// A pause longer than `vad::HANGOVER` in the middle of a hold is **one** turn.
    ///
    /// This is `crate::vad`'s own sentence — "in hold-to-talk it is never paid at all, because
    /// the key release ends the turn" — made a test. A session on `Rule::default` would cut
    /// somebody who stopped to think into two turns while they were still holding the key, and
    /// the first half would already have gone to an agent.
    #[tokio::test]
    async fn a_pause_longer_than_the_hangover_inside_a_hold_is_still_one_turn() {
        let mut zyris = running(Scribe::always("both halves"));

        zyris.press().await;
        zyris.feed(&utterance(1.5)).await;
        zyris.feed(&silence(2.0)).await;
        zyris.feed(&utterance(1.5)).await;
        zyris.release().await;

        assert_eq!(zyris.next().await, VoiceEvent::Listening);
        assert_eq!(zyris.next().await, VoiceEvent::Thinking);
        assert_eq!(zyris.next().await, VoiceEvent::Heard { text: "both halves".into() });
        assert_eq!(zyris.scribe.calls(), 1, "one hold is one turn");
        assert!(
            zyris.scribe.audio(0).len() > seconds(3.0),
            "the pause is inside the turn, so both halves of what was said are in the audio"
        );
        zyris.stops().await;
    }

    /// And the margin still runs: the silence either side of what was said is trimmed, because
    /// `stt::audio_ctx` scales with the length of the clip and a second of nothing is a second
    /// of encoder for nothing.
    #[tokio::test]
    async fn the_silence_either_side_of_an_utterance_is_trimmed_off() {
        let mut zyris = running(Scribe::always("trimmed"));

        zyris.press().await;
        zyris.feed(&silence(1.0)).await;
        zyris.feed(&utterance(1.0)).await;
        zyris.feed(&silence(1.0)).await;
        zyris.release().await;

        assert_eq!(zyris.next().await, VoiceEvent::Listening);
        assert_eq!(zyris.next().await, VoiceEvent::Thinking);
        assert_eq!(zyris.next().await, VoiceEvent::Heard { text: "trimmed".into() });

        let kept = zyris.scribe.audio(0).len();
        assert!(
            kept < seconds(2.0),
            "two of the three seconds fed were silence; {kept} samples is not a trim"
        );
        assert!(kept > seconds(0.9), "the sentence itself must survive the trim");
        zyris.stops().await;
    }

    // -----------------------------------------------------------------------------------------
    // A press and a release that are not well formed
    // -----------------------------------------------------------------------------------------

    #[tokio::test]
    async fn a_release_nobody_pressed_publishes_nothing() {
        let mut zyris = running(Scribe::always("invented"));

        zyris.release().await;
        zyris.feed(&utterance(1.0)).await;
        zyris.release().await;

        zyris.says_nothing().await;
        assert_eq!(zyris.scribe.calls(), 0);
        zyris.stops().await;
    }

    /// A second press inside one hold must not restart the turn: everything said before the
    /// repeat would go with it, and a repeat is something two of the three backends could
    /// produce.
    #[tokio::test]
    async fn a_second_press_inside_one_hold_does_not_throw_away_what_came_first() {
        let mut zyris = running(Scribe::always("all of it"));

        zyris.press().await;
        zyris.feed(&utterance(2.0)).await;
        zyris.press().await;
        zyris.feed(&utterance(2.0)).await;
        zyris.release().await;

        assert_eq!(zyris.next().await, VoiceEvent::Listening);
        assert_eq!(zyris.next().await, VoiceEvent::Thinking);
        assert_eq!(zyris.next().await, VoiceEvent::Heard { text: "all of it".into() });
        assert_eq!(zyris.scribe.calls(), 1);
        assert!(
            zyris.scribe.audio(0).len() > seconds(3.5),
            "the audio from before the repeated press has to still be in the turn"
        );
        zyris.stops().await;
    }

    // -----------------------------------------------------------------------------------------
    // The cap
    // -----------------------------------------------------------------------------------------

    /// A key that goes down and is never heard of again — which is exactly what a Wayland
    /// desktop whose portal never sends `Deactivated` would produce — ends at [`MAX_TURN`],
    /// and the recording is discarded rather than half a minute of something being transcribed.
    #[tokio::test]
    async fn a_key_that_is_never_released_ends_at_the_cap() {
        let mut zyris = running(Scribe::always("invented"));

        zyris.press().await;
        zyris.feed(&tone(MAX_TURN.as_secs_f32() + 1.0)).await;

        assert_eq!(zyris.next().await, VoiceEvent::Listening);
        assert_eq!(zyris.next().await, VoiceEvent::Failed { reason: turn_too_long() });
        assert_eq!(
            zyris.scribe.calls(),
            0,
            "a turn that reached the cap is either a truncated sentence or something that is \
             not speech; both are worse transcribed than discarded"
        );
        zyris.stops().await;
    }

    /// And it fires once, not once every [`MAX_TURN`]: a stuck key produces one failure and
    /// then silence, and the release that finally arrives is an orphan.
    #[tokio::test]
    async fn a_capped_turn_does_not_start_another_one_by_itself() {
        let mut zyris = running(Scribe::always("invented"));

        zyris.press().await;
        zyris.feed(&tone(MAX_TURN.as_secs_f32() + 1.0)).await;
        assert_eq!(zyris.next().await, VoiceEvent::Listening);
        assert_eq!(zyris.next().await, VoiceEvent::Failed { reason: turn_too_long() });

        zyris.feed(&utterance(2.0)).await;
        zyris.release().await;

        zyris.says_nothing().await;
        assert_eq!(zyris.scribe.calls(), 0);
        zyris.stops().await;
    }

    /// The cap is the buffer's cap too, and there is no second number that can disagree with
    /// the first.
    #[test]
    fn the_recording_cannot_outgrow_the_turn() {
        assert_eq!(max_turn_frames() * VAD_FRAME, stt::samples_in(MAX_TURN));
        assert_eq!(MAX_TURN, stt::FULL_WINDOW);
        assert_eq!(
            push_to_talk_rule().hangover,
            MAX_TURN,
            "the hangover has to be at least the cap, or it ends a held turn before the cap does"
        );
        assert_eq!(push_to_talk_rule().min_speech, crate::vad::MIN_SPEECH);
        assert_eq!(push_to_talk_rule().margin, crate::vad::MARGIN);
    }

    // -----------------------------------------------------------------------------------------
    // What whisper answered
    // -----------------------------------------------------------------------------------------

    /// whisper's annotations for audio it found no speech in arrive as the empty string from
    /// `stt::clean`. An empty transcript is not an empty sentence.
    #[tokio::test]
    async fn a_transcript_of_nothing_is_heard_nothing_rather_than_empty_text() {
        let mut zyris = running(Scribe::saying([Ok(String::new())]));

        zyris.press().await;
        zyris.feed(&utterance(2.0)).await;
        zyris.release().await;

        assert_eq!(zyris.next().await, VoiceEvent::Listening);
        assert_eq!(zyris.next().await, VoiceEvent::Thinking);
        assert_eq!(zyris.next().await, VoiceEvent::HeardNothing);
        zyris.stops().await;
    }

    #[tokio::test]
    async fn a_transcription_that_fails_says_why() {
        let fault = stt::Fault::Whisper { detail: "the model is not loaded".into() };
        let mut zyris = running(Scribe::saying([Err(fault.clone())]));

        zyris.press().await;
        zyris.feed(&utterance(2.0)).await;
        zyris.release().await;

        assert_eq!(zyris.next().await, VoiceEvent::Listening);
        assert_eq!(zyris.next().await, VoiceEvent::Thinking);
        assert_eq!(zyris.next().await, VoiceEvent::Failed { reason: fault.to_string() });
        zyris.stops().await;
    }

    // -----------------------------------------------------------------------------------------
    // A key pressed again while a transcription is still running
    // -----------------------------------------------------------------------------------------

    /// `state.full()` blocks and runs on `spawn_blocking`, so a second hold can begin before the
    /// first has been transcribed. Recording is the part that cannot be recovered, so the new
    /// turn starts at once; the transcriptions run one after the other.
    #[tokio::test]
    async fn a_key_pressed_while_a_transcription_is_running_records_anyway() {
        let (scribe, open) = Scribe::gated("said");
        let mut zyris = running(scribe);

        zyris.press().await;
        zyris.feed(&utterance(2.0)).await;
        zyris.release().await;
        assert_eq!(zyris.next().await, VoiceEvent::Listening);
        assert_eq!(zyris.next().await, VoiceEvent::Thinking);

        // The first transcription is now inside the gate and will not come back until told.
        zyris.press().await;
        zyris.feed(&utterance(2.0)).await;
        zyris.release().await;
        assert_eq!(
            zyris.next().await,
            VoiceEvent::Listening,
            "the second hold has to start recording while the first is still being transcribed"
        );
        assert_eq!(zyris.next().await, VoiceEvent::Thinking);

        open.send(()).expect("the gate is held by the scribe");
        open.send(()).expect("the gate is held by the scribe");
        assert_eq!(zyris.next().await, VoiceEvent::Heard { text: "said".into() });
        assert_eq!(zyris.next().await, VoiceEvent::Heard { text: "said".into() });
        assert_eq!(zyris.scribe.calls(), 2);
        zyris.stops().await;
    }

    /// A third turn behind two is a machine that has stopped keeping up, and the recording is
    /// refused out loud rather than queued into a memory that has none to spare.
    #[tokio::test]
    async fn a_third_turn_behind_two_is_refused_rather_than_queued() {
        let (scribe, open) = Scribe::gated("said");
        let mut zyris = running(scribe);

        for _ in 0..3 {
            zyris.press().await;
            zyris.feed(&utterance(2.0)).await;
            zyris.release().await;
        }

        // Two turns are taken — one running, one waiting — and the third is refused out loud.
        assert_eq!(zyris.next().await, VoiceEvent::Listening);
        assert_eq!(zyris.next().await, VoiceEvent::Thinking);
        assert_eq!(zyris.next().await, VoiceEvent::Listening);
        assert_eq!(zyris.next().await, VoiceEvent::Thinking);
        assert_eq!(zyris.next().await, VoiceEvent::Listening);
        assert_eq!(
            zyris.next().await,
            VoiceEvent::Failed { reason: FALLING_BEHIND.to_string() }
        );

        for _ in 0..2 {
            open.send(()).expect("the gate is held by the scribe");
        }
        zyris.stops().await;
    }

    // -----------------------------------------------------------------------------------------
    // Key events that were lost, and stopping
    // -----------------------------------------------------------------------------------------

    /// A subscriber that falls behind a `broadcast` is told how many it missed and nothing
    /// about what they were. One of them may have been the release, so the turn ends.
    #[tokio::test]
    async fn losing_key_events_ends_the_turn_rather_than_guessing() {
        let (audio, audio_rx) = mpsc::unbounded_channel();
        // One slot, so that two sends with nobody reading is a lag.
        let (keys, keys_rx) = broadcast::channel(1);
        let (events, mut events_rx) = broadcast::channel(64);
        let apm = Arc::new(Apm::new().expect("a processor"));
        let scribe = Scribe::always("invented");
        let session =
            tokio::spawn(Session::new(audio_rx, keys_rx, apm, scribe.clone(), events).run());

        keys.send(Push::Pressed).expect("the session is subscribed");
        assert_eq!(
            tokio::time::timeout(PATIENCE, events_rx.recv()).await.expect("in time").expect("open"),
            VoiceEvent::Listening
        );
        let _ = audio.send(Captured::Audio(utterance(2.0)));

        // Two more with the session parked on this test's await: the first is overwritten.
        keys.send(Push::Pressed).expect("subscribed");
        keys.send(Push::Released).expect("subscribed");

        assert_eq!(
            tokio::time::timeout(PATIENCE, events_rx.recv()).await.expect("in time").expect("open"),
            VoiceEvent::Failed { reason: KEY_EVENTS_LOST.to_string() }
        );
        assert_eq!(scribe.calls(), 0);

        drop(keys);
        drop(audio);
        tokio::time::timeout(PATIENCE, session).await.expect("ends").expect("no panic");
    }

    #[tokio::test]
    async fn the_session_stops_when_the_key_stream_closes() {
        let zyris = running(Scribe::always("said"));
        let audio = zyris.audio.clone();

        drop(zyris.keys);
        let stopped = tokio::time::timeout(PATIENCE, zyris.session)
            .await
            .expect("the session ends when the key that drives it is gone")
            .expect("no panic");

        assert_eq!(stopped, Stopped::KeyGone);
        drop(audio);
    }

    #[tokio::test]
    async fn the_session_stops_when_the_microphone_goes_away() {
        let mut zyris = running(Scribe::always("said"));

        zyris.press().await;
        assert_eq!(zyris.next().await, VoiceEvent::Listening);
        zyris.unplug().await;

        assert!(matches!(zyris.next().await, VoiceEvent::Failed { .. }));
        let stopped = tokio::time::timeout(PATIENCE, zyris.session)
            .await
            .expect("the session ends when there is no microphone left")
            .expect("no panic");
        assert_eq!(stopped, Stopped::MicrophoneGone);
    }

    // -----------------------------------------------------------------------------------------
    // The lengths a device actually chooses
    // -----------------------------------------------------------------------------------------

    /// 314, 341 and 342 alternating is one real PipeWire stream on this machine; 1024 is ALSA.
    /// None of them is a multiple of `APM_FRAME` or of `VAD_FRAME`, and the processor panics
    /// rather than erroring on a wrong count, so a session that passed a device's own buffer
    /// through would take the process down on somebody's laptop and nowhere else.
    #[tokio::test]
    async fn the_lengths_a_device_chooses_are_not_the_lengths_the_pipeline_wants() {
        let mut zyris = running(Scribe::always("said"));
        let audio = utterance(3.0);

        zyris.press().await;
        let mut at = 0;
        for length in [314usize, 341, 342, 1024].into_iter().cycle() {
            if at >= audio.len() {
                break;
            }
            let end = (at + length).min(audio.len());
            zyris.feed(&audio[at..end]).await;
            at = end;
        }
        zyris.release().await;

        assert_eq!(zyris.next().await, VoiceEvent::Listening);
        assert_eq!(zyris.next().await, VoiceEvent::Thinking);
        assert_eq!(zyris.next().await, VoiceEvent::Heard { text: "said".into() });
        zyris.stops().await;
    }
}

#[cfg(test)]
mod barge_in {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use crate::playback::{Fill, Speaker, offline};

    /// The deadline every waiting assertion in here carries, for `session::tests`' reason: a
    /// `#[tokio::test]` has no timeout of its own, so a state machine that never leaves a state
    /// is a suite that hangs rather than a test that fails.
    const PATIENCE: Duration = Duration::from_secs(10);

    /// Samples per character, so that a fragment's length in the ledger is arithmetic a test can
    /// read rather than a number to look up.
    const PER_CHAR: usize = 1000;

    // -----------------------------------------------------------------------------------------
    // Doubles
    // -----------------------------------------------------------------------------------------

    /// A voice that makes a fragment's length out of its text and nothing else.
    struct Voicebox {
        said: Mutex<Vec<String>>,
        answers: Mutex<VecDeque<Result<Vec<f32>, String>>>,
        /// Held shut until a test opens it, so that a barge-in can happen *during* a synthesis.
        gate: Option<Mutex<std::sync::mpsc::Receiver<()>>>,
    }

    impl Voicebox {
        fn plain() -> Arc<Voicebox> {
            Arc::new(Voicebox {
                said: Mutex::new(Vec::new()),
                answers: Mutex::new(VecDeque::new()),
                gate: None,
            })
        }

        fn gated() -> (Arc<Voicebox>, std::sync::mpsc::Sender<()>) {
            let (open, gate) = std::sync::mpsc::channel();
            let voice = Arc::new(Voicebox {
                said: Mutex::new(Vec::new()),
                answers: Mutex::new(VecDeque::new()),
                gate: Some(Mutex::new(gate)),
            });
            (voice, open)
        }

        fn refusing(reason: &str) -> Arc<Voicebox> {
            Arc::new(Voicebox {
                said: Mutex::new(Vec::new()),
                answers: Mutex::new(VecDeque::from([Err(reason.to_string())])),
                gate: None,
            })
        }

        fn spoken(&self) -> Vec<String> {
            self.said.lock().expect("not poisoned").clone()
        }
    }

    impl Synthesise for Voicebox {
        fn say(&self, text: &str) -> Result<Vec<f32>, String> {
            self.said.lock().expect("not poisoned").push(text.to_string());
            if let Some(gate) = &self.gate {
                let _ = gate.lock().expect("not poisoned").recv();
            }
            self.answers
                .lock()
                .expect("not poisoned")
                .pop_front()
                .unwrap_or_else(|| Ok(vec![0.1; text.chars().count() * PER_CHAR]))
        }
    }

    // -----------------------------------------------------------------------------------------
    // The harness
    // -----------------------------------------------------------------------------------------

    /// A speaker, a voice, a conversation and the real [`Fill`] behind the queue.
    ///
    /// **The callback is the real one**, from `playback::offline`: what a discard does to
    /// `played`, what `pending` says afterwards and where a fragment starts are the rules being
    /// decided here, and a double for them would be a second implementation of exactly that.
    struct Rig {
        speaking: Arc<Speaking>,
        voice: Arc<Voicebox>,
        conversation: Arc<Conversation>,
        speaker: Speaker,
        fill: Fill,
        _tap: mpsc::UnboundedReceiver<Vec<f32>>,
        events: broadcast::Receiver<VoiceEvent>,
    }

    fn rig(voice: Arc<Voicebox>) -> Rig {
        let (speaker, fill, tap) = offline(441);
        let conversation = Arc::new(Conversation::default());
        let (events, events_rx) = broadcast::channel(64);
        let speaking = Speaking::new(
            voice.clone(),
            Arc::new(speaker.clone()),
            conversation.clone(),
            events,
        );
        Rig { speaking, voice, conversation, speaker, fill, _tap: tap, events: events_rx }
    }

    impl Rig {
        /// One callback: `samples` of whatever is queued reach the device.
        fn play(&mut self, samples: usize) {
            let mut out = vec![0.0f32; samples];
            self.fill.deliver(&mut out, 1, None);
        }

        /// Synthesise and queue one fragment, the way the feed would.
        async fn say(&self, text: &str) {
            self.speaking.synthesise(text).await;
        }

        async fn next_event(&mut self) -> VoiceEvent {
            tokio::time::timeout(PATIENCE, self.events.recv())
                .await
                .expect("an event was expected and none arrived")
                .expect("the event stream is open")
        }
    }

    // -----------------------------------------------------------------------------------------
    // The ledger
    // -----------------------------------------------------------------------------------------

    fn ledger() -> Ledger {
        let mut ledger = Ledger::default();
        ledger.add("One.", 0, 1000);
        ledger.add("Two.", 1000, 2000);
        ledger.add("Three.", 3000, 500);
        ledger
    }

    /// Nothing reached the device, so nothing was heard — **not** "the first one was".
    #[test]
    fn a_fragment_the_speaker_has_not_started_is_not_one_that_was_heard() {
        let read = ledger().at(0);

        assert_eq!(read.heard, Vec::<String>::new());
        assert_eq!(read.cut, None);
        assert_eq!(read.unheard, vec!["One.", "Two.", "Three."]);
    }

    /// The exact boundary: `played` equal to a fragment's end is that fragment heard in full and
    /// the next one not begun. Off by one either way and a sentence changes sides.
    #[test]
    fn the_boundary_between_heard_and_not_is_where_the_fragment_ends() {
        let read = ledger().at(1000);

        assert_eq!(read.heard, vec!["One."]);
        assert_eq!(read.cut, None, "the second has not started");
        assert_eq!(read.unheard, vec!["Two.", "Three."]);
    }

    /// Part way through the second, which is the case the whole message exists for.
    #[test]
    fn a_fragment_the_speaker_was_in_the_middle_of_says_how_far_it_got() {
        let read = ledger().at(2000);

        assert_eq!(read.heard, vec!["One."]);
        assert_eq!(
            read.cut,
            Some(Cut {
                text: "Two.".to_string(),
                at: played_for(1000),
                of: played_for(2000),
            })
        );
        assert_eq!(read.unheard, vec!["Three."]);
    }

    /// Everything queued reached the device. There is nothing to tell anybody about.
    #[test]
    fn a_speaker_that_finished_missed_nothing() {
        let read = ledger().at(3500);

        assert_eq!(read.heard, vec!["One.", "Two.", "Three."]);
        assert!(!read.anything_missed());
    }

    // -----------------------------------------------------------------------------------------
    // The message
    // -----------------------------------------------------------------------------------------

    /// **The copy may not claim the agent's own record is short**, and it is not: generation
    /// finishes long before speech does, so the answer was written whether or not it was heard.
    /// Three tasks of this project have shipped copy claiming more than the code does.
    #[test]
    fn the_message_says_what_was_heard_and_does_not_claim_the_record_is_truncated() {
        let message = ledger().at(2000).message();

        assert!(message.contains("\u{201c}One.\u{201d}"), "{message}");
        assert!(message.contains("Heard 0.0s of 0.0s"), "one second at 44.1 kHz is not a second");
        assert!(message.contains("\u{201c}Two.\u{201d}"), "{message}");
        assert!(message.contains("Not heard at all: \u{201c}Three.\u{201d}"), "{message}");
        assert!(
            message.contains("Your own record of that answer is complete"),
            "the one thing this message must not leave a reader believing is that their own \
             transcript was cut short: {message}"
        );
        assert!(
            message.contains("most of what you wrote had already been written"),
            "and the sentence after it is half of what makes that believable — an agent told \
             only that its record is complete has no reason to think so, since the speech it \
             was writing for stopped: {message}"
        );
    }

    /// A key pressed before a word of the answer came out. "None of it was heard" rather than a
    /// message with nothing in it, which reads as a formatting bug.
    #[test]
    fn a_message_about_speech_that_never_started_says_so() {
        let message = ledger().at(0).message();

        assert!(message.contains("None of it was heard."), "{message}");
        assert!(!message.contains("Heard in full"), "{message}");
    }

    // -----------------------------------------------------------------------------------------
    // Stopping
    // -----------------------------------------------------------------------------------------

    /// **The whole of task 4, in one test.** Two sentences queued, one and a half played, and
    /// the key goes down.
    ///
    /// The third assertion is the discriminator that matters: audio still on the queue must not
    /// be counted as audio the person heard. `Fill` counts a discard into `discarded` and not
    /// into `played` for exactly this, and a version that did the other thing would report every
    /// queued sentence as spoken and cancel a turn saying so.
    #[tokio::test]
    async fn a_key_pressed_part_way_through_an_answer_stops_it_and_says_where() {
        let mut rig = rig(Voicebox::plain());
        rig.say("Yes.").await; // 4 characters, 4000 samples
        rig.say("Here it is.").await; // 11 characters, 11000 samples
        rig.play(6000);

        let interruption = rig.speaking.stop().expect("the speaker had not finished");

        assert_eq!(interruption.heard, vec!["Yes."]);
        assert_eq!(
            interruption.cut.as_ref().map(|cut| cut.text.as_str()),
            Some("Here it is."),
            "the second sentence was two thousand samples in when the key went down"
        );
        assert_eq!(interruption.unheard, Vec::<String>::new());

        // And the speaker really is stopped: the discard happens in the callback.
        rig.play(6000);
        assert_eq!(rig.speaker.played(), 6000, "nothing more reached the device");
        assert_eq!(rig.speaker.pending(), 0, "and nothing is still waiting");

        assert_eq!(
            rig.speaking.stop(),
            None,
            "and the ledger went with it: a second press must not report the same sentence cut \
             off twice, into a turn that has already been cancelled once"
        );
    }

    /// A key pressed between answers interrupts nothing, and **must not post a message**.
    ///
    /// Without this a person who pressed the key to say a second thing would put a "your answer
    /// was cut off" note into the session after every completed answer.
    #[tokio::test]
    async fn a_key_pressed_after_the_answer_finished_interrupts_nothing() {
        let mut rig = rig(Voicebox::plain());
        rig.say("Yes.").await;
        rig.play(4000);

        assert_eq!(rig.speaking.stop(), None);

        rig.speaking.interrupt();
        settle().await;
        assert_eq!(rig.conversation.told(), Vec::new(), "nothing was said to Attacca");
    }

    /// **The turn is cancelled before the message is posted.** The other order posts a message
    /// into a turn that is still generating, and the server may interleave the two.
    #[tokio::test]
    async fn the_turn_is_cancelled_before_the_interruption_is_recorded() {
        let mut rig = rig(Voicebox::plain());
        rig.say("Yes.").await;
        rig.play(1000);

        let interruption = rig.speaking.stop().expect("the speaker had not finished");
        rig.speaking.record(interruption).await;

        let told = rig.conversation.told();
        assert_eq!(told.len(), 2);
        assert_eq!(told[0], Told::Cancel);
        assert!(matches!(told[1], Told::Said(_)));
        assert!(
            rig.conversation.message().contains("\u{201c}Yes.\u{201d}"),
            "and the message that was posted is the one the interruption describes: {:?}",
            rig.conversation.message()
        );
    }

    /// **A fragment that finished being synthesised after the key went down is thrown away.**
    ///
    /// Synthesis is seconds on this machine, so a person who interrupts is nearly always
    /// interrupting during one. Without this they hear the sentence they cut off, several
    /// seconds after cutting it off, over whatever they said instead.
    #[tokio::test]
    async fn a_sentence_that_was_still_being_made_when_the_key_went_down_is_not_played() {
        let (voice, open) = Voicebox::gated();
        let mut rig = rig(voice);

        let speaking = rig.speaking.clone();
        let synthesising =
            tokio::spawn(async move { speaking.synthesise("The answer is this.").await });

        // Wait for the synthesis to have started, then interrupt it.
        let started = tokio::time::Instant::now();
        while rig.voice.spoken().is_empty() {
            assert!(started.elapsed() < PATIENCE, "the synthesis never started");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(rig.speaking.stop(), None, "nothing had reached the speaker yet");
        open.send(()).expect("the synthesis is waiting");
        synthesising.await.expect("the synthesis task does not panic");

        assert_eq!(rig.speaker.pending(), 0, "the finished sentence was not queued");
        rig.play(4096);
        assert_eq!(rig.speaker.played(), 0, "and nothing was played");
    }

    /// A voice that refused says so on the event stream rather than going quiet. A synthesiser
    /// that has stopped working and a turn with nothing sayable in it are the same silence
    /// otherwise.
    #[tokio::test]
    async fn a_fragment_the_voice_refused_is_reported_rather_than_swallowed() {
        let mut rig = rig(Voicebox::refusing("the vocoder would not load"));

        rig.say("Yes.").await;

        assert_eq!(
            rig.next_event().await,
            VoiceEvent::Failed { reason: "the vocoder would not load".to_string() }
        );
    }

    /// The first fragment to reach the speaker is what `Speaking` means, and it is published
    /// once per turn rather than once per sentence.
    #[tokio::test]
    async fn the_answer_being_read_aloud_is_announced_once() {
        let mut rig = rig(Voicebox::plain());

        rig.say("One.").await;
        rig.say("Two.").await;

        assert_eq!(rig.next_event().await, VoiceEvent::Speaking);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), rig.events.recv()).await.is_err(),
            "a second sentence is not a second announcement"
        );
    }

    /// **The end of a turn is not the end of the speaking**, and a window told otherwise would
    /// show `Speaking` over a speaker that stopped a minute ago — or stop showing it while the
    /// machine is still talking. `Spoke` is published when the queue is empty, not when the
    /// agent stopped writing.
    #[tokio::test]
    async fn the_speaker_finishing_is_a_different_moment_from_the_answer_finishing() {
        let mut rig = rig(Voicebox::plain());
        rig.say("Yes.").await;
        assert_eq!(rig.next_event().await, VoiceEvent::Speaking);

        // The turn ends with four thousand samples still queued.
        let speaking = rig.speaking.clone();
        let draining = tokio::spawn(async move { speaking.drained().await });
        assert!(
            tokio::time::timeout(Duration::from_millis(100), rig.events.recv()).await.is_err(),
            "the answer is finished and the speaker is not"
        );

        rig.play(4000);
        tokio::time::timeout(PATIENCE, draining)
            .await
            .expect("the drain watch has to end when the queue does")
            .expect("it does not panic");
        assert_eq!(rig.next_event().await, VoiceEvent::Spoke);
    }

    /// The same drain watch, interrupted: whoever pressed the key owns the ending, and
    /// publishing `Spoke` as well would say the answer was finished being read out.
    #[tokio::test]
    async fn an_interrupted_answer_does_not_also_report_that_it_finished() {
        let mut rig = rig(Voicebox::plain());
        rig.say("Yes.").await;
        assert_eq!(rig.next_event().await, VoiceEvent::Speaking);

        let speaking = rig.speaking.clone();
        let draining = tokio::spawn(async move { speaking.drained().await });
        // The watch has to be waiting before the key goes down, which is the ordinary case: a
        // turn ends, the speaker is still talking, and a person interrupts what is left.
        settle().await;
        rig.play(1000);
        rig.speaking.interrupt();

        tokio::time::timeout(PATIENCE, draining)
            .await
            .expect("the drain watch has to notice the interruption")
            .expect("it does not panic");

        assert_eq!(rig.next_event().await, VoiceEvent::Interrupted);
        assert!(
            tokio::time::timeout(Duration::from_millis(200), rig.events.recv()).await.is_err(),
            "`Spoke` would say the answer was read to the end, which is the opposite of what \
             happened"
        );
    }

    /// Let spawned work run. `session::tests::settle`'s reason, and its shape.
    async fn settle() {
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
    }

    // -----------------------------------------------------------------------------------------
    // Through the session
    // -----------------------------------------------------------------------------------------

    /// **The wiring**: the push-to-talk key is what barge-in is, so the stopping has to happen
    /// on the press and not somewhere a test reaches directly.
    #[tokio::test]
    async fn the_key_going_down_is_what_stops_the_speaker() {
        let mut rig = rig(Voicebox::plain());
        rig.say("Here is a long answer.").await;
        rig.play(1000);

        let (audio, audio_rx) = mpsc::unbounded_channel::<Captured>();
        let (keys, keys_rx) = broadcast::channel(8);
        let (events, _events_rx) = broadcast::channel(64);
        let apm = Arc::new(Apm::new().expect("a processor this machine can build"));
        let session = Session::new(audio_rx, keys_rx, apm, Scribe::always(""), events)
            .speaking(rig.speaking.clone());
        let running = tokio::spawn(session.run());

        keys.send(Push::Pressed).expect("the session is listening");
        // Waited for without playing anything, so that the assertion below is about the key and
        // not about how many samples the waiting happened to consume.
        let stopped = tokio::time::Instant::now();
        while rig.speaker.counters().discarded() == 0 && rig.speaker.pending() > 0 {
            assert!(stopped.elapsed() < PATIENCE, "the key press never reached the speaker");
            tokio::task::yield_now().await;
            rig.play(0);
        }

        rig.play(4096);
        assert_eq!(rig.speaker.played(), 1000, "nothing was played after the key went down");
        assert_eq!(rig.speaker.pending(), 0, "and the queue was thrown away");
        drop(audio);
        drop(keys);
        let _ = tokio::time::timeout(PATIENCE, running).await;
    }

    /// A session with no speaker is every session step 7 built, and a key pressed in one must
    /// still start a turn rather than reaching for something that is not there.
    #[tokio::test]
    async fn a_session_with_no_speaker_still_starts_a_turn() {
        let (audio, audio_rx) = mpsc::unbounded_channel::<Captured>();
        let (keys, keys_rx) = broadcast::channel(8);
        let (events, mut events_rx) = broadcast::channel(64);
        let apm = Arc::new(Apm::new().expect("a processor this machine can build"));
        let session = Session::new(audio_rx, keys_rx, apm, Scribe::always(""), events);
        let running = tokio::spawn(session.run());

        keys.send(Push::Pressed).expect("the session is listening");

        assert_eq!(
            tokio::time::timeout(PATIENCE, events_rx.recv())
                .await
                .expect("a turn has to start")
                .expect("the stream is open"),
            VoiceEvent::Listening
        );
        drop(audio);
        drop(keys);
        let _ = tokio::time::timeout(PATIENCE, running).await;
    }

    /// **The worker, end to end**: fragments off a feed become audio at the speaker, and the
    /// end of the turn becomes the end of the speaking.
    ///
    /// `Speaking::run` is what `run::Engine` spawns, and nothing else in this module reaches it:
    /// every test above drives `synthesise` and `drained` directly, which leaves the `match` that
    /// maps a `TurnEvent` onto them untested. A `Say` read as a `Shown` would be a machine that
    /// never says anything, with no error anywhere.
    #[tokio::test]
    async fn what_the_feed_says_to_say_is_what_reaches_the_speaker() {
        let mut rig = rig(Voicebox::plain());
        let (turns, subscription) = broadcast::channel(16);

        let worker = tokio::spawn(rig.speaking.clone().run(subscription));
        turns
            .send(crate::turn::TurnEvent::Shown {
                kind: crate::speak::Kind::Assistant,
                text: "Yes.".to_string(),
            })
            .expect("the worker is reading");
        turns
            .send(crate::turn::TurnEvent::Say(crate::split::Fragment::spoken("Yes.")))
            .expect("the worker is reading");

        assert_eq!(rig.next_event().await, VoiceEvent::Speaking);
        assert_eq!(rig.voice.spoken(), vec!["Yes."], "and only the fragment, not the delta");
        assert_eq!(rig.speaker.pending(), 4 * PER_CHAR as u64);

        turns.send(crate::turn::TurnEvent::Running(false)).expect("the worker is reading");
        settle().await;
        rig.play(4 * PER_CHAR);
        assert_eq!(rig.next_event().await, VoiceEvent::Spoke);

        drop(turns);
        tokio::time::timeout(PATIENCE, worker)
            .await
            .expect("the worker ends when the feed does, or a stopped session leaks a task")
            .expect("it does not panic");
    }

    /// The double `session::tests` already has, reached through its module so that there is one
    /// of it.
    use super::tests::Scribe;
}
