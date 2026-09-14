//! Knowing when somebody stopped talking — and the rule that decides it, which is the whole of
//! what this module is for.
//!
//! # Why this is not `webrtc-audio-processing`
//!
//! The 2.x `Config` is `pipeline`, `capture_amplifier`, `high_pass_filter`, `echo_canceller`,
//! `noise_suppression` and `gain_controller`. The old APM's `voice_detection` is gone, so there
//! is no flag to turn on and the detector has to be its own crate.
//!
//! [`earshot`] 1.2.2 is that crate: a neural detector with **no external dependency and no
//! model file** — 40 KB of weights compiled in — costing **23.25 µs per 256-sample frame**, or
//! 0.145% of one core on this machine. It was measured side by side against Silero through ONNX
//! Runtime and is 10.2x faster, with no 4 MB runtime downloaded during the build. Unlike the
//! processor in [`crate::apm`] it is behind the plain `voice` feature, so **CI compiles and
//! runs everything below on both runners**.
//!
//! # Two things `earshot` documents that are true only of a release build
//!
//! Both were measured on this machine on 2026-09-15, and both would otherwise be discovered as
//! a crash in a test suite rather than as a note:
//!
//! - **A wrong frame length returns `-1.0`** — in release. `predict_f32` opens with
//!   `debug_assert_eq!(frame.len(), 256)`, so a debug build, which every `cargo test` is,
//!   **panics** instead. An empty slice does the same.
//! - **Samples must be within `[-1, 1]`** and there is a `debug_assert!` for that too, so one
//!   hot sample takes a debug build down. It is reachable from a real microphone: `rubato`'s
//!   FFT resampler can ring slightly past full scale on loud input.
//!
//! So [`Endpointer::push`] checks the length itself and returns [`Fault::WrongLength`], and it
//! clamps every sample into range before the detector sees it. A `-1.0` would be no use as a
//! signal in any case: it is below every threshold, so a caller that let it through would read
//! a length bug as a room that had gone quiet forever.
//!
//! # The rule that ends a turn
//!
//! Four numbers, and every one of them is a decision with a cost on each side. The recording
//! they are argued from is `tests/audio/jfk.wav`, which is in this repository for that reason.
//!
//! ## [`THRESHOLD`] — 0.5
//!
//! `earshot`'s own suggested operating point, left there rather than tuned, because the
//! measurements either side price the alternatives and neither is cheaper. Over two seconds of
//! digital silence the highest score is 0.275, and over a quiet room 0.312; over `jfk.wav`,
//! 67.5% of frames are above 0.5. Lowering it to 0.3 shortens the worst pause inside that
//! sentence from 1040 ms to 896 ms — but −26 dBFS of room noise peaks at 0.629, so in a noisy
//! room a threshold of 0.3 is a turn that never ends. Raising it to 0.7 stretches the same
//! pause to 1232 ms and costs a longer hangover.
//!
//! ## [`HANGOVER`] — 1200 ms of continuous silence ends the turn
//!
//! **This is the number the module exists to get right.** `jfk.wav` is one sentence with two
//! deliberate pauses in it, measured at this threshold as **1040 ms** (from 2.272 s) and
//! **1024 ms** (from 4.400 s). A rule shorter than those cuts one sentence into three, and
//! `a_hangover_short_enough_to_be_tempting_cuts_that_sentence_into_three` demonstrates it with
//! the 700 ms that most end-of-speech detection uses.
//!
//! The two errors are not the same size:
//!
//! - **Ending too early truncates somebody mid-sentence, and nothing downstream can repair it.**
//!   The spec's state machine has no way to join two turns; the first half has already gone to
//!   whisper and from there to an agent that may act on it. A half-sentence is not a shorter
//!   instruction, it is a different one.
//! - **Ending too late costs the hangover, once.** 1.2 s, against whisper's own 0.33 s for a 3 s
//!   utterance on this machine — and in hold-to-talk it is not paid at all, because
//!   [`Endpointer::finish`] ends the turn the instant the key comes up.
//!
//! So when the evidence is one recording the rule should err late, and 1200 ms is that erring,
//! bounded: 160 ms of margin over the longest pause measured. The speaker in that recording is
//! deliberate and slow, which is the conservative direction on purpose. The number to revisit
//! when there is a corpus of ordinary conversation is this one, and
//! `the_hangover_is_longer_than_the_longest_pause_measured_inside_a_sentence` is what will fail
//! if somebody lowers it without one.
//!
//! ## [`MIN_SPEECH`] — 300 ms of speech, or it was not a turn
//!
//! What stops a cough, a door or a hand on a desk becoming an instruction. **Here the asymmetry
//! runs the other way**, which is why this floor errs high while the hangover errs late: a turn
//! wrongly rejected costs a person saying "no" twice, and it is visible to them. A turn wrongly
//! accepted goes to whisper, which is well known for inventing fluent sentences out of noise,
//! and the sentence it invents goes to an agent that can act on it. Missing a word is
//! recoverable; acting on a door is not.
//!
//! 300 ms sits above what an impulse produces — a single full-scale click puts **zero** frames
//! over the threshold here, and a 100 ms full-band burst puts exactly **one**, 16 ms — and below
//! a spoken one-syllable word. It is counted as total speech in the turn rather than as one
//! unbroken run, so a hesitant "no…" still clears it. It is the number in this module with the
//! least evidence behind it, and the honest thing to say is that it wants a corpus of real short
//! commands rather than an argument.
//!
//! ## [`MARGIN`] — 200 ms kept either side
//!
//! A detector that says speech began at frame N is saying it was already under way: the attack
//! of the first consonant is in the frames before it, and a plosive's release is in the frames
//! after the last. One number for both ends rather than two, because there is no measurement
//! that would distinguish them.
//!
//! Trimming the *rest* of the trailing silence is not a nicety. Task 5 scales whisper's
//! `audio_ctx` by how long the clip is, so 1.2 s of hangover left in a 3 s utterance is a third
//! of the work for nothing.
//!
//! # What this module does not own
//!
//! The audio. [`Endpointer`] reports frame **indices** into the stream it was handed, and task
//! 6's session is what keeps the buffer and cuts it. Two copies of the audio, one of them kept
//! by something whose job is arithmetic, is not a trade worth making — and it keeps every test
//! here a test of the rule.

use std::time::Duration;

use crate::capture::VAD_FRAME;

/// A score at or above this is a voice. `earshot`'s own suggestion; see the module.
pub const THRESHOLD: f32 = 0.5;

/// Continuous silence after speech that ends the turn.
///
/// Longer than the 1040 ms pause measured inside `tests/audio/jfk.wav`, deliberately. See the
/// module for the whole argument, which is most of why this module exists.
pub const HANGOVER: Duration = Duration::from_millis(1200);

/// The least total speech a turn can be made of. Below it, the turn ends as nothing.
pub const MIN_SPEECH: Duration = Duration::from_millis(300);

/// Audio kept either side of the speech the detector agreed about.
pub const MARGIN: Duration = Duration::from_millis(200);

/// How a turn ends. Every field is a decision; see the module for the argument for each.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rule {
    /// A score at or above this is a voice.
    pub threshold: f32,
    /// Continuous silence after speech that ends the turn. Never less than one frame.
    pub hangover: Duration,
    /// Total speech below this is not a turn.
    pub min_speech: Duration,
    /// Audio kept either side of the speech.
    pub margin: Duration,
}

impl Default for Rule {
    fn default() -> Rule {
        Rule { threshold: THRESHOLD, hangover: HANGOVER, min_speech: MIN_SPEECH, margin: MARGIN }
    }
}

/// What the endpointer says about the frame just handed to it.
///
/// Four answers rather than a boolean because a caller acts differently on each: audio before a
/// turn has started can be dropped down to the margin, audio inside one has to be kept, and the
/// difference between "waiting out the hangover" and "over" is the difference between a window
/// that still says Listening and one that says Thinking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listening {
    /// Nothing has been said in this turn yet. The hangover is not running.
    Quiet,
    /// Inside an utterance.
    Speech,
    /// Speech has stopped and the hangover is running. It may yet turn back into speech.
    Trailing,
    /// The turn ended on this frame, and the next one belongs to the turn after it.
    Ended(Ended),
}

/// How a turn finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// A turn worth transcribing.
    ///
    /// `first` and `last` are **inclusive frame indices into the stream handed to
    /// [`Endpointer::push`]** since the last [`Endpointer::reset`], not into this turn. They
    /// already carry [`Rule::margin`] on each end and are clamped so that neither can name a
    /// frame the caller does not have.
    Utterance {
        /// First frame to keep, inclusive.
        first: usize,
        /// Last frame to keep, inclusive. The trailing silence past it is trimmed.
        last: usize,
        /// How much of the turn was speech — not how long the turn was.
        speech: Duration,
    },
    /// Something ended the turn and there was not enough speech in it to be one.
    ///
    /// Its own answer rather than an [`Ended::Utterance`] of nothing, because a window shows the
    /// two differently and [`crate::VoiceEvent::HeardNothing`] exists for exactly this.
    TooShort {
        /// How much speech there was, which may be none at all.
        speech: Duration,
    },
}

/// Something one frame could not be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// The frame was not exactly [`VAD_FRAME`] samples.
    ///
    /// **Ours, not `earshot`'s.** See the module for why its documented `-1.0` is not something
    /// to rely on, and why a score would be the wrong shape for this even if it were.
    WrongLength {
        /// What was required — always [`VAD_FRAME`].
        wanted: usize,
        /// What arrived.
        got: usize,
    },
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fault::WrongLength { wanted, got } => {
                write!(f, "the voice detector takes exactly {wanted} samples and got {got}")
            }
        }
    }
}

impl std::error::Error for Fault {}

/// How long one frame lasts: 16 ms, derived rather than typed.
pub const fn frame_duration() -> Duration {
    Duration::from_nanos(VAD_FRAME as u64 * 1_000_000_000 / crate::capture::SAMPLE_RATE as u64)
}

/// How many frames a duration is, **rounded up**, so that a rule written in milliseconds is
/// never quietly shortened by the frame grid.
pub fn frames_in(span: Duration) -> usize {
    span.as_nanos().div_ceil(frame_duration().as_nanos()) as usize
}

/// Decides when a turn is over.
///
/// One per capture stream, driven one frame at a time. It holds **no audio**: what it reports
/// are indices into whatever the caller is buffering, because two copies of the recording — one
/// of them kept by something whose job is arithmetic — is not a trade worth making.
pub struct Endpointer {
    /// Boxed on purpose: `Detector` is about 8 KiB of state and its own documentation asks for
    /// this rather than a stack copy. `default_boxed` builds it on the heap directly instead of
    /// building 8 KiB on the stack and moving it.
    detector: Box<earshot::Detector>,
    rule: Rule,
    /// Frames pushed since the last [`Endpointer::reset`]. The caller's buffer is indexed by it.
    index: usize,
    /// The first frame of the current turn: where a margin reaching backwards has to stop, so
    /// that one turn cannot claim audio the turn before it already had.
    turn_start: usize,
    /// `None` until something is said, which is what keeps the hangover from running over a
    /// room nobody has spoken in.
    first_speech: Option<usize>,
    last_speech: usize,
    speech_frames: usize,
    silence_run: usize,
    /// Somewhere to clamp into. A frame is 1 KiB and this runs 62 times a second, so one
    /// reusable buffer rather than an allocation per frame.
    clamped: [f32; VAD_FRAME],
}

impl Endpointer {
    /// One with [`Rule::default`], which is the rule this module argues for.
    pub fn new() -> Endpointer {
        Endpointer::with_rule(Rule::default())
    }

    /// One with a rule of your own. The window offers it, and the tests price the alternatives.
    pub fn with_rule(rule: Rule) -> Endpointer {
        Endpointer {
            detector: earshot::Detector::default_boxed(),
            rule,
            index: 0,
            turn_start: 0,
            first_speech: None,
            last_speech: 0,
            speech_frames: 0,
            silence_run: 0,
            clamped: [0.0; VAD_FRAME],
        }
    }

    /// The rule in force.
    pub fn rule(&self) -> Rule {
        self.rule
    }

    /// Hand it the next frame: exactly [`VAD_FRAME`] samples of 16 kHz mono, in `[-1, 1]`.
    ///
    /// A frame of the wrong length is [`Fault::WrongLength`] and **does not advance anything** —
    /// it was not audio, so it is neither speech nor silence, and counting it as either would
    /// move the hangover on time that did not pass.
    pub fn push(&mut self, frame: &[f32]) -> Result<Listening, Fault> {
        if frame.len() != VAD_FRAME {
            return Err(Fault::WrongLength { wanted: VAD_FRAME, got: frame.len() });
        }

        // Into range, because `earshot` `debug_assert`s it — and finite, because a NaN written
        // into its three-frame context comes back out of the features of every frame after it,
        // and a score of NaN is below every threshold. One bad sample from a driver would
        // otherwise be a microphone that has gone deaf with nothing said about it.
        for (out, sample) in self.clamped.iter_mut().zip(frame) {
            *out = if sample.is_finite() { sample.clamp(-1.0, 1.0) } else { 0.0 };
        }
        let score = self.detector.predict_f32(&self.clamped);

        let at = self.index;
        self.index += 1;

        if score >= self.rule.threshold {
            self.first_speech.get_or_insert(at);
            self.last_speech = at;
            self.speech_frames += 1;
            self.silence_run = 0;
            return Ok(Listening::Speech);
        }

        // Silence before anybody has said anything is not the end of a turn that never began.
        if self.first_speech.is_none() {
            return Ok(Listening::Quiet);
        }

        self.silence_run += 1;
        if self.silence_run >= self.hangover_frames() {
            Ok(Listening::Ended(self.end()))
        } else {
            Ok(Listening::Trailing)
        }
    }

    /// End the turn now, whatever the audio was doing — the push-to-talk key came up.
    ///
    /// The same [`Rule::min_speech`] floor applies, so a tapped key is [`Ended::TooShort`]
    /// rather than 40 ms of nothing handed to a speech model. The next turn starts clean.
    pub fn finish(&mut self) -> Ended {
        self.end()
    }

    /// Forget everything, including the frame numbering: a new stream, or a rebuilt device.
    ///
    /// `earshot`'s own documentation asks for this when the recording device changes or a new
    /// sequence begins. It is deliberately **not** done between turns of one stream: the
    /// detector carries three frames of context, and throwing it away would make the first
    /// frames of every turn after the first worse than the ones before them.
    pub fn reset(&mut self) {
        self.detector.reset();
        self.index = 0;
        self.new_turn();
    }

    /// A rule asking for no hangover at all still waits one frame — and that floor is **not** a
    /// clamp here. [`Self::push`] counts the silent frame *before* it compares, so the shortest
    /// hangover reachable is one frame whatever this returns. A `.max(1)` was written here and
    /// then removed: a mutation deleting it survived, which is what a defence no test can tell
    /// from its own absence looks like.
    fn hangover_frames(&self) -> usize {
        frames_in(self.rule.hangover)
    }

    fn end(&mut self) -> Ended {
        let speech = frame_duration() * self.speech_frames as u32;
        let ended = match self.first_speech {
            Some(first) if speech >= self.rule.min_speech => {
                let margin = frames_in(self.rule.margin);
                Ended::Utterance {
                    // Not past the start of this turn: the audio before it belongs to the turn
                    // before it, and a caller cutting both would transcribe some of it twice.
                    first: first.saturating_sub(margin).max(self.turn_start),
                    // Not past what was actually pushed. After a hangover there is always room
                    // for the margin; after a key release there may not be.
                    last: (self.last_speech + margin).min(self.index.saturating_sub(1)),
                    speech,
                }
            }
            _ => Ended::TooShort { speech },
        };
        self.new_turn();
        ended
    }

    fn new_turn(&mut self) {
        self.turn_start = self.index;
        self.first_speech = None;
        self.last_speech = 0;
        self.speech_frames = 0;
        self.silence_run = 0;
    }
}

impl Default for Endpointer {
    fn default() -> Endpointer {
        Endpointer::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::SAMPLE_RATE;

    // -----------------------------------------------------------------------------------------
    // Material
    // -----------------------------------------------------------------------------------------

    fn seconds(count: f32) -> usize {
        (SAMPLE_RATE as f32 * count) as usize
    }

    fn silence(secs: f32) -> Vec<f32> {
        vec![0.0; seconds(secs)]
    }

    /// A 440 Hz tone, which `earshot` reads as a voice in 121 of 125 frames (measured on this
    /// machine, 2026-09-15). It is used as a *stimulus* and not as a claim that a sine is
    /// speech — see `a_steady_tone_at_a_speaking_pitch_reads_as_a_voice` for what it means.
    fn tone(secs: f32) -> Vec<f32> {
        (0..seconds(secs))
            .map(|n| {
                0.5 * (std::f32::consts::TAU * 440.0 * n as f32 / SAMPLE_RATE as f32).sin()
            })
            .collect()
    }

    /// A full-band burst: a door, a cough, a hand on a desk.
    fn burst(secs: f32) -> Vec<f32> {
        let mut seed = 12345u32;
        (0..seconds(secs))
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                ((seed >> 8) as f32 / 8388608.0 - 1.0) * 0.6
            })
            .collect()
    }

    /// The recorded sentence every claim about the silence rule is measured against.
    ///
    /// `tests/audio/jfk.wav` — 11.00 s, 16 kHz, mono, 16-bit: a public-domain recording of the
    /// 1961 United States inaugural address, which is a work of that government, and the sample
    /// `whisper.cpp` ships for this same purpose. Kept as audio rather than as a
    /// table of scores on purpose: a table is a second copy of a measurement with nothing that
    /// goes red when `earshot` and the copy stop agreeing.
    ///
    /// The reader is sixteen lines rather than a dependency because `hound` would be a
    /// `[dev-dependencies]` entry on a crate whose whole feature layout exists to keep the
    /// dependency graph small, and this reads one known file.
    fn recorded(name: &str) -> Vec<f32> {
        crate::fixture::wav(name)
    }

    // -----------------------------------------------------------------------------------------
    // Driving it
    // -----------------------------------------------------------------------------------------

    /// Push every whole frame and keep what came back, frame index and all.
    fn listen(endpointer: &mut Endpointer, samples: &[f32]) -> Vec<Listening> {
        samples
            .chunks_exact(VAD_FRAME)
            .map(|frame| endpointer.push(frame).expect("a frame of exactly the right length"))
            .collect()
    }

    /// Every turn the rule ended by itself. A turn ended by a key release is not one of these.
    fn turns(endpointer: &mut Endpointer, samples: &[f32]) -> Vec<Ended> {
        listen(endpointer, samples)
            .into_iter()
            .filter_map(|heard| match heard {
                Listening::Ended(ended) => Some(ended),
                _ => None,
            })
            .collect()
    }

    fn index_of(heard: &[Listening], wanted: Listening) -> usize {
        heard.iter().position(|h| *h == wanted).unwrap_or_else(|| {
            panic!("nothing in this stream was {wanted:?}, so the test is measuring nothing")
        })
    }

    /// Where the turn ended, and the last thing that was said before it.
    ///
    /// Both together rather than separately on purpose: a short hangover ends several turns in
    /// one stream, and the last speech *anywhere* would then belong to a different turn from the
    /// first ending — which is a test measuring two unrelated things and calling it an interval.
    fn first_ending(heard: &[Listening]) -> (usize, usize) {
        let ended = heard
            .iter()
            .position(|h| matches!(h, Listening::Ended(_)))
            .expect("nothing in this stream ended a turn, so the test is measuring nothing");
        let last_speech = heard[..ended]
            .iter()
            .rposition(|h| *h == Listening::Speech)
            .expect("nothing was said before it, so the test is measuring nothing");
        (ended, last_speech)
    }

    // -----------------------------------------------------------------------------------------
    // The rule, and the recording it was argued from
    // -----------------------------------------------------------------------------------------

    /// **The test the whole silence rule rests on.**
    ///
    /// `jfk.wav` is one sentence — "and so my fellow Americans, ask not what your country can
    /// do for you, ask what you can do for your country" — and it has two deliberate pauses in
    /// it a full second long. At the chosen threshold they measure **1040 ms** (from 2.272 s)
    /// and **1024 ms** (from 4.400 s). A rule that ends a turn on either of them hands whisper
    /// three fragments and an agent a sentence that stops halfway.
    ///
    /// Three seconds of silence are appended because the recording itself ends 216 ms after the
    /// last word, which is less than any hangover worth having: without them the turn would
    /// never end inside the file and this would be testing nothing.
    #[test]
    fn the_pauses_inside_a_real_sentence_do_not_end_the_turn() {
        let mut spoken = recorded("jfk.wav");
        spoken.extend(silence(3.0));

        let heard = turns(&mut Endpointer::new(), &spoken);

        assert_eq!(
            heard.len(),
            1,
            "one sentence is one turn; this rule made {} of it: {heard:?}",
            heard.len()
        );
        match heard[0] {
            Ended::Utterance { first, last, speech } => {
                assert!(
                    speech >= Duration::from_secs(7),
                    "about 7.4 s of that recording is speech; this rule found {speech:?}"
                );
                assert!(
                    first * 16 < 400,
                    "the first word starts at 336 ms and the margin reaches behind it, so the \
                     kept audio must start before it, not at frame {first}"
                );
                assert!(
                    last * 16 > 10_500,
                    "the last word ends at 10.77 s; keeping only to frame {last} would cut it"
                );
            }
            other => panic!("a spoken sentence is an utterance, not {other:?}"),
        }
    }

    /// **The argument for [`HANGOVER`], written as a test rather than as a paragraph.**
    ///
    /// 700 ms is the number this rule wants to be: it is what most of the industry uses for
    /// end-of-speech, and it is short enough that nobody waits. On the one recording of real
    /// speech in this repository it cuts a single sentence into three, and each third goes to
    /// whisper on its own. This fails if [`HANGOVER`] is ever lowered to something like it, and
    /// the failure message is the reason.
    #[test]
    fn a_hangover_short_enough_to_be_tempting_cuts_that_sentence_into_three() {
        let mut spoken = recorded("jfk.wav");
        spoken.extend(silence(3.0));
        let tempting = Rule { hangover: Duration::from_millis(700), ..Rule::default() };

        let heard = turns(&mut Endpointer::with_rule(tempting), &spoken);

        assert_eq!(
            heard.len(),
            3,
            "the two 1-second pauses in that sentence each end a turn at 700 ms, so one \
             sentence becomes three; got {heard:?}"
        );
    }

    /// The constant and the measurement, tied together so that neither can move alone.
    #[test]
    fn the_hangover_is_longer_than_the_longest_pause_measured_inside_a_sentence() {
        assert!(
            HANGOVER > Duration::from_millis(1040),
            "1040 ms is the longest pause inside `jfk.wav` at this threshold; a hangover at or \
             below it truncates a person mid-sentence, and nothing downstream can repair that"
        );
    }

    /// The threshold is `earshot`'s own suggested operating point, and the measurements either
    /// side of it are what says leaving it there is a decision rather than a default.
    #[test]
    fn the_threshold_sits_where_the_detector_separates_a_quiet_room_from_a_voice() {
        assert_eq!(THRESHOLD, 0.5);

        let mut detector = Endpointer::new();
        let quiet = listen(&mut detector, &silence(2.0));

        assert!(
            quiet.iter().all(|heard| *heard == Listening::Quiet),
            "two seconds of digital silence scored at most 0.275 here; none of it may read as \
             a voice, or every rule above this one is built on noise"
        );
    }

    // -----------------------------------------------------------------------------------------
    // What ends a turn, and where the audio is cut
    // -----------------------------------------------------------------------------------------

    /// A steady tone at a speaking pitch reads as a voice — 121 of 125 frames, measured — which
    /// is what makes it usable as a stimulus below, and is also worth knowing on its own: an
    /// alarm or a held musical note in the room will hold a turn open. A 1 kHz or 3 kHz tone
    /// does not; it is specifically the pitch range a voice lives in.
    #[test]
    fn a_steady_tone_at_a_speaking_pitch_reads_as_a_voice() {
        let heard = listen(&mut Endpointer::new(), &tone(1.0));

        let voiced = heard.iter().filter(|h| **h == Listening::Speech).count();
        assert!(voiced > heard.len() / 2, "{voiced} of {} frames", heard.len());
    }

    /// The hangover runs from the **last** speech frame, not from the first, and the audio kept
    /// stops at the last speech plus the margin rather than at the end of the hangover.
    ///
    /// Both halves matter to task 5: whisper's `audio_ctx` is scaled by how long the clip is, so
    /// 1.2 s of trailing silence left in a 3 s utterance is a third of the work for nothing.
    #[test]
    fn the_hangover_runs_from_the_last_speech_and_the_silence_after_it_is_trimmed() {
        let mut spoken = tone(1.0);
        spoken.extend(silence(3.0));

        let heard = listen(&mut Endpointer::new(), &spoken);

        let (ended, last_speech) = first_ending(&heard);

        assert_eq!(
            ended - last_speech,
            frames_in(HANGOVER),
            "the turn must end exactly one hangover after the last thing that was said"
        );
        match heard[ended] {
            Listening::Ended(Ended::Utterance { last, .. }) => assert_eq!(
                last,
                last_speech + frames_in(MARGIN),
                "the kept audio stops a margin after the last word, not at the end of the \
                 hangover — whisper is charged for every second it is handed"
            ),
            other => panic!("a second of tone is an utterance, not {other:?}"),
        }
    }

    /// A detector that says speech began at frame N is saying it was already under way: the
    /// attack of the first consonant is in the frames before it. The margin is what keeps them.
    #[test]
    fn the_margin_keeps_the_attack_the_detector_missed() {
        let mut spoken = silence(1.0);
        spoken.extend(tone(1.0));
        spoken.extend(silence(3.0));

        let heard = listen(&mut Endpointer::new(), &spoken);

        let first_speech = index_of(&heard, Listening::Speech);
        let ended = heard
            .iter()
            .find_map(|h| match h {
                Listening::Ended(ended) => Some(*ended),
                _ => None,
            })
            .expect("the turn ends inside the trailing silence");

        match ended {
            Ended::Utterance { first, .. } => {
                // Both halves, because the second alone is satisfied by a margin of nothing:
                // `first_speech - 0` is `first_speech`, and the test would then be pinning no
                // constant at all.
                assert!(
                    first < first_speech,
                    "the kept audio has to start behind the first frame the detector agreed \
                     about; frame {first} is not behind {first_speech}"
                );
                assert_eq!(
                    first,
                    first_speech - frames_in(MARGIN),
                    "and by exactly the margin, not by some other amount"
                );
            }
            other => panic!("a second of tone is an utterance, not {other:?}"),
        }
    }

    /// The margin may not reach past the beginning of the stream, and it may not name a frame
    /// nobody pushed. Both would be indices into a buffer the caller does not have.
    #[test]
    fn the_margin_never_names_audio_that_does_not_exist() {
        let mut spoken = tone(1.0);
        spoken.extend(silence(3.0));
        let mut endpointer = Endpointer::new();

        let heard = listen(&mut endpointer, &spoken);

        let pushed = heard.len();
        let ended = heard
            .iter()
            .find_map(|h| match h {
                Listening::Ended(ended) => Some(*ended),
                _ => None,
            })
            .expect("the turn ends inside the trailing silence");
        match ended {
            Ended::Utterance { first, last, .. } => {
                assert_eq!(first, 0, "speech starts at once here, so the margin is clamped");
                assert!(last < pushed, "frame {last} was never pushed");
            }
            other => panic!("a second of tone is an utterance, not {other:?}"),
        }
    }

    /// Two turns in one stream, with indices into the whole stream rather than into each turn:
    /// task 6 holds one buffer and has to be able to cut both out of it.
    #[test]
    fn two_turns_in_one_stream_are_reported_with_indices_into_the_whole_stream() {
        let mut spoken = tone(1.0);
        spoken.extend(silence(3.0));
        spoken.extend(tone(1.0));
        spoken.extend(silence(3.0));

        let heard = turns(&mut Endpointer::new(), &spoken);

        assert_eq!(heard.len(), 2, "two utterances, two turns: {heard:?}");
        let (first, second) = match (heard[0], heard[1]) {
            (
                Ended::Utterance { last: first_last, .. },
                Ended::Utterance { first: second_first, last: second_last, .. },
            ) => (first_last, (second_first, second_last)),
            other => panic!("both are utterances, not {other:?}"),
        };
        assert!(
            second.0 > first,
            "the second turn starts at frame {} and the first ended at {first}; indices that \
             restarted per turn would cut the wrong audio out of the buffer",
            second.0
        );
        assert!(
            second.1 * 16 > 4_500,
            "the second tone runs from 4.0 s to 5.0 s, so the audio kept for it ends after \
             4.5 s of the stream and not of the turn; frame {} is {} ms in",
            second.1,
            second.1 * 16
        );
    }

    // -----------------------------------------------------------------------------------------
    // What is not a turn
    // -----------------------------------------------------------------------------------------

    /// Digital silence is never anything. The turn does not start, so the hangover never runs
    /// and nothing is ever ended by it.
    #[test]
    fn silence_alone_never_becomes_a_turn() {
        let mut endpointer = Endpointer::new();

        let heard = listen(&mut endpointer, &silence(5.0));

        assert!(heard.iter().all(|h| *h == Listening::Quiet), "{heard:?}");
        assert_eq!(
            endpointer.finish(),
            Ended::TooShort { speech: Duration::ZERO },
            "a key released over silence is not an empty transcript, it is no turn at all"
        );
    }

    /// **The floor that stops a cough becoming a turn.**
    ///
    /// 200 ms of tone puts about twelve frames over the threshold, which is a real detection
    /// and still less than [`MIN_SPEECH`]. The assertion is that the turn *ended* — so the
    /// hangover did run, and the rule really did have something to reject — and that what it
    /// ended as was `TooShort`. A test that only checked "not an utterance" would also pass if
    /// nothing had been detected at all.
    #[test]
    fn a_sound_shorter_than_a_word_ends_the_turn_as_nothing() {
        let mut spoken = silence(0.5);
        spoken.extend(tone(0.2));
        spoken.extend(silence(3.0));

        let heard = turns(&mut Endpointer::new(), &spoken);

        assert_eq!(heard.len(), 1, "the hangover expired, so exactly one turn ended: {heard:?}");
        match heard[0] {
            Ended::TooShort { speech } => assert!(
                speech > Duration::ZERO && speech < MIN_SPEECH,
                "something was heard and it was under the floor, got {speech:?}"
            ),
            other => panic!("200 ms is not a turn; got {other:?}"),
        }
    }

    /// A click and a door: neither may ever produce audio for whisper to read. Whisper invents
    /// sentences out of noise, and a sentence invented out of a door goes to an agent that can
    /// act on it — which is why this floor errs high while [`HANGOVER`] errs late.
    #[test]
    fn a_click_and_a_door_never_produce_something_to_transcribe() {
        let mut noises = silence(0.5);
        noises.extend([0.9]);
        noises.extend(silence(0.5));
        noises.extend(burst(0.1));
        noises.extend(silence(3.0));

        let heard = turns(&mut Endpointer::new(), &noises);

        assert!(
            heard.iter().all(|ended| matches!(ended, Ended::TooShort { .. })),
            "an impulse and a 100 ms burst are not speech: {heard:?}"
        );
    }

    // -----------------------------------------------------------------------------------------
    // The key, which is the other thing that ends a turn
    // -----------------------------------------------------------------------------------------

    /// Push-to-talk: the key comes up in the middle of a word and the turn ends there. The
    /// hangover is never paid, which is most of why erring late costs so little.
    #[test]
    fn a_key_released_mid_sentence_still_yields_what_was_said() {
        let mut endpointer = Endpointer::new();
        let spoken = tone(1.0);
        let heard = listen(&mut endpointer, &spoken);

        let ended = endpointer.finish();

        assert!(
            heard.iter().all(|h| !matches!(h, Listening::Ended(_))),
            "nothing ended it but the key"
        );
        match ended {
            Ended::Utterance { first, last, speech } => {
                assert!(speech >= MIN_SPEECH, "a second of speech clears the floor");
                assert!(first < last);
                assert!(
                    last < heard.len(),
                    "the margin past the last word runs out of stream here, so frame {last} \
                     must still be one the caller has"
                );
            }
            other => panic!("a second of speech is an utterance, not {other:?}"),
        }
    }

    /// A tapped key. The same floor applies, or a tap becomes 40 ms of nothing handed to a
    /// speech model — which is exactly the input whisper hallucinates on.
    #[test]
    fn a_key_tapped_and_released_is_not_a_turn() {
        let mut endpointer = Endpointer::new();
        listen(&mut endpointer, &tone(0.1));

        assert!(matches!(endpointer.finish(), Ended::TooShort { .. }));
    }

    /// Finishing starts a new turn rather than leaving the old one half-open: the key is
    /// pressed again a second later and what is said then is its own turn.
    #[test]
    fn finishing_a_turn_leaves_the_next_one_able_to_start() {
        let mut endpointer = Endpointer::new();
        listen(&mut endpointer, &tone(1.0));
        let first = endpointer.finish();

        listen(&mut endpointer, &tone(1.0));
        let second = endpointer.finish();

        assert!(matches!(first, Ended::Utterance { .. }));
        match (first, second) {
            (Ended::Utterance { last: first_last, .. }, Ended::Utterance { first: next, .. }) => {
                assert!(
                    next > first_last,
                    "the second turn must not reach back into the first: {next} vs {first_last}"
                )
            }
            other => panic!("both are utterances, not {other:?}"),
        }
    }

    /// **`reset` has to forget the audio as well as the numbering**, and a mutation deleting
    /// `Detector::reset` from it survived until this test existed.
    ///
    /// `earshot` keeps 768 samples of context, so without clearing it the first frames of the
    /// new stream are scored partly on the old one's. Here the old stream ends mid-tone and the
    /// new one is silent, which is the case where it shows.
    #[test]
    fn resetting_forgets_the_audio_as_well_as_the_numbering() {
        let mut endpointer = Endpointer::new();
        listen(&mut endpointer, &tone(1.0));

        endpointer.reset();
        let heard = listen(&mut endpointer, &silence(0.1));

        assert!(
            heard.iter().all(|h| *h == Listening::Quiet),
            "a silent new stream must read as silent from its first frame: {heard:?}"
        );
    }

    /// `reset` is for a new stream — a different microphone, a rebuilt device — and it puts the
    /// frame numbering back to the beginning because the caller's buffer went with it.
    #[test]
    fn resetting_starts_the_frame_numbering_again() {
        let mut endpointer = Endpointer::new();
        listen(&mut endpointer, &tone(1.0));
        endpointer.reset();

        let mut spoken = tone(1.0);
        spoken.extend(silence(3.0));
        let heard = turns(&mut endpointer, &spoken);

        match heard.as_slice() {
            [Ended::Utterance { first, .. }] => assert_eq!(*first, 0),
            other => panic!("one turn from the top: {other:?}"),
        }
    }

    // -----------------------------------------------------------------------------------------
    // Frames that are not frames
    // -----------------------------------------------------------------------------------------

    /// **`earshot` documents `-1.0` for a wrong frame length. That is release behaviour only.**
    ///
    /// `predict_f32` opens with `debug_assert_eq!(frame.len(), 256)`, so in a debug build —
    /// which every `cargo test` is — a wrong length **panics** before it can reach the `-1.0`.
    /// Measured on this machine, 2026-09-15: release returns `-1`, debug takes the process down,
    /// and an empty slice does the same. So the length is checked here and the detector is
    /// never shown a frame it could object to, in either profile.
    ///
    /// A score of `-1` is not an alternative to this check. It is below every threshold, so a
    /// caller that let it through would read a length bug as silence forever.
    #[test]
    fn a_wrong_frame_length_is_an_error_and_never_a_score() {
        let mut endpointer = Endpointer::new();

        assert_eq!(
            endpointer.push(&[0.0; 100]),
            Err(Fault::WrongLength { wanted: VAD_FRAME, got: 100 })
        );
        assert_eq!(
            endpointer.push(&[]),
            Err(Fault::WrongLength { wanted: VAD_FRAME, got: 0 }),
            "an empty callback is a wrong length like any other"
        );
        assert_eq!(
            endpointer.push(&[0.0; VAD_FRAME + 1]),
            Err(Fault::WrongLength { wanted: VAD_FRAME, got: VAD_FRAME + 1 })
        );
    }

    /// A refused frame must not move the turn along either: it was not audio, so it is not
    /// silence and it is not speech.
    #[test]
    fn a_refused_frame_does_not_advance_the_turn() {
        let mut endpointer = Endpointer::new();
        listen(&mut endpointer, &tone(1.0));
        let before = match endpointer.finish() {
            Ended::Utterance { last, .. } => last,
            other => panic!("{other:?}"),
        };

        let mut again = Endpointer::new();
        listen(&mut again, &tone(1.0));
        for _ in 0..10 {
            let _ = again.push(&[0.0; 3]);
        }
        let after = match again.finish() {
            Ended::Utterance { last, .. } => last,
            other => panic!("{other:?}"),
        };

        assert_eq!(before, after, "ten refused frames must not count as ten frames of audio");
    }

    /// `earshot` also `debug_assert`s that every sample is within [-1, 1], so a hot sample would
    /// take a debug build down as surely as a wrong length. `rubato`'s FFT resampler can ring
    /// slightly past full scale on loud input, so this is reachable from a real microphone.
    #[test]
    fn a_sample_past_full_scale_is_clamped_rather_than_a_panic() {
        let mut endpointer = Endpointer::new();
        let mut hot = tone(0.1);
        hot[0] = 4.0;
        hot[1] = -4.0;

        for frame in hot.chunks_exact(VAD_FRAME) {
            assert!(endpointer.push(frame).is_ok());
        }
    }

    /// One frame of nonsense must not be the end of the detector.
    ///
    /// `earshot` keeps three frames of context in a ring buffer, so a NaN written into it comes
    /// back out of every feature for as long as it stays there — and the score of every later
    /// frame is then `NaN`, which compares false against any threshold. That is a microphone
    /// that has gone permanently deaf with nothing logged.
    #[test]
    fn one_frame_of_nonsense_does_not_leave_the_detector_deaf() {
        let mut endpointer = Endpointer::new();

        let _ = endpointer.push(&[f32::NAN; VAD_FRAME]);
        let heard = listen(&mut endpointer, &tone(1.0));

        assert!(
            heard.iter().any(|h| *h == Listening::Speech),
            "speech after a bad frame must still be heard"
        );
    }

    // -----------------------------------------------------------------------------------------
    // Arithmetic
    // -----------------------------------------------------------------------------------------

    /// 256 samples at 16 kHz is 16 ms exactly, and both halves are derived rather than typed.
    #[test]
    fn a_frame_is_sixteen_milliseconds() {
        assert_eq!(frame_duration(), Duration::from_millis(16));
        assert_eq!(VAD_FRAME * 1000 / SAMPLE_RATE as usize, 16);
    }

    /// Rounded up, so a rule written in milliseconds is never quietly shortened.
    #[test]
    fn a_duration_is_never_rounded_down_into_fewer_frames() {
        assert_eq!(frames_in(Duration::from_millis(16)), 1);
        assert_eq!(frames_in(Duration::from_millis(17)), 2);
        assert_eq!(frames_in(Duration::from_millis(1200)), 75);
        assert_eq!(frames_in(Duration::from_millis(200)), 13, "200 ms is 12.5 frames");
        assert_eq!(frames_in(Duration::ZERO), 0);
    }

    /// A hangover of nothing still waits one frame, because a hangover of zero frames would end
    /// the turn on the sample after the last one — and a rule nobody can express as "end it at
    /// once" should not be reachable by writing `Duration::ZERO` by accident.
    #[test]
    fn a_hangover_of_nothing_still_waits_one_frame() {
        let mut spoken = tone(0.5);
        spoken.extend(silence(1.0));
        let rule = Rule { hangover: Duration::ZERO, ..Rule::default() };

        let heard = listen(&mut Endpointer::with_rule(rule), &spoken);

        let (ended, last_speech) = first_ending(&heard);

        assert_eq!(
            ended - last_speech,
            1,
            "a rule asking for no hangover at all still waits one frame, because there is no \
             shorter unit of silence than the frame the detector scores"
        );
    }

    /// Task 6 drives this from a task of its own, so it has to be able to move there — and
    /// `earshot`'s `Detector` is the part that would stop it. A compile-time assertion; there
    /// is nothing to run.
    #[test]
    fn an_endpointer_can_be_moved_to_the_task_that_owns_it() {
        fn assert_send<T: Send>() {}
        assert_send::<Endpointer>();
        assert_send::<Ended>();
    }

    /// The rule is reachable, because the window renders it and task 6 configures it.
    #[test]
    fn the_rule_in_force_is_the_one_that_was_asked_for() {
        assert_eq!(Endpointer::new().rule(), Rule::default());
        assert_eq!(
            Rule::default(),
            Rule {
                threshold: THRESHOLD,
                hangover: HANGOVER,
                min_speech: MIN_SPEECH,
                margin: MARGIN
            }
        );

        let mine = Rule { hangover: Duration::from_secs(2), ..Rule::default() };
        assert_eq!(Endpointer::with_rule(mine).rule(), mine);
    }
}
