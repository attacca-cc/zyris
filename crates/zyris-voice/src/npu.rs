//! Whisper on a phone's NPU, minus the NPU: the KV-cache bookkeeping over three compiled graphs,
//! the SoC table, and the bundles a phone downloads. `litert.rs` is the Android half.

use crate::onnx_stt::Runtime;
use crate::stt::Fault;

/// Self-attention cache slots: the decoder's `max_target_positions`, as plan 2A exported it.
pub const CACHE: usize = 448;

/// The compiled graphs, as a runtime drives them.
pub trait Graphs: Send {
    /// Run the encoder and the cross graph for one turn's log-mel; keep cross K and V where the decoder reads them.
    fn begin_turn(&mut self, mel: &[f32]) -> Result<(), Fault>;
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
    fn encode(&mut self, mel: &[f32]) -> Result<(), Fault> {
        self.fed.clear();
        self.last.clear();
        self.graphs.begin_turn(mel)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onnx_stt::Runtime;

    /// Records every call; logits are the position they were asked at, so a test can tell steps apart.
    #[derive(Default)]
    struct Recorder {
        turns: usize,
        steps: Vec<(i32, usize)>,
    }

    impl Graphs for Recorder {
        fn begin_turn(&mut self, _mel: &[f32]) -> Result<(), Fault> {
            self.turns += 1;
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
    fn a_prefix_is_fed_once_then_one_token_a_call() {
        let mut rt = runtime();
        rt.encode(&[]).unwrap();
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
        rt.encode(&[]).unwrap();
        rt.next_logits(&[50258]).unwrap(); // language detection
        rt.next_logits(&[50361, 1, 2, 50258, 50264]).unwrap(); // a prompted prefix: position 0 differs
        let positions: Vec<usize> = rt.graphs().steps.iter().map(|(_, p)| *p).collect();
        assert_eq!(positions, vec![0, 0, 1, 2, 3, 4]);
    }

    #[test]
    fn every_turn_begins_on_the_graphs_and_forgets_what_was_fed() {
        let mut rt = runtime();
        rt.encode(&[]).unwrap();
        rt.next_logits(&(0..300).collect::<Vec<i64>>()).unwrap();
        rt.encode(&[]).unwrap();
        rt.next_logits(&[50258, 50264]).unwrap();
        assert_eq!(rt.graphs().turns, 2);
        assert_eq!(*rt.graphs().steps.last().unwrap(), (50264, 1));
    }

    #[test]
    fn nothing_is_written_past_the_last_slot() {
        let mut rt = runtime();
        rt.encode(&[]).unwrap();
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
}
