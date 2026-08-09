#![cfg(feature = "transfer")]

use std::os::unix::fs::PermissionsExt;

use zyris_capkit::transfer::undo::UndoStore;

#[tokio::test]
async fn moves_the_original_and_reports_where_it_went() {
    let 보관 = tempfile::tempdir().unwrap();
    let 작업 = tempfile::tempdir().unwrap();
    let 희생자 = 작업.path().join("a.pdf");
    tokio::fs::write(&희생자, "예전 것".as_bytes()).await.unwrap();

    let store = UndoStore::new(보관.path());
    let 간_자리 = store.stash(&희생자, 1_754_700_000_000).await.unwrap();

    assert!(!희생자.exists(), "should be a move, not a copy");
    assert_eq!(tokio::fs::read(&간_자리).await.unwrap(), "예전 것".as_bytes());
    assert!(간_자리.starts_with(보관.path()));
}

#[tokio::test]
async fn a_missing_file_has_nothing_to_move() {
    let 보관 = tempfile::tempdir().unwrap();
    let 작업 = tempfile::tempdir().unwrap();
    let store = UndoStore::new(보관.path());
    assert!(store.stash(&작업.path().join("없음"), 1).await.is_none());
}

#[tokio::test]
async fn returns_none_instead_of_panicking_when_stashing_fails() {
    // Give it a location it can't write to. If missing a safety net were allowed to block the
    // transfer, that would leave a state that can't be fixed.
    let 작업 = tempfile::tempdir().unwrap();
    let 희생자 = 작업.path().join("a.pdf");
    tokio::fs::write(&희생자, "x".as_bytes()).await.unwrap();

    let store = UndoStore::new("/proc/못쓰는자리");
    assert!(store.stash(&희생자, 1).await.is_none());
    assert!(희생자.exists(), "if it couldn't move it, the original must still be there");
}

#[tokio::test]
async fn does_not_lose_the_earlier_backup_when_the_same_millisecond_and_name_collide() {
    // Simulates two different peers sending the same name (a.pdf) in the same millisecond.
    // If the slots collide, the one that arrives later silently overwrites the earlier backup.
    let 보관 = tempfile::tempdir().unwrap();
    let 작업1 = tempfile::tempdir().unwrap();
    let 작업2 = tempfile::tempdir().unwrap();
    let 희생자1 = 작업1.path().join("a.pdf");
    let 희생자2 = 작업2.path().join("a.pdf");
    tokio::fs::write(&희생자1, "AAA".as_bytes()).await.unwrap();
    tokio::fs::write(&희생자2, "BBB".as_bytes()).await.unwrap();

    let store = UndoStore::new(보관.path());
    let 자리1 = store.stash(&희생자1, 1000).await.unwrap();
    let 자리2 = store.stash(&희생자2, 1000).await.unwrap();

    assert_ne!(자리1, 자리2, "slots must not collide even with the same millisecond and same name");
    assert_eq!(tokio::fs::read(&자리1).await.unwrap(), "AAA".as_bytes());
    assert_eq!(tokio::fs::read(&자리2).await.unwrap(), "BBB".as_bytes());
}

#[tokio::test]
async fn returns_the_already_made_backup_path_even_when_only_the_original_deletion_fails() {
    // Stripping write permission from the working directory blocks both rename (which needs to
    // change a directory entry) and deleting the original (unlink). The file's own read
    // permission is still intact, so copy goes through — copy succeeds, only the deletion
    // fails.
    let 보관 = tempfile::tempdir().unwrap();
    let 작업 = tempfile::tempdir().unwrap();
    let 희생자 = 작업.path().join("a.pdf");
    tokio::fs::write(&희생자, "예전 것".as_bytes()).await.unwrap();

    let 원래_권한 = tokio::fs::metadata(작업.path()).await.unwrap().permissions();
    tokio::fs::set_permissions(작업.path(), std::fs::Permissions::from_mode(0o555))
        .await
        .unwrap();

    let store = UndoStore::new(보관.path());
    let 결과 = store.stash(&희생자, 1).await;

    // Restore permissions before asserting, so the tempdir can clean itself up.
    tokio::fs::set_permissions(작업.path(), 원래_권한).await.unwrap();

    let 간_자리 = 결과.expect("copy succeeded, so there should be a backup path");
    assert_eq!(tokio::fs::read(&간_자리).await.unwrap(), "예전 것".as_bytes());
}

#[tokio::test]
async fn gives_up_instead_of_copying_a_symlink_when_rename_is_unavailable() {
    // Using permissions (555) to block rename also blocks deleting the original (unlink),
    // which mixes the cause together with the "only the original deletion fails" case. To test
    // purely the symlink dereference here, the working directory is put on /dev/shm so it lands
    // on a different filesystem than the stash location (the /tmp family) — rename genuinely
    // fails with EXDEV, but copy and delete permissions are untouched.
    let 보관 = tempfile::tempdir().unwrap();
    let 작업 = tempfile::tempdir_in("/dev/shm").unwrap();
    let 비밀_보관소 = tempfile::tempdir_in("/dev/shm").unwrap();
    let 비밀_파일 = 비밀_보관소.path().join("secret.txt");
    tokio::fs::write(&비밀_파일, "비밀".as_bytes()).await.unwrap();

    let 희생자 = 작업.path().join("link.pdf");
    std::os::unix::fs::symlink(&비밀_파일, &희생자).unwrap();

    let store = UndoStore::new(보관.path());
    let 결과 = store.stash(&희생자, 1).await;

    assert!(결과.is_none(), "moving a symlink via copy leaks the content it points to");
    assert!(희생자.exists(), "if it gave up, the symlink must still be there");
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
    let 보관 = tempfile::tempdir().unwrap();
    let 작업 = tempfile::tempdir().unwrap();
    let now_ms = 20_260_809_000; // An arbitrary value that doesn't collide with the other tests in this file.

    let 미끼_희생자 = 작업.path().join("미끼.pdf");
    tokio::fs::write(&미끼_희생자, "미끼".as_bytes()).await.unwrap();
    let store = UndoStore::new(보관.path());
    let 미끼_자리 = store.stash(&미끼_희생자, now_ms).await.unwrap();

    // Pre-creates the name right after the slot the bait actually used, as if it were "a
    // backup another process already wrote."
    // **Claiming only one slot would keep this test from being a real tripwire.** The sequence
    // counter is global to the test binary, so if another test in the same file calls `stash`
    // concurrently, the counter advances further than we expect, and then the next `stash`
    // doesn't even land on the slot we claimed — even with the bug reverted, `assert_ne!` would
    // just hold and come out green (it actually happened 1 time in 5). So a **band** of slots
    // is claimed instead. Eight slots can't be jumped over by the handful of tests that run
    // concurrently.
    let 미끼_부모 = 미끼_자리.parent().unwrap();
    let 미끼_부모_이름 = 미끼_부모.file_name().unwrap().to_str().unwrap();
    let (ms_부분, 순번_부분) = 미끼_부모_이름.rsplit_once('-').unwrap();
    let 미끼_순번: u64 = 순번_부분.parse().unwrap();
    let mut 남의_자리들 = Vec::new();
    for 순번 in 미끼_순번 + 1..=미끼_순번 + 8 {
        let 자리 = 보관.path().join(format!("{ms_부분}-{순번}"));
        tokio::fs::create_dir_all(&자리).await.unwrap();
        let 파일 = 자리.join("이미있음.txt");
        tokio::fs::write(&파일, "다른 프로세스의 백업".as_bytes()).await.unwrap();
        남의_자리들.push((자리, 파일));
    }

    let 진짜_희생자 = 작업.path().join("진짜.pdf");
    tokio::fs::write(&진짜_희생자, "진짜 내용".as_bytes()).await.unwrap();
    let 진짜_자리 = store.stash(&진짜_희생자, now_ms).await.unwrap();

    for (자리, 파일) in &남의_자리들 {
        assert_ne!(
            진짜_자리.parent().unwrap(),
            자리.as_path(),
            "must not barge into an already-occupied slot — it must move to the next sequence number"
        );
        assert_eq!(
            tokio::fs::read(파일).await.unwrap(),
            "다른 프로세스의 백업".as_bytes(),
            "someone else's backup must not be deleted or overwritten"
        );
    }
    assert_eq!(tokio::fs::read(&진짜_자리).await.unwrap(), "진짜 내용".as_bytes());
}
