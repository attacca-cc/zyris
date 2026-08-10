//! Strips terminal control sequences from the text `read` returns.
//!
//! **Why this is needed (live measurement, 2026-07-31)**: Attacca's safety guard outright rejects
//! tool output that contains U+001B — `"tool output contains disallowed control character
//! U+001B"`. A single shell prompt is enough to trigger it, so without stripping, `read` is
//! blocked in an actual shell almost every time. This is the kind of defect local tests cannot
//! catch.
//!
//! The same reasoning that ruled out base64 in spec §3.2 applies here: text the agent needs to
//! read must not be left in a form the model cannot consume. There is nothing a model can do with
//! a cursor-movement sequence — if it needs the *result* on screen, `screen` gives it that.
//!
//! **The ring buffer is left untouched.** What gets filtered here is only the response boundary
//! of `read`; `open_stream` passes bytes through unmodified for real TUI consumers.

/// Length with a truncated escape sequence excluded. That tail carries over to the next call.
///
/// Same reason as holding back a UTF-8 tail — if this cut it off here, the next call would be
/// left with only the back half of the sequence, and a few stray letters would leak into the body.
pub(crate) fn trim_incomplete_escape(b: &[u8]) -> usize {
    // Scan backward for ESC, but leave it alone if the sequence it starts is already complete.
    // Even the longest OSC realistically never exceeds this window.
    const LOOKBACK: usize = 256;
    let from = b.len().saturating_sub(LOOKBACK);
    let Some(esc) = b[from..].iter().rposition(|&c| c == 0x1B).map(|i| i + from) else {
        return b.len();
    };
    match scan_escape(b, esc) {
        Some(_) => b.len(),
        None => esc,
    }
}

/// When `b[i]` is ESC, the next index at which the sequence ends. `None` if it is not finished yet.
fn scan_escape(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    let kind = *b.get(j)?;
    match kind {
        // CSI — parameter and intermediate bytes continue, ending on 0x40..=0x7E.
        b'[' => {
            j += 1;
            while let Some(&c) = b.get(j) {
                j += 1;
                if (0x40..=0x7E).contains(&c) {
                    return Some(j);
                }
            }
            None
        }
        // OSC/DCS/SOS/PM/APC — end on BEL or ST (ESC \).
        b']' | b'P' | b'X' | b'^' | b'_' => {
            j += 1;
            while let Some(&c) = b.get(j) {
                if c == 0x07 {
                    return Some(j + 1);
                }
                if c == 0x1B {
                    return match b.get(j + 1) {
                        Some(b'\\') => Some(j + 2),
                        Some(_) => Some(j + 1), // another ESC has started — cut here
                        None => None,
                    };
                }
                j += 1;
            }
            None
        }
        // Everything else is two bytes long (ESC c, ESC 7, ESC = ...).
        _ => Some(j + 1),
    }
}

/// The text left after control sequences are stripped out.
///
/// The only control characters kept are `\n` and `\t`. `\r\n` collapses to `\n`, and a bare `\r`
/// on its own is dropped — that is what a progress bar uses to overwrite the same line, and
/// `screen` is what shows that effect.
pub(crate) fn strip_controls(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            0x1B => match scan_escape(b, i) {
                Some(end) => i = end,
                None => break, // truncated tail — the caller should already have trimmed this
            },
            b'\r' => {
                if b.get(i + 1) == Some(&b'\n') {
                    out.push('\n');
                    i += 2;
                } else {
                    i += 1;
                }
            }
            b'\n' | b'\t' => {
                out.push(b[i] as char);
                i += 1;
            }
            c if c < 0x20 || c == 0x7F => i += 1,
            _ => {
                // From here on it may be multi-byte, so move it across one character at a time.
                let ch = s[i..].chars().next().expect("the boundary is valid");
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_passes_through() {
        assert_eq!(strip_controls("hello world"), "hello world");
        // Multibyte text (Hangul plus a multi-codepoint emoji cluster) survives untouched.
        assert_eq!(strip_controls("가나다 🎯"), "가나다 🎯");
    }

    #[test]
    fn csi_sequences_are_removed() {
        assert_eq!(strip_controls("\u{1b}[2J\u{1b}[3;5HMARKER"), "MARKER");
        assert_eq!(strip_controls("\u{1b}[0;31mred\u{1b}[0m"), "red");
        // bracketed paste — this always tags along with a real prompt.
        assert_eq!(strip_controls("\u{1b}[?2004hsh-5.3$ \u{1b}[?2004l"), "sh-5.3$ ");
    }

    #[test]
    fn osc_sequences_are_removed() {
        assert_eq!(strip_controls("\u{1b}]0;title\u{7}body"), "body");
        assert_eq!(strip_controls("\u{1b}]0;title\u{1b}\\body"), "body");
    }

    #[test]
    fn newlines_survive_and_bare_cr_does_not() {
        assert_eq!(strip_controls("a\r\nb\n"), "a\nb\n");
        assert_eq!(strip_controls("50%\r100%"), "50%100%");
        assert_eq!(strip_controls("a\tb"), "a\tb");
        assert_eq!(strip_controls("a\u{0}b\u{7}c"), "abc");
    }

    /// This one alone blocked `read` outright in live use — no ESC may survive in the result.
    #[test]
    fn no_escape_byte_survives() {
        let messy = "\u{1b}[?2004hsh-5.3$ cd /tmp\r\n\u{1b}[?2004l\r/tmp\r\n";
        let out = strip_controls(messy);
        assert!(!out.contains('\u{1b}'), "ESC survived: {out:?}");
        assert_eq!(out, "sh-5.3$ cd /tmp\n/tmp\n");
    }

    #[test]
    fn a_complete_escape_is_not_held_back() {
        assert_eq!(trim_incomplete_escape(b"a\x1b[0mb"), 6);
        assert_eq!(trim_incomplete_escape(b"plain"), 5);
        assert_eq!(trim_incomplete_escape(b""), 0);
    }

    /// Simply dropping a truncated sequence would leak a fragment like `0m` into the body on the
    /// next call.
    #[test]
    fn an_incomplete_escape_is_held_back() {
        assert_eq!(trim_incomplete_escape(b"ab\x1b"), 2);
        assert_eq!(trim_incomplete_escape(b"ab\x1b["), 2);
        assert_eq!(trim_incomplete_escape(b"ab\x1b[0"), 2);
        assert_eq!(trim_incomplete_escape(b"ab\x1b]0;ti"), 2);
    }
}
