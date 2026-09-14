//! A microphone, at the one rate everything downstream wants: 16 kHz mono `f32`, in chunks of
//! exactly the length the next stage demands.
//!
//! # Why this module is `pub` at all
//!
//! The crate exposes one thing *outward* — a stream of [`VoiceEvent`](crate::VoiceEvent) — and
//! this is not a second one. It is the same accommodation [`crate::Voice::describe`] makes: a
//! window has to be able to list the microphones a person can pick between and say which one is
//! in use, and an event stream cannot say it. Nothing here publishes a `VoiceEvent`; task 6's
//! session does that, and it is the only caller of [`Capture::open`].
//!
//! # The device list is not a microphone list
//!
//! `cpal`'s `pipewire` feature is on, and the reason is written out in the root `Cargo.toml`:
//! the default ALSA backend enumerates seven input devices for the one microphone on this
//! machine, one of them `null`, whose own description reads "generate zero samples (capture)"
//! and which reports `supports_input() == true` with a perfectly ordinary
//! `default_input_config()`. **Someone who picks it gets silence forever and no error.**
//!
//! PipeWire removes that one and replaces it with a different trap, which is why
//! [`InputDevice::direction`] exists. Measured here on 2026-09-15, one microphone, PipeWire:
//!
//! ```text
//! default_sink                      Duplex   <- the speakers' monitor: records what is played
//! default_input                     Input    <- the default, and the right answer
//! Built-in Audio Analog Stereo      Duplex   <- the same speakers, by name
//! Built-in Audio Analog Stereo      Input    <- the same microphone, by name
//! ```
//!
//! Two of the four record the loudspeakers rather than the person, and the two pairs are
//! *spelled identically*. So a list that carried names alone would offer a person two entries
//! called "Built-in Audio Analog Stereo", one of which cannot hear them. The direction travels
//! with every entry, and the entries are ordered so the default and the true inputs come first.
//!
//! **Nothing is filtered out**, deliberately. ALSA's own `default` device reports
//! `DeviceDirection::Unknown` — `cpal`'s comment says it cannot know without opening it — so a
//! filter on `== Input` would hide the one device that always works. A monitor is also a
//! legitimate choice for somebody who wants to transcribe a meeting. The list says what each
//! one is and lets a person decide.
//!
//! # The conversion is ours, on every platform, always
//!
//! The stream is opened at the device's **own** default configuration and converted here. Not
//! at 16 kHz mono, which is what Linux would happily give us: `cpal`'s WASAPI backend puts
//! `AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM` on output streams only — read in
//! `host/wasapi/device.rs`, whose own comment says "Capture streams do not; only native formats
//! will work" — so a Windows microphone hands over its mix format, usually 48 kHz stereo `f32`,
//! and nothing converts it.
//!
//! Asking Linux for 16 kHz mono and Windows for whatever it has would give the two platforms
//! different code paths, and the one that is never run locally is the one that would rot. So
//! both take the same path and [`Conversion`] always resamples and always downmixes — on this
//! machine that is a live 48 kHz stereo conversion, exercised by every local test run. The cost
//! is 21.05 µs per 1024-frame stereo block, downmix included, measured in release here: 0.1% of
//! one core at the 47 blocks a second such a callback delivers.
//!
//! That also makes the conversion a pure function over buffers, so the Windows *shape* is
//! tested here without a Windows machine and without a microphone.
//!
//! # Absent is not broken
//!
//! **"No input device" is `DeviceNotAvailable`, never `default_input_device().is_none()`** —
//! with ALSA configured to nothing that still answers `Some("Default Audio Device")`. The
//! honest signal comes from `default_input_config()` or `build_input_stream()`, and
//! [`read_support`] is where it becomes a [`VoiceSupport`]. Same rule as `zyris-tools`'
//! `announce.rs` applies to `input` and `screen_capture`, and for the same reason: nobody can
//! tell a tool that fails every call from a working one.
//!
//! ALSA writes its own diagnostics straight to this process's stderr, outside anything here can
//! control. A run with no sound card prints lines nothing in this crate produced.

use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::FromSample;
use rubato::{Fft, FixedSync, Resampler};
// The same `tokio::sync` the event bus and the hotkey already use, and the reason the channel
// here is not `std::sync::mpsc`: task 6 reads this from an async task, and a blocking receiver
// there would have to be wrapped in `spawn_blocking` to keep the runtime from stalling.
// `UnboundedSender::send` is not async and does not block, so an audio callback may call it.
use tokio::sync::mpsc;

use crate::VoiceSupport;

/// The rate everything after this module is written for: whisper's input, the APM's, earshot's.
pub const SAMPLE_RATE: u32 = 16_000;

/// 10 ms at [`SAMPLE_RATE`], and the only frame length `webrtc-audio-processing` accepts.
///
/// It **panics** rather than returning `Err` on any other length, which is what makes
/// [`Chunker`] load-bearing rather than a convenience.
pub const APM_FRAME: usize = 160;

/// The frame length `earshot` wants. A wrong one scores `-1.0` there rather than panicking, but
/// a score of "-1" fed into a silence rule is a decision made on nonsense.
pub const VAD_FRAME: usize = 256;

/// How long to wait for a backend to bring a stream up before giving up on it.
///
/// `cpal` documents `None` as "wait indefinitely" and says not every backend honours a value.
/// A bound is still worth asking for: this project has already been bitten once by a startup
/// with no deadline on it (`rmcp`'s handshake, `CLAUDE.md`), and a microphone that never opens
/// must not be a window that never appears.
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

/// Windows' microphone privacy page.
///
/// An unpackaged Win32 application is **not** gated individually, so what a person has to change
/// is the device-wide switch or "let desktop apps access your microphone". Microsoft is testing
/// per-application toggles in Insider builds, so expect this to need revisiting.
pub const MICROPHONE_PRIVACY: &str = "ms-settings:privacy-microphone";

// ---------------------------------------------------------------------------------------------
// Chunks
// ---------------------------------------------------------------------------------------------

/// Cuts a stream of arbitrary-length buffers into chunks of exactly one length.
///
/// **This is what stands between a backend change and a crash.** Observed callback lengths on
/// this project's two backends: 1024 frames on ALSA and at PipeWire's native 48 kHz stereo,
/// 628 at PipeWire stereo, and **314, 341 and 342 alternating** on one PipeWire stream at
/// 16 kHz mono. None of them is a multiple of [`APM_FRAME`], only one is a multiple of
/// [`VAD_FRAME`], and the last of them is not even constant from one call to the next. The APM
/// panics rather than erroring when it is handed the wrong count, so no stage downstream may
/// ever see a buffer of the length a device happened to choose.
///
/// Reused twice: once at the end of the pipeline, to hand the caller the size it asked for, and
/// once *inside* [`Conversion`], because `rubato`'s FFT resampler also wants a fixed number of
/// frames per call and a callback is not it.
pub struct Chunker {
    frames: usize,
    /// The tail of the last call: fewer than `frames` samples, waiting for the rest.
    held: Vec<f32>,
    /// Whole chunks assembled by the current call. Cleared at the top of every [`Self::push`],
    /// which is what lets `push` hand out slices rather than allocate a `Vec` per chunk in an
    /// audio callback.
    ready: Vec<f32>,
}

impl Chunker {
    /// Panics on a chunk of nothing, which is a programming error rather than a condition: every
    /// caller's size is a constant in this crate.
    pub fn new(frames: usize) -> Chunker {
        assert!(frames > 0, "a chunk of no samples is not a chunk");
        Chunker { frames, held: Vec::new(), ready: Vec::new() }
    }

    /// Add a buffer; get back every whole chunk that is now complete, in order.
    ///
    /// Whatever does not fill a chunk is kept for the next call. Nothing is dropped, nothing is
    /// padded, and every slice handed out is exactly [`Self::frames`] long.
    pub fn push(&mut self, samples: &[f32]) -> std::slice::Chunks<'_, f32> {
        let frames = self.frames;
        self.ready.clear();

        let mut rest = samples;
        if !self.held.is_empty() {
            let wanted = (frames - self.held.len()).min(rest.len());
            self.held.extend_from_slice(&rest[..wanted]);
            rest = &rest[wanted..];
            if self.held.len() == frames {
                self.ready.extend_from_slice(&self.held);
                self.held.clear();
            }
        }

        let whole = rest.len() / frames * frames;
        self.ready.extend_from_slice(&rest[..whole]);
        self.held.extend_from_slice(&rest[whole..]);

        self.ready.chunks(frames)
    }

    /// The length of every chunk this hands out.
    pub fn frames(&self) -> usize {
        self.frames
    }
}

/// Average the channels of one interleaved buffer, appending the mono result to `out`.
///
/// An average rather than the first channel: a headset wired out of phase, or a device whose
/// microphone is on the right, would otherwise be silent or half as loud. A frame the device did
/// not finish — a buffer length that does not divide by the channel count — is dropped rather
/// than completed with a sample nobody recorded.
pub fn downmix_into(interleaved: &[f32], channels: u16, out: &mut Vec<f32>) {
    let channels = usize::from(channels.max(1));
    let scale = 1.0 / channels as f32;
    out.extend(
        interleaved.chunks_exact(channels).map(|frame| frame.iter().sum::<f32>() * scale),
    );
}

// ---------------------------------------------------------------------------------------------
// Problems
// ---------------------------------------------------------------------------------------------

/// What a caller is supposed to do about something the audio device reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Recovery {
    /// Nothing. The stream is still running and still delivering audio.
    Continue,
    /// Build the stream again: the device it was on is gone, or the configuration it was built
    /// with no longer describes anything.
    Rebuild,
    /// Somebody else has it, or the machine is out of something. Worth trying again after a
    /// delay; not worth trying again immediately.
    Retry,
    /// Nothing this process can do. A person has to act, and [`DeviceProblem::reason`] is what
    /// to tell them.
    Stop,
}

/// Something the audio device said, in terms both a caller and a person can act on.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceProblem {
    pub recovery: Recovery,
    /// A sentence for the window, not an error code. Carries the backend's own words when it
    /// had any, because "the audio backend returned an unclassified error" on its own tells
    /// nobody anything.
    pub reason: String,
    /// A settings page that would fix it, when there is one. Only Windows has one here.
    pub settings: Option<String>,
}

impl DeviceProblem {
    fn new(recovery: Recovery, reason: impl Into<String>) -> DeviceProblem {
        DeviceProblem { recovery, reason: reason.into(), settings: None }
    }

    fn pointing_at(mut self, settings: &str) -> DeviceProblem {
        self.settings = Some(settings.to_owned());
        self
    }
}

/// Whether a message is Windows refusing the microphone.
///
/// **`cpal` does not classify this one**, and that was read rather than assumed:
/// `host/wasapi/mod.rs`'s `From<windows::core::Error> for Error` maps eight `AUDCLNT_E_*`
/// HRESULTs and `E_ACCESSDENIED` (`0x80070005`) is not among them, so a denied microphone
/// arrives as [`cpal::ErrorKind::BackendError`] carrying
/// `std::io::Error::from_raw_os_error(0x80070005 as i32).to_string()`. The message is the only
/// thing that distinguishes it from a genuinely unclassifiable backend fault.
///
/// Matched on the words rather than on the number, because the number in that string is
/// `-2147024891` (the HRESULT reinterpreted as `i32`, which is what `io::Error` is handed) and
/// not the `0x80070005` every document writes. Both spellings are accepted so a future `cpal`
/// that formats it the other way does not silently stop being recognised. **Not observed on a
/// Windows machine — there is none here — so this is reasoned from the source of `cpal`,
/// `windows-result` and `std`.**
fn looks_like_access_denied(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("access is denied")
        || message.contains("0x80070005")
        || message.contains("-2147024891")
}

/// Turn a `cpal` error into what to do and what to say.
///
/// Every variant is named, including the ones that are not failures at all, because
/// [`cpal::ErrorKind`] is `#[non_exhaustive]` and the catch-all has to be the *conservative*
/// answer rather than a place new kinds go to be ignored.
pub fn classify(error: &cpal::Error) -> DeviceProblem {
    use cpal::ErrorKind::*;

    let detail = error.message().unwrap_or_default();
    let say = |sentence: &str| {
        if detail.is_empty() || detail == sentence {
            sentence.to_owned()
        } else {
            format!("{sentence} ({detail})")
        }
    };

    // Read before the kinds, because the kind for this one is `BackendError`.
    if looks_like_access_denied(detail) {
        return DeviceProblem::new(
            Recovery::Stop,
            say("Windows is not letting Zyris use the microphone"),
        )
        .pointing_at(MICROPHONE_PRIVACY);
    }

    match error.kind() {
        // Not a failure. `cpal` reports it only for a stream built on the *default* device, and
        // reports that the stream was rerouted and is still running — so rebuilding would drop
        // audio for no reason. A stream built on a named device never gets this; it gets
        // `DeviceNotAvailable` and has to be rebuilt, which is why `Capture::follows_default`
        // exists.
        DeviceChanged => DeviceProblem::new(
            Recovery::Continue,
            say("the system switched to a different microphone and recording carried on there"),
        ),
        // A fraction of a second of audio was lost. Rebuilding would lose more.
        Xrun => DeviceProblem::new(
            Recovery::Continue,
            say("the computer could not keep up and a fraction of a second was lost"),
        ),
        // The stream runs, just without the scheduling priority it asked for.
        RealtimeDenied => DeviceProblem::new(
            Recovery::Continue,
            say("the audio thread did not get real-time priority, so speech may stutter under load"),
        ),

        DeviceNotAvailable => DeviceProblem::new(
            Recovery::Rebuild,
            say("the microphone is not there any more"),
        ),
        StreamInvalidated => DeviceProblem::new(
            Recovery::Rebuild,
            say("the microphone's settings changed underneath us"),
        ),

        DeviceBusy => DeviceProblem::new(
            Recovery::Retry,
            say("another program is using the microphone"),
        ),
        ResourceExhausted => DeviceProblem::new(
            Recovery::Retry,
            say("this computer ran out of something the audio system needed"),
        ),

        PermissionDenied => {
            let problem = DeviceProblem::new(
                Recovery::Stop,
                say(if cfg!(target_os = "windows") {
                    "this computer is not letting Zyris use the microphone"
                } else {
                    "this computer is not letting Zyris use the microphone; on Linux this is \
                     usually membership of the `audio` group"
                }),
            );
            if cfg!(target_os = "windows") {
                problem.pointing_at(MICROPHONE_PRIVACY)
            } else {
                problem
            }
        }
        HostUnavailable => DeviceProblem::new(
            Recovery::Stop,
            say("the sound server is not running, so no microphone can be reached"),
        ),
        UnsupportedConfig => DeviceProblem::new(
            Recovery::Stop,
            say("this microphone does not offer a format Zyris can read"),
        ),
        UnsupportedOperation => DeviceProblem::new(
            Recovery::Stop,
            say("this device cannot record"),
        ),
        InvalidInput => DeviceProblem::new(
            Recovery::Stop,
            say("Zyris asked the sound system for something it will not accept"),
        ),
        // `Other` is documented as permanent, and `BackendError` carries whatever the platform
        // said. A new `#[non_exhaustive]` variant lands here too, which is the safe end: it
        // stops rather than retrying something nobody has read yet.
        _ => DeviceProblem::new(Recovery::Stop, say("the sound system reported a problem")),
    }
}

/// Whether this machine can be listened to, read off a probe of the device rather than off a
/// list of devices.
///
/// The probe is `default_input_config()` — or `build_input_stream()`, which fails the same way.
/// See this module's documentation for why `default_input_device().is_none()` is not the
/// question.
pub fn read_support(probe: Result<cpal::SupportedStreamConfig, cpal::Error>) -> VoiceSupport {
    match probe {
        Ok(_) => VoiceSupport::Ready,
        Err(error) => VoiceSupport::Unavailable { reason: classify(&error).reason },
    }
}

/// Ask this machine whether it has a microphone that answers.
///
/// Opens nothing and keeps nothing. Cheap enough for a window to call, and the answer it gives
/// is the one [`crate::Voice::describe`] will carry once task 6 wires a session together.
pub fn support() -> VoiceSupport {
    match cpal::default_host().default_input_device() {
        // The honest half: a device exists, so ask it something.
        Some(device) => read_support(device.default_input_config()),
        // And the other half, which is real on a machine with no sound card at all.
        None => VoiceSupport::Unavailable {
            reason: "this computer has no microphone the sound system can see".to_owned(),
        },
    }
}

// ---------------------------------------------------------------------------------------------
// Conversion
// ---------------------------------------------------------------------------------------------

/// The resampler, and the staging a fixed-size FFT needs in front of it.
struct Resample {
    fft: Fft<f32>,
    /// Callback lengths are not chunk lengths. `rubato` wants exactly `staging.frames()` per
    /// call, so the same [`Chunker`] that serves the pipeline serves the resampler.
    staging: Chunker,
    scratch: Vec<f32>,
    /// `output_frames_next()`, read once. Constant because of [`FixedSync::Both`] — see
    /// [`Conversion::feed`].
    produced: usize,
}

/// Downmix to mono and resample to [`SAMPLE_RATE`], whatever the device hands over.
///
/// A pure function over buffers with a little state: no device, no thread, no clock. That is
/// deliberate, and it is the only way the Windows path gets tested — see the module
/// documentation.
pub struct Conversion {
    channels: u16,
    source_rate: u32,
    mono: Vec<f32>,
    out: Vec<f32>,
    /// `None` when the device is already at [`SAMPLE_RATE`]. The downmix still runs.
    resampler: Option<Resample>,
}

impl Conversion {
    /// Build a conversion from a device's own format to 16 kHz mono.
    ///
    /// Fails only on a format `rubato` will not resample — a rate of zero, in practice. The
    /// failure is a [`DeviceProblem`] rather than a type of its own because everything that can
    /// go wrong between a person and a microphone should arrive at the window in one shape.
    pub fn new(source_rate: u32, channels: u16) -> Result<Conversion, DeviceProblem> {
        let resampler = if source_rate == SAMPLE_RATE {
            None
        } else {
            // About 20 ms of input per FFT chunk. `FixedSync::Both` treats it as a reference and
            // rounds to a multiple of the minimum block size for this pair of rates, so the
            // authority on the real sizes is `input_frames_next()`, never this number.
            let reference = (source_rate as usize / 50).max(1);
            let fft = Fft::<f32>::new(
                source_rate as usize,
                SAMPLE_RATE as usize,
                reference,
                1,
                // Both sides fixed. It is the only mode in which the chunk sizes never move,
                // which is what lets `feed` treat a size mismatch as impossible rather than as a
                // case — see the `expect` there and the test that pins it.
                FixedSync::Both,
            )
            .map_err(|error| {
                DeviceProblem::new(
                    Recovery::Stop,
                    format!(
                        "this microphone records at {source_rate} Hz and Zyris cannot convert \
                         that to {SAMPLE_RATE} Hz ({error})"
                    ),
                )
            })?;
            let produced = fft.output_frames_next();
            Some(Resample {
                staging: Chunker::new(fft.input_frames_next()),
                scratch: vec![0.0; produced],
                produced,
                fft,
            })
        };

        Ok(Conversion {
            channels: channels.max(1),
            source_rate,
            mono: Vec::new(),
            out: Vec::new(),
            resampler,
        })
    }

    /// Hand over one callback's worth of interleaved samples; get back however much 16 kHz mono
    /// audio is ready.
    ///
    /// Often nothing: the resampler holds up to one FFT chunk, about 20 ms. The slice is valid
    /// until the next call.
    pub fn feed(&mut self, interleaved: &[f32]) -> &[f32] {
        self.fill(interleaved);
        &self.out
    }

    fn fill(&mut self, interleaved: &[f32]) {
        // Destructured so the staging chunker and the output buffer are two borrows rather than
        // one.
        let Conversion { channels, mono, out, resampler, .. } = self;
        mono.clear();
        out.clear();
        downmix_into(interleaved, *channels, mono);

        let Some(Resample { fft, staging, scratch, produced }) = resampler else {
            out.extend_from_slice(mono);
            return;
        };

        for chunk in staging.push(mono) {
            {
                let input = rubato::audioadapter_buffers::direct::SequentialSlice::new(
                    chunk,
                    1,
                    chunk.len(),
                )
                .expect("the staging chunker hands out exactly one channel of `frames` samples");
                let mut output = rubato::audioadapter_buffers::direct::SequentialSlice::new_mut(
                    &mut scratch[..],
                    1,
                    *produced,
                )
                .expect("the scratch buffer was allocated at `output_frames_next()`");
                // **Cannot fail.** `process_into_buffer` validates the channel count and the two
                // buffer lengths, and all three are fixed at construction: `FixedSync::Both` is
                // the one mode whose `update_chunk_sizes` does nothing, so
                // `input_frames_next()` and `output_frames_next()` never move.
                // `the_resamplers_chunk_sizes_never_move` is the test that says so, and it is
                // what fails if a future `rubato` changes its mind.
                fft.process_into_buffer(&input, &mut output, None)
                    .expect("both sides of the resampler are fixed, so the sizes always match");
            }
            out.extend_from_slice(&scratch[..*produced]);
        }
    }

    /// The rate the device records at.
    pub fn source_rate(&self) -> u32 {
        self.source_rate
    }

    /// How many channels the device interleaves.
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Input frames the resampler takes per call, or 0 when there is no resampler.
    pub fn staged_frames(&self) -> usize {
        self.resampler.as_ref().map_or(0, |r| r.staging.frames())
    }

    /// Output frames the resampler produces per call, or 0 when there is no resampler.
    pub fn produced_frames(&self) -> usize {
        self.resampler.as_ref().map_or(0, |r| r.produced)
    }
}

// ---------------------------------------------------------------------------------------------
// Devices
// ---------------------------------------------------------------------------------------------

/// Which end of a device this is, which microphone to open, and what one looks like on a
/// screen.
///
/// **Declared in [`crate::view`] and re-exported here**, which is the same accommodation
/// [`crate::Push`] makes: this module is behind the `voice` feature, a window has to name all
/// three in a build that does not have it, and [`Choice`] in particular is *stored* — it is
/// written into the settings file beside whether to listen at all. This is where they are
/// produced, so this is where the documentation about what the backend means by each of them
/// lives: see the module documentation above.
pub use crate::view::{Choice, Direction, InputDevice};

impl From<cpal::DeviceDirection> for Direction {
    fn from(direction: cpal::DeviceDirection) -> Direction {
        match direction {
            cpal::DeviceDirection::Input => Direction::Input,
            cpal::DeviceDirection::Output => Direction::Output,
            cpal::DeviceDirection::Duplex => Direction::Duplex,
            // `#[non_exhaustive]`, and "we do not know" is the right reading of a variant this
            // code has never seen.
            _ => Direction::Unknown,
        }
    }
}

/// Everything this machine could listen with, default and true inputs first.
///
/// An error here is the host being unreachable, not an empty list: a machine with no microphone
/// answers `Ok(vec![])`, which a window renders differently from "the sound server is not
/// running". This project has had to separate those twice before.
pub fn devices() -> Result<Vec<InputDevice>, DeviceProblem> {
    let host = cpal::default_host();
    let default = host.default_input_device().and_then(|device| device.id().ok());

    let mut devices: Vec<InputDevice> = host
        .input_devices()
        .map_err(|error| classify(&error))?
        .filter_map(|device| {
            // A device that will not say what it is has been unplugged between the enumeration
            // and now. Dropping it is right: it is absent, and there is nothing to tell anybody.
            let id = device.id().ok()?;
            let description = device.description().ok();
            Some(InputDevice {
                is_default: Some(&id) == default.as_ref(),
                id: id.to_string(),
                // `Display` rather than `description().name()` so a backend that cannot build a
                // whole description still gives a person something to read.
                name: device.to_string(),
                direction: description.map_or(Direction::Unknown, |d| d.direction().into()),
            })
        })
        .collect();

    // Stable, so two devices with the same name keep the order the host listed them in.
    devices.sort_by_key(|device| match (device.is_default, device.direction) {
        (true, _) => 0,
        (_, Direction::Input) => 1,
        (_, Direction::Unknown) => 2,
        (_, Direction::Duplex) => 3,
        (_, Direction::Output) => 4,
    });
    Ok(devices)
}

// ---------------------------------------------------------------------------------------------
// The stream
// ---------------------------------------------------------------------------------------------

/// What comes off a microphone.
///
/// One channel rather than two so a problem cannot overtake the audio that preceded it: a caller
/// reading this sees the last chunk that arrived before the device went away.
#[derive(Debug, Clone, PartialEq)]
pub enum Captured {
    /// Exactly the number of 16 kHz mono samples [`Capture::open`] was asked for.
    Audio(Vec<f32>),
    /// The device said something. [`DeviceProblem::recovery`] says what to do about it, and it
    /// is not always "stop" — a rerouted default stream reports and keeps running.
    Problem(DeviceProblem),
}

/// An open microphone.
///
/// **Dropping this stops the capture**, which is the whole of its lifetime management: `cpal`
/// stops and closes the stream in its own `Drop`. Keep it for as long as audio is wanted.
pub struct Capture {
    stream: cpal::Stream,
    source: cpal::SupportedStreamConfig,
    device: String,
    follows_default: bool,
}

impl Capture {
    /// Open a microphone and start delivering 16 kHz mono chunks of exactly `frames` samples.
    ///
    /// The receiver carries [`Captured`]. It is unbounded: the audio callback must not block,
    /// and a bounded channel that filled would either block it or drop somebody's words. Reading
    /// it promptly is the caller's job — 100 chunks a second at [`APM_FRAME`].
    ///
    /// The stream is opened at the device's own format and converted here. See the module
    /// documentation for why that is not a Linux-only luxury.
    pub fn open(
        choice: &Choice,
        frames: usize,
    ) -> Result<(Capture, mpsc::UnboundedReceiver<Captured>), DeviceProblem> {
        let host = cpal::default_host();
        let follows_default = matches!(choice, Choice::Default);

        let device = match choice {
            Choice::Default => host.default_input_device().ok_or_else(|| {
                DeviceProblem::new(
                    Recovery::Stop,
                    "this computer has no microphone the sound system can see",
                )
            })?,
            Choice::Device { id } => {
                let id: cpal::DeviceId = id.parse().map_err(|_| {
                    DeviceProblem::new(
                        Recovery::Rebuild,
                        "the microphone Zyris was told to use is not one this computer knows",
                    )
                })?;
                host.device_by_id(&id).ok_or_else(|| {
                    DeviceProblem::new(
                        Recovery::Rebuild,
                        "the microphone Zyris was told to use is not plugged in",
                    )
                })?
            }
        };

        // The probe that decides "absent" as well as the format to open at. One question, one
        // answer: `read_support` reads the same `Result`.
        let source = device.default_input_config().map_err(|error| classify(&error))?;
        let name = device.to_string();
        let config = source.config();

        let conversion = Conversion::new(source.sample_rate(), source.channels())?;
        let chunker = Chunker::new(frames);
        let (sender, receiver) = mpsc::unbounded_channel();

        let problems = sender.clone();
        let on_error = move |error: cpal::Error| {
            // A closed receiver means the session went away; there is nobody to tell.
            let _ = problems.send(Captured::Problem(classify(&error)));
        };

        // One arm per sample format because a Windows microphone hands over whatever its mix
        // format is, and that is not always `f32`. Only one arm runs, so moving the same values
        // in each is fine.
        let stream = match source.sample_format() {
            cpal::SampleFormat::I8 => device.build_input_stream::<i8, _, _>(
                config,
                widen(conversion, chunker, sender),
                on_error,
                Some(OPEN_TIMEOUT),
            ),
            cpal::SampleFormat::I16 => device.build_input_stream::<i16, _, _>(
                config,
                widen(conversion, chunker, sender),
                on_error,
                Some(OPEN_TIMEOUT),
            ),
            cpal::SampleFormat::I32 => device.build_input_stream::<i32, _, _>(
                config,
                widen(conversion, chunker, sender),
                on_error,
                Some(OPEN_TIMEOUT),
            ),
            cpal::SampleFormat::U8 => device.build_input_stream::<u8, _, _>(
                config,
                widen(conversion, chunker, sender),
                on_error,
                Some(OPEN_TIMEOUT),
            ),
            cpal::SampleFormat::U16 => device.build_input_stream::<u16, _, _>(
                config,
                widen(conversion, chunker, sender),
                on_error,
                Some(OPEN_TIMEOUT),
            ),
            cpal::SampleFormat::U32 => device.build_input_stream::<u32, _, _>(
                config,
                widen(conversion, chunker, sender),
                on_error,
                Some(OPEN_TIMEOUT),
            ),
            cpal::SampleFormat::F32 => device.build_input_stream::<f32, _, _>(
                config,
                widen(conversion, chunker, sender),
                on_error,
                Some(OPEN_TIMEOUT),
            ),
            cpal::SampleFormat::F64 => device.build_input_stream::<f64, _, _>(
                config,
                widen(conversion, chunker, sender),
                on_error,
                Some(OPEN_TIMEOUT),
            ),
            // `SampleFormat` is `#[non_exhaustive]` and the 24-bit formats arrive in a wrapper
            // type. Refusing is honest: a format nothing here can widen is a microphone Zyris
            // cannot read, and saying so beats handing whisper noise.
            other => {
                return Err(DeviceProblem::new(
                    Recovery::Stop,
                    format!("this microphone records as {other}, which Zyris cannot read"),
                ));
            }
        }
        .map_err(|error| classify(&error))?;

        // Streams come back stopped, input ones included.
        stream.play().map_err(|error| classify(&error))?;

        Ok((Capture { stream, source, device: name, follows_default }, receiver))
    }

    /// The format the device is actually recording at, before this crate converts it.
    pub fn source(&self) -> &cpal::SupportedStreamConfig {
        &self.source
    }

    /// What the open device is called, for the window.
    pub fn device(&self) -> &str {
        &self.device
    }

    /// Whether this stream follows the system default.
    ///
    /// **The difference decides how a route change arrives.** A stream on the default device is
    /// rerouted by the backend and reports [`Recovery::Continue`]; a stream on a named device is
    /// not, and reports [`Recovery::Rebuild`] when that device disappears. A caller that treated
    /// the two the same would either rebuild for nothing or go deaf without noticing.
    pub fn follows_default(&self) -> bool {
        self.follows_default
    }

    /// Stop delivering audio without dropping the handle.
    ///
    /// Not every backend can suspend a stream, so this can fail; that is not a reason to stop
    /// the session, which is why it says so rather than returning `()`.
    pub fn pause(&self) -> Result<(), DeviceProblem> {
        self.stream.pause().map_err(|error| classify(&error))
    }

    /// Start again after [`Self::pause`].
    pub fn resume(&self) -> Result<(), DeviceProblem> {
        self.stream.play().map_err(|error| classify(&error))
    }
}

/// The audio callback: widen to `f32`, convert, chunk, send.
///
/// **A type rather than a closure, and that is the only reason it is one.** `cpal` wants
/// `FnMut(&[T], &InputCallbackInfo)`, and `InputCallbackInfo` has private fields — so a test
/// cannot call the closure [`widen`] returns, and the four lines that join the widening, the
/// conversion and the chunking together were the one stage of the audio path nothing could
/// reach. `widen` is now the adaptor to `cpal`'s signature and adds nothing else;
/// `tests/a_held_key_turns_a_recording_into_text.rs` drives this.
///
/// Generic over the device's sample type because Windows hands over its mix format and will not
/// convert. The buffers are all allocated on the first call and reused, so the steady state is
/// one allocation per chunk sent and nothing else.
pub struct Callback {
    conversion: Conversion,
    chunker: Chunker,
    /// The device's samples as `f32`, before conversion. Kept across calls so the steady state
    /// allocates nothing for it.
    wide: Vec<f32>,
}

impl Callback {
    /// The conversion from the device's format, and the chunk length the caller asked for.
    pub fn new(conversion: Conversion, chunker: Chunker) -> Callback {
        Callback { conversion, chunker, wide: Vec::new() }
    }

    /// One buffer from the device, as [`Captured::Audio`] chunks on `sender`.
    pub fn deliver<T>(&mut self, data: &[T], sender: &mpsc::UnboundedSender<Captured>)
    where
        T: cpal::SizedSample,
        f32: FromSample<T>,
    {
        self.wide.clear();
        self.wide.extend(data.iter().map(|sample| sample.to_sample::<f32>()));
        for chunk in self.chunker.push(self.conversion.feed(&self.wide)) {
            // Nobody listening means the session ended; the stream is about to be dropped.
            if sender.send(Captured::Audio(chunk.to_vec())).is_err() {
                return;
            }
        }
    }
}

fn widen<T>(
    conversion: Conversion,
    chunker: Chunker,
    sender: mpsc::UnboundedSender<Captured>,
) -> impl FnMut(&[T], &cpal::InputCallbackInfo)
where
    T: cpal::SizedSample,
    f32: FromSample<T>,
{
    let mut callback = Callback::new(conversion, chunker);
    move |data: &[T], _: &cpal::InputCallbackInfo| callback.deliver(data, &sender)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three callback lengths this project has actually seen, and the chunk sizes the rest
    /// of the pipeline demands. 1024 is ALSA; 314 and 628 are PipeWire at 16 kHz mono and at
    /// stereo. [`APM_FRAME`] and 300 divide none of the three; [`VAD_FRAME`] divides 1024 only.
    const OBSERVED_CALLBACKS: [usize; 3] = [314, 628, 1024];
    const WANTED_CHUNKS: [usize; 3] = [APM_FRAME, VAD_FRAME, 300];

    fn ramp(len: usize, from: usize) -> Vec<f32> {
        (from..from + len).map(|i| i as f32).collect()
    }

    /// **The one thing standing between a backend change and a crash.** The APM panics rather
    /// than returning `Err` when it is handed the wrong number of samples, so a chunk of the
    /// wrong length is not a bad answer, it is a dead process.
    #[test]
    fn every_observed_callback_length_produces_only_whole_chunks() {
        for callback in OBSERVED_CALLBACKS {
            for wanted in WANTED_CHUNKS {
                let mut chunker = Chunker::new(wanted);
                let mut seen: Vec<f32> = Vec::new();
                let mut fed = 0usize;

                for round in 0..40 {
                    let buffer = ramp(callback, round * callback);
                    fed += buffer.len();
                    for chunk in chunker.push(&buffer) {
                        assert_eq!(
                            chunk.len(),
                            wanted,
                            "a {callback}-sample callback produced a chunk of {} where {wanted} \
                             was asked for",
                            chunk.len()
                        );
                        seen.extend_from_slice(chunk);
                    }
                }

                assert_eq!(
                    seen.len(),
                    fed / wanted * wanted,
                    "{callback} into {wanted}: everything that could be emitted must be"
                );
                assert_eq!(seen, ramp(seen.len(), 0), "samples must arrive in order and unaltered");
            }
        }
    }

    /// **A backend's callback length is not even constant within one stream.** Measured here on
    /// 2026-09-15, PipeWire, a 16 kHz mono request: 314, 341 and 342 samples, alternating. A
    /// chunker that quietly assumed one length would work for a while and then not.
    #[test]
    fn a_callback_length_that_changes_from_call_to_call_is_still_cut_cleanly() {
        for wanted in WANTED_CHUNKS {
            let mut chunker = Chunker::new(wanted);
            let mut seen: Vec<f32> = Vec::new();
            let mut fed = 0usize;

            for (round, callback) in [314usize, 341, 342].into_iter().cycle().take(90).enumerate() {
                let buffer = ramp(callback, fed);
                fed += buffer.len();
                for chunk in chunker.push(&buffer) {
                    assert_eq!(chunk.len(), wanted, "round {round} of {callback} samples");
                    seen.extend_from_slice(chunk);
                }
            }

            assert_eq!(seen, ramp(fed / wanted * wanted, 0));
        }
    }

    /// A callback shorter than one chunk emits nothing and loses nothing.
    #[test]
    fn samples_are_held_until_there_are_enough_of_them() {
        let mut chunker = Chunker::new(APM_FRAME);

        for i in 0..APM_FRAME - 1 {
            assert_eq!(chunker.push(&[i as f32]).count(), 0, "not a whole chunk yet");
        }

        let last: Vec<&[f32]> = chunker.push(&[(APM_FRAME - 1) as f32]).collect();
        assert_eq!(last.len(), 1);
        assert_eq!(last[0], ramp(APM_FRAME, 0));
    }

    /// One callback that carries several chunks' worth hands them all over at once, in order.
    #[test]
    fn one_long_callback_emits_every_chunk_it_holds() {
        let mut chunker = Chunker::new(APM_FRAME);

        let emitted: Vec<Vec<f32>> =
            chunker.push(&ramp(APM_FRAME * 3 + 7, 0)).map(<[f32]>::to_vec).collect();

        assert_eq!(emitted.len(), 3);
        for (index, chunk) in emitted.iter().enumerate() {
            assert_eq!(chunk, &ramp(APM_FRAME, index * APM_FRAME));
        }
        assert_eq!(chunker.push(&ramp(APM_FRAME - 7, APM_FRAME * 3 + 7)).count(), 1);
    }

    /// An empty callback is not a chunk boundary and does not flush the tail. A backend that
    /// delivers one on a device with nothing to say must not make the APM see a short frame.
    #[test]
    fn an_empty_callback_emits_nothing_and_keeps_what_was_held() {
        let mut chunker = Chunker::new(APM_FRAME);
        assert_eq!(chunker.push(&ramp(APM_FRAME - 1, 0)).count(), 0);

        assert_eq!(chunker.push(&[]).count(), 0);

        let after: Vec<&[f32]> = chunker.push(&[(APM_FRAME - 1) as f32]).collect();
        assert_eq!(after.len(), 1, "the held samples must still be there");
        assert_eq!(after[0], ramp(APM_FRAME, 0));
    }

    #[test]
    fn stereo_becomes_the_mean_of_its_channels() {
        let mut out = Vec::new();

        downmix_into(&[1.0, 3.0, -2.0, 2.0], 2, &mut out);

        assert_eq!(out, vec![2.0, 0.0]);
    }

    #[test]
    fn mono_is_left_alone() {
        let mut out = Vec::new();

        downmix_into(&[0.25, -0.5, 1.0], 1, &mut out);

        assert_eq!(out, vec![0.25, -0.5, 1.0]);
    }

    /// A buffer that does not divide by the channel count is truncated rather than completed
    /// with a sample nobody recorded.
    #[test]
    fn a_frame_the_device_did_not_finish_is_not_invented() {
        let mut out = Vec::new();

        downmix_into(&[1.0, 3.0, 7.0], 2, &mut out);

        assert_eq!(out, vec![2.0]);
    }

    /// Goertzel: how much of `freq` is in `samples`, normalised by length.
    fn energy_at(samples: &[f32], rate: f32, freq: f32) -> f32 {
        let coeff = 2.0 * (2.0 * std::f32::consts::PI * freq / rate).cos();
        let (mut s1, mut s2) = (0.0f32, 0.0f32);
        for &x in samples {
            let s0 = x + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        ((s1 * s1 + s2 * s2 - coeff * s1 * s2).abs()).sqrt() / samples.len() as f32
    }

    fn tone(rate: u32, freq: f32, seconds: f32, channels: u16, right: f32) -> Vec<f32> {
        let frames = (rate as f32 * seconds) as usize;
        let mut buffer = Vec::with_capacity(frames * channels as usize);
        for frame in 0..frames {
            let value =
                (2.0 * std::f32::consts::PI * freq * frame as f32 / rate as f32).sin() * 0.5;
            buffer.push(value);
            for _ in 1..channels {
                buffer.push(value * right);
            }
        }
        buffer
    }

    /// Push a whole clip through in the odd-sized pieces a backend actually delivers.
    fn feed_in_callbacks(
        conversion: &mut Conversion,
        clip: &[f32],
        callback_frames: usize,
    ) -> Vec<f32> {
        let samples_per_callback = callback_frames * usize::from(conversion.channels());
        let mut out = Vec::new();
        for piece in clip.chunks(samples_per_callback) {
            out.extend_from_slice(conversion.feed(piece));
        }
        out
    }

    /// **The Windows shape, run on Linux.** WASAPI hands capture streams their native format
    /// only — 48 kHz stereo `f32` is the usual one — so this is the conversion that has to
    /// happen and that no end-to-end test on this machine would otherwise reach.
    #[test]
    fn forty_eight_kilohertz_stereo_becomes_sixteen_kilohertz_mono() {
        let mut conversion = Conversion::new(48_000, 2).expect("48 kHz stereo is ordinary");
        let clip = tone(48_000, 440.0, 1.0, 2, 1.0);

        let out = feed_in_callbacks(&mut conversion, &clip, 1024);

        assert!(
            (out.len() as i64 - 16_000).abs() < 1_500,
            "a second of 48 kHz stereo must come out as about a second of 16 kHz mono, got {}",
            out.len()
        );
        let wanted = energy_at(&out, 16_000.0, 440.0);
        let elsewhere = energy_at(&out, 16_000.0, 1_400.0);
        assert!(
            wanted > 0.1 && wanted > elsewhere * 20.0,
            "the tone must survive the conversion: 440 Hz {wanted}, 1400 Hz {elsewhere}"
        );
    }

    /// Anti-aliasing is the reason this is `rubato` and not "take every third sample". A tone
    /// above the new Nyquist must be removed, not folded down onto a frequency a person never
    /// made.
    #[test]
    fn a_tone_above_the_new_nyquist_is_removed_rather_than_folded_down() {
        let mut conversion = Conversion::new(48_000, 1).expect("48 kHz mono is ordinary");
        let clip = tone(48_000, 10_000.0, 1.0, 1, 1.0);

        let out = feed_in_callbacks(&mut conversion, &clip, 1024);

        let alias = energy_at(&out, 16_000.0, 6_000.0);
        assert!(alias < 0.02, "10 kHz must not come back as 6 kHz; got {alias}");
    }

    /// The two channels of a device wired out of phase cancel, which is what an average means.
    #[test]
    fn the_two_channels_are_averaged_and_not_just_taken_from_the_left() {
        let mut conversion = Conversion::new(16_000, 2).expect("16 kHz stereo is ordinary");
        let clip = tone(16_000, 440.0, 0.5, 2, -1.0);

        let out = feed_in_callbacks(&mut conversion, &clip, 628);

        let left_only = energy_at(&out, 16_000.0, 440.0);
        assert!(left_only < 1e-5, "L and -L average to silence; got {left_only}");
    }

    /// A device that already offers what we want still goes through the same code. There is no
    /// "Linux is fine" branch to fall off on Windows.
    #[test]
    fn a_device_already_at_sixteen_kilohertz_mono_passes_through_unchanged() {
        let mut conversion = Conversion::new(SAMPLE_RATE, 1).expect("16 kHz mono is ordinary");
        let clip = ramp(1024 * 3, 0);

        let out = feed_in_callbacks(&mut conversion, &clip, 1024);

        assert_eq!(out, clip);
    }

    /// Nothing is lost between callbacks, whatever length they come in. Three backends' worth of
    /// callback lengths through one 48 kHz stereo conversion, and the total has to come out at
    /// the ratio give or take the resampler's own carry.
    #[test]
    fn odd_callback_lengths_lose_nothing_across_the_conversion() {
        for callback in OBSERVED_CALLBACKS {
            let mut conversion = Conversion::new(48_000, 2).expect("48 kHz stereo is ordinary");
            let clip = tone(48_000, 440.0, 2.0, 2, 1.0);

            let out = feed_in_callbacks(&mut conversion, &clip, callback);

            let expected = 2 * SAMPLE_RATE as i64;
            assert!(
                (out.len() as i64 - expected).abs() < 2_000,
                "{callback}-frame callbacks produced {} samples where about {expected} were due",
                out.len()
            );
        }
    }

    /// The `expect` inside [`Conversion::feed`] rests on this: with both sides of the resampler
    /// fixed, `rubato` never renegotiates the sizes, so a length mismatch is impossible rather
    /// than merely unlikely. If upstream changes that, this test is what says so.
    #[test]
    fn the_resamplers_chunk_sizes_never_move() {
        let mut conversion = Conversion::new(44_100, 2).expect("44.1 kHz stereo is ordinary");
        let (staged, produced) = (conversion.staged_frames(), conversion.produced_frames());
        assert!(staged > 0 && produced > 0, "a resampler with no chunk size is not configured");

        for round in 0..50 {
            conversion.feed(&tone(44_100, 300.0, 0.02, 2, 1.0));
            assert_eq!(conversion.staged_frames(), staged, "round {round}");
            assert_eq!(conversion.produced_frames(), produced, "round {round}");
        }
    }

    /// Every `ErrorKind` `cpal` can report, and what a caller is supposed to do about it. Built
    /// from the kind alone, so this runs on a machine with no sound card at all.
    #[test]
    fn every_error_kind_says_what_to_do_about_it() {
        use cpal::ErrorKind::*;

        let cases = [
            (DeviceChanged, Recovery::Continue),
            (DeviceNotAvailable, Recovery::Rebuild),
            (StreamInvalidated, Recovery::Rebuild),
            (DeviceBusy, Recovery::Retry),
            (Xrun, Recovery::Continue),
            (RealtimeDenied, Recovery::Continue),
            (PermissionDenied, Recovery::Stop),
            (HostUnavailable, Recovery::Stop),
            (UnsupportedConfig, Recovery::Stop),
            (UnsupportedOperation, Recovery::Stop),
            (InvalidInput, Recovery::Stop),
            (ResourceExhausted, Recovery::Retry),
            (BackendError, Recovery::Stop),
            (Other, Recovery::Stop),
        ];

        for (kind, expected) in cases {
            let problem = classify(&cpal::Error::new(kind));
            assert_eq!(problem.recovery, expected, "{kind:?}");
            assert!(!problem.reason.is_empty(), "{kind:?} must be sayable to a person");
        }
    }

    /// **A rerouted default stream is not a failure.** `cpal` only reports it for a stream built
    /// on the default device, and the stream is still running — rebuilding it would drop audio
    /// for no reason.
    #[test]
    fn a_route_change_is_not_a_reason_to_rebuild() {
        let problem = classify(&cpal::Error::new(cpal::ErrorKind::DeviceChanged));

        assert_eq!(problem.recovery, Recovery::Continue);
        assert!(problem.settings.is_none());
    }

    /// **Windows denies the microphone as a backend error, not as `PermissionDenied`.**
    /// `cpal`'s WASAPI `From<windows::core::Error>` maps eight HRESULTs and `E_ACCESSDENIED` is
    /// not among them, so it falls through to `ErrorKind::BackendError` carrying
    /// `std::io::Error::from_raw_os_error(0x80070005 as i32).to_string()`. Reading the message
    /// is the only way to tell it from an unclassifiable backend fault.
    #[test]
    fn windows_access_denied_is_read_as_a_permission_problem() {
        for message in [
            // What `io::Error`'s `Display` produces when `FormatMessageW` resolves the code.
            "Access is denied. (os error -2147024891)",
            // And when it does not: `std` falls back to the number alone, and the number it was
            // handed is the HRESULT reinterpreted as `i32`. **A classifier that matched only on
            // the words would miss this one**, which is why it is here — a mutation removing
            // either half of `looks_like_access_denied` has to go red.
            "os error -2147024891",
            // And the spelling every document uses, in case a future `cpal` formats the HRESULT
            // itself rather than going through `io::Error`.
            "Failed to initialize audio client: 0x80070005",
            "Failed to initialize audio client: Access is denied. (0x80070005)",
            // And with no number at all, which is what `windows::core::Error::message()` gives
            // on its own — the spelling a `cpal` that stopped routing through `io::Error` would
            // produce. **This is the case that makes the words clause load-bearing**; every
            // other message here carries a number, so without it that clause could be deleted
            // and nothing would go red.
            "Access is denied.",
        ] {
            let problem =
                classify(&cpal::Error::with_message(cpal::ErrorKind::BackendError, message));

            assert_eq!(problem.recovery, Recovery::Stop, "{message}");
            assert_eq!(
                problem.settings.as_deref(),
                Some(MICROPHONE_PRIVACY),
                "a person who is refused the microphone needs the page that grants it"
            );
        }
    }

    /// And an ordinary backend error is not quietly turned into a permission story.
    #[test]
    fn an_unclassifiable_backend_error_stays_unclassified() {
        let problem = classify(&cpal::Error::with_message(
            cpal::ErrorKind::BackendError,
            "snd_pcm_hw_params failed",
        ));

        assert!(problem.settings.is_none());
        assert!(problem.reason.contains("snd_pcm_hw_params failed"), "{}", problem.reason);
    }

    /// **"No input device" is `DeviceNotAvailable`, never an empty `default_input_device()`.**
    /// With ALSA configured to nothing, `default_input_device()` still answers
    /// `Some("Default Audio Device")` — the same rule `EnigoInput::new` follows, and for the
    /// same reason: a control that fails every call is worse than an absent one.
    #[test]
    fn a_machine_with_no_microphone_says_so_rather_than_looking_ready() {
        let absent = read_support(Err(cpal::Error::new(cpal::ErrorKind::DeviceNotAvailable)));

        assert!(
            matches!(absent, VoiceSupport::Unavailable { .. }),
            "a device that will not answer must not be announced as ready"
        );
        let VoiceSupport::Unavailable { reason } = absent else { unreachable!() };
        assert!(!reason.is_empty());

        let refused = read_support(Err(cpal::Error::new(cpal::ErrorKind::PermissionDenied)));
        assert!(matches!(refused, VoiceSupport::Unavailable { .. }));

        let present = read_support(Ok(cpal::SupportedStreamConfig::new(
            2,
            48_000,
            cpal::SupportedBufferSize::Unknown,
            cpal::SampleFormat::F32,
        )));
        assert_eq!(present, VoiceSupport::Ready);
    }

    /// Task 6 owns the capture from a task of its own, so it has to be able to move there.
    /// A compile-time assertion; there is nothing to run.
    #[test]
    fn a_capture_can_be_moved_to_the_task_that_owns_it() {
        fn assert_send<T: Send>() {}
        assert_send::<Capture>();
        assert_send::<Conversion>();
        assert_send::<Chunker>();
    }

    /// The list is a list, whatever this machine has. It may legitimately be empty — a runner
    /// with no sound card — but every entry a host does report has to be usable: an id a choice
    /// can be stored as, and a name a person can read.
    #[test]
    fn every_device_offered_can_be_named_and_chosen() {
        let Ok(devices) = devices() else {
            // No host. That is an answer, and `classify` already has tests.
            return;
        };

        assert!(devices.iter().filter(|d| d.is_default).count() <= 1, "{devices:?}");
        for device in &devices {
            assert!(!device.id.is_empty(), "{device:?}");
            assert!(!device.name.is_empty(), "{device:?}");
            assert!(
                device.id.parse::<cpal::DeviceId>().is_ok(),
                "an id that will not parse back cannot be stored as a choice: {device:?}"
            );
        }
        // Whatever the host said, the default and the true microphones sort to the front.
        let ordered: Vec<bool> =
            devices.iter().map(|d| d.is_default || d.direction == Direction::Input).collect();
        assert!(
            ordered.windows(2).all(|pair| pair[0] >= pair[1]),
            "microphones must not be listed below the loudspeakers: {devices:?}"
        );
    }

    /// **Needs a microphone, so it is not part of the suite.** Run it by hand with
    /// `cargo test -p zyris-voice --features voice -- --ignored --nocapture`.
    ///
    /// It is the only thing that proves the whole module against a real device: the callback
    /// length the backend chose, the widening, the conversion and the chunker all together.
    #[tokio::test]
    #[ignore = "opens the microphone on this machine"]
    async fn the_microphone_delivers_chunks_of_exactly_the_size_asked_for() {
        let (capture, mut chunks) =
            Capture::open(&Choice::Default, APM_FRAME).expect("a microphone on this machine");
        println!(
            "opened {} at {} Hz ({} ch, {}), following the default: {}",
            capture.device(),
            capture.source().sample_rate(),
            capture.source().channels(),
            capture.source().sample_format(),
            capture.follows_default()
        );

        let mut audio = 0usize;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            // **The deadline is the assertion.** A microphone that opened and then said nothing
            // would otherwise leave `recv()` pending forever, and `#[tokio::test]` has no
            // timeout of its own — the suite would hang rather than fail.
            let next = tokio::time::timeout(Duration::from_millis(500), chunks.recv()).await;
            match next {
                Ok(Some(Captured::Audio(chunk))) => {
                    assert_eq!(chunk.len(), APM_FRAME, "a short chunk would panic the APM");
                    audio += 1;
                }
                Ok(Some(Captured::Problem(problem))) => println!("problem: {problem:?}"),
                Ok(None) => panic!("the stream ended while the capture was still held"),
                Err(_) => panic!("half a second with no audio from an open microphone"),
            }
        }

        println!("{audio} chunks of {APM_FRAME} samples in two seconds");
        assert!(
            audio > 150,
            "two seconds at 16 kHz is about 200 chunks of {APM_FRAME}, got {audio}"
        );
    }
}
