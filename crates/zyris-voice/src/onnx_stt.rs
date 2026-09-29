//! Whisper as an exported model: the decoding every runtime shares, over a two-method
//! [`Runtime`].

use crate::stt::Fault;

pub const MAX_TOKENS: usize = 224;
pub const MAX_PROMPT: usize = 223;
pub const MAX_POSITIONS: usize = 448;

pub trait Runtime: Send {
    fn encode(&mut self, mel: &[f32]) -> Result<(), Fault>;
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
pub fn greedy(
    runtime: &mut dyn Runtime,
    specials: &Specials,
    prefix: Vec<i64>,
) -> Result<Vec<u32>, Fault> {
    let mut tokens = prefix;
    let mut out = Vec::new();
    for step in 0..MAX_TOKENS {
        if tokens.len() >= MAX_POSITIONS {
            break;
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

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
    }

    impl Scripted {
        pub(crate) fn saying(steps: Vec<Vec<(u32, f32)>>) -> Scripted {
            Scripted {
                steps,
                lengths: Vec::new(),
                encoded: 0,
            }
        }
    }

    impl Runtime for Scripted {
        fn encode(&mut self, _mel: &[f32]) -> Result<(), Fault> {
            self.encoded += 1;
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

    #[test]
    fn decoding_stops_at_end_of_text() {
        let mut runtime = Scripted::saying(vec![vec![(1911, 5.0)], vec![(1176, 5.0)]]);
        let out = greedy(&mut runtime, &specials(), prefix(&specials(), EN, &[])).expect("decodes");
        assert_eq!(out, vec![1911, 1176]);
    }

    #[test]
    fn decoding_that_never_ends_stops_at_the_cap() {
        let mut runtime = Scripted::saying(vec![vec![(1911, 5.0)]; 1000]);
        let out = greedy(&mut runtime, &specials(), prefix(&specials(), EN, &[])).expect("decodes");
        assert_eq!(out.len(), MAX_TOKENS);
    }

    #[test]
    fn a_long_prompt_and_a_long_answer_stay_inside_the_context() {
        let prompt: Vec<u32> = (0..500).collect();
        let mut runtime = Scripted::saying(vec![vec![(1911, 5.0)]; 1000]);
        greedy(&mut runtime, &specials(), prefix(&specials(), EN, &prompt)).expect("decodes");
        assert!(runtime.lengths.iter().all(|n| *n <= MAX_POSITIONS));
    }

    #[test]
    fn the_first_token_is_never_a_space_or_the_end() {
        let mut runtime = Scripted::saying(vec![vec![(220, 9.0), (50257, 8.0), (1911, 5.0)]]);
        let out = greedy(&mut runtime, &specials(), prefix(&specials(), EN, &[])).expect("decodes");
        assert_eq!(out, vec![1911]);
    }

    #[test]
    fn special_and_suppressed_tokens_are_never_written() {
        let mut runtime = Scripted::saying(vec![vec![(50359, 9.0), (2, 8.0), (1911, 5.0)]]);
        let out = greedy(&mut runtime, &specials(), prefix(&specials(), EN, &[])).expect("decodes");
        assert_eq!(out, vec![1911]);
    }
}
