//! Whisper as an exported model: the decoding every runtime shares, over a two-method
//! [`Runtime`].

use crate::stt::Fault;

pub const MAX_TOKENS: usize = 224;
pub const MAX_PROMPT: usize = 223;
pub const MAX_POSITIONS: usize = 448;

pub trait Runtime: Send {
    /// `mel` is a whole 30 s window; `frames` is how many of its frames hold audio, the rest
    /// being padding. A runtime with graphs for shorter windows picks on it.
    fn encode(&mut self, mel: &[f32], frames: usize) -> Result<(), Fault>;
    fn next_logits(&mut self, tokens: &[i64]) -> Result<Vec<f32>, Fault>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Specials {
    pub eot: u32,
    pub sot: u32,
    pub transcribe: u32,
    pub no_timestamps: u32,
    pub prev: u32,
    pub languages: Vec<(String, u32)>,
    pub suppress: Vec<u32>,
    pub begin_suppress: Vec<u32>,
}

fn fault(detail: impl std::fmt::Display) -> Fault {
    Fault::Whisper {
        detail: detail.to_string(),
    }
}

impl Specials {
    /// From the model's `generation_config.json`.
    pub fn from_generation_config(json: &str) -> Result<Specials, Fault> {
        let v: serde_json::Value = serde_json::from_str(json).map_err(fault)?;
        let id = |value: &serde_json::Value, key: &str| {
            value
                .as_u64()
                .map(|n| n as u32)
                .ok_or_else(|| fault(format!("generation_config.json has no {key}")))
        };
        let list = |key: &str| -> Vec<u32> {
            v[key]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|n| n.as_u64())
                .map(|n| n as u32)
                .collect()
        };
        let mut languages: Vec<(String, u32)> = v["lang_to_id"]
            .as_object()
            .ok_or_else(|| fault("generation_config.json has no lang_to_id"))?
            .iter()
            .filter_map(|(tag, n)| {
                let code = tag
                    .trim_start_matches("<|")
                    .trim_end_matches("|>")
                    .to_string();
                Some((code, n.as_u64()? as u32))
            })
            .collect();
        languages.sort_by_key(|(_, id)| *id);
        Ok(Specials {
            eot: id(&v["eos_token_id"], "eos_token_id")?,
            sot: id(&v["decoder_start_token_id"], "decoder_start_token_id")?,
            transcribe: id(&v["task_to_id"]["transcribe"], "task_to_id.transcribe")?,
            no_timestamps: id(&v["no_timestamps_token_id"], "no_timestamps_token_id")?,
            prev: id(&v["prev_sot_token_id"], "prev_sot_token_id")?,
            languages,
            suppress: list("suppress_tokens"),
            begin_suppress: list("begin_suppress_tokens"),
        })
    }
}

/// The language token whisper finds likeliest after the start token alone, as whisper itself
/// detects. Detected per turn: the same person says one thing in Korean and the next in English.
pub fn detect_language(runtime: &mut dyn Runtime, specials: &Specials) -> Result<u32, Fault> {
    let logits = runtime.next_logits(&[i64::from(specials.sot)])?;
    specials
        .languages
        .iter()
        .map(|(_, id)| *id)
        .filter(|id| (*id as usize) < logits.len())
        .max_by(|a, b| logits[*a as usize].total_cmp(&logits[*b as usize]))
        .ok_or_else(|| fault("the model lists no language tokens"))
}

/// `<|startofprev|> prompt… <|startoftranscript|> <|lang|> <|transcribe|> <|notimestamps|>`, the
/// prompt only when there is one, and only its last [`MAX_PROMPT`] tokens.
pub fn prefix(specials: &Specials, language: u32, prompt: &[u32]) -> Vec<i64> {
    let mut tokens = Vec::new();
    if !prompt.is_empty() {
        tokens.push(i64::from(specials.prev));
        let kept = &prompt[prompt.len().saturating_sub(MAX_PROMPT)..];
        tokens.extend(kept.iter().map(|id| i64::from(*id)));
    }
    tokens.extend(
        [
            specials.sot,
            language,
            specials.transcribe,
            specials.no_timestamps,
        ]
        .map(i64::from),
    );
    tokens
}

/// The likeliest token each step, until end-of-text, [`MAX_TOKENS`] tokens, or the model's
/// [`MAX_POSITIONS`]. Special tokens (every id above end-of-text) and the model's suppressed
/// tokens are never chosen; on the first step neither is a bare space or end-of-text.
///
/// ponytail: greedy with a cap is the only guard against a repeating decoder; add whisper.cpp's
/// temperature fallback if phase 0 shows loops on real speech.
///
/// `stop` set gives up before the next step with a fault: the reading is no longer wanted.
pub fn greedy(
    runtime: &mut dyn Runtime,
    specials: &Specials,
    prefix: Vec<i64>,
    stop: &std::sync::atomic::AtomicBool,
) -> Result<Vec<u32>, Fault> {
    let mut tokens = prefix;
    let mut out = Vec::new();
    for step in 0..MAX_TOKENS {
        if tokens.len() >= MAX_POSITIONS {
            break;
        }
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            tracing::info!(step, "a reading was abandoned");
            return Err(fault("the reading was abandoned"));
        }
        let mut logits = runtime.next_logits(&tokens)?;
        // At the first step only, the begin-suppressed tokens too.
        let first = if step == 0 {
            specials.begin_suppress.len()
        } else {
            0
        };
        let banned = specials
            .suppress
            .iter()
            .chain(&specials.begin_suppress[..first]);
        for id in banned {
            if let Some(logit) = logits.get_mut(*id as usize) {
                *logit = f32::NEG_INFINITY;
            }
        }
        for logit in logits.iter_mut().skip(specials.eot as usize + 1) {
            *logit = f32::NEG_INFINITY;
        }
        let next = logits
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(id, _)| id as u32)
            .ok_or_else(|| fault("the decoder returned no logits"))?;
        if next == specials.eot {
            break;
        }
        out.push(next);
        tokens.push(i64::from(next));
    }
    Ok(out)
}

use std::path::Path;
use std::sync::Mutex;

/// The directory an exported whisper lives in, for the tests that need a real one.
pub const MODEL_ENV: &str = "ZYRIS_ONNX_WHISPER";

/// ONNX Runtime on the CPU: `encoder_model.onnx` and `decoder_model.onnx` as `optimum` exports
/// them. The decoder has no KV cache, so each step reruns every token so far; for whisper's short
/// turns on base that costs less than the cache's forty extra inputs.
pub struct Ort {
    encoder: ort::session::Session,
    decoder: ort::session::Session,
    bins: usize,
    hidden: Vec<f32>,
    hidden_shape: Vec<usize>,
}

impl Ort {
    pub fn load(dir: &Path, bins: usize) -> Result<Ort, Fault> {
        Ok(Ort {
            encoder: session(&dir.join("onnx/encoder_model.onnx"))?,
            decoder: session(&dir.join("onnx/decoder_model.onnx"))?,
            bins,
            hidden: Vec::new(),
            hidden_shape: Vec::new(),
        })
    }
}

fn session(path: &Path) -> Result<ort::session::Session, Fault> {
    if !path.is_file() {
        return Err(fault(format!("{} is missing", path.display())));
    }
    ort::session::Session::builder()
        .and_then(|b| b.with_intra_threads(crate::stt::threads()))
        .and_then(|b| b.commit_from_file(path))
        .map_err(|e| fault(format!("{}: {e}", path.display())))
}

impl Runtime for Ort {
    fn encode(&mut self, mel: &[f32], _frames: usize) -> Result<(), Fault> {
        let features =
            ort::value::TensorRef::from_array_view((vec![1, self.bins, crate::mel::FRAMES], mel))
                .map_err(fault)?;
        let out = self
            .encoder
            .run(ort::inputs!["input_features" => features])
            .map_err(fault)?;
        let (shape, data) = out["last_hidden_state"]
            .try_extract_tensor::<f32>()
            .map_err(fault)?;
        self.hidden_shape = shape.iter().map(|d| *d as usize).collect();
        self.hidden = data.to_vec();
        Ok(())
    }

    fn next_logits(&mut self, tokens: &[i64]) -> Result<Vec<f32>, Fault> {
        let ids = ort::value::TensorRef::from_array_view((vec![1, tokens.len()], tokens))
            .map_err(fault)?;
        let hidden =
            ort::value::TensorRef::from_array_view((self.hidden_shape.clone(), &self.hidden[..]))
                .map_err(fault)?;
        let out = self
            .decoder
            .run(ort::inputs!["input_ids" => ids, "encoder_hidden_states" => hidden])
            .map_err(fault)?;
        let (shape, data) = out["logits"].try_extract_tensor::<f32>().map_err(fault)?;
        let vocab = *shape
            .last()
            .ok_or_else(|| fault("the decoder returned no logits"))? as usize;
        Ok(data[data.len() - vocab..].to_vec())
    }
}

/// Whisper as an exported model, behind the same trait whisper.cpp's `Stt` is.
pub struct OnnxStt {
    runtime: Mutex<Box<dyn Runtime>>,
    specials: Specials,
    tokenizer: crate::bpe::Tokenizer,
    bins: usize,
    /// Set by [`crate::session::Transcribe::abandon`]; cleared as each window starts.
    stop: std::sync::atomic::AtomicBool,
}

impl OnnxStt {
    /// An `optimum` export of whisper: `config.json`, `generation_config.json`, `tokenizer.json`,
    /// and `onnx/encoder_model.onnx` and `onnx/decoder_model.onnx`.
    pub fn load(dir: &Path) -> Result<OnnxStt, Fault> {
        let read = |name: &str| {
            let path = dir.join(name);
            std::fs::read_to_string(&path).map_err(|e| fault(format!("{}: {e}", path.display())))
        };
        let config: serde_json::Value =
            serde_json::from_str(&read("config.json")?).map_err(fault)?;
        let bins = config["num_mel_bins"]
            .as_u64()
            .ok_or_else(|| fault("config.json has no num_mel_bins"))? as usize;
        let specials = Specials::from_generation_config(&read("generation_config.json")?)?;
        let tokenizer =
            crate::bpe::Tokenizer::from_json(&read("tokenizer.json")?).map_err(fault)?;
        Ok(OnnxStt::with(
            Box::new(Ort::load(dir, bins)?),
            specials,
            tokenizer,
            bins,
        ))
    }

    pub fn with(
        runtime: Box<dyn Runtime>,
        specials: Specials,
        tokenizer: crate::bpe::Tokenizer,
        bins: usize,
    ) -> OnnxStt {
        OnnxStt {
            runtime: Mutex::new(runtime),
            specials,
            tokenizer,
            bins,
            stop: Default::default(),
        }
    }

    /// Thirty seconds at a time, as whisper reads: the wake-word check hands over up to
    /// `session::watch_cap()`, 35 s, and what is past the first window is a second one.
    fn run(&self, audio: &[f32], expecting: Option<&str>) -> Result<String, Fault> {
        let mut heard = Vec::new();
        for window in audio.chunks(crate::mel::SAMPLES) {
            let text = self.window(window, expecting)?;
            if !text.is_empty() {
                heard.push(text);
            }
        }
        Ok(heard.join(" "))
    }

    fn window(&self, audio: &[f32], expecting: Option<&str>) -> Result<String, Fault> {
        let mel = crate::mel::log_mel(audio, self.bins);
        let mut runtime = self
            .runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Cleared here, under the lock, rather than when a reading ends: a stop meant for a
        // reading that had already finished must not end the one after it.
        self.stop.store(false, std::sync::atomic::Ordering::Relaxed);
        let hop = crate::mel::SAMPLES / crate::mel::FRAMES;
        runtime.encode(&mel, audio.len().div_ceil(hop).min(crate::mel::FRAMES))?;
        let language = detect_language(&mut **runtime, &self.specials)?;
        if let Some((code, _)) = self
            .specials
            .languages
            .iter()
            .find(|(_, id)| *id == language)
        {
            tracing::debug!(language = %code, "whisper detected the language");
        }
        let prompt = expecting
            .map(|words| self.tokenizer.encode(&format!(" {}", words.trim())))
            .unwrap_or_default();
        let ids = greedy(
            &mut **runtime,
            &self.specials,
            prefix(&self.specials, language, &prompt),
            &self.stop,
        )?;
        Ok(crate::stt::clean(&self.tokenizer.decode(&ids)))
    }
}

impl crate::session::Transcribe for OnnxStt {
    fn transcribe(&self, audio: &[f32]) -> Result<String, Fault> {
        self.run(audio, None)
    }

    fn transcribe_expecting(&self, audio: &[f32], phrase: &str) -> Result<String, Fault> {
        self.run(audio, Some(phrase))
    }

    fn abandon(&self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    pub(crate) const VOCAB: usize = 51_865;
    const KO: u32 = 50_264;
    const EN: u32 = 50_259;

    /// whisper-base's own numbers, cut down to what the tests touch.
    pub(crate) fn specials() -> Specials {
        Specials::from_generation_config(
            r#"{"eos_token_id": 50257, "decoder_start_token_id": 50258,
                "task_to_id": {"transcribe": 50359, "translate": 50358},
                "no_timestamps_token_id": 50363, "prev_sot_token_id": 50361,
                "lang_to_id": {"<|en|>": 50259, "<|ko|>": 50264},
                "suppress_tokens": [1, 2, 50358],
                "begin_suppress_tokens": [220, 50257]}"#,
        )
        .expect("the excerpt reads")
    }

    /// A runtime that answers from a script: at step `n`, each `(id, score)` of `steps[n]` gets
    /// that score and everything else 0; past the script it says end-of-text.
    pub(crate) struct Scripted {
        pub steps: Vec<Vec<(u32, f32)>>,
        pub lengths: Vec<usize>,
        pub encoded: usize,
        pub frames: Vec<usize>,
    }

    impl Scripted {
        pub(crate) fn saying(steps: Vec<Vec<(u32, f32)>>) -> Scripted {
            Scripted {
                steps,
                lengths: Vec::new(),
                encoded: 0,
                frames: Vec::new(),
            }
        }
    }

    impl Runtime for Scripted {
        fn encode(&mut self, _mel: &[f32], frames: usize) -> Result<(), Fault> {
            self.encoded += 1;
            self.frames.push(frames);
            Ok(())
        }
        fn next_logits(&mut self, tokens: &[i64]) -> Result<Vec<f32>, Fault> {
            assert!(
                tokens.len() <= MAX_POSITIONS,
                "the decoder was handed {} positions",
                tokens.len()
            );
            let step = self.lengths.len();
            self.lengths.push(tokens.len());
            let mut logits = vec![0.0; VOCAB];
            match self.steps.get(step) {
                Some(scores) => scores.iter().for_each(|(id, s)| logits[*id as usize] = *s),
                None => logits[50_257] = 10.0,
            }
            Ok(logits)
        }
    }

    #[test]
    fn the_generation_config_names_every_special_token() {
        let s = specials();
        assert_eq!(
            (s.eot, s.sot, s.transcribe, s.no_timestamps, s.prev),
            (50257, 50258, 50359, 50363, 50361)
        );
        assert_eq!(
            s.languages,
            vec![("en".to_string(), EN), ("ko".to_string(), KO)]
        );
    }

    #[test]
    fn the_language_is_the_likeliest_language_token() {
        let mut runtime = Scripted::saying(vec![vec![(EN, 4.0), (KO, 5.0), (1911, 9.0)]]);
        assert_eq!(
            detect_language(&mut runtime, &specials()).expect("detects"),
            KO
        );
        assert_eq!(
            runtime.lengths,
            vec![1],
            "detection reads only the start token"
        );
    }

    #[test]
    fn the_prefix_is_prompt_then_start_language_task() {
        assert_eq!(
            prefix(&specials(), KO, &[]),
            vec![50258, 50264, 50359, 50363]
        );
        assert_eq!(
            prefix(&specials(), KO, &[7, 8]),
            vec![50361, 7, 8, 50258, 50264, 50359, 50363]
        );
    }

    #[test]
    fn a_long_prompt_keeps_its_last_223_tokens() {
        let prompt: Vec<u32> = (0..500).collect();
        let tokens = prefix(&specials(), EN, &prompt);
        assert_eq!(tokens.len(), 1 + MAX_PROMPT + 4);
        assert_eq!(
            tokens[1],
            500 - MAX_PROMPT as i64,
            "the start of the prompt is what goes"
        );
    }

    /// The runtime is told how much of each window is audio: a short set is picked on it.
    #[test]
    fn each_window_says_how_many_of_its_frames_are_audio() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        struct Frames(std::sync::Arc<std::sync::Mutex<Vec<usize>>>);
        impl Runtime for Frames {
            fn encode(&mut self, _mel: &[f32], frames: usize) -> Result<(), Fault> {
                self.0.lock().unwrap().push(frames);
                Ok(())
            }
            fn next_logits(&mut self, _tokens: &[i64]) -> Result<Vec<f32>, Fault> {
                let mut logits = vec![0.0; VOCAB];
                logits[KO as usize] = 5.0;
                Ok(logits)
            }
        }
        let stt = OnnxStt::with(Box::new(Frames(seen.clone())), specials(), tiny_tokenizer(), 80);
        // Ten seconds exactly, ten and a bit, and thirty-five: the last is two windows.
        for samples in [160_000, 160_001, 560_000] {
            let _ = stt.transcribe(&vec![0.1; samples]);
        }
        assert_eq!(*seen.lock().unwrap(), [1000, 1001, 3000, 500]);
    }

    #[test]
    fn decoding_told_to_stop_stops_with_a_fault() {
        let mut runtime = Scripted::saying(vec![vec![(1911, 5.0)]; 1000]);
        let stop = AtomicBool::new(true);
        assert!(greedy(&mut runtime, &specials(), prefix(&specials(), EN, &[]), &stop).is_err());
        assert!(runtime.lengths.is_empty(), "not one step was run");
    }

    #[test]
    fn decoding_stops_at_end_of_text() {
        let mut runtime = Scripted::saying(vec![vec![(1911, 5.0)], vec![(1176, 5.0)]]);
        let out = greedy(&mut runtime, &specials(), prefix(&specials(), EN, &[]), &AtomicBool::new(false)).expect("decodes");
        assert_eq!(out, vec![1911, 1176]);
    }

    #[test]
    fn decoding_that_never_ends_stops_at_the_cap() {
        let mut runtime = Scripted::saying(vec![vec![(1911, 5.0)]; 1000]);
        let out = greedy(&mut runtime, &specials(), prefix(&specials(), EN, &[]), &AtomicBool::new(false)).expect("decodes");
        assert_eq!(out.len(), MAX_TOKENS);
    }

    #[test]
    fn a_long_prompt_and_a_long_answer_stay_inside_the_context() {
        let prompt: Vec<u32> = (0..500).collect();
        let mut runtime = Scripted::saying(vec![vec![(1911, 5.0)]; 1000]);
        greedy(&mut runtime, &specials(), prefix(&specials(), EN, &prompt), &AtomicBool::new(false)).expect("decodes");
        assert!(runtime.lengths.iter().all(|n| *n <= MAX_POSITIONS));
    }

    #[test]
    fn the_first_token_is_never_a_space_or_the_end() {
        let mut runtime = Scripted::saying(vec![vec![(220, 9.0), (50257, 8.0), (1911, 5.0)]]);
        let out = greedy(&mut runtime, &specials(), prefix(&specials(), EN, &[]), &AtomicBool::new(false)).expect("decodes");
        assert_eq!(out, vec![1911]);
    }

    #[test]
    fn special_and_suppressed_tokens_are_never_written() {
        let mut runtime = Scripted::saying(vec![vec![(50359, 9.0), (2, 8.0), (1911, 5.0)]]);
        let out = greedy(&mut runtime, &specials(), prefix(&specials(), EN, &[]), &AtomicBool::new(false)).expect("decodes");
        assert_eq!(out, vec![1911]);
    }

    use crate::session::Transcribe;

    fn tiny_tokenizer() -> crate::bpe::Tokenizer {
        crate::bpe::Tokenizer::from_json(
            r#"{"added_tokens": [], "model": {"type": "BPE",
                "vocab": {"Ġhello": 1911}, "merges": []}}"#,
        )
        .expect("reads")
    }

    #[test]
    fn a_turn_is_encoded_once_detected_and_decoded() {
        let runtime = Scripted::saying(vec![vec![(EN, 5.0)], vec![(1911, 5.0)]]);
        let stt = OnnxStt::with(Box::new(runtime), specials(), tiny_tokenizer(), 80);
        assert_eq!(
            stt.transcribe(&vec![0.1; 16_000]).expect("transcribes"),
            "hello"
        );
    }

    /// The wake-word check hands over up to `watch_cap()`, 35 s: the phrase and a whole request
    /// after it. Whisper reads 30 s at a time, so the rest is a second window, not dropped.
    #[test]
    fn more_than_thirty_seconds_is_read_in_windows() {
        let runtime = Scripted::saying(vec![
            vec![(EN, 5.0)],
            vec![(1911, 5.0)],
            vec![(50_257, 10.0)],
            vec![(EN, 5.0)],
            vec![(1911, 5.0)],
        ]);
        let stt = OnnxStt::with(Box::new(runtime), specials(), tiny_tokenizer(), 80);
        let heard = stt
            .transcribe(&vec![0.1; crate::mel::SAMPLES + 16_000])
            .expect("transcribes");
        assert_eq!(heard, "hello hello");
    }

    #[test]
    fn no_audio_is_no_text_and_no_model_run() {
        struct Untouchable;
        impl Runtime for Untouchable {
            fn encode(&mut self, _mel: &[f32], _frames: usize) -> Result<(), Fault> {
                panic!("the model ran on no audio")
            }
            fn next_logits(&mut self, _tokens: &[i64]) -> Result<Vec<f32>, Fault> {
                panic!("the model ran on no audio")
            }
        }
        let stt = OnnxStt::with(Box::new(Untouchable), specials(), tiny_tokenizer(), 80);
        assert_eq!(stt.transcribe(&[]).expect("answers"), "");
    }

    #[test]
    fn a_directory_missing_a_file_says_which() {
        let dir = std::env::temp_dir().join(format!("zyris-onnx-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let error = OnnxStt::load(&dir)
            .err()
            .expect("an empty directory is not a model")
            .to_string();
        std::fs::remove_dir_all(&dir).ok();
        assert!(error.contains("config.json"), "{error}");
    }
}
