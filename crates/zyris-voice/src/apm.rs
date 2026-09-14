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

use crate::capture::APM_FRAME;

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
}

impl Apm {
    /// Build one for [`SAMPLE_RATE`].
    ///
    /// Fails only where there is a library to fail: a build without `aec` cannot refuse. The
    /// signature is a `Result` in both states anyway, so that the caller has one code path.
    pub fn new() -> Result<Apm, Fault> {
        #[cfg(not(feature = "aec"))]
        {
            Ok(Apm {})
        }
        #[cfg(feature = "aec")]
        {
            use webrtc_audio_processing::config::{
                Config, EchoCanceller, HighPassFilter, NoiseSuppression, NoiseSuppressionLevel,
            };

            let processor = webrtc_audio_processing::Processor::new(crate::capture::SAMPLE_RATE)
                .map_err(|error| Fault::Refused(error.to_string()))?;

            processor.set_config(Config {
                // On — and **while noise suppression is also on this line changes nothing**,
                // which was found by mutating it away and watching every test stay green.
                // `Config::noise_suppression`'s own documentation says it force-enables high
                // pass filtering, so the C++ runs the filter either way. The line stays
                // because it says what is wanted rather than what happens to follow, and it
                // becomes load-bearing the moment noise suppression is turned down.
                high_pass_filter: Some(HighPassFilter { apply_in_full_band: true }),
                // `Full` is AEC3 with the delay estimated rather than declared, because this
                // process does not yet know what the playback path costs — step 8 does, and
                // `EchoCanceller::Full { stream_delay_ms }` is where it goes when it does.
                echo_canceller: Some(EchoCanceller::Full { stream_delay_ms: None }),
                // `Moderate` rather than `High`: the levels above it trade speech distortion
                // for quiet, and the consumer downstream is a speech model, not a listener.
                noise_suppression: Some(NoiseSuppression {
                    level: NoiseSuppressionLevel::Moderate,
                    analyze_linear_aec_output: false,
                }),
                // Deliberately off. A gain controller would push samples past the [-1, 1] that
                // `earshot` documents, and whisper normalises its own input anyway.
                ..Config::default()
            });

            Ok(Apm { processor })
        }
    }

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
    /// **Nothing in step 7 calls this, and that is the point of it being here.** What step 7
    /// owes step 8 is a processor that is already owned, already configured and already
    /// reachable from the playback side, so that step 8 adds a caller rather than a redesign.
    /// Until there is one, [`Self::erle_db`] explains what the echo canceller is doing.
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
    /// # This reads 0.18 dB in step 7, and that is not a bug
    ///
    /// Measured on this machine, 2026-09-15: with [`EchoCanceller::Full`] configured and
    /// **no render frame ever fed**, `process_capture_frame` returns `Ok` and ERLE settles at
    /// **0.18 dB** — which is to say it cancels nothing and errors at nothing. There is nothing
    /// for it to cancel: step 7 has no playback, so the canceller has never been shown a
    /// reference signal. It becomes real the first time step 8 calls [`Self::analyze_render`].
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

        /// The seam step 8 will use, exercised now so that step 8 finds a caller's worth of
        /// wiring rather than a question.
        #[test]
        fn the_render_seam_accepts_a_frame_before_anything_has_ever_played() {
            let apm = Apm::new().expect("a processor at the capture rate");

            assert_eq!(apm.analyze_render(&vec![0.0; APM_FRAME]), Ok(()));
            let mut capture = vec![0.0; APM_FRAME];
            assert_eq!(apm.process_capture(&mut capture), Ok(()));
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
