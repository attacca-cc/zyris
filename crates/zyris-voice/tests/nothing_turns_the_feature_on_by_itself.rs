//! The `voice` feature has to stay off unless somebody asks for it on the command line.
//!
//! Cargo unifies features across a build, so **one manifest anywhere in this workspace naming
//! `zyris-voice`'s `voice` feature puts the entire native audio stack into
//! `cargo test --workspace`** — 2m 06s of cold build at a 714 MB peak, on every checkout,
//! forever, for a feature almost no edit touches. A `[dev-dependencies]` entry is the way that
//! normally happens: a feature-gated test needs the dependency, somebody writes
//! `zyris-voice = { path = ".", features = ["voice"] }`, and nothing goes red — the suite just
//! quietly starts taking two minutes longer to compile and nobody connects the two.
//!
//! There is no compiler diagnostic for this and no cargo warning. So the manifests are read.
//!
//! # Why a manifest scan and not `cfg!(feature = "voice")`
//!
//! A test asserting `!cfg!(feature = "voice")` states the invariant directly, and would fail the
//! moment anybody ran `cargo test -p zyris-voice --features voice` on purpose — which is a
//! thing tasks 3 to 6 of step 7 do all day. This states the same invariant about the only place
//! it can be violated by accident.

use std::path::PathBuf;

/// The dependency whose feature is at stake, as a manifest spells it.
const CRATE: &str = "zyris-voice";

#[test]
fn no_manifest_in_this_workspace_turns_the_audio_stack_on() {
    let manifests = manifests();

    // A wrong path here would scan nothing and pass, which is the one way a guard like this
    // fails silently. There are five sibling crates and the workspace root.
    assert!(
        manifests.len() >= 6,
        "only {} manifests were found; this test is looking in the wrong place",
        manifests.len()
    );

    let mut found_a_dependency_on_this_crate = false;

    for (path, text) in &manifests {
        for entry in dependencies_on(text, CRATE) {
            found_a_dependency_on_this_crate = true;
            assert!(
                !entry.contains("features"),
                "{} depends on `{CRATE}` and names a feature: {entry}\n\nNothing in this \
                 workspace may turn `voice` (or `aec`) on from a manifest. Cargo unifies \
                 features, so this puts cpal, whisper.cpp and the rest into every \
                 `cargo test --workspace` — which is the whole thing the flag exists to \
                 prevent. Pass `--features` on the command line instead, or forward it from a \
                 feature of your own the way `zyris-app` does.",
                path
            );
        }
    }

    // `zyris-app` does depend on this crate, so a scan that found no dependency at all is a
    // scan whose parser stopped working, not a workspace that got cleaner.
    assert!(
        found_a_dependency_on_this_crate,
        "no manifest in this workspace depends on `{CRATE}` at all. Either the parser below \
         has stopped seeing dependency entries, or `zyris-app` no longer has a voice."
    );
}

/// Off by default, said in the one place that decides it.
#[test]
fn the_default_feature_set_is_empty() {
    let manifest = std::fs::read_to_string(crate_dir().join("Cargo.toml"))
        .expect("this crate's own manifest is readable");

    let default = uncommented(&manifest)
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("default"))
        .map(str::to_string)
        .expect("`default` is declared explicitly, so that it cannot drift by omission");

    assert_eq!(
        default.replace(' ', ""),
        "default=[]",
        "`voice` is off by default and `default` says so. With it on, whisper.cpp and \
         `webrtc-audio-processing` compile on every checkout of this workspace."
    );
}

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every `Cargo.toml` in the workspace: the root, and one per member.
fn manifests() -> Vec<(String, String)> {
    let crates = crate_dir().parent().expect("crates/ is this crate's parent").to_path_buf();
    let workspace = crates.parent().expect("the workspace root is above crates/").to_path_buf();

    let mut paths = vec![workspace.join("Cargo.toml")];
    let entries = std::fs::read_dir(&crates).expect("crates/ is readable");
    for entry in entries {
        let manifest = entry.expect("a directory entry is readable").path().join("Cargo.toml");
        if manifest.is_file() {
            paths.push(manifest);
        }
    }
    paths.sort();

    paths
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("{} is readable: {error}", path.display()));
            (path.display().to_string(), text)
        })
        .collect()
}

/// Every declaration of a dependency on `name`, as one string each.
///
/// Both spellings TOML allows are found: the inline `name = { .. }` inside a dependency table,
/// and the `[dependencies.name]` table with its body. `[features]` is deliberately *not* a
/// dependency table — `zyris-app`'s `voice = ["zyris-voice/voice"]` is the forwarding this
/// whole design is built on, and flagging it would make the test unpassable.
fn dependencies_on(manifest: &str, name: &str) -> Vec<String> {
    let manifest = uncommented(manifest);
    let mut found = Vec::new();
    let mut table = String::new();
    let mut collecting: Option<String> = None;

    for line in manifest.lines() {
        let line = line.trim();

        if line.starts_with('[') {
            if let Some(entry) = collecting.take() {
                found.push(entry);
            }
            table = line.trim_matches(['[', ']']).to_string();
            // `[target.'cfg(..)'.dev-dependencies.zyris-voice]` as well as
            // `[dependencies.zyris-voice]`.
            if is_dependency_table(&table)
                && table.rsplit('.').next().is_some_and(|last| last == name)
            {
                collecting = Some(line.to_string());
            }
            continue;
        }

        if let Some(entry) = collecting.as_mut() {
            entry.push('\n');
            entry.push_str(line);
            continue;
        }

        if is_dependency_table(&table)
            && line.split('=').next().is_some_and(|key| key.trim().trim_matches('"') == name)
        {
            found.push(line.to_string());
        }
    }

    if let Some(entry) = collecting {
        found.push(entry);
    }

    found
}

/// Whether a table header names somewhere dependencies are declared.
///
/// Its last segment is what decides, so the `[target.'cfg(..)'.dev-dependencies]` form is
/// covered without listing every shape a target predicate can take.
fn is_dependency_table(table: &str) -> bool {
    let mut segments: Vec<&str> = table.split('.').collect();
    // `[dependencies.zyris-voice]` — the table's own name is the dependency, so the segment
    // before it is the one that says which kind of table this is.
    if segments.len() > 1 {
        let last = segments[segments.len() - 1];
        if !last.ends_with("dependencies") {
            segments.pop();
        }
    }
    segments
        .last()
        .is_some_and(|last| matches!(*last, "dependencies" | "dev-dependencies" | "build-dependencies"))
}

/// TOML with its `#` comments removed. A `#` inside a quoted value is not one.
fn uncommented(manifest: &str) -> String {
    manifest
        .lines()
        .map(|line| {
            let mut quoted = false;
            for (at, character) in line.char_indices() {
                match character {
                    '"' => quoted = !quoted,
                    '#' if !quoted => return &line[..at],
                    _ => {}
                }
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The parser above is what this test rests on, so it is exercised on shapes rather than only
/// on the manifests that happen to be here today.
mod parsing {
    use super::*;

    fn offends(manifest: &str) -> bool {
        dependencies_on(manifest, CRATE).iter().any(|entry| entry.contains("features"))
    }

    #[test]
    fn a_dev_dependency_naming_the_feature_is_caught() {
        assert!(offends(
            "[dev-dependencies]\nzyris-voice = { path = \".\", features = [\"voice\"] }\n"
        ));
    }

    #[test]
    fn the_table_spelling_of_the_same_thing_is_caught() {
        assert!(offends(
            "[dev-dependencies.zyris-voice]\npath = \".\"\nfeatures = [\"aec\"]\n"
        ));
    }

    #[test]
    fn a_target_specific_dependency_is_caught() {
        assert!(offends(
            "[target.'cfg(target_os = \"linux\")'.dependencies]\n\
             zyris-voice = { path = \"../zyris-voice\", features = [\"voice\"] }\n"
        ));
    }

    /// The forwarding the whole design depends on. Flagging it would make this unpassable.
    #[test]
    fn forwarding_the_feature_from_a_feature_is_not_an_offence() {
        assert!(!offends(
            "[dependencies]\nzyris-voice = { path = \"../zyris-voice\" }\n\n\
             [features]\nvoice = [\"zyris-voice/voice\"]\n"
        ));
    }

    /// A comment showing what not to write is not writing it.
    #[test]
    fn a_commented_out_dependency_is_not_an_offence() {
        assert!(!offends(
            "[dev-dependencies]\n# zyris-voice = { path = \".\", features = [\"voice\"] }\n"
        ));
    }

    /// **The case `uncommented` actually exists for**, and the one the commented-out case above
    /// does not reach: there, the `#` also makes the key stop being `zyris-voice`, so the entry
    /// is skipped for the wrong reason and the stripper could be deleted without anything
    /// noticing. Here the declaration is innocent and only the words after the `#` are not — a
    /// scan that read them would fail this manifest for a note somebody left themselves.
    #[test]
    fn a_comment_after_an_innocent_dependency_is_not_an_offence() {
        assert!(!offends(
            "[dependencies]\n\
             zyris-voice = { path = \"../zyris-voice\" }  # never `features = [\"voice\"]` here\n"
        ));
    }

    /// Another crate's features are its own business.
    #[test]
    fn a_feature_on_some_other_dependency_is_not_an_offence() {
        assert!(!offends("[dependencies]\ntokio = { workspace = true, features = [\"sync\"] }\n"));
    }

    #[test]
    fn a_plain_dependency_on_this_crate_is_seen_at_all() {
        assert_eq!(
            dependencies_on("[dependencies]\nzyris-voice = { path = \"../zyris-voice\" }\n", CRATE)
                .len(),
            1
        );
    }
}
