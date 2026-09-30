//! Whisper on a phone's NPU, minus the NPU: the KV-cache bookkeeping over three compiled graphs,
//! the SoC table, and the bundles a phone downloads. `litert.rs` is the Android half.

use crate::onnx_stt::Runtime;
use crate::stt::Fault;

/// Self-attention cache slots: the decoder's `max_target_positions`, as plan 2A exported it.
pub const CACHE: usize = 448;

/// The compiled graphs, as a runtime drives them.
pub trait Graphs: Send {
    /// Run the encoder and the cross graph for one turn's log-mel; keep cross K and V where the decoder reads them.
    /// `frames` of the window hold audio.
    fn begin_turn(&mut self, mel: &[f32], frames: usize) -> Result<(), Fault>;
    /// Run the decoder for `token` at `position`: return its logits, and write its K and V into slot `position`.
    fn step(&mut self, token: i32, position: usize) -> Result<Vec<f32>, Fault>;
}

/// The f32 index of `[layer][0][head][position][0]` in a `[L, 1, H, CACHE, Dh]` cache.
pub fn slot_offset(
    layer: usize,
    head: usize,
    position: usize,
    heads: usize,
    head_dim: usize,
) -> usize {
    ((layer * heads + head) * CACHE + position) * head_dim
}

/// Encoder positions in a bundle's short graph set: ten seconds, two mel frames a position.
///
/// Measured on an S23 (2026-09-30): the encoder takes 65-158 ms instead of ~885 ms and a decoder
/// step ~57 ms instead of ~88 ms, and clips up to ten seconds transcribe exactly as with 1500.
pub const SHORT_POSITIONS: usize = 500;

/// Whether `frames` of audio fit the short set.
pub fn is_short(frames: usize) -> bool {
    frames <= 2 * SHORT_POSITIONS
}

/// The first `frames` of each of the `bins` rows of a `[bins, FRAMES]` log-mel: what the short
/// encoder reads.
pub fn head_frames(mel: &[f32], bins: usize, frames: usize) -> Vec<f32> {
    mel.chunks_exact(crate::mel::FRAMES)
        .take(bins)
        .flat_map(|row| &row[..frames])
        .copied()
        .collect()
}

/// `onnx_stt::Runtime` over [`Graphs`], feeding only the tokens not yet in the cache.
///
/// **Slots past the prefix are never cleared**, because the decoder masks every slot after the
/// position it is at (plan 2A, `whisper_kv.Decoder`). A second turn overwrites from 0 and never
/// reads what the first left behind.
pub struct NpuRuntime<G: Graphs> {
    graphs: G,
    fed: Vec<i64>,
    last: Vec<f32>,
}

impl<G: Graphs> NpuRuntime<G> {
    pub fn new(graphs: G) -> Self {
        NpuRuntime {
            graphs,
            fed: Vec::new(),
            last: Vec::new(),
        }
    }

    #[cfg(test)]
    fn graphs(&self) -> &G {
        &self.graphs
    }
}

impl<G: Graphs> Runtime for NpuRuntime<G> {
    fn encode(&mut self, mel: &[f32], frames: usize) -> Result<(), Fault> {
        self.fed.clear();
        self.last.clear();
        self.graphs.begin_turn(mel, frames)
    }

    fn next_logits(&mut self, tokens: &[i64]) -> Result<Vec<f32>, Fault> {
        if tokens.len() > CACHE {
            return Err(Fault::Whisper {
                detail: format!(
                    "{} tokens do not fit the NPU decoder's {CACHE}",
                    tokens.len()
                ),
            });
        }
        // Not an extension of what the cache holds (language detection's `[sot]`, then a prompted
        // prefix that starts with `<|startofprev|>`): feed it again from slot 0.
        if !tokens.starts_with(&self.fed) {
            self.fed.clear();
        }
        for (position, token) in tokens.iter().enumerate().skip(self.fed.len()) {
            self.last = self.graphs.step(*token as i32, position)?;
            self.fed.push(*token);
        }
        Ok(self.last.clone())
    }
}

/// One SoC's bundle: its HTP generation and the files it downloads.
pub struct Bundle {
    pub soc: &'static str,
    pub htp: u32,
    pub files: &'static [crate::model::Model],
}

/// Where a bundle stands on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BundleState {
    Ready {
        dir: std::path::PathBuf,
    },
    Partial {
        dir: std::path::PathBuf,
        have: u64,
        bytes: u64,
    },
    Absent {
        dir: std::path::PathBuf,
        bytes: u64,
    },
}

/// The bundle compiled for `soc` (`ro.soc.model`), if there is one.
pub fn bundle_for(soc: &str) -> Option<&'static Bundle> {
    crate::npu_catalog::BUNDLES.iter().find(|b| b.soc == soc)
}

/// Every byte a bundle downloads.
pub fn total_bytes(bundle: &Bundle) -> u64 {
    bundle.files.iter().map(|f| f.bytes).sum()
}

/// Where `bundle` lives under `root` (the models cache directory).
pub fn bundle_dir(bundle: &Bundle, root: &std::path::Path) -> std::path::PathBuf {
    root.join("npu")
        .join(format!("whisper-small-npu-{}", bundle.soc))
}

/// Ready only when every file is there at its size; a download cut short leaves only `.part`
/// files, which are never read as whole (`model::fetch`), so a killed app comes back `Partial`.
pub fn state(bundle: &Bundle, root: &std::path::Path) -> BundleState {
    use crate::model::{ModelState, inspect};
    let dir = bundle_dir(bundle, root);
    let bytes = total_bytes(bundle);
    let (mut have, mut ready, mut any) = (0, 0, false);
    for file in bundle.files {
        match inspect(&dir.join(file.file), Some(file.bytes)) {
            ModelState::Ready { bytes, .. } => {
                have += bytes;
                ready += 1;
                any = true;
            }
            ModelState::Absent { .. } | ModelState::Nowhere { .. } => {}
            _ => any = true,
        }
    }
    if ready == bundle.files.len() {
        BundleState::Ready { dir }
    } else if any {
        BundleState::Partial { dir, have, bytes }
    } else {
        BundleState::Absent { dir, bytes }
    }
}

/// Download every file of `bundle` that is not already whole, each checked against its size and
/// SHA-256 by `model::fetch`. `progress` sees the bytes of the whole bundle.
pub async fn fetch(
    bundle: &Bundle,
    root: &std::path::Path,
    mut progress: impl FnMut(crate::model::Progress),
) -> Result<std::path::PathBuf, crate::model::Fault> {
    use crate::model::{ModelState, Progress, inspect};
    let dir = bundle_dir(bundle, root);
    let total = total_bytes(bundle);
    let mut done = 0;
    for file in bundle.files {
        if let ModelState::Ready { .. } = inspect(&dir.join(file.file), Some(file.bytes)) {
            done += file.bytes;
            continue;
        }
        crate::model::fetch(file, &dir, |p| {
            progress(Progress {
                received: done + p.received,
                total: Some(total),
            })
        })
        .await?;
        done += file.bytes;
    }
    Ok(dir)
}

/// Whether this run's NPU warm-up has failed, and why: tried once per run, never again after a
/// failure, so a phone whose DSP refuses does not pay a failing load on every start.
#[derive(Default)]
pub struct WarmUp {
    failed: std::sync::Mutex<Option<String>>,
}

impl WarmUp {
    /// Run `load` unless an earlier attempt this run failed; remember a failure.
    pub fn try_once<T>(&self, load: impl FnOnce() -> Result<T, Fault>) -> Result<T, String> {
        let mut failed = self.failed.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(reason) = failed.as_ref() {
            return Err(reason.clone());
        }
        load().map_err(|fault| {
            let reason = fault.to_string();
            *failed = Some(reason.clone());
            reason
        })
    }

    /// Why the NPU is not in use this run, if it failed.
    pub fn reason(&self) -> Option<String> {
        self.failed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

/// This phone's SoC and the bundle compiled for it.
pub struct Probe {
    pub soc: String,
    pub bundle: &'static Bundle,
}

/// The NPU this build can use here: an Android phone at API 31 or later (LiteRT's NPU floor)
/// whose `ro.soc.model` has a bundle. `None` everywhere else, and in a build without `npu`.
pub fn probe() -> Option<Probe> {
    #[cfg(all(feature = "npu", target_os = "android"))]
    {
        static FOUND: std::sync::OnceLock<Option<(String, &'static Bundle)>> =
            std::sync::OnceLock::new();
        let (soc, bundle) = FOUND
            .get_or_init(|| {
                let prop = |name: &str| {
                    std::process::Command::new("getprop")
                        .arg(name)
                        .output()
                        .ok()
                        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                        .unwrap_or_default()
                };
                let api: u32 = prop("ro.build.version.sdk").parse().unwrap_or(0);
                let soc = prop("ro.soc.model");
                let bundle = bundle_for(&soc).filter(|_| api >= 31)?;
                tracing::info!(%soc, api, "this phone's NPU has a whisper bundle");
                Some((soc, bundle))
            })
            .as_ref()?;
        return Some(Probe {
            soc: soc.clone(),
            bundle,
        });
    }
    #[allow(unreachable_code)]
    None
}

/// The whisper transcriber on this phone's NPU, from a bundle on disk, warmed up: one silent turn
/// through all three graphs, since a graph that opens is not yet one that runs.
#[cfg(all(feature = "npu", target_os = "android"))]
pub fn load(dir: &std::path::Path) -> Result<crate::onnx_stt::OnnxStt, Fault> {
    let native = native_lib_dir().ok_or_else(|| Fault::Whisper {
        detail: "the app's native library directory was not found".into(),
    })?;
    // The DSP loads the Hexagon skel by path. Set before LiteRT opens the dispatch library, once.
    static ADSP: std::sync::Once = std::sync::Once::new();
    ADSP.call_once(|| {
        let path = format!(
            "{};/vendor/lib/rfsa/adsp;/vendor/dsp/cdsp;/system/lib/rfsa/adsp",
            native.display()
        );
        // SAFETY: set once, before the NPU is first opened; nothing else here reads it concurrently.
        unsafe { std::env::set_var("ADSP_LIBRARY_PATH", path) };
    });
    let mut graphs = crate::litert::LiteRtGraphs::open(dir, &native, 12, 12, 64)?;
    tracing::info!(
        fully_accelerated = graphs.fully_accelerated(),
        "the NPU graphs are open"
    );
    let started = std::time::Instant::now();
    graphs.warm_up()?;
    tracing::info!(
        ms = started.elapsed().as_millis() as u64,
        "the NPU answered its warm-up"
    );
    let read = |name: &str| {
        std::fs::read_to_string(dir.join(name)).map_err(|e| Fault::Whisper {
            detail: format!("{name}: {e}"),
        })
    };
    let specials =
        crate::onnx_stt::Specials::from_generation_config(&read("generation_config.json")?)?;
    let tokenizer = crate::bpe::Tokenizer::from_json(&read("tokenizer.json")?)
        .map_err(|detail| Fault::Whisper { detail })?;
    Ok(crate::onnx_stt::OnnxStt::with(
        Box::new(NpuRuntime::new(graphs)),
        specials,
        tokenizer,
        80,
    ))
}

/// Where Android extracted this app's native libraries (legacy packaging): the directory of the
/// app's own library, read from the process's memory map.
#[cfg(all(feature = "npu", target_os = "android"))]
fn native_lib_dir() -> Option<std::path::PathBuf> {
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    let line = maps.lines().find(|l| l.ends_with("/libzyris_app_lib.so"))?;
    let path = std::path::Path::new(line.split_whitespace().last()?);
    path.parent().map(std::path::Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onnx_stt::Runtime;

    /// Records every call; logits are the position they were asked at, so a test can tell steps apart.
    #[derive(Default)]
    struct Recorder {
        turns: usize,
        steps: Vec<(i32, usize)>,
        frames: Vec<usize>,
    }

    impl Graphs for Recorder {
        fn begin_turn(&mut self, _mel: &[f32], frames: usize) -> Result<(), Fault> {
            self.turns += 1;
            self.frames.push(frames);
            Ok(())
        }
        fn step(&mut self, token: i32, position: usize) -> Result<Vec<f32>, Fault> {
            self.steps.push((token, position));
            Ok(vec![position as f32; 4])
        }
    }

    fn runtime() -> NpuRuntime<Recorder> {
        NpuRuntime::new(Recorder::default())
    }

    #[test]
    fn ten_seconds_is_short_and_a_frame_more_is_not() {
        assert!(is_short(1));
        assert!(is_short(2 * SHORT_POSITIONS));
        assert!(!is_short(2 * SHORT_POSITIONS + 1));
        assert!(!is_short(crate::mel::FRAMES));
    }

    #[test]
    fn the_head_of_a_mel_is_the_first_frames_of_every_row() {
        let frames = crate::mel::FRAMES;
        let mel: Vec<f32> = (0..2 * frames).map(|i| i as f32).collect();
        let head = head_frames(&mel, 2, 3);
        assert_eq!(head, [0.0, 1.0, 2.0, frames as f32, frames as f32 + 1.0, frames as f32 + 2.0]);
    }

    #[test]
    fn the_frames_reach_the_graphs() {
        let mut rt = runtime();
        rt.encode(&[], 640).unwrap();
        assert_eq!(rt.graphs.frames, [640]);
    }

    #[test]
    fn a_prefix_is_fed_once_then_one_token_a_call() {
        let mut rt = runtime();
        rt.encode(&[], 0).unwrap();
        assert_eq!(
            rt.next_logits(&[50258, 50264, 50359, 50363]).unwrap(),
            vec![3.0; 4]
        );
        assert_eq!(
            rt.next_logits(&[50258, 50264, 50359, 50363, 7]).unwrap(),
            vec![4.0; 4]
        );
        assert_eq!(
            rt.graphs().steps,
            vec![(50258, 0), (50264, 1), (50359, 2), (50363, 3), (7, 4)]
        );
    }

    #[test]
    fn a_list_that_is_not_an_extension_is_fed_again_from_the_start() {
        let mut rt = runtime();
        rt.encode(&[], 0).unwrap();
        rt.next_logits(&[50258]).unwrap(); // language detection
        rt.next_logits(&[50361, 1, 2, 50258, 50264]).unwrap(); // a prompted prefix: position 0 differs
        let positions: Vec<usize> = rt.graphs().steps.iter().map(|(_, p)| *p).collect();
        assert_eq!(positions, vec![0, 0, 1, 2, 3, 4]);
    }

    #[test]
    fn every_turn_begins_on_the_graphs_and_forgets_what_was_fed() {
        let mut rt = runtime();
        rt.encode(&[], 0).unwrap();
        rt.next_logits(&(0..300).collect::<Vec<i64>>()).unwrap();
        rt.encode(&[], 0).unwrap();
        rt.next_logits(&[50258, 50264]).unwrap();
        assert_eq!(rt.graphs().turns, 2);
        assert_eq!(*rt.graphs().steps.last().unwrap(), (50264, 1));
    }

    #[test]
    fn nothing_is_written_past_the_last_slot() {
        let mut rt = runtime();
        rt.encode(&[], 0).unwrap();
        let full: Vec<i64> = (0..CACHE as i64).collect();
        assert!(rt.next_logits(&full).is_ok());
        let over: Vec<i64> = (0..=CACHE as i64).collect();
        assert!(rt.next_logits(&over).is_err());
        assert!(rt.graphs().steps.iter().all(|(_, p)| *p < CACHE));
    }

    #[test]
    fn a_slot_is_where_the_cache_layout_puts_it() {
        // [L, 1, H, CACHE, Dh] with H = 12, Dh = 64.
        assert_eq!(slot_offset(0, 0, 0, 12, 64), 0);
        assert_eq!(slot_offset(0, 0, 1, 12, 64), 64);
        assert_eq!(slot_offset(0, 1, 0, 12, 64), CACHE * 64);
        assert_eq!(slot_offset(1, 0, 0, 12, 64), 12 * CACHE * 64);
    }

    #[test]
    fn every_phone_soc_the_bundles_were_built_for_has_a_bundle_and_nothing_else_does() {
        for soc in [
            "SM8450", "SM8475", "SM8550", "SM8650", "SM8750", "SM8845", "SM8850",
        ] {
            let b = bundle_for(soc).unwrap_or_else(|| panic!("{soc}"));
            assert_eq!(b.files.len(), 6);
            assert!(b.files.iter().any(|f| f.file == "decoder.tflite"));
        }
        for soc in ["SM8350", "SA8295", "MT6989", "", "sm8550"] {
            assert!(bundle_for(soc).is_none(), "{soc}");
        }
    }

    #[test]
    fn a_bundle_is_ready_only_when_every_file_is_whole() {
        let root = std::env::temp_dir().join(format!("zyris-npu-state-{}", std::process::id()));
        let b = bundle_for("SM8550").unwrap();
        assert!(matches!(state(b, &root), BundleState::Absent { .. }));
        let dir = root.join("npu/whisper-small-npu-SM8550");
        std::fs::create_dir_all(&dir).unwrap();
        let small = b.files.iter().min_by_key(|f| f.bytes).unwrap();
        std::fs::write(dir.join(small.file), vec![0u8; small.bytes as usize]).unwrap();
        assert!(matches!(state(b, &root), BundleState::Partial { .. }));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_failed_warm_up_is_remembered_for_the_run() {
        let gate = WarmUp::default();
        assert!(
            gate.try_once(|| Err::<(), _>(Fault::Whisper {
                detail: "dsp".into()
            }))
            .is_err()
        );
        let mut called = false;
        assert!(
            gate.try_once(|| {
                called = true;
                Ok(())
            })
            .is_err(),
            "the first failure stands"
        );
        assert!(!called, "no second attempt in the same run");
        assert_eq!(
            gate.reason().as_deref(),
            Some("speech recognition failed: dsp")
        );
    }
}
