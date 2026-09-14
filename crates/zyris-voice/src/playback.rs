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

use crate::capture::{DeviceProblem, Recovery, classify, sane};
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

/// The sentinel [`Playback::stream_delay`] reads as "the backend has not said yet".
const UNKNOWN: u64 = u64::MAX;

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
    /// Samples taken off the queue and handed over. Shared, so the sending side can say how much
    /// is still to come without a second channel.
    played: Arc<AtomicU64>,
    /// `playback - callback` from the last callback, in nanoseconds, or [`UNKNOWN`].
    delay: Arc<AtomicU64>,
    /// Callbacks that ran out of audio part-way. See the module documentation.
    starved: u64,
}

impl Fill {
    /// `frame` is [`render_frame`] for the stream's rate.
    pub fn new(
        audio: mpsc::UnboundedReceiver<Vec<f32>>,
        render: mpsc::UnboundedSender<Vec<f32>>,
        frame: usize,
        played: Arc<AtomicU64>,
        delay: Arc<AtomicU64>,
    ) -> Fill {
        Fill {
            audio,
            front: Vec::new(),
            at: 0,
            mono: Vec::new(),
            chunker: crate::capture::Chunker::new(frame),
            render,
            played,
            delay,
            starved: 0,
        }
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
            let delay = at.playback.saturating_duration_since(at.callback);
            self.delay.store(delay.as_nanos().min(u128::from(UNKNOWN - 1)) as u64, Ordering::Relaxed);
        }

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

/// An open speaker.
///
/// **Dropping this stops playback**, exactly as dropping a [`crate::capture::Capture`] stops the
/// microphone: `cpal` closes the stream in its own `Drop`.
pub struct Playback {
    stream: cpal::Stream,
    config: cpal::StreamConfig,
    device: String,
    audio: mpsc::UnboundedSender<Vec<f32>>,
    /// Samples handed to [`Self::speak`], ever.
    sent: AtomicU64,
    /// Samples the callback has taken off the queue, ever.
    played: Arc<AtomicU64>,
    delay: Arc<AtomicU64>,
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

        let played = Arc::new(AtomicU64::new(0));
        let delay = Arc::new(AtomicU64::new(UNKNOWN));

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
            let fill = Fill::new(from_caller, to_canceller, frame, played.clone(), delay.clone());
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
                audio,
                sent: AtomicU64::new(0),
                played,
                delay,
            },
            render,
        ))
    }

    /// Hand one fragment to the speaker. `false` means the stream has gone.
    pub fn speak(&self, samples: Vec<f32>) -> bool {
        self.sent.fetch_add(samples.len() as u64, Ordering::Relaxed);
        self.audio.send(samples).is_ok()
    }

    /// How many samples are queued and not yet handed to the device.
    ///
    /// The only honest answer to "is it still talking?" from this side. It is **not** "how much
    /// is still audible": the device holds another [`Self::stream_delay`] of it.
    pub fn pending(&self) -> u64 {
        self.sent.load(Ordering::Relaxed).saturating_sub(self.played.load(Ordering::Relaxed))
    }

    /// `playback - callback` as the backend last reported it: how far ahead of the speaker the
    /// callback is writing.
    ///
    /// **Task 4 needs this and task 1 records it**, which is why it is here before anything reads
    /// it: `webrtc-audio-processing` wants a `stream_delay_ms` that is this plus the input side's
    /// `callback - capture`, and a canceller given the wrong delay cancels nothing while
    /// reporting that it is working.
    pub fn stream_delay(&self) -> Option<Duration> {
        match self.delay.load(Ordering::Relaxed) {
            UNKNOWN => None,
            nanos => Some(Duration::from_nanos(nanos)),
        }
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

    fn rig(frame: usize) -> (mpsc::UnboundedSender<Vec<f32>>, mpsc::UnboundedReceiver<Vec<f32>>, Fill, Arc<AtomicU64>, Arc<AtomicU64>) {
        let (to_fill, audio) = mpsc::unbounded_channel();
        let (tap, render) = mpsc::unbounded_channel();
        let played = Arc::new(AtomicU64::new(0));
        let delay = Arc::new(AtomicU64::new(UNKNOWN));
        let fill = Fill::new(audio, tap, frame, played.clone(), delay.clone());
        (to_fill, render, fill, played, delay)
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
        let (to_fill, mut render, mut fill, played, _) = rig(441);
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
        assert_eq!(played.load(Ordering::Relaxed), 1500);
        assert!(!drained(&mut render).is_empty());
    }

    /// An empty queue is silence, not noise and not a failure — but it is **counted**.
    #[test]
    fn an_empty_queue_is_silence_and_is_counted() {
        let (to_fill, _render, mut fill, _, _) = rig(441);
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
        let (to_fill, _render, mut fill, played, _) = rig(441);
        to_fill.send(vec![0.25, 0.5, 0.75]).expect("queued");
        let mut out = vec![0.0f32; 6];
        fill.deliver(&mut out, 2, None);
        assert_eq!(out, vec![0.25, 0.25, 0.5, 0.5, 0.75, 0.75]);
        assert_eq!(played.load(Ordering::Relaxed), 3, "three frames, not six samples");
    }

    /// A buffer that is not a whole number of frames leaves no stale samples behind it.
    #[test]
    fn a_partial_frame_is_written_as_silence_and_not_left_alone() {
        let (to_fill, _render, mut fill, _, _) = rig(441);
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
        let (to_fill, mut render, mut fill, _, _) = rig(441);
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
        let (to_fill, mut render, mut fill, _, _) = rig(4);
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
        let (to_fill, _render, mut fill, _, _) = rig(441);
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
        let (to_fill, _render, fill, _, delay) = rig(441);
        to_fill.send(vec![0.5; 16]).expect("queued");
        let mut callback = widen::<f32>(fill, 1);

        let at = cpal::StreamInstant::ZERO + Duration::from_millis(500);
        let info = cpal::OutputCallbackInfo::new(OutputStreamTimestamp {
            callback: at,
            playback: at + Duration::from_millis(23),
        });
        let mut out = vec![0.0f32; 16];
        callback(&mut out, &info);

        assert_eq!(delay.load(Ordering::Relaxed), Duration::from_millis(23).as_nanos() as u64);
        assert_eq!(out, vec![0.5; 16], "and the audio still got through");
    }

    /// A backend that predicts playback before the callback is saying "nothing I can measure",
    /// not "negative latency" — and an underflow here would be a panic in an audio callback.
    #[test]
    fn a_backwards_timestamp_is_no_delay_rather_than_a_panic() {
        let (_to_fill, _render, mut fill, _, delay) = rig(441);
        let at = cpal::StreamInstant::ZERO + Duration::from_millis(500);
        let mut out = vec![0.0f32; 8];
        fill.deliver(
            &mut out,
            1,
            Some(OutputStreamTimestamp { callback: at, playback: at - Duration::from_millis(10) }),
        );
        assert_eq!(delay.load(Ordering::Relaxed), 0);
    }

    /// Until the backend has said anything, the delay is **unknown** rather than zero. Task 4
    /// hands this to a canceller, and a confident zero is worse than no answer.
    #[test]
    fn an_unreported_delay_is_not_zero() {
        let delay = Arc::new(AtomicU64::new(UNKNOWN));
        assert_eq!(UNKNOWN, u64::MAX);
        assert!(matches!(
            match delay.load(Ordering::Relaxed) {
                UNKNOWN => None,
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
        assert!(playback.speak(said.samples));
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while playback.pending() > 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(playback.pending(), 0, "the speaker never took it");

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
