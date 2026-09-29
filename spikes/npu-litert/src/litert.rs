//! Just enough of LiteRT's C API to run a compiled model with managed buffers, and `LiteRt`,
//! whisper's two graphs behind phase 1's `onnx_stt::Runtime`.
#![allow(non_upper_case_globals, non_camel_case_types, non_snake_case, dead_code)]
#[allow(unsafe_op_in_unsafe_fn)]
mod sys {
    include!(concat!(env!("OUT_DIR"), "/litert.rs"));
}
use std::ffi::CString;
use sys::*;
use zyris_voice::stt::Fault;

fn check(status: LiteRtStatus, what: &str) -> Result<(), Fault> {
    if status == kLiteRtStatusOk {
        Ok(())
    } else {
        Err(Fault::Whisper { detail: format!("LiteRT {what}: status {status}") })
    }
}

pub struct Env(LiteRtEnvironment, CString);

impl Env {
    /// `dispatch_dir` holds `libLiteRtDispatch_Qualcomm.so` (or MediaTek's) and the vendor libraries.
    pub fn new(dispatch_dir: &str) -> Result<Env, Fault> {
        let dir = CString::new(dispatch_dir).unwrap();
        let mut value: LiteRtAny = unsafe { std::mem::zeroed() };
        value.type_ = kLiteRtAnyTypeString;
        value.__bindgen_anon_1.str_value = dir.as_ptr();
        let option = LiteRtEnvOption { tag: kLiteRtEnvOptionTagDispatchLibraryDir, value };
        let mut env = std::ptr::null_mut();
        check(unsafe { LiteRtCreateEnvironment(1, &option, &mut env) }, "create environment")?;
        Ok(Env(env, dir))
    }
}

pub struct Graph {
    env: LiteRtEnvironment,
    model: LiteRtModel,
    compiled: LiteRtCompiledModel,
    inputs: Vec<LiteRtRankedTensorType>,
    outputs: Vec<LiteRtRankedTensorType>,
    pub fully_accelerated: bool,
}

impl Graph {
    pub fn open(env: &Env, path: &str, npu: bool) -> Result<Graph, Fault> {
        let file = CString::new(path).unwrap();
        let (mut model, mut options, mut compiled) =
            (std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
        unsafe {
            check(LiteRtCreateModelFromFile(env.0, file.as_ptr(), &mut model), "load model")?;
            check(LiteRtCreateOptions(&mut options), "options")?;
            let accel = if npu { kLiteRtHwAcceleratorNpu } else { kLiteRtHwAcceleratorCpu };
            check(LiteRtSetOptionsHardwareAccelerators(options, accel as _), "accelerators")?;
            check(LiteRtCreateCompiledModel(env.0, model, options, &mut compiled), "compile")?;
            LiteRtDestroyOptions(options);
            let mut signature = std::ptr::null_mut();
            check(LiteRtGetModelSignature(model, 0, &mut signature), "signature")?;
            let (mut n_in, mut n_out) = (0, 0);
            check(LiteRtGetNumSignatureInputs(signature, &mut n_in), "inputs")?;
            check(LiteRtGetNumSignatureOutputs(signature, &mut n_out), "outputs")?;
            let ranked = |tensor| -> Result<LiteRtRankedTensorType, Fault> {
                let mut t = std::mem::zeroed();
                check(LiteRtGetRankedTensorType(tensor, &mut t), "tensor type")?;
                Ok(t)
            };
            let mut inputs = Vec::new();
            for i in 0..n_in {
                let mut tensor = std::ptr::null_mut();
                check(LiteRtGetSignatureInputTensorByIndex(signature, i, &mut tensor), "input tensor")?;
                inputs.push(ranked(tensor)?);
            }
            let mut outputs = Vec::new();
            for i in 0..n_out {
                let mut tensor = std::ptr::null_mut();
                check(LiteRtGetSignatureOutputTensorByIndex(signature, i, &mut tensor), "output tensor")?;
                outputs.push(ranked(tensor)?);
            }
            let mut fully = false;
            check(LiteRtCompiledModelIsFullyAccelerated(compiled, &mut fully), "fully accelerated")?;
            Ok(Graph { env: env.0, model, compiled, inputs, outputs, fully_accelerated: fully })
        }
    }

    /// Runs signature 0 with these inputs (raw bytes, in signature order) and returns the outputs.
    pub fn run(&mut self, inputs: &[&[u8]]) -> Result<Vec<Vec<u8>>, Fault> {
        unsafe {
            let buffer = |index: usize, input: bool| -> Result<LiteRtTensorBuffer, Fault> {
                let mut req = std::ptr::null_mut();
                if input {
                    check(LiteRtGetCompiledModelInputBufferRequirements(self.compiled, 0, index as _, &mut req), "input requirements")?;
                } else {
                    check(LiteRtGetCompiledModelOutputBufferRequirements(self.compiled, 0, index as _, &mut req), "output requirements")?;
                }
                let t = if input { &self.inputs[index] } else { &self.outputs[index] };
                let mut b = std::ptr::null_mut();
                check(LiteRtCreateManagedTensorBufferFromRequirements(self.env, t, req, &mut b), "buffer")?;
                Ok(b)
            };
            let mut ins = Vec::new();
            for (i, bytes) in inputs.iter().enumerate() {
                let b = buffer(i, true)?;
                let mut at = std::ptr::null_mut();
                check(LiteRtLockTensorBuffer(b, &mut at, kLiteRtTensorBufferLockModeWrite), "lock input")?;
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), at as *mut u8, bytes.len());
                LiteRtUnlockTensorBuffer(b);
                ins.push(b);
            }
            let mut outs = (0..self.outputs.len()).map(|i| buffer(i, false)).collect::<Result<Vec<_>, _>>()?;
            check(
                LiteRtRunCompiledModel(self.compiled, 0, ins.len(), ins.as_mut_ptr(), outs.len(), outs.as_mut_ptr()),
                "run",
            )?;
            let mut result = Vec::new();
            for b in &outs {
                let (mut size, mut at) = (0usize, std::ptr::null_mut());
                check(LiteRtGetTensorBufferPackedSize(*b, &mut size), "output size")?;
                check(LiteRtLockTensorBuffer(*b, &mut at, kLiteRtTensorBufferLockModeRead), "lock output")?;
                result.push(std::slice::from_raw_parts(at as *const u8, size).to_vec());
                LiteRtUnlockTensorBuffer(*b);
            }
            ins.into_iter().chain(outs).for_each(|b| LiteRtDestroyTensorBuffer(b));
            Ok(result)
        }
    }
}

impl Drop for Graph {
    fn drop(&mut self) {
        unsafe {
            LiteRtDestroyCompiledModel(self.compiled);
            LiteRtDestroyModel(self.model)
        }
    }
}

// The graphs are used from one thread at a time, behind `OnnxStt`'s mutex.
unsafe impl Send for Graph {}

/// Whisper's encoder and fixed-window decoder, compiled for this phone.
pub struct LiteRt {
    pub encoder: Graph,
    pub decoder: Graph,
    pub tokens: usize,
    hidden: Vec<u8>,
    mask: Vec<u8>,
}

impl LiteRt {
    pub fn new(encoder: Graph, decoder: Graph, tokens: usize) -> LiteRt {
        let mask: Vec<u8> = (0..tokens * tokens)
            .map(|i| if i % tokens <= i / tokens { 0f32 } else { f32::NEG_INFINITY })
            .flat_map(f32::to_le_bytes)
            .collect();
        LiteRt { encoder, decoder, tokens, hidden: Vec::new(), mask }
    }
}

/// Where the time goes: encoder microseconds, decoder microseconds, decoder calls.
pub static ENCODE_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static DECODE_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static DECODE_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl zyris_voice::onnx_stt::Runtime for LiteRt {
    fn encode(&mut self, mel: &[f32]) -> Result<(), Fault> {
        let bytes: Vec<u8> = mel.iter().flat_map(|v| v.to_le_bytes()).collect();
        let t = std::time::Instant::now();
        self.hidden = self.encoder.run(&[&bytes])?.remove(0);
        ENCODE_US.fetch_add(t.elapsed().as_micros() as u64, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    fn next_logits(&mut self, tokens: &[i64]) -> Result<Vec<f32>, Fault> {
        if tokens.len() > self.tokens {
            return Err(Fault::Whisper {
                detail: format!("{} tokens do not fit the decoder's {}", tokens.len(), self.tokens),
            });
        }
        let mut ids = vec![0i32; self.tokens];
        for (slot, t) in ids.iter_mut().zip(tokens) {
            *slot = *t as i32;
        }
        let ids: Vec<u8> = ids.iter().flat_map(|v| v.to_le_bytes()).collect();
        let t = std::time::Instant::now();
        let out = self.decoder.run(&[&self.hidden, &ids, &self.mask])?.remove(0);
        DECODE_US.fetch_add(t.elapsed().as_micros() as u64, std::sync::atomic::Ordering::Relaxed);
        DECODE_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let logits: Vec<f32> = out.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect();
        let vocab = logits.len() / self.tokens;
        let at = (tokens.len() - 1) * vocab;
        Ok(logits[at..at + vocab].to_vec())
    }
}
