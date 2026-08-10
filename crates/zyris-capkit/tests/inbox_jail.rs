#![cfg(feature = "transfer")]

//! The jail is judged only by the real filesystem. There's something a pure function can't see
//! here — symlinks.

use zyris_capkit::transfer::inbox::{Inbox, InboxError};

fn temp_inbox() -> (tempfile::TempDir, Inbox) {
    let dir = tempfile::tempdir().unwrap();
    let inbox = Inbox::new(dir.path());
    (dir, inbox)
}

#[tokio::test]
async fn each_sender_node_gets_its_own_directory() {
    let (dir, inbox) = temp_inbox();
    let path = inbox.resolve("arch-zyris-code", "report.pdf").await.unwrap();
    assert_eq!(path, dir.path().join("arch-zyris-code").join("report.pdf"));
    assert!(path.parent().unwrap().is_dir(), "the parent must already be created");
}

#[tokio::test]
async fn path_escape_is_already_blocked_at_the_name_stage() {
    let (dir, inbox) = temp_inbox();
    let path = inbox.resolve("peer", "../../etc/passwd").await.unwrap();
    assert!(path.starts_with(dir.path()), "actual: {}", path.display());
    // safe_name only takes the last path segment — it doesn't prepend "etc". The
    // The only_one_path_component_survives test in name.rs has already pinned this value down.
    assert_eq!(path.file_name().unwrap(), "passwd");
}

#[tokio::test]
async fn sender_node_name_is_sanitized_too() {
    let (dir, inbox) = temp_inbox();
    // The peer can pick the slug however they like, so it also has to be reduced to a single
    // segment.
    let path = inbox.resolve("../../..", "a.txt").await.unwrap();
    assert!(path.starts_with(dir.path()), "actual: {}", path.display());
}

#[cfg(unix)]
#[tokio::test]
async fn rejects_when_the_parent_is_a_symlink() {
    let (dir, inbox) = temp_inbox();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("peer")).unwrap();

    let result = inbox.resolve("peer", "a.txt").await;
    assert!(matches!(result, Err(InboxError::SymlinkInPath)), "actual: {result:?}");
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
    let real_dir = tempfile::tempdir().unwrap();
    let link_holder = tempfile::tempdir().unwrap();
    let link = link_holder.path().join("inbox-link");
    std::os::unix::fs::symlink(real_dir.path(), &link).unwrap();

    let inbox = Inbox::new(&link);
    let result = inbox.resolve("peer", "a.txt").await;
    assert!(result.is_ok(), "actual: {result:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn rejects_even_when_the_destination_itself_is_a_symlink() {
    let (dir, inbox) = temp_inbox();
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("peer")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("stolen"), dir.path().join("peer").join("a.txt"))
        .unwrap();

    let result = inbox.resolve("peer", "a.txt").await;
    assert!(matches!(result, Err(InboxError::SymlinkInPath)), "actual: {result:?}");
}
