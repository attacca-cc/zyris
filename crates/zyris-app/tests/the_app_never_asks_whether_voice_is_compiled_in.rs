//! **The one thing standing between the two builds diverging.**
//!
//! `voice` is off by default because a cold build of the audio stack is 2m 06s against a warm
//! 1.4 s, so nearly every build anybody makes of this program is the one that cannot listen. If
//! `zyris-app` were allowed a `#[cfg(feature = "voice")]`, the two builds would stop being the
//! same program: the on side would be compiled by nothing anybody runs, and it would rot for
//! however long it took somebody to turn the flag on. `zyris-voice` is written so that it
//! exports the same stream either way precisely so that this crate never has to ask.
//!
//! Nothing else in the toolchain notices. `cargo build` is perfectly happy to compile a
//! `#[cfg]` whose arm is never taken, and `cargo test --workspace` never turns the feature on,
//! so the on arm would not even be type-checked. Hence a test that reads this crate's own
//! source, in the same spirit as `gui.rs`'s `both_ways_out_of_this_process_hand_everything_back`
//! and `announce.rs`'s reading of the README.
//!
//! It lives under `tests/` rather than in `src/`, which is not incidental: a test that scans
//! `src/**` cannot be written inside `src/**` without matching itself.
//!
//! # What it catches, and what it would miss
//!
//! It strips comments first and then removes every space, so all of these are caught:
//! `#[cfg(feature = "voice")]`, `#[cfg(feature="voice")]`, `cfg!(feature = "voice")`,
//! `#[cfg(all(unix, feature = "voice"))]`, `#[cfg_attr(feature = "voice", ...)]`, and an
//! attribute wrapped across two lines. A comment or a doc comment that merely mentions the
//! string does **not** trip it — the comment stripper is tested below for exactly that.
//!
//! It would miss a build script writing a `cargo::rustc-cfg` of its own and code testing *that*
//! name instead, and it would miss a feature spelled some other way. Both are conscious
//! detours rather than the accident this guards.

use std::path::{Path, PathBuf};

/// Whitespace is removed before the search, so this spelling covers every spacing.
const ASKING: &str = "feature=\"voice\"";

/// Whether this source asks whether the audio stack is compiled in.
///
/// **One function, used by the scan and by every case below it.** It was two, briefly, and a
/// mutation removing the whitespace-insensitivity from the scan left the cases passing against
/// their own private copy — duplicated logic in a guard is a guard that can be half-disabled.
fn asks(source: &str) -> bool {
    let code = without_comments(source);
    let code: String = code.chars().filter(|character| !character.is_whitespace()).collect();
    code.contains(ASKING)
}

#[test]
fn the_app_never_asks_whether_voice_is_compiled_in() {
    let sources = rust_sources();

    // A path typo that scanned nothing would pass every assertion below in silence, which is
    // the one failure a guard like this cannot afford.
    assert!(
        sources.len() > 5,
        "only {} source files were found under {}; this test scans the wrong place and is \
         proving nothing",
        sources.len(),
        crate_dir().display()
    );

    // And a scan that reached only the top of `src/` would pass that count on this crate's
    // seven root modules alone while seeing nothing inside `hotkey/` — which is where a
    // platform `#[cfg]` is most likely to be written in the first place. `build.rs` is
    // deliberately excluded from this half: it sits beside `src/` rather than under it, and
    // counting it would make a non-recursive walk pass.
    let src = crate_dir().join("src");
    assert!(
        sources.iter().any(|path| {
            path.starts_with(&src) && path.parent().is_some_and(|parent| parent != src)
        }),
        "nothing below the top of {} was scanned; this crate keeps modules in subdirectories \
         and a scan that stops at the first level misses exactly where a platform `#[cfg]` \
         would be written",
        src.display()
    );

    for path in sources {
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{} is readable: {error}", path.display()));

        assert!(
            !asks(&source),
            "{} asks whether the audio stack is compiled in. `zyris-app` must not: it forwards \
             the `voice` feature to `zyris-voice` and takes the same code path either way, so \
             that the build with the feature on cannot quietly stop compiling. `zyris-voice` \
             exports `VoiceEvent` and an already-ended stream when the feature is off, which is \
             what makes that possible — call it unconditionally instead.",
            path.display()
        );
    }
}

/// And the application has to actually ask, or the forwarding is decoration.
///
/// Without this, deleting every mention of `zyris_voice` from `zyris-app` leaves both of the
/// other tests green: the manifest still forwards, the sources still contain no `#[cfg]`, and
/// the dependency becomes one nothing calls. It is a weak assertion on purpose — it says the
/// crate is reached, not how — because how is tasks 6 and 7's to decide.
#[test]
fn the_app_actually_asks_zyris_voice_something() {
    let asked = rust_sources().into_iter().any(|path| {
        let source = std::fs::read_to_string(&path).unwrap_or_default();
        without_comments(&source).contains("zyris_voice::")
    });

    assert!(
        asked,
        "nothing in zyris-app calls into `zyris_voice`. The `voice` feature would still forward \
         and CI would still compile the audio stack, but the application would have stopped \
         consuming it and no other test here would say so."
    );
}

/// The other half of the same rule: the feature has to *exist* here and forward, or the test
/// above passes for a crate that simply has no voice in it and nobody notices for a release.
#[test]
fn the_feature_forwards_and_does_nothing_else() {
    let manifest = std::fs::read_to_string(crate_dir().join("Cargo.toml"))
        .expect("zyris-app's own manifest is readable");
    let declaration = manifest
        .lines()
        .map(|line| line.split('#').next().unwrap_or_default().trim())
        .find(|line| line.starts_with("voice"))
        .unwrap_or_else(|| {
            panic!(
                "zyris-app has no `voice` feature any more. Without one there is nothing to \
                 build with, and `.github/workflows/ci.yml`'s voice step compiles nothing."
            )
        });

    assert_eq!(
        declaration.replace(' ', ""),
        "voice=[\"zyris-voice/voice\"]",
        "zyris-app's `voice` feature must forward to `zyris-voice/voice` and do nothing else. \
         Anything extra in it is a second thing the two builds can differ by."
    );
}

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every `.rs` file this crate compiles, plus its build script.
///
/// `tests/` is deliberately not included: this file lives there and names the pattern it is
/// looking for.
fn rust_sources() -> Vec<PathBuf> {
    let mut found = Vec::new();
    collect(&crate_dir().join("src"), &mut found);
    let build_script = crate_dir().join("build.rs");
    if build_script.is_file() {
        found.push(build_script);
    }
    found.sort();
    found
}

fn collect(directory: &Path, into: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("{} is readable: {error}", directory.display()));
    for entry in entries {
        let path = entry.expect("a directory entry is readable").path();
        if path.is_dir() {
            collect(&path, into);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            into.push(path);
        }
    }
}

/// Rust source with its comments removed and everything else left alone.
///
/// Written out rather than done with a search-and-replace because the three things that make a
/// naive version wrong are all present in this workspace: a `//` inside a string literal (every
/// URL), a `"` inside a character literal, and a `'` that is a lifetime rather than a quote.
/// Block comments nest in Rust, so the depth is counted.
fn without_comments(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut at = 0;

    while at < chars.len() {
        match chars[at] {
            '/' if chars.get(at + 1) == Some(&'/') => {
                // A line comment, doc comment included. The newline is kept, so line numbers
                // and token boundaries survive.
                while at < chars.len() && chars[at] != '\n' {
                    at += 1;
                }
            }
            '/' if chars.get(at + 1) == Some(&'*') => {
                let mut depth = 1usize;
                at += 2;
                while at < chars.len() && depth > 0 {
                    if chars[at] == '/' && chars.get(at + 1) == Some(&'*') {
                        depth += 1;
                        at += 2;
                    } else if chars[at] == '*' && chars.get(at + 1) == Some(&'/') {
                        depth -= 1;
                        at += 2;
                    } else {
                        at += 1;
                    }
                }
                // A space, so that the tokens either side of a removed comment do not fuse.
                out.push(' ');
            }
            'r' if raw_string_hashes(&chars, at).is_some() => {
                let hashes = raw_string_hashes(&chars, at).expect("just checked");
                let opening: String = std::iter::once('r')
                    .chain(std::iter::repeat_n('#', hashes))
                    .chain(std::iter::once('"'))
                    .collect();
                let closing: String =
                    std::iter::once('"').chain(std::iter::repeat_n('#', hashes)).collect();
                out.push_str(&opening);
                at += opening.chars().count();
                let rest: String = chars[at..].iter().collect();
                let body_length = match rest.find(&closing) {
                    Some(offset) => rest[..offset].chars().count(),
                    None => chars.len() - at,
                };
                out.extend(&chars[at..at + body_length]);
                at += body_length;
                if at < chars.len() {
                    out.push_str(&closing);
                    at += closing.chars().count();
                }
            }
            '"' => {
                out.push('"');
                at += 1;
                while at < chars.len() && chars[at] != '"' {
                    if chars[at] == '\\' && at + 1 < chars.len() {
                        out.push(chars[at]);
                        at += 1;
                    }
                    out.push(chars[at]);
                    at += 1;
                }
                if at < chars.len() {
                    out.push('"');
                    at += 1;
                }
            }
            '\'' => {
                // `'x'` and `'\n'` are character literals and can contain a quote or a slash;
                // `'a` in `&'a str` is a lifetime and must not swallow the code after it.
                let escaped = chars.get(at + 1) == Some(&'\\');
                let plain = chars.get(at + 2) == Some(&'\'');
                if escaped {
                    out.push('\'');
                    at += 1;
                    while at < chars.len() && chars[at] != '\'' {
                        out.push(chars[at]);
                        at += 1;
                    }
                } else if plain {
                    out.extend(&chars[at..at + 2]);
                    at += 2;
                } else {
                    out.push('\'');
                    at += 1;
                }
            }
            other => {
                out.push(other);
                at += 1;
            }
        }
    }

    out
}

/// How many `#` a raw string starting at `at` uses, or `None` if this `r` is not one.
///
/// The `r` of an identifier such as `render` must not be mistaken for one, so the character
/// before it has to be something that cannot end an identifier.
fn raw_string_hashes(chars: &[char], at: usize) -> Option<usize> {
    let preceded_by_identifier = at > 0 && (chars[at - 1].is_alphanumeric() || chars[at - 1] == '_');
    if preceded_by_identifier {
        return None;
    }
    let mut hashes = 0;
    while chars.get(at + 1 + hashes) == Some(&'#') {
        hashes += 1;
    }
    (chars.get(at + 1 + hashes) == Some(&'"')).then_some(hashes)
}

/// The comment stripper is the part of this file that can be wrong without anything saying so,
/// so it gets its own cases — one per thing in this workspace that breaks a naive version.
mod stripper {
    use super::asks;
    use super::without_comments;

    /// **The requirement that makes this test worth writing rather than a `grep`.** Every one of
    /// these sentences exists somewhere in this workspace, and none of them is a code path.
    #[test]
    fn a_comment_mentioning_the_string_is_not_asking() {
        assert!(!asks("// there is no #[cfg(feature = \"voice\")] in this crate\n"));
        assert!(!asks("/// Forwards to `zyris-voice/voice`; never `feature = \"voice\"` here.\n"));
        assert!(!asks("//! nothing here reads feature=\"voice\"\n"));
        assert!(!asks("/* #[cfg(feature = \"voice\")] */\nfn main() {}\n"));
        assert!(!asks("/* outer /* #[cfg(feature = \"voice\")] */ still a comment */\n"));
    }

    #[test]
    fn every_spelling_of_the_question_is_caught() {
        assert!(asks("#[cfg(feature = \"voice\")]\nfn f() {}\n"));
        assert!(asks("#[cfg(feature=\"voice\")]\nfn f() {}\n"));
        assert!(asks("if cfg!(feature = \"voice\") { f() }\n"));
        assert!(asks("#[cfg(all(target_os = \"linux\", feature = \"voice\"))]\nfn f() {}\n"));
        assert!(asks("#[cfg_attr(feature = \"voice\", allow(dead_code))]\nfn f() {}\n"));
        // Wrapped across lines by a formatter, which whitespace removal is what handles.
        assert!(asks("#[cfg(all(\n    unix,\n    feature =\n        \"voice\",\n))]\nfn f() {}\n"));
    }

    /// A `//` inside a string is not a comment, and treating it as one would blind the scan to
    /// everything after it in the file.
    #[test]
    fn a_slash_inside_a_string_does_not_start_a_comment() {
        let source = "let url = \"https://attacca.cc\";\n#[cfg(feature = \"voice\")]\nfn f() {}\n";

        assert!(asks(source));
    }

    /// Both of these appear in this workspace and both break a naive quote counter.
    #[test]
    fn a_quote_in_a_character_literal_and_a_lifetime_do_not_swallow_the_file() {
        assert!(asks("let quote = '\"';\n#[cfg(feature = \"voice\")]\nfn f() {}\n"));
        assert!(asks("fn f<'a>(s: &'a str) -> &'a str { s }\n#[cfg(feature = \"voice\")]\n"));
        assert!(asks("let slash = '/';\n#[cfg(feature = \"voice\")]\nfn f() {}\n"));
        assert!(asks("let escaped = '\\'';\n#[cfg(feature = \"voice\")]\nfn f() {}\n"));
    }

    #[test]
    fn a_raw_string_is_left_alone_and_does_not_hide_what_follows() {
        let source =
            "let json = r#\"{\"a\": \"//\"}\"#;\n#[cfg(feature = \"voice\")]\nfn f() {}\n";

        assert!(asks(source));
        assert!(without_comments(source).contains("r#\"{\"a\": \"//\"}\"#"));
    }

    /// The `r` of an ordinary identifier is not the start of a raw string.
    #[test]
    fn an_identifier_beginning_with_r_is_not_a_raw_string() {
        assert!(asks("let render = 1;\n#[cfg(feature = \"voice\")]\nfn f() {}\n"));
    }
}
