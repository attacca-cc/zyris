//! The one thing in this crate a test cannot check.
//!
//! `GGML_NATIVE` defaults to **ON** in whisper.cpp's CMake, which bakes `-march=native` into
//! the binary. A release built on a machine with AVX-512 then executes an illegal instruction
//! on a user's laptop — and **no test on the build machine can catch that**, because the build
//! machine is the one CPU where it works. The symptom the user gets is `SIGILL` with no
//! message, before `main`.
//!
//! `whisper-rs-sys/build.rs` forwards any `WHISPER_*`, `GGML_*` or `CMAKE_*` environment
//! variable to CMake, so `GGML_NATIVE=OFF` is the whole fix. What was missing was anything
//! that *notices its absence*. `ci.yml` sets it on the voice step and `release.yml` sets it
//! for the bundle job, but a workflow file is a place the rule can be forgotten a second time;
//! a build script travels with the crate and runs on every path that can produce a binary,
//! including `pnpm tauri build`, which never runs a test.
//!
//! **`GGML_NATIVE=OFF` is not enough on its own**, and for a while this file said it was. ggml
//! turns every instruction set off with it unless each one is named, so the guard also wants the
//! AVX2 set that `.cargo/config.toml` provides.
//!
//! Only `--release` is guarded. A debug build is nobody's download, and failing it would make
//! `cargo test -p zyris-voice --features voice` need an environment variable to run at all.
//!
//! `ZYRIS_ALLOW_NATIVE_WHISPER=1` is the deliberate way past it, for somebody building a
//! release for the machine it will run on and nowhere else. **A false value is not a way past
//! it**: see `build/flags.rs`, which is where both spellings live and where the tests for them
//! are, since `cargo test` never runs a `#[test]` inside a build script.

#[path = "build/flags.rs"]
mod flags;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=build/flags.rs");
    println!("cargo::rerun-if-env-changed=GGML_NATIVE");
    for flag in flags::REQUIRED_SIMD {
        println!("cargo::rerun-if-env-changed={flag}");
    }
    println!("cargo::rerun-if-env-changed=ZYRIS_ALLOW_NATIVE_WHISPER");

    // No whisper in this build, so nothing to bake anything into.
    if std::env::var_os("CARGO_FEATURE_VOICE").is_none() {
        return;
    }
    if std::env::var("PROFILE").as_deref() != Ok("release") {
        return;
    }
    if flags::allows_native(std::env::var("ZYRIS_ALLOW_NATIVE_WHISPER").ok().as_deref()) {
        println!(
            "cargo::warning=building whisper.cpp with -march=native; this binary may SIGILL on \
             any other CPU"
        );
        return;
    }

    // CMake's own spelling of false, which is what whisper-rs-sys forwards this value as.
    if !flags::cmake_off(std::env::var("GGML_NATIVE").ok().as_deref()) {
        panic!(
            "refusing to build a release with the audio stack while GGML_NATIVE is not OFF.\n\
             \n\
             whisper.cpp defaults it ON, which compiles -march=native into the binary: it will \
             run here and die with SIGILL on any CPU older than this one, before main, with no \
             message. No test can catch that, because this machine is the CPU it works on.\n\
             \n\
             Set GGML_NATIVE=OFF, or set ZYRIS_ALLOW_NATIVE_WHISPER=1 if this build is only \
             ever going to run on this machine. `.cargo/config.toml` sets it for every build \
             run from this workspace."
        );
    }

    // With GGML_NATIVE OFF, ggml turns every instruction set off unless it is named, and whisper
    // runs a scalar encoder about ten times slower. See `.cargo/config.toml`.
    let missing: Vec<&str> = flags::REQUIRED_SIMD
        .into_iter()
        .filter(|flag| !flags::cmake_on(std::env::var(flag).ok().as_deref()))
        .collect();
    if !missing.is_empty() {
        panic!(
            "refusing to build a release with the audio stack while {} not ON.\n\
             \n\
             With GGML_NATIVE=OFF, whisper.cpp compiles no instruction set it is not told to, and \
             transcription runs about ten times slower: 2.06 s instead of 0.22 s for a 3.5 s \
             clip. `.cargo/config.toml` sets all of them for builds run from this workspace; a \
             build started elsewhere has to set them itself. After changing them, run \
             `cargo clean --release -p whisper-rs-sys`: it does not rebuild on its own.",
            missing.join(", ") + if missing.len() == 1 { " is" } else { " are" }
        );
    }
}
