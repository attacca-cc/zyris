//! How the two environment variables the release guard reads are spelled.
//!
//! In a file of its own, and included by `build.rs` with `#[path]`, because **a build script is
//! not a test target**: `cargo test` never runs one, so a rule that lives only in `build.rs`
//! has nothing that can go red. `tests/the_release_guard_reads_a_flag_the_way_it_is_written.rs`
//! includes this same file the same way.

/// Whether a value means "yes, build with `-march=native`, I know what that costs".
///
/// **`ZYRIS_ALLOW_NATIVE_WHISPER=0` used to turn the guard off**, because the check was
/// `is_some_and(|v| !v.is_empty())` and every non-empty value — `0`, `no`, `false` — read as
/// consent. Somebody setting `0` means the opposite of what they got, and what they got was a
/// release that runs on the build machine and dies with `SIGILL` on anybody else's, before
/// `main`, with no message.
///
/// So the same spelling of falsehood is accepted here as in [`cmake_off`]: a variable that is
/// set to a false value is not set.
pub fn allows_native(value: Option<&str>) -> bool {
    match value {
        Some(value) => !cmake_off(Some(value)),
        None => false,
    }
}

/// Whether a value is CMake's own spelling of false, which is how `whisper-rs-sys` forwards
/// `GGML_NATIVE` to it.
pub fn cmake_off(value: Option<&str>) -> bool {
    match value {
        Some(value) => matches!(
            value.trim().to_ascii_uppercase().as_str(),
            "OFF" | "0" | "FALSE" | "N" | "NO" | "IGNORE" | "NOTFOUND" | ""
        ),
        None => false,
    }
}

/// Whether a value is CMake's spelling of true. Unset is not: whisper.cpp's default for every
/// instruction-set option is OFF once `GGML_NATIVE` is, so an absent `GGML_AVX2` builds without it.
pub fn cmake_on(value: Option<&str>) -> bool {
    match value {
        Some(value) => matches!(
            value.trim().to_ascii_uppercase().as_str(),
            "ON" | "1" | "TRUE" | "Y" | "YES"
        ),
        None => false,
    }
}

/// The instruction sets a release must name, since `GGML_NATIVE=OFF` turns every one of them off.
/// Haswell's, which is the floor the README gives for speech.
pub const REQUIRED_SIMD: [&str; 5] = ["GGML_AVX", "GGML_AVX2", "GGML_FMA", "GGML_F16C", "GGML_BMI2"];

#[cfg(test)]
mod tests {
    use super::*;

    /// **The defect this file exists for.** Setting the escape hatch to a false value must not
    /// open it.
    #[test]
    fn a_false_value_does_not_allow_a_native_build() {
        for value in ["0", "no", "NO", "false", "False", "off", "OFF", "n", " 0 ", ""] {
            assert!(
                !allows_native(Some(value)),
                "{value:?} read as consent to bake -march=native into a release"
            );
        }
        assert!(!allows_native(None), "unset is not consent either");
    }

    /// And the hatch still opens for somebody who meant it.
    #[test]
    fn a_true_value_still_allows_it() {
        for value in ["1", "yes", "true", "TRUE", "y", " 1 "] {
            assert!(allows_native(Some(value)), "{value:?} is how the hatch is asked for");
        }
    }

    /// The two variables read the same spelling of false, which is the whole fix: one set of
    /// words means no, wherever it is written.
    #[test]
    fn the_two_flags_agree_about_what_false_looks_like() {
        for value in ["0", "no", "NO", "false", "off", "n", ""] {
            assert!(cmake_off(Some(value)), "{value:?} is CMake's false");
            assert!(!allows_native(Some(value)), "{value:?} must be false here too");
        }
        assert!(!cmake_off(Some("ON")), "and true is still true");
        assert!(!cmake_off(None), "an unset GGML_NATIVE is whisper.cpp's default, which is ON");
    }

    /// An instruction set is compiled in only when it is asked for, so unset must read as absent.
    #[test]
    fn only_a_true_value_asks_for_an_instruction_set() {
        for value in ["ON", "on", "1", "true", "YES", " y "] {
            assert!(cmake_on(Some(value)), "{value:?} is CMake's true");
        }
        for value in ["OFF", "0", "no", ""] {
            assert!(!cmake_on(Some(value)), "{value:?} is not");
        }
        assert!(!cmake_on(None), "unset builds without it once GGML_NATIVE is OFF");
    }

    /// The floor the README gives for speech is Haswell, and AVX2 is the one whose absence costs
    /// the tenfold slowdown.
    #[test]
    fn a_release_needs_avx2_named() {
        assert!(REQUIRED_SIMD.contains(&"GGML_AVX2"), "{REQUIRED_SIMD:?}");
    }
}
