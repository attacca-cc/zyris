//! The model files this crate downloads, and the one shape that puts them on disk safely.
//!
//! **One type, not one per model.** `stt.rs` worked this out for whisper's 141 MB
//! `ggml-base.bin` — a uniquely named `.part` file, a SHA-256 computed over the stream, one
//! atomic rename, and a [`ModelState`] that separates "nothing there" from "there and
//! unreadable". Step 8 needs the same thing for Supertonic's six files and ten voice styles,
//! and a second implementation of it would be a second place for the rename to be forgotten.
//! So the machinery moved here and both callers name it: [`crate::stt::BASE`] is one
//! [`Model`], [`crate::tts::FILES`] is a table of them, and nothing about the integrity rules
//! knows which is which.
//!
//! What stayed behind in `stt.rs` is what is actually about whisper: which model, which
//! environment variable overrides it, and the two failures — a refusal from whisper.cpp, a
//! transcription task that went away — that have nothing to do with a file.

use std::path::{Path, PathBuf};

/// One file this build knows how to fetch and check.
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
    /// **Not "whatever was downloaded here once."** Every model this crate fetches lives in a
    /// Hugging Face repository pinned to a commit, and Hugging Face serves the Git LFS pointer
    /// for a large file at `.../raw/<commit>/<path>` — `oid sha256:…` and `size …`, which the
    /// `resolve` endpoint repeats as `x-linked-etag`. A small file is not in LFS and is hashed
    /// from the bytes the pinned commit serves. Either way the constant is upstream's claim
    /// about the file and not a local accident, and both were checked against the copies
    /// measured here.
    pub sha256: &'static str,
}

/// The suffix a download wears until it is complete and checked.
///
/// Visible in the cache directory, and meant to be: a file with this in its name is the
/// wreckage of an interrupted download and never a model. See [`fetch`].
pub const PART_SUFFIX: &str = ".part";

/// What went wrong getting a model file onto this disk, in words a window can render.
///
/// Four arms and no more: every one of them is about the network or the file system.
/// A caller that also has its own failures — `stt::Fault` has two — wraps this rather than
/// repeating it, because two enums with the same four arms and nothing that goes red when
/// they stop agreeing is the shape this workspace keeps finding in its own review notes.
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
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fault::NoCacheDirectory => write!(
                f,
                "this system does not name a cache directory, so there is nowhere to keep the \
                 downloaded models"
            ),
            Fault::Transfer { url, detail } => {
                write!(f, "a model could not be downloaded from {url}: {detail}")
            }
            Fault::NotTheModel { detail } => write!(
                f,
                "the download did not produce the model and has been discarded: {detail}"
            ),
            Fault::Storage { path, detail } => {
                write!(f, "a model could not be saved to {}: {detail}", path.display())
            }
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
    /// The file is there and is the size it should be. [`crate::stt::Stt::load`] is the next step.
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

// The names the platform directory is assembled from, as constants rather than as three
// literals inside the call below — **because the README spells this directory out on two
// platforms and one of the two was wrong.** `directories` 6.0.0 builds a Windows project path
// as `{organization}\{application}` and puts `cache` under it, so the model lives at
// `%LOCALAPPDATA%\attacca\zyris\cache\models`: the same `attacca\zyris` every other Windows path
// on that page already carries, and the segment that one was missing. On Linux the qualifier and
// the organization are ignored and it is `~/.cache/zyris/models`.
//
// Nothing on this machine can produce a Windows path, so what checks the page is
// `the_readme_names_the_directory_the_model_is_kept_in`: a test over the words, with both
// spellings assembled from these four rather than typed a second time.
pub(crate) const QUALIFIER: &str = "cc";
pub(crate) const ORGANIZATION: &str = "attacca";
pub(crate) const APPLICATION: &str = "zyris";
pub(crate) const MODELS: &str = "models";

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
    if let Some(dirs) = directories::ProjectDirs::from(QUALIFIER, ORGANIZATION, APPLICATION) {
        return Some(dirs.cache_dir().join(MODELS));
    }
    // `ProjectDirs` failed to name one, which on Linux means neither `XDG_CACHE_HOME` nor
    // `HOME` is set. `BaseDirs` is the same question asked with fewer requirements.
    if let Some(dirs) = directories::BaseDirs::new() {
        return Some(dirs.cache_dir().join(APPLICATION).join(MODELS));
    }
    // Deliberately **not** `std::env::temp_dir()`. `data_dir` falls back to it because an
    // audit log that cannot be written is survivable; a 141 MB download into a directory the
    // system empties is a download repeated on every launch, forever.
    None
}

/// The same decision with the environment passed in.
///
/// **Not a convenience.** `cargo test` runs a crate's tests on threads of one process, and
/// `set_var` is process-wide: a test that set [`crate::stt::MODEL_ENV`] to prove this rule would be
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
/// `expected` is `None` for a file an operator named through [`crate::stt::MODEL_ENV`]: they may well
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
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The README names this directory on two platforms and one of the two was wrong.**
    ///
    /// It said `%LOCALAPPDATA%\zyris\cache\models`, dropping the organization segment that every
    /// other Windows path on that page carries: `directories` 6.0.0's Windows project path is
    /// `{organization}\{application}` with `cache` under it, so it is
    /// `%LOCALAPPDATA%\attacca\zyris\cache\models`. A person following that page would have
    /// looked for 141 MB in a directory that does not exist, concluded nothing had downloaded,
    /// and had no way to tell that from a download that had failed.
    ///
    /// Nothing here can produce a Windows path — this machine is Linux and `directories` reads
    /// the platform, not a parameter — so the Windows half is checked as **words assembled from
    /// the same four constants `cache_dir` is written in terms of**, which is what makes the two
    /// unable to drift apart. The Linux half is checked against what this machine actually
    /// answers, which is the half a spelling test on its own could not reach.
    ///
    /// The same shape `announce.rs` uses over the tool count, and for the same reason: nothing
    /// else in this project ever reads that page again.
    #[test]
    fn the_readme_names_the_directory_the_model_is_kept_in() {
        let readme = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../README.md"),
        )
        .expect("the README is readable from this crate");

        let windows = format!("%LOCALAPPDATA%\\{ORGANIZATION}\\{APPLICATION}\\cache\\{MODELS}");
        assert!(
            readme.contains(&windows),
            "the README does not name `{windows}`, which is where `directories` puts the model \
             on Windows. Every other Windows path on that page carries `{ORGANIZATION}\\\
             {APPLICATION}`, and this one did not."
        );

        let linux = format!("~/.cache/{APPLICATION}/{MODELS}");
        assert!(readme.contains(&linux), "the README does not name `{linux}`");

        // And the Linux spelling is what this machine really answers, so the sentence above is
        // pinned to the code rather than only to itself. `HOME` is set wherever `cargo test`
        // runs; a machine that names no cache directory at all is `ModelState::Nowhere` and has
        // nothing for this to check.
        // The tail differs by platform and the sentence above says both: `directories` puts a
        // cache under `~/.cache/<app>` on Linux and under `<org>\\<app>\\cache` on Windows, so
        // only Linux ends in `<app>/<models>`. Asserting the Linux shape everywhere is what made
        // this fail on Windows while every claim it checks was true.
        if let Some(dir) = cache_dir() {
            let tail = if cfg!(windows) {
                std::path::Path::new(APPLICATION).join("cache").join(MODELS)
            } else {
                std::path::Path::new(APPLICATION).join(MODELS)
            };
            assert!(
                dir.ends_with(&tail),
                "this machine keeps models at {}, which does not end in {}",
                dir.display(),
                tail.display()
            );
        }

        // The qualifier is the one of the four the README never shows: `directories` ignores it
        // on Linux and on Windows and uses it only in a macOS bundle identifier. Named here so
        // that deleting it from `cache_dir` is a compile error rather than a silent move of
        // every model on a platform this project does not ship to.
        assert_eq!(QUALIFIER, "cc");
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

        // **Windows deletes lazily and this assertion has to know that.** `remove_file` on a
        // file another handle still holds does not unlink it; it marks it delete-pending, and
        // the entry stays enumerable until the last handle closes. On Linux the unlink is
        // immediate. The first version of this read the directory once for the condition and
        // again for the message and printed `a part file was left behind: []` — the file had
        // gone between the two reads, which is the whole tell.
        //
        // So the rule being asserted is "nothing is left behind", not "nothing is visible on
        // this instruction", and a bounded wait is what states it on both platforms. A real
        // leak still fails: the file never goes.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let left = parts_in(dir);
            if left.is_empty() {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "a part file was left behind: {left:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
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
}
