//! Washes the name the sending side proposed into a single path component the receiving side can
//! use.
//!
//! Stripping rather than rejecting: one awkward name is no reason for a whole transfer to fail. A
//! default name is handed out only when nothing survives the wash.
//!
//! **This alone does not make anything safe.** Washing guards the ordinary path; whatever slips
//! past it is caught by `inbox::resolve`'s real-path checks. Both have to be there.

/// 윈도우가 장치로 예약한 이름들. 확장자가 붙어도 예약이라 `con.txt`도 걸린다.
const 예약어: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// 이름 하나가 넘으면 안 되는 바이트 수. `peer::part_path`가 `.part` 꼬리를 붙일 때도 이
/// 한도 안에 들어가도록 줄기를 줄여야 하므로 모듈 밖에서 보인다.
pub(super) const 최대_바이트: usize = 255;

pub fn safe_name(proposed: &str) -> String {
    // **마지막 조각만 취한다.** 구분자를 지우기만 하면 `../../etc/passwd`가
    // `....etcpasswd`가 되는데, 안전하기는 해도 사람이 무엇인지 알아볼 수 없다.
    // 윈도우 경로도 같이 받으므로 `/`와 `\` 둘 다 구분자로 본다.
    let 마지막_조각 = proposed.rsplit(['/', '\\']).next().unwrap_or(proposed);
    let 씻은_것: String =
        마지막_조각.chars().filter(|&c| c != ':' && !c.is_control()).collect();
    let 씻은_것 = 씻은_것.trim();
    // 끝의 마침표와 공백은 윈도우가 조용히 지운다. 우리가 먼저 지워 양쪽 이름을 같게 둔다.
    let 씻은_것 = 씻은_것.trim_end_matches(['.', ' ']);

    // `trim_end_matches`가 바로 위에서 끝의 마침표·공백을 전부 지우므로, 여기 남은 문자열이
    // "."나 ".."뿐인 경우는 없다 — 그런 문자열은 이미 위에서 ""로 지워져 `is_empty`에 걸린다.
    if 씻은_것.is_empty() {
        return "file".to_string();
    }

    let 씻은_것 = 예약어_회피(씻은_것);
    let 잘린_것 = 길이_맞추기(&씻은_것);
    // **자르기가 끝난 뒤에도 다시 지운다.** 문자 경계에서 자르다 보면 우연히 마침표나
    // 공백 위에서 끝날 수 있다 — 예를 들어 확장자가 16바이트를 넘어 버려지고, 남은
    // 글자 수를 딱 채우는 지점이 하필 마침표인 경우다. 여기서 다시 지우지 않으면 이
    // 모듈이 지키려는 "윈도우가 조용히 지우는 것과 같은 이름"이라는 불변이 깨진다.
    // 다 지워져 비면 기본 이름으로 돌아간다.
    let 잘린_것 = 잘린_것.trim_end_matches(['.', ' ']);
    if 잘린_것.is_empty() { "file".to_string() } else { 잘린_것.to_string() }
}

/// 예약어면 줄기 끝에 `_`를 붙인다. 확장자는 그대로 둔다 — 사람이 무엇이었는지 알아야 한다.
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

/// 파일시스템 한도에 맞춘다. **문자 경계에서 자른다** — 바이트로 자르면 한글에서 패닉한다.
fn 길이_맞추기(이름: &str) -> String {
    if 이름.len() <= 최대_바이트 {
        return 이름.to_string();
    }
    // 확장자는 살린다. 무엇이었는지 알아보는 유일한 단서다.
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
    fn 경로_조각_하나만_남는다() {
        let 표 = [
            ("report.pdf", "report.pdf"),
            // 구분자를 지우는 것이 아니라 **마지막 조각만 취한다.** 지우기만 하면
            // `../../etc/passwd`가 `....etcpasswd`가 되어 안전하지만 읽을 수 없다.
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
            // 유니코드 전각 마침표는 `..`로 정규화되지 않지만 구분자도 아니다. 남긴다.
            ("．．", "．．"),
            // 윈도우 예약 이름은 확장자가 붙어도 예약이다.
            ("CON", "CON_"),
            ("con.txt", "con_.txt"),
            ("LPT1.log", "LPT1_.log"),
            // 끝의 마침표와 공백은 윈도우가 조용히 지운다.
            ("name.", "name"),
            ("name ", "name"),
        ];
        for (넣은_것, 나올_것) in 표 {
            assert_eq!(safe_name(넣은_것), 나올_것, "입력: {넣은_것:?}");
        }
    }

    #[test]
    fn 아무리_길어도_255바이트를_안_넘는다() {
        let 긴_것 = "가".repeat(300);
        let 나온_것 = safe_name(&긴_것);
        assert!(나온_것.len() <= 255, "{}바이트", 나온_것.len());
        // 문자 경계에서 잘라야 한다 — 바이트로 자르면 패닉한다.
        assert!(나온_것.chars().all(|c| c == '가'));
    }

    #[test]
    fn 확장자가_있으면_자를_때도_남긴다() {
        let 긴_것 = format!("{}.tar.gz", "a".repeat(300));
        let 나온_것 = safe_name(&긴_것);
        assert!(나온_것.len() <= 255);
        assert!(나온_것.ends_with(".gz"), "실제: {나온_것}");
    }

    // 리뷰 지적: 자르기가 마침표나 공백 위에서 끝나면 그 흔적이 남아, 씻기가 지키려는
    // "윈도우가 조용히 지우는 것과 같은 이름" 불변이 깨진다. 아래 세 테스트가 그 지점을
    // 덮는다. 셋 다 확장자가 16바이트 이상이라 통째로 버려지는 경로도 함께 지난다.

    #[test]
    fn 확장자가_너무_길어_버려지고_자른_자리가_마침표면_다시_지운다() {
        // 확장자 후보 `.` + "b"*20 = 21바이트라 16바이트 문턱을 넘어 버려진다. 남는
        // 255바이트를 채우는 지점이 정확히 그 마침표 위라, 다시 지우지 않으면 "a"
        // 254개 뒤에 마침표 하나가 그대로 남는다.
        let 긴_것 = format!("{}.{}", "a".repeat(254), "b".repeat(20));
        let 나온_것 = safe_name(&긴_것);
        assert_eq!(나온_것, "a".repeat(254), "실제: {나온_것}");
        assert!(!나온_것.ends_with('.'), "실제: {나온_것}");
    }

    #[test]
    fn 확장자가_없어_자른_자리가_공백이면_다시_지운다() {
        // 마침표 대신 공백을 구분자로 써서, 확장자 판정 자체가 안 걸리는 경로도 같이 본다.
        let 긴_것 = format!("{} {}", "a".repeat(254), "b".repeat(20));
        let 나온_것 = safe_name(&긴_것);
        assert_eq!(나온_것, "a".repeat(254), "실제: {나온_것}");
        assert!(!나온_것.ends_with(' '), "실제: {나온_것}");
    }

    #[test]
    fn 자르고_남은_것이_전부_지워지면_기본_이름으로_돌아간다() {
        // 앞부분이 죄다 마침표라 255바이트 한도 안에서 잘리는 자리도 마침표뿐이다.
        // 자르기 뒤 재트리밍이 통째로 비워내는 경우이므로 기본 이름으로 떨어져야 한다.
        let 긴_것 = format!("{}z.{}", ".".repeat(260), "b".repeat(20));
        let 나온_것 = safe_name(&긴_것);
        assert_eq!(나온_것, "file", "실제: {나온_것}");
    }
}
