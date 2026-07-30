use enigo::Key;
use zyris::WireError;

/// Split a chord such as `ctrl+shift+t` into the modifiers to hold and the key to press.
///
/// Segments are separated by `+` and matched case-insensitively. A single-character segment is
/// the character itself, so `ctrl+C` and `ctrl+c` both press C — the shift is yours to add. An
/// empty segment is a literal `+`, which is what makes `ctrl++` mean "zoom in".
pub(crate) fn parse_chord(chord: &str) -> zyris::Result<(Vec<Key>, Key)> {
    // `+` is both the separator and a key, and splitting cannot tell them apart. Peel the literal
    // off the end first — `+` is the key alone, `ctrl++` is Control plus it — and what is left is
    // unambiguous.
    let trimmed = chord.trim();
    if trimmed.is_empty() {
        return Err(WireError::invalid_params("empty chord"));
    }
    let (body, plus_is_the_key) = match trimmed {
        "+" => ("", true),
        _ => match trimmed.strip_suffix("++") {
            Some(rest) => (rest, true),
            None => (trimmed, false),
        },
    };

    let mut modifiers = Vec::new();
    let mut key = plus_is_the_key.then_some(Key::Unicode('+'));

    for segment in body.split('+') {
        let segment = segment.trim();
        if segment.is_empty() {
            if body.is_empty() {
                continue;
            }
            return Err(WireError::invalid_params(format!(
                "empty key in chord `{chord}`"
            )));
        }
        if let Some(modifier) = modifier(&segment.to_ascii_lowercase()) {
            modifiers.push(modifier);
        } else {
            set_key(&mut key, named(segment, chord)?, chord)?;
        }
    }

    match key {
        Some(key) => Ok((modifiers, key)),
        None => Err(WireError::invalid_params(format!(
            "chord `{chord}` is all modifiers and has no key to press"
        ))),
    }
}

fn set_key(slot: &mut Option<Key>, key: Key, chord: &str) -> zyris::Result<()> {
    if slot.is_some() {
        return Err(WireError::invalid_params(format!(
            "chord `{chord}` presses more than one non-modifier key"
        )));
    }
    *slot = Some(key);
    Ok(())
}

fn modifier(lowered: &str) -> Option<Key> {
    // `Key::Meta` is the platform's own: Command on macOS, Super on Linux, Win on Windows.
    Some(match lowered {
        "ctrl" | "control" => Key::Control,
        "shift" => Key::Shift,
        "alt" | "option" | "opt" => Key::Alt,
        "meta" | "super" | "cmd" | "command" | "win" | "windows" => Key::Meta,
        _ => return None,
    })
}

fn named(segment: &str, chord: &str) -> zyris::Result<Key> {
    let lowered = segment.to_ascii_lowercase();
    let key = match lowered.as_str() {
        "enter" | "return" => Key::Return,
        "tab" => Key::Tab,
        "esc" | "escape" => Key::Escape,
        "backspace" => Key::Backspace,
        "del" | "delete" => Key::Delete,
        "space" => Key::Space,
        "up" => Key::UpArrow,
        "down" => Key::DownArrow,
        "left" => Key::LeftArrow,
        "right" => Key::RightArrow,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" | "pgup" => Key::PageUp,
        "pagedown" | "pgdn" => Key::PageDown,
        "capslock" => Key::CapsLock,
        "printscreen" | "prtsc" => Key::PrintScr,
        // enigo has no `Insert` on macOS, and neither does the keyboard.
        #[cfg(not(target_os = "macos"))]
        "insert" => Key::Insert,
        "f1" => Key::F1,
        "f2" => Key::F2,
        "f3" => Key::F3,
        "f4" => Key::F4,
        "f5" => Key::F5,
        "f6" => Key::F6,
        "f7" => Key::F7,
        "f8" => Key::F8,
        "f9" => Key::F9,
        "f10" => Key::F10,
        "f11" => Key::F11,
        "f12" => Key::F12,
        "f13" => Key::F13,
        "f14" => Key::F14,
        "f15" => Key::F15,
        "f16" => Key::F16,
        "f17" => Key::F17,
        "f18" => Key::F18,
        "f19" => Key::F19,
        "f20" => Key::F20,
        _ => {
            // Use the original casing: `Key::Unicode` carries the character, not a keycode.
            let mut chars = segment.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Key::Unicode(c),
                _ => {
                    return Err(WireError::invalid_params(format!(
                        "unknown key `{segment}` in chord `{chord}`"
                    )))
                }
            }
        }
    };
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(chord: &str) -> (Vec<Key>, Key) {
        parse_chord(chord).unwrap()
    }

    #[test]
    fn a_bare_character_is_itself() {
        assert_eq!(parse("c"), (vec![], Key::Unicode('c')));
        assert_eq!(parse("C"), (vec![], Key::Unicode('C')));
    }

    #[test]
    fn named_keys_are_case_insensitive() {
        assert_eq!(parse("Enter"), (vec![], Key::Return));
        assert_eq!(parse("enter"), (vec![], Key::Return));
        assert_eq!(parse("F12"), (vec![], Key::F12));
        assert_eq!(parse("pgdn"), (vec![], Key::PageDown));
    }

    #[test]
    fn modifiers_come_back_in_order() {
        assert_eq!(parse("ctrl+c"), (vec![Key::Control], Key::Unicode('c')));
        assert_eq!(
            parse("ctrl+shift+t"),
            (vec![Key::Control, Key::Shift], Key::Unicode('t'))
        );
        assert_eq!(parse("cmd+alt+f4"), (vec![Key::Meta, Key::Alt], Key::F4));
    }

    #[test]
    fn whitespace_around_segments_is_ignored() {
        assert_eq!(parse(" ctrl + c "), (vec![Key::Control], Key::Unicode('c')));
    }

    #[test]
    fn plus_is_both_the_separator_and_a_key() {
        assert_eq!(parse("+"), (vec![], Key::Unicode('+')));
        assert_eq!(parse("ctrl++"), (vec![Key::Control], Key::Unicode('+')));
        assert_eq!(
            parse("ctrl+shift++"),
            (vec![Key::Control, Key::Shift], Key::Unicode('+'))
        );
    }

    #[test]
    fn a_chord_needs_exactly_one_non_modifier() {
        assert!(parse_chord("ctrl").is_err());
        assert!(parse_chord("ctrl+shift").is_err());
        assert!(parse_chord("a+b").is_err());
    }

    #[test]
    fn unknown_names_and_stray_separators_are_rejected() {
        assert!(parse_chord("ctrl+nope").is_err());
        assert!(parse_chord("ctrl++c").is_err());
        assert!(parse_chord("").is_err());
    }
}
