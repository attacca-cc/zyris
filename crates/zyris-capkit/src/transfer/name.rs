//! Washes the name the sending side proposed into a single path component the receiving side can
//! use.
//!
//! Stripping rather than rejecting: one awkward name is no reason for a whole transfer to fail. A
//! default name is handed out only when nothing survives the wash.
//!
//! **This alone does not make anything safe.** Washing guards the ordinary path; whatever slips
//! past it is caught by `inbox::resolve`'s real-path checks. Both have to be there.

/// Names Windows reserves for devices. Reserved even with an extension, so `con.txt` is caught
/// too.
const 예약어: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// The byte count a single name must not exceed. Visible outside the module because
/// `peer::part_path` also has to shorten the stem to stay within this limit when it appends the
/// `.part` suffix.
pub(super) const 최대_바이트: usize = 255;

pub fn safe_name(proposed: &str) -> String {
    // **Only the last component is kept.** Just stripping separators would turn
    // `../../etc/passwd` into `....etcpasswd` — safe, but unrecognizable to a human. Windows
    // paths arrive here too, so both `/` and `\` count as separators.
    let 마지막_조각 = proposed.rsplit(['/', '\\']).next().unwrap_or(proposed);
    let 씻은_것: String =
        마지막_조각.chars().filter(|&c| c != ':' && !c.is_control()).collect();
    let 씻은_것 = 씻은_것.trim();
    // Windows silently drops trailing dots and spaces. We strip them first so both names match.
    let 씻은_것 = 씻은_것.trim_end_matches(['.', ' ']);

    // `trim_end_matches` just above already stripped every trailing dot and space, so what is
    // left here can never be just "." or ".." — a string like that would already have been wiped
    // down to "" and caught by `is_empty`.
    if 씻은_것.is_empty() {
        return "file".to_string();
    }

    let 씻은_것 = 예약어_회피(씻은_것);
    let 잘린_것 = 길이_맞추기(&씻은_것);
    // **Strip again after truncating.** Cutting at a character boundary can happen to land right
    // on a dot or a space — for instance when the extension is dropped for being over 16 bytes
    // and the point that exactly fills the remaining budget happens to be a dot. Skip this second
    // strip and the invariant this module exists to keep — "the same name Windows would silently
    // produce" — breaks. If stripping empties the string entirely, fall back to the default name.
    let 잘린_것 = 잘린_것.trim_end_matches(['.', ' ']);
    if 잘린_것.is_empty() { "file".to_string() } else { 잘린_것.to_string() }
}

/// Appends `_` to the stem if it is a reserved name. The extension is left alone — a human still
/// needs to tell what the file was.
fn 예약어_회피(이름: &str) -> String {
    let (줄기, 확장자) = match 이름.split_once('.') {
        Some((줄기, 나머지)) => (줄기, Some(나머지)),
        None => (이름, None),
    };
    let 걸렸나 = 예약어.iter().any(|r| r.eq_ignore_ascii_case(줄기));
    if !걸렸나 {
        return 이름.to_string();
    }
    match 확장자 {
        Some(확장자) => format!("{줄기}_.{확장자}"),
        None => format!("{줄기}_"),
    }
}

/// Fits the name to the filesystem limit. **Cuts at a character boundary** — cutting by byte
/// panics on Korean text.
fn 길이_맞추기(이름: &str) -> String {
    if 이름.len() <= 최대_바이트 {
        return 이름.to_string();
    }
    // Keep the extension — it is the only clue left to what the file was.
    let 확장자 = 이름.rsplit_once('.').map(|(_, e)| format!(".{e}")).unwrap_or_default();
    let 확장자 = if 확장자.len() < 16 { 확장자 } else { String::new() };
    let 남길_바이트 = 최대_바이트 - 확장자.len();

    let mut 줄기 = String::new();
    for c in 이름.chars() {
        if 줄기.len() + c.len_utf8() > 남길_바이트 {
            break;
        }
        줄기.push(c);
    }
    format!("{줄기}{확장자}")
}

#[cfg(test)]
mod tests {
    use super::safe_name;

    #[test]
    fn only_one_path_component_survives() {
        let 표 = [
            ("report.pdf", "report.pdf"),
            // Not stripping separators — **taking only the last component.** Just stripping
            // would turn `../../etc/passwd` into `....etcpasswd`: safe, but unreadable.
            ("../../etc/passwd", "passwd"),
            ("/etc/passwd", "passwd"),
            ("a/b/c.txt", "c.txt"),
            (r"C:\Windows\system32\cmd.exe", "cmd.exe"),
            ("..", "file"),
            (".", "file"),
            ("", "file"),
            ("   ", "file"),
            ("a\0b.txt", "ab.txt"),
            ("a\nb.txt", "ab.txt"),
            // Unicode fullwidth dots do not normalize to `..`, but they are not separators
            // either. Keep them.
            ("．．", "．．"),
            // Windows reserved names stay reserved even with an extension.
            ("CON", "CON_"),
            ("con.txt", "con_.txt"),
            ("LPT1.log", "LPT1_.log"),
            // Windows silently drops trailing dots and spaces.
            ("name.", "name"),
            ("name ", "name"),
        ];
        for (넣은_것, 나올_것) in 표 {
            assert_eq!(safe_name(넣은_것), 나올_것, "input: {넣은_것:?}");
        }
    }

    #[test]
    fn never_exceeds_255_bytes_no_matter_how_long() {
        let 긴_것 = "가".repeat(300);
        let 나온_것 = safe_name(&긴_것);
        assert!(나온_것.len() <= 255, "{} bytes", 나온_것.len());
        // Must cut at a character boundary — cutting by byte panics.
        assert!(나온_것.chars().all(|c| c == '가'));
    }

    #[test]
    fn extension_survives_truncation() {
        let 긴_것 = format!("{}.tar.gz", "a".repeat(300));
        let 나온_것 = safe_name(&긴_것);
        assert!(나온_것.len() <= 255);
        assert!(나온_것.ends_with(".gz"), "actual: {나온_것}");
    }

    // Review finding: if truncation ends right on a dot or a space, the trace stays behind and
    // breaks the invariant the wash exists to keep — "the same name Windows would silently
    // produce". The three tests below cover that spot. All three also exercise the path where the
    // extension is 16 bytes or longer and gets dropped outright.

    #[test]
    fn extension_dropped_for_length_and_cut_lands_on_dot_strips_again() {
        // Extension candidate `.` + "b"*20 = 21 bytes, over the 16-byte threshold, so it gets
        // dropped. The point that fills the remaining 255 bytes lands exactly on that dot — skip
        // the second strip and one dot is left dangling after the 254 "a"s.
        let 긴_것 = format!("{}.{}", "a".repeat(254), "b".repeat(20));
        let 나온_것 = safe_name(&긴_것);
        assert_eq!(나온_것, "a".repeat(254), "actual: {나온_것}");
        assert!(!나온_것.ends_with('.'), "actual: {나온_것}");
    }

    #[test]
    fn no_extension_and_cut_lands_on_space_strips_again() {
        // Uses a space instead of a dot as the divider, to also cover the path where extension
        // detection never triggers at all.
        let 긴_것 = format!("{} {}", "a".repeat(254), "b".repeat(20));
        let 나온_것 = safe_name(&긴_것);
        assert_eq!(나온_것, "a".repeat(254), "actual: {나온_것}");
        assert!(!나온_것.ends_with(' '), "actual: {나온_것}");
    }

    #[test]
    fn falls_back_to_default_name_when_truncation_leaves_nothing() {
        // The whole prefix is dots, so wherever the 255-byte limit cuts, it also lands on a dot.
        // The post-truncation re-strip empties the string entirely, so this must fall back to the
        // default name.
        let 긴_것 = format!("{}z.{}", ".".repeat(260), "b".repeat(20));
        let 나온_것 = safe_name(&긴_것);
        assert_eq!(나온_것, "file", "actual: {나온_것}");
    }
}
