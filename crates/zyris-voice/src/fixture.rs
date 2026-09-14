//! The recordings under `tests/audio`, read without a dependency.
//!
//! **One reader, not three.** `vad`, `session` and `apm` all need `jfk.wav`, and each of them
//! had its own copy of the same thirty lines until task 4 of step 8 wanted a third. Two copies
//! of a parser that has to agree, with nothing that goes red when they stop, is the shape this
//! workspace keeps finding in its own review notes.
//!
//! A short RIFF reader rather than `hound`, for the reason `vad`'s tests already gave: a
//! `[dev-dependencies]` entry on a crate whose whole feature layout exists to keep the
//! dependency graph small is a cost out of proportion to twenty lines.

use crate::capture::SAMPLE_RATE;

/// One 16 kHz mono 16-bit recording from `crates/zyris-voice/tests/audio`, as `f32` in `[-1, 1]`.
///
/// Panics rather than returning an error: a missing or mis-formatted fixture is a broken
/// checkout, not a condition any caller can do anything about.
pub fn wav(name: &str) -> Vec<f32> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audio").join(name);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");

    let (mut rate, mut channels, mut bits) = (0u32, 0u16, 0u16);
    let mut samples = Vec::new();
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().expect("4 bytes")) as usize;
        let body = &bytes[at + 8..(at + 8 + size).min(bytes.len())];
        match id {
            b"fmt " => {
                channels = u16::from_le_bytes(body[2..4].try_into().expect("2 bytes"));
                rate = u32::from_le_bytes(body[4..8].try_into().expect("4 bytes"));
                bits = u16::from_le_bytes(body[14..16].try_into().expect("2 bytes"));
            }
            b"data" => {
                samples = body
                    .chunks_exact(2)
                    .map(|s| f32::from(i16::from_le_bytes(s.try_into().expect("2 bytes"))) / 32768.0)
                    .collect();
            }
            _ => {}
        }
        at += 8 + size + (size & 1);
    }

    // The whole crate is written for one format. A fixture in another would be read as nonsense
    // rather than refused, which is the failure this assertion exists to prevent.
    assert_eq!(
        (rate, channels, bits),
        (SAMPLE_RATE, 1, 16),
        "{} must be 16 kHz mono 16-bit",
        path.display()
    );
    assert!(!samples.is_empty(), "{} has no audio in it", path.display());
    samples
}
