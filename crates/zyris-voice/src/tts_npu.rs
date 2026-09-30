//! Supertonic's `vector_estimator` and `vocoder` on a Qualcomm NPU, through ONNX Runtime's QNN EP:
//! one pair of sessions per [`Bucket`], compiled on this phone once and kept as EP context files.
//! What was measured, and why this shape: `docs/superpowers/specs/2026-09-30-tts-npu-design.md`.

use crate::tts::Bucket;
use ort::session::Session;
use std::collections::HashMap;
use std::sync::Mutex;

use std::path::PathBuf;
use std::sync::Arc;

/// The NPU's sessions for each [`Bucket`] that is ready, or why the NPU is not reading at all.
pub struct NpuVoice {
    ready: Mutex<HashMap<Bucket, (Session, Session)>>,
    off: Mutex<Option<String>>,
}

impl NpuVoice {
    fn empty() -> NpuVoice {
        NpuVoice { ready: Mutex::new(HashMap::new()), off: Mutex::new(None) }
    }

    /// Open or compile every bucket on a thread of its own, smallest first; each is used the
    /// moment both of its graphs are ready, and the processor reads until then.
    pub fn start(models: PathBuf, cache: PathBuf) -> Arc<NpuVoice> {
        let npu = Arc::new(NpuVoice::empty());
        #[cfg(all(feature = "npu", target_os = "android"))]
        {
            let npu = npu.clone();
            std::thread::spawn(move || qnn::prepare(&npu, &models, &cache));
        }
        #[cfg(not(all(feature = "npu", target_os = "android")))]
        {
            let _ = (models, cache);
            npu.disable("this build has no NPU support".into());
        }
        npu
    }

    /// Run `f` on `bucket`'s estimator and vocoder, if they are ready and the NPU is on.
    pub fn with<R>(
        &self,
        bucket: Bucket,
        f: impl FnOnce(&mut Session, &mut Session) -> R,
    ) -> Option<R> {
        if self.off().is_some() {
            return None;
        }
        let mut ready = self.ready.lock().unwrap_or_else(|p| p.into_inner());
        ready.get_mut(&bucket).map(|(estimator, vocoder)| f(estimator, vocoder))
    }

    /// Stop handing out sessions for the rest of the run, and say why.
    pub fn disable(&self, reason: String) {
        tracing::warn!(%reason, "answers are read on the processor for the rest of this run");
        *self.off.lock().unwrap_or_else(|p| p.into_inner()) = Some(reason);
        self.ready.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }

    /// Why the NPU is not reading answers, if it is not.
    pub fn off(&self) -> Option<String> {
        self.off.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

#[cfg(all(feature = "npu", target_os = "android"))]
mod qnn {
    use super::*;
    use ort::execution_providers::{QNNExecutionProvider, qnn::QNNPerformanceMode};
    use std::path::Path;

    /// Every bucket, smallest first: the compiled context where one opens, else a compile that
    /// leaves one behind (5.5 s for the estimator at 64 frames on an S23; 0.33 s to open after).
    pub(super) fn prepare(npu: &NpuVoice, models: &Path, cache: &Path) {
        if let Err(fault) = crate::npu::reach_the_dsp() {
            return npu.disable(fault.to_string());
        }
        let dir = cache.join("tts-npu").join(models.file_name().unwrap_or_default());
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return npu.disable(format!("{}: {e}", dir.display()));
        }
        for bucket in crate::tts::BUCKETS {
            let started = std::time::Instant::now();
            let name = |graph: &str| dir.join(format!("{graph}-{}x{}_ctx.onnx", bucket.frames, bucket.text));
            let pair = session(&models.join("vector_estimator.onnx"), bucket, &name("estimator"))
                .and_then(|e| Ok((e, session(&models.join("vocoder.onnx"), bucket, &name("vocoder"))?)));
            match pair {
                Ok(pair) => {
                    tracing::info!(
                        frames = bucket.frames,
                        text = bucket.text,
                        ms = started.elapsed().as_millis() as u64,
                        "an NPU voice bucket is ready"
                    );
                    npu.ready.lock().unwrap_or_else(|p| p.into_inner()).insert(bucket, pair);
                }
                Err(e) => return npu.disable(format!("the NPU could not prepare the voice: {e}")),
            }
        }
    }

    fn builder(bucket: Bucket) -> ort::Result<ort::session::builder::SessionBuilder> {
        Session::builder()?
            .with_dimension_override("batch_size", 1)?
            .with_dimension_override("latent_length", bucket.frames as i64)?
            .with_dimension_override("text_length", bucket.text as i64)?
            .with_execution_providers([QNNExecutionProvider::default()
                .with_backend_path("libQnnHtp.so")
                .with_performance_mode(QNNPerformanceMode::Burst)
                .with_htp_fp16_precision(true)
                .build()
                .error_on_failure()])
    }

    /// The compiled context if it opens; one that does not is deleted and compiled again.
    fn session(model: &Path, bucket: Bucket, ctx: &Path) -> ort::Result<Session> {
        if ctx.exists() {
            match builder(bucket)?.commit_from_file(ctx) {
                Ok(session) => return Ok(session),
                Err(e) => {
                    tracing::warn!(%e, ctx = %ctx.display(), "a compiled NPU context would not open; compiling again");
                    let _ = std::fs::remove_file(ctx);
                    // ONNX Runtime writes the weights beside it as `<name>_qnn.bin`.
                    let bin = ctx.to_string_lossy().replace("_ctx.onnx", "_ctx_qnn.bin");
                    let _ = std::fs::remove_file(bin);
                }
            }
        }
        // Compiled from the model, the session keeps the model's fp32 weights beside the HTP's:
        // the app's PSS was 3.17 GB after the first compile on an S23, against 1.35 GB opened
        // from the contexts. So the compiling session is dropped and the context opened instead.
        drop(
            builder(bucket)?
                .with_config_entry("ep.context_enable", "1")?
                .with_config_entry("ep.context_file_path", ctx.to_string_lossy())?
                .commit_from_file(model)?,
        );
        builder(bucket)?.commit_from_file(ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing is ready until something is put there, and a disabled voice hands out nothing
    /// again, with the reason kept for the Voice screen.
    #[test]
    fn a_disabled_voice_hands_out_nothing_and_says_why() {
        let npu = NpuVoice::empty();
        assert!(npu.with(crate::tts::BUCKETS[0], |_, _| ()).is_none());
        npu.disable("the DSP said no".into());
        assert_eq!(npu.off().as_deref(), Some("the DSP said no"));
        assert!(npu.with(crate::tts::BUCKETS[0], |_, _| ()).is_none());
    }
}
