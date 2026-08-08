//! 감옥은 실제 파일시스템으로만 판정한다. 순수 함수가 못 보는 것이 여기 있다 — 심링크.

use zyris_capkit::transfer::inbox::{Inbox, InboxError};

fn 임시_inbox() -> (tempfile::TempDir, Inbox) {
    let 자리 = tempfile::tempdir().unwrap();
    let inbox = Inbox::new(자리.path());
    (자리, inbox)
}

#[tokio::test]
async fn 보낸_노드마다_제_디렉터리를_받는다() {
    let (자리, inbox) = 임시_inbox();
    let 길 = inbox.resolve("arch-zyris-code", "report.pdf").await.unwrap();
    assert_eq!(길, 자리.path().join("arch-zyris-code").join("report.pdf"));
    assert!(길.parent().unwrap().is_dir(), "부모를 미리 만들어 둬야 한다");
}

#[tokio::test]
async fn 경로_탈출은_이름_단계에서_이미_막힌다() {
    let (자리, inbox) = 임시_inbox();
    let 길 = inbox.resolve("peer", "../../etc/passwd").await.unwrap();
    assert!(길.starts_with(자리.path()), "실제: {}", 길.display());
    // safe_name은 마지막 경로 조각만 취한다 — "etc"까지 붙이지 않는다. name.rs의
    // 경로_조각_하나만_남는다 테스트가 이미 이 값을 못박아 뒀다.
    assert_eq!(길.file_name().unwrap(), "passwd");
}

#[tokio::test]
async fn 보낸_노드_이름도_씻는다() {
    let (자리, inbox) = 임시_inbox();
    // 상대가 slug를 마음대로 부를 수 있으므로 그것도 조각 하나여야 한다.
    let 길 = inbox.resolve("../../..", "a.txt").await.unwrap();
    assert!(길.starts_with(자리.path()), "실제: {}", 길.display());
}

#[cfg(unix)]
#[tokio::test]
async fn 부모가_심링크면_거부한다() {
    let (자리, inbox) = 임시_inbox();
    let 밖 = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(밖.path(), 자리.path().join("peer")).unwrap();

    let 결과 = inbox.resolve("peer", "a.txt").await;
    assert!(matches!(결과, Err(InboxError::SymlinkInPath)), "실제: {결과:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn 목적지_자체가_심링크여도_거부한다() {
    let (자리, inbox) = 임시_inbox();
    let 밖 = tempfile::tempdir().unwrap();
    std::fs::create_dir(자리.path().join("peer")).unwrap();
    std::os::unix::fs::symlink(밖.path().join("훔친다"), 자리.path().join("peer").join("a.txt"))
        .unwrap();

    let 결과 = inbox.resolve("peer", "a.txt").await;
    assert!(matches!(결과, Err(InboxError::SymlinkInPath)), "실제: {결과:?}");
}
