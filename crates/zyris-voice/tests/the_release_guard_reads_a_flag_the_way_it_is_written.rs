//! `build.rs`'s own tests. A build script is not a test target — `cargo test` compiles it and
//! never runs a `#[test]` inside it — so the part of it worth arguing about lives in
//! `build/flags.rs` and is reached from here by path. The tests are in that file, beside the
//! code they decide; this target exists to make `cargo test` run them.
//!
//! Not behind the `voice` feature: the flag it is about is read on every build that could
//! produce a release binary, and the parsing has no audio in it.

#[path = "../build/flags.rs"]
mod flags;
