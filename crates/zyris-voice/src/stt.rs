//! Whisper, the file it needs on disk, and the one parameter that makes it fast enough to
//! hold a conversation with.
//!
//! # The encoder walks thirty seconds whatever you give it
//!
//! A three-second utterance costs nearly what an eleven-second one does, because the encoder
//! runs over a fixed thirty-second window regardless of how much audio is in it. So "whisper
//! base stays ahead of real time on a slow CPU" is true of a long clip and false of the thing
//! a person waiting for an answer actually notices. [`audio_ctx`] is the lever that fixes it,
//! and it is the reason this module exists rather than three lines in the session.
//!
//! Measured on this machine (i3-7100U, 4 threads) with `ggml-base.bin`, **release build** —
//! see the note on [`Stt::transcribe`] about why that qualifier matters — over
//! `tests/audio/jfk.wav`:
//!
//! | clip | `audio_ctx` | time |
//! |---|---|---|
//! | 1.5 s | 1500 (whisper's default) | 3.55 s |
//! | 1.5 s | **450** (this module's rule) | **0.81 s** |
//! | 3 s | 1500 | 3.63 s |
//! | 3 s | **450** | **0.88 s** |
//! | 11 s | 1500 | 3.92 s |
//! | 11 s | **583** (this module's rule) | **1.32 s** |
//!
//! Thirteen windows of that recording — one second to eleven, from the start and from the
//! middle — were run through the shipped rule and every one of them transcribed correctly, in
//! 0.81 s to 1.01 s for anything under eight seconds.
//!
//! **The lever is sharp at the other end**, and that is the half this module is really about:
//! a context that is too small does not lose a little accuracy, it makes whisper invent — and
//! then costs *more* time than leaving it alone, because inventing means decoding more tokens
//! and then decoding the whole thing again at a higher temperature. [`MIN_CTX`] carries the
//! measurements.
//!
//! Nothing here opens a microphone or publishes a [`crate::VoiceEvent`]. Task 6's session is
//! the only caller.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::capture::SAMPLE_RATE;

/// A whisper model this build knows how to fetch and check.
///
/// A struct rather than four loose constants so the download can be tested against a
/// three-byte file on loopback instead of a 141 MB one over the network — see
/// [`fetch`] and the tests at the foot of this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Model {
    /// Where to get it. Pinned to a **commit**, not to `main`: the bytes behind a branch can
    /// change, and then the digest below would be the only thing standing between a person
    /// and a silent model swap. With the revision pinned the digest is a second lock rather
    /// than the only one.
    pub url: &'static str,
    /// The name it takes on disk. Kept as upstream spells it so a file somebody already has
    /// can be dropped into the cache directory and recognised.
    pub file: &'static str,
    /// Exactly how many bytes it is. Cheap enough to check on every launch.
    pub bytes: u64,
    /// SHA-256, lowercase hex.
    ///
    /// **Not "whatever was downloaded here once."** Hugging Face serves the Git LFS pointer
    /// for this path at `.../raw/main/ggml-base.bin`, and it reads
    /// `oid sha256:60ed5bc3…` / `size 147951465`; the `resolve` endpoint repeats it as
    /// `x-linked-etag`. Both were read on 2026-09-14 and both agree with the file measured
    /// here, so this constant is upstream's claim about the model and not a local accident.
    pub sha256: &'static str,
}

/// The model the product ships against: whisper `base`, multilingual, 141 MB.
///
/// A different size is a code change, deliberately — see the plan's "Deliberately not here".
pub const BASE: Model = Model {
    url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/\
          5359861c739e955e79d9a303bcbc70fb988958b1/ggml-base.bin",
    file: "ggml-base.bin",
    bytes: 147_951_465,
    sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
};

/// Names a model file directly, bypassing the cache directory entirely.
///
/// Two callers: this crate's own tests, which must not download 141 MB to run, and a person
/// who already has a `ggml-*.bin` and would rather not have a second copy. **A file named
/// this way is not size-checked** — see [`state`] — because it is an operator's explicit
/// choice and may deliberately be a different size of model.
pub const MODEL_ENV: &str = "ZYRIS_WHISPER_MODEL";

/// The suffix a download wears until it is complete and checked.
///
/// Visible in the cache directory, and meant to be: a file with this in its name is the
/// wreckage of an interrupted download and never a model. See [`fetch`].
pub const PART_SUFFIX: &str = ".part";

/// What went wrong, in words a window can render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// This platform would not name a cache directory. Rare, and fatal to the download.
    NoCacheDirectory,
    /// The transfer itself failed — no network, a refused connection, an HTTP status.
    Transfer { url: String, detail: String },
    /// The bytes arrived and are not the model. Either the wrong length or the wrong digest.
    NotTheModel { detail: String },
    /// The bytes arrived and could not be written or renamed.
    Storage { path: PathBuf, detail: String },
    /// whisper.cpp refused the model or the audio.
    Whisper { detail: String },
    /// The blocking transcription task went away — a panic inside whisper, or a shutting-down
    /// runtime. Its own variant because it says nothing about the audio.
    Lost,
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fault::NoCacheDirectory => write!(
                f,
                "this system does not name a cache directory, so there is nowhere to keep the \
                 speech model; set {MODEL_ENV} to a file you have downloaded yourself"
            ),
            Fault::Transfer { url, detail } => {
                write!(f, "the speech model could not be downloaded from {url}: {detail}")
            }
            Fault::NotTheModel { detail } => write!(
                f,
                "the download did not produce the speech model and has been discarded: {detail}"
            ),
            Fault::Storage { path, detail } => {
                write!(f, "the speech model could not be saved to {}: {detail}", path.display())
            }
            Fault::Whisper { detail } => write!(f, "speech recognition failed: {detail}"),
            Fault::Lost => write!(f, "speech recognition stopped before it produced anything"),
        }
    }
}

impl std::error::Error for Fault {}

/// What is on disk where the model should be. **Three answers, not two.**
///
/// The same distinction the MCP screen and the audit tail had to make: "there is no model" and
/// "there is something there and it is not a model" send a person to different places, and
/// collapsing them is how a truncated file turns into a mysterious whisper error months later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelState {
    /// The file is there and is the size it should be. [`Stt::load`] is the next step.
    Ready { path: PathBuf, bytes: u64 },
    /// Nothing is there yet. [`fetch`] is the next step.
    Absent { path: PathBuf },
    /// Something is there and it is the wrong size. Fetching again replaces it.
    ///
    /// **This cannot be produced by an interrupted download of ours** — [`fetch`] never gives
    /// a file this name until it has checked it — so reaching it means a copy that was made by
    /// hand, a disk that lost the file, or a different model dropped in under this name.
    Damaged { path: PathBuf, bytes: u64, expected: u64 },
    /// Something is there and it could not be looked at: a directory where the file should be,
    /// a permission this user does not have, a mount that has gone away.
    ///
    /// **Not [`ModelState::Absent`]**, and the difference is the whole reason this variant was
    /// added in task 7 rather than in task 5. `std::fs::metadata` fails for more than one
    /// reason, and reading every one of them as "nothing is there" put a **Download** button in
    /// front of a person whose problem a download cannot fix — the confident false negative
    /// this workspace has now shipped once per screen that guessed. Task 7's screen renders
    /// these three separately, so the state it renders has to keep them apart.
    Unreadable { path: PathBuf, detail: String },
    /// There is no directory to keep it in and none was named.
    Nowhere { reason: String },
}

/// Where models live: the platform cache directory, shared by every instance.
///
/// **Not `data_dir(instance)`.** `zyris-app` scopes its data directory by instance so a
/// `--server` run shares no credentials with the production node; that argument is about
/// secrets, and it does not reach a 141 MB read-only file whose contents are fixed by the
/// digest above. Scoping it would download it once per instance and keep two identical copies.
///
/// The cache directory rather than the data directory for the same reason: it is exactly what
/// a platform's "clear caches" is allowed to delete, and losing it costs a download, not a
/// setting. Task 7's delete button lives on this directory.
pub fn cache_dir() -> Option<PathBuf> {
    if let Some(dirs) = directories::ProjectDirs::from("cc", "attacca", "zyris") {
        return Some(dirs.cache_dir().join("models"));
    }
    // `ProjectDirs` failed to name one, which on Linux means neither `XDG_CACHE_HOME` nor
    // `HOME` is set. `BaseDirs` is the same question asked with fewer requirements.
    if let Some(dirs) = directories::BaseDirs::new() {
        return Some(dirs.cache_dir().join("zyris").join("models"));
    }
    // Deliberately **not** `std::env::temp_dir()`. `data_dir` falls back to it because an
    // audit log that cannot be written is survivable; a 141 MB download into a directory the
    // system empties is a download repeated on every launch, forever.
    None
}

/// Where the model file should be, honouring [`MODEL_ENV`].
pub fn model_path(model: &Model) -> Option<PathBuf> {
    model_path_given(model, std::env::var_os(MODEL_ENV))
}

/// The same decision with the environment passed in.
///
/// **Not a convenience.** `cargo test` runs a crate's tests on threads of one process, and
/// `set_var` is process-wide: a test that set [`MODEL_ENV`] to prove this rule would be
/// changing what the model-gated tests further down see, at whatever moment the scheduler
/// chose. Passing it in is the only way to test the rule and the model on the same run.
pub fn model_path_given(model: &Model, named: Option<std::ffi::OsString>) -> Option<PathBuf> {
    if let Some(named) = named {
        let named = PathBuf::from(named);
        if !named.as_os_str().is_empty() {
            return Some(named);
        }
    }
    Some(cache_dir()?.join(model.file))
}

/// Look at one path and say which of the three things it is.
///
/// `expected` is `None` for a file an operator named through [`MODEL_ENV`]: they may well
/// have pointed at `ggml-small.bin` on purpose, and calling that "damaged" would be this
/// program telling a person their own choice is broken.
pub fn inspect(path: &Path, expected: Option<u64>) -> ModelState {
    match std::fs::metadata(path) {
        // Something is there and it is not a file. `metadata` answers happily for a directory —
        // with a length, which on Linux is 4096 and would read as a damaged model of exactly
        // that size — so the one thing it does not say is the thing that matters here.
        Ok(meta) if !meta.is_file() => ModelState::Unreadable {
            path: path.to_path_buf(),
            detail: "there is something at this path and it is not a file".to_string(),
        },
        Ok(meta) => {
            let bytes = meta.len();
            match expected {
                Some(want) if bytes != want => {
                    ModelState::Damaged { path: path.to_path_buf(), bytes, expected: want }
                }
                _ => ModelState::Ready { path: path.to_path_buf(), bytes },
            }
        }
        // Only "it is not there" is absence. Everything else — a permission, a directory in the
        // way, an I/O error on a failing disk — is a machine somebody has to look at, and
        // offering to download 141 MB over it would be this program answering the wrong
        // question confidently.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ModelState::Absent { path: path.to_path_buf() }
        }
        Err(error) => {
            ModelState::Unreadable { path: path.to_path_buf(), detail: error.to_string() }
        }
    }
}

/// What the window asks: is the speech model here?
///
/// Cheap — one `stat`. The digest is **not** re-checked here, and that is a decision: hashing
/// 141 MB costs about a second of disk and CPU on this machine, on every launch, to defend
/// against a case [`fetch`] already makes impossible. The length is checked because it is free
/// and because it is what a half-copied file gets wrong.
pub fn state(model: &Model) -> ModelState {
    let Some(path) = model_path(model) else {
        return ModelState::Nowhere {
            reason: "this system does not name a cache directory for downloaded files".into(),
        };
    };
    let expected = if std::env::var_os(MODEL_ENV).is_some_and(|v| !v.is_empty()) {
        None
    } else {
        Some(model.bytes)
    };
    inspect(&path, expected)
}

/// How far a download has got. Task 7 renders it; nothing here does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// Bytes written so far.
    pub received: u64,
    /// What the server said the whole thing is, if it said. Never trusted for the integrity
    /// check — [`Model::bytes`] is.
    pub total: Option<u64>,
}

/// Download `model` into `dir`, and **either put a complete, verified file there or put
/// nothing there at all**.
///
/// # Why it is done this way
///
/// Three failures have to be impossible, and they are not the same failure:
///
/// 1. **A truncated file under the model's own name.** The bytes go to a uniquely named
///    `…{PART_SUFFIX}` file and are moved onto the model's name with a single `rename` once
///    they have been checked. `rename` within one directory is atomic on every platform this
///    ships to — POSIX says so, and Rust's Windows `rename` is `MoveFileExW` with
///    `MOVEFILE_REPLACE_EXISTING` — so the final name is never observed half-written. A
///    process killed mid-download leaves a `.part` file, and the next launch reads
///    [`ModelState::Absent`], which is the truth.
/// 2. **The wrong bytes at the right length.** A captive portal or a proxy answering `200`
///    with an HTML page is the ordinary version of this. The SHA-256 is computed over the
///    stream as it arrives — no second pass over 141 MB — and compared with
///    [`Model::sha256`] before the rename.
/// 3. **Two instances downloading at once.** The cache is shared, so this is reachable. Each
///    gets its own `.part` name (process id and a clock reading), so neither can be hashing
///    bytes the other wrote, and whichever renames last wins with an identical file.
///
/// The `.part` file is removed on every failure path. A power cut can still leave one; it is
/// named so that it reads as wreckage rather than as a model, and Task 7's delete button
/// clears the directory.
///
/// `progress` is called as bytes arrive. It runs on the caller's task, so it must be quick.
pub async fn fetch(
    model: &Model,
    dir: &Path,
    mut progress: impl FnMut(Progress),
) -> Result<PathBuf, Fault> {
    install_tls_provider();

    std::fs::create_dir_all(dir)
        .map_err(|e| Fault::Storage { path: dir.to_path_buf(), detail: e.to_string() })?;

    let final_path = dir.join(model.file);
    let part_path = dir.join(format!(
        "{}{PART_SUFFIX}-{}-{}",
        model.file,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));

    let outcome = stream_into(model, &part_path, &mut progress).await;
    match outcome {
        Ok(()) => match std::fs::rename(&part_path, &final_path) {
            Ok(()) => Ok(final_path),
            Err(e) => {
                let _ = std::fs::remove_file(&part_path);
                Err(Fault::Storage { path: final_path, detail: e.to_string() })
            }
        },
        Err(fault) => {
            let _ = std::fs::remove_file(&part_path);
            Err(fault)
        }
    }
}

/// The body of [`fetch`], separated so that every one of its failures goes through the one
/// `remove_file` above rather than through a copy of it per `?`.
async fn stream_into(
    model: &Model,
    part_path: &Path,
    progress: &mut impl FnMut(Progress),
) -> Result<(), Fault> {
    use std::io::Write as _;

    use sha2::Digest as _;

    let transfer = |detail: String| Fault::Transfer { url: model.url.into(), detail };

    let response = reqwest::get(model.url).await.map_err(|e| transfer(e.to_string()))?;
    if !response.status().is_success() {
        return Err(transfer(format!("the server answered {}", response.status())));
    }
    let total = response.content_length();

    let file = std::fs::File::create(part_path)
        .map_err(|e| Fault::Storage { path: part_path.to_path_buf(), detail: e.to_string() })?;
    let mut writer = std::io::BufWriter::new(file);
    let mut hasher = sha2::Sha256::new();
    let mut received: u64 = 0;
    let mut response = response;

    // Hashing and writing happen on this task rather than on a blocking one, and the
    // arithmetic is why: SHA-256 runs at about 210 MB/s here, so a 64 KB chunk is roughly
    // 0.3 ms of work — well under the point where an async executor notices. A `spawn_blocking`
    // per chunk would cost more in handoffs than it saves.
    while let Some(chunk) = response.chunk().await.map_err(|e| transfer(e.to_string()))? {
        hasher.update(&chunk);
        writer.write_all(&chunk).map_err(|e| Fault::Storage {
            path: part_path.to_path_buf(),
            detail: e.to_string(),
        })?;
        received += chunk.len() as u64;
        progress(Progress { received, total });
    }

    let file = writer.into_inner().map_err(|e| Fault::Storage {
        path: part_path.to_path_buf(),
        detail: e.to_string(),
    })?;
    // Not a nicety: without it the rename can be durable while the contents are not, and a
    // power cut then leaves a correctly named file full of nothing — exactly the failure the
    // rename exists to prevent.
    file.sync_all().map_err(|e| Fault::Storage {
        path: part_path.to_path_buf(),
        detail: e.to_string(),
    })?;
    drop(file);

    if received != model.bytes {
        return Err(Fault::NotTheModel {
            detail: format!("it is {received} bytes and the model is {}", model.bytes),
        });
    }
    let got = hex(&hasher.finalize());
    if got != model.sha256 {
        return Err(Fault::NotTheModel {
            detail: format!("its SHA-256 is {got} and the model's is {}", model.sha256),
        });
    }
    Ok(())
}

/// Choose the TLS provider, once per process.
///
/// `reqwest` is taken with `rustls-no-provider` so that this workspace does not grow an
/// `aws-lc-rs` build on top of the `ring` `iroh` already compiles — and the price of that is
/// that somebody has to say which provider it is. `install_default` answers `Err` when one is
/// already installed, which is a normal outcome here: `iroh` is on the same process and may
/// have got there first. Either way there is a provider afterwards, which is all this needs.
fn install_tls_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Lowercase hex, by hand. `hex` is in the lockfile through somebody else's dependency and
/// borrowing it here would make this crate's graph depend on that staying true.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// The window the encoder always walks, whatever it is given.
pub const FULL_WINDOW: Duration = Duration::from_secs(30);

/// The encoder positions that window maps to, and the model's own maximum.
///
/// Read back from the model as `n_audio_ctx`; asking for more is refused by whisper.cpp with
/// `audio_ctx is larger than the maximum allowed`.
pub const FULL_CTX: u16 = 1500;

/// Samples of 16 kHz audio per encoder position: 20 ms each.
pub const SAMPLES_PER_CTX: usize =
    (SAMPLE_RATE as usize * FULL_WINDOW.as_secs() as usize) / FULL_CTX as usize;

/// Headroom past the end of the audio: thirty-two positions, 640 ms.
///
/// **This is not what protects a short utterance, and an earlier version of this file said it
/// was.** Cutting at exactly the length of the audio is certainly wrong — see the table under
/// [`MIN_CTX`] — but so is cutting 640 ms past it. The thing that has to hold is a *floor* on
/// the whole context, and [`MIN_CTX`] is it. This constant stays because it costs nothing
/// (32 positions on an eleven-second turn is 3 ms) and because a little silence after the last
/// word is what tells the decoder the speech ended.
pub const CTX_MARGIN: u16 = 32;

/// The shortest audio whisper.cpp will look at.
///
/// Not ours: `whisper_full_with_state` returns **0 with no segments and a warning on stderr**
/// when the spectrogram is under ten frames — "input is too short - N ms < 100 ms". Read in
/// the vendored `whisper.cpp/src/whisper.cpp` at `delta_min = 10`. [`Stt::transcribe`] refuses
/// it up front instead, because "succeeded and said nothing" and "was never looked at" are not
/// the same answer and a session cannot tell them apart afterwards.
pub const MIN_AUDIO: Duration = Duration::from_millis(100);

/// **The floor, and the number this module turns on.** 300 positions is six seconds of encoder
/// context, whatever the utterance is.
///
/// Below about 215 positions whisper `base` stops transcribing what it heard and starts saying
/// something else. Measured on this machine on 2026-09-14, release build, `ggml-base.bin`,
/// `tests/audio/jfk.wav` — the first 1.5 s of it, whose words are "and so my fellow", and the
/// first 3 s, whose words are "and so my fellow Americans":
///
/// | clip | `audio_ctx` | time | what it said |
/// |---|---|---|---|
/// | 1.5 s | 75 (exact cut) | 2.60 s | "and saw my fellow" ×many |
/// | 1.5 s | 107 (cut + 32) | 6.17 s | "and saw my phone" ×many |
/// | 1.5 s | 139 | 3.56 s | **"And saw my phone"** — wrong, and not repeating |
/// | 1.5 s | 175 | 1.11 s | **"And saw my phone"** |
/// | 1.5 s | **225** | 0.72 s | "And so my fellow" |
/// | 1.5 s | 1500 (the full window) | 3.74 s | "And so my fellow" |
/// | 3 s | 150 (exact cut) | 2.71 s | its own sentence **44 times** |
/// | 3 s | 182 (cut + 32) | 10.74 s | its own sentence **36 times** |
/// | 3 s | **214** | 0.45 s | "And so my fellow Americans" |
/// | 3 s | 1500 (the full window) | 3.79 s | "And so my fellow Americans" |
/// | 11 s | 551 (exact cut) | 1.24 s | the whole sentence, correctly |
/// | 11 s | 1500 (the full window) | 3.85 s | the whole sentence, correctly |
///
/// And the clip that set the value, three seconds taken from 4.4 s in — a window that starts
/// and ends in the middle of a phrase, which is what a person who begins speaking before the
/// key is down produces. Its words are "what your country can do for you":
///
/// | `audio_ctx` | time | what it said |
/// |---|---|---|
/// | 300 | 4.93 s | "What your country can do for you? **What your country can do for you?**" |
/// | 375 | 4.26 s | right |
/// | **450** | **2.03 s** | right |
/// | 600 | 1.63 s | right |
/// | 1500 (the full window) | 4.99 s | right |
///
/// Three things fall out of that, and only the first was expected:
///
/// 1. **An undersized context is slow *and* wrong.** It is not a quality setting.
/// 2. **The margin is not the rule.** Eleven seconds is right with *no* margin at all, and
///    three seconds is wrong with the documented one. What both short cases need is a
///    context of about 215 positions in absolute terms — a little over four seconds — so the
///    guard is a floor, not an offset.
/// 3. **There is a band in between where it is fast and quietly wrong**: 1.5 s at 175
///    answered "And saw my phone" in 1.11 s with no repetition and nothing to notice, and the
///    mid-phrase clip at 300 said its sentence twice. That is the failure this floor exists
///    for, and it is why 450 and not the 225 the first two clips would have allowed: the
///    errors are not the same size. Being slow costs a second; being wrong sends an agent an
///    instruction nobody gave.
///
/// **450 is also not simply the cautious end of a trade.** On the clip that needed it, it is
/// two and a half times *faster* than leaving the lever alone, because a context that makes
/// whisper repeat itself makes it decode for longer and then makes its own temperature
/// fallback decode the whole thing again. Too small is slow; only too large is merely slow.
pub const MIN_CTX: u16 = 450;

/// How many encoder positions to give whisper for `samples` of 16 kHz mono audio.
///
/// `ceil(seconds / 30 * 1500) + 32`, **floored at [`MIN_CTX`]** and clamped to [`FULL_CTX`].
/// The floor is the part that matters and [`MIN_CTX`] carries the measurements.
///
/// **The clamp is what keeps a long turn merely slow rather than wrong.** `earshot` reads a
/// steady 440 Hz tone as a voice in 121 frames out of 125 — an alarm or a held note at
/// speaking pitch keeps a turn open — so an utterance arriving here is not guaranteed to be
/// short. Past thirty seconds there is nothing left to ask for: the model has 1500 positions
/// and whisper.cpp refuses more.
pub fn audio_ctx(samples: usize) -> u16 {
    let positions = samples.div_ceil(SAMPLES_PER_CTX);
    let with_margin = positions.saturating_add(CTX_MARGIN as usize);
    with_margin.clamp(MIN_CTX as usize, FULL_CTX as usize) as u16
}

/// The exact-cut value: the context that covers the audio and nothing more.
///
/// Public so a test can assert [`audio_ctx`] is **not** it. That is the assertion that stops
/// somebody reading [`CTX_MARGIN`] as decoration and simplifying it away, so it needs
/// something to compare against that is not a second copy of the same arithmetic.
pub fn exact_cut(samples: usize) -> u16 {
    samples.div_ceil(SAMPLES_PER_CTX).min(FULL_CTX as usize) as u16
}

/// The language whisper is told it is hearing.
///
/// **Told, not asked.** `set_language(None)` turns on detection, and detection cost 10.79 s
/// against 3.44 s on the same 3 s clip here — over three times slower, for a question this
/// product already knows the answer to. Korean is unmeasured and is step 8's problem.
pub const LANGUAGE: &str = "en";

/// How many threads whisper gets.
///
/// Four on this machine, which is what every measurement in this module was taken with. The
/// cap is there because whisper.cpp does not get faster past a machine's physical cores and
/// a 32-thread server would spend the difference on contention.
pub fn threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8)
}

/// Everything about a transcription that is a decision rather than a default.
///
/// A plain struct, and not just a `FullParams` built inline, because three of these were
/// measured and one of them is a trap — and `FullParams` has no getters, so a test could
/// otherwise only assert on them by reading this file as text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// `Some(LANGUAGE)`. `None` would mean detection; see [`LANGUAGE`].
    pub language: Option<&'static str>,
    /// See [`threads`].
    pub threads: usize,
    /// See [`audio_ctx`]. The whole reason this module exists.
    pub audio_ctx: u16,
    /// Always false. This is speech *into* the machine; translating it would silently answer
    /// an English question somebody asked in Korean.
    pub translate: bool,
    /// Always false: each turn is decoded without the previous turn's tokens.
    ///
    /// whisper carries the last transcript in as a prompt by default, which helps a
    /// continuous recording and hurts this: turns here are minutes apart and about different
    /// things, and a stale prompt is how whisper starts repeating a sentence nobody said.
    pub carry_previous_turn: bool,
}

impl Settings {
    /// The settings for one utterance of `samples` 16 kHz mono samples.
    pub fn for_audio(samples: usize) -> Settings {
        Settings {
            language: Some(LANGUAGE),
            threads: threads(),
            audio_ctx: audio_ctx(samples),
            translate: false,
            carry_previous_turn: false,
        }
    }
}

/// Whisper, loaded, and able to transcribe.
///
/// `Send + Sync`: `WhisperContext` is both, which is what lets one live behind an `Arc` and be
/// handed to [`transcribe`]'s blocking task over and over rather than reloaded per turn.
pub struct Stt {
    context: whisper_rs::WhisperContext,
}

impl Stt {
    /// Load a model from disk.
    ///
    /// Installs whisper.cpp's logging hooks first, once per process. Without them the library
    /// writes a screenful of model parameters to stderr on every load, which in `--headless`
    /// is the process log.
    pub fn load(path: &Path) -> Result<Stt, Fault> {
        static HOOKS: std::sync::Once = std::sync::Once::new();
        HOOKS.call_once(whisper_rs::install_logging_hooks);

        let mut parameters = whisper_rs::WhisperContextParameters::default();
        // No GPU. The default is already off without a GPU feature; saying so keeps it off
        // the day somebody turns one on for a different reason.
        parameters.use_gpu = false;

        let context = whisper_rs::WhisperContext::new_with_params(path, parameters)
            .map_err(|e| Fault::Whisper { detail: format!("{path:?} did not load: {e}") })?;
        Ok(Stt { context })
    }

    /// Transcribe one utterance. **Blocks** — see [`transcribe`] for the callable-from-async
    /// form, and do not call this one from a runtime thread.
    ///
    /// **Every timing in this module is a release figure, and the qualifier is load-bearing.**
    /// `whisper-rs-sys/build.rs` branches on `cfg!(debug_assertions)`: a release build gets
    /// `CMAKE_BUILD_TYPE=Release`, and everything else gets `RelWithDebInfo` **plus
    /// `-DWHISPER_DEBUG`**, whisper.cpp's verbose trace logging, one of whose 26 call sites is
    /// inside the per-token decode loop. Same machine, same model, same three seconds:
    /// **0.3 s under `cargo test --release` and 28.0 s under `cargo test`.** So an ordinary
    /// `cargo test` cannot measure this, and the one test that tries takes its deadline from
    /// `cfg!(debug_assertions)` for that reason.
    ///
    /// Audio is 16 kHz mono `f32`, which is what `capture::Capture` delivers.
    ///
    /// Audio shorter than [`MIN_AUDIO`] answers `Ok("")` without asking whisper, because
    /// whisper would answer the same thing by a different route — a warning and zero
    /// segments — and a caller cannot tell that apart from a silent room.
    pub fn transcribe(&self, audio: &[f32]) -> Result<String, Fault> {
        if audio.len() < samples_in(MIN_AUDIO) {
            return Ok(String::new());
        }
        let settings = Settings::for_audio(audio.len());

        let mut state = self
            .context
            .create_state()
            .map_err(|e| Fault::Whisper { detail: format!("no decoder state: {e}") })?;

        let mut params =
            whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(settings.language);
        params.set_n_threads(settings.threads as std::ffi::c_int);
        params.set_audio_ctx(settings.audio_ctx as std::ffi::c_int);
        params.set_translate(settings.translate);
        params.set_no_context(!settings.carry_previous_turn);
        // Nothing of whisper's own goes to stdout: this process has a window and a log, and
        // neither of them is a terminal it may print to.
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_no_timestamps(true);

        state
            .full(params, audio)
            .map_err(|e| Fault::Whisper { detail: format!("transcription failed: {e}") })?;

        let mut text = String::new();
        for segment in state.as_iter() {
            // Lossy on purpose: a model that emits one invalid UTF-8 byte must cost a
            // replacement character, not the whole sentence.
            text.push_str(segment.to_str_lossy().map_err(|e| Fault::Whisper {
                detail: format!("a transcribed segment could not be read: {e}"),
            })?.as_ref());
        }
        Ok(clean(&text))
    }
}

/// How many 16 kHz samples a span is.
pub fn samples_in(span: Duration) -> usize {
    (span.as_secs_f64() * SAMPLE_RATE as f64).round() as usize
}

/// Transcribe without blocking the runtime.
///
/// `state.full()` is a synchronous call into whisper.cpp that occupies a thread for as long as
/// it takes — a third of a second at best here, and seconds on a long turn — so it goes to the
/// blocking pool. The core has a window, a tray, a websocket and an audit log on the same
/// runtime, and none of them may stop while somebody is being transcribed.
pub async fn transcribe(stt: std::sync::Arc<Stt>, audio: Vec<f32>) -> Result<String, Fault> {
    off_the_runtime(move || stt.transcribe(&audio)).await?
}

/// Run `work` somewhere that is allowed to block, and turn a lost task into a [`Fault`].
///
/// Separated from [`transcribe`] so the property can be tested without a 141 MB model: a
/// current-thread runtime must still make progress on another task while this is running.
pub async fn off_the_runtime<T, F>(work: F) -> Result<T, Fault>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work).await.map_err(|_| Fault::Lost)
}

/// Tidy one transcript, and refuse to hand on a non-answer dressed as one.
///
/// whisper emits its own annotations for audio it found no speech in — `[BLANK_AUDIO]`,
/// `(silence)`, `[ Music ]` — as ordinary segment text. They are not a transcript, and
/// downstream they become a sentence an agent may act on, so a transcript that is *only*
/// annotation becomes the empty string and the session reports that nobody said anything.
/// Annotation mixed in with real words is left alone: cutting it would need to guess where a
/// sentence ends.
pub fn clean(text: &str) -> String {
    let trimmed = text.trim();
    let stripped = trimmed
        .strip_prefix('[')
        .and_then(|t| t.strip_suffix(']'))
        .or_else(|| trimmed.strip_prefix('(').and_then(|t| t.strip_suffix(')')));
    match stripped {
        Some(inner) if !inner.contains('[') && !inner.contains('(') => String::new(),
        _ => trimmed.split_whitespace().collect::<Vec<_>>().join(" "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seconds(s: f64) -> usize {
        (s * SAMPLE_RATE as f64) as usize
    }

    /// The values every timing in this module was taken at. If either moves, the tables in the
    /// module and on [`MIN_CTX`] are describing something else.
    #[test]
    fn the_context_matches_the_values_that_were_measured() {
        assert_eq!(
            audio_ctx(seconds(3.0)),
            450,
            "three seconds is short enough that the floor decides it; it took 32.77 s at the \
             182 this file used to produce"
        );
        // Exactly eleven seconds is 550 positions and no remainder. `tests/audio/jfk.wav` is
        // 176_017 samples — seventeen past that — so the recording itself lands on 583, which
        // is the number in the module's table.
        assert_eq!(audio_ctx(seconds(11.0)), 582);
        assert_eq!(audio_ctx(176_017), 583, "jfk.wav ran in 1.32 s at 583");
    }

    /// **The invariant the floor exists for**, asserted over the whole range rather than at the
    /// one length somebody happened to measure: whisper `base` starts inventing below about
    /// 215 positions, so nothing this function returns may be near that.
    ///
    /// A test of `MIN_CTX` alone would not do it — the mutation that matters is one that lets
    /// a *particular* length slip under, and only the range says it cannot.
    #[test]
    fn no_utterance_of_any_length_gets_a_context_that_was_measured_to_invent() {
        for tenths in 0..=350 {
            let samples = seconds(tenths as f64 / 10.0);
            assert!(
                audio_ctx(samples) >= 375,
                "{tenths} tenths of a second got {}; 175 answered \"And saw my phone\" in 1.11 s \
                 with nothing to notice, and 300 said a three-second phrase twice",
                audio_ctx(samples)
            );
        }
    }

    /// **The mutation-stopper for the offset half of the rule.** The exact cut is the case
    /// where a three-second clip said its own sentence forty-four times, and the floor above
    /// is what actually prevents it — but a value that fell back to the cut for *long* audio,
    /// where the floor does not apply, would be invisible to every other test here.
    ///
    /// Asserting the value exactly rather than merely "bigger" is deliberate: `+ 1` satisfies
    /// "bigger" and is not what was measured.
    #[test]
    fn the_context_is_never_the_exact_cut() {
        for tenths in 1..=290 {
            let samples = seconds(tenths as f64 / 10.0);
            let cut = exact_cut(samples);
            let used = audio_ctx(samples);
            assert_ne!(
                used, cut,
                "{tenths} tenths of a second: cutting the context at the length of the audio \
                 is the case where three seconds said its own sentence 44 times"
            );
            assert_eq!(
                used,
                cut.max(MIN_CTX - CTX_MARGIN) + CTX_MARGIN,
                "{tenths} tenths of a second: the value is the floor or the cut plus exactly \
                 CTX_MARGIN, and nothing in between"
            );
        }
    }

    /// The context must cover the audio. Under-covering truncates the utterance silently —
    /// whisper decodes what fits and says nothing about the rest.
    #[test]
    fn the_context_always_covers_the_audio_it_is_given() {
        for tenths in 1..=600 {
            let samples = seconds(tenths as f64 / 10.0);
            assert!(
                audio_ctx(samples) >= exact_cut(samples),
                "{tenths} tenths of a second is not covered"
            );
        }
    }

    /// A long turn has to stay *slow*, not become wrong. `earshot` holds a turn open on a
    /// steady tone at speaking pitch, so nothing here may assume an utterance is short.
    #[test]
    fn a_turn_longer_than_the_window_is_clamped_rather_than_overrunning() {
        assert_eq!(audio_ctx(seconds(30.0)), FULL_CTX);
        assert_eq!(audio_ctx(seconds(60.0)), FULL_CTX, "a minute is still 1500");
        assert_eq!(audio_ctx(seconds(3600.0)), FULL_CTX, "an alarm left ringing is still 1500");
        assert_eq!(audio_ctx(usize::MAX), FULL_CTX, "and the arithmetic does not wrap");
    }

    /// The floor is a floor: short audio gets it, and it is not quietly the exact cut again.
    #[test]
    fn everything_short_gets_the_floor_and_not_its_own_length() {
        assert_eq!(audio_ctx(samples_in(MIN_AUDIO)), MIN_CTX);
        assert_eq!(audio_ctx(1), MIN_CTX, "nothing below the floor goes below the floor");
        assert_eq!(audio_ctx(0), MIN_CTX);
        assert_eq!(MIN_AUDIO, Duration::from_millis(100), "whisper.cpp's own delta_min = 10");

        // Where the floor stops deciding: audio long enough that cut + margin passes it.
        let crossover = seconds((MIN_CTX - CTX_MARGIN) as f64 * 0.02);
        assert_eq!(audio_ctx(crossover), MIN_CTX);
        assert!(audio_ctx(crossover + SAMPLES_PER_CTX) > MIN_CTX);
    }

    /// A turn shorter than whisper will look at answers "" rather than being handed over for
    /// whisper to warn about on stderr and return nothing from.
    #[test]
    fn audio_too_short_for_whisper_to_look_at_is_refused_here() {
        assert!(samples_in(MIN_AUDIO) > 0);
        assert_eq!(samples_in(MIN_AUDIO), 1_600, "100 ms of 16 kHz mono");
        assert_eq!(samples_in(Duration::from_secs(1)), SAMPLE_RATE as usize);
    }

    /// [`Settings`] must carry the scaled context and not some other number, or every
    /// assertion about [`audio_ctx`] above is about a function nothing calls.
    #[test]
    fn the_settings_carry_the_scaled_context() {
        for tenths in [1u32, 7, 30, 110, 400] {
            let samples = seconds(tenths as f64 / 10.0);
            assert_eq!(Settings::for_audio(samples).audio_ctx, audio_ctx(samples));
        }
        assert_ne!(
            Settings::for_audio(seconds(3.0)).audio_ctx,
            FULL_CTX,
            "leaving the lever alone is the 3.4 s case"
        );
    }

    /// Whisper is *told* the language. Detecting it cost 10.79 s against 3.44 s on the same
    /// clip, and this program already knows the answer.
    #[test]
    fn the_language_is_configured_and_not_detected() {
        let settings = Settings::for_audio(seconds(3.0));

        assert_eq!(settings.language, Some(LANGUAGE));
        assert!(settings.language.is_some(), "None here is `detect the language`, three times slower");
    }

    /// The two that are quietly dangerous rather than slow.
    #[test]
    fn a_turn_is_decoded_alone_and_in_the_language_it_was_spoken_in() {
        let settings = Settings::for_audio(seconds(3.0));

        assert!(!settings.carry_previous_turn, "a stale prompt is how whisper starts repeating");
        assert!(!settings.translate, "this is speech into the machine, not a translator");
        assert!(settings.threads >= 1);
    }

    /// A model that is there, one that is not, and one that is the wrong size, told apart.
    #[test]
    fn three_answers_about_the_file_on_disk() {
        let dir = tempdir();
        let path = dir.join("ggml-base.bin");

        assert_eq!(
            inspect(&path, Some(10)),
            ModelState::Absent { path: path.clone() },
            "nothing there is `absent`"
        );

        std::fs::write(&path, b"1234567890").expect("write");
        assert_eq!(
            inspect(&path, Some(10)),
            ModelState::Ready { path: path.clone(), bytes: 10 }
        );

        std::fs::write(&path, b"123").expect("write");
        assert_eq!(
            inspect(&path, Some(10)),
            ModelState::Damaged { path: path.clone(), bytes: 3, expected: 10 },
            "a short file must not read as a missing one, or the next run fails inside whisper"
        );
    }

    /// **The fourth answer, and it used to be the first one.** Everything `metadata` refused was
    /// read as "nothing is there", which puts a Download button in front of a person a download
    /// cannot help. A directory is the case that is reachable on both platforms: `metadata`
    /// answers for one, with a length — 4096 on Linux — so it would otherwise have read as a
    /// damaged model of exactly that size.
    #[test]
    fn something_that_is_not_a_file_is_not_a_missing_one() {
        let dir = tempdir();
        let path = dir.join("ggml-base.bin");
        std::fs::create_dir(&path).expect("create a directory where the model should be");

        assert!(
            matches!(inspect(&path, Some(10)), ModelState::Unreadable { .. }),
            "a directory in the model's place is neither absent nor a short download; it is \
             something a person has to look at"
        );
    }

    /// The other half of the same rule, and the half only one platform can decide.
    ///
    /// `metadata` on a path whose *parent* is a file fails with `ENOTDIR` on Unix — which is not
    /// `NotFound`, and so is not an absence. Windows answers `ERROR_PATH_NOT_FOUND`, which `std`
    /// maps to `NotFound`, so there the same situation genuinely reads as absent and this test
    /// would be asserting the opposite of what the platform says. Hence `#[cfg(unix)]` rather
    /// than a cleverer path: the arm is right on both and only one of them can show it.
    #[cfg(unix)]
    #[test]
    fn a_path_that_cannot_be_walked_is_not_a_missing_file() {
        let dir = tempdir();
        let blocking = dir.join("in-the-way");
        std::fs::write(&blocking, b"not a directory").expect("write");

        assert!(
            matches!(inspect(&blocking.join("ggml-base.bin"), Some(10)), ModelState::Unreadable { .. }),
            "only `not found` is an absence; everything else is a machine somebody has to look at"
        );
    }

    /// A file an operator named themselves is not size-checked: they may have pointed at a
    /// different size of model on purpose, and calling that damaged is this program arguing
    /// with its user.
    #[test]
    fn a_file_named_by_hand_is_taken_as_given() {
        let dir = tempdir();
        let path = dir.join("mine.bin");
        std::fs::write(&path, b"abc").expect("write");

        assert!(matches!(inspect(&path, None), ModelState::Ready { bytes: 3, .. }));
    }

    /// The digest is checked, and a right-length wrong-content answer is the case that makes
    /// it worth the CPU: a proxy or a captive portal answering `200` with a page.
    #[tokio::test]
    async fn the_wrong_bytes_at_the_right_length_are_refused() {
        let body = b"xxxxxxxxxxxxxxxxxx".to_vec();
        let (url, _server) = serve(200, body.clone());
        let dir = tempdir();
        let model = model_for(&url, body.len() as u64, "00".repeat(32));

        let fault = fetch(&model, &dir, |_| {}).await.expect_err("the digest does not match");

        assert!(matches!(fault, Fault::NotTheModel { .. }), "{fault:?}");
        assert_nothing_left_behind(&dir, &model);
    }

    /// A body that is not the length the model is must leave **nothing** under the model's
    /// name, and no leftover a later run could mistake for one.
    ///
    /// **The digest here is the right one**, and that is the design of the test: with a wrong
    /// digest as well, the digest check would answer first and deleting the length check would
    /// change nothing anything here could see. Found by mutation, not by reading.
    #[tokio::test]
    async fn a_body_of_the_wrong_length_leaves_no_file_at_all() {
        use sha2::Digest as _;

        let body = b"not really a model".to_vec();
        let digest = hex(&sha2::Sha256::digest(&body));
        let (url, _server) = serve(200, body.clone());
        let dir = tempdir();
        // A complete, well-formed transfer of something that is not the model — a server
        // repointed at a different file is the realistic version — so only the length says so.
        let model = model_for(&url, body.len() as u64 + 1_000, digest);

        let fault = fetch(&model, &dir, |_| {}).await.expect_err("the length does not match");

        assert!(matches!(fault, Fault::NotTheModel { .. }), "{fault:?}");
        assert_nothing_left_behind(&dir, &model);
    }

    /// And the case that actually happens: a connection that stops in the middle. The server
    /// promises more than it sends and then closes, which is what a dropped Wi-Fi link looks
    /// like from here. `reqwest` fails the read, so this is a transfer fault rather than a
    /// digest one — and the file must still not be there.
    #[tokio::test]
    async fn a_connection_that_stops_halfway_leaves_no_file_at_all() {
        use sha2::Digest as _;

        let body = b"half a model".to_vec();
        let digest = hex(&sha2::Sha256::digest(&body));
        let (url, _server) = serve_declaring(200, body.clone(), body.len() + 4_000);
        let dir = tempdir();
        let model = model_for(&url, body.len() as u64, digest);

        let fault = fetch(&model, &dir, |_| {}).await.expect_err("the body stopped short");

        assert!(matches!(fault, Fault::Transfer { .. }), "{fault:?}");
        assert_nothing_left_behind(&dir, &model);
    }

    /// An HTTP error is its own fault: nothing is wrong with the file on disk, and telling
    /// somebody their model is corrupt when the server said 404 sends them to the wrong place.
    #[tokio::test]
    async fn a_server_that_refuses_is_a_transfer_fault() {
        let (url, _server) = serve(404, b"nope".to_vec());
        let dir = tempdir();
        let model = model_for(&url, 4, "00".repeat(32));

        let fault = fetch(&model, &dir, |_| {}).await.expect_err("404");

        assert!(matches!(fault, Fault::Transfer { .. }), "{fault:?}");
        assert_nothing_left_behind(&dir, &model);
    }

    /// And the good case, so that none of the above passes for a downloader that never
    /// succeeds at anything.
    #[tokio::test]
    async fn a_complete_download_lands_under_the_models_own_name() {
        use sha2::Digest as _;

        let body = b"this stands in for 141 MB of model".to_vec();
        let digest = hex(&sha2::Sha256::digest(&body));
        let (url, _server) = serve(200, body.clone());
        let dir = tempdir();
        let model = model_for(&url, body.len() as u64, digest);

        let mut seen = Vec::new();
        let path = fetch(&model, &dir, |p| seen.push(p.received)).await.expect("it matches");

        assert_eq!(path, dir.join(model.file));
        assert_eq!(std::fs::read(&path).expect("read"), body);
        assert_eq!(seen.last().copied(), Some(body.len() as u64), "progress reaches the end");
        assert!(parts_in(&dir).is_empty(), "the part file is gone: {:?}", parts_in(&dir));
    }

    /// `state.full()` occupies a thread for a third of a second at best. The runtime it was
    /// called from must keep going — the window, the tray and the websocket are on it.
    ///
    /// **The deadline is the assertion**, as elsewhere in this crate: a `transcribe` that ran
    /// whisper inline would not fail this, it would hang it.
    #[tokio::test]
    async fn transcription_does_not_stop_the_runtime_it_was_called_from() {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let blocked = off_the_runtime(move || {
            // Waits for the other task, which can only run if this is not on the runtime.
            rx.recv().expect("the runtime kept going");
            7u32
        });

        let also = async move {
            tokio::task::yield_now().await;
            tx.send(()).expect("the blocking side is still there");
        };

        let (answer, ()) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            futures_join(blocked, also),
        )
        .await
        .expect("a runtime that stopped is what this catches");

        assert_eq!(answer, Ok(7));
    }

    /// Whisper's own "there was nothing here" is text like any other segment, and downstream
    /// it becomes something an agent may act on.
    #[test]
    fn an_annotation_is_not_a_transcript() {
        assert_eq!(clean(" [BLANK_AUDIO] "), "");
        assert_eq!(clean("(silence)"), "");
        assert_eq!(clean("[ Music ]"), "");
        assert_eq!(
            clean(" and so my fellow  Americans "),
            "and so my fellow Americans",
            "ordinary text keeps its words and loses its padding"
        );
        assert_eq!(
            clean("[BLANK_AUDIO] turn the lights off"),
            "[BLANK_AUDIO] turn the lights off",
            "annotation beside real words is left alone rather than guessed at"
        );
    }

    /// Where the model goes is a decision: the cache, shared by every instance, never the
    /// per-instance data directory and never the temporary directory.
    #[test]
    fn the_model_lives_in_the_shared_cache_and_can_be_pointed_elsewhere() {
        if let Some(dir) = cache_dir() {
            assert!(
                !dir.starts_with(std::env::temp_dir()),
                "a 141 MB download into a directory the system empties is a download per launch"
            );
            assert!(dir.ends_with("models"));
        }

        // The override is what the tests and a person with their own copy both use. Asked of
        // the pure form: `set_var` here would change what the model-gated tests below see, on
        // another thread of the same process.
        assert_eq!(
            model_path_given(&BASE, Some("/somewhere/of/my/own/ggml-small.bin".into())),
            Some(PathBuf::from("/somewhere/of/my/own/ggml-small.bin"))
        );
        assert_eq!(
            model_path_given(&BASE, Some(std::ffi::OsString::new())),
            cache_dir().map(|d| d.join(BASE.file)),
            "an empty override is nobody naming a file, not a file called nothing"
        );
        assert_eq!(model_path_given(&BASE, None), cache_dir().map(|d| d.join(BASE.file)));
    }

    // ---- the model-gated half -------------------------------------------------------------
    //
    // **A 141 MB download is not something `cargo test` does**, so everything that needs real
    // whisper is gated on `ZYRIS_WHISPER_MODEL` naming a `ggml-base.bin` that is already
    // there. What that costs is written down in `CLAUDE.md`: CI never decodes a sample, so a
    // wiring mistake between `audio_ctx` and `set_audio_ctx` would pass there and fail only on
    // a machine with the model. The arithmetic above is covered either way.

    fn base_model() -> Option<PathBuf> {
        let path = PathBuf::from(std::env::var_os(MODEL_ENV)?);
        path.is_file().then_some(path)
    }

    fn jfk() -> Vec<f32> {
        let bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audio/jfk.wav"),
        )
        .expect("tests/audio/jfk.wav is in the repository");
        // 16-bit mono PCM; the header on this file is the canonical 44 bytes.
        bytes[44..]
            .chunks_exact(2)
            .map(|p| i16::from_le_bytes([p[0], p[1]]) as f32 / 32768.0)
            .collect()
    }

    /// The plan's "done when", and everything an undersized context does, in one test.
    ///
    /// **Three transcriptions of the same three seconds**, at the rule, at whisper's default,
    /// and at the exact cut — and every assertion is a *comparison between them* rather than a
    /// number on the clock. Two reasons, both found by writing it the other way first:
    ///
    /// 1. **`cargo test` runs tests in parallel**, so a wall-clock assertion measures how many
    ///    other whisper threads are on this machine's four cores. The same clip that takes
    ///    **0.88 s** with the machine otherwise idle took **5.89 s** inside the suite.
    /// 2. **`whisper-rs-sys` builds whisper.cpp differently in a debug profile** — see
    ///    [`Stt::transcribe`] — so the same assertion would need two constants anyway.
    ///
    /// A ratio survives both: contention and `-DWHISPER_DEBUG` slow every arm together.
    ///
    /// The repetition assertion is the one that would catch a silent regression. Measured
    /// 2026-09-14: at the rule the clip says its sentence **once**; at the exact cut it says it
    /// **forty-four times**.
    #[test]
    fn a_three_second_utterance_costs_a_fraction_of_the_full_window() {
        let Some(model) = base_model() else {
            eprintln!("skipped: set {MODEL_ENV} to a ggml-base.bin to run this");
            return;
        };
        let stt = Stt::load(&model).expect("the model loads");
        let audio: Vec<f32> = jfk().into_iter().take(seconds(3.0)).collect();

        // **Through `Stt::transcribe`, not through `transcribe_at`.** This is the only
        // assertion anywhere that ties `audio_ctx` to the `set_audio_ctx` call: with the
        // forced-context helper on all three arms, replacing that line with `FULL_CTX` passed
        // every test in this file. Found by mutation.
        let started = std::time::Instant::now();
        let scaled = stt.transcribe(&audio).expect("it transcribes");
        let scaled_took = started.elapsed();
        let (full, full_took) = timed(&stt, &audio, FULL_CTX);
        let (cut, cut_took) = timed(&stt, &audio, exact_cut(audio.len()));

        eprintln!(
            "3 s at {}: {scaled_took:?} {scaled:?} / at {FULL_CTX}: {full_took:?} {full:?} / \
             at {}: {cut_took:?} {cut:?}",
            audio_ctx(audio.len()),
            exact_cut(audio.len())
        );

        assert!(
            scaled.to_lowercase().contains("fellow americans"),
            "the scaled context has to transcribe it: {scaled:?}"
        );
        assert_eq!(says_it(&scaled), 1, "and say it once: {scaled:?}");
        assert_eq!(says_it(&full), 1, "as does the full window: {full:?}");
        assert!(
            says_it(&cut) > 5,
            "the exact cut repeats itself; it said the sentence {} times: {cut:?}",
            says_it(&cut)
        );
        assert!(
            scaled_took * 2 < full_took,
            "the whole point is that scaling the context is much cheaper than not; got \
             {scaled_took:?} against {full_took:?} for the full window"
        );
        assert!(
            cut_took > scaled_took,
            "an undersized context is slower as well as wrong; got {cut_took:?} against \
             {scaled_took:?}"
        );
    }

    /// How many times the transcript says the only phrase in these three seconds.
    fn says_it(text: &str) -> usize {
        text.to_lowercase().matches("fellow american").count()
    }

    // ---- helpers --------------------------------------------------------------------------

    fn timed(stt: &Stt, audio: &[f32], ctx: u16) -> (String, std::time::Duration) {
        let started = std::time::Instant::now();
        let text = transcribe_at(stt, audio, ctx);
        (text, started.elapsed())
    }

    /// The same call [`Stt::transcribe`] makes, with the context forced. Only a test wants
    /// this; the module never lets a caller choose a context.
    fn transcribe_at(stt: &Stt, audio: &[f32], ctx: u16) -> String {
        let mut state = stt.context.create_state().expect("state");
        let mut params =
            whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some(LANGUAGE));
        params.set_n_threads(threads() as std::ffi::c_int);
        params.set_audio_ctx(ctx as std::ffi::c_int);
        params.set_no_context(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_no_timestamps(true);
        state.full(params, audio).expect("full");
        let mut text = String::new();
        for segment in state.as_iter() {
            text.push_str(segment.to_str_lossy().expect("utf8").as_ref());
        }
        clean(&text)
    }

    /// A directory that goes away with the test. `tempfile` is not a dependency of this crate
    /// and one download test is not a reason to make it one.
    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zyris-stt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).expect("a temporary directory");
        dir
    }

    fn model_for(url: &str, bytes: u64, sha256: String) -> Model {
        Model {
            url: Box::leak(url.to_owned().into_boxed_str()),
            file: "ggml-base.bin",
            bytes,
            sha256: Box::leak(sha256.into_boxed_str()),
        }
    }

    fn parts_in(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .expect("read_dir")
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.to_string_lossy().contains(PART_SUFFIX))
            .collect()
    }

    fn assert_nothing_left_behind(dir: &Path, model: &Model) {
        assert!(
            !dir.join(model.file).exists(),
            "a failed download must never leave a file under the model's own name"
        );
        assert!(parts_in(dir).is_empty(), "a part file was left behind: {:?}", parts_in(dir));
    }

    /// One HTTP response on loopback. Enough to exercise the real `reqwest` path without a
    /// network, which is the only way the integrity rules above can be tested at all.
    fn serve(status: u16, body: Vec<u8>) -> (String, std::thread::JoinHandle<()>) {
        let declared = body.len();
        serve_declaring(status, body, declared)
    }

    /// The same, with `Content-Length` free to disagree with what is sent — which is how a
    /// connection dropped mid-transfer is reproduced without unplugging anything.
    fn serve_declaring(
        status: u16,
        body: Vec<u8>,
        declared: usize,
    ) -> (String, std::thread::JoinHandle<()>) {
        use std::io::{Read as _, Write as _};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}/ggml-base.bin", listener.local_addr().expect("addr"));
        let handle = std::thread::spawn(move || {
            let Ok((mut socket, _)) = listener.accept() else { return };
            let mut request = [0u8; 2048];
            let _ = socket.read(&mut request);
            let reason = if status == 200 { "OK" } else { "Not Found" };
            let head = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Length: {declared}\r\nConnection:                  close\r\n\r\n"
            );
            let _ = socket.write_all(head.as_bytes());
            let _ = socket.write_all(&body);
            let _ = socket.flush();
        });
        (url, handle)
    }

    /// Two futures to completion without pulling in `futures`. `tokio::join!` would do, and
    /// this is the same thing spelled so the timeout above can wrap it.
    async fn futures_join<A: std::future::Future, B: std::future::Future>(a: A, b: B) -> (A::Output, B::Output) {
        tokio::join!(a, b)
    }
}
