//! The keys of the voice settings file: what it holds, what each one is called in it, and what a
//! value of each kind looks like.
//!
//! **One description of the file, in the crate that owns the file.** `zyris config` edits
//! `voice.json` with no window in front of it, and the alternative to this list is a second copy
//! of `Settings`'s field names living in `zyris-app` — a copy that nothing checks, so the day a
//! field is renamed the console goes on writing a key the voice reads past in silence, and the
//! person who typed the command is told it worked. Here the names sit beside the struct they
//! describe, and the test at the bottom of this file — behind the `voice` feature, because that
//! is what `Settings` is behind — fails the moment the two disagree.
//!
//! **Not behind the feature, and that is the other half of it.** A build with no audio stack
//! still has the file, still has `zyris config`, and a `config list` that could not name a single
//! key there would be a command missing from most of the builds of this program.

use crate::view::Choice;

/// What the settings file is called, inside the instance's data directory.
///
/// The **data** directory and not the cache, and scoped by instance like everything else the app
/// derives from the instance name: a `--server` run choosing to listen must not turn the
/// microphone on for the production node.
pub const SETTINGS_FILE: &str = "voice.json";

/// What kind of value a key holds.
///
/// The four shapes the file actually contains, and no more: a switch, a line of text, a number,
/// and the one structure — a device, which is either the system's default or one named device.
/// A kind is here rather than inferred from the value so that a console command can tell a person
/// what it expected when what they typed is not it, and so that `true` can be written as a
/// boolean rather than as the string that spells one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bool,
    Text,
    Number,
    /// Which microphone, or which speaker: `Choice`, written in the file as a tagged object.
    Device,
}

/// One key of the settings file.
pub struct Key {
    /// What `zyris config` calls it: the file's name, prefixed with the file's own name. Spelled
    /// out rather than computed, because this is the string a person types.
    pub name: &'static str,
    /// What the file calls it. `Settings` is `rename_all = "camelCase"`, so this is not always the
    /// field's Rust name.
    pub json: &'static str,
    pub kind: Kind,
    /// Whether the file may leave it out entirely, which is what a `null` in the file means for
    /// these. `listen` is not optional — an absent `listen` is `false` and nothing else — while
    /// every `Option`? field is, and an absent `readAloud` is not "no answer" but "on".
    pub optional: bool,
    /// One line, for the console's table. Written for a person who has never opened the window.
    pub help: &'static str,
}

/// Every key `zyris config` can read or write.
///
/// **The whole file, rather than a subset somebody chose.** A console that could change some of
/// the settings and not the rest would leave a person to work out which half they were missing;
/// the ones that make no sense on a machine with no microphone are harmless to write there and
/// are read as what they say the next time a window opens.
pub const KEYS: &[Key] = &[
    Key {
        name: "voice.listen",
        json: "listen",
        kind: Kind::Bool,
        optional: false,
        help: "whether a microphone should be open. A window reads this at launch and opens one; \
               a run started --headless never listens",
    },
    Key {
        name: "voice.readAloud",
        json: "readAloud",
        kind: Kind::Bool,
        optional: true,
        help: "whether answers are read aloud while listening. Absent means on",
    },
    Key {
        name: "voice.device",
        json: "device",
        kind: Kind::Device,
        optional: false,
        help: "which microphone to open: `default` follows the system's default, anything else \
               is a device id",
    },
    Key {
        name: "voice.speaker",
        json: "speaker",
        kind: Kind::Device,
        optional: false,
        help: "which speaker answers are read through, in the same form as voice.device",
    },
    Key {
        name: "voice.session",
        json: "session",
        kind: Kind::Text,
        optional: true,
        help: "the Attacca session this machine talks to and listens to",
    },
    Key {
        name: "voice.agent",
        json: "agent",
        kind: Kind::Text,
        optional: true,
        help: "which agent a new session is created against, on an account with more than one",
    },
    Key {
        name: "voice.wakePhrase",
        json: "wakePhrase",
        kind: Kind::Text,
        optional: true,
        help: "the words the wake word listens for, instead of the recordings",
    },
    Key {
        name: "voice.speechModel",
        json: "speechModel",
        kind: Kind::Text,
        optional: true,
        help: "which speech model listening uses, by id",
    },
    Key {
        name: "voice.vocabulary",
        json: "vocabulary",
        kind: Kind::Text,
        optional: true,
        help: "names every turn is read expecting, so they come back spelled as written",
    },
    Key {
        name: "voice.speakingRate",
        json: "speakingRate",
        kind: Kind::Number,
        optional: true,
        help: "how fast answers are read, as a multiple of the voice's own pace",
    },
    Key {
        name: "voice.volume",
        json: "volume",
        kind: Kind::Number,
        optional: true,
        help: "how loud answers are read",
    },
    Key {
        name: "voice.inputGain",
        json: "inputGain",
        kind: Kind::Number,
        optional: true,
        help: "how much the microphone is amplified before anything reads it",
    },
    Key {
        name: "voice.transcribeOn",
        json: "transcribeOn",
        kind: Kind::Text,
        optional: true,
        help: "where speech is transcribed: `cpu`, or `gpu:N`",
    },
    Key {
        name: "voice.speakOn",
        json: "speakOn",
        kind: Kind::Text,
        optional: true,
        help: "where answers are read: `cpu`, `gpu` or `npu`",
    },
    Key {
        name: "voice.defaultsFetched",
        json: "defaultsFetched",
        kind: Kind::Bool,
        optional: false,
        help: "an internal note to itself: the default speech model and voice have already been \
               fetched once",
    },
];

/// The one key with this name, if there is one.
pub fn key(name: &str) -> Option<&'static Key> {
    // The name a person types is matched exactly. A case-insensitive match would be kinder to
    // `voice.ReadAloud` and would also make `voice.readaloud` legal, which is a second spelling
    // of a key that the table above then has to stay in step with.
    KEYS.iter().find(|key| key.name == name)
}

/// What a device choice looks like on the command line: `default`, or the id of one device.
///
/// The words rather than the file's own JSON — `{"kind":"device","id":"…"}` is what a person would
/// have to type at a console to name a microphone, and that is the detail the window exists to
/// spare them.
pub fn read_device(text: &str) -> Result<Choice, String> {
    match text {
        "default" => Ok(Choice::Default),
        "" => Err("a device is `default` or a device id, and this is neither".to_string()),
        id => Ok(Choice::Device { id: id.to_string() }),
    }
}

/// A device choice as the command line writes it, so that reading one back and writing it out
/// again cannot disagree.
///
/// **A device whose id is the word `default` cannot be written**, which is the one thing this
/// spelling costs: `read_device("default")` gives the system's default rather than that device.
/// The Voice screen picks it by name, and no machine this has been tried on enumerates such an
/// id.
pub fn write_device(choice: &Choice) -> String {
    match choice {
        Choice::Default => "default".to_string(),
        Choice::Device { id } => id.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The words on the command line are the two shapes the file holds, in both directions.
    #[test]
    fn a_device_read_and_written_back_is_the_same_device() {
        assert_eq!(read_device("default").unwrap(), Choice::Default);
        assert_eq!(read_device("alsa_input.pci-0000_00_1f.3").unwrap(), Choice::Device {
            id: "alsa_input.pci-0000_00_1f.3".to_string(),
        });

        assert_eq!(write_device(&Choice::Default), "default");
        assert_eq!(
            write_device(&Choice::Device { id: "hw:1,0".to_string() }),
            "hw:1,0",
            "the id has to survive being written out, or a device cannot be named twice"
        );
    }

    /// **What the console writes is what the settings read**, pinned here rather than discovered
    /// by a person whose microphone did not open.
    ///
    /// The file holds a tagged object, which is `Choice`'s own `#[serde(tag = "kind")]`, and the
    /// test is on the *shape* because `zyris config` builds it by round-tripping through
    /// `read_device` and then through `Choice`'s `Serialize` — never by writing JSON of its own.
    #[test]
    fn the_file_holds_a_choice_as_a_tagged_object() {
        assert_eq!(
            serde_json::to_value(Choice::Default).unwrap(),
            serde_json::json!({ "kind": "default" })
        );
        assert_eq!(
            serde_json::to_value(Choice::Device { id: "hw:1,0".to_string() }).unwrap(),
            serde_json::json!({ "kind": "device", "id": "hw:1,0" })
        );
        // And back, which is what makes reading a value out of the file a value the console can
        // print.
        let device: Choice =
            serde_json::from_value(serde_json::json!({ "kind": "device", "id": "hw:1,0" })).unwrap();
        assert_eq!(write_device(&device), "hw:1,0");
    }

    #[test]
    fn a_key_is_found_by_the_name_it_is_typed_as() {
        assert_eq!(key("voice.listen").map(|key| key.json), Some("listen"));
        assert_eq!(key("voice.speakOn").map(|key| key.json), Some("speakOn"));
        assert!(key("voice.ReadAloud").is_none());
        assert!(key("listen").is_none(), "a key is the file's name *and* the group it is in");
    }

    /// **Every key in the file is a key the console can name, and the other way round.**
    ///
    /// This is the whole reason the table is in this crate: a field renamed in `Settings` and not
    /// here would leave `zyris config set voice.<oldName>` writing a key the voice reads past,
    /// and a key listed here that the struct no longer has would be a setting that can be set and
    /// never takes effect. Both are silent, and both fail this test.
    ///
    /// Behind `voice`, because `Settings` is: that is the feature the file belongs to, and CI
    /// compiles this arm (`cargo test -p zyris-voice --features voice`).
    #[cfg(feature = "voice")]
    #[test]
    fn the_keys_are_exactly_the_fields_of_the_settings_the_voice_reads() {
        use std::collections::BTreeSet;

        let file = crate::run::Settings::default();
        let in_file: BTreeSet<String> = serde_json::to_value(&file)
            .expect("the settings serialize")
            .as_object()
            .expect("the settings are a JSON object")
            .keys()
            .cloned()
            .collect();
        let declared: BTreeSet<String> = KEYS.iter().map(|key| key.json.to_string()).collect();

        assert_eq!(
            declared, in_file,
            "`zyris config`'s key list and the settings file have to name the same settings; \
             the difference is what a person cannot change from a console (declared and not in \
             the file) or cannot change at all (in the file and not declared)"
        );
    }

    /// **The names the console is addressed by are unique.**
    ///
    /// Two keys with one name would mean `config get` resolving to whichever is written first,
    /// and the other one unreachable — the same shape `zyris-mcp`'s duplicate-server rule exists
    /// to prevent, and just as silent.
    #[test]
    fn no_two_keys_share_a_name() {
        let mut seen = std::collections::BTreeSet::new();
        for key in KEYS {
            assert!(seen.insert(key.name), "two keys are called {}", key.name);
            assert!(seen.insert(key.json), "two keys are written to the file as {}", key.json);
        }
    }
}
