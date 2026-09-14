//! The speaker: one output stream, a queue of synthesised fragments, and a tap carrying exactly
//! what was handed to the device.
//!
//! # Three things this is not symmetrical with capture
//!
//! - **The rate is the model's, not whisper's.** [`crate::tts::SAMPLE_RATE`] is 44.1 kHz where
//!   [`crate::capture::SAMPLE_RATE`] is 16 kHz, and unlike capture there is **no resampler
//!   here**: `cpal`'s WASAPI backend sets `AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM` on *output*
//!   streams and accepts 8 kHz to 384 kHz, its own comment saying capture streams get native
//!   formats only; ALSA's plug layer and PipeWire accept an arbitrary output request too. So the
//!   stream is opened at the rate the vocoder produces and the platform converts. If a backend
//!   ever refuses, [`Playback::open`] says so rather than playing at the wrong speed — a voice
//!   at the wrong rate is not a degraded answer, it is a different person.
//! - **Nobody is talking when the queue is empty**, so an underrun is silence rather than a
//!   failure. It is counted ([`Fill::starved`]) because a stream that is starving every callback
//!   is a synthesiser that has stopped keeping up, which is a real fault with no error attached.
//! - **The audio that leaves has to be kept**, briefly. Echo cancellation needs the *reference*
//!   signal — what the speaker actually emitted, silence included — cut into the frames the
//!   processor wants. That is the tap, and it is why this module exists in task 1 rather than in
//!   task 4: retrofitting a tap into a callback is how the alignment gets lost.
//!
//! # The seam, built first on purpose
//!
//! `cpal` wants `FnMut(&mut [T], &OutputCallbackInfo)`. [`Fill`] is the whole of what that
//! closure does, as a type a test can drive; [`widen`] is the adaptor and adds nothing.
//!
//! **The reason given for this shape in the step-7 notes and in the step-8 plan is wrong**, and
//! it is worth writing down because it was believed twice: "`OutputCallbackInfo`'s only field is
//! `pub(crate)`, so a test cannot construct one". The *field* is `pub(crate)`; `cpal` 0.18.2 also
//! has `pub fn OutputCallbackInfo::new(OutputStreamTimestamp)`, `OutputStreamTimestamp`'s two
//! fields are public, and `StreamInstant::ZERO` plus `checked_add` build any instant you like —
//! so a test can construct one, and the tests below do. The same is true of `InputCallbackInfo`,
//! which `capture.rs` says otherwise about. The split is kept anyway, for the reason that
//! survives: the fill logic is worth testing without a `cpal` type in the way, and `widen` is
//! then four lines with nothing in them to get wrong.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _};
use cpal::{FromSample, OutputStreamTimestamp};
use tokio::sync::mpsc;

use crate::apm::Apm;
use crate::capture::{
    Chunker, Conversion, DeviceProblem, Recovery, UNKNOWN_DELAY, classify, read_delay, sane,
    store_delay,
};
use crate::view::Choice;

/// How long `cpal` may take to hand over a stream before this gives up. `capture`'s number.
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

/// Ten milliseconds of the output stream, which is the frame the echo canceller wants.
///
/// `apm::analyze_render` takes exactly 10 ms — 160 samples at 16 kHz — so a tap cut at 10 ms of
/// *this* stream resamples to exactly one of its frames whatever the output rate is. At 44.1 kHz
/// that is 441 samples, a number no backend hands out on its own.
pub fn render_frame(rate: u32) -> usize {
    (rate as usize / 100).max(1)
}

/// The five numbers the callback, the `Playback` and the [`Speaker`] all read.
///
/// A struct rather than five arguments because they are always built together, always cloned
/// together and mean nothing apart — and because [`Playback::open`] builds a `Fill` once per
/// attempted layout, where five positional `Arc`s of the same type is five chances to swap two
/// of them with no type error to say so.
#[derive(Clone)]
pub struct Counters {
    /// Samples handed to [`Speaker::speak`].
    sent: Arc<AtomicU64>,
    /// Samples written to the device.
    played: Arc<AtomicU64>,
    /// Samples taken off the queue and thrown away.
    discarded: Arc<AtomicU64>,
    /// `playback - callback`, in nanoseconds.
    delay: Arc<AtomicU64>,
    /// How many times [`Playback::silence`] has been called.
    silenced: Arc<AtomicU64>,
}

impl Counters {
    /// A fresh set, all at zero and with no delay reported yet.
    pub fn new() -> Counters {
        Counters {
            sent: Arc::new(AtomicU64::new(0)),
            played: Arc::new(AtomicU64::new(0)),
            discarded: Arc::new(AtomicU64::new(0)),
            delay: Arc::new(AtomicU64::new(UNKNOWN_DELAY)),
            silenced: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Samples written to the device, ever. **How far playback got.**
    pub fn played(&self) -> u64 {
        self.played.load(Ordering::Relaxed)
    }

    /// Samples taken off the queue and thrown away, ever.
    pub fn discarded(&self) -> u64 {
        self.discarded.load(Ordering::Relaxed)
    }
}

impl Default for Counters {
    fn default() -> Counters {
        Counters::new()
    }
}

/// The audio callback: take from the queue, spread across the channels, keep a copy.
///
/// Generic over the device's sample type for the same reason [`crate::capture::Callback`] is:
/// Windows hands over whatever its mix format is and will not convert.
pub struct Fill {
    /// Fragments waiting to be played, oldest first.
    audio: mpsc::UnboundedReceiver<Vec<f32>>,
    /// The fragment being played and how far into it we are. A fragment is almost never a whole
    /// number of callbacks, so one is nearly always half spent.
    front: Vec<f32>,
    at: usize,
    /// This callback's mono output, before it is spread across the channels. Reused, so the
    /// steady state allocates only the tap's frames.
    mono: Vec<f32>,
    /// Exactly what went to the device, in 10 ms frames. **Silence included**: a reference signal
    /// with the gaps taken out would drift against the microphone by the length of every gap.
    chunker: crate::capture::Chunker,
    render: mpsc::UnboundedSender<Vec<f32>>,
    /// Samples taken off the queue and **written to the device**. Shared, so the sending side
    /// can say how much is still to come without a second channel — and so that barge-in can
    /// read how far playback actually got.
    played: Arc<AtomicU64>,
    /// Samples taken off the queue and thrown away by [`Playback::silence`]. Counted apart from
    /// `played` because the two answer different questions: what is still queued, and what was
    /// spoken. Adding a discard to `played` would make an interruption claim the speaker had
    /// said everything that was ever queued.
    discarded: Arc<AtomicU64>,
    /// `playback - callback` from the last callback, in nanoseconds, or `UNKNOWN_DELAY`.
    delay: Arc<AtomicU64>,
    /// Which round of speech this is. [`Playback::silence`] increments the shared counter; the
    /// callback notices on its next run and drops everything it is holding.
    silenced: Arc<AtomicU64>,
    seen: u64,
    /// Callbacks that ran out of audio part-way. See the module documentation.
    starved: u64,
}

impl Fill {
    /// `frame` is [`render_frame`] for the stream's rate.
    pub fn new(
        audio: mpsc::UnboundedReceiver<Vec<f32>>,
        render: mpsc::UnboundedSender<Vec<f32>>,
        frame: usize,
        counters: Counters,
    ) -> Fill {
        Fill {
            audio,
            front: Vec::new(),
            at: 0,
            mono: Vec::new(),
            chunker: Chunker::new(frame),
            render,
            played: counters.played,
            discarded: counters.discarded,
            delay: counters.delay,
            silenced: counters.silenced,
            seen: 0,
            starved: 0,
        }
    }

    /// Everything [`Playback::silence`] asked to be thrown away, thrown away.
    ///
    /// Run at the top of every callback rather than from the caller's thread, because the queue's
    /// receiving end lives in here and an audio callback is the only thing allowed to touch it.
    /// What the device has already been written is **not** recalled: those samples are in its
    /// buffer and will be heard, which is exactly why [`Playback::stream_delay`] is the
    /// uncertainty in how far speech got and not a number anything can undo.
    fn discard_if_silenced(&mut self) {
        let silenced = self.silenced.load(Ordering::Relaxed);
        if silenced == self.seen {
            return;
        }
        self.seen = silenced;

        let mut dropped = (self.front.len() - self.at) as u64;
        self.front.clear();
        self.at = 0;
        while let Ok(fragment) = self.audio.try_recv() {
            dropped += fragment.len() as u64;
        }
        self.discarded.fetch_add(dropped, Ordering::Relaxed);
    }

    /// Fill one buffer the device is waiting for.
    ///
    /// `out` is interleaved: `out.len()` is frames × `channels`. A frame the device did not ask
    /// for a whole of is not written, the same rule [`crate::capture::downmix_into`] applies in
    /// the other direction.
    pub fn deliver<T>(&mut self, out: &mut [T], channels: u16, at: Option<OutputStreamTimestamp>)
    where
        T: cpal::SizedSample + FromSample<f32>,
    {
        if let Some(at) = at {
            // Saturating rather than checked: a backend whose predicted playback instant is
            // before the callback is saying "no latency I can measure", not "negative latency".
            store_delay(&self.delay, at.playback.saturating_duration_since(at.callback));
        }
        self.discard_if_silenced();

        let channels = usize::from(channels.max(1));
        let frames = out.len() / channels;
        self.mono.clear();
        self.mono.reserve(frames);

        let mut starved = false;
        while self.mono.len() < frames {
            if self.at == self.front.len() {
                match self.audio.try_recv() {
                    Ok(next) => {
                        self.front = next;
                        self.at = 0;
                        // An empty fragment would spin this loop forever.
                        if self.front.is_empty() {
                            continue;
                        }
                    }
                    Err(_) => {
                        starved = true;
                        break;
                    }
                }
            }
            let wanted = (frames - self.mono.len()).min(self.front.len() - self.at);
            self.mono.extend(self.front[self.at..self.at + wanted].iter().copied().map(sane));
            self.at += wanted;
        }

        self.played.fetch_add(self.mono.len() as u64, Ordering::Relaxed);
        if starved {
            self.starved += 1;
            self.mono.resize(frames, 0.0);
        }

        for (frame, sample) in out.chunks_exact_mut(channels).zip(self.mono.iter().copied()) {
            let wide = T::from_sample(sample);
            for slot in frame.iter_mut() {
                *slot = wide;
            }
        }
        // The frames the device did not ask a whole of. Leaving somebody else's buffer contents
        // there is a click.
        let written = frames * channels;
        for slot in out[written..].iter_mut() {
            *slot = T::from_sample(0.0f32);
        }

        for frame in self.chunker.push(&self.mono) {
            // Nobody listening means task 4's canceller is not running; that is ordinary.
            if self.render.send(frame.to_vec()).is_err() {
                break;
            }
        }
    }

    /// How many callbacks ran out of audio part-way through.
    ///
    /// Not a failure on its own — the queue is empty whenever nobody is speaking — but a number
    /// that climbs *while* a fragment is playing is a synthesiser falling behind the speaker, and
    /// there is no error anywhere else to say so.
    pub fn starved(&self) -> u64 {
        self.starved
    }
}

/// The adaptor to `cpal`'s signature, and nothing else.
pub fn widen<T>(
    mut fill: Fill,
    channels: u16,
) -> impl FnMut(&mut [T], &cpal::OutputCallbackInfo) + Send + 'static
where
    T: cpal::SizedSample + FromSample<f32> + Send + 'static,
{
    move |out: &mut [T], info: &cpal::OutputCallbackInfo| {
        fill.deliver(out, channels, Some(info.timestamp()))
    }
}

/// The queue in front of an open speaker, and the counters behind it.
///
/// Cheap to clone and `Send + Sync`, which a [`Playback`] is not. Everything here is either a
/// channel or an atomic; nothing in it can fail because the device went away, only report that
/// it did.
#[derive(Clone)]
pub struct Speaker {
    audio: mpsc::UnboundedSender<Vec<f32>>,
    counters: Counters,
}

impl Speaker {
    /// Hand one fragment to the speaker.
    ///
    /// Answers **where in the stream this fragment starts** — the sample count already handed
    /// over before it — or `None` when the stream has gone. That offset is what lets a ledger of
    /// what was queued be read against [`Counters::played`] later without a second counter: two
    /// counters that have to agree, with nothing that goes red when they stop, is the shape this
    /// workspace keeps finding in its own review notes.
    pub fn speak(&self, samples: Vec<f32>) -> Option<u64> {
        let at = self.counters.sent.fetch_add(samples.len() as u64, Ordering::Relaxed);
        self.audio.send(samples).is_ok().then_some(at)
    }

    /// Throw away everything queued and not yet written to the device.
    ///
    /// **What has already been written is not recalled.** Those samples are inside the device's
    /// own buffer; they will be heard, and no API here can stop them. The gap between "handed
    /// over" and "heard" is [`Self::stream_delay`] — 42.67 ms on this machine — which is the
    /// whole of the uncertainty in how far speech got, and the reason [`Counters::played`] is
    /// the honest answer rather than an estimate from a clock.
    ///
    /// Takes effect on the next callback, which is within one buffer. It does not stop the
    /// stream: a paused stream is one more thing to remember to start again, not every backend
    /// can pause, and an empty queue plays silence — which is what it does between sentences
    /// anyway.
    pub fn silence(&self) {
        self.counters.silenced.fetch_add(1, Ordering::Relaxed);
    }

    /// Samples written to the device, ever. **How far playback got.**
    pub fn played(&self) -> u64 {
        self.counters.played()
    }

    /// How many samples are queued and not yet handed to the device.
    ///
    /// The only honest answer to "is it still talking?" from this side. It is **not** "how much
    /// is still audible": the device holds another [`Self::stream_delay`] of it.
    pub fn pending(&self) -> u64 {
        self.counters
            .sent
            .load(Ordering::Relaxed)
            .saturating_sub(self.counters.played())
            .saturating_sub(self.counters.discarded())
    }

    /// `playback - callback` as the backend last reported it: how far ahead of the speaker the
    /// callback is writing.
    ///
    /// **One of the two halves of the echo canceller's stream delay**, the other being
    /// [`crate::capture::Capture::stream_delay`]. A canceller given the wrong delay cancels less
    /// while going on reporting that it is working, so half of it is not a usable figure.
    pub fn stream_delay(&self) -> Option<Duration> {
        read_delay(&self.counters.delay)
    }

    /// The counters this stream shares with its callback.
    pub fn counters(&self) -> &Counters {
        &self.counters
    }
}

/// An open speaker.
///
/// **Dropping this stops playback**, exactly as dropping a [`crate::capture::Capture`] stops the
/// microphone: `cpal` closes the stream in its own `Drop`.
pub struct Playback {
    stream: cpal::Stream,
    config: cpal::StreamConfig,
    device: String,
    /// Everything about this stream that is not the `cpal::Stream` itself. See [`Speaker`].
    out: Speaker,
}

impl Playback {
    /// Open a speaker at [`crate::tts::SAMPLE_RATE`] and start it.
    ///
    /// The receiver carries the render tap: 10 ms frames of exactly what went to the device.
    /// It is unbounded, like capture's, because an audio callback may not block — and it must be
    /// read or dropped: a caller that keeps it and never reads it grows a buffer at 100 frames a
    /// second.
    pub fn open(
        choice: &Choice,
    ) -> Result<(Playback, mpsc::UnboundedReceiver<Vec<f32>>), DeviceProblem> {
        let host = cpal::default_host();
        let device = match choice {
            Choice::Default => host.default_output_device().ok_or_else(|| {
                DeviceProblem::new(
                    Recovery::Stop,
                    "this computer has no speaker the sound system can see",
                )
            })?,
            Choice::Device { id } => {
                let id: cpal::DeviceId = id.parse().map_err(|_| {
                    DeviceProblem::new(
                        Recovery::Rebuild,
                        "the speaker Zyris was told to use is not one this computer knows",
                    )
                })?;
                host.device_by_id(&id).ok_or_else(|| {
                    DeviceProblem::new(
                        Recovery::Rebuild,
                        "the speaker Zyris was told to use is not plugged in",
                    )
                })?
            }
        };

        // The probe that decides "absent" as well as the sample format to ask for. The same one
        // question, one answer rule `capture::open` follows — `default_output_device` answering
        // `Some` is not evidence that anything can be opened.
        let native = device.default_output_config().map_err(|error| classify(&error))?;
        let name = device.to_string();
        let rate = crate::tts::SAMPLE_RATE;
        let frame = render_frame(rate);

        let counters = Counters::new();

        // Mono first: the vocoder is mono and a backend that will take it saves the spreading.
        // A backend that insists on its own channel count gets it, and [`Fill`] spreads.
        //
        // **The queue is built inside the attempt, not before it.** A `Fill` owns the only
        // receiving end of it, and `build_output_stream` consumes the closure the `Fill` is
        // inside — so a refused attempt takes the queue with it and a second attempt needs its
        // own. Nothing here tries a *rate* other than the model's; see the module documentation
        // for why a wrong rate is refused rather than converted.
        let mut opened = None;
        let mut refusal = None;
        for channels in [1u16, native.channels()] {
            let config = cpal::StreamConfig {
                channels,
                sample_rate: rate,
                buffer_size: cpal::BufferSize::Default,
            };
            let (audio, from_caller) = mpsc::unbounded_channel();
            let (to_canceller, render) = mpsc::unbounded_channel();
            let fill = Fill::new(from_caller, to_canceller, frame, counters.clone());
            match start(&device, &config, native.sample_format(), fill, channels) {
                Ok(stream) => {
                    opened = Some((stream, config, audio, render));
                    break;
                }
                Err(problem) => refusal = Some(problem),
            }
        }
        let Some((stream, config, audio, render)) = opened else {
            return Err(refusal.unwrap_or_else(|| {
                DeviceProblem::new(Recovery::Stop, "this speaker would not open at any layout")
            }));
        };

        // Output streams come back stopped.
        stream.play().map_err(|error| classify(&error))?;

        Ok((
            Playback {
                stream,
                config,
                device: name,
                out: Speaker { audio, counters },
            },
            render,
        ))
    }

    /// The half of this that is `Send`, for whatever is doing the talking.
    ///
    /// **A `cpal::Stream` is not `Send` on every platform**, exactly as `capture::Capture` is
    /// not, so a `Playback` cannot be moved onto a task or held by anything that is. Everything
    /// a speaker is actually asked to do — queue audio, throw it away, say how far it got — is
    /// atomics and a channel, and that is what this hands out. The `Playback` itself stays on
    /// the thread that opened it, and dropping it is still what closes the device.
    pub fn handle(&self) -> Speaker {
        self.out.clone()
    }

    /// `playback - callback` as the backend last reported it: how far ahead of the speaker the
    /// callback is writing.
    ///
    /// [`Speaker::stream_delay`], which is where it is argued.
    pub fn stream_delay(&self) -> Option<Duration> {
        self.out.stream_delay()
    }

    /// The layout the stream was actually opened at.
    pub fn config(&self) -> &cpal::StreamConfig {
        &self.config
    }

    /// What the open device is called, for the window.
    pub fn device(&self) -> &str {
        &self.device
    }

    /// Stop the device without dropping the handle. Not every backend can.
    pub fn pause(&self) -> Result<(), DeviceProblem> {
        self.stream.pause().map_err(|error| classify(&error))
    }

    /// Start again after [`Self::pause`].
    pub fn resume(&self) -> Result<(), DeviceProblem> {
        self.stream.play().map_err(|error| classify(&error))
    }
}

// ---------------------------------------------------------------------------------------------
// The render side of the echo canceller
// ---------------------------------------------------------------------------------------------

/// What went to the speaker, on its way to the echo canceller's reference input.
///
/// # Which of the two resamplers this project could have had
///
/// `apm::analyze_render` takes exactly 160 samples of 16 kHz mono and **panics** on anything
/// else, while the vocoder produces 44.1 kHz. So something has to resample, and there were two
/// honest ways to arrange it:
///
/// 1. Resample once, to 16 kHz, and both play and reference from that. One resampler; the voice
///    loses everything above 8 kHz.
/// 2. Play the model's own rate and resample a second time, here, for the reference only. Two
///    resamplers; the voice is what the model made.
///
/// **This is (2)**, and the reason is that (1) pays a permanent cost to save a negligible one.
/// The output stream is already opened at [`crate::tts::SAMPLE_RATE`] because a voice at the
/// wrong rate is a different person — this module's opening paragraph — and 8 kHz of bandwidth
/// is the difference between a voice and a telephone. What (2) costs is one more `rubato` pass
/// over mono audio at a hundred frames a second, which `capture` already measured in the other
/// direction at **21.05 µs per 1024-frame stereo block**, and this one is mono and shorter. The
/// reference also does not need to sound like anything: it is a correlation input, and the
/// resampling either side of it is the same operation the microphone's own path already does.
///
/// # The tap's frames are not the canceller's frames
///
/// Task 1's note says a tap cut at 10 ms of the output stream "resamples to exactly one of its
/// frames whatever the output rate is". The *rate* arithmetic is right and the framing is not:
/// [`Conversion`] holds an FFT resampler whose input chunk is about 20 ms, fixed at
/// construction, so 441 samples in usually produces nothing and occasionally produces several
/// frames' worth at once. A [`Chunker`] after it is therefore not optional either, and this type
/// has both.
pub struct Render {
    apm: Arc<Apm>,
    conversion: Conversion,
    to_apm: Chunker,
    analysed: u64,
}

impl Render {
    /// One reference path from a stream running at `output_rate`.
    ///
    /// Fails only on a rate `rubato` will not convert from, which is the same failure
    /// [`crate::capture::Conversion::new`] has and arrives in the same shape.
    pub fn new(apm: Arc<Apm>, output_rate: u32) -> Result<Render, DeviceProblem> {
        Ok(Render {
            apm,
            // One channel: the tap carries the callback's mono, before it was spread.
            conversion: Conversion::new(output_rate, 1)?,
            to_apm: Chunker::new(crate::capture::APM_FRAME),
            analysed: 0,
        })
    }

    /// One tap frame. Answers how many canceller frames it completed.
    pub fn feed(&mut self, played: &[f32]) -> Result<usize, crate::apm::Fault> {
        let mut frames = 0;
        let converted = self.conversion.feed(played);
        for frame in self.to_apm.push(converted) {
            self.apm.analyze_render(frame)?;
            frames += 1;
        }
        self.analysed += frames as u64;
        Ok(frames)
    }

    /// How many frames have reached the canceller, ever.
    ///
    /// **The instrumentation that says whether the render side is running at all.** `erle_db`
    /// answers `Some` on an `aec` build that has never been shown a reference, so a figure near
    /// zero there is ambiguous between "nothing is playing" and "nothing is wired up"; this
    /// number is not.
    pub fn analysed(&self) -> u64 {
        self.analysed
    }

    /// Read the tap until the speaker goes away.
    ///
    /// A refused frame is logged and the loop goes on. The alternative — stopping — would leave
    /// the canceller with a reference that ends part way through a sentence, which is worse than
    /// one with a gap in it: AEC3 would go on subtracting an echo path it is no longer being
    /// told about.
    pub async fn run(mut self, mut tap: mpsc::UnboundedReceiver<Vec<f32>>) {
        while let Some(frame) = tap.recv().await {
            if let Err(fault) = self.feed(&frame) {
                tracing::warn!(%fault, "the echo canceller refused a frame of what was played");
            }
        }
    }
}

/// A speaker with no device behind it: the queue, the callback and the counters, and the samples
/// going nowhere.
///
/// **What a test drives instead of a sound card**, and the reason it is here rather than in a
/// test module: the rules barge-in depends on — what a discard does to `played`, what `pending`
/// says afterwards, where a fragment starts — live in [`Fill`] and [`Speaker`], and a double
/// standing in for them would be a second implementation of exactly the thing being decided. The
/// caller drives [`Fill::deliver`] itself, which is how many samples "reach the device" and when.
///
/// The third element is the render tap, which has to be kept or dropped: keeping it and never
/// reading it grows a buffer.
pub fn offline(frame: usize) -> (Speaker, Fill, mpsc::UnboundedReceiver<Vec<f32>>) {
    let (audio, from_caller) = mpsc::unbounded_channel();
    let (to_canceller, tap) = mpsc::unbounded_channel();
    let counters = Counters::new();
    let fill = Fill::new(from_caller, to_canceller, frame, counters.clone());
    (Speaker { audio, counters }, fill, tap)
}

/// Build one output stream at one layout.
fn start(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    format: cpal::SampleFormat,
    fill: Fill,
    channels: u16,
) -> Result<cpal::Stream, DeviceProblem> {
    // There is nowhere to report a mid-stream failure yet: both channels out of this module carry
    // audio. Task 4 gives it one. Until then a stream that fails goes quiet, which
    // `Playback::pending` shows as a number that stops moving.
    let on_error = |_: cpal::Error| {};

    // One arm per sample format, because Windows hands over whatever its mix format is. Only one
    // runs, so moving the same `Fill` in each is fine.
    macro_rules! build {
        ($ty:ty) => {
            device.build_output_stream::<$ty, _, _>(
                config.clone(),
                widen(fill, channels),
                on_error,
                Some(OPEN_TIMEOUT),
            )
        };
    }
    match format {
        cpal::SampleFormat::I8 => build!(i8),
        cpal::SampleFormat::I16 => build!(i16),
        cpal::SampleFormat::I32 => build!(i32),
        cpal::SampleFormat::U8 => build!(u8),
        cpal::SampleFormat::U16 => build!(u16),
        cpal::SampleFormat::U32 => build!(u32),
        cpal::SampleFormat::F32 => build!(f32),
        cpal::SampleFormat::F64 => build!(f64),
        // `SampleFormat` is `#[non_exhaustive]` and the 24-bit formats arrive in a wrapper type.
        // Refusing is honest, exactly as it is on the capture side.
        other => {
            return Err(DeviceProblem::new(
                Recovery::Stop,
                format!("this speaker plays {other}, which Zyris cannot write"),
            ));
        }
    }
    .map_err(|error| classify(&error))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rig(
        frame: usize,
    ) -> (mpsc::UnboundedSender<Vec<f32>>, mpsc::UnboundedReceiver<Vec<f32>>, Fill, Counters) {
        let (to_fill, audio) = mpsc::unbounded_channel();
        let (tap, render) = mpsc::unbounded_channel();
        let counters = Counters::new();
        let fill = Fill::new(audio, tap, frame, counters.clone());
        (to_fill, render, fill, counters)
    }

    fn drained(render: &mut mpsc::UnboundedReceiver<Vec<f32>>) -> Vec<Vec<f32>> {
        let mut out = Vec::new();
        while let Ok(frame) = render.try_recv() {
            out.push(frame);
        }
        out
    }

    /// A fragment is never a whole number of callbacks, and a callback is never a whole number of
    /// fragments. Every sample comes out once, in order, whatever the two lengths are.
    #[test]
    fn a_fragment_crosses_callbacks_and_callbacks_cross_fragments() {
        let (to_fill, mut render, mut fill, counters) = rig(441);
        // Every sample is inside [-1, 1]: `sane` clamps, so a ramp of plain integers would come
        // out as fifteen hundred copies of 1.0 and prove nothing about the ordering.
        let ramp = |from: usize, to: usize| -> Vec<f32> {
            (from..to).map(|i| i as f32 / 1500.0 - 0.5).collect()
        };
        to_fill.send(ramp(0, 1000)).expect("queued");
        to_fill.send(ramp(1000, 1500)).expect("queued");

        let mut heard = Vec::new();
        // The lengths `cpal` has actually handed this project, on the other side of the same API.
        for callback in [1024usize, 314, 341, 342] {
            let mut out = vec![-1.0f32; callback];
            fill.deliver(&mut out, 1, None);
            heard.extend_from_slice(&out);
        }
        let wanted = ramp(0, 1500);
        assert_eq!(&heard[..1500], &wanted[..], "the samples came out in order");
        assert!(heard[1500..].iter().all(|&s| s == 0.0), "and then silence");
        assert_eq!(counters.played(), 1500);
        assert!(!drained(&mut render).is_empty());
    }

    /// An empty queue is silence, not noise and not a failure — but it is **counted**.
    #[test]
    fn an_empty_queue_is_silence_and_is_counted() {
        let (to_fill, _render, mut fill, _counters) = rig(441);
        let mut out = vec![7.0f32; 64];
        fill.deliver(&mut out, 1, None);
        assert!(out.iter().all(|&s| s == 0.0), "the caller's buffer was not left as it was");
        assert_eq!(fill.starved(), 1);

        to_fill.send(vec![0.5; 64]).expect("queued");
        let mut out = vec![0.0f32; 64];
        fill.deliver(&mut out, 1, None);
        assert_eq!(fill.starved(), 1, "a callback that had enough is not starving");
    }

    /// Mono goes to every channel. A voice on the left only is a bug a mono fixture cannot see —
    /// the same case `capture`'s downmix test had to be rewritten for.
    #[test]
    fn one_voice_reaches_every_channel() {
        let (to_fill, _render, mut fill, counters) = rig(441);
        to_fill.send(vec![0.25, 0.5, 0.75]).expect("queued");
        let mut out = vec![0.0f32; 6];
        fill.deliver(&mut out, 2, None);
        assert_eq!(out, vec![0.25, 0.25, 0.5, 0.5, 0.75, 0.75]);
        assert_eq!(counters.played(), 3, "three frames, not six samples");
    }

    /// A buffer that is not a whole number of frames leaves no stale samples behind it.
    #[test]
    fn a_partial_frame_is_written_as_silence_and_not_left_alone() {
        let (to_fill, _render, mut fill, _counters) = rig(441);
        to_fill.send(vec![0.5; 8]).expect("queued");
        let mut out = vec![9.0f32; 5];
        fill.deliver(&mut out, 2, None);
        assert_eq!(out, vec![0.5, 0.5, 0.5, 0.5, 0.0], "the odd slot is silence, not 9.0");
    }

    /// **The tap is what the speaker emitted**, in fixed frames, silence included.
    ///
    /// The frames matter because the echo canceller panics rather than erroring on a wrong count;
    /// the silence matters because a reference with the gaps removed drifts against the
    /// microphone by the length of every gap, and a drifting canceller cancels nothing while
    /// reporting a figure.
    #[test]
    fn the_tap_carries_exactly_what_was_played_in_fixed_frames() {
        let (to_fill, mut render, mut fill, _counters) = rig(441);
        to_fill.send(vec![0.5; 700]).expect("queued");

        let mut out = vec![0.0f32; 1024];
        fill.deliver(&mut out, 1, None);
        let frames = drained(&mut render);

        assert_eq!(frames.len(), 1024 / 441, "whole frames only; the rest is held");
        for frame in &frames {
            assert_eq!(frame.len(), 441);
        }
        let tapped: Vec<f32> = frames.concat();
        assert_eq!(&tapped[..], &out[..tapped.len()], "the tap is the buffer, sample for sample");
        assert!(tapped[700..].iter().all(|&s| s == 0.0), "the silence is in the reference too");
    }

    /// A sample that is not a number never reaches a speaker. `capture`'s rule, in the other
    /// direction and for the same reason: an infinity has no loudness and a `NaN` has no sign.
    #[test]
    fn nothing_that_is_not_a_number_reaches_the_device() {
        let (to_fill, mut render, mut fill, _counters) = rig(4);
        to_fill.send(vec![f32::NAN, f32::INFINITY, 4.0, -4.0]).expect("queued");
        let mut out = vec![0.0f32; 4];
        fill.deliver(&mut out, 1, None);
        assert_eq!(out, vec![0.0, 0.0, 1.0, -1.0]);
        assert_eq!(drained(&mut render), vec![vec![0.0, 0.0, 1.0, -1.0]], "the tap is clean too");
    }

    /// An empty fragment must not stop the stream. A filter that removed everything from a delta
    /// can produce one, and a `while` loop that treated it as "the queue is empty" would spin.
    #[test]
    fn an_empty_fragment_is_stepped_over() {
        let (to_fill, _render, mut fill, _counters) = rig(441);
        to_fill.send(Vec::new()).expect("queued");
        to_fill.send(vec![0.5; 4]).expect("queued");
        let mut out = vec![0.0f32; 4];
        fill.deliver(&mut out, 1, None);
        assert_eq!(out, vec![0.5; 4]);
    }

    /// **The timestamps are kept**, which is all task 4 asks of task 1.
    ///
    /// `OutputCallbackInfo` *can* be constructed — see the module documentation, which is where
    /// the plan's claim to the contrary is corrected — so this goes through [`widen`] and the
    /// real `cpal` type rather than around them.
    #[test]
    fn the_delay_the_backend_reports_survives_the_callback() {
        let (to_fill, _render, fill, counters) = rig(441);
        to_fill.send(vec![0.5; 16]).expect("queued");
        let mut callback = widen::<f32>(fill, 1);

        let at = cpal::StreamInstant::ZERO + Duration::from_millis(500);
        let info = cpal::OutputCallbackInfo::new(OutputStreamTimestamp {
            callback: at,
            playback: at + Duration::from_millis(23),
        });
        let mut out = vec![0.0f32; 16];
        callback(&mut out, &info);

        assert_eq!(counters.delay.load(Ordering::Relaxed), Duration::from_millis(23).as_nanos() as u64);
        assert_eq!(out, vec![0.5; 16], "and the audio still got through");
    }

    /// A backend that predicts playback before the callback is saying "nothing I can measure",
    /// not "negative latency" — and an underflow here would be a panic in an audio callback.
    #[test]
    fn a_backwards_timestamp_is_no_delay_rather_than_a_panic() {
        let (_to_fill, _render, mut fill, counters) = rig(441);
        let at = cpal::StreamInstant::ZERO + Duration::from_millis(500);
        let mut out = vec![0.0f32; 8];
        fill.deliver(
            &mut out,
            1,
            Some(OutputStreamTimestamp { callback: at, playback: at - Duration::from_millis(10) }),
        );
        assert_eq!(counters.delay.load(Ordering::Relaxed), 0);
    }

    /// Until the backend has said anything, the delay is **unknown** rather than zero. Task 4
    /// hands this to a canceller, and a confident zero is worse than no answer.
    #[test]
    fn an_unreported_delay_is_not_zero() {
        let delay = Arc::new(AtomicU64::new(UNKNOWN_DELAY));
        assert_eq!(UNKNOWN_DELAY, u64::MAX);
        assert!(matches!(
            match delay.load(Ordering::Relaxed) {
                UNKNOWN_DELAY => None,
                nanos => Some(Duration::from_nanos(nanos)),
            },
            None
        ));
    }

    /// **One sentence, out loud, on the real device.** Ignored, because CI has no speaker and a
    /// machine running the suite should not start talking.
    ///
    /// Run it with the models in place:
    ///
    /// ```text
    /// ZYRIS_TTS_MODELS=… cargo test -p zyris-voice --features voice \
    ///     --lib playback::tests::one_sentence_out_of_a_real_speaker -- --ignored --nocapture
    /// ```
    ///
    /// What it proves that nothing else here can: that the 44.1 kHz **mono** request is granted
    /// — the module's whole reason for having no resampler — and that the tap really carries
    /// what was played while it is playing.
    #[test]
    #[ignore = "opens the real speaker and makes a noise"]
    fn one_sentence_out_of_a_real_speaker() {
        let Some(dir) = crate::tts::models_dir().filter(|dir| {
            matches!(crate::tts::state_in(dir), crate::tts::VoiceState::Ready { .. })
        }) else {
            eprintln!("skipped: no Supertonic models; set {}", crate::tts::MODELS_ENV);
            return;
        };
        let mut tts =
            crate::tts::Tts::load(&dir, crate::tts::DEFAULT_VOICE).expect("the models load");
        let said = tts.say("Zyris is listening.").expect("it speaks");

        let (playback, mut tap) = Playback::open(&Choice::Default).expect("a speaker opens");
        eprintln!(
            "speaker {:?} at {} Hz, {} channel(s)",
            playback.device(),
            playback.config().sample_rate,
            playback.config().channels
        );
        assert_eq!(
            playback.config().sample_rate,
            crate::tts::SAMPLE_RATE,
            "the rate is the model's or nothing"
        );

        let wanted = said.samples.len() as u64;
        let out = playback.handle();
        assert!(out.speak(said.samples).is_some());
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while out.pending() > 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(out.pending(), 0, "the speaker never took it");

        let mut tapped = 0usize;
        while let Ok(frame) = tap.try_recv() {
            assert_eq!(frame.len(), render_frame(playback.config().sample_rate));
            tapped += frame.len();
        }
        assert!(
            tapped as u64 >= wanted,
            "the tap carried {tapped} of {wanted} samples the device was given"
        );
        eprintln!("delay {:?}", playback.stream_delay());
    }

    /// Ten milliseconds, whatever the stream rate turns out to be — because that is the frame the
    /// echo canceller takes, and it is fixed at 16 kHz on its side.
    #[test]
    fn the_tap_frame_is_ten_milliseconds_of_whatever_rate_this_is() {
        assert_eq!(render_frame(crate::tts::SAMPLE_RATE), 441);
        assert_eq!(render_frame(48_000), 480);
        assert_eq!(render_frame(16_000), crate::capture::APM_FRAME);
        assert_eq!(render_frame(0), 1, "a chunk of no samples is a panic in `Chunker::new`");
    }
}

#[cfg(test)]
mod stopping_and_the_reference {
    use super::*;

    /// One callback of `frames` samples, mono, with no timestamp.
    fn callback(fill: &mut Fill, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0f32; frames];
        fill.deliver(&mut out, 1, None);
        out
    }

    /// **The offset is the speaker's own answer, not a count kept beside it.**
    ///
    /// A ledger of what was queued is read against `played` later, and the two have to be in the
    /// same coordinates. A caller keeping its own running total would agree with this until the
    /// first refused `speak`, and nothing would go red when they stopped agreeing.
    #[test]
    fn a_fragment_is_told_where_in_the_stream_it_starts() {
        let (speaker, _fill, _tap) = offline(441);

        assert_eq!(speaker.speak(vec![0.1; 100]), Some(0));
        assert_eq!(speaker.speak(vec![0.2; 250]), Some(100));
        assert_eq!(speaker.speak(vec![0.3; 7]), Some(350));
        assert_eq!(speaker.pending(), 357);
    }

    /// **The whole of what stopping is**, and the discriminator is the third assertion: audio
    /// thrown away must not be counted as audio played, or an interruption would report that
    /// the person heard every sentence that had been queued for them.
    #[test]
    fn silencing_throws_the_queue_away_and_does_not_call_it_played() {
        let (speaker, mut fill, _tap) = offline(441);
        speaker.speak(vec![0.5; 1000]).expect("queued");
        speaker.speak(vec![0.5; 2000]).expect("queued");

        // 600 samples reach the device, out of 3000.
        callback(&mut fill, 600);
        assert_eq!(speaker.played(), 600);

        speaker.silence();
        // The discard happens in the callback, because the queue's receiving end is in there.
        callback(&mut fill, 600);

        assert_eq!(speaker.played(), 600, "nothing more reached the device");
        assert_eq!(speaker.counters().discarded(), 2400, "and the rest was thrown away");
        assert_eq!(speaker.pending(), 0, "so nothing is still waiting to be spoken");
    }

    /// A speaker that was silenced goes on working. The next answer is not a new stream.
    #[test]
    fn a_speaker_that_was_silenced_speaks_again() {
        let (speaker, mut fill, _tap) = offline(441);
        speaker.speak(vec![0.5; 1000]).expect("queued");
        speaker.silence();
        callback(&mut fill, 32);

        let at = speaker.speak(vec![0.25; 64]).expect("queued");
        let out = callback(&mut fill, 64);

        assert_eq!(at, 1000, "the stream's coordinates do not restart");
        assert_eq!(out, vec![0.25; 64]);
        assert_eq!(speaker.played(), 64);
    }

    /// Silencing an empty speaker is not an error and costs nothing. A key pressed between
    /// answers goes through this path every time.
    #[test]
    fn silencing_a_speaker_that_is_saying_nothing_does_nothing() {
        let (speaker, mut fill, _tap) = offline(441);
        speaker.silence();
        callback(&mut fill, 128);

        assert_eq!(speaker.played(), 0);
        assert_eq!(speaker.counters().discarded(), 0);
        assert_eq!(speaker.pending(), 0);
    }

    // -----------------------------------------------------------------------------------------
    // The render reference
    // -----------------------------------------------------------------------------------------

    fn ramp(len: usize, from: usize) -> Vec<f32> {
        (from..from + len).map(|i| (i % 97) as f32 / 200.0).collect()
    }

    /// **The tap's frame is not the canceller's frame**, which is what makes the chunker inside
    /// [`Render`] load-bearing rather than tidy.
    ///
    /// `render_frame(44_100)` is 441 samples — 10 ms — and the note task 1 left says that
    /// resamples to "exactly one" 160-sample frame. The rate arithmetic is right and the framing
    /// is not: `Conversion` holds an FFT resampler with a fixed input chunk of about 20 ms, so
    /// the first tap frames produce nothing at all. The assertion is on that, because a `Render`
    /// written to the note would hand `analyze_render` whatever came back and the library would
    /// panic on the length.
    #[test]
    fn one_ten_millisecond_tap_frame_is_not_one_canceller_frame() {
        let apm = Arc::new(Apm::new().expect("a processor at the capture rate"));
        let mut render = Render::new(apm, crate::tts::SAMPLE_RATE).expect("44.1 kHz is ordinary");

        let first = render.feed(&ramp(441, 0)).expect("a frame the canceller accepts");

        assert_eq!(first, 0, "the resampler is still filling its first chunk");
        assert_eq!(render.analysed(), 0);
    }

    /// One second of what was played arrives as **one second of the canceller's frames**, at its
    /// rate, whatever the speaker's rate is.
    ///
    /// 100 frames of 160 samples is 16,000 samples is one second. The tolerance is one frame,
    /// which is what the resampler holds at the end; anything bigger is a rate that is wrong,
    /// and a rate that is wrong is a reference the canceller cannot line up against the
    /// microphone — which shows up as an echo canceller that silently does nothing.
    #[test]
    fn a_second_of_the_speaker_is_a_second_of_the_canceller() {
        let apm = Arc::new(Apm::new().expect("a processor at the capture rate"));
        let rate = crate::tts::SAMPLE_RATE;
        let mut render = Render::new(apm, rate).expect("44.1 kHz is ordinary");

        let frame = render_frame(rate);
        for i in 0..100 {
            render.feed(&ramp(frame, i * frame)).expect("a frame the canceller accepts");
        }

        let wanted = crate::capture::SAMPLE_RATE as u64 / crate::capture::APM_FRAME as u64;
        assert!(
            render.analysed().abs_diff(wanted) <= 1,
            "one second of 44.1 kHz playback should be {wanted} canceller frames, not {}",
            render.analysed()
        );
    }

    /// A speaker already at the canceller's rate needs no resampler, and the path still has to
    /// work: `Conversion` answers `None` for a matching rate and the chunker does the rest.
    #[test]
    fn a_speaker_at_the_cancellers_own_rate_still_goes_through_the_chunker() {
        let apm = Arc::new(Apm::new().expect("a processor at the capture rate"));
        let rate = crate::capture::SAMPLE_RATE;
        let mut render = Render::new(apm, rate).expect("16 kHz is ordinary");

        // 250 samples is not a multiple of 160, which is the point.
        for i in 0..8 {
            render.feed(&ramp(250, i * 250)).expect("a frame the canceller accepts");
        }

        assert_eq!(render.analysed(), 2000 / crate::capture::APM_FRAME as u64);
    }

    /// The pump ends when the speaker does, rather than leaving a task on the runtime holding
    /// an `Arc` on a processor the next session has replaced.
    #[tokio::test]
    async fn the_render_pump_ends_with_the_speaker() {
        let apm = Arc::new(Apm::new().expect("a processor at the capture rate"));
        let render = Render::new(apm, crate::tts::SAMPLE_RATE).expect("44.1 kHz is ordinary");
        let (tap, frames) = mpsc::unbounded_channel();

        let pump = tokio::spawn(render.run(frames));
        tap.send(vec![0.0; 441]).expect("the pump is reading");
        drop(tap);

        tokio::time::timeout(Duration::from_secs(5), pump)
            .await
            .expect("the pump has to end when the tap closes, or a stopped session leaks a task")
            .expect("the pump does not panic");
    }
}
