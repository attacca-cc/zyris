//! Recording a wake word, and keeping it. **Nothing reads these.**
//!
//! # Read this before writing any copy about it
//!
//! A wake word recorded here is **never matched against anything**. It is not compared to the
//! microphone, it does not wake this machine, and turning "keep the microphone on" on — when
//! there is such a switch — would not make it do so. All this module does is write what
//! somebody said into files, so that a matcher built later can be built **without asking them
//! to record it again**.
//!
//! That is a deliberate decision of the user's, taken on 2026-09-15, and it is worth writing
//! down why so that nobody quietly re-opens it:
//!
//! - The spec asks for a wake word "matched acoustically, so it is not tied to a language",
//!   which is the right shape and turned out to be a research problem. The published recipes
//!   reach **5.4 % false rejects at 0.1 false accepts an hour with five enrolments**, and
//!   **nobody has released weights.** What is downloadable is unlicensed, or English-only, or
//!   needs a gradient fine-tune that ONNX Runtime cannot do.
//! - The honest thing that could be built today is DTW over MFCC templates, which measures
//!   around **two false wakes an hour at recall 0.67** — a microphone that interrupts twice an
//!   hour and misses one attempt in three. That is not a feature, and shipping it with a label
//!   that says "wake word" would be the sixth time this project has claimed more than the code
//!   does.
//!
//! So [`NOTHING_READS_THESE`] is a sentence for the window and not a comment: whatever screen
//! offers this has to say it.
//!
//! # What is stored, and why that is the shape
//!
//! **Five takes** ([`TAKES`]), each a **16 kHz mono WAV in 32-bit IEEE float**, untrimmed,
//! beside a small JSON manifest.
//!
//! *Five, because the number cannot be raised afterwards.* A matcher that wants fewer can use a
//! subset; a matcher that wants more cannot invent the takes that were never recorded, and the
//! only published figure anybody has — the 5.4 % above — is measured at five enrolments. Three
//! is what a DTW template needs and what most few-shot enrolment flows ask for, so three would
//! be enough for the thing that could be built today and short for the thing that is worth
//! waiting for. The difference costs somebody about twenty seconds, once, and 640 KB of disk.
//!
//! *32-bit float at 16 kHz mono, because that is what the detector will be handed.* It is
//! exactly what [`crate::capture::Capture`] delivers and what [`crate::apm::Apm`] has finished
//! with — the same representation a matcher will see at run time, so nothing has to remember a
//! conversion, and no second copy of the audio exists at a different depth. A 16-bit file would
//! be smaller and would introduce a quantisation that is inaudible and that a template built
//! from it would nonetheless carry.
//!
//! *Untrimmed, because trimming is a decision made by today's rule.* [`crate::vad`]'s threshold
//! and margins are argued from one recorded sentence, and a matcher may well want its own
//! boundaries — or the attack of the first word, which the detector is measurably late for. So
//! the whole recording is kept and what *today's* endpointer thought is written into the
//! manifest as [`Spoken`] instead. Audio can always be trimmed later; the reverse is not
//! available.
//!
//! *Whether the processor touched it is recorded too.* A build without the `aec` feature has no
//! high-pass filter and no noise suppression, so its recordings are conditioned differently
//! from the build that has the library — measured in [`crate::apm`], where a hum that reads as
//! a voice in 6 frames of 125 reads as one in 0 after conditioning. A matcher trained on one
//! and run on the other would be quietly worse, and nothing would say so. [`Manifest`] carries
//! it per take.
//!
//! # Where it is kept
//!
//! The platform **data** directory, not the cache. [`crate::stt`] argues the model into the
//! cache because losing it costs a download; a recording of somebody's voice is the opposite —
//! losing it costs asking them to do it again, which is the one thing this module exists to
//! avoid. It is also not scoped by instance the way `zyris-app`'s credentials are: that scoping
//! is about secrets, and two copies of the same person's voice is not what it is for.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::apm::Conditioning;
use crate::capture::{SAMPLE_RATE, VAD_FRAME};
use crate::vad::{Ended, Endpointer};

/// The sentence a window has to render beside anything about the wake word.
///
/// Public, and a constant rather than something each screen writes for itself, because this is
/// the claim that must not drift: there is no matcher, and the recordings do nothing yet.
pub const NOTHING_READS_THESE: &str =
    "Zyris keeps this recording so a wake word can be added later without asking you to record \
     it again. Nothing listens for it yet, and saving it does not make Zyris respond to it.";

/// How many takes are kept. See the module for why the number is five and cannot be raised
/// after the fact.
pub const TAKES: usize = 5;

/// The longest one take may be.
///
/// A wake phrase is two or three words. Five seconds is several times that, so a recording
/// reaching it is a key that was not let go rather than a long wake word — the same reading
/// [`crate::session::MAX_TURN`] gives the cap it enforces, at a sixth of the length because this
/// is a phrase and not a sentence.
pub const MAX_TAKE: Duration = Duration::from_secs(5);

/// The most a recorder that can only stop on a frame boundary may keep.
///
/// **[`MAX_TAKE`] is not a whole number of detector frames and that is a real difference, not a
/// rounding one.** Five seconds is 80,000 samples and a frame is 256, so a recording that adds a
/// frame and then asks whether it has enough stops at 80,128 — and [`Take::recorded`] refuses
/// anything over 80,000. Every take that reached the cap was thrown away for being 8 ms too long,
/// which is the whole of what a person saw: the recording did not stop, and then it failed.
///
/// Two layers counting one limit in two units, with nothing that goes red when they disagree —
/// the shape this project keeps finding. So the recorder asks here rather than doing the
/// arithmetic again, and the test below pins that what this returns is something
/// [`Take::recorded`] accepts.
///
/// (The turn cap in `session.rs` has the same shape and gets away with it: 30 s is 480,000
/// samples, which *is* 1,875 frames exactly. An accident, and it is asserted there.)
pub fn longest_take(frame: usize) -> usize {
    let longest = crate::stt::samples_in(MAX_TAKE);
    longest - longest % frame
}

/// The file the takes are described in.
pub const MANIFEST: &str = "wake-word.json";

/// The version this module writes. Present so a later matcher can tell what it is reading
/// rather than guessing from the fields that happen to be there.
pub const VERSION: u32 = 1;

/// `WAVE_FORMAT_IEEE_FLOAT`. The takes are written at 32-bit float; see the module.
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
/// `WAVE_FORMAT_PCM`, which this module never writes and can read.
const WAVE_FORMAT_PCM: u16 = 1;

/// What the name of the `n`th take's file is. One-based, because it is shown to a person.
pub fn take_file(nth: usize) -> String {
    format!("take-{nth}.wav")
}

/// Something that stopped a wake word being recorded or read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// This platform would not name a data directory.
    NoDataDirectory,
    /// A take with no audio in it.
    Empty,
    /// A take longer than [`MAX_TAKE`].
    TooLong {
        /// How long it was, in whole seconds.
        seconds: u64,
    },
    /// There are already [`TAKES`] of them. Re-recording means clearing first, deliberately:
    /// silently replacing the oldest would make which five are kept depend on how many times
    /// somebody pressed the button.
    Enough,
    /// The bytes could not be written.
    Storage {
        /// What was being written.
        path: PathBuf,
        /// What the operating system said.
        detail: String,
    },
    /// Something on disk could not be read, or is not what this module writes.
    Unreadable {
        /// What was being read.
        path: PathBuf,
        /// Why it could not be used.
        detail: String,
    },
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fault::NoDataDirectory => write!(
                f,
                "this system does not name a directory Zyris may keep files in, so there is \
                 nowhere to put a wake word recording"
            ),
            Fault::Empty => write!(f, "nothing was recorded, so there is nothing to keep"),
            Fault::TooLong { seconds } => write!(
                f,
                "that recording is {seconds} seconds long, which is longer than a wake word; \
                 hold the key just while you say it"
            ),
            Fault::Enough => write!(
                f,
                "there are already {TAKES} recordings of the wake word; clear them to record it \
                 again"
            ),
            Fault::Storage { path, detail } => {
                write!(f, "the recording could not be saved to {}: {detail}", path.display())
            }
            Fault::Unreadable { path, detail } => {
                write!(f, "{} could not be read: {detail}", path.display())
            }
        }
    }
}

impl std::error::Error for Fault {}

/// Where the speech was, in samples, according to the rule in force when it was recorded.
///
/// **Metadata, not a cut.** The audio on disk is the whole recording; this says what
/// [`crate::vad`]'s endpointer thought of it at the time, so that a matcher can use today's
/// boundaries, its own, or neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Spoken {
    /// First sample of the speech, inclusive.
    pub first: usize,
    /// Last sample of the speech, inclusive.
    pub last: usize,
}

/// One recording, in memory.
#[derive(Debug, Clone, PartialEq)]
pub struct Take {
    samples: Vec<f32>,
    conditioning: Conditioning,
    spoken: Option<Spoken>,
}

impl Take {
    /// Accept what was recorded, or say why it is not a take.
    ///
    /// `samples` is 16 kHz mono, conditioned by [`crate::apm::Apm`] exactly as the live pipeline
    /// conditions it — `describe()`'s answer is what goes into `conditioning`, so that a
    /// recording made on a build with no echo canceller is not silently mistaken for one made
    /// on a build that has it.
    ///
    /// The endpointer is run over it **and its verdict is kept rather than applied**; see the
    /// module. A recording the endpointer found no speech in is still a take: that is a judgement
    /// today's rule made about a phrase it was never argued from, and throwing the audio away on
    /// it would be exactly the mistake this module is written to avoid.
    pub fn recorded(samples: Vec<f32>, conditioning: Conditioning) -> Result<Take, Fault> {
        if samples.is_empty() {
            return Err(Fault::Empty);
        }
        let longest = crate::stt::samples_in(MAX_TAKE);
        if samples.len() > longest {
            return Err(Fault::TooLong {
                seconds: (samples.len() as u64) / u64::from(SAMPLE_RATE),
            });
        }
        Ok(Take { spoken: look_for_speech(&samples), samples, conditioning })
    }

    /// The audio, untrimmed.
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    /// What the processor did to it.
    pub fn conditioning(&self) -> &Conditioning {
        &self.conditioning
    }

    /// Where today's rule thought the speech was, if it found any.
    pub fn spoken(&self) -> Option<Spoken> {
        self.spoken
    }
}

/// Run the endpointer over a whole recording and report where it thought the speech was.
///
/// A fresh [`Endpointer`] per recording, and [`Endpointer::finish`] at the end, because a take
/// is one closed recording rather than a stream that goes on.
fn look_for_speech(samples: &[f32]) -> Option<Spoken> {
    let mut endpointer = Endpointer::new();
    for frame in samples.chunks(VAD_FRAME) {
        if frame.len() == VAD_FRAME {
            // The only error is a wrong length, which the guard above rules out.
            let _ = endpointer.push(frame);
        }
    }
    match endpointer.finish() {
        Ended::Utterance { first, last, .. } => Some(Spoken {
            first: first * VAD_FRAME,
            last: ((last + 1) * VAD_FRAME - 1).min(samples.len().saturating_sub(1)),
        }),
        Ended::TooShort { .. } => None,
    }
}

/// One line of the manifest.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recorded {
    /// The file, relative to the store's directory.
    pub file: String,
    /// How many samples are in it, so a reader can check the file against what was meant.
    pub samples: usize,
    /// When, in seconds since the epoch. `None` where the clock would not say.
    pub recorded_at: Option<u64>,
    /// `"full"` or `"untouched"`: whether the processor conditioned it. See the module.
    pub conditioning: String,
    /// Where today's endpointer thought the speech was. `None` means it found none, which is
    /// not a reason to have thrown the audio away.
    pub spoken: Option<Spoken>,
}

/// What is on disk, described.
///
/// Written so that a matcher built in a year can read it without this crate: it says the rate,
/// the channel count and the sample format rather than assuming whoever reads it knows.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    /// [`VERSION`].
    pub version: u32,
    /// 16000.
    pub sample_rate: u32,
    /// 1.
    pub channels: u16,
    /// `"f32"`.
    pub sample_format: String,
    /// How many takes this store means to hold, as of when it was written.
    pub wanted: usize,
    /// The takes, in the order they were recorded.
    pub takes: Vec<Recorded>,
}

impl Manifest {
    fn empty() -> Manifest {
        Manifest {
            version: VERSION,
            sample_rate: SAMPLE_RATE,
            channels: 1,
            sample_format: "f32".to_string(),
            wanted: TAKES,
            takes: Vec::new(),
        }
    }
}

/// How far along the recording is.
///
/// **Four answers, not two.** "Nothing recorded" and "recorded, and unreadable" are different
/// things a window says differently, and this project has now had to separate them three times
/// — the audit tail, the inbox, and the MCP server list — before adding a fourth.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum Enrolment {
    /// Nobody has recorded anything.
    Nothing,
    /// Some takes, and how many are still wanted.
    #[serde(rename_all = "camelCase")]
    Partial {
        /// How many are stored.
        recorded: usize,
        /// [`TAKES`].
        wanted: usize,
    },
    /// All [`TAKES`] of them. Still matched against nothing — see [`NOTHING_READS_THESE`].
    #[serde(rename_all = "camelCase")]
    Complete {
        /// How many are stored.
        recorded: usize,
    },
    /// There is something there and it could not be read.
    #[serde(rename_all = "camelCase")]
    Unreadable {
        /// A sentence for a person.
        reason: String,
    },
}

/// Where the takes live.
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// A store in a directory of your choosing. The directory is created when something is
    /// written, not here.
    pub fn at(dir: impl Into<PathBuf>) -> Store {
        Store { dir: dir.into() }
    }

    /// The one this machine uses: the platform data directory. See the module for why it is the
    /// data directory and not the cache.
    pub fn on_this_machine() -> Result<Store, Fault> {
        Ok(Store { dir: default_dir().ok_or(Fault::NoDataDirectory)? })
    }

    /// Where this one is.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// How far along the recording is. Cheap, and safe to call on every render.
    pub fn state(&self) -> Enrolment {
        match self.manifest() {
            Err(fault) => Enrolment::Unreadable { reason: fault.to_string() },
            Ok(None) => Enrolment::Nothing,
            Ok(Some(manifest)) if manifest.takes.is_empty() => Enrolment::Nothing,
            Ok(Some(manifest)) => {
                let recorded = manifest.takes.len();
                if recorded >= TAKES {
                    Enrolment::Complete { recorded }
                } else {
                    Enrolment::Partial { recorded, wanted: TAKES }
                }
            }
        }
    }

    /// Keep one take, and say where that leaves the enrolment.
    ///
    /// The audio is written first and the manifest second, so a process killed in between
    /// leaves a file nothing points at rather than a line pointing at nothing — the first is
    /// wreckage a later recording overwrites, the second is a store that reads as complete and
    /// is not.
    pub fn add(&self, take: &Take) -> Result<Enrolment, Fault> {
        let mut manifest = self.manifest()?.unwrap_or_else(Manifest::empty);
        if manifest.takes.len() >= TAKES {
            return Err(Fault::Enough);
        }

        std::fs::create_dir_all(&self.dir)
            .map_err(|e| Fault::Storage { path: self.dir.clone(), detail: e.to_string() })?;

        let file = take_file(manifest.takes.len() + 1);
        let path = self.dir.join(&file);
        write_wav(&path, take.samples())?;

        manifest.takes.push(Recorded {
            file,
            samples: take.samples().len(),
            recorded_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|since| since.as_secs()),
            conditioning: match take.conditioning() {
                Conditioning::Full => "full".to_string(),
                Conditioning::Untouched { .. } => "untouched".to_string(),
            },
            spoken: take.spoken(),
        });
        self.write_manifest(&manifest)?;
        Ok(self.state())
    }

    /// Read every take back, in the order they were recorded.
    ///
    /// **This is the promise the whole module is for**, and it is a method rather than a
    /// description of a file format so that a matcher built later is written against something
    /// that is exercised today.
    pub fn takes(&self) -> Result<Vec<Take>, Fault> {
        let Some(manifest) = self.manifest()? else { return Ok(Vec::new()) };
        let mut takes = Vec::with_capacity(manifest.takes.len());
        for recorded in &manifest.takes {
            let path = self.dir.join(&recorded.file);
            let samples = read_wav(&path)?;
            if samples.len() != recorded.samples {
                return Err(Fault::Unreadable {
                    path,
                    detail: format!(
                        "the manifest says {} samples and the file holds {}",
                        recorded.samples,
                        samples.len()
                    ),
                });
            }
            takes.push(Take {
                samples,
                conditioning: if recorded.conditioning == "full" {
                    Conditioning::Full
                } else {
                    Conditioning::Untouched { reason: crate::apm::NO_ECHO_CANCELLER.to_string() }
                },
                spoken: recorded.spoken,
            });
        }
        Ok(takes)
    }

    /// Forget the wake word: every take and the manifest.
    ///
    /// Named files rather than the whole directory, because a directory somebody pointed this
    /// at may hold things nobody here put there.
    pub fn clear(&self) -> Result<(), Fault> {
        for nth in 1..=TAKES {
            remove(&self.dir.join(take_file(nth)))?;
        }
        remove(&self.dir.join(MANIFEST))
    }

    /// The manifest, or `None` where nothing has been recorded. An unreadable one is a
    /// [`Fault`] and not an empty store — see [`Enrolment`].
    pub fn manifest(&self) -> Result<Option<Manifest>, Fault> {
        let path = self.dir.join(MANIFEST);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Fault::Unreadable { path, detail: e.to_string() }),
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| Fault::Unreadable { path, detail: e.to_string() })
    }

    /// Written through a uniquely named temporary file and one `rename`, the way
    /// [`crate::stt`]'s download is: the manifest is the only thing that says which files are
    /// takes, and a half-written one would make the store unreadable rather than incomplete.
    fn write_manifest(&self, manifest: &Manifest) -> Result<(), Fault> {
        let path = self.dir.join(MANIFEST);
        let bytes = serde_json::to_vec_pretty(manifest)
            .map_err(|e| Fault::Storage { path: path.clone(), detail: e.to_string() })?;
        let part = self.dir.join(format!(
            "{MANIFEST}.part-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| since.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::write(&part, &bytes)
            .map_err(|e| Fault::Storage { path: part.clone(), detail: e.to_string() })?;
        std::fs::rename(&part, &path).map_err(|e| {
            let _ = std::fs::remove_file(&part);
            Fault::Storage { path, detail: e.to_string() }
        })
    }
}

fn remove(path: &Path) -> Result<(), Fault> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Fault::Storage { path: path.to_path_buf(), detail: e.to_string() }),
    }
}

/// Where takes live on this machine.
///
/// The **data** directory: losing it costs asking somebody to record their voice again, which
/// is what a cache directory is explicitly allowed to do to whatever is in it.
pub fn default_dir() -> Option<PathBuf> {
    if let Some(dirs) = directories::ProjectDirs::from("cc", "attacca", "zyris") {
        return Some(dirs.data_dir().join("wake-word"));
    }
    // `ProjectDirs` failed, which on Linux means neither `XDG_DATA_HOME` nor `HOME` is set.
    if let Some(dirs) = directories::BaseDirs::new() {
        return Some(dirs.data_dir().join("zyris").join("wake-word"));
    }
    None
}

// -------------------------------------------------------------------------------------------
// WAV
// -------------------------------------------------------------------------------------------

/// Write 16 kHz mono 32-bit float.
///
/// Thirty lines rather than `hound`, for the reason the rest of this crate gives about its
/// dependency graph: one known format, written once and read once, on a crate whose whole
/// feature layout exists to keep that graph small.
fn write_wav(path: &Path, samples: &[f32]) -> Result<(), Fault> {
    let data = samples.len() * 4;
    let mut out = Vec::with_capacity(44 + data);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((36 + data) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&WAVE_FORMAT_IEEE_FLOAT.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(SAMPLE_RATE * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data as u32).to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    std::fs::write(path, &out)
        .map_err(|e| Fault::Storage { path: path.to_path_buf(), detail: e.to_string() })
}

/// Read one back.
///
/// Accepts the 32-bit float this module writes and 16-bit PCM, which is what every other tool
/// on a machine produces — a person who replaces a take with one they recorded elsewhere should
/// not get a store that reads as damaged. Anything else is [`Fault::Unreadable`] rather than
/// silence, because a file this cannot read is a wake word that is not there.
fn read_wav(path: &Path) -> Result<Vec<f32>, Fault> {
    let bytes = std::fs::read(path)
        .map_err(|e| Fault::Unreadable { path: path.to_path_buf(), detail: e.to_string() })?;
    let bad = |detail: String| Fault::Unreadable { path: path.to_path_buf(), detail };

    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(bad("this is not a WAV file".to_string()));
    }

    let (mut format, mut channels, mut rate, mut bits) = (0u16, 0u16, 0u32, 0u16);
    let mut samples: Option<Vec<f32>> = None;
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().expect("4 bytes")) as usize;
        let body = &bytes[at + 8..(at + 8 + size).min(bytes.len())];
        match id {
            b"fmt " if body.len() >= 16 => {
                format = u16::from_le_bytes(body[0..2].try_into().expect("2 bytes"));
                channels = u16::from_le_bytes(body[2..4].try_into().expect("2 bytes"));
                rate = u32::from_le_bytes(body[4..8].try_into().expect("4 bytes"));
                bits = u16::from_le_bytes(body[14..16].try_into().expect("2 bytes"));
            }
            b"data" => {
                samples = Some(match (format, bits) {
                    (WAVE_FORMAT_IEEE_FLOAT, 32) => body
                        .chunks_exact(4)
                        .map(|s| f32::from_le_bytes(s.try_into().expect("4 bytes")))
                        .collect(),
                    (WAVE_FORMAT_PCM, 16) => body
                        .chunks_exact(2)
                        .map(|s| {
                            f32::from(i16::from_le_bytes(s.try_into().expect("2 bytes")))
                                / 32768.0
                        })
                        .collect(),
                    _ => {
                        return Err(bad(format!(
                            "Zyris reads 32-bit float and 16-bit WAV files, and this one is \
                             format {format} at {bits} bits"
                        )));
                    }
                });
            }
            _ => {}
        }
        at += 8 + size + (size & 1);
    }

    if channels != 1 || rate != SAMPLE_RATE {
        return Err(bad(format!(
            "a wake word recording is one channel at {SAMPLE_RATE} Hz, and this one is \
             {channels} at {rate}"
        )));
    }
    samples.ok_or_else(|| bad("there is no audio in this file".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::SAMPLE_RATE;

    fn seconds(count: f32) -> usize {
        (SAMPLE_RATE as f32 * count) as usize
    }

    fn silence(secs: f32) -> Vec<f32> {
        vec![0.0; seconds(secs)]
    }

    /// `tests/audio/jfk.wav`, cut to something a wake word's length, with silence either side.
    /// Real speech, because the endpointer's verdict is one of the things being stored and a
    /// sine is not what it was argued from.
    fn spoken_phrase() -> Vec<f32> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audio/jfk.wav");
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let mut at = 12;
        let mut words = Vec::new();
        while at + 8 <= bytes.len() {
            let id = &bytes[at..at + 4];
            let size =
                u32::from_le_bytes(bytes[at + 4..at + 8].try_into().expect("4 bytes")) as usize;
            if id == b"data" {
                let body = &bytes[at + 8..(at + 8 + size).min(bytes.len())];
                words = body
                    .chunks_exact(2)
                    .map(|s| f32::from(i16::from_le_bytes(s.try_into().expect("2"))) / 32768.0)
                    .collect::<Vec<f32>>();
                break;
            }
            at += 8 + size + (size & 1);
        }
        let mut phrase = silence(0.5);
        phrase.extend_from_slice(&words[seconds(0.5)..seconds(1.5)]);
        phrase.extend(silence(0.5));
        phrase
    }

    /// A directory that goes away with the test. `tempfile` is not a dependency of this crate
    /// and one store is not a reason to make it one — the same call `crate::stt`'s tests make.
    struct Scratch {
        dir: PathBuf,
    }

    impl Scratch {
        fn new() -> Scratch {
            let dir = std::env::temp_dir().join(format!(
                "zyris-wake-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or_default()
            ));
            std::fs::create_dir_all(&dir).expect("a temporary directory");
            Scratch { dir }
        }

        fn store(&self) -> Store {
            Store::at(&self.dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn take(samples: Vec<f32>) -> Take {
        Take::recorded(samples, Conditioning::Full).expect("a take")
    }

    // -----------------------------------------------------------------------------------------
    // The claim
    // -----------------------------------------------------------------------------------------

    /// **The one thing in this module that must not drift.** Matching is out of scope by
    /// decision, so the sentence a window renders has to say both halves: nothing listens for
    /// it, and saving one does not change that. A screen that said "wake word saved" and no
    /// more would be this project's sixth piece of copy claiming more than the code does.
    #[test]
    fn the_copy_says_plainly_that_nothing_listens_yet() {
        let said = NOTHING_READS_THESE.to_ascii_lowercase();
        assert!(
            said.contains("nothing listens for it yet"),
            "the copy has to say that nothing listens: {NOTHING_READS_THESE}"
        );
        assert!(
            said.contains("does not make zyris respond"),
            "and that saving one does not change that: {NOTHING_READS_THESE}"
        );
        assert!(
            said.contains("without asking you to record it again"),
            "and why it is being kept at all: {NOTHING_READS_THESE}"
        );
    }

    // -----------------------------------------------------------------------------------------
    // What is kept
    // -----------------------------------------------------------------------------------------

    /// The format decision, asserted where it can be wrong: 32-bit float in and exactly the
    /// same bits out. A 16-bit file would round every sample and nothing downstream would say
    /// so.
    #[test]
    fn a_take_comes_back_out_sample_for_sample() {
        let scratch = Scratch::new();
        let store = scratch.store();
        let said: Vec<f32> = (0..seconds(1.0))
            .map(|n| (n as f32 * 0.000_137).sin() * 0.987_654_3)
            .collect();

        store.add(&take(said.clone())).expect("stored");

        let back = store.takes().expect("read back");
        assert_eq!(back.len(), 1);
        assert_eq!(
            back[0].samples(),
            said.as_slice(),
            "the recording has to come back bit for bit, or the matcher built later is trained \
             on something the microphone will never hand it"
        );
    }

    /// **Untrimmed**, and today's verdict kept beside it rather than applied.
    #[test]
    fn the_whole_recording_is_kept_and_the_endpointers_verdict_only_described() {
        let scratch = Scratch::new();
        let store = scratch.store();
        let phrase = spoken_phrase();

        store.add(&take(phrase.clone())).expect("stored");

        let back = store.takes().expect("read back");
        assert_eq!(
            back[0].samples().len(),
            phrase.len(),
            "every sample recorded is kept: a later matcher may want its own boundaries, and \
             audio that was trimmed away cannot be given back"
        );
        let spoken = back[0].spoken().expect("today's rule found the phrase");
        assert!(
            spoken.first > 0 && spoken.last < phrase.len() - 1,
            "and it has to be a description of where the speech was, not the whole file: {spoken:?}"
        );
    }

    /// A phrase today's rule hears nothing in is still kept. The rule was argued from one
    /// recorded sentence and a wake word is not one; throwing the audio away on its say-so is
    /// exactly the mistake this module exists to avoid.
    #[test]
    fn a_recording_the_rule_found_no_speech_in_is_still_kept() {
        let scratch = Scratch::new();
        let store = scratch.store();

        let quiet = take(silence(1.0));
        assert_eq!(quiet.spoken(), None);
        store.add(&quiet).expect("stored anyway");

        assert_eq!(store.takes().expect("read back")[0].samples().len(), seconds(1.0));
    }

    /// Which build recorded it, because the two condition the microphone differently and a
    /// matcher trained on one and run on the other would be quietly worse.
    #[test]
    fn which_build_conditioned_it_survives_the_round_trip() {
        let scratch = Scratch::new();
        let store = scratch.store();
        let untouched = Conditioning::Untouched { reason: crate::apm::NO_ECHO_CANCELLER.into() };

        store
            .add(&Take::recorded(spoken_phrase(), untouched.clone()).expect("a take"))
            .expect("stored");

        assert_eq!(
            store.manifest().expect("readable").expect("written").takes[0].conditioning,
            "untouched"
        );
        assert_eq!(store.takes().expect("read back")[0].conditioning(), &untouched);
    }

    /// The manifest has to be readable by something that is not this crate, so it says the rate,
    /// the channel count and the sample format rather than assuming them.
    #[test]
    fn the_manifest_says_what_a_later_reader_would_otherwise_have_to_guess() {
        let scratch = Scratch::new();
        let store = scratch.store();
        store.add(&take(spoken_phrase())).expect("stored");

        let manifest = store.manifest().expect("readable").expect("written");
        assert_eq!(manifest.version, VERSION);
        assert_eq!(manifest.sample_rate, SAMPLE_RATE);
        assert_eq!(manifest.channels, 1);
        assert_eq!(manifest.sample_format, "f32");
        assert_eq!(manifest.wanted, TAKES);
        assert_eq!(manifest.takes[0].file, take_file(1));
        assert_eq!(manifest.takes[0].samples, spoken_phrase().len());
    }

    // -----------------------------------------------------------------------------------------
    // How many
    // -----------------------------------------------------------------------------------------

    /// Five, and a sixth is refused rather than silently replacing one — which of the five were
    /// kept would otherwise depend on how many times somebody pressed the button.
    /// **Five is a decision and this is where it is made.** The test below is written in terms
    /// of [`TAKES`] and passes for any value of it, which is right — it is about the rule — and
    /// leaves the number itself untested; a mutation changing it to three survived a pass for
    /// exactly that reason. The argument is in the module: the count cannot be raised after
    /// somebody has recorded, and the only published figure anybody has is measured at five.
    #[test]
    fn five_is_the_number_and_not_an_arbitrary_one() {
        assert_eq!(TAKES, 5);
    }

    /// What the recorder may keep is something the store accepts, and it is not obvious.
    ///
    /// **This is a bug that shipped and a person found in the first five minutes.** `MAX_TAKE`
    /// is 80,000 samples and a detector frame is 256, so a recorder that adds a frame and then
    /// asks whether it has enough stops at 80,128 — and `Take::recorded` refuses anything over
    /// 80,000. Every take that reached the cap was thrown away for being 8 ms too long, and what
    /// a person saw was a recording that would not stop and then failed.
    ///
    /// Two layers counting one limit in two units. The test is written against both of them at
    /// once for that reason: it asks `longest_take` and hands the answer to `recorded`.
    #[test]
    fn the_longest_the_recorder_may_keep_is_a_length_the_store_accepts() {
        let longest = longest_take(crate::capture::VAD_FRAME);
        assert_eq!(longest % crate::capture::VAD_FRAME, 0, "a recorder stops on a frame boundary");

        Take::recorded(vec![0.1; longest], Conditioning::Untouched { reason: "test".into() })
            .expect("the longest take a recorder can make is one the store keeps");

        // And one frame more is refused, which is what the recorder used to hand over.
        let over = longest + crate::capture::VAD_FRAME;
        assert!(
            over > crate::stt::samples_in(MAX_TAKE),
            "the next frame really does cross the limit, or this test proves nothing"
        );
        let refused = Take::recorded(
            vec![0.1; over],
            Conditioning::Untouched { reason: "test".into() },
        );
        assert!(matches!(refused, Err(Fault::TooLong { .. })), "{refused:?}");
    }

    #[test]
    fn five_takes_are_kept_and_the_sixth_is_refused() {
        let scratch = Scratch::new();
        let store = scratch.store();
        assert_eq!(store.state(), Enrolment::Nothing);

        for nth in 1..TAKES {
            assert_eq!(
                store.add(&take(spoken_phrase())).expect("stored"),
                Enrolment::Partial { recorded: nth, wanted: TAKES }
            );
        }
        assert_eq!(
            store.add(&take(spoken_phrase())).expect("stored"),
            Enrolment::Complete { recorded: TAKES }
        );

        assert_eq!(store.add(&take(spoken_phrase())), Err(Fault::Enough));
        assert_eq!(store.takes().expect("read back").len(), TAKES);
    }

    #[test]
    fn a_recording_far_longer_than_a_wake_word_is_refused() {
        let too_long = silence(MAX_TAKE.as_secs_f32() + 1.0);

        assert_eq!(
            Take::recorded(too_long, Conditioning::Full),
            Err(Fault::TooLong { seconds: MAX_TAKE.as_secs() + 1 })
        );
        assert!(Take::recorded(silence(MAX_TAKE.as_secs_f32()), Conditioning::Full).is_ok());
    }

    #[test]
    fn nothing_recorded_is_not_a_take() {
        assert_eq!(Take::recorded(Vec::new(), Conditioning::Full), Err(Fault::Empty));
    }

    // -----------------------------------------------------------------------------------------
    // Three answers about what is on disk
    // -----------------------------------------------------------------------------------------

    /// "Nobody recorded anything" and "there is something there and it cannot be read" are
    /// different sentences a window says differently. This project has had to separate them
    /// three times before this one.
    #[test]
    fn a_store_that_cannot_be_read_is_not_an_empty_store() {
        let scratch = Scratch::new();
        let store = scratch.store();
        assert_eq!(store.state(), Enrolment::Nothing);

        std::fs::write(scratch.dir.join(MANIFEST), b"{ this is not json").expect("written");

        match store.state() {
            Enrolment::Unreadable { reason } => {
                assert!(reason.contains(MANIFEST), "the sentence has to name the file: {reason}")
            }
            other => panic!("a damaged store must not read as an empty one: {other:?}"),
        }
        assert!(store.takes().is_err(), "and reading it must fail rather than answer nothing");
    }

    /// A take named in the manifest that is not on disk is a fault, not a shorter list: a store
    /// that quietly enrolled four instead of five would be a matcher trained on less than it
    /// was told.
    #[test]
    fn a_take_the_manifest_names_and_the_disk_does_not_have_is_a_fault() {
        let scratch = Scratch::new();
        let store = scratch.store();
        store.add(&take(spoken_phrase())).expect("stored");
        std::fs::remove_file(scratch.dir.join(take_file(1))).expect("removed");

        assert!(matches!(store.takes(), Err(Fault::Unreadable { .. })));
    }

    /// And one that is there at the wrong length, which is what a half-written file is.
    #[test]
    fn a_take_that_is_not_the_length_the_manifest_claims_is_a_fault() {
        let scratch = Scratch::new();
        let store = scratch.store();
        store.add(&take(spoken_phrase())).expect("stored");
        write_wav(&scratch.dir.join(take_file(1)), &silence(0.5)).expect("rewritten");

        match store.takes() {
            Err(Fault::Unreadable { detail, .. }) => {
                assert!(detail.contains("samples"), "{detail}")
            }
            other => panic!("expected a length complaint, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------------------------
    // Clearing
    // -----------------------------------------------------------------------------------------

    #[test]
    fn clearing_forgets_the_wake_word_and_leaves_everything_else_alone() {
        let scratch = Scratch::new();
        let store = scratch.store();
        store.add(&take(spoken_phrase())).expect("stored");
        let stranger = scratch.dir.join("somebody-elses-file.txt");
        std::fs::write(&stranger, b"not ours").expect("written");

        store.clear().expect("cleared");

        assert_eq!(store.state(), Enrolment::Nothing);
        assert!(!scratch.dir.join(take_file(1)).exists());
        assert!(
            stranger.exists(),
            "clearing names the files it wrote; a directory somebody pointed this at may hold \
             things nobody here put there"
        );
        // And clearing an already-empty store is not an error.
        store.clear().expect("clearing twice is not a failure");
    }

    // -----------------------------------------------------------------------------------------
    // The file format, read from the other side
    // -----------------------------------------------------------------------------------------

    /// 16-bit PCM is what every other tool on a machine produces, so a take somebody replaced
    /// by hand is read rather than reported as damage.
    #[test]
    fn a_sixteen_bit_recording_somebody_else_made_is_read() {
        let scratch = Scratch::new();
        let path = scratch.dir.join("theirs.wav");
        let data: Vec<u8> =
            [0i16, 16384, -16384].iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&((36 + data.len()) as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
        wav.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
        wav.extend_from_slice(&data);
        std::fs::write(&path, &wav).expect("written");

        assert_eq!(read_wav(&path).expect("read"), vec![0.0, 0.5, -0.5]);
    }

    /// A file at the wrong rate or with two channels is refused by name rather than resampled
    /// behind somebody's back: a matcher built on a mixture of rates would be worse for a
    /// reason nothing said out loud.
    #[test]
    fn a_recording_at_the_wrong_rate_is_refused_and_says_so() {
        let scratch = Scratch::new();
        let path = scratch.dir.join("wrong.wav");
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&36u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&WAVE_FORMAT_IEEE_FLOAT.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&48_000u32.to_le_bytes());
        wav.extend_from_slice(&(48_000u32 * 8).to_le_bytes());
        wav.extend_from_slice(&8u16.to_le_bytes());
        wav.extend_from_slice(&32u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&0u32.to_le_bytes());
        std::fs::write(&path, &wav).expect("written");

        match read_wav(&path) {
            Err(Fault::Unreadable { detail, .. }) => {
                assert!(detail.contains("48000") && detail.contains('2'), "{detail}")
            }
            other => panic!("expected a rate complaint, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------------------------
    // Where it lives
    // -----------------------------------------------------------------------------------------

    /// The data directory, not the cache, and never the system's temporary directory: a
    /// recording of somebody's voice is the one thing here that a download cannot replace.
    #[test]
    fn takes_live_where_a_clear_caches_cannot_reach_them() {
        let Some(dir) = default_dir() else {
            // A machine that names no data directory. `Store::on_this_machine` says so.
            assert!(matches!(Store::on_this_machine(), Err(Fault::NoDataDirectory)));
            return;
        };
        assert!(
            !dir.starts_with(std::env::temp_dir()),
            "{} is somewhere the system may empty",
            dir.display()
        );
        if let Some(cache) = crate::stt::cache_dir() {
            assert!(
                !dir.starts_with(&cache),
                "the model's cache is allowed to be deleted and this is not"
            );
        }
        assert!(dir.ends_with("wake-word"));
    }
}
