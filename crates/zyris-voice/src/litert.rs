//! LiteRT's C API on Android: whisper's compiled graphs on the NPU, behind [`npu::Graphs`].
//!
//! **Loaded, not linked.** `libLiteRt.so` is opened with `libloading` the first time the NPU is
//! chosen, so the app keeps `minSdk` 26 and a phone that never chooses it never touches LiteRT.
//!
//! **Every buffer is created once, when a graph opens.** The phase 0 spike created, registered
//! with the DSP and destroyed about 30 MB of buffers on every decoder step, which was a third of
//! its per-token time. The self-attention cache is the decoder's own input buffers 2 and 3: a step
//! writes one slot into them, and nothing else moves between steps.
#![allow(
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    dead_code
)]

#[allow(unsafe_op_in_unsafe_fn, clippy::all)]
mod sys {
    include!(concat!(env!("OUT_DIR"), "/litert.rs"));
}

use crate::npu::{self, Graphs};
use crate::stt::Fault;
use std::ffi::CString;
use std::path::Path;
use std::sync::OnceLock;
use sys::*;

fn fault(detail: impl std::fmt::Display) -> Fault {
    Fault::Whisper {
        detail: detail.to_string(),
    }
}

/// `libLiteRt.so`, opened once per process.
fn lib() -> Result<&'static LiteRtLib, Fault> {
    static LIB: OnceLock<Result<LiteRtLib, String>> = OnceLock::new();
    LIB.get_or_init(|| {
        unsafe { LiteRtLib::new("libLiteRt.so") }.map_err(|e| format!("libLiteRt.so: {e}"))
    })
    .as_ref()
    .map_err(fault)
}

fn check(status: LiteRtStatus, what: &str) -> Result<(), Fault> {
    if status == kLiteRtStatusOk {
        Ok(())
    } else {
        Err(fault(format!("LiteRT {what}: status {status}")))
    }
}

/// A LiteRT environment whose NPU dispatch library is in `dispatch_dir`.
struct Env {
    handle: LiteRtEnvironment,
    _dir: CString,
}

// Owned by one `LiteRtGraphs`, used from one thread at a time behind `OnnxStt`'s mutex.
unsafe impl Send for Env {}

impl Env {
    fn new(dispatch_dir: &Path) -> Result<Env, Fault> {
        let lib = lib()?;
        let dir = CString::new(dispatch_dir.to_string_lossy().as_bytes()).map_err(fault)?;
        let mut value: LiteRtAny = unsafe { std::mem::zeroed() };
        value.type_ = kLiteRtAnyTypeString;
        value.__bindgen_anon_1.str_value = dir.as_ptr();
        let option = LiteRtEnvOption {
            tag: kLiteRtEnvOptionTagDispatchLibraryDir,
            value,
        };
        let mut handle = std::ptr::null_mut();
        check(
            unsafe { lib.LiteRtCreateEnvironment(1, &option, &mut handle) },
            "create environment",
        )?;
        Ok(Env { handle, _dir: dir })
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        if let Ok(lib) = lib() {
            unsafe { lib.LiteRtDestroyEnvironment(self.handle) }
        }
    }
}

/// One tensor buffer, made from the compiled model's own requirements and kept for the graph's life.
struct Buffer {
    handle: LiteRtTensorBuffer,
    bytes: usize,
}

/// One compiled graph, its buffers created once.
struct Graph {
    model: LiteRtModel,
    compiled: LiteRtCompiledModel,
    inputs: Vec<Buffer>,
    outputs: Vec<Buffer>,
    fully_accelerated: bool,
}

// A graph is used from one thread at a time, behind `OnnxStt`'s mutex.
unsafe impl Send for Graph {}

impl Graph {
    fn open(env: &Env, path: &Path) -> Result<Graph, Fault> {
        let lib = lib()?;
        let file = CString::new(path.to_string_lossy().as_bytes()).map_err(fault)?;
        let (mut model, mut options, mut compiled) = (
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        unsafe {
            check(
                lib.LiteRtCreateModelFromFile(env.handle, file.as_ptr(), &mut model),
                &format!("load {}", path.display()),
            )?;
            check(lib.LiteRtCreateOptions(&mut options), "options")?;
            check(
                lib.LiteRtSetOptionsHardwareAccelerators(options, kLiteRtHwAcceleratorNpu as _),
                "accelerators",
            )?;
            let made = lib.LiteRtCreateCompiledModel(env.handle, model, options, &mut compiled);
            lib.LiteRtDestroyOptions(options);
            check(made, &format!("compile {}", path.display()))?;
            let mut signature = std::ptr::null_mut();
            check(
                lib.LiteRtGetModelSignature(model, 0, &mut signature),
                "signature",
            )?;
            let (mut n_in, mut n_out) = (0, 0);
            check(
                lib.LiteRtGetNumSignatureInputs(signature, &mut n_in),
                "inputs",
            )?;
            check(
                lib.LiteRtGetNumSignatureOutputs(signature, &mut n_out),
                "outputs",
            )?;
            let buffer = |index: usize, input: bool| -> Result<Buffer, Fault> {
                let mut tensor = std::ptr::null_mut();
                let mut requirements = std::ptr::null_mut();
                if input {
                    check(
                        lib.LiteRtGetSignatureInputTensorByIndex(
                            signature,
                            index as _,
                            &mut tensor,
                        ),
                        "input tensor",
                    )?;
                    check(
                        lib.LiteRtGetCompiledModelInputBufferRequirements(
                            compiled,
                            0,
                            index as _,
                            &mut requirements,
                        ),
                        "input requirements",
                    )?;
                } else {
                    check(
                        lib.LiteRtGetSignatureOutputTensorByIndex(
                            signature,
                            index as _,
                            &mut tensor,
                        ),
                        "output tensor",
                    )?;
                    check(
                        lib.LiteRtGetCompiledModelOutputBufferRequirements(
                            compiled,
                            0,
                            index as _,
                            &mut requirements,
                        ),
                        "output requirements",
                    )?;
                }
                let mut ranked = std::mem::zeroed();
                check(
                    lib.LiteRtGetRankedTensorType(tensor, &mut ranked),
                    "tensor type",
                )?;
                let mut handle = std::ptr::null_mut();
                check(
                    lib.LiteRtCreateManagedTensorBufferFromRequirements(
                        env.handle,
                        &ranked,
                        requirements,
                        &mut handle,
                    ),
                    "buffer",
                )?;
                let mut bytes = 0usize;
                check(
                    lib.LiteRtGetTensorBufferPackedSize(handle, &mut bytes),
                    "buffer size",
                )?;
                Ok(Buffer { handle, bytes })
            };
            let inputs = (0..n_in as usize)
                .map(|i| buffer(i, true))
                .collect::<Result<Vec<_>, _>>()?;
            let outputs = (0..n_out as usize)
                .map(|i| buffer(i, false))
                .collect::<Result<Vec<_>, _>>()?;
            let mut fully_accelerated = false;
            check(
                lib.LiteRtCompiledModelIsFullyAccelerated(compiled, &mut fully_accelerated),
                "fully accelerated",
            )?;
            tracing::info!(graph = %path.display(), fully_accelerated, "an NPU graph is ready");
            Ok(Graph {
                model,
                compiled,
                inputs,
                outputs,
                fully_accelerated,
            })
        }
    }

    /// Write `data` into input `index` at byte `offset`.
    fn write(&self, index: usize, offset: usize, data: &[u8]) -> Result<(), Fault> {
        let lib = lib()?;
        let buffer = &self.inputs[index];
        if offset + data.len() > buffer.bytes {
            return Err(fault(format!(
                "input {index}: {} bytes at {offset} do not fit {}",
                data.len(),
                buffer.bytes
            )));
        }
        unsafe {
            let mut at = std::ptr::null_mut();
            check(
                lib.LiteRtLockTensorBuffer(
                    buffer.handle,
                    &mut at,
                    kLiteRtTensorBufferLockModeWrite,
                ),
                "lock input",
            )?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), (at as *mut u8).add(offset), data.len());
            lib.LiteRtUnlockTensorBuffer(buffer.handle);
        }
        Ok(())
    }

    /// Write several `(offset, bytes)` pieces into input `index` under one lock.
    fn write_pieces<'a>(
        &self,
        index: usize,
        pieces: impl Iterator<Item = (usize, &'a [u8])>,
    ) -> Result<(), Fault> {
        let lib = lib()?;
        let buffer = &self.inputs[index];
        unsafe {
            let mut at = std::ptr::null_mut();
            check(
                lib.LiteRtLockTensorBuffer(
                    buffer.handle,
                    &mut at,
                    kLiteRtTensorBufferLockModeReadWrite,
                ),
                "lock input",
            )?;
            let mut outcome = Ok(());
            for (offset, data) in pieces {
                if offset + data.len() > buffer.bytes {
                    outcome = Err(fault(format!(
                        "input {index}: {} bytes at {offset} do not fit {}",
                        data.len(),
                        buffer.bytes
                    )));
                    break;
                }
                std::ptr::copy_nonoverlapping(
                    data.as_ptr(),
                    (at as *mut u8).add(offset),
                    data.len(),
                );
            }
            lib.LiteRtUnlockTensorBuffer(buffer.handle);
            outcome
        }
    }

    /// Output `index`, copied out.
    fn read(&self, index: usize) -> Result<Vec<u8>, Fault> {
        let lib = lib()?;
        let buffer = &self.outputs[index];
        unsafe {
            let mut at = std::ptr::null_mut();
            check(
                lib.LiteRtLockTensorBuffer(buffer.handle, &mut at, kLiteRtTensorBufferLockModeRead),
                "lock output",
            )?;
            let bytes = std::slice::from_raw_parts(at as *const u8, buffer.bytes).to_vec();
            lib.LiteRtUnlockTensorBuffer(buffer.handle);
            Ok(bytes)
        }
    }

    fn run(&mut self) -> Result<(), Fault> {
        let lib = lib()?;
        let mut inputs: Vec<LiteRtTensorBuffer> = self.inputs.iter().map(|b| b.handle).collect();
        let mut outputs: Vec<LiteRtTensorBuffer> = self.outputs.iter().map(|b| b.handle).collect();
        check(
            unsafe {
                lib.LiteRtRunCompiledModel(
                    self.compiled,
                    0,
                    inputs.len(),
                    inputs.as_mut_ptr(),
                    outputs.len(),
                    outputs.as_mut_ptr(),
                )
            },
            "run",
        )
    }
}

impl Drop for Graph {
    fn drop(&mut self) {
        if let Ok(lib) = lib() {
            unsafe {
                for buffer in self.inputs.iter().chain(&self.outputs) {
                    lib.LiteRtDestroyTensorBuffer(buffer.handle);
                }
                lib.LiteRtDestroyCompiledModel(self.compiled);
                lib.LiteRtDestroyModel(self.model);
            }
        }
    }
}

fn bytes_of(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// One context's encoder, cross graph and decoder.
struct Set {
    encoder: Graph,
    cross: Graph,
    decoder: Graph,
}

impl Set {
    /// `encoder{suffix}.tflite` and its two siblings.
    fn open(env: &Env, bundle: &Path, suffix: &str) -> Result<Set, Fault> {
        let graph = |name: &str| Graph::open(env, &bundle.join(format!("{name}{suffix}.tflite")));
        Ok(Set {
            encoder: graph("encoder")?,
            cross: graph("cross")?,
            decoder: graph("decoder")?,
        })
    }

    fn fully_accelerated(&self) -> bool {
        self.encoder.fully_accelerated
            && self.cross.fully_accelerated
            && self.decoder.fully_accelerated
    }
}

/// Whisper's encoder, cross graph and one-token decoder, compiled for this phone's NPU, twice:
/// for a whole 30 s window and for its first ten seconds ([`npu::SHORT_POSITIONS`]).
///
/// Decoder inputs, in signature order (plan 2A, `whisper_kv.Decoder.forward`): token `[1, 1]` i32,
/// position `[1]` i32, self K and V `[L, 1, H, 448, Dh]` f32, cross K and V `[L, 1, H, P, Dh]`
/// f32 with P 1500 or 500. Outputs: logits `[1, vocab]`, the token's K and V `[L, 1, H, 1, Dh]`.
pub struct LiteRtGraphs {
    // Declared before `env`, so they are dropped before it.
    long: Set,
    short: Set,
    /// Whether the turn in progress is on the short set.
    on_short: bool,
    env: Env,
    layers: usize,
    heads: usize,
    head_dim: usize,
    /// This turn so far, for the one line each turn logs: encoder and cross milliseconds, then
    /// decoder steps and their milliseconds.
    turn: (u64, u64, u64),
}

impl LiteRtGraphs {
    /// The bundle's six graphs, with LiteRT's Qualcomm dispatch library (and the QNN libraries
    /// beside it) in `dispatch_dir`, the app's native library directory.
    pub fn open(
        bundle: &Path,
        dispatch_dir: &Path,
        layers: usize,
        heads: usize,
        head_dim: usize,
    ) -> Result<LiteRtGraphs, Fault> {
        let env = Env::new(dispatch_dir)?;
        Ok(LiteRtGraphs {
            long: Set::open(&env, bundle, "")?,
            short: Set::open(&env, bundle, "-10s")?,
            on_short: false,
            env,
            layers,
            heads,
            head_dim,
            turn: (0, 0, 0),
        })
    }

    /// Whether LiteRT put every op of all six graphs on the NPU.
    pub fn fully_accelerated(&self) -> bool {
        self.long.fully_accelerated() && self.short.fully_accelerated()
    }

    /// One turn on silence and one decoder step on each set: the NPU answers, or this says why
    /// it did not.
    ///
    /// **Opening is not proof.** With the DSP unreachable, LiteRT still opens every graph as
    /// "fully accelerated", and fails only when one runs (phase 0 findings).
    pub fn warm_up(&mut self) -> Result<(), Fault> {
        let silence = vec![-1.5; 80 * crate::mel::FRAMES];
        for frames in [crate::mel::FRAMES, 1] {
            self.begin_turn(&silence, frames)?;
            self.step(50258, 0)?;
        }
        Ok(())
    }
}

impl Graphs for LiteRtGraphs {
    fn begin_turn(&mut self, mel: &[f32], frames: usize) -> Result<(), Fault> {
        // The turn before this one, in one line: what a person tuning this needs, at INFO.
        let (encode_ms, steps, step_ms) = std::mem::take(&mut self.turn);
        if steps > 0 {
            tracing::info!(
                encode_ms,
                steps,
                step_ms_avg = step_ms / steps,
                short = self.on_short,
                "an NPU turn"
            );
        }
        let started = std::time::Instant::now();
        self.on_short = npu::is_short(frames);
        let (set, mel) = if self.on_short {
            (&mut self.short, npu::head_frames(mel, 80, 2 * npu::SHORT_POSITIONS))
        } else {
            (&mut self.long, mel.to_vec())
        };
        set.encoder.write(0, 0, &bytes_of(&mel))?;
        set.encoder.run()?;
        let hidden = set.encoder.read(0)?;
        set.cross.write(0, 0, &hidden)?;
        set.cross.run()?;
        // Cross K and V, once per turn, into the decoder's inputs 4 and 5.
        set.decoder.write(4, 0, &set.cross.read(0)?)?;
        set.decoder.write(5, 0, &set.cross.read(1)?)?;
        self.turn.0 = started.elapsed().as_millis() as u64;
        tracing::debug!(target: "zyris_voice::npu", ms = self.turn.0, "encoder and cross");
        Ok(())
    }

    fn step(&mut self, token: i32, position: usize) -> Result<Vec<f32>, Fault> {
        let started = std::time::Instant::now();
        let decoder = if self.on_short {
            &mut self.short.decoder
        } else {
            &mut self.long.decoder
        };
        decoder.write(0, 0, &token.to_le_bytes())?;
        decoder.write(1, 0, &(position as i32).to_le_bytes())?;
        decoder.run()?;
        let logits: Vec<f32> = decoder
            .read(0)?
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        // The token's K and V, into its slot of the cache the decoder reads next step.
        let row = self.head_dim * 4;
        let (heads, head_dim) = (self.heads, self.head_dim);
        for (output, input) in [(1, 2), (2, 3)] {
            let new = decoder.read(output)?;
            let pieces = (0..self.layers * heads).map(|i| {
                let (layer, head) = (i / heads, i % heads);
                (
                    npu::slot_offset(layer, head, position, heads, head_dim) * 4,
                    &new[i * row..(i + 1) * row],
                )
            });
            decoder.write_pieces(input, pieces)?;
        }
        let ms = started.elapsed().as_millis() as u64;
        self.turn.1 += 1;
        self.turn.2 += ms;
        tracing::debug!(target: "zyris_voice::npu", position, ms, "decoder step");
        Ok(logits)
    }
}
