#![cfg(feature = "transfer")]

//! The jail is judged only by the real filesystem. There's something a pure function can't see
//! here — symlinks.

use zyris_capkit::transfer::inbox::{Inbox, InboxError};

fn 임시_inbox() -> (tempfile::TempDir, Inbox) {
    let 자리 = tempfile::tempdir().unwrap();
    let inbox = Inbox::new(자리.path());
    (자리, inbox)
}

#[tokio::test]
async fn each_sender_node_gets_its_own_directory() {
    let (자리, inbox) = 임시_inbox();
    let 길 = inbox.resolve("arch-zyris-code", "report.pdf").await.unwrap();
    assert_eq!(길, 자리.path().join("arch-zyris-code").join("report.pdf"));
    assert!(길.parent().unwrap().is_dir(), "the parent must already be created");
}

#[tokio::test]
async fn path_escape_is_already_blocked_at_the_name_stage() {
    let (자리, inbox) = 임시_inbox();
    let 길 = inbox.resolve("peer", "../../etc/passwd").await.unwrap();
    assert!(길.starts_with(자리.path()), "actual: {}", 길.display());
    // safe_name only takes the last path segment — it doesn't prepend "etc". The
    // The only_one_path_component_survives test in name.rs has already pinned this value down.
    assert_eq!(길.file_name().unwrap(), "passwd");
}

#[tokio::test]
async fn sender_node_name_is_sanitized_too() {
    let (자리, inbox) = 임시_inbox();
    // The peer can pick the slug however they like, so it also has to be reduced to a single
    // segment.
    let 길 = inbox.resolve("../../..", "a.txt").await.unwrap();
    assert!(길.starts_with(자리.path()), "actual: {}", 길.display());
}

#[cfg(unix)]
#[tokio::test]
async fn rejects_when_the_parent_is_a_symlink() {
    let (자리, inbox) = 임시_inbox();
    let 밖 = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(밖.path(), 자리.path().join("peer")).unwrap();

    let 결과 = inbox.resolve("peer", "a.txt").await;
    assert!(matches!(결과, Err(InboxError::SymlinkInPath)), "actual: {결과:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn passes_even_when_an_ancestor_of_the_root_is_a_symlink() {
    // The inbox itself or one of its ancestors being a symlink — like macOS's /var →
    // /private/var, or a symlinked home directory — is the receiver's own setup, not an
    // attack. If the walk starts from a canonicalized baseline, strip_prefix fails and it has
    // to walk again from the filesystem root, and along the way it trips over a symlink
    // ancestor outside the inbox and rejects a perfectly normal request — this is what guards
    // against that here.
    let 진짜 = tempfile::tempdir().unwrap();
    let 링크_담을_곳 = tempfile::tempdir().unwrap();
    let 링크 = 링크_담을_곳.path().join("inbox-link");
    std::os::unix::fs::symlink(진짜.path(), &링크).unwrap();

    let inbox = Inbox::new(&링크);
    let 결과 = inbox.resolve("peer", "a.txt").await;
    assert!(결과.is_ok(), "actual: {결과:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn rejects_even_when_the_destination_itself_is_a_symlink() {
    let (자리, inbox) = 임시_inbox();
    let 밖 = tempfile::tempdir().unwrap();
    std::fs::create_dir(자리.path().join("peer")).unwrap();
    std::os::unix::fs::symlink(밖.path().join("훔친다"), 자리.path().join("peer").join("a.txt"))
        .unwrap();

    let 결과 = inbox.resolve("peer", "a.txt").await;
    assert!(matches!(결과, Err(InboxError::SymlinkInPath)), "actual: {결과:?}");
}
