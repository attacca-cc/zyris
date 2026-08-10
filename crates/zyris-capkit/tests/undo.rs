#![cfg(feature = "transfer")]

use std::os::unix::fs::PermissionsExt;

use zyris_capkit::transfer::undo::UndoStore;

#[tokio::test]
async fn moves_the_original_and_reports_where_it_went() {
    let store_dir = tempfile::tempdir().unwrap();
    let job = tempfile::tempdir().unwrap();
    let victim = job.path().join("a.pdf");
    tokio::fs::write(&victim, "old content".as_bytes()).await.unwrap();

    let store = UndoStore::new(store_dir.path());
    let landed_at = store.stash(&victim, 1_754_700_000_000).await.unwrap();

    assert!(!victim.exists(), "should be a move, not a copy");
    assert_eq!(tokio::fs::read(&landed_at).await.unwrap(), "old content".as_bytes());
    assert!(landed_at.starts_with(store_dir.path()));
}

#[tokio::test]
async fn a_missing_file_has_nothing_to_move() {
    let store_dir = tempfile::tempdir().unwrap();
    let job = tempfile::tempdir().unwrap();
    let store = UndoStore::new(store_dir.path());
    assert!(store.stash(&job.path().join("missing"), 1).await.is_none());
}

#[tokio::test]
async fn returns_none_instead_of_panicking_when_stashing_fails() {
    // Give it a location it can't write to. If missing a safety net were allowed to block the
    // transfer, that would leave a state that can't be fixed.
    let job = tempfile::tempdir().unwrap();
    let victim = job.path().join("a.pdf");
    tokio::fs::write(&victim, "x".as_bytes()).await.unwrap();

    let store = UndoStore::new("/proc/unwritable");
    assert!(store.stash(&victim, 1).await.is_none());
    assert!(victim.exists(), "if it couldn't move it, the original must still be there");
}

#[tokio::test]
async fn does_not_lose_the_earlier_backup_when_the_same_millisecond_and_name_collide() {
    // Simulates two different peers sending the same name (a.pdf) in the same millisecond.
    // If the slots collide, the one that arrives later silently overwrites the earlier backup.
    let store_dir = tempfile::tempdir().unwrap();
    let job1 = tempfile::tempdir().unwrap();
    let job2 = tempfile::tempdir().unwrap();
    let victim1 = job1.path().join("a.pdf");
    let victim2 = job2.path().join("a.pdf");
    tokio::fs::write(&victim1, "AAA".as_bytes()).await.unwrap();
    tokio::fs::write(&victim2, "BBB".as_bytes()).await.unwrap();

    let store = UndoStore::new(store_dir.path());
    let dir1 = store.stash(&victim1, 1000).await.unwrap();
    let dir2 = store.stash(&victim2, 1000).await.unwrap();

    assert_ne!(dir1, dir2, "slots must not collide even with the same millisecond and same name");
    assert_eq!(tokio::fs::read(&dir1).await.unwrap(), "AAA".as_bytes());
    assert_eq!(tokio::fs::read(&dir2).await.unwrap(), "BBB".as_bytes());
}

#[tokio::test]
async fn returns_the_already_made_backup_path_even_when_only_the_original_deletion_fails() {
    // Stripping write permission from the working directory blocks both rename (which needs to
    // change a directory entry) and deleting the original (unlink). The file's own read
    // permission is still intact, so copy goes through — copy succeeds, only the deletion
    // fails.
    let store_dir = tempfile::tempdir().unwrap();
    let job = tempfile::tempdir().unwrap();
    let victim = job.path().join("a.pdf");
    tokio::fs::write(&victim, "old content".as_bytes()).await.unwrap();

    let original_mode = tokio::fs::metadata(job.path()).await.unwrap().permissions();
    tokio::fs::set_permissions(job.path(), std::fs::Permissions::from_mode(0o555))
        .await
        .unwrap();

    let store = UndoStore::new(store_dir.path());
    let result = store.stash(&victim, 1).await;

    // Restore permissions before asserting, so the tempdir can clean itself up.
    tokio::fs::set_permissions(job.path(), original_mode).await.unwrap();

    let landed_at = result.expect("copy succeeded, so there should be a backup path");
    assert_eq!(tokio::fs::read(&landed_at).await.unwrap(), "old content".as_bytes());
}

#[tokio::test]
async fn gives_up_instead_of_copying_a_symlink_when_rename_is_unavailable() {
    // Using permissions (555) to block rename also blocks deleting the original (unlink),
    // which mixes the cause together with the "only the original deletion fails" case. To test
    // purely the symlink dereference here, the working directory is put on /dev/shm so it lands
    // on a different filesystem than the stash location (the /tmp family) — rename genuinely
    // fails with EXDEV, but copy and delete permissions are untouched.
    let store_dir = tempfile::tempdir().unwrap();
    let job = tempfile::tempdir_in("/dev/shm").unwrap();
    let secret_store = tempfile::tempdir_in("/dev/shm").unwrap();
    let secret_file = secret_store.path().join("secret.txt");
    tokio::fs::write(&secret_file, "secret".as_bytes()).await.unwrap();

    let victim = job.path().join("link.pdf");
    std::os::unix::fs::symlink(&secret_file, &victim).unwrap();

    let store = UndoStore::new(store_dir.path());
    let result = store.stash(&victim, 1).await;

    assert!(result.is_none(), "moving a symlink via copy leaks the content it points to");
    assert!(victim.exists(), "if it gave up, the symlink must still be there");
}

#[tokio::test]
async fn moves_to_the_next_sequence_number_when_the_slot_is_already_taken() {
    // The sequence number (`AtomicU64`) is only unique within a process — the situation where
    // another process has already created a slot at the same undo root with the same
    // millisecond and sequence number can't be reproduced by pre-exhausting the sequence number
    // within the same process (the counter only ever moves forward). Instead, a "different
    // process" having already claimed a slot is simulated directly with a directory: run the
    // store once first to find out what slot name it will actually use, then pre-create the
    // very next slot.
    let store_dir = tempfile::tempdir().unwrap();
    let job = tempfile::tempdir().unwrap();
    let now_ms = 20_260_809_000; // An arbitrary value that doesn't collide with the other tests in this file.

    let decoy_victim = job.path().join("decoy.pdf");
    tokio::fs::write(&decoy_victim, "decoy".as_bytes()).await.unwrap();
    let store = UndoStore::new(store_dir.path());
    let decoy_dir = store.stash(&decoy_victim, now_ms).await.unwrap();

    // Pre-creates the name right after the slot the bait actually used, as if it were "a
    // backup another process already wrote."
    // **Claiming only one slot would keep this test from being a real tripwire.** The sequence
    // counter is global to the test binary, so if another test in the same file calls `stash`
    // concurrently, the counter advances further than we expect, and then the next `stash`
    // doesn't even land on the slot we claimed — even with the bug reverted, `assert_ne!` would
    // just hold and come out green (it actually happened 1 time in 5). So a **band** of slots
    // is claimed instead. Eight slots can't be jumped over by the handful of tests that run
    // concurrently.
    let decoy_parent = decoy_dir.parent().unwrap();
    let decoy_parent_name = decoy_parent.file_name().unwrap().to_str().unwrap();
    let (ms_part, seq_part) = decoy_parent_name.rsplit_once('-').unwrap();
    let decoy_seq: u64 = seq_part.parse().unwrap();
    let mut other_dirs = Vec::new();
    for seq in decoy_seq + 1..=decoy_seq + 8 {
        let dir = store_dir.path().join(format!("{ms_part}-{seq}"));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let file = dir.join("already-exists.txt");
        tokio::fs::write(&file, "another process's backup".as_bytes()).await.unwrap();
        other_dirs.push((dir, file));
    }

    let real_victim = job.path().join("real.pdf");
    tokio::fs::write(&real_victim, "real content".as_bytes()).await.unwrap();
    let real_dir = store.stash(&real_victim, now_ms).await.unwrap();

    for (dir, file) in &other_dirs {
        assert_ne!(
            real_dir.parent().unwrap(),
            dir.as_path(),
            "must not barge into an already-occupied slot — it must move to the next sequence number"
        );
        assert_eq!(
            tokio::fs::read(file).await.unwrap(),
            "another process's backup".as_bytes(),
            "someone else's backup must not be deleted or overwritten"
        );
    }
    assert_eq!(tokio::fs::read(&real_dir).await.unwrap(), "real content".as_bytes());
}
