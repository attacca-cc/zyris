//! Whisper, the file it needs on disk, and the one parameter that makes it fast enough to
//! hold a conversation with.
//!
//! # The encoder walks thirty seconds whatever you give it
//!
//! A three-second utterance costs nearly what an eleven-second one does, because the encoder
//! runs over a fixed thirty-second window regardless of how much audio is in it. So "whisper
//! base stays ahead of real time on a slow CPU" is true of a long clip and false of the thing
//! a person waiting for an answer actually notices. [`audio_ctx`] is the lever that fixes it,
//! and it is the reason this module exists rather than three lines in the session.
//!
//! Measured on this machine (i3-7100U, 4 threads) with `ggml-base.bin`, **release build** —
//! see the note on [`Stt::transcribe`] about why that qualifier matters — over
//! `tests/audio/jfk.wav`:
//!
//! | clip | `audio_ctx` | time |
//! |---|---|---|
//! | 1.5 s | 1500 (whisper's default) | 3.55 s |
//! | 1.5 s | **450** (this module's rule) | **0.81 s** |
//! | 3 s | 1500 | 3.63 s |
//! | 3 s | **450** | **0.88 s** |
//! | 11 s | 1500 | 3.92 s |
//! | 11 s | **583** (this module's rule) | **1.32 s** |
//!
//! Thirteen windows of that recording — one second to eleven, from the start and from the
//! middle — were run through the shipped rule and every one of them transcribed correctly, in
//! 0.81 s to 1.01 s for anything under eight seconds.
//!
//! **The lever is sharp at the other end**, and that is the half this module is really about:
//! a context that is too small does not lose a little accuracy, it makes whisper invent — and
//! then costs *more* time than leaving it alone, because inventing means decoding more tokens
//! and then decoding the whole thing again at a higher temperature. [`MIN_CTX`] carries the
//! measurements.
//!
//! Nothing here opens a microphone or publishes a [`crate::VoiceEvent`]. Task 6's session is
//! the only caller.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::capture::SAMPLE_RATE;

// **The file machinery is `model.rs`'s and there is one copy of it.** `Model`, `ModelState`,
// `fetch` and the rest were written here for `ggml-base.bin` and moved out when Supertonic's
// six files needed exactly the same `.part`-hash-rename dance; they are re-exported under
// their old names so that every caller in this crate and in the window goes on naming
// `stt::Model` for the speech model and `tts::…` for the other one.
pub use crate::model::{
    Model, ModelState, PART_SUFFIX, Progress, cache_dir, inspect,
    model_path_given,
};

/// The model the product ships against: whisper `base`, multilingual, 141 MB.
///
/// A different size is a code change, deliberately — see the plan's "Deliberately not here".
pub const BASE: Model = Model {
    url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/\
          5359861c739e955e79d9a303bcbc70fb988958b1/ggml-base.bin",
    file: "ggml-base.bin",
    bytes: 147_951_465,
    sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
};

/// Names a model file directly, bypassing the cache directory entirely.
///
/// Two callers: this crate's own tests, which must not download 141 MB to run, and a person
/// who already has a `ggml-*.bin` and would rather not have a second copy. **A file named
/// this way is not size-checked** — see [`state`] — because it is an operator's explicit
/// choice and may deliberately be a different size of model.
pub const MODEL_ENV: &str = "ZYRIS_WHISPER_MODEL";

/// What went wrong, in words a window can render.
///
/// **Two arms of its own and a borrowed one.** Everything about getting the file onto this
/// disk is [`crate::model::Fault`], which `tts.rs` shares; what is left here is what only
/// whisper can produce. Spelling those four out a second time would be two enums that have to
/// agree with nothing to say when they stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// The model file could not be fetched, checked or stored. See [`crate::model::Fault`].
    File(crate::model::Fault),
    /// whisper.cpp refused the model or the audio.
    Whisper { detail: String },
    /// The blocking transcription task went away — a panic inside whisper, or a shutting-down
    /// runtime. Its own variant because it says nothing about the audio.
    Lost,
}

impl From<crate::model::Fault> for Fault {
    fn from(fault: crate::model::Fault) -> Fault {
        Fault::File(fault)
    }
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fault::File(fault) => write!(f, "{fault}"),
            Fault::Whisper { detail } => write!(f, "speech recognition failed: {detail}"),
            Fault::Lost => write!(f, "speech recognition stopped before it produced anything"),
        }
    }
}

impl std::error::Error for Fault {}
/// Where the model file should be, honouring [`MODEL_ENV`].
pub fn model_path(model: &Model) -> Option<PathBuf> {
    model_path_given(model, std::env::var_os(MODEL_ENV))
}

/// What the window asks: is the speech model here?
///
/// Cheap — one `stat`. The digest is **not** re-checked here, and that is a decision: hashing
/// 141 MB costs about a second of disk and CPU on this machine, on every launch, to defend
/// against a case [`fetch`] already makes impossible. The length is checked because it is free
/// and because it is what a half-copied file gets wrong.
pub fn state(model: &Model) -> ModelState {
    let Some(path) = model_path(model) else {
        return ModelState::Nowhere {
            reason: "this system does not name a cache directory for downloaded files".into(),
        };
    };
    let expected = if std::env::var_os(MODEL_ENV).is_some_and(|v| !v.is_empty()) {
        None
    } else {
        Some(model.bytes)
    };
    inspect(&path, expected)
}

/// Download the speech model into `dir`. See [`crate::model::fetch`] for what it guarantees.
pub async fn fetch(
    model: &Model,
    dir: &Path,
    progress: impl FnMut(Progress),
) -> Result<std::path::PathBuf, Fault> {
    crate::model::fetch(model, dir, progress).await.map_err(Fault::File)
}

/// The window the encoder always walks, whatever it is given.
pub const FULL_WINDOW: Duration = Duration::from_secs(30);

/// The encoder positions that window maps to, and the model's own maximum.
///
/// Read back from the model as `n_audio_ctx`; asking for more is refused by whisper.cpp with
/// `audio_ctx is larger than the maximum allowed`.
pub const FULL_CTX: u16 = 1500;

/// Samples of 16 kHz audio per encoder position: 20 ms each.
pub const SAMPLES_PER_CTX: usize =
    (SAMPLE_RATE as usize * FULL_WINDOW.as_secs() as usize) / FULL_CTX as usize;

/// Headroom past the end of the audio: thirty-two positions, 640 ms.
///
/// **This is not what protects a short utterance, and an earlier version of this file said it
/// was.** Cutting at exactly the length of the audio is certainly wrong — see the table under
/// [`MIN_CTX`] — but so is cutting 640 ms past it. The thing that has to hold is a *floor* on
/// the whole context, and [`MIN_CTX`] is it. This constant stays because it costs nothing
/// (32 positions on an eleven-second turn is 3 ms) and because a little silence after the last
/// word is what tells the decoder the speech ended.
pub const CTX_MARGIN: u16 = 32;

/// The shortest audio whisper.cpp will look at.
///
/// Not ours: `whisper_full_with_state` returns **0 with no segments and a warning on stderr**
/// when the spectrogram is under ten frames — "input is too short - N ms < 100 ms". Read in
/// the vendored `whisper.cpp/src/whisper.cpp` at `delta_min = 10`. [`Stt::transcribe`] refuses
/// it up front instead, because "succeeded and said nothing" and "was never looked at" are not
/// the same answer and a session cannot tell them apart afterwards.
pub const MIN_AUDIO: Duration = Duration::from_millis(100);

/// **The floor, and the number this module turns on.** 300 positions is six seconds of encoder
/// context, whatever the utterance is.
///
/// Below about 215 positions whisper `base` stops transcribing what it heard and starts saying
/// something else. Measured on this machine on 2026-09-14, release build, `ggml-base.bin`,
/// `tests/audio/jfk.wav` — the first 1.5 s of it, whose words are "and so my fellow", and the
/// first 3 s, whose words are "and so my fellow Americans":
///
/// | clip | `audio_ctx` | time | what it said |
/// |---|---|---|---|
/// | 1.5 s | 75 (exact cut) | 2.60 s | "and saw my fellow" ×many |
/// | 1.5 s | 107 (cut + 32) | 6.17 s | "and saw my phone" ×many |
/// | 1.5 s | 139 | 3.56 s | **"And saw my phone"** — wrong, and not repeating |
/// | 1.5 s | 175 | 1.11 s | **"And saw my phone"** |
/// | 1.5 s | **225** | 0.72 s | "And so my fellow" |
/// | 1.5 s | 1500 (the full window) | 3.74 s | "And so my fellow" |
/// | 3 s | 150 (exact cut) | 2.71 s | its own sentence **44 times** |
/// | 3 s | 182 (cut + 32) | 10.74 s | its own sentence **36 times** |
/// | 3 s | **214** | 0.45 s | "And so my fellow Americans" |
/// | 3 s | 1500 (the full window) | 3.79 s | "And so my fellow Americans" |
/// | 11 s | 551 (exact cut) | 1.24 s | the whole sentence, correctly |
/// | 11 s | 1500 (the full window) | 3.85 s | the whole sentence, correctly |
///
/// And the clip that set the value, three seconds taken from 4.4 s in — a window that starts
/// and ends in the middle of a phrase, which is what a person who begins speaking before the
/// key is down produces. Its words are "what your country can do for you":
///
/// | `audio_ctx` | time | what it said |
/// |---|---|---|
/// | 300 | 4.93 s | "What your country can do for you? **What your country can do for you?**" |
/// | 375 | 4.26 s | right |
/// | **450** | **2.03 s** | right |
/// | 600 | 1.63 s | right |
/// | 1500 (the full window) | 4.99 s | right |
///
/// Three things fall out of that, and only the first was expected:
///
/// 1. **An undersized context is slow *and* wrong.** It is not a quality setting.
/// 2. **The margin is not the rule.** Eleven seconds is right with *no* margin at all, and
///    three seconds is wrong with the documented one. What both short cases need is a
///    context of about 215 positions in absolute terms — a little over four seconds — so the
///    guard is a floor, not an offset.
/// 3. **There is a band in between where it is fast and quietly wrong**: 1.5 s at 175
///    answered "And saw my phone" in 1.11 s with no repetition and nothing to notice, and the
///    mid-phrase clip at 300 said its sentence twice. That is the failure this floor exists
///    for, and it is why 450 and not the 225 the first two clips would have allowed: the
///    errors are not the same size. Being slow costs a second; being wrong sends an agent an
///    instruction nobody gave.
///
/// **450 is also not simply the cautious end of a trade.** On the clip that needed it, it is
/// two and a half times *faster* than leaving the lever alone, because a context that makes
/// whisper repeat itself makes it decode for longer and then makes its own temperature
/// fallback decode the whole thing again. Too small is slow; only too large is merely slow.
pub const MIN_CTX: u16 = 450;

/// How many encoder positions to give whisper for `samples` of 16 kHz mono audio.
///
/// `ceil(seconds / 30 * 1500) + 32`, **floored at [`MIN_CTX`]** and clamped to [`FULL_CTX`].
/// The floor is the part that matters and [`MIN_CTX`] carries the measurements.
///
/// **The clamp is what keeps a long turn merely slow rather than wrong.** `earshot` reads a
/// steady 440 Hz tone as a voice in 121 frames out of 125 — an alarm or a held note at
/// speaking pitch keeps a turn open — so an utterance arriving here is not guaranteed to be
/// short. Past thirty seconds there is nothing left to ask for: the model has 1500 positions
/// and whisper.cpp refuses more.
pub fn audio_ctx(samples: usize) -> u16 {
    let positions = samples.div_ceil(SAMPLES_PER_CTX);
    let with_margin = positions.saturating_add(CTX_MARGIN as usize);
    with_margin.clamp(MIN_CTX as usize, FULL_CTX as usize) as u16
}

/// The exact-cut value: the context that covers the audio and nothing more.
///
/// Public so a test can assert [`audio_ctx`] is **not** it. That is the assertion that stops
/// somebody reading [`CTX_MARGIN`] as decoration and simplifying it away, so it needs
/// something to compare against that is not a second copy of the same arithmetic.
pub fn exact_cut(samples: usize) -> u16 {
    samples.div_ceil(SAMPLES_PER_CTX).min(FULL_CTX as usize) as u16
}

/// The language whisper is told it is hearing.
///
/// **Told, not asked.** `set_language(None)` turns on detection, and detection cost 10.79 s
/// against 3.44 s on the same 3 s clip here — over three times slower, for a question this
/// product already knows the answer to. Korean is unmeasured and is step 8's problem.
pub const LANGUAGE: &str = "en";

/// How many threads whisper gets.
///
/// Four on this machine, which is what every measurement in this module was taken with. The
/// cap is there because whisper.cpp does not get faster past a machine's physical cores and
/// a 32-thread server would spend the difference on contention.
pub fn threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8)
}

/// Everything about a transcription that is a decision rather than a default.
///
/// A plain struct, and not just a `FullParams` built inline, because three of these were
/// measured and one of them is a trap — and `FullParams` has no getters, so a test could
/// otherwise only assert on them by reading this file as text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// `Some(LANGUAGE)`. `None` would mean detection; see [`LANGUAGE`].
    pub language: Option<&'static str>,
    /// See [`threads`].
    pub threads: usize,
    /// See [`audio_ctx`]. The whole reason this module exists.
    pub audio_ctx: u16,
    /// Always false. This is speech *into* the machine; translating it would silently answer
    /// an English question somebody asked in Korean.
    pub translate: bool,
    /// Always false: each turn is decoded without the previous turn's tokens.
    ///
    /// whisper carries the last transcript in as a prompt by default, which helps a
    /// continuous recording and hurts this: turns here are minutes apart and about different
    /// things, and a stale prompt is how whisper starts repeating a sentence nobody said.
    pub carry_previous_turn: bool,
}

impl Settings {
    /// The settings for one utterance of `samples` 16 kHz mono samples.
    pub fn for_audio(samples: usize) -> Settings {
        Settings {
            language: Some(LANGUAGE),
            threads: threads(),
            audio_ctx: audio_ctx(samples),
            translate: false,
            carry_previous_turn: false,
        }
    }
}

/// Whisper, loaded, and able to transcribe.
///
/// `Send + Sync`: `WhisperContext` is both, which is what lets one live behind an `Arc` and be
/// handed to [`transcribe`]'s blocking task over and over rather than reloaded per turn.
pub struct Stt {
    context: whisper_rs::WhisperContext,
}

impl Stt {
    /// Load a model from disk.
    ///
    /// Installs whisper.cpp's logging hooks first, once per process. Without them the library
    /// writes a screenful of model parameters to stderr on every load, which in `--headless`
    /// is the process log.
    pub fn load(path: &Path) -> Result<Stt, Fault> {
        static HOOKS: std::sync::Once = std::sync::Once::new();
        HOOKS.call_once(whisper_rs::install_logging_hooks);

        let mut parameters = whisper_rs::WhisperContextParameters::default();
        // No GPU. The default is already off without a GPU feature; saying so keeps it off
        // the day somebody turns one on for a different reason.
        parameters.use_gpu = false;

        let context = whisper_rs::WhisperContext::new_with_params(path, parameters)
            .map_err(|e| Fault::Whisper { detail: format!("{path:?} did not load: {e}") })?;
        Ok(Stt { context })
    }

    /// Transcribe one utterance. **Blocks** — see [`transcribe`] for the callable-from-async
    /// form, and do not call this one from a runtime thread.
    ///
    /// **Every timing in this module is a release figure, and the qualifier is load-bearing.**
    /// `whisper-rs-sys/build.rs` branches on `cfg!(debug_assertions)`: a release build gets
    /// `CMAKE_BUILD_TYPE=Release`, and everything else gets `RelWithDebInfo` **plus
    /// `-DWHISPER_DEBUG`**, whisper.cpp's verbose trace logging, one of whose 26 call sites is
    /// inside the per-token decode loop. Same machine, same model, same three seconds:
    /// **0.3 s under `cargo test --release` and 28.0 s under `cargo test`.** So an ordinary
    /// `cargo test` cannot measure this, and the one test that tries takes its deadline from
    /// `cfg!(debug_assertions)` for that reason.
    ///
    /// Audio is 16 kHz mono `f32`, which is what `capture::Capture` delivers.
    ///
    /// Audio shorter than [`MIN_AUDIO`] answers `Ok("")` without asking whisper, because
    /// whisper would answer the same thing by a different route — a warning and zero
    /// segments — and a caller cannot tell that apart from a silent room.
    pub fn transcribe(&self, audio: &[f32]) -> Result<String, Fault> {
        if audio.len() < samples_in(MIN_AUDIO) {
            return Ok(String::new());
        }
        let settings = Settings::for_audio(audio.len());

        let mut state = self
            .context
            .create_state()
            .map_err(|e| Fault::Whisper { detail: format!("no decoder state: {e}") })?;

        let mut params =
            whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(settings.language);
        params.set_n_threads(settings.threads as std::ffi::c_int);
        params.set_audio_ctx(settings.audio_ctx as std::ffi::c_int);
        params.set_translate(settings.translate);
        params.set_no_context(!settings.carry_previous_turn);
        // Nothing of whisper's own goes to stdout: this process has a window and a log, and
        // neither of them is a terminal it may print to.
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_no_timestamps(true);

        state
            .full(params, audio)
            .map_err(|e| Fault::Whisper { detail: format!("transcription failed: {e}") })?;

        let mut text = String::new();
        for segment in state.as_iter() {
            // Lossy on purpose: a model that emits one invalid UTF-8 byte must cost a
            // replacement character, not the whole sentence.
            text.push_str(segment.to_str_lossy().map_err(|e| Fault::Whisper {
                detail: format!("a transcribed segment could not be read: {e}"),
            })?.as_ref());
        }
        Ok(clean(&text))
    }
}

/// How many 16 kHz samples a span is.
pub fn samples_in(span: Duration) -> usize {
    (span.as_secs_f64() * SAMPLE_RATE as f64).round() as usize
}

/// Transcribe without blocking the runtime.
///
/// `state.full()` is a synchronous call into whisper.cpp that occupies a thread for as long as
/// it takes — a third of a second at best here, and seconds on a long turn — so it goes to the
/// blocking pool. The core has a window, a tray, a websocket and an audit log on the same
/// runtime, and none of them may stop while somebody is being transcribed.
pub async fn transcribe(stt: std::sync::Arc<Stt>, audio: Vec<f32>) -> Result<String, Fault> {
    off_the_runtime(move || stt.transcribe(&audio)).await?
}

/// Run `work` somewhere that is allowed to block, and turn a lost task into a [`Fault`].
///
/// Separated from [`transcribe`] so the property can be tested without a 141 MB model: a
/// current-thread runtime must still make progress on another task while this is running.
pub async fn off_the_runtime<T, F>(work: F) -> Result<T, Fault>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work).await.map_err(|_| Fault::Lost)
}

/// Tidy one transcript, and refuse to hand on a non-answer dressed as one.
///
/// whisper emits its own annotations for audio it found no speech in — `[BLANK_AUDIO]`,
/// `(silence)`, `[ Music ]` — as ordinary segment text. They are not a transcript, and
/// downstream they become a sentence an agent may act on, so a transcript that is *only*
/// annotation becomes the empty string and the session reports that nobody said anything.
/// Annotation mixed in with real words is left alone: cutting it would need to guess where a
/// sentence ends.
pub fn clean(text: &str) -> String {
    let trimmed = text.trim();
    let stripped = trimmed
        .strip_prefix('[')
        .and_then(|t| t.strip_suffix(']'))
        .or_else(|| trimmed.strip_prefix('(').and_then(|t| t.strip_suffix(')')));
    match stripped {
        Some(inner) if !inner.contains('[') && !inner.contains('(') => String::new(),
        _ => trimmed.split_whitespace().collect::<Vec<_>>().join(" "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seconds(s: f64) -> usize {
        (s * SAMPLE_RATE as f64) as usize
    }

    /// The values every timing in this module was taken at. If either moves, the tables in the
    /// module and on [`MIN_CTX`] are describing something else.
    #[test]
    fn the_context_matches_the_values_that_were_measured() {
        assert_eq!(
            audio_ctx(seconds(3.0)),
            450,
            "three seconds is short enough that the floor decides it; it took 32.77 s at the \
             182 this file used to produce"
        );
        // Exactly eleven seconds is 550 positions and no remainder. `tests/audio/jfk.wav` is
        // 176_017 samples — seventeen past that — so the recording itself lands on 583, which
        // is the number in the module's table.
        assert_eq!(audio_ctx(seconds(11.0)), 582);
        assert_eq!(audio_ctx(176_017), 583, "jfk.wav ran in 1.32 s at 583");
    }

    /// **The invariant the floor exists for**, asserted over the whole range rather than at the
    /// one length somebody happened to measure: whisper `base` starts inventing below about
    /// 215 positions, so nothing this function returns may be near that.
    ///
    /// A test of `MIN_CTX` alone would not do it — the mutation that matters is one that lets
    /// a *particular* length slip under, and only the range says it cannot.
    #[test]
    fn no_utterance_of_any_length_gets_a_context_that_was_measured_to_invent() {
        for tenths in 0..=350 {
            let samples = seconds(tenths as f64 / 10.0);
            assert!(
                audio_ctx(samples) >= 375,
                "{tenths} tenths of a second got {}; 175 answered \"And saw my phone\" in 1.11 s \
                 with nothing to notice, and 300 said a three-second phrase twice",
                audio_ctx(samples)
            );
        }
    }

    /// **The mutation-stopper for the offset half of the rule.** The exact cut is the case
    /// where a three-second clip said its own sentence forty-four times, and the floor above
    /// is what actually prevents it — but a value that fell back to the cut for *long* audio,
    /// where the floor does not apply, would be invisible to every other test here.
    ///
    /// Asserting the value exactly rather than merely "bigger" is deliberate: `+ 1` satisfies
    /// "bigger" and is not what was measured.
    #[test]
    fn the_context_is_never_the_exact_cut() {
        for tenths in 1..=290 {
            let samples = seconds(tenths as f64 / 10.0);
            let cut = exact_cut(samples);
            let used = audio_ctx(samples);
            assert_ne!(
                used, cut,
                "{tenths} tenths of a second: cutting the context at the length of the audio \
                 is the case where three seconds said its own sentence 44 times"
            );
            assert_eq!(
                used,
                cut.max(MIN_CTX - CTX_MARGIN) + CTX_MARGIN,
                "{tenths} tenths of a second: the value is the floor or the cut plus exactly \
                 CTX_MARGIN, and nothing in between"
            );
        }
    }

    /// The context must cover the audio. Under-covering truncates the utterance silently —
    /// whisper decodes what fits and says nothing about the rest.
    #[test]
    fn the_context_always_covers_the_audio_it_is_given() {
        for tenths in 1..=600 {
            let samples = seconds(tenths as f64 / 10.0);
            assert!(
                audio_ctx(samples) >= exact_cut(samples),
                "{tenths} tenths of a second is not covered"
            );
        }
    }

    /// A long turn has to stay *slow*, not become wrong. `earshot` holds a turn open on a
    /// steady tone at speaking pitch, so nothing here may assume an utterance is short.
    #[test]
    fn a_turn_longer_than_the_window_is_clamped_rather_than_overrunning() {
        assert_eq!(audio_ctx(seconds(30.0)), FULL_CTX);
        assert_eq!(audio_ctx(seconds(60.0)), FULL_CTX, "a minute is still 1500");
        assert_eq!(audio_ctx(seconds(3600.0)), FULL_CTX, "an alarm left ringing is still 1500");
        assert_eq!(audio_ctx(usize::MAX), FULL_CTX, "and the arithmetic does not wrap");
    }

    /// The floor is a floor: short audio gets it, and it is not quietly the exact cut again.
    #[test]
    fn everything_short_gets_the_floor_and_not_its_own_length() {
        assert_eq!(audio_ctx(samples_in(MIN_AUDIO)), MIN_CTX);
        assert_eq!(audio_ctx(1), MIN_CTX, "nothing below the floor goes below the floor");
        assert_eq!(audio_ctx(0), MIN_CTX);
        assert_eq!(MIN_AUDIO, Duration::from_millis(100), "whisper.cpp's own delta_min = 10");

        // Where the floor stops deciding: audio long enough that cut + margin passes it.
        let crossover = seconds((MIN_CTX - CTX_MARGIN) as f64 * 0.02);
        assert_eq!(audio_ctx(crossover), MIN_CTX);
        assert!(audio_ctx(crossover + SAMPLES_PER_CTX) > MIN_CTX);
    }

    /// A turn shorter than whisper will look at answers "" rather than being handed over for
    /// whisper to warn about on stderr and return nothing from.
    #[test]
    fn audio_too_short_for_whisper_to_look_at_is_refused_here() {
        assert!(samples_in(MIN_AUDIO) > 0);
        assert_eq!(samples_in(MIN_AUDIO), 1_600, "100 ms of 16 kHz mono");
        assert_eq!(samples_in(Duration::from_secs(1)), SAMPLE_RATE as usize);
    }

    /// [`Settings`] must carry the scaled context and not some other number, or every
    /// assertion about [`audio_ctx`] above is about a function nothing calls.
    #[test]
    fn the_settings_carry_the_scaled_context() {
        for tenths in [1u32, 7, 30, 110, 400] {
            let samples = seconds(tenths as f64 / 10.0);
            assert_eq!(Settings::for_audio(samples).audio_ctx, audio_ctx(samples));
        }
        assert_ne!(
            Settings::for_audio(seconds(3.0)).audio_ctx,
            FULL_CTX,
            "leaving the lever alone is the 3.4 s case"
        );
    }

    /// Whisper is *told* the language. Detecting it cost 10.79 s against 3.44 s on the same
    /// clip, and this program already knows the answer.
    #[test]
    fn the_language_is_configured_and_not_detected() {
        let settings = Settings::for_audio(seconds(3.0));

        assert_eq!(settings.language, Some(LANGUAGE));
        assert!(settings.language.is_some(), "None here is `detect the language`, three times slower");
    }

    /// The two that are quietly dangerous rather than slow.
    #[test]
    fn a_turn_is_decoded_alone_and_in_the_language_it_was_spoken_in() {
        let settings = Settings::for_audio(seconds(3.0));

        assert!(!settings.carry_previous_turn, "a stale prompt is how whisper starts repeating");
        assert!(!settings.translate, "this is speech into the machine, not a translator");
        assert!(settings.threads >= 1);
    }

    /// `state.full()` occupies a thread for a third of a second at best. The runtime it was
    /// called from must keep going — the window, the tray and the websocket are on it.
    ///
    /// **The deadline is the assertion**, as elsewhere in this crate: a `transcribe` that ran
    /// whisper inline would not fail this, it would hang it.
    #[tokio::test]
    async fn transcription_does_not_stop_the_runtime_it_was_called_from() {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let blocked = off_the_runtime(move || {
            // Waits for the other task, which can only run if this is not on the runtime.
            rx.recv().expect("the runtime kept going");
            7u32
        });

        let also = async move {
            tokio::task::yield_now().await;
            tx.send(()).expect("the blocking side is still there");
        };

        let (answer, ()) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            futures_join(blocked, also),
        )
        .await
        .expect("a runtime that stopped is what this catches");

        assert_eq!(answer, Ok(7));
    }

    /// Whisper's own "there was nothing here" is text like any other segment, and downstream
    /// it becomes something an agent may act on.
    #[test]
    fn an_annotation_is_not_a_transcript() {
        assert_eq!(clean(" [BLANK_AUDIO] "), "");
        assert_eq!(clean("(silence)"), "");
        assert_eq!(clean("[ Music ]"), "");
        assert_eq!(
            clean(" and so my fellow  Americans "),
            "and so my fellow Americans",
            "ordinary text keeps its words and loses its padding"
        );
        assert_eq!(
            clean("[BLANK_AUDIO] turn the lights off"),
            "[BLANK_AUDIO] turn the lights off",
            "annotation beside real words is left alone rather than guessed at"
        );
    }

    /// Where the model goes is a decision: the cache, shared by every instance, never the
    /// per-instance data directory and never the temporary directory.
    #[test]
    fn the_model_lives_in_the_shared_cache_and_can_be_pointed_elsewhere() {
        if let Some(dir) = cache_dir() {
            assert!(
                !dir.starts_with(std::env::temp_dir()),
                "a 141 MB download into a directory the system empties is a download per launch"
            );
            assert!(dir.ends_with("models"));
        }

        // The override is what the tests and a person with their own copy both use. Asked of
        // the pure form: `set_var` here would change what the model-gated tests below see, on
        // another thread of the same process.
        assert_eq!(
            model_path_given(&BASE, Some("/somewhere/of/my/own/ggml-small.bin".into())),
            Some(PathBuf::from("/somewhere/of/my/own/ggml-small.bin"))
        );
        assert_eq!(
            model_path_given(&BASE, Some(std::ffi::OsString::new())),
            cache_dir().map(|d| d.join(BASE.file)),
            "an empty override is nobody naming a file, not a file called nothing"
        );
        assert_eq!(model_path_given(&BASE, None), cache_dir().map(|d| d.join(BASE.file)));
    }

    // ---- the model-gated half -------------------------------------------------------------
    //
    // **A 141 MB download is not something `cargo test` does**, so everything that needs real
    // whisper is gated on `ZYRIS_WHISPER_MODEL` naming a `ggml-base.bin` that is already
    // there. What that costs is written down in `CLAUDE.md`: CI never decodes a sample, so a
    // wiring mistake between `audio_ctx` and `set_audio_ctx` would pass there and fail only on
    // a machine with the model. The arithmetic above is covered either way.

    fn base_model() -> Option<PathBuf> {
        let path = PathBuf::from(std::env::var_os(MODEL_ENV)?);
        path.is_file().then_some(path)
    }

    fn jfk() -> Vec<f32> {
        let bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audio/jfk.wav"),
        )
        .expect("tests/audio/jfk.wav is in the repository");
        // 16-bit mono PCM; the header on this file is the canonical 44 bytes.
        bytes[44..]
            .chunks_exact(2)
            .map(|p| i16::from_le_bytes([p[0], p[1]]) as f32 / 32768.0)
            .collect()
    }

    /// The plan's "done when", and everything an undersized context does, in one test.
    ///
    /// **Three transcriptions of the same three seconds**, at the rule, at whisper's default,
    /// and at the exact cut — and every assertion is a *comparison between them* rather than a
    /// number on the clock. Two reasons, both found by writing it the other way first:
    ///
    /// 1. **`cargo test` runs tests in parallel**, so a wall-clock assertion measures how many
    ///    other whisper threads are on this machine's four cores. The same clip that takes
    ///    **0.88 s** with the machine otherwise idle took **5.89 s** inside the suite.
    /// 2. **`whisper-rs-sys` builds whisper.cpp differently in a debug profile** — see
    ///    [`Stt::transcribe`] — so the same assertion would need two constants anyway.
    ///
    /// A ratio survives both: contention and `-DWHISPER_DEBUG` slow every arm together.
    ///
    /// The repetition assertion is the one that would catch a silent regression. Measured
    /// 2026-09-14: at the rule the clip says its sentence **once**; at the exact cut it says it
    /// **forty-four times**.
    #[test]
    fn a_three_second_utterance_costs_a_fraction_of_the_full_window() {
        let Some(model) = base_model() else {
            eprintln!("skipped: set {MODEL_ENV} to a ggml-base.bin to run this");
            return;
        };
        let stt = Stt::load(&model).expect("the model loads");
        let audio: Vec<f32> = jfk().into_iter().take(seconds(3.0)).collect();

        // **Through `Stt::transcribe`, not through `transcribe_at`.** This is the only
        // assertion anywhere that ties `audio_ctx` to the `set_audio_ctx` call: with the
        // forced-context helper on all three arms, replacing that line with `FULL_CTX` passed
        // every test in this file. Found by mutation.
        let started = std::time::Instant::now();
        let scaled = stt.transcribe(&audio).expect("it transcribes");
        let scaled_took = started.elapsed();
        let (full, full_took) = timed(&stt, &audio, FULL_CTX);
        let (cut, cut_took) = timed(&stt, &audio, exact_cut(audio.len()));

        eprintln!(
            "3 s at {}: {scaled_took:?} {scaled:?} / at {FULL_CTX}: {full_took:?} {full:?} / \
             at {}: {cut_took:?} {cut:?}",
            audio_ctx(audio.len()),
            exact_cut(audio.len())
        );

        assert!(
            scaled.to_lowercase().contains("fellow americans"),
            "the scaled context has to transcribe it: {scaled:?}"
        );
        assert_eq!(says_it(&scaled), 1, "and say it once: {scaled:?}");
        assert_eq!(says_it(&full), 1, "as does the full window: {full:?}");
        assert!(
            says_it(&cut) > 5,
            "the exact cut repeats itself; it said the sentence {} times: {cut:?}",
            says_it(&cut)
        );
        assert!(
            scaled_took * 2 < full_took,
            "the whole point is that scaling the context is much cheaper than not; got \
             {scaled_took:?} against {full_took:?} for the full window"
        );
        assert!(
            cut_took > scaled_took,
            "an undersized context is slower as well as wrong; got {cut_took:?} against \
             {scaled_took:?}"
        );
    }

    /// How many times the transcript says the only phrase in these three seconds.
    fn says_it(text: &str) -> usize {
        text.to_lowercase().matches("fellow american").count()
    }

    // ---- helpers --------------------------------------------------------------------------

    fn timed(stt: &Stt, audio: &[f32], ctx: u16) -> (String, std::time::Duration) {
        let started = std::time::Instant::now();
        let text = transcribe_at(stt, audio, ctx);
        (text, started.elapsed())
    }

    /// The same call [`Stt::transcribe`] makes, with the context forced. Only a test wants
    /// this; the module never lets a caller choose a context.
    fn transcribe_at(stt: &Stt, audio: &[f32], ctx: u16) -> String {
        let mut state = stt.context.create_state().expect("state");
        let mut params =
            whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some(LANGUAGE));
        params.set_n_threads(threads() as std::ffi::c_int);
        params.set_audio_ctx(ctx as std::ffi::c_int);
        params.set_no_context(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_no_timestamps(true);
        state.full(params, audio).expect("full");
        let mut text = String::new();
        for segment in state.as_iter() {
            text.push_str(segment.to_str_lossy().expect("utf8").as_ref());
        }
        clean(&text)
    }

    /// Two futures to completion without pulling in `futures`. `tokio::join!` would do, and
    /// this is the same thing spelled so the timeout above can wrap it.
    async fn futures_join<A: std::future::Future, B: std::future::Future>(a: A, b: B) -> (A::Output, B::Output) {
        tokio::join!(a, b)
    }
}
