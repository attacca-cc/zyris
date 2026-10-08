//! Whether this processor can run the speech engine this binary carries.
//!
//! # The failure this module exists to stop
//!
//! `.cargo/config.toml` compiles whisper.cpp's ggml **for the AVX2 set on every build** — a local
//! `cargo build`, CI and the release bundles alike. ggml is therefore told the processor has
//! AVX2, FMA, F16C and BMI2 rather than asking it, and the instructions that assume them carry
//! no runtime check. On a processor that does not have them, the first such instruction executed
//! is `SIGILL`: the process dies with no message and nothing to read.
//!
//! **The application makes one such call before it makes anything else.** `zyris_voice::start`
//! builds `run::Engine`, `Engine::new` calls `stt::warm_the_gpu` on its first line, and that
//! calls `stt::devices`, which asks ggml for its device registry —
//! `ggml_backend_dev_count()`, compiled C. So a machine without AVX2 got a process that died
//! before its first window, which is exactly what the README used to say and what
//! [`NEEDS_AVX2`] now replaces.
//!
//! # What was measured, rather than assumed
//!
//! 2026-10-08, this workspace's own Debian 12 VM, with `qemu-user-static` 7.2 — which emulates
//! `CPUID`, so `-cpu SandyBridge` is a processor with AVX and without AVX2, FMA, F16C or BMI2,
//! and an AVX2 instruction really does raise `SIGILL` there (checked first with a two-line
//! `_mm256_add_epi32` probe, and with `is_x86_feature_detected!`, which agrees with the model):
//!
//! | binary | `-cpu max` | `-cpu SandyBridge` |
//! |---|---|---|
//! | `main` that returns, linked against an AVX2 `whisper-rs-sys` | runs | **runs** |
//! | … and then calls `ggml_backend_dev_count()` | runs, prints `1` device | **`SIGILL`, exit 132** |
//! | `main` that returns, linked against `ort` (`download-binaries`) | runs | **runs** |
//! | … and then creates an ONNX Runtime environment (`ort::init().commit()`) | `Ok(true)` | `Ok(true)` |
//!
//! Two things follow, and the whole shape of the fix is downstream of them:
//!
//! 1. **This is not a load-time failure.** A binary that statically links the AVX2 code reaches
//!    `main` on the older processor: the CRT runs, the constructors run, the linker's work is
//!    done. What dies is the *call*. So the process can be kept alive by not making it, without
//!    touching the linker, the manifests or the build flags.
//! 2. **The first of those calls is ours and it is on the app's startup path**, not inside
//!    transcription and not inside synthesis. Gating the *use* of speech is therefore enough for
//!    a window, a tray and every tool to come up on a machine that cannot speak.
//!
//! ONNX Runtime is the entry that is **not** shown to need AVX2 by anything measured here: its
//! own kernel dispatch picks what the processor reports, and creating an environment on the
//! SandyBridge model answers `Ok(true)`. It is refused on such a machine anyway, with the rest of
//! the audio stack, because the two halves of speech are gated together and because a graph run
//! is deeper into that library than this machine can reach without a model. See [`NEEDS_AVX2`].
//!
//! # Where the gate is, and why not lower down
//!
//! At [`crate::start`], which is the one function `zyris-app` calls: a machine without AVX2 gets
//! [`crate::Voice::disabled`] carrying [`NEEDS_AVX2`], the same answer a build without the
//! `voice` feature gets, and the Voice screen renders the sentence through
//! `view::VoiceView::unavailable` — which fills in *every* field of the screen with it, so
//! nothing reads as an empty list that is about to fill.
//!
//! The alternative — checking in `stt::devices` and in `Stt::load` and in `tts` — would leave
//! the engine built and running with a microphone, a wake word and a synthesis queue that can
//! never produce anything. One gate at the door says the same sentence once, and `stt::devices`
//! carries a second check only so that the ggml call has no path to it at all.
//!
//! **`--headless` is not a separate case**: it takes the same `start`, and a node with no window
//! is a node that cannot speak here for the same reason.

/// Whether this processor carries the instruction set whisper.cpp is compiled for.
///
/// The five flags are exactly the five `.cargo/config.toml` turns on, and they are asked for
/// **together** rather than as "AVX2" alone: AVX2 implies AVX but not FMA, F16C or BMI2, and a
/// processor that has the first without the others would pass a one-flag check and then fault on
/// the first `vfmadd` ggml runs. A build with only `GGML_AVX2` on would want a narrower question;
/// the workspace's own build wants all five, so this asks all five.
///
/// Cheap enough to call on any path — the answer is cached by `std`, which reads `CPUID` once.
pub fn has_avx2() -> bool {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        std::arch::is_x86_feature_detected!("avx")
            && std::arch::is_x86_feature_detected!("avx2")
            && std::arch::is_x86_feature_detected!("fma")
            && std::arch::is_x86_feature_detected!("f16c")
            && std::arch::is_x86_feature_detected!("bmi2")
    }
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    {
        // Apple Silicon, arm64 Android, and anything else this ships on: no build here asks
        // whisper for an x86 instruction set, so there is nothing to refuse. Saying `true` rather
        // than "not applicable" is what keeps the gate one `if` at the call site.
        true
    }
}

/// What a person is told when their processor is the reason speech is off.
///
/// A sentence for somebody reading the Voice screen, in [`crate::NOT_COMPILED_IN`]'s voice and
/// with the same job: say which capability is missing, why, and that the rest of the program is
/// not affected. It names the processors rather than the instruction set alone, because the
/// question the screen raises is "is my computer too old for this" and "AVX2" is not an answer
/// to it.
pub const NEEDS_AVX2: &str =
    "this computer's processor does not have AVX2 (Intel Haswell / AMD Excavator, 2013 and \
     later), which the speech engine in this build is compiled for — so Zyris cannot listen or \
     speak here. Everything else works";

/// [`NEEDS_AVX2`], only when this machine really is missing it.
///
/// `None` on every machine that can run the audio stack, which is what makes the call site read
/// as one question rather than as a capability test followed by a decision.
pub fn speech_unavailable() -> Option<&'static str> {
    (!has_avx2()).then_some(NEEDS_AVX2)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sentence has to be one a person can act on: it names the thing that is missing, the
    /// processors that have it, and that nothing else about the program changed.
    #[test]
    fn the_reason_names_the_instruction_set_the_processors_and_the_rest_of_the_program() {
        assert!(NEEDS_AVX2.contains("AVX2"), "{NEEDS_AVX2}");
        assert!(NEEDS_AVX2.contains("Haswell"), "{NEEDS_AVX2}");
        assert!(NEEDS_AVX2.contains("Everything else works"), "{NEEDS_AVX2}");
    }

    /// **The two sentences a machine can be given are different sentences.**
    ///
    /// A build with no audio stack compiled in and a machine that cannot run it are two
    /// different facts about two different things — the binary and the computer — and a screen
    /// that renders one of them for both sends somebody to the wrong place.
    #[test]
    fn a_machine_without_avx2_is_told_something_other_than_a_build_without_the_audio_stack() {
        assert_ne!(NEEDS_AVX2, crate::NOT_COMPILED_IN);
    }

    /// On a machine that has it, the gate is silent — this is the regression check for the
    /// machines the release is built and tested on.
    ///
    /// It asserts the pairing rather than the flag, so it passes on an arm64 runner too, where
    /// there is no AVX2 to have and nothing is refused either.
    #[test]
    fn a_machine_that_has_avx2_is_never_given_the_reason() {
        if has_avx2() {
            assert_eq!(speech_unavailable(), None);
        } else {
            assert_eq!(speech_unavailable(), Some(NEEDS_AVX2));
        }
    }
}
