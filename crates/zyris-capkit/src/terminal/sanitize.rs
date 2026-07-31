//! `read`가 돌려줄 텍스트에서 터미널 제어 시퀀스를 걷어낸다.
//!
//! **왜 필요한가(라이브 실측, 2026-07-31)**: Attacca의 안전 가드가 U+001B가 든 tool
//! 출력을 통째로 거부한다 — `"tool output contains disallowed control character U+001B"`.
//! 셸 프롬프트 하나만 있어도 걸리므로, 걷어내지 않으면 `read`는 실제 셸에서 거의 항상
//! 차단된다. 로컬 테스트로는 잡을 수 없는 종류의 결함이다.
//!
//! 스펙 §3.2가 base64를 물리친 논리가 그대로 적용된다: 에이전트가 읽어야 하는 텍스트를
//! 모델이 쓸 수 없는 형태로 두면 안 된다. 모델은 커서 이동 시퀀스로 할 수 있는 것이
//! 없다 — 화면의 *결과*가 필요하면 `screen`이 그것을 준다.
//!
//! **링버퍼는 원본 그대로 둔다.** 여기서 거르는 것은 `read`의 응답 경계뿐이고,
//! `open_stream`은 진짜 TUI 소비자를 위해 바이트를 손대지 않고 흘려보낸다.

/// 잘린 이스케이프 시퀀스를 뺀 길이. 그 꼬리는 다음 호출로 넘어간다.
///
/// UTF-8 꼬리를 붙잡아 두는 것과 같은 이유다 — 여기서 잘라 버리면 다음 호출에
/// 시퀀스의 뒷부분만 남아 알파벳 몇 글자가 본문에 튀어나온다.
pub(crate) fn trim_incomplete_escape(b: &[u8]) -> usize {
    // 뒤에서부터 ESC를 찾되, 이미 끝난 시퀀스면 그대로 둔다. 가장 긴 OSC도 현실적으로
    // 이 창을 넘지 않는다.
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

/// `b[i]`가 ESC일 때, 시퀀스가 끝나는 다음 인덱스. 아직 안 끝났으면 `None`.
fn scan_escape(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    let kind = *b.get(j)?;
    match kind {
        // CSI — 파라미터·중간 바이트가 이어지다 0x40..=0x7E에서 끝난다.
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
        // OSC·DCS·SOS·PM·APC — BEL이나 ST(ESC \)로 끝난다.
        b']' | b'P' | b'X' | b'^' | b'_' => {
            j += 1;
            while let Some(&c) = b.get(j) {
                if c == 0x07 {
                    return Some(j + 1);
                }
                if c == 0x1B {
                    return match b.get(j + 1) {
                        Some(b'\\') => Some(j + 2),
                        Some(_) => Some(j + 1), // 다른 ESC가 시작됐다 — 여기서 끊는다
                        None => None,
                    };
                }
                j += 1;
            }
            None
        }
        // 나머지는 두 바이트짜리다 (ESC c, ESC 7, ESC = …).
        _ => Some(j + 1),
    }
}

/// 제어 시퀀스를 걷어내고 남은 텍스트.
///
/// 남기는 제어 문자는 `\n`과 `\t` 둘뿐이다. `\r\n`은 `\n`으로 접고, 홀로 남은 `\r`는
/// 버린다 — 진행 표시줄이 같은 줄을 덮어쓰는 용도인데 그 효과는 `screen`이 보여준다.
pub(crate) fn strip_controls(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            0x1B => match scan_escape(b, i) {
                Some(end) => i = end,
                None => break, // 잘린 꼬리 — 호출자가 이미 잘라냈어야 한다
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
                // 여기부터는 멀티바이트일 수 있으므로 문자 단위로 옮긴다.
                let ch = s[i..].chars().next().expect("경계는 유효하다");
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
        assert_eq!(strip_controls("가나다 🎯"), "가나다 🎯");
    }

    #[test]
    fn csi_sequences_are_removed() {
        assert_eq!(strip_controls("\u{1b}[2J\u{1b}[3;5HMARKER"), "MARKER");
        assert_eq!(strip_controls("\u{1b}[0;31mred\u{1b}[0m"), "red");
        // bracketed paste — 실제 프롬프트에 늘 붙어 나온다.
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

    /// 이 하나가 라이브에서 `read`를 통째로 막았다 — 결과에 ESC가 남으면 안 된다.
    #[test]
    fn no_escape_byte_survives() {
        let messy = "\u{1b}[?2004hsh-5.3$ cd /tmp\r\n\u{1b}[?2004l\r/tmp\r\n";
        let out = strip_controls(messy);
        assert!(!out.contains('\u{1b}'), "ESC가 남았다: {out:?}");
        assert_eq!(out, "sh-5.3$ cd /tmp\n/tmp\n");
    }

    #[test]
    fn a_complete_escape_is_not_held_back() {
        assert_eq!(trim_incomplete_escape(b"a\x1b[0mb"), 6);
        assert_eq!(trim_incomplete_escape(b"plain"), 5);
        assert_eq!(trim_incomplete_escape(b""), 0);
    }

    /// 잘린 시퀀스를 그냥 버리면 다음 호출에 `0m` 같은 조각이 본문으로 튀어나온다.
    #[test]
    fn an_incomplete_escape_is_held_back() {
        assert_eq!(trim_incomplete_escape(b"ab\x1b"), 2);
        assert_eq!(trim_incomplete_escape(b"ab\x1b["), 2);
        assert_eq!(trim_incomplete_escape(b"ab\x1b[0"), 2);
        assert_eq!(trim_incomplete_escape(b"ab\x1b]0;ti"), 2);
    }
}
