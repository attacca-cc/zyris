//! Supertonic 3: the files it needs on disk, the four ONNX graphs, and the normalisation
//! without which most of the world's text is silently unsayable.
//!
//! # What it is
//!
//! A 99M-parameter flow-matching text-to-speech model, released by Supertone and **archived**
//! (`supertone-oss-archive/supertonic`, 2026-07-23; the vendor moved to a hosted API). The
//! code is MIT and the **weights are BigScience OpenRAIL-M**: shippable, closed-source
//! compatible, and carrying use-based restrictions that travel with redistribution. That is
//! not a detail to keep in a manifest — see [`WEIGHTS_LICENCE`], which the README repeats.
//!
//! Six files, **398 MB**, plus ten voice styles of about 292 KB each. Output is 44.1 kHz mono.
//!
//! # The shape of one utterance
//!
//! ```text
//! text -> prepare -> Indexer -> ids
//!                                +--> duration_predictor -> seconds
//!                                +--> text_encoder       -> text_emb
//!                                                            |
//!             N(0,1) latent, one frame per 3072 samples  ----+--> vector_estimator x TOTAL_STEP
//!                                                                   |
//!                                                                   +--> vocoder -> 44.1 kHz f32
//! ```
//!
//! [`TOTAL_STEP`] and [`SPEED`] are the reference implementation's own defaults, read out of
//! `rust/src/example_onnx.rs` in the archived repository rather than guessed.
//!
//! # The trap this module exists for
//!
//! `unicode_indexer.json` is a flat table of 65,536 entries, codepoint to token id, where
//! `-1` means "this model has no token for that character". **8,321 of the 65,536 are
//! mapped**, and the ones that are not include:
//!
//! - **every one of the 11,172 precomposed Hangul syllables** (가…힣) and every compatibility
//!   jamo — while all **69 conjoining jamo** U+1100–U+11FF, which is exactly the NFD set, are
//!   mapped to the contiguous ids 560–628;
//! - **every precomposed accented Latin letter** — `é`, `ü`, `ñ` are all `-1` — while the
//!   bases and the combining marks they decompose to are mapped (`e` 64, U+0301 146).
//!
//! Attacca sends NFC. Feed it straight in and Korean is entirely absent and French is mangled,
//! **and nothing says so**: `-1` is not an error, it is an index, and ONNX `Gather` reads a
//! negative index as counting from the end of the embedding table. So the failure is a
//! confident, fluent, wrong voice.
//!
//! [`prepare`] therefore runs NFKD first, exactly as the reference does. The plan for this step
//! said Hangul NFD is algorithmic and needs no table, which is true and was **not enough**: the
//! accented-Latin half above was measured on 2026-09-15 and a hand-rolled Hangul-only
//! decomposition would have left every European language quietly worse.
//!
//! A codepoint still unmapped after that — an emoji, a rare CJK ideograph — is **not dropped
//! in silence**; see [`Spoken`].

use std::path::{Path, PathBuf};

use crate::model::{self, Model, ModelState};

// ---------------------------------------------------------------------------------------------
// The model, as numbers
// ---------------------------------------------------------------------------------------------

/// What the vocoder produces. From `tts.json`'s `ae.sample_rate`.
///
/// **Not [`crate::capture::SAMPLE_RATE`].** Capture is 16 kHz because whisper wants it; this is
/// 44.1 kHz because the vocoder produces it, and the two meeting is task 4's problem.
pub const SAMPLE_RATE: u32 = 44_100;

/// `tts.json`'s `ae.base_chunk_size` — the autoencoder's hop, in samples.
pub const BASE_CHUNK: usize = 512;

/// `tts.json`'s `ttl.chunk_compress_factor`.
pub const CHUNK_COMPRESS: usize = 6;

/// `tts.json`'s `ttl.latent_dim`.
pub const LATENT_DIM: usize = 24;

/// The latent's channel count: [`LATENT_DIM`] × [`CHUNK_COMPRESS`] = 144.
pub const LATENT_WIDTH: usize = LATENT_DIM * CHUNK_COMPRESS;

/// Samples of audio per latent frame: 3072, which is **69.66 ms**.
///
/// The granularity of everything this model does. An utterance shorter than one frame still
/// costs one.
pub const SAMPLES_PER_FRAME: usize = BASE_CHUNK * CHUNK_COMPRESS;

/// How many times the vector estimator runs. The reference implementation's default.
///
/// Measured here over 0.557 s of audio: 0.54 s at 2 steps, 0.59 s at 4, 0.79–0.87 s at 8,
/// 1.29 s at 16. **The floor is not the step count** — two steps buy 250 ms out of 800 and cost
/// the quality the model was released with — so the reference default stays and task 2 argues
/// the *fragment length* instead.
pub const TOTAL_STEP: usize = 8;

/// Speech rate. The reference implementation's default: the predicted duration is divided by it,
/// so above 1.0 is faster.
pub const SPEED: f32 = 1.05;

/// The voice used when nothing has chosen one. The reference's own default.
pub const DEFAULT_VOICE: &str = "M1";

/// What has to travel with the weights wherever they go.
///
/// The code is MIT; the **weights** are not. This is a constant rather than a sentence in the
/// README because more than one screen will have to say it and two spellings of a licence
/// obligation is one spelling too many.
pub const WEIGHTS_LICENCE: &str = "Supertonic 3's weights are licensed BigScience OpenRAIL-M: \
     free to use and to redistribute, including commercially, with use-based restrictions that \
     must be passed on with any copy. The example code is MIT.";

/// The archived snapshot every file below is pinned to.
///
/// The README of the archived repository names this revision as the one its examples are run
/// against. Pinning a commit rather than `main` is what makes each `sha256` below a second lock
/// rather than the only one — the same rule [`crate::stt::BASE`] follows.
pub const REVISION: &str = "aafc6e32416a594460b32413efc49d7fe4ce6d46";

macro_rules! supertonic {
    ($path:literal) => {
        concat!(
            "https://huggingface.co/supertone-oss-archive/supertonic-3/resolve/",
            "aafc6e32416a594460b32413efc49d7fe4ce6d46",
            "/",
            $path
        )
    };
}

/// The six files the model itself is. Sizes and digests read from the pinned revision on
/// 2026-09-15 and checked against the copies measured on this machine.
pub const FILES: [Model; 6] = [
    Model {
        url: supertonic!("onnx/duration_predictor.onnx"),
        file: "duration_predictor.onnx",
        bytes: 3_700_147,
        sha256: "c3eb91414d5ff8a7a239b7fe9e34e7e2bf8a8140d8375ffb14718b1c639325db",
    },
    Model {
        url: supertonic!("onnx/text_encoder.onnx"),
        file: "text_encoder.onnx",
        bytes: 36_416_150,
        sha256: "c7befd5ea8c3119769e8a6c1486c4edc6a3bc8365c67621c881bbb774b9902ff",
    },
    Model {
        url: supertonic!("onnx/vector_estimator.onnx"),
        file: "vector_estimator.onnx",
        bytes: 256_534_781,
        sha256: "883ac868ea0275ef0e991524dc64f16b3c0376efd7c320af6b53f5b780d7c61c",
    },
    Model {
        url: supertonic!("onnx/vocoder.onnx"),
        file: "vocoder.onnx",
        bytes: 101_424_195,
        sha256: "085de76dd8e8d5836d6ca66826601f615939218f90e519f70ee8a36ed2a4c4ba",
    },
    Model {
        url: supertonic!("onnx/tts.json"),
        file: "tts.json",
        bytes: 8_253,
        sha256: "42078d3aef1cd43ab43021f3c54f47d2d75ceb4e75f627f118890128b06a0d09",
    },
    Model {
        url: supertonic!("onnx/unicode_indexer.json"),
        file: "unicode_indexer.json",
        bytes: 277_676,
        sha256: "9bf7346e43883a81f8645c81224f786d43c5b57f3641f6e7671a7d6c493cb24f",
    },
];

/// One shipped voice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Voice {
    /// What a person and a settings file call it: `M1`, `F3`.
    pub name: &'static str,
    /// The file it lives in.
    pub model: Model,
}

/// The ten voices Supertonic ships.
///
/// **All ten are required, not just [`DEFAULT_VOICE`]**, and that is a decision: together they
/// are 2.9 MB against the model's 398, and a voice picker that had to download before it could
/// offer a choice would be a control that appears to do nothing for as long as the network takes.
pub const VOICES: [Voice; 10] = [
    voice("F1", supertonic!("voice_styles/F1.json"), "F1.json", 292_046, "bbdec6ee00231c2c742ad05483df5334cab3b52fda3ba38e6a07059c4563dbc2"),
    voice("F2", supertonic!("voice_styles/F2.json"), "F2.json", 292_423, "7c722c6a72707b1a77f035d67f0d1351ba187738e06f7683e8c72b1df3477fc6"),
    voice("F3", supertonic!("voice_styles/F3.json"), "F3.json", 290_794, "12f6ef2573baa2defa1128069cb59f203e3ab67c92af77b42df8a0e3a2f7c6ab"),
    voice("F4", supertonic!("voice_styles/F4.json"), "F4.json", 291_808, "c2fa764c1225a76dfc3e2c73e8aa4f70d9ee48793860eb34c295fff01c2e032b"),
    voice("F5", supertonic!("voice_styles/F5.json"), "F5.json", 291_479, "45966e73316415626cf41a7d1c6f3b4c70dbc1ba2bee5c1978ef0ce33244fc8d"),
    voice("M1", supertonic!("voice_styles/M1.json"), "M1.json", 291_748, "e35604687f5d23694b8e91593a93eec0e4eca6c0b02bb8ed69139ab2ea6b0a5b"),
    voice("M2", supertonic!("voice_styles/M2.json"), "M2.json", 292_055, "b76cbf62bac707c710cf0ae5aba5e31eea1a6339a9734bfae33ab98499534a50"),
    voice("M3", supertonic!("voice_styles/M3.json"), "M3.json", 290_198, "ea1ac35ccb91b0d7ecad533a2fbd0eec10c91513d8951e3b25fbba99954e159b"),
    voice("M4", supertonic!("voice_styles/M4.json"), "M4.json", 291_522, "ca8eefad4fcd989c9379032ff3e50738adc547eeb5e221b82593a6d7b3bac303"),
    voice("M5", supertonic!("voice_styles/M5.json"), "M5.json", 291_469, "dd22b92740314321f8ae11c5e87f8dd60d060f15dd3a632b5adf77f471f77af2"),
];

const fn voice(
    name: &'static str,
    url: &'static str,
    file: &'static str,
    bytes: u64,
    sha256: &'static str,
) -> Voice {
    Voice { name, model: Model { url, file, bytes, sha256 } }
}

/// Every file that has to be on disk before a word can be spoken, model and voices together.
pub fn required() -> Vec<Model> {
    FILES.iter().copied().chain(VOICES.iter().map(|v| v.model)).collect()
}

/// What the whole download costs, in bytes. The screen shows it before asking.
pub fn total_bytes() -> u64 {
    required().iter().map(|m| m.bytes).sum()
}

// ---------------------------------------------------------------------------------------------
// Where the files live
// ---------------------------------------------------------------------------------------------

/// A directory of its own under the models cache, because there are sixteen files and one of
/// them is called `tts.json`.
pub const DIRECTORY: &str = "supertonic-3";

/// Names a directory holding the files, bypassing the cache entirely.
///
/// Two callers, the same two as [`crate::stt::MODEL_ENV`]: this crate's own tests, which must
/// not download 398 MB to run, and somebody who already has the archive unpacked somewhere.
/// Files named this way are **still size-checked**, unlike whisper's: `ZYRIS_WHISPER_MODEL`
/// points at *one* file an operator may deliberately have chosen a different size of, while this
/// points at a directory that either is the archived snapshot or is not.
pub const MODELS_ENV: &str = "ZYRIS_TTS_MODELS";

/// Where the files should be.
pub fn models_dir() -> Option<PathBuf> {
    models_dir_given(std::env::var_os(MODELS_ENV))
}

/// The same decision with the environment passed in, for the reason [`crate::stt::model_path_given`]
/// gives: `set_var` is process-wide and a test that used it would be changing what every other
/// test on the same process sees, at whatever moment the scheduler chose.
pub fn models_dir_given(named: Option<std::ffi::OsString>) -> Option<PathBuf> {
    if let Some(named) = named {
        let named = PathBuf::from(named);
        if !named.as_os_str().is_empty() {
            return Some(named);
        }
    }
    Some(model::cache_dir()?.join(DIRECTORY))
}

/// What is on disk where the voice should be. **Four answers, not two.**
///
/// The fifth thing in this workspace to need this shape, after the audit tail, the inbox, the
/// MCP server list, the wake word store and `stt`'s own [`ModelState`]: "nothing there" and
/// "there and unreadable" send a person to different places, and a **Download** button in front
/// of somebody a download cannot help is the confident false negative this project keeps
/// shipping when it collapses them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceState {
    /// Every file is there at the size it should be.
    Ready { dir: PathBuf },
    /// Some are absent or the wrong size. Fetching replaces exactly those.
    ///
    /// A file of the wrong size is in `missing` rather than in a fifth arm: the answer is the
    /// same — fetch it again — and [`model::fetch`] writes over it atomically.
    Incomplete { dir: PathBuf, missing: Vec<String>, bytes: u64 },
    /// Something is in the way and could not be looked at: a directory where a file should be,
    /// a permission this user does not have, a mount that has gone.
    Unreadable { dir: PathBuf, detail: String },
    /// There is no directory to keep them in and none was named.
    Nowhere { reason: String },
}

/// What the window asks: can this machine speak?
pub fn state() -> VoiceState {
    match models_dir() {
        Some(dir) => state_in(&dir),
        None => VoiceState::Nowhere {
            reason: "this system does not name a cache directory for downloaded files".into(),
        },
    }
}

/// The same question about one named directory.
pub fn state_in(dir: &Path) -> VoiceState {
    let mut missing = Vec::new();
    let mut bytes = 0;
    for model in required() {
        match model::inspect(&dir.join(model.file), Some(model.bytes)) {
            ModelState::Ready { .. } => {}
            ModelState::Absent { .. } | ModelState::Damaged { .. } => {
                missing.push(model.file.to_string());
                bytes += model.bytes;
            }
            ModelState::Unreadable { path, detail } => {
                return VoiceState::Unreadable {
                    dir: dir.to_path_buf(),
                    detail: format!("{}: {detail}", path.display()),
                };
            }
            // `inspect` answers this only when it was handed no path at all, which cannot
            // happen here: the directory is the caller's.
            ModelState::Nowhere { reason } => {
                return VoiceState::Unreadable { dir: dir.to_path_buf(), detail: reason };
            }
        }
    }
    if missing.is_empty() {
        VoiceState::Ready { dir: dir.to_path_buf() }
    } else {
        VoiceState::Incomplete { dir: dir.to_path_buf(), missing, bytes }
    }
}

/// Download whatever is not already there, one file at a time.
///
/// Each file goes through [`model::fetch`], so each is either complete and verified under its
/// own name or absent — there is no half-written `vector_estimator.onnx` at any point. `progress`
/// is called with the total across the whole set, not per file, because that is the only number
/// worth showing for a 398 MB download in sixteen pieces.
pub async fn fetch_missing(
    dir: &Path,
    mut progress: impl FnMut(model::Progress),
) -> Result<(), model::Fault> {
    let wanted = required();
    let total: u64 = wanted.iter().map(|m| m.bytes).sum();
    let mut done: u64 = 0;
    for model in wanted {
        match model::inspect(&dir.join(model.file), Some(model.bytes)) {
            ModelState::Ready { .. } => {
                done += model.bytes;
                progress(model::Progress { received: done, total: Some(total) });
                continue;
            }
            _ => {}
        }
        let already = done;
        model::fetch(&model, dir, |p| {
            progress(model::Progress { received: already + p.received, total: Some(total) })
        })
        .await?;
        done += model.bytes;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------------------------

/// The languages the model was trained on, as the archived reference lists them.
///
/// There is **no language embedding** — `tts.json` says `"n_langs": 0, "lang_emb_dim": 0` — so
/// the language is not a parameter. [`prepare`] wraps the text in `<en>…</en>` and the model
/// reads those four characters as characters, like any others. That is why an unknown tag would
/// not fail: it would simply be spoken at.
pub const LANGUAGES: [&str; 32] = [
    "en", "ko", "ja", "ar", "bg", "cs", "da", "de", "el", "es", "et", "fi", "fr", "hi", "hr",
    "hu", "id", "it", "lt", "lv", "nl", "pl", "pt", "ro", "ru", "sk", "sl", "sv", "tr", "uk",
    "vi", "na",
];

/// The language a fragment is read in when its script does not say otherwise.
pub const LANGUAGE: &str = "en";

/// The language to read `text` in, judged from the script it is written in.
///
/// **From the text and not from what the person said**, because the answer is what is being
/// read, and an agent asked in Korean may still answer a line in English. Per fragment, since
/// the splitter hands over a sentence or so at a time and an answer can change language between
/// two of them.
///
/// Hangul reads as Korean even among Latin words — "Rust의 borrow checker는" is a Korean
/// sentence — and kana as Japanese. Everything else is [`LANGUAGE`]: the other thirty languages
/// share the Latin, Cyrillic or Greek scripts with each other, and telling them apart is a
/// language-identification problem this does not pretend to solve.
pub fn language_for(text: &str) -> &'static str {
    let (mut hangul, mut kana) = (0usize, 0usize);
    for c in text.chars() {
        match c {
            '\u{AC00}'..='\u{D7A3}' | '\u{1100}'..='\u{11FF}' | '\u{3130}'..='\u{318F}' => hangul += 1,
            '\u{3040}'..='\u{30FF}' => kana += 1,
            _ => {}
        }
    }
    if hangul > 0 && hangul >= kana {
        "ko"
    } else if kana > 0 {
        "ja"
    } else {
        LANGUAGE
    }
}

/// Whether the model knows this language tag.
pub fn is_language(lang: &str) -> bool {
    LANGUAGES.contains(&lang)
}

/// Normalise one fragment into what the model was trained to read.
///
/// **NFKD first, and that is the whole point of this function** — see the module documentation.
/// After that: the punctuation substitutions the reference makes, whitespace collapsed, a full
/// stop added if the fragment does not end in punctuation (the model runs on before it otherwise),
/// and the language tags.
///
/// Emoji are deliberately **not** stripped here, unlike in the reference. They are unmapped, so
/// they fall through to [`Indexer::encode`], which reports them rather than deleting them in
/// silence. One rule instead of two, and the one that leaves a trace.
pub fn normalise(text: &str) -> String {
    use unicode_normalization::UnicodeNormalization as _;

    let mut text: String = text.nfkd().collect();

    for (from, to) in [
        ("\u{2013}", "-"),
        ("\u{2011}", "-"),
        ("\u{2014}", "-"),
        ("_", " "),
        ("\u{201C}", "\""),
        ("\u{201D}", "\""),
        ("\u{2018}", "'"),
        ("\u{2019}", "'"),
        ("\u{00B4}", "'"),
        ("`", "'"),
        ("[", " "),
        ("]", " "),
        ("|", " "),
        ("/", " "),
        ("#", " "),
        ("\u{2192}", " "),
        ("\u{2190}", " "),
        ("@", " at "),
    ] {
        if text.contains(from) {
            text = text.replace(from, to);
        }
    }
    for symbol in ["\u{2665}", "\u{2606}", "\u{2661}", "\u{00A9}", "\\"] {
        if text.contains(symbol) {
            text = text.replace(symbol, "");
        }
    }
    for (from, to) in
        [(" ,", ","), (" .", "."), (" !", "!"), (" ?", "?"), (" ;", ";"), (" :", ":"), (" '", "'")]
    {
        while text.contains(from) {
            text = text.replace(from, to);
        }
    }
    while text.contains("\"\"") {
        text = text.replace("\"\"", "\"");
    }
    while text.contains("''") {
        text = text.replace("''", "'");
    }

    let mut collapsed = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            space = true;
            continue;
        }
        if space && !collapsed.is_empty() {
            collapsed.push(' ');
        }
        space = false;
        collapsed.push(c);
    }
    collapsed
}

/// [`normalise`], plus the ending the model needs and the tags it reads as characters.
///
/// The two are separate because the caller has to be able to ask whether there is anything here
/// worth speaking **before** the tags and the full stop are added — three or four tokens that are
/// always speakable and would make every fragment look like one.
pub fn prepare(text: &str, lang: &str) -> String {
    let mut body = normalise(text);
    if !body.is_empty() && !ends_a_sentence(&body) {
        body.push('.');
    }
    format!("<{lang}>{body}</{lang}>")
}

/// Whether a normalised body has anything in it the voice can say.
///
/// Whitespace does not count: a fragment of nothing but spaces is not something to spend 800 ms
/// of a flow-matching model on.
pub fn speakable(indexer: &Indexer, body: &str) -> bool {
    body.chars().any(|c| !c.is_whitespace() && indexer.id(c).is_some())
}

/// Whether the fragment already ends in something the model reads as an ending.
///
/// The reference's own list, including the quotes and the CJK closers, because a sentence that
/// ends `…said "no."` must not become `…said "no.".`
fn ends_a_sentence(text: &str) -> bool {
    matches!(
        text.chars().next_back(),
        Some(
            '.' | '!'
                | '?'
                | ';'
                | ':'
                | ','
                | '\''
                | '"'
                | '\u{201C}'
                | '\u{201D}'
                | '\u{2018}'
                | '\u{2019}'
                | ')'
                | ']'
                | '}'
                | '\u{2026}'
                | '\u{3002}'
                | '\u{300D}'
                | '\u{300F}'
                | '\u{3011}'
                | '\u{3009}'
                | '\u{300B}'
                | '\u{203A}'
                | '\u{00BB}'
        )
    )
}

/// One fragment turned into token ids, **and what could not be turned into one**.
///
/// The second half is the point. 8,321 of 65,536 codepoints are mapped, so an unmapped one is
/// reachable with ordinary text — an emoji, a rare ideograph, a private-use glyph — and step 6
/// of this project settled what happens then: **not a silent drop**, because nothing downstream
/// can tell a character that was thrown away from one the model chose not to voice.
///
/// So an unmapped codepoint becomes **a single space** — a word boundary, so that the words
/// either side do not fuse into one — and is recorded in `unvoiced`, in the order it was first
/// seen, once each. A caller hands that to a person; nothing here decides how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spoken {
    /// What goes to the model.
    pub ids: Vec<i64>,
    /// The distinct characters that had no token, in the order they first appeared.
    pub unvoiced: Vec<char>,
}

/// The codepoint-to-token table, as it is on disk.
pub struct Indexer {
    table: Vec<i32>,
}

impl Indexer {
    /// Read `unicode_indexer.json`.
    pub fn read(path: &Path) -> Result<Indexer, Fault> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Fault::Unusable { path: path.to_path_buf(), detail: e.to_string() })?;
        let table: Vec<i32> = serde_json::from_str(&text)
            .map_err(|e| Fault::Unusable { path: path.to_path_buf(), detail: e.to_string() })?;
        Ok(Indexer::new(table))
    }

    /// From a table already in memory. The tests build small ones.
    pub fn new(table: Vec<i32>) -> Indexer {
        Indexer { table }
    }

    /// How many codepoints this table can say.
    pub fn mapped(&self) -> usize {
        self.table.iter().filter(|&&id| id >= 0).count()
    }

    /// The token for one character, or `None` when the model has none.
    ///
    /// **`None` and not the raw `-1`.** A `-1` handed on is an index ONNX reads as "the last row
    /// of the embedding table", which is a real token belonging to a real character — so passing
    /// it through is not a degraded answer, it is a different word, spoken confidently.
    pub fn id(&self, c: char) -> Option<i64> {
        match self.table.get(c as usize) {
            Some(&id) if id >= 0 => Some(i64::from(id)),
            _ => None,
        }
    }

    /// Turn prepared text into tokens. See [`Spoken`] for what happens to what it cannot.
    pub fn encode(&self, prepared: &str) -> Spoken {
        let space = self.id(' ');
        let mut ids = Vec::with_capacity(prepared.len());
        let mut unvoiced: Vec<char> = Vec::new();
        for c in prepared.chars() {
            match self.id(c) {
                // Never two space tokens in a row. `prepare` has already collapsed whitespace,
                // so the only way to get one is a substitution beside a real space — and a run of
                // them is a pause the model was not trained on.
                Some(id) if Some(id) == space && ids.last() == Some(&id) => {}
                Some(id) => ids.push(id),
                None => {
                    if !unvoiced.contains(&c) {
                        unvoiced.push(c);
                    }
                    // A word boundary rather than nothing, and never two in a row.
                    if let Some(space) = space
                        && ids.last() != Some(&space)
                    {
                        ids.push(space);
                    }
                }
            }
        }
        Spoken { ids, unvoiced }
    }
}

// ---------------------------------------------------------------------------------------------
// The voice style
// ---------------------------------------------------------------------------------------------

/// One voice, as two tensors: the one the encoder and estimator read, and the one the duration
/// predictor reads.
#[derive(Debug, Clone, PartialEq)]
pub struct Style {
    /// `style_ttl`, shaped `[1, 50, 256]`.
    pub ttl: Vec<f32>,
    /// That shape, read from the file rather than assumed.
    pub ttl_shape: [usize; 3],
    /// `style_dp`, shaped `[1, 8, 16]`.
    pub dp: Vec<f32>,
    /// That shape.
    pub dp_shape: [usize; 3],
}

#[derive(serde::Deserialize)]
struct StyleFile {
    style_ttl: StyleTensor,
    style_dp: StyleTensor,
}

#[derive(serde::Deserialize)]
struct StyleTensor {
    data: Vec<Vec<Vec<f32>>>,
    dims: Vec<usize>,
}

impl StyleTensor {
    fn flatten(self, path: &Path, which: &str) -> Result<(Vec<f32>, [usize; 3]), Fault> {
        let bad = |detail: String| Fault::Unusable { path: path.to_path_buf(), detail };
        if self.dims.len() != 3 {
            return Err(bad(format!("{which} has {} dimensions and not three", self.dims.len())));
        }
        let shape = [self.dims[0], self.dims[1], self.dims[2]];
        let flat: Vec<f32> = self.data.into_iter().flatten().flatten().collect();
        let wanted = shape[0] * shape[1] * shape[2];
        if flat.len() != wanted {
            return Err(bad(format!(
                "{which} says it is {shape:?} — {wanted} numbers — and carries {}",
                flat.len()
            )));
        }
        Ok((flat, shape))
    }
}

/// Read one voice style file.
pub fn read_style(path: &Path) -> Result<Style, Fault> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| Fault::Unusable { path: path.to_path_buf(), detail: e.to_string() })?;
    let parsed: StyleFile = serde_json::from_str(&text)
        .map_err(|e| Fault::Unusable { path: path.to_path_buf(), detail: e.to_string() })?;
    let (ttl, ttl_shape) = parsed.style_ttl.flatten(path, "style_ttl")?;
    let (dp, dp_shape) = parsed.style_dp.flatten(path, "style_dp")?;
    Ok(Style { ttl, ttl_shape, dp, dp_shape })
}

// ---------------------------------------------------------------------------------------------
// Failures
// ---------------------------------------------------------------------------------------------

/// What went wrong, in words a window can render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// A model file could not be fetched, checked or stored. See [`model::Fault`].
    File(model::Fault),
    /// A file is there and is not what it claims to be: unreadable, or JSON that is not the
    /// shape this module was written against.
    Unusable { path: PathBuf, detail: String },
    /// ONNX Runtime refused a graph or a tensor.
    Onnx { detail: String },
    /// This machine has no voice named that.
    NoSuchVoice { name: String },
    /// **Nothing in the fragment can be spoken.** Not an empty answer: an empty `Vec<f32>`
    /// arriving at the speaker is indistinguishable from a synthesiser that has stopped working,
    /// and the queue would go on accepting fragments forever.
    NothingSpeakable { unvoiced: Vec<char> },
    /// The blocking synthesis task went away — a panic, or a shutting-down runtime.
    Lost,
}

impl From<model::Fault> for Fault {
    fn from(fault: model::Fault) -> Fault {
        Fault::File(fault)
    }
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fault::File(fault) => write!(f, "{fault}"),
            Fault::Unusable { path, detail } => {
                write!(f, "{} is not usable as part of the voice: {detail}", path.display())
            }
            Fault::Onnx { detail } => write!(f, "the voice model could not be run: {detail}"),
            Fault::NoSuchVoice { name } => write!(f, "there is no voice called {name}"),
            Fault::NothingSpeakable { unvoiced } => write!(
                f,
                "there is nothing in this that the voice can say; it is made only of \
                 characters it has no sound for ({})",
                unvoiced.iter().collect::<String>()
            ),
            Fault::Lost => write!(f, "the voice stopped before it produced anything"),
        }
    }
}

impl std::error::Error for Fault {}

// ---------------------------------------------------------------------------------------------
// Synthesis
// ---------------------------------------------------------------------------------------------

/// How many threads ONNX Runtime may use inside one graph. [`crate::stt::threads`]'s rule.
pub fn threads() -> usize {
    crate::stt::threads()
}

/// One spoken fragment.
#[derive(Debug, Clone, PartialEq)]
pub struct Utterance {
    /// 44.1 kHz mono, in `[-1, 1]`.
    pub samples: Vec<f32>,
    /// What the text contained that the voice has no sound for. Usually empty; never ignored.
    pub unvoiced: Vec<char>,
}

impl Utterance {
    /// How long it is.
    pub fn duration(&self) -> std::time::Duration {
        std::time::Duration::from_secs_f64(self.samples.len() as f64 / f64::from(SAMPLE_RATE))
    }
}

/// The four graphs, the table and one voice, loaded and ready.
///
/// **Loading is 1.8–2.7 s and the resident set is about 451 MB**, measured on this machine; with
/// whisper and the audio stack already at 213 MB that is most of what a 3.6 GB machine has spare
/// once a webview is up. Build one and keep it.
pub struct Tts {
    duration: ort::session::Session,
    encoder: ort::session::Session,
    estimator: ort::session::Session,
    vocoder: ort::session::Session,
    indexer: Indexer,
    style: Style,
    voice: String,
    steps: usize,
    speed: f32,
    seed: u64,
}

impl Tts {
    /// Load every graph out of `dir` and take the voice called `voice`.
    pub fn load(dir: &Path, voice: &str) -> Result<Tts, Fault> {
        Tts::load_on(dir, voice, GPU_BUILT_IN)
    }

    /// [`Tts::load`], on the GPU or not. `gpu` is ignored in a build without `gpu-tts`.
    pub fn load_on(dir: &Path, voice: &str, gpu: bool) -> Result<Tts, Fault> {
        let style_file = VOICES
            .iter()
            .find(|v| v.name == voice)
            .ok_or_else(|| Fault::NoSuchVoice { name: voice.to_string() })?
            .model
            .file;
        let style = read_style(&dir.join(style_file))?;
        let indexer = Indexer::read(&dir.join("unicode_indexer.json"))?;
        Ok(Tts {
            duration: session(&dir.join("duration_predictor.onnx"), gpu)?,
            encoder: session(&dir.join("text_encoder.onnx"), gpu)?,
            estimator: session(&dir.join("vector_estimator.onnx"), gpu)?,
            vocoder: session(&dir.join("vocoder.onnx"), gpu)?,
            indexer,
            style,
            voice: voice.to_string(),
            steps: TOTAL_STEP,
            speed: SPEED,
            seed: SEED,
        })
    }

    /// Which voice this is speaking with.
    pub fn voice(&self) -> &str {
        &self.voice
    }

    /// The table, so a caller can say in advance what it will not be able to speak.
    pub fn indexer(&self) -> &Indexer {
        &self.indexer
    }

    /// How fast it talks. [`SPEED`] is the reference default; above 1.0 is faster.
    ///
    /// Clamped rather than refused: this is a person moving a slider, and half to double speed
    /// is the range the predicted duration stays a sensible number over. **A setter and not a
    /// constant** because a mutation removing the division by it survived the whole suite —
    /// nothing could decide the clause without being able to move it.
    pub fn set_speed(&mut self, speed: f32) {
        self.speed = if speed.is_finite() { speed.clamp(0.5, 2.0) } else { SPEED };
    }

    /// What it is set to now.
    pub fn speed(&self) -> f32 {
        self.speed
    }

    /// Say one fragment, in the language its script says ([`language_for`]). Blocking, and for
    /// several hundred milliseconds — see [`TOTAL_STEP`].
    pub fn say(&mut self, text: &str) -> Result<Utterance, Fault> {
        self.say_in(text, language_for(text))
    }

    /// Say one fragment in a named language. An unknown tag is refused rather than spoken:
    /// `<xx>` would be read out as characters, which is four syllables nobody asked for.
    pub fn say_in(&mut self, text: &str, lang: &str) -> Result<Utterance, Fault> {
        if !is_language(lang) {
            return Err(Fault::Unusable {
                path: PathBuf::from(lang),
                detail: format!("{lang} is not one of the {} languages this voice knows", LANGUAGES.len()),
            });
        }
        let body = normalise(text);
        let spoken = self.indexer.encode(&prepare(text, lang));
        if !speakable(&self.indexer, &body) {
            return Err(Fault::NothingSpeakable { unvoiced: spoken.unvoiced });
        }

        let chars = spoken.ids.len();
        let ids_shape = [1usize, chars];
        let mask = vec![1.0f32; chars];
        let mask_shape = [1usize, 1, chars];

        let seconds = self.predict_duration(&spoken.ids, &ids_shape, &mask, &mask_shape)?;
        let emb = self.encode_text(&spoken.ids, &ids_shape, &mask, &mask_shape)?;

        let samples = (seconds * SAMPLE_RATE as f32) as usize;
        let frames = samples.div_ceil(SAMPLES_PER_FRAME).max(1);
        let latent_shape = [1usize, LATENT_WIDTH, frames];
        let latent_mask = vec![1.0f32; frames];
        let latent_mask_shape = [1usize, 1, frames];
        let mut latent = noise(LATENT_WIDTH * frames, self.seed);

        for step in 0..self.steps {
            latent = self.denoise(
                &latent,
                &latent_shape,
                &emb.0,
                &emb.1,
                &mask,
                &mask_shape,
                &latent_mask,
                &latent_mask_shape,
                step,
            )?;
        }

        let mut wav = self.vocode(&latent, &latent_shape)?;
        // The vocoder produces a whole number of latent frames; the duration predictor said how
        // much of that is speech. Keeping the rest would pad every fragment with up to 69 ms of
        // whatever the model put in an unasked-for frame.
        wav.truncate(samples.min(wav.len()));
        Ok(Utterance { samples: wav, unvoiced: spoken.unvoiced })
    }

    fn predict_duration(
        &mut self,
        ids: &[i64],
        ids_shape: &[usize; 2],
        mask: &[f32],
        mask_shape: &[usize; 3],
    ) -> Result<f32, Fault> {
        let out = self
            .duration
            .run(ort::inputs![
                "text_ids" => tensor_i64(ids_shape, ids)?,
                "style_dp" => tensor(&self.style.dp_shape, &self.style.dp)?,
                "text_mask" => tensor(mask_shape, mask)?,
            ])
            .map_err(onnx)?;
        let (_, seconds) = out["duration"].try_extract_tensor::<f32>().map_err(onnx)?;
        let seconds = *seconds.first().ok_or_else(|| Fault::Onnx {
            detail: "the duration predictor returned no number".into(),
        })?;
        Ok(seconds / self.speed)
    }

    fn encode_text(
        &mut self,
        ids: &[i64],
        ids_shape: &[usize; 2],
        mask: &[f32],
        mask_shape: &[usize; 3],
    ) -> Result<(Vec<f32>, [usize; 3]), Fault> {
        let out = self
            .encoder
            .run(ort::inputs![
                "text_ids" => tensor_i64(ids_shape, ids)?,
                "style_ttl" => tensor(&self.style.ttl_shape, &self.style.ttl)?,
                "text_mask" => tensor(mask_shape, mask)?,
            ])
            .map_err(onnx)?;
        let (shape, data) = out["text_emb"].try_extract_tensor::<f32>().map_err(onnx)?;
        Ok((data.to_vec(), three(shape)?))
    }

    #[allow(clippy::too_many_arguments)]
    fn denoise(
        &mut self,
        latent: &[f32],
        latent_shape: &[usize; 3],
        emb: &[f32],
        emb_shape: &[usize; 3],
        mask: &[f32],
        mask_shape: &[usize; 3],
        latent_mask: &[f32],
        latent_mask_shape: &[usize; 3],
        step: usize,
    ) -> Result<Vec<f32>, Fault> {
        let current = [step as f32];
        let total = [self.steps as f32];
        let out = self
            .estimator
            .run(ort::inputs![
                "noisy_latent" => tensor(latent_shape, latent)?,
                "text_emb" => tensor(emb_shape, emb)?,
                "style_ttl" => tensor(&self.style.ttl_shape, &self.style.ttl)?,
                "latent_mask" => tensor(latent_mask_shape, latent_mask)?,
                "text_mask" => tensor(mask_shape, mask)?,
                "current_step" => tensor(&[1usize], &current)?,
                "total_step" => tensor(&[1usize], &total)?,
            ])
            .map_err(onnx)?;
        let (_, data) = out["denoised_latent"].try_extract_tensor::<f32>().map_err(onnx)?;
        Ok(data.to_vec())
    }

    fn vocode(&mut self, latent: &[f32], shape: &[usize; 3]) -> Result<Vec<f32>, Fault> {
        let out = self
            .vocoder
            .run(ort::inputs!["latent" => tensor(shape, latent)?])
            .map_err(onnx)?;
        let (_, wav) = out["wav_tts"].try_extract_tensor::<f32>().map_err(onnx)?;
        Ok(wav.to_vec())
    }
}

/// Whether this build can read answers on the GPU at all.
pub const GPU_BUILT_IN: bool = cfg!(feature = "gpu-tts");

fn session(path: &Path, gpu: bool) -> Result<ort::session::Session, Fault> {
    ort::session::Session::builder()
        .and_then(|b| b.with_intra_threads(threads()))
        .and_then(|b| if gpu { on_the_gpu(b) } else { Ok(b) })
        .and_then(|b| b.commit_from_file(path))
        .map_err(|e| Fault::Unusable { path: path.to_path_buf(), detail: e.to_string() })
}

/// WebGPU in a `gpu-tts` build — Vulkan underneath on Linux, Direct3D 12 on Windows, so any
/// vendor's card and no CUDA install. Not an error when there is no adapter: ONNX Runtime then runs
/// every node on the processor, as a build without the feature always does.
///
/// **Which card is Dawn's choice**, the high-performance adapter. ONNX Runtime 1.22's WebGPU
/// provider takes no adapter option; the one way to name a card is to build a Dawn device here and
/// hand it over (`ep.webgpuexecutionprovider.webgpuDevice`), which is not done.
///
/// The environment is committed first: registering the provider on the first builder of a
/// process, before anything else has made one, failed with "Attempt to use DefaultLogger but none
/// has been registered" and left that graph on the processor.
#[cfg(feature = "gpu-tts")]
fn on_the_gpu(
    builder: ort::session::builder::SessionBuilder,
) -> ort::Result<ort::session::builder::SessionBuilder> {
    static ENVIRONMENT: std::sync::Once = std::sync::Once::new();
    ENVIRONMENT.call_once(|| {
        let _ = ort::init().commit();
    });
    builder.with_execution_providers([
        ort::execution_providers::WebGPUExecutionProvider::default().build(),
    ])
}

#[cfg(not(feature = "gpu-tts"))]
fn on_the_gpu(
    builder: ort::session::builder::SessionBuilder,
) -> ort::Result<ort::session::builder::SessionBuilder> {
    Ok(builder)
}

fn onnx(error: ort::Error) -> Fault {
    Fault::Onnx { detail: error.to_string() }
}

fn three(shape: &ort::tensor::Shape) -> Result<[usize; 3], Fault> {
    let dims: Vec<usize> = shape.iter().map(|d| *d as usize).collect();
    match dims.as_slice() {
        [a, b, c] => Ok([*a, *b, *c]),
        other => Err(Fault::Onnx {
            detail: format!("expected a three-dimensional tensor and got {other:?}"),
        }),
    }
}

fn tensor<'a>(
    shape: &[usize],
    data: &'a [f32],
) -> Result<ort::value::TensorRef<'a, f32>, Fault> {
    ort::value::TensorRef::from_array_view((shape.to_vec(), data)).map_err(onnx)
}

fn tensor_i64<'a>(
    shape: &[usize],
    data: &'a [i64],
) -> Result<ort::value::TensorRef<'a, i64>, Fault> {
    ort::value::TensorRef::from_array_view((shape.to_vec(), data)).map_err(onnx)
}

/// The seed the noisy latent is drawn from.
///
/// **Fixed, so that the same sentence is spoken the same way every time.** Flow matching starts
/// from noise, and the reference implementation takes it from the thread's random number
/// generator — which makes every rendering of a sentence different, including the one a person
/// asks to hear again after barging in on it. A constant costs nothing the model notices (the
/// estimator is conditioned on the text either way) and buys a test that can assert on samples.
const SEED: u64 = 0x5EED_0000_5EED_0000;

/// `count` samples of N(0, 1), deterministically.
///
/// splitmix64 into Box–Muller. Thirty lines rather than `rand` + `rand_distr`, which the
/// reference uses and which would be two more crates in a graph whose whole feature layout
/// exists to stay small.
fn noise(count: usize, seed: u64) -> Vec<f32> {
    let mut state = seed;
    let mut next = || {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    // (0, 1]: `ln(0)` is negative infinity and one zero would poison the whole latent.
    let mut uniform = || ((next() >> 11) as f64 + 1.0) / ((1u64 << 53) as f64 + 1.0);

    let mut out = Vec::with_capacity(count);
    while out.len() < count {
        let (u1, u2) = (uniform(), uniform());
        let r = (-2.0 * u1.ln()).sqrt();
        let theta = std::f64::consts::TAU * u2;
        out.push((r * theta.cos()) as f32);
        if out.len() < count {
            out.push((r * theta.sin()) as f32);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fragment_is_read_in_the_language_its_script_says() {
        assert_eq!(language_for("The file is already there."), "en");
        assert_eq!(language_for("내일 아침 서울 날씨가 어떨지 알려줘."), "ko");
        assert_eq!(language_for("Rust의 borrow checker는 엄격합니다."), "ko", "Hangul among Latin words");
        assert_eq!(language_for("ファイルはもうあります。"), "ja");
        assert_eq!(language_for("12:30, 3 files."), "en", "nothing but digits and punctuation");
        assert!(is_language(language_for("")), "whatever it answers has to be a tag the model knows");
    }

    /// A table shaped like the one on disk, built from what was measured rather than copied out
    /// of it: precomposed Hangul unmapped, conjoining jamo mapped, ASCII mapped.
    ///
    /// The real table is 277 KB and is a model file — gitignored, downloaded, and absent on
    /// every CI runner. `the_table_on_disk_is_the_one_this_module_was_written_against` checks
    /// this stand-in's shape against the real one when it is there; without that, this would be
    /// a second copy of a measurement with nothing to say when the two stop agreeing.
    fn stand_in() -> Indexer {
        let mut table = vec![-1i32; 0x1_0000];
        for (n, c) in (0x20u32..0x7F).enumerate() {
            table[c as usize] = n as i32;
        }
        // The 69 conjoining jamo, at the ids the real table gives them.
        for (n, c) in (0x1100u32..=0x1112).enumerate() {
            table[c as usize] = 560 + n as i32;
        }
        for (n, c) in (0x1161u32..=0x1175).enumerate() {
            table[c as usize] = 580 + n as i32;
        }
        for (n, c) in (0x11A8u32..=0x11C2).enumerate() {
            table[c as usize] = 602 + n as i32;
        }
        // A combining acute, which is where `é` goes.
        table[0x301] = 146;
        Indexer::new(table)
    }

    /// **The test this task was written around, and it fails without [`prepare`]'s NFKD.**
    ///
    /// Attacca sends NFC. Every precomposed Hangul syllable is unmapped, so NFC Korean encodes
    /// to nothing but unvoiced characters — and `-1` is not an error, so without this the first
    /// anybody would know is a voice that says the English half of a sentence and skips the
    /// Korean half without a word about it.
    #[test]
    fn hangul_reaches_the_model_only_after_decomposition() {
        let table = stand_in();

        let raw = table.encode("한국어");
        assert_eq!(raw.unvoiced, vec!['한', '국', '어'], "not one syllable had a token");
        assert!(
            !raw.ids.iter().any(|&id| (560..=628).contains(&id)),
            "no jamo reached the model: {:?}",
            raw.ids
        );
        assert_eq!(raw.ids, vec![table.id(' ').unwrap()], "three syllables left one boundary");

        let ready = table.encode(&prepare("한국어", "ko"));
        assert!(
            ready.unvoiced.is_empty(),
            "after NFKD every Hangul character is jamo, and jamo are mapped: {:?}",
            ready.unvoiced
        );
        // 한국어 is 8 jamo; `<ko>` and `</ko>` are 4 and 5 characters; `prepare` adds a full stop.
        assert_eq!(ready.ids.len(), 8 + 4 + 5 + 1);
    }

    /// The other half of the same measurement, and the half the plan did not have.
    ///
    /// "Hangul NFD is algorithmic, no table needed" is true and would have shipped a voice that
    /// could not say `café`: precomposed accented Latin is unmapped exactly like Hangul, and it
    /// is the *general* decomposition that rescues it.
    #[test]
    fn accented_latin_needs_the_same_decomposition_hangul_does() {
        let table = stand_in();
        assert_eq!(table.id('é'), None, "the composed letter has no token");
        assert!(table.id('e').is_some() && table.id('\u{301}').is_some());

        let ready = table.encode(&prepare("café", "fr"));
        assert!(ready.unvoiced.is_empty(), "{:?}", ready.unvoiced);
    }

    /// A character with no token is **reported and replaced by a boundary**, never dropped.
    #[test]
    fn a_character_the_voice_cannot_say_leaves_a_trace() {
        let table = stand_in();
        let spoken = table.encode(&prepare("hi 🙂 there", "en"));

        assert_eq!(spoken.unvoiced, vec!['🙂'], "the emoji is named");
        let space = table.id(' ').expect("a space has a token");
        assert!(spoken.ids.contains(&space), "it left a word boundary behind");
        // "hi", one boundary, "there" — never "hithere", and never two spaces where one glyph
        // and the space beside it were.
        assert_eq!(spoken.ids, table.encode("<en>hi there.</en>").ids);
    }

    /// Two unmapped characters in a row are one boundary, not two.
    #[test]
    fn a_run_of_characters_with_no_token_is_one_boundary() {
        let table = stand_in();
        let spoken = table.encode("a🙂🙃b");
        let space = table.id(' ').expect("a space has a token");
        assert_eq!(spoken.ids, vec![table.id('a').unwrap(), space, table.id('b').unwrap()]);
        assert_eq!(spoken.unvoiced, vec!['🙂', '🙃']);

        // The same character twice is named once: a person is told what the voice cannot say,
        // not how many times it came up.
        let repeated = table.encode("a🙂b🙂c🙂");
        assert_eq!(repeated.unvoiced, vec!['🙂']);
    }

    /// A fragment of nothing but unsayable characters is **refused**, not spoken as silence.
    ///
    /// An empty `Vec<f32>` at the speaker is indistinguishable from a synthesiser that has
    /// stopped working — the same argument `stt::MIN_AUDIO` makes about whisper and the same one
    /// `vad::MIN_SPEECH` makes about a turn.
    #[test]
    fn nothing_speakable_is_refused_rather_than_spoken_as_silence() {
        let table = stand_in();
        assert!(!speakable(&table, &normalise("🙂🙃")), "nothing in it has a sound");
        assert!(!speakable(&table, &normalise("   ")), "nor has whitespace");
        assert!(speakable(&table, &normalise("ok")), "and something that has is not refused");
        assert_eq!(table.encode(&prepare("🙂🙃", "en")).unvoiced, vec!['🙂', '🙃']);
    }

    /// The refusal is decided on the **body**, before the tags and the full stop are added.
    ///
    /// Deciding it on the finished string was the first attempt and it does not work: `<en>`,
    /// `</en>` and the stop are always speakable, so a fragment of nothing but emoji comes out
    /// longer than the tags and looks like a sentence.
    #[test]
    fn what_is_speakable_is_decided_before_the_tags_are_added() {
        let table = stand_in();
        assert_eq!(normalise("🙂🙃"), "🙂🙃", "the body carries no tags and no ending");
        assert_eq!(prepare("🙂🙃", "en"), "<en>🙂🙃.</en>");
        assert!(
            table.encode(&prepare("🙂🙃", "en")).ids.len() > prepare("", "en").chars().count(),
            "which is exactly why the finished string cannot decide it"
        );
        assert!(!speakable(&table, &normalise("🙂🙃")));
    }

    /// `prepare` wraps rather than parameterises, because the model has no language embedding.
    #[test]
    fn the_language_is_four_characters_and_not_a_parameter() {
        assert_eq!(prepare("hello.", "en"), "<en>hello.</en>");
        assert_eq!(prepare("hello.", "ko"), "<ko>hello.</ko>");
    }

    /// The model runs on past a fragment that does not end, so `prepare` ends it — and does not
    /// end one that already did.
    #[test]
    fn a_fragment_is_given_an_ending_only_if_it_has_none() {
        assert_eq!(prepare("no ending", "en"), "<en>no ending.</en>");
        assert_eq!(prepare("an ending!", "en"), "<en>an ending!</en>");
        assert_eq!(prepare("she said \"no.\"", "en"), "<en>she said \"no.\"</en>");
    }

    /// Whitespace is collapsed, because a delta stream arrives with whatever spacing the model
    /// wrote and a run of newlines is not a pause the voice knows how to take.
    #[test]
    fn whitespace_is_one_space() {
        assert_eq!(prepare("a \n\n  b", "en"), "<en>a b.</en>");
        assert_eq!(prepare("   ", "en"), "<en></en>");
    }

    /// A language the model does not know is refused rather than read out as `<xx>`.
    #[test]
    fn an_unknown_language_is_not_spoken_as_characters() {
        assert!(is_language("ko") && is_language("en"));
        assert!(!is_language("xx"), "there is no xx");
        assert_eq!(LANGUAGES.len(), 32, "the archived reference lists 32, `na` included");
    }

    /// Sixteen files, and the number the screen shows before asking for 398 MB.
    #[test]
    fn everything_needed_is_named_once_and_adds_up() {
        let all = required();
        assert_eq!(all.len(), FILES.len() + VOICES.len());

        let mut names: Vec<&str> = all.iter().map(|m| m.file).collect();
        names.sort_unstable();
        let mut unique = names.clone();
        unique.dedup();
        assert_eq!(names, unique, "two files would land on one name in one directory");

        assert_eq!(total_bytes(), 401_276_744);
        assert!(
            VOICES.iter().any(|v| v.name == DEFAULT_VOICE),
            "the default voice has to be one of the ones that are fetched"
        );
        for model in all {
            assert!(model.url.contains(REVISION), "{} is not pinned", model.file);
            assert_eq!(model.sha256.len(), 64, "{} has no digest", model.file);
        }
    }

    /// A directory missing one file says **which**, and how much fetching it costs.
    #[test]
    fn an_incomplete_directory_names_what_is_missing() {
        let dir = tempdir();
        for model in required() {
            std::fs::write(dir.join(model.file), vec![0u8; model.bytes as usize % 64 + 1])
                .expect("write");
        }
        // Give one of them exactly the right size; everything else is the wrong size.
        std::fs::write(dir.join(FILES[4].file), vec![0u8; FILES[4].bytes as usize])
            .expect("write");

        match state_in(&dir) {
            VoiceState::Incomplete { missing, bytes, .. } => {
                assert!(!missing.contains(&FILES[4].file.to_string()), "{missing:?}");
                assert_eq!(missing.len(), required().len() - 1);
                assert_eq!(bytes, total_bytes() - FILES[4].bytes);
            }
            other => panic!("{other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Nothing there at all is still `Incomplete`, and it is the whole download.
    #[test]
    fn an_empty_directory_costs_the_whole_download() {
        let dir = tempdir();
        match state_in(&dir) {
            VoiceState::Incomplete { missing, bytes, .. } => {
                assert_eq!(missing.len(), required().len());
                assert_eq!(bytes, total_bytes());
            }
            other => panic!("{other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// **Something in the way is not something missing.** The fifth time this workspace has had
    /// to keep these apart; a `Download` button over a directory named `vocoder.onnx` helps
    /// nobody.
    #[test]
    fn something_that_is_not_a_file_is_not_a_missing_one() {
        let dir = tempdir();
        std::fs::create_dir_all(dir.join(FILES[0].file)).expect("a directory in the way");
        match state_in(&dir) {
            VoiceState::Unreadable { detail, .. } => {
                assert!(detail.contains(FILES[0].file), "{detail}");
            }
            other => panic!("{other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The environment variable names a directory and the cache is the fallback.
    #[test]
    fn the_files_live_in_the_shared_cache_and_can_be_pointed_elsewhere() {
        let named = models_dir_given(Some("/somewhere/else".into()));
        assert_eq!(named, Some(PathBuf::from("/somewhere/else")));
        // An empty value is not a choice.
        let empty = models_dir_given(Some("".into()));
        assert_eq!(empty, models_dir_given(None));
        if let Some(dir) = models_dir_given(None) {
            assert!(dir.ends_with(DIRECTORY), "{dir:?}");
        }
    }

    /// The latent is one frame per 69.66 ms of audio, rounded **up**, and never zero.
    ///
    /// Rounding down would cut the last syllable off every fragment whose length is not a
    /// multiple of 3072 samples, which is almost all of them; zero frames is a tensor ONNX
    /// refuses.
    #[test]
    fn the_latent_covers_the_audio_and_is_never_empty() {
        for (samples, frames) in
            [(0usize, 1usize), (1, 1), (3071, 1), (3072, 1), (3073, 2), (44_100, 15)]
        {
            assert_eq!(samples.div_ceil(SAMPLES_PER_FRAME).max(1), frames, "for {samples}");
        }
        assert_eq!(SAMPLES_PER_FRAME, 3072);
        assert_eq!(LATENT_WIDTH, 144);
    }

    /// The noise is the same every time, which is what lets a test assert on samples at all —
    /// and what makes a sentence a person asks to hear again sound like the one they cut off.
    #[test]
    fn the_noise_is_normal_and_repeatable() {
        let a = noise(4096, SEED);
        let b = noise(4096, SEED);
        assert_eq!(a, b, "the same seed is the same latent");
        assert_ne!(a, noise(4096, SEED + 1));

        assert!(a.iter().all(|s| s.is_finite()), "one non-finite sample poisons the whole latent");
        let mean = a.iter().map(|&s| f64::from(s)).sum::<f64>() / a.len() as f64;
        let var =
            a.iter().map(|&s| (f64::from(s) - mean).powi(2)).sum::<f64>() / a.len() as f64;
        assert!(mean.abs() < 0.05, "mean {mean}");
        assert!((var - 1.0).abs() < 0.1, "variance {var}");
        // An odd count must not lose the second half of the last pair.
        assert_eq!(noise(7, SEED).len(), 7);
    }

    /// **The weights are not this repository's licence and the page has to say so.**
    ///
    /// The code is MIT and the weights are BigScience OpenRAIL-M, which carries use-based
    /// restrictions that travel with any copy. A licence obligation that lives only in a source
    /// comment is one nobody redistributing a build will ever read, so the README carries it and
    /// this fails if the sentence moves — the same shape `the_readme_names_the_directory_the_    /// model_is_kept_in` uses one module over.
    #[test]
    fn the_readme_says_what_licence_the_weights_carry() {
        let readme = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../README.md"),
        )
        .expect("the README is readable from this crate");

        for wanted in ["BigScience OpenRAIL-M", "use-based restrictions", MODELS_ENV, DIRECTORY] {
            assert!(readme.contains(wanted), "the README does not say {wanted:?}");
        }
        assert!(
            readme.contains(&format!("{} MB", total_bytes() / 1_000_000)),
            "the README does not say how big the download is"
        );
    }

    /// A directory that goes away with the test.
    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zyris-tts-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).expect("a temporary directory");
        dir
    }

    // -----------------------------------------------------------------------------------------
    // Everything below needs the 398 MB on disk and is skipped without it.
    //
    // **So CI never synthesises a sample**, exactly as it never decodes one — `stt.rs` records
    // the same cost for the same reason. What that buys is a suite that runs in seconds on a
    // machine with no models; what it costs is that a mistake between the graph names and the
    // tensors fed to them is caught only here.
    // -----------------------------------------------------------------------------------------

    fn models() -> Option<PathBuf> {
        let dir = models_dir()?;
        match state_in(&dir) {
            VoiceState::Ready { dir } => Some(dir),
            _ => None,
        }
    }

    /// The stand-in table above is only worth anything if the real one has the shape it claims.
    #[test]
    fn the_table_on_disk_is_the_one_this_module_was_written_against() {
        let Some(dir) = models() else {
            eprintln!("skipped: no Supertonic models; set {MODELS_ENV}");
            return;
        };
        let real = Indexer::read(&dir.join("unicode_indexer.json")).expect("the table reads");
        assert_eq!(real.mapped(), 8_321, "how many codepoints this voice can say");

        let precomposed = (0xAC00u32..=0xD7A3).filter_map(char::from_u32);
        assert_eq!(
            precomposed.filter(|&c| real.id(c).is_some()).count(),
            0,
            "not one of the 11,172 precomposed Hangul syllables has a token"
        );
        let stand_in = stand_in();
        for c in (0x1100u32..=0x1112).chain(0x1161..=0x1175).chain(0x11A8..=0x11C2) {
            let c = char::from_u32(c).expect("a jamo");
            assert_eq!(real.id(c), stand_in.id(c), "jamo {c:?} moved");
        }
        assert_eq!(real.id('é'), None, "the composed letter has no token in the real table");
        // The *ids* of ASCII are not a contiguous run from the space — the real table skips
        // characters the model has no sound for — so only the jamo block above is asserted by
        // value. Here it is enough that the decomposition lands on something.
        assert!(real.id('e').is_some() && real.id(' ').is_some());
        assert_eq!(
            real.id('\u{301}'),
            stand_in.id('\u{301}'),
            "the combining acute is where `café` goes and it moved"
        );
    }

    /// The same sentence on the processor and on the GPU, in a build that has one.
    ///
    /// **The timings are printed, not asserted** — nothing here may assert on a clock — but the
    /// two answers have to agree on how long the sentence is: the GPU runs the same graphs, and a
    /// provider that dropped or mangled a node would show first as a sentence of another length.
    /// Measured on Windows 11 with an RTX 3050, release, 2026-09-26: 1.37 s on the processor and
    /// 0.37 s through WebGPU for 3.6 s of audio. DirectML, tried first because the plain Windows
    /// ONNX Runtime already carries it, managed 1.26 s and was dropped.
    #[test]
    fn the_gpu_says_what_the_processor_says() {
        if !GPU_BUILT_IN {
            eprintln!("skipped: this build reads answers on the processor only");
            return;
        }
        let Some(dir) = models() else {
            eprintln!("skipped: no Supertonic models; set {MODELS_ENV}");
            return;
        };
        let sentence = "The quick brown fox jumps over the lazy dog, twice.";
        let mut lengths = Vec::new();
        for gpu in [false, true] {
            let mut tts = Tts::load_on(&dir, DEFAULT_VOICE, gpu).expect("the models load");
            // Twice, so the second is the one timed: a GPU provider compiles its kernels for the
            // shapes it is first handed.
            tts.say(sentence).expect("it speaks");
            let started = std::time::Instant::now();
            let said = tts.say(sentence).expect("it speaks");
            eprintln!(
                "{}: {:?} for {:?} of audio",
                if gpu { "gpu" } else { "cpu" },
                started.elapsed(),
                said.duration()
            );
            lengths.push(said.duration().as_secs_f32());
        }
        assert!(
            (lengths[0] - lengths[1]).abs() < 0.1,
            "the processor and the GPU disagree on how long the sentence is: {lengths:?}"
        );
    }

    /// One sentence, out of the real graphs, as samples.
    #[test]
    fn one_sentence_comes_out_as_audio() {
        let Some(dir) = models() else {
            eprintln!("skipped: no Supertonic models; set {MODELS_ENV}");
            return;
        };
        let loading = std::time::Instant::now();
        let mut tts = Tts::load(&dir, DEFAULT_VOICE).expect("the models load");
        eprintln!("load {:?}", loading.elapsed());

        let started = std::time::Instant::now();
        let said = tts.say("Ready when you are.").expect("it speaks");
        eprintln!(
            "say {:?} for {:?} of audio",
            started.elapsed(),
            said.duration()
        );

        assert!(said.unvoiced.is_empty(), "{:?}", said.unvoiced);
        assert!(
            said.duration() > std::time::Duration::from_millis(500),
            "four words is not {:?}",
            said.duration()
        );
        // **The vocoder's trailing frame is trimmed.** It produces a whole number of 3072-sample
        // latent frames; the duration predictor said how much of that is speech. A length that
        // is still a multiple of the frame is one nobody cut, and the cost is up to 69.66 ms of
        // whatever the model put in a frame it was not asked for, on the end of every fragment.
        assert_ne!(
            said.samples.len() % SAMPLES_PER_FRAME,
            0,
            "{} samples is a whole number of latent frames, so nothing was trimmed",
            said.samples.len()
        );
        assert!(said.samples.iter().all(|s| s.is_finite()), "a non-finite sample reached a speaker");
        let peak = said.samples.iter().fold(0.0f32, |a, &s| a.max(s.abs()));
        assert!(peak > 0.05, "peak {peak} — this is silence, not speech");
        assert!(peak <= 1.0, "peak {peak} — this would clip");

        // The same sentence twice is the same audio: see `SEED`.
        let again = tts.say("Ready when you are.").expect("it speaks again");
        assert_eq!(said.samples, again.samples);

        // **Time to the first sample of a short fragment**, which is the number task 2's
        // splitter is argued from: nothing plays until the whole fragment is synthesised, so
        // this is the wait a person hears before the voice starts.
        let started = std::time::Instant::now();
        let short = tts.say("Yes.").expect("it speaks");
        eprintln!(
            "short fragment: {:?} to first sample, for {:?} of audio",
            started.elapsed(),
            short.duration()
        );
        assert!(!short.samples.is_empty());
    }

    /// **The rate really is a rate**, and it is what the predicted duration is divided by.
    ///
    /// Written because the mutation removing that division survived every other test in this
    /// file: a five per cent change in how long a sentence takes is not something an assertion
    /// about samples can see. Doubling it is.
    #[test]
    fn the_rate_changes_how_long_a_sentence_takes() {
        let Some(dir) = models() else {
            eprintln!("skipped: no Supertonic models; set {MODELS_ENV}");
            return;
        };
        let mut tts = Tts::load(&dir, DEFAULT_VOICE).expect("the models load");
        assert_eq!(tts.speed(), SPEED, "the reference default, until somebody moves it");
        let ordinary = tts.say("Ready when you are.").expect("it speaks").samples.len();

        tts.set_speed(2.0);
        let quick = tts.say("Ready when you are.").expect("it speaks").samples.len();
        assert!(
            (quick as f64) < ordinary as f64 * 0.7,
            "at twice the rate it is {quick} samples against {ordinary}"
        );

        // Out of range is clamped, not refused: this is a slider, and a rate of a thousand is a
        // fragment of no samples, which `NothingSpeakable` exists to keep off the queue.
        tts.set_speed(1_000.0);
        assert_eq!(tts.speed(), 2.0);
        tts.set_speed(f32::NAN);
        assert_eq!(tts.speed(), SPEED);
    }

    /// Korean, all the way through the real graphs. Quality is a person's judgement and is in
    /// the README's "What nobody has checked by hand"; that it produces speech at all is this.
    #[test]
    fn korean_is_spoken_at_all() {
        let Some(dir) = models() else {
            eprintln!("skipped: no Supertonic models; set {MODELS_ENV}");
            return;
        };
        let mut tts = Tts::load(&dir, DEFAULT_VOICE).expect("the models load");
        let said = tts.say_in("안녕하세요. 준비되었습니다.", "ko").expect("it speaks");
        assert!(said.unvoiced.is_empty(), "{:?}", said.unvoiced);
        let peak = said.samples.iter().fold(0.0f32, |a, &s| a.max(s.abs()));
        assert!(peak > 0.05, "peak {peak} — Korean came out as silence");
    }

    /// A fragment made only of characters the voice has no sound for never reaches ONNX.
    #[test]
    fn a_fragment_of_nothing_sayable_is_refused_by_the_real_model() {
        let Some(dir) = models() else {
            eprintln!("skipped: no Supertonic models; set {MODELS_ENV}");
            return;
        };
        let mut tts = Tts::load(&dir, DEFAULT_VOICE).expect("the models load");
        match tts.say("🙂🙃") {
            Err(Fault::NothingSpeakable { unvoiced }) => assert_eq!(unvoiced, vec!['🙂', '🙃']),
            other => panic!("{other:?}"),
        }
    }
}
