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
        }
    }

    /// Run until the key stream or the microphone goes away.
    pub async fn run(mut self) -> Stopped {
        loop {
            let woke = {
                let pending = &mut self.pending;
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
                Woke::Key(Ok(Push::Pressed)) => self.pressed(),
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
            }
        }
    }

    /// One buffer from the device: condition it, cut it into detector frames, and record it.
    fn heard(&mut self, samples: &[f32]) {
        if self.turn.is_none() {
            // Not recording. The audio is dropped rather than buffered, and the endpointer is
            // not advanced — which is what keeps a turn.s first frame equal to its `turn_start`.
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
            if self.turn.is_none() {
                // The cap ended the turn part way through this buffer. The rest belongs to
                // nothing.
                break;
            }
            self.frame(frame);
        }
        self.ready = ready;
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
            // unreachable unless that stops being true.
            Err(_) => return,
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
        match captured {
            Captured::Audio(samples) => self.heard(&samples),
            // A rerouted default stream reports and keeps running; ending the turn on it would
            // end every turn a person started while plugging in a headset.
            Captured::Problem(problem) if problem.recovery != Recovery::Continue => {
                self.abort(problem.reason)
            }
            Captured::Problem(_) => {}
        }
    }

    /// The key went down.
    fn pressed(&mut self) {
        // Before anything else: what is already in the channel is not part of this turn.
        self.drain();
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
            Ended::TooShort { .. } => self.publish(VoiceEvent::HeardNothing),
            Ended::Utterance { first, last, .. } => {
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
        if self.pending.is_none() {
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
        match done {
            // An empty transcript is not an empty sentence. `stt::clean` turns whisper's own
            // annotations for audio it found no speech in — `[BLANK_AUDIO]`, `(silence)` —
            // into exactly this, and a window shows "nobody spoke" differently from "".
            Ok(Ok(text)) if text.is_empty() => self.publish(VoiceEvent::HeardNothing),
            Ok(Ok(text)) => self.publish(VoiceEvent::Heard { text }),
            Ok(Err(fault)) => self.publish(VoiceEvent::Failed { reason: fault.to_string() }),
            Err(_) => {
                self.publish(VoiceEvent::Failed { reason: stt::Fault::Lost.to_string() })
            }
        }
        if let Some(next) = self.queued.take() {
            self.pending = Some(self.spawn(next));
        }
    }

    /// `send` fails only when nobody is subscribed, which is the ordinary state of a machine
    /// whose window is closed. Discarded on purpose.
    fn publish(&self, event: VoiceEvent) {
        let _ = self.events.send(event);
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
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audio/jfk.wav");
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");

        let (mut rate, mut channels, mut bits) = (0u32, 0u16, 0u16);
        let mut samples = Vec::new();
        let mut at = 12;
        while at + 8 <= bytes.len() {
            let id = &bytes[at..at + 4];
            let size =
                u32::from_le_bytes(bytes[at + 4..at + 8].try_into().expect("4 bytes")) as usize;
            let body = &bytes[at + 8..(at + 8 + size).min(bytes.len())];
            match id {
                b"fmt " => {
                    channels = u16::from_le_bytes(body[2..4].try_into().expect("2 bytes"));
                    rate = u32::from_le_bytes(body[4..8].try_into().expect("4 bytes"));
                    bits = u16::from_le_bytes(body[14..16].try_into().expect("2 bytes"));
                }
                b"data" => {
                    samples = body
                        .chunks_exact(2)
                        .map(|s| {
                            f32::from(i16::from_le_bytes(s.try_into().expect("2 bytes")))
                                / 32768.0
                        })
                        .collect();
                }
                _ => {}
            }
            at += 8 + size + (size & 1);
        }
        assert_eq!((rate, channels, bits), (SAMPLE_RATE, 1, 16));
        assert!(!samples.is_empty());
        samples
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
    struct Scribe {
        heard: Mutex<Vec<Vec<f32>>>,
        answers: Mutex<VecDeque<Result<String, stt::Fault>>>,
        /// When set, every call blocks until a token is put on it. This is how "a key pressed
        /// again while a transcription is still running" is made a sequence rather than a race.
        gate: Option<Mutex<std::sync::mpsc::Receiver<()>>>,
    }

    impl Scribe {
        fn saying(answers: impl IntoIterator<Item = Result<String, stt::Fault>>) -> Arc<Scribe> {
            Arc::new(Scribe {
                heard: Mutex::new(Vec::new()),
                answers: Mutex::new(answers.into_iter().collect()),
                gate: None,
            })
        }

        fn always(text: &str) -> Arc<Scribe> {
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
