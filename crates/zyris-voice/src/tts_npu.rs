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

// Used by the QNN half, which only an Android build with `npu` compiles; tested everywhere.
#[cfg_attr(not(all(feature = "npu", target_os = "android")), allow(dead_code))]
/// The compiled context's file name for `graph` in `bucket`. Named for the graph's own bytes (the
/// start of its SHA-256 in [`crate::tts::FILES`]) as well as the bucket, so a new model is compiled
/// anew instead of read through the old one's weights.
fn context_name(graph: &str, bucket: Bucket) -> String {
    let sha = crate::tts::FILES.iter().find(|f| f.file == graph).map_or("unknown", |f| &f.sha256[..12]);
    format!("{}-{}x{}-{sha}_ctx.onnx", graph.trim_end_matches(".onnx"), bucket.frames, bucket.text)
}

// Used by the QNN half, which only an Android build with `npu` compiles; tested everywhere.
#[cfg_attr(not(all(feature = "npu", target_os = "android")), allow(dead_code))]
/// The graphs that run on the NPU.
const GRAPHS: [&str; 2] = ["vector_estimator.onnx", "vocoder.onnx"];

// Used by the QNN half, which only an Android build with `npu` compiles; tested everywhere.
#[cfg_attr(not(all(feature = "npu", target_os = "android")), allow(dead_code))]
/// Which of `names` are not a current context or its weights: left by an older bucket or model,
/// and ~130 MB apiece.
fn stale(names: impl Iterator<Item = String>) -> Vec<String> {
    let current: Vec<String> = crate::tts::BUCKETS
        .iter()
        .flat_map(|b| GRAPHS.map(|g| context_name(g, *b)))
        .flat_map(|name| [name.replace("_ctx.onnx", "_ctx_qnn.bin"), name])
        .collect();
    names.filter(|name| !current.contains(name)).collect()
}

// Used by the QNN half, which only an Android build with `npu` compiles; tested everywhere.
#[cfg_attr(not(all(feature = "npu", target_os = "android")), allow(dead_code))]
/// Whether the NPU is off once every bucket has been tried: only when none is ready. A bucket
/// that failed (a large one out of memory, say) leaves the others reading, and its sentences go
/// to the processor.
fn off_after(ready: usize, failed: Vec<String>) -> Option<String> {
    (ready == 0 && !failed.is_empty())
        .then(|| format!("the NPU could not prepare the voice: {}", failed.join("; ")))
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
        // What an older bucket or model compiled is not read again, and takes space.
        let names = std::fs::read_dir(&dir).into_iter().flatten().flatten();
        for name in stale(names.map(|e| e.file_name().to_string_lossy().into_owned())) {
            tracing::info!(%name, "removing a compiled NPU context nothing uses");
            let _ = std::fs::remove_file(dir.join(name));
        }
        let (mut ready, mut failed) = (0, Vec::new());
        for bucket in crate::tts::BUCKETS {
            let started = std::time::Instant::now();
            let open = |graph: &str| session(&models.join(graph), bucket, &dir.join(context_name(graph, bucket)));
            match open(GRAPHS[0]).and_then(|e| Ok((e, open(GRAPHS[1])?))) {
                Ok(pair) => {
                    tracing::info!(
                        frames = bucket.frames,
                        text = bucket.text,
                        ms = started.elapsed().as_millis() as u64,
                        "an NPU voice bucket is ready"
                    );
                    npu.ready.lock().unwrap_or_else(|p| p.into_inner()).insert(bucket, pair);
                    ready += 1;
                }
                Err(e) => {
                    tracing::warn!(frames = bucket.frames, text = bucket.text, %e, "an NPU voice bucket could not be prepared; its sentences are read on the processor");
                    failed.push(e.to_string());
                }
            }
        }
        if let Some(reason) = off_after(ready, failed) {
            npu.disable(reason);
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
    /// A context is named for its graph's own bytes as well as its bucket: a new model is
    /// compiled anew, not read through the old model's weights.
    #[test]
    fn a_context_is_named_for_its_model_and_bucket() {
        let name = context_name("vector_estimator.onnx", crate::tts::BUCKETS[0]);
        let sha = crate::tts::FILES.iter().find(|f| f.file == "vector_estimator.onnx").unwrap().sha256;
        assert_eq!(name, format!("vector_estimator-64x192-{}_ctx.onnx", &sha[..12]));
    }

    /// What an older bucket or model left behind is found; the current contexts are not.
    #[test]
    fn contexts_of_other_buckets_and_models_are_stale() {
        let current = context_name("vocoder.onnx", crate::tts::BUCKETS[1]);
        let bin = current.replace("_ctx.onnx", "_ctx_qnn.bin");
        let names = [current.clone(), bin, "estimator-64x96_ctx.onnx".into(), "estimator-64x96_ctx_qnn.bin".into()];
        assert_eq!(
            stale(names.into_iter()),
            ["estimator-64x96_ctx.onnx", "estimator-64x96_ctx_qnn.bin"]
        );
    }

    /// One bucket failing leaves the others reading; only none at all turns the NPU off.
    #[test]
    fn the_npu_is_off_only_when_no_bucket_is_ready() {
        assert_eq!(off_after(1, vec!["out of memory".into()]), None);
        assert_eq!(
            off_after(0, vec!["out of memory".into()]).as_deref(),
            Some("the NPU could not prepare the voice: out of memory")
        );
    }

    #[test]
    fn a_disabled_voice_hands_out_nothing_and_says_why() {
        let npu = NpuVoice::empty();
        assert!(npu.with(crate::tts::BUCKETS[0], |_, _| ()).is_none());
        npu.disable("the DSP said no".into());
        assert_eq!(npu.off().as_deref(), Some("the DSP said no"));
        assert!(npu.with(crate::tts::BUCKETS[0], |_, _| ()).is_none());
    }
}
