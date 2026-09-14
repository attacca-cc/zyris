//! The whole path, joined up: a recorded file where the microphone would be, a key held over
//! it, and a `VoiceEvent` out the other end.
//!
//! Every layer below this has its own tests and they are thorough; what none of them touches is
//! the *join*. The audio here goes through the real `capture::Callback` a `cpal` stream is
//! given — the same widening, the same `Conversion`, the same `Chunker` — and then through the
//! real `Session`, which owns the real `Apm` and the real `Endpointer`. Nothing in this file
//! reimplements a stage; the only thing standing in for something is the device, and what
//! stands in for it is a 48 kHz stereo `i16` stream built from a recording, which is the shape
//! `cpal` hands over on Windows and, at this machine's default configuration, on Linux too.
//!
//! # What it covers, and what it cannot
//!
//! **Whisper is gated on `ZYRIS_WHISPER_MODEL`**, for the reason `CLAUDE.md` records: a 141 MB
//! download is not something `cargo test` does, and so **CI never decodes a sample**. That hole
//! is inherited here and it is narrowed rather than accepted: the ungated test asserts on the
//! *audio that reaches the transcriber* — its rate, its length, and that it is the recording
//! that was played and not some other signal — so a resampler, a chunker or an endpointer that
//! stopped working fails on both runners. What only the gated test can say is that the words
//! come back right.
//!
//! What nothing here reaches: opening a real device, which is `cpal`'s own code; the clock,
//! because audio arrives as fast as the loop will take it rather than at 48 kHz; the key, which
//! is a desktop session's business and lives in `zyris-app`; and what the processor does to the
//! audio, which is `apm.rs`'s own test — a mutation making `process_capture` a no-op survives
//! every assertion here, because noise suppression is not visible in the shape of a sentence.

#![cfg(feature = "voice")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{broadcast, mpsc};

use zyris_voice::apm::Apm;
use zyris_voice::capture::{
    APM_FRAME, Callback, Chunker, Conversion, SAMPLE_RATE, VAD_FRAME,
};
use zyris_voice::session::{Session, Transcribe};
use zyris_voice::{Push, VoiceEvent, stt};

/// The device this file pretends to be: 48 kHz, two channels, 16-bit.
///
/// Not 16 kHz mono, and not for convenience: `capture` opens every device at its own format and
/// converts here on **every** platform, so a fixture already at 16 kHz mono would leave the
/// resampler dead in the one test that claims to join everything up. 48 kHz stereo is what this
/// machine's microphone reports and what a Windows one hands over. `i16` rather than `f32`
/// because the widening is generic and Windows is where a mix format that is not `f32` turns
/// up.
const DEVICE_RATE: u32 = 48_000;
const DEVICE_CHANNELS: u16 = 2;

/// The callback lengths this project has actually measured, in a cycle.
///
/// 1024 frames on ALSA and at PipeWire's native format, and 314 / 341 / 342 alternating on one
/// PipeWire stream. A fixed length is the one thing a device does not promise, so the stand-in
/// gives all four in turn.
const CALLBACK_FRAMES: [usize; 4] = [1024, 314, 341, 342];

/// How far the RMS envelopes are allowed to disagree before this stops being that recording.
///
/// Measured 2026-09-15 over `jfk.wav` through the whole path: 0.994 without `aec` and 0.992
/// with it. The threshold is well below both because what it is written to catch is a stage
/// that stopped working, not a tenth of a per cent of noise suppression.
const SAME_RECORDING: f32 = 0.9;

/// How long one point of that envelope is.
const ENVELOPE_MS: usize = 10;

/// What twelve seconds of [`still_holding`] `jfk.wav` comes out of the whole path as.
///
/// Measured 2026-09-15: **178,176 samples without `aec` and 178,432 with it** — one detector
/// frame apart — beginning 130 ms and 120 ms into what was played. The recording's first word
/// is at 336 ms and `vad::MARGIN` is 200 ms, which is where the 130 comes from; the far end is
/// the last word at about 11.0 s plus the same margin, which is why the second of held silence
/// after it does not reach whisper.
///
/// **This is a second copy of a measurement**, which this workspace is otherwise wary of, and
/// here that is the point. A loose assertion — "shorter than what was played, longer than a
/// second less" — was written first, and a mutation pass walked straight through it: no margin,
/// a detector threshold nothing reaches, a turn that starts where the key did and a turn that
/// ends where the key did **all passed**. An end-to-end test that cannot see the silence rule
/// move is a test of the plumbing. If the rule is deliberately changed, this is where it says
/// so; change these with it.
const TURN_SAMPLES: usize = 178_176;

/// 100 ms either way — six detector frames of room for a float that rounded differently on
/// another machine, and a quarter of the least any of those mutations moves it.
const TURN_SLACK: usize = SAMPLE_RATE as usize / 10;

/// How far into what was played the turn may start. Measured at 120-130 ms;
/// the mutations above put it at 310 and 340.
const TURN_STARTS_WITHIN_MS: usize = 200;

// ---------------------------------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------------------------------

/// **The ungated half**: everything but whisper, on both CI runners.
///
/// The double records what it was handed, so the assertions are about the audio that arrived at
/// transcription — which is the only place the resampler, the two chunkers, the processor and
/// the endpointer can all be wrong at once.
#[tokio::test]
async fn a_recording_held_under_the_key_reaches_whisper_as_the_sentence_that_was_said() {
    let played = still_holding(&jfk());
    let bench = Arc::new(Bench::new("a transcript nobody read"));
    let events =
        hold_the_key_over(&as_a_device_hands_it_over(&played, Wiring::Both), bench.clone()).await;

    assert_eq!(
        events,
        vec![
            VoiceEvent::Listening,
            VoiceEvent::Thinking,
            VoiceEvent::Heard { text: "a transcript nobody read".to_string() },
        ],
        "a held key over a sentence is Listening, then Thinking, then what was said"
    );

    let mut turns = bench.taken();
    assert_eq!(turns.len(), 1, "one hold is one utterance");
    let audio = turns.remove(0);

    assert_eq!(
        audio.len() % VAD_FRAME,
        0,
        "the session hands whisper whole detector frames; got {} samples",
        audio.len()
    );

    // The rate and the silence rule, both said as a length: twelve seconds of a device cannot
    // arrive as more than twelve seconds of 16 kHz audio — a conversion that stopped converting
    // would arrive as three times it — and what is left after the trimming is a number the rule
    // decides. See `TURN_SAMPLES` for why that number is written down rather than bounded
    // loosely.
    let seconds = audio.len() as f32 / SAMPLE_RATE as f32;
    let whole = played.len() as f32 / SAMPLE_RATE as f32;

    // And the content. A ten-millisecond RMS envelope is what survives both feature
    // states — `aec`'s noise suppression changes the samples and leaves the shape of the
    // sentence alone — and it is enough to say this is *that* recording, at that rate, starting
    // where the endpointer said it started.
    let (lag, fit) = best_alignment(&envelope(&played), &envelope(&audio));
    eprintln!(
        "{} samples reached whisper: {seconds:.2} s of a {whole:.2} s recording, best envelope \
         fit {fit:.3} at {} ms in",
        audio.len(),
        lag * ENVELOPE_MS
    );
    assert!(
        fit > SAME_RECORDING,
        "the audio that reached whisper has to be the recording that was played: the best \
         envelope correlation was {fit:.3}, at {} ms in",
        lag * ENVELOPE_MS
    );
    assert!(
        audio.len().abs_diff(TURN_SAMPLES) <= TURN_SLACK,
        "the silence rule leaves {TURN_SAMPLES} samples of this recording; it left {}. Either a \
         stage of the path has stopped working, or the rule was changed and this number has to \
         be changed with it",
        audio.len()
    );
    assert!(
        lag * ENVELOPE_MS <= TURN_STARTS_WITHIN_MS,
        "and the turn starts where the sentence does, one margin before the first word: best \
         match at {} ms in",
        lag * ENVELOPE_MS
    );
}

/// The other half of the same claim: a key held over a quiet room is **not** an utterance.
///
/// Without this, the test above passes for a path that transcribes whatever it is given,
/// including nothing — and "whisper invents fluent sentences out of noise" is the reason that
/// matters more here than the usual.
#[tokio::test]
async fn a_key_held_over_a_quiet_room_reaches_whisper_not_at_all() {
    let silence = vec![0.0f32; SAMPLE_RATE as usize * 2];
    let bench = Arc::new(Bench::new("nobody should ever see this"));
    let events =
        hold_the_key_over(&as_a_device_hands_it_over(&silence, Wiring::Both), bench.clone())
            .await;

    assert_eq!(
        events,
        vec![VoiceEvent::Listening, VoiceEvent::HeardNothing],
        "two seconds of silence is a turn that ends with nothing in it"
    );
    assert!(bench.taken().is_empty(), "and whisper is never asked");
}

/// A device whose microphone is on the right channel is still a person talking.
///
/// `downmix_into` averages the channels rather than taking the first, and its own documentation
/// names this case — but the average and the first channel are the same thing for a stand-in
/// that puts one signal on both, so the mutation taking `frame[0]` survived the test above.
/// Here it is the difference between a sentence and silence.
#[tokio::test]
async fn a_microphone_wired_to_the_right_channel_is_still_heard() {
    let played = still_holding(&jfk());
    let bench = Arc::new(Bench::new("a transcript nobody read"));
    let events = hold_the_key_over(
        &as_a_device_hands_it_over(&played, Wiring::RightOnly),
        bench.clone(),
    )
    .await;

    assert_eq!(
        events.last(),
        Some(&VoiceEvent::Heard { text: "a transcript nobody read".to_string() }),
        "half the channels is half as loud and is not half a sentence"
    );
    let audio = bench.taken().pop().expect("the turn reached the transcriber");
    let (_, fit) = best_alignment(&envelope(&played), &envelope(&audio));
    assert!(
        fit > SAME_RECORDING,
        "and it is the same recording, quieter: envelope correlation {fit:.3}"
    );
}

/// **The gated half**: the same path with the real model behind it, asserting on the words.
///
/// Skipped unless `ZYRIS_WHISPER_MODEL` names a `ggml-base.bin` this machine already has — see
/// the module documentation, and `stt.rs`, which makes the same trade for the same reason.
#[tokio::test]
async fn a_recording_held_under_the_key_comes_back_as_what_was_said() {
    let Some(model) = base_model() else {
        eprintln!("skipped: set {} to a ggml-base.bin to run this", stt::MODEL_ENV);
        return;
    };
    let recording = jfk();
    let stt = Arc::new(stt::Stt::load(&model).expect("the model loads"));

    let started = std::time::Instant::now();
    let played = still_holding(&recording);
    let events =
        hold_the_key_over(&as_a_device_hands_it_over(&played, Wiring::Both), stt).await;
    let took = started.elapsed();

    let text = match events.last() {
        Some(VoiceEvent::Heard { text }) => text.clone(),
        other => panic!("the turn had to end in a transcript; it ended in {other:?}"),
    };
    eprintln!(
        "{:.2} s of audio, key down to text in {took:?}: {text:?}",
        recording.len() as f32 / SAMPLE_RATE as f32
    );
    assert_eq!(
        events[..2],
        [VoiceEvent::Listening, VoiceEvent::Thinking],
        "and it got there through the states a window renders"
    );
    let said = text.to_lowercase();
    assert!(
        said.contains("fellow americans"),
        "the sentence in this recording has to survive the whole path: {text:?}"
    );
    assert!(said.contains("country"), "all of it, not the first words of it: {text:?}");
}

/// **The gated half again, over a turn the length a conversation actually has.**
///
/// Three seconds is what somebody says into a held key, and it is where `stt::audio_ctx` scales
/// whisper's context down rather than leaving it at the full window. What too small a context
/// does is not "less accurate" — it is **the same sentence over and over** — so the assertion is
/// that it is said once.
///
/// Two things this was written believing, and both were measured afterwards rather than assumed:
///
/// - **It does not catch a missing floor.** `stt::MIN_CTX` set to zero leaves this clip at a
///   context of 192 and it still comes back right, once, and quickly. The floor is a latency
///   guard with whisper's own temperature fallback behind it, and what keeps it is `stt.rs`'s
///   gated test, which compares three contexts against each other rather than reading one.
/// - **It does catch the language being wrong, and the eleven-second test does not.** `LANGUAGE`
///   mutated to `"ko"` leaves the long recording's sentence standing and takes this short one
///   apart — and the file went from 7 s to 220 s doing it, which is the other half of what a
///   wrong language costs.
#[tokio::test]
async fn three_seconds_under_the_key_comes_back_once_rather_than_forty_times() {
    let Some(model) = base_model() else {
        eprintln!("skipped: set {} to a ggml-base.bin to run this", stt::MODEL_ENV);
        return;
    };
    let recording = jfk();
    let three_seconds = &recording[..SAMPLE_RATE as usize * 3];
    let stt = Arc::new(stt::Stt::load(&model).expect("the model loads"));

    let started = std::time::Instant::now();
    let played = still_holding(three_seconds);
    let events =
        hold_the_key_over(&as_a_device_hands_it_over(&played, Wiring::Both), stt).await;
    let took = started.elapsed();

    let text = match events.last() {
        Some(VoiceEvent::Heard { text }) => text.clone(),
        other => panic!("the turn had to end in a transcript; it ended in {other:?}"),
    };
    eprintln!("3 s of audio, key down to text in {took:?}: {text:?}");
    let times = text.to_lowercase().matches("fellow american").count();
    assert_eq!(times, 1, "the only phrase in these three seconds, {times} times: {text:?}");
}

// ---------------------------------------------------------------------------------------------
// The harness
// ---------------------------------------------------------------------------------------------

/// Press, play the whole recording through the real callback, release, and collect the events.
///
/// **The press is waited for.** Audio already in the channel when a key goes down belongs to
/// nobody — that is `Session::drain`'s rule and it is deliberate — so a harness that fed audio
/// before the session had read the press would be measuring that rule rather than this path.
/// `VoiceEvent::Listening` is what says the press has been read.
async fn hold_the_key_over(device: &[i16], heard: Arc<dyn Transcribe>) -> Vec<VoiceEvent> {
    let (mic, from_mic) = mpsc::unbounded_channel();
    let (keys, from_keys) = broadcast::channel(8);
    let (events, mut from_session) = broadcast::channel(64);

    let apm = Arc::new(Apm::new().expect("the processor is built"));
    let session = Session::new(from_mic, from_keys, apm, heard, events);
    let running = tokio::spawn(session.run());

    keys.send(Push::Pressed).expect("the session is listening for keys");
    let mut seen = vec![next(&mut from_session).await];

    // The device, playing the file. `Callback` is exactly what `cpal` is handed.
    let mut callback = Callback::new(
        Conversion::new(DEVICE_RATE, DEVICE_CHANNELS).expect("48 kHz stereo converts"),
        Chunker::new(APM_FRAME),
    );
    let mut at = 0;
    for frames in CALLBACK_FRAMES.iter().cycle() {
        if at >= device.len() {
            break;
        }
        let end = (at + frames * usize::from(DEVICE_CHANNELS)).min(device.len());
        callback.deliver(&device[at..end], &mic);
        at = end;
    }

    keys.send(Push::Released).expect("the session is listening for keys");
    loop {
        let event = next(&mut from_session).await;
        let ends_the_turn = matches!(
            event,
            VoiceEvent::Heard { .. } | VoiceEvent::HeardNothing | VoiceEvent::Failed { .. }
        );
        seen.push(event);
        if ends_the_turn {
            break;
        }
    }

    // Dropping the key sender is `Stopped::KeyGone`, which is how the loop ends.
    drop(keys);
    drop(mic);
    let stopped = tokio::time::timeout(Duration::from_secs(10), running).await;
    assert!(stopped.is_ok(), "the session has to stop when the key stream goes away");
    seen
}

/// One event, or a test that says what it was waiting for rather than hanging.
///
/// Generous on purpose: a debug build of whisper.cpp carries `-DWHISPER_DEBUG` and is 88 times
/// slower than a release one, so the gated test spends minutes where the product spends a
/// second. Nothing here asserts on the clock for the same reason.
async fn next(events: &mut broadcast::Receiver<VoiceEvent>) -> VoiceEvent {
    tokio::time::timeout(Duration::from_secs(600), events.recv())
        .await
        .expect("the session has to publish something")
        .expect("the event channel stays open while the session runs")
}

/// A transcriber that keeps what it was handed.
struct Bench {
    answer: String,
    seen: Mutex<Vec<Vec<f32>>>,
}

impl Bench {
    fn new(answer: &str) -> Bench {
        Bench { answer: answer.to_string(), seen: Mutex::new(Vec::new()) }
    }

    /// Every utterance this was asked to transcribe, in order.
    fn taken(&self) -> Vec<Vec<f32>> {
        self.seen.lock().expect("nothing panics while holding this").clone()
    }
}

impl Transcribe for Bench {
    fn transcribe(&self, audio: &[f32]) -> Result<String, stt::Fault> {
        self.seen.lock().expect("nothing panics while holding this").push(audio.to_vec());
        Ok(self.answer.clone())
    }
}

// ---------------------------------------------------------------------------------------------
// The device, and the recording it plays
// ---------------------------------------------------------------------------------------------

/// The recording, and a second of somebody who has not let go of the key yet.
///
/// Nobody releases on the last syllable, and without that second **the fixture cannot tell a
/// trailing trim from no trailing trim**: `jfk.wav` ends 16 ms after its last word, which is
/// inside `vad::MARGIN`, so "the turn ends one margin after the last word" and "the turn ends
/// where the key did" are the same answer. A mutation doing the second survived until this was
/// here. It is also what `MARGIN` is *for* — `stt::audio_ctx` scales with the length of a turn,
/// so a second of silence carried into whisper is a tenth of the work for nothing.
fn still_holding(recording: &[f32]) -> Vec<f32> {
    let mut played = recording.to_vec();
    played.resize(played.len() + SAMPLE_RATE as usize, 0.0);
    played
}

/// Which of the stand-in device's two channels the person is on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Wiring {
    /// One microphone in a stereo stream: the same signal on both channels.
    Both,
    /// A device whose microphone is on the right, which is the case `downmix_into` exists for.
    RightOnly,
}

/// 16 kHz mono as a device would hand it over: 48 kHz, two channels, interleaved, 16-bit.
///
/// Linear interpolation up to 48 kHz. It is not the resampler under test and it does not have
/// to be good; what it must not do is leave the rate alone.
fn as_a_device_hands_it_over(mono: &[f32], wiring: Wiring) -> Vec<i16> {
    let up = (DEVICE_RATE / SAMPLE_RATE) as usize;
    let mut out = Vec::with_capacity(mono.len() * up * usize::from(DEVICE_CHANNELS));
    for j in 0..mono.len() * up {
        let at = j as f32 / up as f32;
        let low = at.floor() as usize;
        let frac = at - low as f32;
        let a = mono.get(low).copied().unwrap_or(0.0);
        let b = mono.get(low + 1).copied().unwrap_or(a);
        let quantised = ((a + (b - a) * frac).clamp(-1.0, 1.0) * 32767.0) as i16;
        match wiring {
            Wiring::Both => out.extend_from_slice(&[quantised, quantised]),
            Wiring::RightOnly => out.extend_from_slice(&[0, quantised]),
        }
    }
    out
}

/// The sentence every claim in this workspace about the silence rule is measured against.
///
/// `tests/audio/jfk.wav`: 11.00 s, 16 kHz, mono, 16-bit. Read with the same short RIFF reader
/// `vad.rs` uses rather than with a `hound` dev-dependency, on a crate whose feature layout
/// exists to keep the dependency graph small — and a dev-dependency here is the exact thing
/// `nothing_turns_the_feature_on_by_itself.rs` is written to prevent.
fn jfk() -> Vec<f32> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audio/jfk.wav");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(&bytes[0..4], b"RIFF", "{} is not a RIFF file", path.display());
    assert_eq!(&bytes[8..12], b"WAVE", "{} is not a WAVE file", path.display());

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
                        f32::from(i16::from_le_bytes(s.try_into().expect("2 bytes"))) / 32768.0
                    })
                    .collect()
            }
            _ => {}
        }
        at += 8 + size + (size & 1);
    }
    assert_eq!(
        (rate, channels, bits),
        (SAMPLE_RATE, 1, 16),
        "{} is not 16 kHz mono 16-bit",
        path.display()
    );
    samples
}

/// A `ggml-base.bin` this machine already has, if `ZYRIS_WHISPER_MODEL` names one.
fn base_model() -> Option<std::path::PathBuf> {
    let path = std::path::PathBuf::from(std::env::var_os(stt::MODEL_ENV)?);
    path.is_file().then_some(path)
}

// ---------------------------------------------------------------------------------------------
// Is this that recording?
// ---------------------------------------------------------------------------------------------

/// One RMS point per [`ENVELOPE_MS`].
///
/// Ten milliseconds and not a hundred, which is what this was written with first: the turn
/// starts 136 ms into the recording, so hundred-millisecond windows straddle the sentence's own
/// and the correlation falls to 0.900 for no reason but the grid. At ten it is 0.994.
fn envelope(samples: &[f32]) -> Vec<f32> {
    let window = SAMPLE_RATE as usize / 1000 * ENVELOPE_MS;
    samples
        .chunks(window)
        .map(|w| (w.iter().map(|s| s * s).sum::<f32>() / w.len() as f32).sqrt())
        .collect()
}

/// Where `part` sits inside `whole`, and how well it fits there.
///
/// **Only the lags at which the whole of `part` still overlaps**, which is not a detail: a lag
/// near the end of `whole` leaves two points against two, and two points correlate perfectly
/// with anything. Written the other way first, and it answered 1.000 at 10.8 s into an 11 s
/// recording — a pass, on an assertion that was measuring nothing.
fn best_alignment(whole: &[f32], part: &[f32]) -> (usize, f32) {
    let mut best = (0, f32::MIN);
    for lag in 0..=whole.len().saturating_sub(part.len()) {
        let fit = pearson(&whole[lag..], part);
        if fit > best.1 {
            best = (lag, fit);
        }
    }
    best
}

/// Pearson correlation over the overlap of two series. Scale-free, which is what makes it
/// readable in a build whose noise suppression has moved every sample.
fn pearson(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    if n < 2 {
        return f32::MIN;
    }
    let (a, b) = (&a[..n], &b[..n]);
    let mean = |xs: &[f32]| xs.iter().sum::<f32>() / n as f32;
    let (ma, mb) = (mean(a), mean(b));
    let (mut top, mut la, mut lb) = (0.0, 0.0, 0.0);
    for i in 0..n {
        let (x, y) = (a[i] - ma, b[i] - mb);
        top += x * y;
        la += x * x;
        lb += y * y;
    }
    if la == 0.0 || lb == 0.0 { 0.0 } else { top / (la * lb).sqrt() }
}
