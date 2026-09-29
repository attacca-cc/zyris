//! Whisper's tokenizer: GPT-2's byte-level BPE, read from a Hugging Face `tokenizer.json`.
//!
//! Decoding is what every transcript needs. Encoding is needed only for the prompt that
//! `Transcribe::transcribe_expecting` passes: the wake phrase, or the vocabulary.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

/// GPT-2's pre-tokenizer, without its `\s+(?!\S)` alternative: the `regex` crate has no
/// look-ahead, so [`pieces`] does that part by hand.
static PIECES: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"'s|'t|'re|'ve|'m|'ll|'d| ?\p{L}+| ?\p{N}+| ?[^\s\p{L}\p{N}]+|\s+")
        .expect("the pattern is valid")
});

pub struct Tokenizer {
    ids: HashMap<String, u32>,
    tokens: HashMap<u32, String>,
    ranks: HashMap<(String, String), usize>,
    special: HashSet<u32>,
    byte_char: [char; 256],
    char_byte: HashMap<char, u8>,
}

impl Tokenizer {
    pub fn from_json(json: &str) -> Result<Tokenizer, String> {
        let root: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let model = &root["model"];
        let vocab = model["vocab"]
            .as_object()
            .ok_or("tokenizer.json has no model.vocab")?;
        let mut ids = HashMap::new();
        for (token, id) in vocab {
            ids.insert(
                token.clone(),
                id.as_u64().ok_or("a vocab id is not a number")? as u32,
            );
        }
        let mut special = HashSet::new();
        for added in root["added_tokens"].as_array().into_iter().flatten() {
            let id = added["id"].as_u64().ok_or("an added token has no id")? as u32;
            let content = added["content"]
                .as_str()
                .ok_or("an added token has no content")?;
            ids.insert(content.to_string(), id);
            if added["special"].as_bool() == Some(true) {
                special.insert(id);
            }
        }
        let merges = model["merges"]
            .as_array()
            .ok_or("tokenizer.json has no merges")?;
        let mut ranks = HashMap::new();
        for (rank, merge) in merges.iter().enumerate() {
            let pair = match merge {
                serde_json::Value::String(s) => s
                    .split_once(' ')
                    .map(|(a, b)| (a.to_string(), b.to_string())),
                serde_json::Value::Array(p) => {
                    match (
                        p.first().and_then(|v| v.as_str()),
                        p.get(1).and_then(|v| v.as_str()),
                    ) {
                        (Some(a), Some(b)) => Some((a.to_string(), b.to_string())),
                        _ => None,
                    }
                }
                _ => None,
            };
            ranks.insert(pair.ok_or("a merge is not a pair")?, rank);
        }
        let byte_char = byte_chars();
        let char_byte = byte_char
            .iter()
            .enumerate()
            .map(|(b, c)| (*c, b as u8))
            .collect();
        let tokens = ids.iter().map(|(t, id)| (*id, t.clone())).collect();
        Ok(Tokenizer {
            ids,
            tokens,
            ranks,
            special,
            byte_char,
            char_byte,
        })
    }

    /// Plain text to ids, with no special tokens added.
    pub fn encode(&self, text: &str) -> Vec<u32> {
        let mut out = Vec::new();
        for piece in pieces(text) {
            let mut symbols: Vec<String> = piece
                .bytes()
                .map(|b| self.byte_char[b as usize].to_string())
                .collect();
            loop {
                let best = symbols
                    .windows(2)
                    .enumerate()
                    .filter_map(|(i, w)| {
                        self.ranks
                            .get(&(w[0].clone(), w[1].clone()))
                            .map(|r| (*r, i))
                    })
                    .min();
                let Some((_, i)) = best else { break };
                let merged = format!("{}{}", symbols[i], symbols[i + 1]);
                symbols.splice(i..i + 2, [merged]);
            }
            out.extend(symbols.iter().filter_map(|s| self.ids.get(s)));
        }
        out
    }

    /// Ids to text, leaving special tokens out. The bytes of every token are joined before they
    /// are read as UTF-8, because one character can be split across tokens.
    pub fn decode(&self, ids: &[u32]) -> String {
        let mut bytes = Vec::new();
        for id in ids.iter().filter(|id| !self.special.contains(id)) {
            let Some(token) = self.tokens.get(id) else {
                continue;
            };
            for c in token.chars() {
                match self.char_byte.get(&c) {
                    Some(b) => bytes.push(*b),
                    None => {
                        let mut buffer = [0u8; 4];
                        bytes.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
                    }
                }
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

/// GPT-2's pieces. What the regex leaves out, `\s+(?!\S)`, is done here: a run of whitespace
/// followed by text gives up its last character. A space joins the next piece (" and"); any
/// other character stands alone.
fn pieces(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < text.len() {
        let Some(found) = PIECES.find_at(text, at) else {
            break;
        };
        let piece = found.as_str();
        if found.end() < text.len() && piece.chars().all(char::is_whitespace) {
            let last = piece.chars().next_back().expect("a match is never empty");
            let cut = found.end() - last.len_utf8();
            if cut > found.start() {
                out.push(&text[found.start()..cut]);
            }
            if last == ' ' {
                at = cut;
            } else {
                out.push(&text[cut..found.end()]);
                at = found.end();
            }
            continue;
        }
        out.push(piece);
        at = found.end();
    }
    out
}

/// GPT-2's `bytes_to_unicode`: printable bytes stand for themselves, and the rest are moved up
/// past U+0100 so that every byte is a visible character.
fn byte_chars() -> [char; 256] {
    let mut table = ['\0'; 256];
    let mut shifted = 0u32;
    for b in 0..256u32 {
        let printable =
            (33..=126).contains(&b) || (161..=172).contains(&b) || (174..=255).contains(&b);
        table[b as usize] = if printable {
            char::from_u32(b).expect("a byte is a char")
        } else {
            shifted += 1;
            char::from_u32(255 + shifted).expect("under U+0200")
        };
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tokenizer small enough to reason about: `Ġ` is the byte-level space.
    fn tiny() -> Tokenizer {
        Tokenizer::from_json(
            r#"{
              "added_tokens": [{"id": 6, "content": "<|endoftext|>", "special": true}],
              "model": {
                "type": "BPE",
                "vocab": {"a": 0, "b": 1, "ab": 2, "Ġ": 3, "Ġab": 4, "c": 5},
                "merges": ["a b", "Ġ ab"]
              }
            }"#,
        )
        .expect("the tiny tokenizer reads")
    }

    #[test]
    fn merges_apply_in_rank_order() {
        assert_eq!(tiny().encode(" ab c"), vec![4, 3, 5]);
    }

    #[test]
    fn special_tokens_are_left_out_of_text() {
        assert_eq!(tiny().decode(&[4, 6, 3, 5]), " ab c");
    }

    #[test]
    fn merges_may_be_written_as_pairs() {
        let pairs = Tokenizer::from_json(
            r#"{"added_tokens": [], "model": {"type": "BPE",
                "vocab": {"a": 0, "b": 1, "ab": 2}, "merges": [["a", "b"]]}}"#,
        )
        .expect("reads");
        assert_eq!(pairs.encode("ab"), vec![2]);
    }

    /// The real whisper tokenizer, when `ZYRIS_ONNX_WHISPER` names the exported model directory.
    /// The ids are what Hugging Face's `tokenizers` gives for the same strings (2026-09-29).
    fn whisper() -> Option<Tokenizer> {
        let dir = std::path::PathBuf::from(std::env::var_os("ZYRIS_ONNX_WHISPER")?);
        let json = std::fs::read_to_string(dir.join("tokenizer.json")).ok()?;
        Some(Tokenizer::from_json(&json).expect("whisper's tokenizer.json reads"))
    }

    #[test]
    fn prompts_encode_as_hugging_face_encodes_them() {
        let Some(tokenizer) = whisper() else {
            eprintln!("skipped: set ZYRIS_ONNX_WHISPER to the exported model directory");
            return;
        };
        assert_eq!(tokenizer.encode(" Hey Zyris"), vec![1911, 1176, 88, 5714]);
        assert_eq!(
            tokenizer.encode(" Agent / 에이전트"),
            vec![27174, 2460, 20122, 3946, 254, 226, 8857]
        );
        // Runs of spaces keep their last space for the next word; a trailing newline stays whole.
        assert_eq!(
            tokenizer.encode(" GitHub, Attacca  and   spaces\n"),
            vec![23331, 11, 7298, 326, 496, 220, 293, 220, 220, 7673, 198]
        );
    }

    /// Hangul syllables are three bytes and are split across tokens (254 and 226 above are
    /// halves of one syllable). Decoding joins the bytes first, or the text fills with U+FFFD.
    #[test]
    fn a_syllable_split_across_tokens_decodes_whole() {
        let Some(tokenizer) = whisper() else {
            eprintln!("skipped: set ZYRIS_ONNX_WHISPER to the exported model directory");
            return;
        };
        let text = tokenizer.decode(&[50258, 27174, 2460, 20122, 3946, 254, 226, 8857, 50257]);
        assert_eq!(text, " Agent / 에이전트");
    }
}
