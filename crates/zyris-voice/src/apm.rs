//! What happens to a microphone frame before anything else is allowed to read it: a high-pass
//! filter, noise suppression, and — from step 8 — echo cancellation.
//!
//! # This module exists in two shapes and has one set of signatures
//!
//! `webrtc-audio-processing` is behind the **`aec`** feature, not `voice`, and the reason is a
//! product constraint rather than a build-time convenience: no Debian or Ubuntu ships 2.x, its
//! `bundled` path shells out to a `meson` that is on neither CI runner nor on this development
//! machine, and upstream has no supported Windows build at all. A single `voice` feature
//! carrying it would make `cargo build -p zyris-app --features voice` fail on the platform this
//! product ships an `.exe` for. The root `Cargo.toml` writes that out in full.
//!
//! So **`aec` is compiled by nothing in CI**, and the pipeline has to work without it. [`Apm`]
//! therefore has exactly one set of method signatures, written once, with only the *bodies*
//! reading the feature — the same accommodation [`crate::Voice::describe`] makes for the window.
//! Task 6's session has no `#[cfg]` in it as a result, and neither build can rot into a shape
//! the other does not have.
//!
//! # What a build without `aec` loses
//!
//! [`Apm::describe`] says it to a person; here is the whole of it, in the order it will bite.
//!
//! 1. **The high-pass filter.** DC offset and rumble below the speech band. Real today, and it
//!    is not cosmetic: a buffer of DC `0.3` plus a 50 Hz mains hum scores over `earshot`'s
//!    threshold in 6 of 125 frames on this machine (measured 2026-09-15), which is a detector
//!    being told somebody is talking by a power supply.
//! 2. **Noise suppression** at [`config::NoiseSuppressionLevel::Moderate`]. A fan, a laptop's
//!    own fan, traffic. Real today, and it matters *because* of what sits downstream:
//!    `earshot` claims resilience only down to about 3 dB SNR, and whisper is well known for
//!    inventing sentences out of noise. Both failures are silent.
//! 3. **Echo cancellation.** Nothing today — see [`Apm::erle_db`] — and everything from step 8,
//!    when this machine starts speaking while the microphone is still open. Without it whisper
//!    transcribes Zyris's own voice and hands it back to the agent as if a person had said it.
//!
//! A build without `aec` still hears, still detects speech and still transcribes. It is noisier
//! into both the detector and the model, and once step 8 can speak it will hear itself.
//!
//! # The frame length is checked here, because the library panics
//!
//! `process_capture_frame` **panics** rather than returning `Err` when the sample count is not
//! exactly 10 ms at the configured rate — 160 samples at 16 kHz, which is
//! [`crate::capture::APM_FRAME`]. That is what makes [`crate::capture::Chunker`] load-bearing
//! rather than a convenience, and it is why [`check`] runs in **both** feature states: a length
//! the `aec` build refuses must not be a length the plain `voice` build quietly accepts, or the
//! bug appears only on the one machine that has the library.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::capture::{APM_FRAME, UNKNOWN_DELAY, read_delay, store_delay};

/// The longest round trip [`Apm::set_stream_delay`] will pass on.
///
/// The library takes a `u16` of milliseconds, so there is a ceiling whatever this says; half a
/// second is already four times the worst path this project has measured (42.67 ms of output
/// plus whatever the input side reports), and a backend claiming more than that is reporting
/// something no echo canceller can act on. Clamped rather than refused: it is a number read off
/// a device, not typed by a person.
pub const MAX_STREAM_DELAY: Duration = Duration::from_millis(500);

/// Why a build without the `aec` feature does nothing to the microphone.
///
/// Worded for a person reading the window rather than a developer reading a log, for the same
/// reason [`crate::NOT_COMPILED_IN`] is: whoever installed a build like this did not choose the
/// feature flags.
pub const NO_ECHO_CANCELLER: &str =
    "this build has no echo canceller, so background noise reaches the microphone unfiltered \
     and Zyris will hear itself speak";

/// Something one frame could not be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// The frame was not exactly [`APM_FRAME`] samples.
    ///
    /// **Ours, not the library's.** `webrtc-audio-processing` panics on a wrong length; this is
    /// the check that stands in front of it, and it is the same answer in both feature states.
    WrongLength {
        /// What was required — always [`APM_FRAME`].
        wanted: usize,
        /// What arrived.
        got: usize,
    },
    /// The audio processor refused, and this is what it said. Also how a processor that would
    /// not start at all reports itself.
    Refused(String),
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fault::WrongLength { wanted, got } => {
                write!(f, "the audio processor takes exactly {wanted} samples and got {got}")
            }
            Fault::Refused(why) => write!(f, "the audio processor refused the frame: {why}"),
        }
    }
}

impl std::error::Error for Fault {}

/// What this build does to the microphone before the detector and the model see it.
///
/// Two answers rather than a boolean, for the reason `zyris-tools`'s `announce.rs` gives about
/// `input` and `screen_capture`: something that silently does nothing is worse than something
/// absent, because nobody can tell the two apart.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum Conditioning {
    /// The high-pass filter, noise suppression and the echo canceller are all running.
    Full,
    /// The microphone is passed through untouched, and this is what that costs.
    Untouched {
        /// A sentence for a person, not an error code.
        reason: String,
    },
}

/// The one length every entry point here accepts.
///
/// Separate from the call it guards so that the plain `voice` build — which is the one CI
/// compiles — tests the rule that the `aec` build depends on.
pub fn check(frame: &[f32]) -> Result<(), Fault> {
    if frame.len() == APM_FRAME {
        Ok(())
    } else {
        Err(Fault::WrongLength { wanted: APM_FRAME, got: frame.len() })
    }
}

/// The audio processing module: one per capture stream, shared with whatever plays audio.
///
/// # Sharing it
///
/// Every method takes `&self` and the type is `Send + Sync`, because the library's own
/// `Processor` is — it exposes the thread-safe subset of the C++ API deliberately so that the
/// capture and render threads can hold one instance. So **step 8 shares an [`Arc<Apm>`]**, and
/// there is no `Arc` inside this struct: a second indirection that every caller would have to
/// go through anyway buys nothing, and `Arc<Apm>` is the shape the library documents.
/// `sharing_one_processor_between_two_threads_is_what_the_library_is_for` pins it, and it is one
/// of the tests that compiles in both feature states.
pub struct Apm {
    #[cfg(feature = "aec")]
    processor: webrtc_audio_processing::Processor,
    /// What [`Apm::set_stream_delay`] was last given, in nanoseconds, or [`UNKNOWN_DELAY`].
    ///
    /// **Kept in both feature states on purpose.** The figure is assembled from two `cpal`
    /// timestamps that exist on every build, and the assembling is the part that can be wrong;
    /// holding it here is what lets the test deciding it run where CI runs rather than only on
    /// the one machine that has the library.
    delay: AtomicU64,
}

impl Apm {
    /// Build one for [`SAMPLE_RATE`].
    ///
    /// Fails only where there is a library to fail: a build without `aec` cannot refuse. The
    /// signature is a `Result` in both states anyway, so that the caller has one code path.
    pub fn new() -> Result<Apm, Fault> {
        #[cfg(not(feature = "aec"))]
        {
            Ok(Apm { delay: AtomicU64::new(UNKNOWN_DELAY) })
        }
        #[cfg(feature = "aec")]
        {
            let processor = webrtc_audio_processing::Processor::new(crate::capture::SAMPLE_RATE)
                .map_err(|error| Fault::Refused(error.to_string()))?;

            processor.set_config(config(None));

            Ok(Apm { processor, delay: AtomicU64::new(UNKNOWN_DELAY) })
        }
    }

    /// Tell the echo canceller how far the loudspeaker is behind the microphone: `playback -
    /// callback` on the output stream plus `callback - capture` on the input one.
    ///
    /// # Why it is a setter rather than an argument to [`Apm::new`]
    ///
    /// The processor is built when a microphone opens; the speaker opens later, or never. A
    /// delay declared at construction would therefore always be the wrong one, and
    /// `EchoCanceller::Full` takes an `Option` precisely so that "not known yet" is sayable —
    /// AEC3 estimates the delay itself while it is `None`, which is what step 7 shipped and
    /// what a machine with nothing playing stays on.
    ///
    /// # Why it rewrites the whole configuration
    ///
    /// `set_config` **replaces**; it does not merge. A call passing only an echo canceller would
    /// switch the high-pass filter and the noise suppressor off — a microphone quietly getting
    /// worse at the moment a speaker opens, with nothing to say so. [`config`] is the one place
    /// the settings are written and both callers go through it.
    pub fn set_stream_delay(&self, delay: Option<Duration>) {
        let delay = delay.map(|delay| delay.min(MAX_STREAM_DELAY));
        match delay {
            Some(delay) => store_delay(&self.delay, delay),
            None => self.delay.store(UNKNOWN_DELAY, Ordering::Relaxed),
        }
        #[cfg(feature = "aec")]
        {
            // Rounded rather than truncated: the frame is 10 ms, so half a millisecond of bias
            // is five per cent of one and it costs nothing not to have it.
            let ms = delay.map(|delay| (delay.as_secs_f64() * 1000.0).round() as u16);
            self.processor.set_config(config(ms));
        }
    }

    /// What [`Apm::set_stream_delay`] was last told, or `None` while nothing has said.
    pub fn stream_delay(&self) -> Option<Duration> {
        read_delay(&self.delay)
    }
}

/// The whole configuration, in one place, because `set_config` replaces rather than merges.
#[cfg(feature = "aec")]
fn config(stream_delay_ms: Option<u16>) -> webrtc_audio_processing::config::Config {
    use webrtc_audio_processing::config::{
        Config, EchoCanceller, HighPassFilter, NoiseSuppression, NoiseSuppressionLevel,
    };

    Config {
                // On — and **while noise suppression is also on this line changes nothing**,
                // which was found by mutating it away and watching every test stay green.
                // `Config::noise_suppression`'s own documentation says it force-enables high
                // pass filtering, so the C++ runs the filter either way. The line stays
                // because it says what is wanted rather than what happens to follow, and it
        // becomes load-bearing the moment noise suppression is turned down.
        high_pass_filter: Some(HighPassFilter { apply_in_full_band: true }),
        // `Full` is AEC3. The delay is estimated by it while `stream_delay_ms` is `None` and
        // declared once a speaker is open — see [`Apm::set_stream_delay`].
        echo_canceller: Some(EchoCanceller::Full { stream_delay_ms }),
        // `Moderate` rather than `High`: the levels above it trade speech distortion
        // for quiet, and the consumer downstream is a speech model, not a listener.
        noise_suppression: Some(NoiseSuppression {
            level: NoiseSuppressionLevel::Moderate,
            analyze_linear_aec_output: false,
        }),
        // Deliberately off. A gain controller would push samples past the [-1, 1] that
        // `earshot` documents, and whisper normalises its own input anyway.
        ..Config::default()
    }
}

impl Apm {
    /// Clean up one captured frame, in place. Exactly [`APM_FRAME`] samples, mono, 16 kHz.
    ///
    /// With `aec` off this checks the length and returns the frame untouched, which is the
    /// whole of what [`Conditioning::Untouched`] means.
    pub fn process_capture(&self, frame: &mut [f32]) -> Result<(), Fault> {
        check(frame)?;
        #[cfg(feature = "aec")]
        {
            self.processor
                .process_capture_frame([&mut *frame])
                .map_err(|error| Fault::Refused(error.to_string()))?;
        }
        Ok(())
    }

    /// Tell the echo canceller what is about to be played, without modifying it.
    ///
    /// **Its caller is [`crate::playback::Render`]**, which takes the tap off the output stream
    /// — what was handed to the device, silence included — resamples it to 16 kHz and cuts it
    /// into these frames. Step 7 left this here with no caller so that step 8 would add one
    /// rather than redesign anything, and that is what happened.
    ///
    /// Feeding it is what makes [`Self::erle_db`] mean something: with nothing ever analysed
    /// the canceller has no reference and removes nothing.
    pub fn analyze_render(&self, frame: &[f32]) -> Result<(), Fault> {
        check(frame)?;
        #[cfg(feature = "aec")]
        {
            self.processor
                .analyze_render_frame([frame])
                .map_err(|error| Fault::Refused(error.to_string()))?;
        }
        Ok(())
    }

    /// Echo return loss enhancement, in dB: how much of the loudspeaker the canceller removed
    /// from the microphone.
    ///
    /// # 0.18 dB with no reference, and what it reads once there is one
    ///
    /// Measured on this machine, 2026-09-15: with `EchoCanceller::Full` configured and **no
    /// render frame ever fed**, `process_capture_frame` returns `Ok` and ERLE settles at
    /// **0.1755 dB** — which is to say it cancels nothing and errors at nothing. There is
    /// nothing for it to cancel until [`Self::analyze_render`] has been shown what is playing.
    ///
    /// Once it has been, over a synthetic echo path — the reference delayed and attenuated back
    /// into the capture side — the same figure is what
    /// `feeding_the_render_side_is_what_makes_the_canceller_cancel` asserts on. **That is a
    /// measurement of this wiring, not of a room**: nobody has yet held a microphone in front of
    /// a loudspeaker playing this voice, and the README says so.
    ///
    /// `None` on a build without `aec`, and also whenever the library has no estimate yet.
    pub fn erle_db(&self) -> Option<f64> {
        #[cfg(not(feature = "aec"))]
        {
            None
        }
        #[cfg(feature = "aec")]
        {
            self.processor.get_stats().echo_return_loss_enhancement
        }
    }

    /// What this build does to the microphone, and what it costs when the answer is "nothing".
    ///
    /// Cheap and safe to call on every render, like [`crate::Voice::describe`].
    pub fn describe(&self) -> Conditioning {
        #[cfg(not(feature = "aec"))]
        {
            Conditioning::Untouched { reason: NO_ECHO_CANCELLER.into() }
        }
        #[cfg(feature = "aec")]
        {
            Conditioning::Full
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The length the whole chunking layer exists to deliver, stated as arithmetic rather than
    /// as a number, so that a change of capture rate cannot leave the two disagreeing.
    ///
    /// 10 ms is not ours: `GetFrameSize` in the C++ library is `sample_rate_hz / 100` and there
    /// is no way to ask for anything else.
    #[test]
    fn the_frame_is_ten_milliseconds_at_the_rate_everything_downstream_runs_at() {
        assert_eq!(APM_FRAME, crate::capture::SAMPLE_RATE as usize / 100);
        assert_eq!(APM_FRAME, 160);
    }

    /// **The check that stands in front of a panic.**
    ///
    /// `process_capture_frame` panics rather than returning `Err` on a wrong sample count, so a
    /// caller that trusted the library to complain would take the process down instead. This is
    /// deliberately tested in *both* feature states: `aec` is compiled by nothing in CI, so if
    /// the guard lived only on the `aec` side nothing anywhere would run it.
    #[test]
    fn a_frame_of_the_wrong_length_is_refused_rather_than_handed_to_a_library_that_panics() {
        let apm = Apm::new().expect("a processor at the capture rate");

        let mut short = vec![0.0; APM_FRAME - 1];
        let mut long = vec![0.0; APM_FRAME + 1];
        let mut none: Vec<f32> = Vec::new();

        assert_eq!(
            apm.process_capture(&mut short),
            Err(Fault::WrongLength { wanted: APM_FRAME, got: APM_FRAME - 1 })
        );
        assert_eq!(
            apm.process_capture(&mut long),
            Err(Fault::WrongLength { wanted: APM_FRAME, got: APM_FRAME + 1 })
        );
        assert_eq!(
            apm.process_capture(&mut none),
            Err(Fault::WrongLength { wanted: APM_FRAME, got: 0 }),
            "an empty buffer is a wrong length like any other, not a no-op"
        );
    }

    /// The render seam is bound by the same rule, and for the same reason: the library panics
    /// there too. Step 8 is the first caller, so this is the only thing that will have run it.
    #[test]
    fn the_render_seam_refuses_a_wrong_length_too() {
        let apm = Apm::new().expect("a processor at the capture rate");

        assert_eq!(
            apm.analyze_render(&[0.0; 32]),
            Err(Fault::WrongLength { wanted: APM_FRAME, got: 32 })
        );
    }

    /// A frame of exactly the right length is accepted in both builds — the `aec` one because
    /// capture-only processing really does return `Ok`, and the plain one because it is a
    /// passthrough. Neither may refuse it.
    #[test]
    fn a_frame_of_the_right_length_is_accepted_whichever_build_this_is() {
        let apm = Apm::new().expect("a processor at the capture rate");

        let mut frame = vec![0.05; APM_FRAME];

        assert_eq!(apm.process_capture(&mut frame), Ok(()));
        assert_eq!(apm.analyze_render(&vec![0.05; APM_FRAME]), Ok(()));
    }

    /// **What step 7 owes step 8**: one processor, reachable from the thread that captures and
    /// from the thread that plays, with no redesign in between.
    ///
    /// The assertion is that it compiles and that both threads get through — `Arc<Apm>` is the
    /// shape the library documents, and a `Processor` that had stopped being `Sync` upstream
    /// would fail this rather than being discovered in step 8.
    #[test]
    fn sharing_one_processor_between_two_threads_is_what_the_library_is_for() {
        let apm = std::sync::Arc::new(Apm::new().expect("a processor at the capture rate"));

        let capture = std::sync::Arc::clone(&apm);
        let render = std::sync::Arc::clone(&apm);

        let a = std::thread::spawn(move || {
            let mut frame = vec![0.0; APM_FRAME];
            capture.process_capture(&mut frame)
        });
        let b = std::thread::spawn(move || render.analyze_render(&vec![0.0; APM_FRAME]));

        assert_eq!(a.join().expect("the capture thread"), Ok(()));
        assert_eq!(b.join().expect("the render thread"), Ok(()));
    }

    /// A build has to be able to say which of the two it is, before anybody has spoken.
    ///
    /// The assertion is on the *reason* and not only on the variant, because an empty sentence
    /// renders as a window that says nothing, which reads exactly like a microphone that is
    /// about to work.
    #[test]
    fn each_build_says_what_it_does_to_the_microphone() {
        let apm = Apm::new().expect("a processor at the capture rate");

        if cfg!(feature = "aec") {
            assert_eq!(apm.describe(), Conditioning::Full);
        } else {
            assert_eq!(
                apm.describe(),
                Conditioning::Untouched { reason: NO_ECHO_CANCELLER.into() }
            );
            assert!(
                NO_ECHO_CANCELLER.contains("hear itself"),
                "the sentence has to name the consequence, not only the absence"
            );
        }
    }

    /// **The round trip, kept where it was put**, in both feature states.
    ///
    /// The figure is assembled from two `cpal` timestamps and the assembling is the part that
    /// can be wrong, so the test that decides it runs on the build CI compiles rather than only
    /// on the one machine with the library.
    #[test]
    fn the_delay_the_canceller_is_told_is_the_one_it_was_given() {
        let apm = Apm::new().expect("a processor at the capture rate");

        assert_eq!(apm.stream_delay(), None, "nothing has measured anything yet");

        // 42.67 ms of output path plus 21 ms of input path, which is the shape of this
        // machine s answer: `Playback::stream_delay` plus `Capture::stream_delay`.
        apm.set_stream_delay(Some(Duration::from_micros(63_670)));
        assert_eq!(apm.stream_delay(), Some(Duration::from_micros(63_670)));

        apm.set_stream_delay(None);
        assert_eq!(
            apm.stream_delay(),
            None,
            "`None` has to be reachable again: it is what puts AEC3 back on its own estimate, \
             and a speaker that closed is exactly when that is wanted"
        );
    }

    /// A backend reporting something absurd is clamped, not refused and not wrapped.
    ///
    /// The library takes a `u16` of milliseconds, so an unclamped figure would arrive as
    /// whatever `as u16` made of it — 1000 ms would become 1000, but 70 seconds would become
    /// 4464, a confident wrong number with nothing to say it was one.
    #[test]
    fn an_absurd_delay_is_clamped_rather_than_wrapped() {
        let apm = Apm::new().expect("a processor at the capture rate");

        apm.set_stream_delay(Some(Duration::from_secs(70)));

        assert_eq!(apm.stream_delay(), Some(MAX_STREAM_DELAY));
    }

    /// The wire shape the window switches on, pinned here rather than discovered in TypeScript.
    #[test]
    fn conditioning_serializes_as_a_tagged_union_in_camel_case() {
        assert_eq!(
            serde_json::to_string(&Conditioning::Full).expect("serializable"),
            r#"{"state":"full"}"#
        );
        assert_eq!(
            serde_json::to_string(&Conditioning::Untouched { reason: "no".into() })
                .expect("serializable"),
            r#"{"state":"untouched","reason":"no"}"#
        );
    }

    /// A fault is rendered for a person somewhere, so it has to read as a sentence.
    #[test]
    fn a_fault_says_what_was_wrong_with_the_frame() {
        assert_eq!(
            Fault::WrongLength { wanted: 160, got: 314 }.to_string(),
            "the audio processor takes exactly 160 samples and got 314"
        );
        assert!(Fault::Refused("bad sample rate".into()).to_string().contains("bad sample rate"));
    }

/// How much of a loudspeaker `feeding_the_render_side_is_what_makes_the_canceller_cancel`
/// requires the canceller to remove, in dB, once it has been shown what is playing.
///
/// **Measured, not chosen.** On this machine the canceller takes 41 dB off a synthetic echo it
/// has been told about and 12 dB off the same echo it has not — the second figure being the
/// noise suppressor, which removes broadband noise whatever the render side is doing. 25 dB sits
/// between the two with room on both sides for a slower or busier machine.
///
/// It is a measurement of **this wiring, not of a room**: see the test for what the echo path
/// is and what it is not.
#[cfg(feature = "aec")]
const CANCELLED_DB: f64 = 25.0;

/// What the same measurement reads with no reference ever fed, which is the state it is
/// discriminating against. The noise suppressor alone scored 11.92 dB here.
#[cfg(feature = "aec")]
const UNCANCELLED_DB: f64 = 16.0;

    /// Everything below runs only where the library exists, which is **this machine and
    /// nowhere else** — not either CI runner, not the `.deb`, not the `.exe`. Read
    /// `crates/zyris-voice/Cargo.toml` for why that is forced rather than chosen.
    #[cfg(feature = "aec")]
    mod with_the_echo_canceller {
        use super::*;

        /// **The measurement this module's documentation rests on.**
        ///
        /// Capture-only is not an error state: with `EchoCanceller::Full` configured and no
        /// render frame ever fed, every capture frame is accepted and the canceller removes
        /// nothing. Asserting the ERLE is *small* rather than absent is the point — a later
        /// reader finding 0.18 dB in the statistics must be able to see that it was expected.
        #[test]
        fn capture_only_is_accepted_and_cancels_nothing() {
            let apm = Apm::new().expect("a processor at the capture rate");

            // A second of speech-shaped input, and not one render frame.
            for i in 0..100 {
                let mut frame: Vec<f32> = (0..APM_FRAME)
                    .map(|n| {
                        let t = (i * APM_FRAME + n) as f32 / crate::capture::SAMPLE_RATE as f32;
                        0.3 * (std::f32::consts::TAU * 440.0 * t).sin()
                    })
                    .collect();
                assert_eq!(apm.process_capture(&mut frame), Ok(()));
            }

            // `Some`, not "`Some` or `None`". The canceller reports a figure because it is
            // running; it is the *figure* that says it has nothing to cancel. Tolerating `None`
            // here would make this test pass for an `Apm` built with no echo canceller at all,
            // which is the one mutation nothing else in step 7 can distinguish.
            let erle = apm.erle_db().expect(
                "AEC3 is configured, so it reports an enhancement figure even when it is zero",
            );
            assert!(
                erle < 1.0,
                "with nothing ever played there is nothing to cancel; 0.1755 dB was measured \
                 here on 2026-09-15 and anything near it is the expected answer, not a bug \
                 (got {erle} dB)"
            );
        }

        /// The seam, exercised with nothing having played.
        #[test]
        fn the_render_seam_accepts_a_frame_before_anything_has_ever_played() {
            let apm = Apm::new().expect("a processor at the capture rate");

            assert_eq!(apm.analyze_render(&vec![0.0; APM_FRAME]), Ok(()));
            let mut capture = vec![0.0; APM_FRAME];
            assert_eq!(apm.process_capture(&mut capture), Ok(()));
        }

        /// **The measurement the whole render side is instrumented by**, and the thing that
        /// separates a canceller doing its job from one that is merely configured.
        ///
        /// # What this echo path is, and what it is not
        ///
        /// Synthetic: the reference delayed by [`ECHO_DELAY`] and halved, with nothing else in
        /// the microphone. That is a loudspeaker a foot from a microphone to a first
        /// approximation and it is **not a room** — no reflections, no non-linearity, no moving
        /// microphone, no near-end speech. Nobody has yet held this machine's microphone in front
        /// of this machine's speakers playing this machine's voice, and the README says so under
        /// what nobody has checked by hand.
        ///
        /// What it does decide is the half that can be wired up wrongly and look fine: whether
        /// [`Apm::analyze_render`] is being called at all, with what was actually played, at the
        /// rate and the framing the library wants. A canceller that is never shown a reference
        /// returns `Ok` from every call and reports a number.
        ///
        /// # The control is the same run with the render side switched off
        ///
        /// Necessary, because **the noise suppressor removes broadband noise whether or not
        /// there is an echo canceller** — 11.92 dB of it here. An assertion on the figure alone
        /// would pass for a build that never fed a reference. The two runs share every line
        /// except the one that matters.
        ///
        /// # Four seconds, and the reason the number is not asserted over more
        ///
        /// **The cancellation stops after about six seconds of this stimulus**, sharply: 20 to
        /// 30 dB per second up to second five and 0.4 dB from second six onwards, with the same
        /// cliff for stationary noise, for gated noise and for looped speech, and whether or not
        /// a near-end signal is present. It has not been characterised further and it may well be
        /// an artefact of an echo that is a perfect delayed copy forever — AEC3 has stationarity
        /// and audibility heuristics that a real room never satisfies. **It is a reason not to
        /// read the figure below as a prediction of a room**, and it is written down here rather
        /// than tuned around.
        #[test]
        fn feeding_the_render_side_is_what_makes_the_canceller_cancel() {
            /// 20 ms, which is two frames — long enough that a canceller ignoring the reference
            /// cannot subtract it by accident, short enough to be an ordinary desk.
            const ECHO_DELAY: usize = crate::capture::SAMPLE_RATE as usize / 50;
            const SECONDS: usize = 4;

            let far = noise(crate::capture::SAMPLE_RATE as usize * SECONDS + ECHO_DELAY, 99);

            // dB of echo taken out of the microphone, measured after the first second so that
            // the filter has converged.
            let removed = |feed_render: bool| {
                let apm = Apm::new().expect("a processor at the capture rate");
                let (mut went_in, mut came_out) = (0.0f64, 0.0f64);
                for i in 0..(crate::capture::SAMPLE_RATE as usize * SECONDS / APM_FRAME) {
                    let at = i * APM_FRAME;
                    if feed_render {
                        apm.analyze_render(&far[at..at + APM_FRAME])
                            .expect("a frame of the right length");
                    }
                    // What the microphone picks up: the loudspeaker, delayed and quieter.
                    let mut heard: Vec<f32> = far[at + ECHO_DELAY..at + ECHO_DELAY + APM_FRAME]
                        .iter()
                        .map(|sample| sample * 0.5)
                        .collect();
                    let before: f64 = heard.iter().map(|s| f64::from(*s).powi(2)).sum();
                    apm.process_capture(&mut heard).expect("a frame of the right length");
                    if at >= crate::capture::SAMPLE_RATE as usize {
                        went_in += before;
                        came_out += heard.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>();
                    }
                }
                10.0 * (went_in / came_out.max(1e-18)).log10()
            };

            let without = removed(false);
            let with = removed(true);

            assert!(
                without < UNCANCELLED_DB,
                "the control has to stay a control: with no reference the only thing removing \
                 anything is the noise suppressor, which measured 11.92 dB here (got {without} dB)"
            );
            assert!(
                with > CANCELLED_DB,
                "feeding the reference is the whole of the render side: the canceller took \
                 {with} dB off a loudspeaker it was told about against {without} dB off the same \
                 one it was not, and a figure near the control means nothing is reaching \
                 `analyze_render`"
            );
        }

        /// **`echo_return_loss_enhancement` is a constant in this build and cannot instrument
        /// anything.** The plan and this repository's own notes both say ERLE is what tells
        /// anybody whether barge-in is working; measured here, it is not.
        ///
        /// It reads **0.17551203072071075 dB** — the same figure to every digit — with no
        /// reference, with `analyze_render_frame`, with `process_render_frame`, at six seconds,
        /// at fifteen and at thirty, while the echo measured by
        /// `feeding_the_render_side_is_what_makes_the_canceller_cancel` goes from 12 dB to 41 dB.
        /// `echo_return_loss` is a constant -30.0 and `delay_ms` a constant 16 beside it, the
        /// last of those even when no render frame has ever been fed.
        ///
        /// So this is pinned as a **known-useless** statistic rather than removed: `erle_db` is
        /// public, a screen could reasonably show it, and the next person to reach for it needs
        /// to find this rather than a plausible-looking number. What says the render side is
        /// running is `crate::playback::Render::analysed`, and what says it works is the
        /// measurement above.
        #[test]
        fn the_libraries_own_erle_figure_says_nothing_about_whether_it_is_cancelling() {
            const ECHO_DELAY: usize = crate::capture::SAMPLE_RATE as usize / 50;
            let far = noise(crate::capture::SAMPLE_RATE as usize * 4 + ECHO_DELAY, 99);

            let erle = |feed_render: bool| {
                let apm = Apm::new().expect("a processor at the capture rate");
                for i in 0..(crate::capture::SAMPLE_RATE as usize * 4 / APM_FRAME) {
                    let at = i * APM_FRAME;
                    if feed_render {
                        apm.analyze_render(&far[at..at + APM_FRAME]).expect("the right length");
                    }
                    let mut heard: Vec<f32> = far[at + ECHO_DELAY..at + ECHO_DELAY + APM_FRAME]
                        .iter()
                        .map(|sample| sample * 0.5)
                        .collect();
                    apm.process_capture(&mut heard).expect("the right length");
                }
                apm.erle_db().expect("AEC3 reports a figure whether or not it has cancelled")
            };

            assert_eq!(
                erle(true),
                erle(false),
                "if these ever differ, the library has started reporting a real figure and this \
                 test and everything written around it should be revisited"
            );
        }

        /// **`set_config` replaces rather than merges**, so the call that declares the delay is
        /// also a call that could switch the high-pass filter and the noise suppressor off — a
        /// microphone quietly getting worse at the moment a speaker opens, with nothing to say
        /// so. The assertion is the DC-offset one, run *after* the delay has been declared.
        #[test]
        fn declaring_the_delay_does_not_turn_the_rest_of_the_processing_off() {
            let apm = Apm::new().expect("a processor at the capture rate");
            apm.set_stream_delay(Some(Duration::from_millis(64)));

            let mean = |frame: &[f32]| frame.iter().sum::<f32>() / frame.len() as f32;
            let mut last = vec![0.0; APM_FRAME];
            for i in 0..50 {
                let mut frame: Vec<f32> = (0..APM_FRAME)
                    .map(|n| {
                        let t = (i * APM_FRAME + n) as f32 / crate::capture::SAMPLE_RATE as f32;
                        0.3 + 0.3 * (std::f32::consts::TAU * 50.0 * t).sin()
                    })
                    .collect();
                apm.process_capture(&mut frame).expect("a frame of the right length");
                last = frame;
            }

            assert!(
                mean(&last).abs() < 0.05,
                "the high-pass filter is still running after the delay was declared; {} of DC \
                 came through, which is what a configuration written in two places looks like",
                mean(&last)
            );
        }

        /// Broadband noise, which is what an echo canceller is easiest to measure against: a
        /// tone would be taken out by the noise suppressor before the canceller saw it.
        fn noise(len: usize, seed: u32) -> Vec<f32> {
            let mut seed = seed;
            (0..len)
                .map(|_| {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    ((seed >> 8) as f32 / 8388608.0 - 1.0) * 0.4
                })
                .collect()
        }

        /// **The first of the three things a build without `aec` loses, measured.**
        ///
        /// A DC offset with mains hum on it is exactly what the high-pass filter is for, and
        /// leaving it in is not cosmetic: it is what puts a power supply over the voice
        /// detector's threshold. The assertion is on the offset being gone from the output,
        /// because that is the property, and a test on "the output differs" would pass for a
        /// filter that had changed the audio in any way at all.
        #[test]
        fn the_high_pass_filter_takes_a_dc_offset_out_from_under_the_detector() {
            let apm = Apm::new().expect("a processor at the capture rate");

            let mean = |frame: &[f32]| frame.iter().sum::<f32>() / frame.len() as f32;
            let mut last = vec![0.0; APM_FRAME];

            // The filter has state, so it needs a moment; half a second is plenty.
            for i in 0..50 {
                let mut frame: Vec<f32> = (0..APM_FRAME)
                    .map(|n| {
                        let t = (i * APM_FRAME + n) as f32 / crate::capture::SAMPLE_RATE as f32;
                        0.3 + 0.3 * (std::f32::consts::TAU * 50.0 * t).sin()
                    })
                    .collect();
                apm.process_capture(&mut frame).expect("a frame of the right length");
                last = frame;
            }

            assert!(
                mean(&last).abs() < 0.05,
                "a constant 0.3 offset went in and {} came out; the high-pass filter is what \
                 removes it, and a build without `aec` does not have one",
                mean(&last)
            );
        }
        /// **The second of the three, measured.** Noise suppression takes about 12 dB off a
        /// steady noise floor: 0.02904 RMS in and 0.00713 out over the last second of three
        /// (2026-09-15). The bar here is 6 dB, which is well clear of what the high-pass filter
        /// alone could do to white noise — below 80 Hz is about half a percent of the band — so
        /// this fails if noise suppression is switched off and passes if only the filter is.
        ///
        /// It matters because of what is downstream: `earshot` claims resilience only to about
        /// 3 dB SNR and whisper invents sentences out of noise. Both failures are silent.
        #[test]
        fn noise_suppression_takes_the_room_down_before_the_model_hears_it() {
            let apm = Apm::new().expect("a processor at the capture rate");
            let mut seed = 12345u32;
            let raw: Vec<f32> = (0..crate::capture::SAMPLE_RATE as usize * 3)
                .map(|_| {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    ((seed >> 8) as f32 / 8388608.0 - 1.0) * 0.05
                })
                .collect();

            let mut cleaned = raw.clone();
            for frame in cleaned.chunks_exact_mut(APM_FRAME) {
                apm.process_capture(frame).expect("a frame of the right length");
            }

            // The last second only: the suppressor has to learn the noise before it can remove
            // it, and averaging over the whole clip would hide how well it ends up doing.
            let rms = |samples: &[f32]| {
                (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
            };
            let last_second = raw.len() - crate::capture::SAMPLE_RATE as usize;
            let (before, after) = (rms(&raw[last_second..]), rms(&cleaned[last_second..]));

            assert!(
                after < before / 2.0,
                "a steady noise floor of {before} came out at {after}; a build without `aec` \
                 has no noise suppression and hands all of it to the detector and the model"
            );
        }

        /// **What a build without `aec` loses, as one measurement rather than three
        /// paragraphs.**
        ///
        /// A DC offset with mains hum on it puts `earshot` over its threshold in 6 of 125
        /// frames on this machine — a power supply telling the voice detector somebody is
        /// talking. The same buffer through the high-pass filter puts it over in none. That is
        /// the whole of the argument for the `aec` feature being worth having, and it is the
        /// half of it that is real *today*: the echo canceller itself does nothing until step 8.
        ///
        /// It lives here rather than in `vad` because it is a claim about the processor.
        #[test]
        fn cleaning_the_frame_first_keeps_a_power_supply_out_of_the_voice_detector() {
            use crate::vad::{Endpointer, Listening};

            let hum = |n: usize| {
                let t = n as f32 / crate::capture::SAMPLE_RATE as f32;
                0.3 + 0.3 * (std::f32::consts::TAU * 50.0 * t).sin()
            };
            let raw: Vec<f32> = (0..crate::capture::SAMPLE_RATE as usize * 2).map(hum).collect();

            let apm = Apm::new().expect("a processor at the capture rate");
            let mut cleaned = raw.clone();
            for frame in cleaned.chunks_exact_mut(APM_FRAME) {
                apm.process_capture(frame).expect("a frame of the right length");
            }

            let voiced = |samples: &[f32]| {
                let mut endpointer = Endpointer::new();
                samples
                    .chunks_exact(crate::capture::VAD_FRAME)
                    .filter(|frame| {
                        endpointer.push(frame).expect("the right length") == Listening::Speech
                    })
                    .count()
            };

            assert!(
                voiced(&raw) > 0,
                "the point of this test is that untouched hum reaches the detector; if it no \
                 longer does, the stimulus has stopped being the one that was measured"
            );
            assert_eq!(
                voiced(&cleaned),
                0,
                "the high-pass filter is what takes it back out, and a build without `aec` has \
                 no high-pass filter"
            );
        }
    }
}
