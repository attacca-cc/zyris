//! The Windows echo canceller: the operating system's own Voice Capture DSP.
//!
//! `webrtc-audio-processing` cannot be built for Windows — upstream issue #34 has been open
//! since 2023-09-27, the CI matrix is `[ubuntu-latest, macos-latest]`, and the MSVC pull request
//! is unmerged and unverified at runtime by its own author. So the `aec` feature is Linux's, and
//! the platform this product ships an `.exe` for needs a different canceller. This is it, and it
//! is the one that was already on the machine: `C:\Windows\System32\mfwmaaec.dll`, shipped with
//! Windows since Vista.
//!
//! # Which of the three routes this is, and what the other two would have cost
//!
//! 1. **`IAcousticEchoCancellationControl` is not a canceller.** Its one method,
//!    `SetEchoCancellationRenderEndpoint`, chooses *which* render endpoint the operating
//!    system's own AEC treats as the reference; the cancelling happens in `audiodg.exe` either
//!    way, and only on Windows 11 ≥ 22000. Reaching it needs the `IAudioClient` behind the
//!    capture stream, and **`cpal` 0.18.2 does not expose one** (`GetDefaultAudioEndpoint(flow,
//!    eConsole)` at `src/host/wasapi/device.rs:1159`, roles compared against `eConsole` at
//!    `stream.rs:127`). So it costs a second Windows-only capture path or a patched `cpal`, and
//!    on the default endpoint this program already plays to it buys **nothing**, because that
//!    endpoint is the reference the operating system picks anyway. It is worth having the day
//!    somebody plays to a non-default device, and not before.
//! 2. **This.** A cancellation stage rather than a pointer at one, back to Vista rather than
//!    only on Windows 11, and its AEC-only format is 16 kHz 16-bit mono —
//!    [`crate::capture::SAMPLE_RATE`] exactly, so nothing here resamples for whisper. Every
//!    piece of COM plumbing is generated: `#[implement(IMediaBuffer)]` writes the vtable.
//! 3. **`aec-rs` 1.0.0** vendors speexdsp, a 2000s MDF canceller **with no nonlinear residual
//!    suppressor**. Adequate against a headset and wrong against a loudspeaker, which is this
//!    product's case, and it would have been a vendored C build on both platforms for a worse
//!    result than the operating system gives away. It is the pre-Vista floor, and there is no
//!    pre-Vista.
//!
//! # Filter mode, and why it is not source mode
//!
//! The DSP has two shapes, and `GetStreamCount` is how you tell them apart — measured on
//! Windows 11 build 26200, 2026-09-15:
//!
//! | `MFPKEY_WMAAECMA_DMO_SOURCE_MODE` | input streams | output streams |
//! |---|---|---|
//! | true (**the default**) | 0 | 1 |
//! | false | **2** | 1 |
//!
//! In source mode the DSP opens the microphone and the render endpoint itself. That is less code
//! and it gets device-clock drift right for free — and it was the first thing built here — but
//! it replaces the capture path wholesale, and the endpoints it opens are the system defaults:
//! `MFPKEY_WMAAECMA_DEVICE_INDEXES` takes the old `waveIn`/`waveOut` ordinal and nothing `cpal`
//! hands out. **The Voice screen's device picker would quietly stop meaning anything**, which is
//! the kind of control that lies that this project keeps refusing to ship.
//!
//! Filter mode takes the microphone on stream 0 and the loudspeaker reference on stream 1 and
//! gives back the cleaned microphone. That is the shape [`crate::apm::Apm`] already has on
//! Linux — `process_capture` and `analyze_render` — so `cpal` keeps the microphone, the picker
//! keeps working, and [`crate::playback::Render`]'s tap keeps its purpose.
//!
//! And it is the shape that can be **measured**: filter mode touches no device, so
//! `AllocateStreamingResources` succeeds on a machine with no sound card and synthetic audio can
//! be run through the real canceller. See [`tests`].
//!
//! # What was and was not verified
//!
//! Built and tested on Windows 11 build 26200, 2026-09-15, with the real `mfwmaaec.dll`:
//! **56.19 dB of a synthetic echo removed with the reference fed, against 3.73 dB with silence
//! fed in its place**, and no falling away over twenty seconds.
//!
//! **That is still not the same claim as "echo cancellation works."** The echo path is the
//! reference delayed 20 ms and halved and nothing else in the microphone: **no room, no
//! reflections, no non-linearity, no near-end speech**. Nobody has held a microphone in front of
//! a loudspeaker, and this is not wired into the capture path yet — see "What is left" below.
//!
//! # What is left, precisely
//!
//! [`Apm`] is an `Arc` shared by the capture thread and the render thread, with `&self` methods,
//! because `webrtc-audio-processing`'s `Processor` is `Send + Sync`. **A COM object is not**: it
//! belongs to the apartment of the thread that created it, and [`Dsp::process`] wants both
//! frames in one call. So the remaining wiring is a ring buffer from
//! [`crate::playback::Render`]'s tap to the capture thread, and a `Dsp` owned by that thread —
//! not a field on `Apm`. That is the whole of it, and it is why this module is tested and not
//! yet called.
//!
//! # Where the constants came from
//!
//! win32metadata does not carry `DEFINE_PROPERTYKEY`, so `windows-rs` has no `MFPKEY_WMAAECMA_*`
//! and the plan expected these to be guessed. They are not: every one is transcribed from
//! `C:\Program Files (x86)\Windows Kits\10\Include\10.0.26100.0\um\wmcodecdsp.h` on a real
//! Windows machine, and the three media GUIDs from `wmsdkidl.h` beside it. The header's own text
//! is quoted at each, and the suite checks the transcription rather than trusting it.

use std::cell::{Cell, RefCell};
use std::marker::PhantomData;

use windows::Win32::Foundation::{E_INVALIDARG, PROPERTYKEY, VARIANT_BOOL, VARIANT_TRUE};
use windows::Win32::Media::Audio::WAVEFORMATEX;
use windows::Win32::Media::DxMediaObjects::{
    DMO_MEDIA_TYPE, DMO_OUTPUT_DATA_BUFFER, IMediaBuffer, IMediaBuffer_Impl, IMediaObject,
};
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::Variant::{VT_BOOL, VT_I4};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
// `windows-core` is a direct dependency of this crate as well as `windows`, and only because
// `#[implement]` writes `::windows_core::` into what it generates — an absolute crate path that
// an alias cannot satisfy. It is the same compiled copy these names come from.
use windows::core::{ComObject, GUID, HRESULT, Interface, implement};

use crate::capture::{DeviceProblem, MICROPHONE_PRIVACY, Recovery, SAMPLE_RATE};

/// `C:\Windows\System32\mfwmaaec.dll`, registered as `AEC`.
///
/// Read out of `HKEY_CLASSES_ROOT\CLSID\{745057C7-F353-4F2D-A7EE-58434477730E}` on the machine
/// rather than copied from a web page: the registry is what `CoCreateInstance` consults, so it
/// is the only source that cannot be stale.
pub const CWMAUDIOAEC: GUID = GUID::from_u128(0x745057c7_f353_4f2d_a7ee_58434477730e);

/// `{ 0x6f52c567, 0x360, 0x4bd2, { 0x96, 0x17, 0xcc, 0xbf, 0x14, 0x21, 0xc9, 0x39 } }` —
/// the one `fmtid` every `MFPKEY_WMAAECMA_*` key in `wmcodecdsp.h` shares.
pub const WMAAECMA: GUID = GUID::from_u128(0x6f52c567_0360_4bd2_9617_ccbf1421c939);

/// `#define PID_FIRST_USABLE 2`, from `propkeydef.h` (and `( 0x2 )` in `PropIdl.h`).
///
/// Every key in the header is written `PID_FIRST_USABLE + n`, so getting this wrong shifts all
/// twelve by the same amount — the failure that would look like the DSP ignoring its
/// configuration rather than like a bug.
const PID_FIRST_USABLE: u32 = 2;

/// The `n`th `MFPKEY_WMAAECMA_*` key, in the header's own order.
const fn key(n: u32) -> PROPERTYKEY {
    PROPERTYKEY { fmtid: WMAAECMA, pid: PID_FIRST_USABLE + n }
}

/// `MFPKEY_WMAAECMA_SYSTEM_MODE` — which of the six processing graphs to run. `VT_I4`.
pub const SYSTEM_MODE: PROPERTYKEY = key(0);
/// `MFPKEY_WMAAECMA_DMO_SOURCE_MODE` — whether the DSP opens the devices itself. `VT_BOOL`.
///
/// **True is the default**, and this module sets it false. The module documentation has the
/// stream counts that distinguish the two and the argument for the one taken.
pub const DMO_SOURCE_MODE: PROPERTYKEY = key(1);
/// `MFPKEY_WMAAECMA_DEVICE_INDEXES` — which two endpoints, in source mode. `VT_I4`.
///
/// **Never set here**, and it could not usefully be: it takes the old `waveIn`/`waveOut`
/// ordinal, which is not anything `cpal` hands out, and in filter mode the DSP opens nothing.
pub const DEVICE_INDEXES: PROPERTYKEY = key(2);
/// `MFPKEY_WMAAECMA_FEATURE_MODE` — whether the `FEATR_*` keys below are consulted at all.
///
/// `VT_BOOL`. The documentation calls it the switch that makes the other six do anything, and
/// this module sets it — but **with the values set here it changes nothing that can be
/// measured**, and neither does its position among them.
///
/// Measured on Windows 11 build 26200, 2026-09-15, by running eight seconds of quiet noise
/// through the real DSP: the output is 0.002546 RMS with this key set first, with it set last,
/// and with it not set at all. Turning noise suppression off and the gain controller on moves
/// that to 0.009923 **only when this key is set** — so it is real, and the reason it is
/// invisible here is that the DSP's own defaults already are what this module asks for. It is
/// set anyway, for the reason `apm.rs` gives about its high-pass filter: a line that says what
/// is wanted rather than what happens to follow becomes load-bearing the day a default moves.
pub const FEATURE_MODE: PROPERTYKEY = key(3);
/// `MFPKEY_WMAAECMA_FEATR_FRAME_SIZE` — the DSP's internal frame, in samples. `VT_I4`.
pub const FEATR_FRAME_SIZE: PROPERTYKEY = key(4);
/// `MFPKEY_WMAAECMA_FEATR_ECHO_LENGTH` — the tail the canceller models, in samples. `VT_I4`.
pub const FEATR_ECHO_LENGTH: PROPERTYKEY = key(5);
/// `MFPKEY_WMAAECMA_FEATR_NS` — noise suppression. `VT_I4`, 0 off / 1 on.
pub const FEATR_NS: PROPERTYKEY = key(6);
/// `MFPKEY_WMAAECMA_FEATR_AGC` — automatic gain control. `VT_BOOL`.
pub const FEATR_AGC: PROPERTYKEY = key(7);
/// `MFPKEY_WMAAECMA_FEATR_AES` — acoustic echo *suppression*, the nonlinear stage. `VT_I4`,
/// 0 / 1 / 2 passes.
///
/// **This is the reason route 3 was refused.** `aec-rs`'s speexdsp has a linear canceller and
/// nothing after it, so whatever survives the linear stage — against a loudspeaker in a room,
/// most of it — is what the microphone hears.
pub const FEATR_AES: PROPERTYKEY = key(8);
/// `MFPKEY_WMAAECMA_FEATR_VAD` — the DSP's own voice activity detector. `VT_I4`.
pub const FEATR_VAD: PROPERTYKEY = key(9);
/// `MFPKEY_WMAAECMA_FEATR_CENTER_CLIP` — centre clipping of the residual. `VT_BOOL`.
pub const FEATR_CENTER_CLIP: PROPERTYKEY = key(10);
/// `MFPKEY_WMAAECMA_FEATR_NOISE_FILL` — comfort noise over the cancelled gaps. `VT_BOOL`.
pub const FEATR_NOISE_FILL: PROPERTYKEY = key(11);

/// `SINGLE_CHANNEL_AEC = 0`, from the `SYSTEM_MODE` enumeration in `wmcodecdsp.h`.
///
/// The five after it are microphone-array modes (`ADAPTIVE_ARRAY_ONLY`, `OPTIBEAM_ARRAY_ONLY`,
/// `ADAPTIVE_ARRAY_AND_AEC`, `OPTIBEAM_ARRAY_AND_AEC`, `SINGLE_CHANNEL_NSAGC`). A laptop or a
/// headset is one channel, and an array mode on a single-channel device fails at
/// `AllocateStreamingResources` rather than working worse.
pub const SINGLE_CHANNEL_AEC: i32 = 0;

/// `#define WMAAECMA_E_NO_ACTIVE_RENDER_STREAM 0x87CC000A`, from `wmcodecdsp.h`.
///
/// The DSP saying it has nothing to cancel against. It belongs to source mode, where the DSP
/// watches the render endpoint itself; filter mode is handed a reference and cannot raise it.
/// [`classify`] answers [`Recovery::Retry`] because on a machine with speakers it clears the
/// moment something plays.
pub const E_NO_ACTIVE_RENDER_STREAM: HRESULT = HRESULT(0x87CC_000Au32 as i32);

/// `HRESULT_FROM_WIN32(ERROR_NOT_FOUND)`.
///
/// **Measured, not assumed** (Windows 11 build 26200, 2026-09-15): a machine with no audio
/// hardware at all — `Get-PnpDevice -Class AudioEndpoint` returns nothing, `Win32_SoundDevice`
/// is empty, both audio services running — fails `AllocateStreamingResources` in **source mode**
/// with `0x80070490`, and not with [`E_NO_ACTIVE_RENDER_STREAM`]. The two are different
/// conditions: this one is "there is no device", that one is "the device is idle", and only the
/// second clears on its own. Filter mode on the same machine allocates fine.
pub const E_NOT_FOUND: HRESULT = HRESULT(0x8007_0490u32 as i32);

/// `MEDIATYPE_Audio`, spelled `WMMEDIATYPE_Audio` in `wmsdkidl.h`:
/// `0x73647561, 0x0000, 0x0010, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71`.
pub const MEDIATYPE_AUDIO: GUID = GUID::from_u128(0x73647561_0000_0010_8000_00aa00389b71);
/// `MEDIASUBTYPE_PCM`, spelled `WMMEDIASUBTYPE_PCM` in `wmsdkidl.h`:
/// `0x00000001, 0x0000, 0x0010, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71`.
pub const MEDIASUBTYPE_PCM: GUID = GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71);
/// `FORMAT_WaveFormatEx`, spelled `WMFORMAT_WaveFormatEx` in `wmsdkidl.h`:
/// `0x05589f81, 0xc356, 0x11ce, 0xbf, 0x01, 0x00, 0xaa, 0x00, 0x55, 0x59, 0x5a`.
pub const FORMAT_WAVE_FORMAT_EX: GUID = GUID::from_u128(0x05589f81_c356_11ce_bf01_00aa0055595a);

/// `WAVE_FORMAT_PCM`, from `mmreg.h`. One, and it has been one since 1991.
const WAVE_FORMAT_PCM: u16 = 1;

/// Bytes per sample on every one of the three streams: 16-bit.
const BYTES_PER_SAMPLE: usize = 2;

/// The frame this takes and gives back: 10 ms at 16 kHz.
///
/// The same length as [`crate::capture::APM_FRAME`], and deliberately — a Windows canceller that
/// wanted a different frame from the Linux one would put a second chunker in the capture path.
pub const FRAME: usize = crate::capture::APM_FRAME;

/// The format of all three streams: 16 kHz, 16-bit, mono.
///
/// **Not a choice — it is what the AEC-only mode produces**, and it happens to be exactly what
/// whisper wants, so this path has no `rubato` in it at all. `capture.rs` resamples because
/// `cpal`'s WASAPI backend sets `AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM` on output streams only;
/// there is no such stream here.
pub fn wave_format() -> WAVEFORMATEX {
    let channels: u16 = 1;
    let block_align = channels * BYTES_PER_SAMPLE as u16;
    WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM,
        nChannels: channels,
        nSamplesPerSec: SAMPLE_RATE,
        nAvgBytesPerSec: SAMPLE_RATE * u32::from(block_align),
        nBlockAlign: block_align,
        wBitsPerSample: (BYTES_PER_SAMPLE * 8) as u16,
        // Zero, and it must be: `WAVE_FORMAT_PCM` is the one tag for which `cbSize` is defined
        // to be ignored, and a non-zero one would have the DSP read past the struct.
        cbSize: 0,
    }
}

/// A `VT_I4` property value.
fn i4(value: i32) -> PROPVARIANT {
    let mut variant = PROPVARIANT::default();
    variant.Anonymous.Anonymous = std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
        vt: VT_I4,
        wReserved1: 0,
        wReserved2: 0,
        wReserved3: 0,
        Anonymous: PROPVARIANT_0_0_0 { lVal: value },
    });
    variant
}

/// A `VT_BOOL` property value.
///
/// **`VARIANT_TRUE` is `-1`, not `1`**, and a DSP handed `1` reads it as true anyway — which is
/// why the mistake is invisible until something else compares against `VARIANT_TRUE`. The
/// constant is taken from `windows-rs` rather than written out.
fn vbool(value: bool) -> PROPVARIANT {
    let mut variant = PROPVARIANT::default();
    variant.Anonymous.Anonymous = std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
        vt: VT_BOOL,
        wReserved1: 0,
        wReserved2: 0,
        wReserved3: 0,
        Anonymous: PROPVARIANT_0_0_0 {
            boolVal: if value { VARIANT_TRUE } else { VARIANT_BOOL(0) },
        },
    });
    variant
}

/// What a failed COM call means to somebody who has to do something about it.
///
/// The same three-field [`DeviceProblem`] `capture.rs` hands the window, deliberately: the
/// Windows canceller is a second way for the microphone to be unavailable, not a second shape
/// of bad news.
pub fn classify(code: HRESULT) -> DeviceProblem {
    // `windows-rs` renders this as "Access is denied. (0x80070005)"; the sentence a person needs
    // is about the privacy switch, not about the HRESULT.
    const E_ACCESSDENIED: HRESULT = HRESULT(0x8007_0005u32 as i32);
    match code {
        E_NO_ACTIVE_RENDER_STREAM => DeviceProblem {
            recovery: Recovery::Retry,
            reason: "Nothing is playing, so the echo canceller has no reference to work from."
                .to_string(),
            settings: None,
        },
        E_NOT_FOUND => DeviceProblem {
            recovery: Recovery::Rebuild,
            reason: "Windows has no microphone and speaker for the echo canceller to use."
                .to_string(),
            settings: None,
        },
        E_ACCESSDENIED => DeviceProblem {
            recovery: Recovery::Stop,
            reason: "Windows is not letting this program use the microphone.".to_string(),
            settings: Some(MICROPHONE_PRIVACY.to_string()),
        },
        other => DeviceProblem {
            recovery: Recovery::Rebuild,
            reason: format!(
                "The Windows echo canceller refused: {}",
                windows::core::Error::from(other).message()
            ),
            settings: None,
        },
    }
}

/// One block of 16-bit audio, on its way into or out of the DSP.
///
/// `#[implement]` generates the whole `IMediaBuffer` vtable, so there is no hand-written COM
/// here — which was the part the plan expected to cost something and does not.
#[implement(IMediaBuffer)]
struct Buffer {
    /// Fixed at construction. `GetMaxLength` answers its capacity and it never reallocates: the
    /// DSP holds the pointer `GetBufferAndLength` gave it across the call, so a `Vec` that grew
    /// would leave it writing into freed memory.
    bytes: RefCell<Vec<u8>>,
    /// How many bytes of `bytes` are meant.
    used: Cell<u32>,
}

impl Buffer {
    fn with_capacity(capacity: usize) -> ComObject<Buffer> {
        ComObject::new(Buffer { bytes: RefCell::new(vec![0u8; capacity]), used: Cell::new(0) })
    }

    /// Fill it with samples on their way in, clipped into 16-bit.
    ///
    /// **The clipping is the language's**: a float-to-integer `as` cast has saturated rather
    /// than wrapped since Rust 1.45, so there is no `clamp` here and there must not be one --
    /// it would be a clause no test could decide. What matters is that a sample past 1.0 does
    /// not become full scale of the opposite sign, which is a click the canceller would then
    /// have to model, and the test below is on that and not on the absent clamp.
    fn fill(&self, samples: &[f32]) {
        let mut bytes = self.bytes.borrow_mut();
        let count = samples.len().min(bytes.len() / BYTES_PER_SAMPLE);
        for (slot, sample) in bytes.chunks_exact_mut(BYTES_PER_SAMPLE).zip(&samples[..count]) {
            // Rounded, not truncated: truncation is a half-step of bias on every sample in one
            // direction, and the round trip test below is tight enough to tell the two apart.
            let scaled = (sample * 32_767.0).round() as i16;
            slot.copy_from_slice(&scaled.to_le_bytes());
        }
        self.used.set((count * BYTES_PER_SAMPLE) as u32);
    }

    /// The bytes the DSP last wrote, as samples scaled into `[-1, 1)`.
    fn samples(&self) -> Vec<f32> {
        let bytes = self.bytes.borrow();
        // `used` comes from the DSP. Clamping rather than trusting it is the difference between
        // a bad frame and a panic on the capture thread.
        let used = (self.used.get() as usize).min(bytes.len());
        bytes[..used]
            .chunks_exact(BYTES_PER_SAMPLE)
            .map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32_768.0)
            .collect()
    }
}

impl IMediaBuffer_Impl for Buffer_Impl {
    fn SetLength(&self, cblength: u32) -> windows::core::Result<()> {
        if cblength as usize > self.bytes.borrow().len() {
            // The interface's own contract: longer than the buffer is `E_INVALIDARG`, not a
            // silent truncation. Truncating would make a too-long write indistinguishable from
            // a short one.
            return Err(E_INVALIDARG.into());
        }
        self.used.set(cblength);
        Ok(())
    }

    fn GetMaxLength(&self) -> windows::core::Result<u32> {
        Ok(self.bytes.borrow().len() as u32)
    }

    fn GetBufferAndLength(
        &self,
        ppbuffer: *mut *mut u8,
        pcblength: *mut u32,
    ) -> windows::core::Result<()> {
        // Both are optional in the contract, and the DSP does ask for one without the other.
        if ppbuffer.is_null() && pcblength.is_null() {
            return Err(E_INVALIDARG.into());
        }
        if !ppbuffer.is_null() {
            unsafe { *ppbuffer = self.bytes.borrow_mut().as_mut_ptr() };
        }
        if !pcblength.is_null() {
            unsafe { *pcblength = self.used.get() };
        }
        Ok(())
    }
}

/// Whether this thread's COM apartment was ours to set up, and so ours to tear down.
struct Apartment {
    ours: bool,
}

impl Apartment {
    /// `RPC_E_CHANGED_MODE` means somebody already put this thread in a single-threaded
    /// apartment. That is not a failure — the DSP is `ThreadingModel = Both` — but it does mean
    /// the apartment is not ours to uninitialise, and calling `CoUninitialize` on somebody
    /// else's is how a process loses COM halfway through its life.
    fn enter() -> Apartment {
        const RPC_E_CHANGED_MODE: HRESULT = HRESULT(0x8001_0106u32 as i32);
        let code = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        Apartment { ours: code != RPC_E_CHANGED_MODE }
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.ours {
            unsafe { CoUninitialize() };
        }
    }
}

/// The Voice Capture DSP in filter mode: microphone in, loudspeaker reference in, cleaned
/// microphone out.
///
/// # Why it is not `Send`
///
/// A COM object belongs to the apartment of the thread that created it, and the apartment is
/// torn down in [`Drop`], so a `Dsp` that moved would uninitialise one it never entered. It is
/// not shareable either, which is the whole of what the module documentation calls "what is
/// left": [`crate::apm::Apm`] is an `Arc` with `&self` methods and this cannot be a field on it.
pub struct Dsp {
    object: IMediaObject,
    microphone: ComObject<Buffer>,
    loudspeaker: ComObject<Buffer>,
    cleaned: ComObject<Buffer>,
    /// Declared after everything above so that it is dropped after them: releasing a COM
    /// interface once its apartment is gone is undefined, and Rust drops fields in declaration
    /// order.
    _apartment: Apartment,
    _not_send: PhantomData<*const ()>,
}

impl Dsp {
    /// Create the DSP, configure it, and take its streaming resources.
    ///
    /// **This touches no audio device**, which is what filter mode buys: it succeeds on a
    /// machine with no sound card, and that is where it was tested.
    pub fn open() -> Result<Dsp, DeviceProblem> {
        let apartment = Apartment::enter();
        let object: IMediaObject =
            unsafe { CoCreateInstance(&CWMAUDIOAEC, None, CLSCTX_INPROC_SERVER) }
                .map_err(|error| classify(error.code()))?;
        let store: IPropertyStore = object.cast().map_err(|error| classify(error.code()))?;

        // Order matters twice over. `SYSTEM_MODE` chooses the graph, so it is first; and
        // `FEATURE_MODE` has to be true *before* any `FEATR_*` key, or the DSP keeps its own
        // defaults and every setting after it is accepted and ignored.
        let settings = [
            (SYSTEM_MODE, i4(SINGLE_CHANNEL_AEC)),
            // False is filter mode: two input streams, and this program keeps its microphone.
            // See the module documentation for the count that distinguishes the two.
            (DMO_SOURCE_MODE, vbool(false)),
            (FEATURE_MODE, vbool(true)),
            // On. The consumer is whisper, and `apm.rs` argues the same way about the same
            // decision on Linux.
            (FEATR_NS, i4(1)),
            // Off, exactly as on Linux: a gain controller pushes samples past the [-1, 1] the
            // detector documents, and whisper normalises its own input.
            (FEATR_AGC, vbool(false)),
            // One pass. Two is documented for harder rooms and costs latency; nobody has heard
            // either yet, so the honest setting is the default one and not the brave one.
            (FEATR_AES, i4(1)),
            // Off. `earshot` decides when somebody stopped talking, and two detectors
            // disagreeing is worse than one.
            (FEATR_VAD, i4(0)),
        ];
        for (key, value) in &settings {
            unsafe { store.SetValue(key, value) }.map_err(|error| classify(error.code()))?;
        }

        let format = wave_format();
        let media_type = DMO_MEDIA_TYPE {
            majortype: MEDIATYPE_AUDIO,
            subtype: MEDIASUBTYPE_PCM,
            bFixedSizeSamples: true.into(),
            bTemporalCompression: false.into(),
            lSampleSize: BYTES_PER_SAMPLE as u32,
            formattype: FORMAT_WAVE_FORMAT_EX,
            pUnk: std::mem::ManuallyDrop::new(None),
            cbFormat: std::mem::size_of::<WAVEFORMATEX>() as u32,
            // `format` is still alive for the whole of this function, which is why the media
            // type is built here rather than returned from a helper: it borrows the format.
            pbFormat: std::ptr::from_ref(&format).cast::<u8>().cast_mut(),
        };
        // Stream 0 is the microphone and stream 1 is the loudspeaker. Getting these the wrong
        // way round gives a canceller that subtracts the room from the answer.
        for stream in [MICROPHONE_STREAM, LOUDSPEAKER_STREAM] {
            unsafe { object.SetInputType(stream, Some(&media_type), 0) }
                .map_err(|error| classify(error.code()))?;
        }
        unsafe { object.SetOutputType(0, Some(&media_type), 0) }
            .map_err(|error| classify(error.code()))?;
        unsafe { object.AllocateStreamingResources() }
            .map_err(|error| classify(error.code()))?;

        let bytes = FRAME * BYTES_PER_SAMPLE;
        Ok(Dsp {
            object,
            microphone: Buffer::with_capacity(bytes),
            loudspeaker: Buffer::with_capacity(bytes),
            // The DSP does not have to answer one frame for one frame, so the buffer it writes
            // into is larger than the one it is fed.
            cleaned: Buffer::with_capacity(bytes * 4),
            _apartment: apartment,
            _not_send: PhantomData,
        })
    }

    /// One frame of microphone and one of loudspeaker in, whatever cleaned audio is ready out.
    ///
    /// Both must be [`FRAME`] samples. **An empty answer is ordinary**: the DSP buffers, so the
    /// first calls produce nothing and later ones produce more than one frame's worth.
    pub fn process(
        &mut self,
        microphone: &[f32],
        loudspeaker: &[f32],
    ) -> Result<Vec<f32>, DeviceProblem> {
        self.microphone.get().fill(microphone);
        self.loudspeaker.get().fill(loudspeaker);
        for (stream, buffer) in [
            (MICROPHONE_STREAM, &self.microphone),
            (LOUDSPEAKER_STREAM, &self.loudspeaker),
        ] {
            unsafe {
                self.object.ProcessInput(
                    stream,
                    &buffer.to_interface::<IMediaBuffer>(),
                    0,
                    0,
                    0,
                )
            }
            .map_err(|error| classify(error.code()))?;
        }

        // Reset before the call, not after: a `ProcessOutput` that writes nothing leaves the
        // previous frame's length in place, and the caller would be handed the same audio twice
        // with nothing to say so.
        self.cleaned.get().used.set(0);
        let mut buffers = [DMO_OUTPUT_DATA_BUFFER {
            pBuffer: std::mem::ManuallyDrop::new(Some(self.cleaned.to_interface::<IMediaBuffer>())),
            dwStatus: 0,
            rtTimestamp: 0,
            rtTimelength: 0,
        }];
        let mut status = 0u32;
        let outcome = unsafe { self.object.ProcessOutput(0, &mut buffers, &mut status) };
        // The interface it was handed is ours to release, whichever way the call went.
        unsafe { std::mem::ManuallyDrop::drop(&mut buffers[0].pBuffer) };
        outcome.map_err(|error| classify(error.code()))?;
        Ok(self.cleaned.get().samples())
    }
}

/// The microphone goes in here.
pub const MICROPHONE_STREAM: u32 = 0;
/// And what the loudspeaker is about to play, here.
pub const LOUDSPEAKER_STREAM: u32 = 1;

impl Drop for Dsp {
    fn drop(&mut self) {
        let _ = unsafe { self.object.FreeStreamingResources() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The twelve keys differ only in `pid`, and the header writes each as `PID_FIRST_USABLE +
    /// n` in this order. A transposition is what this catches, and it is the one that would look
    /// like the DSP ignoring its configuration rather than like a bug.
    #[test]
    fn every_property_key_is_the_header_s_own_arithmetic() {
        let keys = [
            SYSTEM_MODE,
            DMO_SOURCE_MODE,
            DEVICE_INDEXES,
            FEATURE_MODE,
            FEATR_FRAME_SIZE,
            FEATR_ECHO_LENGTH,
            FEATR_NS,
            FEATR_AGC,
            FEATR_AES,
            FEATR_VAD,
            FEATR_CENTER_CLIP,
            FEATR_NOISE_FILL,
        ];
        for (n, key) in keys.iter().enumerate() {
            assert_eq!(key.fmtid, WMAAECMA, "key {n} is in the wrong property set");
            assert_eq!(key.pid, 2 + n as u32, "key {n} has the wrong pid");
        }
    }

    /// Against the headers' own text rather than against the constants themselves.
    ///
    /// `{ 0x6f52c567, 0x360, 0x4bd2, { 0x96, 0x17, 0xcc, 0xbf, 0x14, 0x21, 0xc9, 0x39 } }` —
    /// note the `0x360`, three digits in C and four here.
    #[test]
    fn the_transcribed_guids_are_what_the_headers_say() {
        assert_eq!(format!("{WMAAECMA:?}"), "6F52C567-0360-4BD2-9617-CCBF1421C939");
        assert_eq!(format!("{CWMAUDIOAEC:?}"), "745057C7-F353-4F2D-A7EE-58434477730E");
        assert_eq!(format!("{MEDIATYPE_AUDIO:?}"), "73647561-0000-0010-8000-00AA00389B71");
        assert_eq!(format!("{MEDIASUBTYPE_PCM:?}"), "00000001-0000-0010-8000-00AA00389B71");
        assert_eq!(format!("{FORMAT_WAVE_FORMAT_EX:?}"), "05589F81-C356-11CE-BF01-00AA0055595A");
    }

    #[test]
    fn the_format_is_what_whisper_wants_and_needs_no_resampler() {
        // Read into locals first: `WAVEFORMATEX` is `#[repr(packed)]`, so a reference to any
        // field of it — which `assert_eq!` takes — is a compile error and not a lint.
        let format = wave_format();
        let (rate, channels, bits) =
            (format.nSamplesPerSec, format.nChannels, format.wBitsPerSample);
        let (align, bytes_per_second, extra) =
            (format.nBlockAlign, format.nAvgBytesPerSec, format.cbSize);
        assert_eq!(rate, crate::capture::SAMPLE_RATE);
        assert_eq!(channels, 1);
        assert_eq!(bits, 16);
        // The two that are arithmetic rather than choices, and the two a hand-written
        // `WAVEFORMATEX` gets wrong: a DSP handed an inconsistent block alignment reads the
        // buffer at the wrong stride and produces noise rather than an error.
        assert_eq!(align, 2);
        assert_eq!(bytes_per_second, 32_000);
        assert_eq!(extra, 0);
        // And the frame is the Linux one, so the capture path needs no second chunker.
        assert_eq!(FRAME, crate::capture::APM_FRAME);
    }

    /// `VARIANT_TRUE` is `-1`. A `1` here is accepted by the DSP and is still wrong.
    #[test]
    fn a_true_property_value_is_minus_one_and_not_one() {
        let variant = vbool(true);
        unsafe {
            assert_eq!(variant.Anonymous.Anonymous.vt, VT_BOOL);
            assert_eq!(variant.Anonymous.Anonymous.Anonymous.boolVal.0, -1);
        }
        let variant = vbool(false);
        unsafe { assert_eq!(variant.Anonymous.Anonymous.Anonymous.boolVal.0, 0) };
    }

    #[test]
    fn an_integer_property_value_carries_its_type_and_its_number() {
        let variant = i4(SINGLE_CHANNEL_AEC);
        unsafe {
            assert_eq!(variant.Anonymous.Anonymous.vt, VT_I4);
            assert_eq!(variant.Anonymous.Anonymous.Anonymous.lVal, 0);
        }
        let variant = i4(7);
        unsafe { assert_eq!(variant.Anonymous.Anonymous.Anonymous.lVal, 7) };
    }

    /// Through the interface, not through the struct: this is what says the generated vtable is
    /// wired to these bodies at all.
    #[test]
    fn the_buffer_answers_the_dsp_through_the_generated_vtable() {
        let object = Buffer::with_capacity(8);
        let iface = object.to_interface::<IMediaBuffer>();

        assert_eq!(unsafe { iface.GetMaxLength() }.unwrap(), 8);

        let mut pointer: *mut u8 = std::ptr::null_mut();
        let mut length: u32 = 0;
        unsafe { iface.GetBufferAndLength(Some(&mut pointer), Some(&mut length)) }.unwrap();
        assert!(!pointer.is_null());
        assert_eq!(length, 0, "nothing has been written yet");

        // What the DSP does: write into the pointer, then say how far it got.
        unsafe { std::ptr::copy_nonoverlapping([0x00u8, 0x40, 0x00, 0xc0].as_ptr(), pointer, 4) };
        unsafe { iface.SetLength(4) }.unwrap();

        let mut length: u32 = 0;
        unsafe { iface.GetBufferAndLength(None, Some(&mut length)) }.unwrap();
        assert_eq!(length, 4, "a missing buffer pointer must still answer the length");
        assert_eq!(object.get().samples(), vec![0.5, -0.5]);
    }

    /// The contract, and the mutation that matters: truncating instead of refusing would make a
    /// too-long write indistinguishable from a short one.
    #[test]
    fn a_length_past_the_end_is_refused_rather_than_truncated() {
        let object = Buffer::with_capacity(8);
        let iface = object.to_interface::<IMediaBuffer>();
        assert!(unsafe { iface.SetLength(9) }.is_err());
        assert_eq!(object.get().used.get(), 0, "a refused length must change nothing");
        assert!(unsafe { iface.SetLength(8) }.is_ok());
    }

    /// A length the DSP could not have meant is clamped rather than trusted: the alternative is
    /// a panic on the capture thread.
    #[test]
    fn a_length_past_the_end_is_clamped_on_the_way_out() {
        let object = Buffer::with_capacity(8);
        object.get().used.set(4_000);
        assert_eq!(object.get().samples().len(), 4);
    }

    /// What goes in comes back, to within the step of a 16-bit sample.
    ///
    /// Two mutations are here for. The scale factor: halving it costs 6 dB of everything reaching
    /// whisper, and the cancellation measurement below cannot see it because it scales the
    /// microphone and the reference together. And the rounding: the tolerance is tighter than a
    /// truncated conversion's worst case and looser than a rounded one's.
    #[test]
    fn a_frame_survives_the_round_trip_through_sixteen_bits() {
        let object = Buffer::with_capacity(FRAME * BYTES_PER_SAMPLE);
        let sent: Vec<f32> = (0..FRAME).map(|n| n as f32 / FRAME as f32 - 0.5).collect();
        object.get().fill(&sent);
        for (back, was) in object.get().samples().iter().zip(&sent) {
            assert!((back - was).abs() < 1.0 / 40_000.0, "{back} is not {was}");
        }
    }

    /// More samples than the buffer holds is the DSP reading past the end of it, and there is
    /// nothing downstream that would notice.
    #[test]
    fn a_frame_longer_than_the_buffer_does_not_claim_the_extra() {
        let object = Buffer::with_capacity(8);
        object.get().fill(&[0.1; 64]);
        assert_eq!(object.get().used.get(), 8);
    }

    /// Full scale in and full scale out, and nothing wrapping in between.
    ///
    /// It is Rust's saturating `as` that does this, so what the test pins is the behaviour and
    /// not a line of ours -- adding a `clamp` in front of it is a mutation nothing can catch,
    /// which is why there is not one.
    #[test]
    fn a_sample_past_full_scale_clips_rather_than_wrapping() {
        let object = Buffer::with_capacity(4);
        object.get().fill(&[2.0, -2.0]);
        let back = object.get().samples();
        assert!(back[0] > 0.99, "{back:?}");
        assert!(back[1] < -0.99, "{back:?}");
    }

    /// The one HRESULT that is not a failure of this program, the one that is a missing device,
    /// and the one a person has to act on — three different sentences because they are three
    /// different conditions.
    #[test]
    fn nothing_playing_and_no_device_and_no_permission_are_three_answers() {
        let quiet = classify(E_NO_ACTIVE_RENDER_STREAM);
        assert_eq!(quiet.recovery, Recovery::Retry);
        assert!(quiet.settings.is_none());

        let absent = classify(E_NOT_FOUND);
        assert_eq!(absent.recovery, Recovery::Rebuild);
        assert_ne!(absent.reason, quiet.reason);
        // And not the catch-all, which is a sentence about COM rather than about a machine
        // with no sound card. Deleting the arm leaves the recovery right and the sentence
        // useless, which is the mutation with nothing else to catch it.
        assert!(!absent.reason.contains("refused"), "{}", absent.reason);

        let denied = classify(HRESULT(0x8007_0005u32 as i32));
        assert_eq!(denied.recovery, Recovery::Stop);
        assert_eq!(denied.settings.as_deref(), Some(MICROPHONE_PRIVACY));
        assert!(
            !denied.reason.contains("0x80070005"),
            "the sentence is for a person, not a log line"
        );
        // And it says what is actually wrong. A permission problem is not a missing device,
        // and a sentence that swapped the two would send somebody to buy a microphone.
        assert!(!denied.reason.contains("no microphone"), "{}", denied.reason);
    }

    /// **The test that could not be written before there was a Windows machine.**
    ///
    /// It creates the real `mfwmaaec.dll`, hands its property store all seven values, sets three
    /// media types and allocates. A key from the wrong property set is refused, and so is a
    /// `pid` that is not one of the twelve — see the control below.
    #[test]
    fn the_real_dsp_takes_this_configuration() {
        Dsp::open().expect("the DSP is part of Windows and filter mode needs no device");
    }

    /// The control for the test above: if the property store accepted anything at all, that
    /// test would pass with the constants transcribed wrongly and prove nothing.
    #[test]
    fn the_property_store_refuses_a_key_that_is_not_its_own() {
        let _apartment = Apartment::enter();
        let object: IMediaObject =
            unsafe { CoCreateInstance(&CWMAUDIOAEC, None, CLSCTX_INPROC_SERVER) }.unwrap();
        let store: IPropertyStore = object.cast().unwrap();
        let elsewhere = PROPERTYKEY { fmtid: GUID::from_u128(0x0), pid: 2 };
        assert!(
            unsafe { store.SetValue(&elsewhere, &i4(0)) }.is_err(),
            "if this passes, the test above is not a discriminator and has to be replaced"
        );
    }

    /// Filter mode's own shape, and the discriminator for [`DMO_SOURCE_MODE`].
    ///
    /// Source mode reports 0 inputs and 1 output; filter mode reports **2 and 1**. So setting
    /// that property the wrong way round — the mutation with no other symptom, since both modes
    /// configure and allocate without complaint — fails here.
    #[test]
    fn filter_mode_takes_two_streams_in_and_gives_one_back() {
        let dsp = Dsp::open().expect("the DSP takes this configuration");
        let (mut inputs, mut outputs) = (0u32, 0u32);
        unsafe { dsp.object.GetStreamCount(&mut inputs, &mut outputs) }.unwrap();
        assert_eq!((inputs, outputs), (2, 1));
    }

    /// Entering an apartment somebody else already made is not ours to leave.
    ///
    /// The mutation: `ours` always true. Nothing else in the suite would notice, and what it
    /// costs in the product is a `CoUninitialize` against a thread that is still using COM.
    #[test]
    fn an_apartment_somebody_else_made_is_not_ours_to_leave() {
        std::thread::spawn(|| {
            use windows::Win32::System::Com::COINIT_APARTMENTTHREADED;
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok().unwrap();
            assert!(!Apartment::enter().ours, "a changed mode is somebody else's apartment");
            unsafe { CoUninitialize() };
        })
        .join()
        .unwrap();
        assert!(Apartment::enter().ours, "a thread with no apartment yet is ours");
    }

    /// **The measurement, in the shape task 4 settled on**: the same audio twice, changing only
    /// whether the loudspeaker reference is the truth or silence.
    ///
    /// The control is not optional. Noise suppression takes broadband noise out whatever the
    /// reference is doing, so a single run showing the microphone getting quieter says nothing
    /// about echo cancellation. The echo path is synthetic — the reference delayed 20 ms and
    /// halved, and nothing else in the microphone — and **is not a room**: no reflections, no
    /// non-linearity, no near-end speech.
    #[test]
    fn feeding_the_reference_is_what_makes_the_canceller_cancel() {
        let seconds = 4;
        let frames = seconds * SAMPLE_RATE as usize / FRAME;
        let reference = speech_shaped(frames * FRAME);
        let delay = SAMPLE_RATE as usize / 50; // 20 ms
        let microphone: Vec<f32> = (0..reference.len())
            .map(|n| if n < delay { 0.0 } else { reference[n - delay] * 0.5 })
            .collect();

        let truth = run(&microphone, &reference);
        let silence = run(&microphone, &vec![0.0; reference.len()]);

        // Measured on Windows 11 build 26200, 2026-09-15: 56.19 dB with the reference and
        // 3.73 dB with silence. The bars are 30 and 10 — far from those figures, and far
        // enough from each other that a canceller doing nothing cannot pass both.
        assert!(truth > 30.0, "with the reference: {truth:.2} dB");
        assert!(silence < 10.0, "with silence for a reference: {silence:.2} dB");
    }

    /// **The six-second cliff does not happen here.**
    ///
    /// On Linux, `webrtc-audio-processing` stops cancelling after about six seconds of any
    /// synthetic stimulus: it takes 20 to 30 dB out through second 5 and 0.4 dB out after it,
    /// at the same place for white noise, for speech-shaped bursts and for `jfk.wav`. Task 4 left
    /// that uncharacterised and sized its test around it. Over twenty seconds of the same
    /// stimulus this DSP goes 46, 47, 49, 55, 55, 63, 63, 64, 66, 66, 66, 66, 66, 66, 66, 66,
    /// 66, 67, 70, 70 dB: it converges and stays. So the cliff is a property of that
    /// implementation and not of a perfectly-delayed synthetic echo, which is the reading task
    /// 4 could not choose between.
    #[test]
    fn cancellation_does_not_fall_away_after_six_seconds() {
        let seconds = 10;
        let frames = seconds * SAMPLE_RATE as usize / FRAME;
        let reference = speech_shaped(frames * FRAME);
        let delay = SAMPLE_RATE as usize / 50;
        let microphone: Vec<f32> = (0..reference.len())
            .map(|n| if n < delay { 0.0 } else { reference[n - delay] * 0.5 })
            .collect();
        let mut dsp = Dsp::open().expect("filter mode needs no device");
        let mut out = Vec::new();
        for frame in 0..frames {
            let at = frame * FRAME..(frame + 1) * FRAME;
            out.extend(dsp.process(&microphone[at.clone()], &reference[at]).unwrap());
        }
        let second = SAMPLE_RATE as usize;
        let late = 10.0
            * (energy(&microphone[8 * second..9 * second])
                / energy(&out[8 * second..9 * second]).max(1e-12))
                .log10();
        assert!(late > 30.0, "the ninth second still cancels: {late:.2} dB");
    }

    /// One run of the canceller, answering how many dB of the microphone came out.
    fn run(microphone: &[f32], loudspeaker: &[f32]) -> f64 {
        let mut dsp = Dsp::open().expect("filter mode needs no device");
        let mut out = Vec::new();
        for frame in 0..microphone.len() / FRAME {
            let at = frame * FRAME..(frame + 1) * FRAME;
            out.extend(dsp.process(&microphone[at.clone()], &loudspeaker[at]).unwrap());
        }
        // Skip the first second either side: the canceller has to converge and the comparison is
        // about the steady state.
        let skip = SAMPLE_RATE as usize;
        10.0 * (energy(&microphone[skip..]) / energy(&out[skip.min(out.len())..]).max(1e-12)).log10()
    }

    fn energy(samples: &[f32]) -> f64 {
        samples.iter().map(|s| f64::from(*s) * f64::from(*s)).sum::<f64>()
            / samples.len().max(1) as f64
    }

    /// Noise gated into bursts, which is closer to speech than a tone and is what task 4 used.
    fn speech_shaped(count: usize) -> Vec<f32> {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        (0..count)
            .map(|n| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let noise = (state >> 40) as f32 / 8_388_608.0 - 0.5;
                // 300 ms on, 200 ms off.
                let gate = if (n / (SAMPLE_RATE as usize / 2)) % 2 == 0 { 0.4 } else { 0.02 };
                noise * gate
            })
            .collect()
    }
}
