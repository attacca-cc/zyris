use std::os::unix::fs::PermissionsExt;

use zyris_capkit::transfer::undo::UndoStore;

#[tokio::test]
async fn 원본을_옮기고_간_자리를_알려_준다() {
    let 보관 = tempfile::tempdir().unwrap();
    let 작업 = tempfile::tempdir().unwrap();
    let 희생자 = 작업.path().join("a.pdf");
    tokio::fs::write(&희생자, "예전 것".as_bytes()).await.unwrap();

    let store = UndoStore::new(보관.path());
    let 간_자리 = store.stash(&희생자, 1_754_700_000_000).await.unwrap();

    assert!(!희생자.exists(), "복사가 아니라 이동이어야 한다");
    assert_eq!(tokio::fs::read(&간_자리).await.unwrap(), "예전 것".as_bytes());
    assert!(간_자리.starts_with(보관.path()));
}

#[tokio::test]
async fn 없는_파일은_옮길_것이_없다() {
    let 보관 = tempfile::tempdir().unwrap();
    let 작업 = tempfile::tempdir().unwrap();
    let store = UndoStore::new(보관.path());
    assert!(store.stash(&작업.path().join("없음"), 1).await.is_none());
}

#[tokio::test]
async fn 보관에_실패해도_none일_뿐_패닉하지_않는다() {
    // 쓸 수 없는 자리를 준다. 안전망이 없다고 전송을 막으면 고칠 수 없는 상태가 생긴다.
    let 작업 = tempfile::tempdir().unwrap();
    let 희생자 = 작업.path().join("a.pdf");
    tokio::fs::write(&희생자, "x".as_bytes()).await.unwrap();

    let store = UndoStore::new("/proc/못쓰는자리");
    assert!(store.stash(&희생자, 1).await.is_none());
    assert!(희생자.exists(), "못 옮겼으면 원본은 그대로 있어야 한다");
}

#[tokio::test]
async fn 같은_밀리초_같은_이름이_와도_먼저_온_백업을_잃지_않는다() {
    // 서로 다른 두 피어가 같은 밀리초에 같은 이름(a.pdf)으로 보내오는 상황을 흉내 낸다.
    // 자리가 겹치면 뒤에 온 것이 앞선 백업을 조용히 덮어써 버린다.
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

    assert_ne!(자리1, 자리2, "같은 밀리초·같은 이름이라도 자리가 겹치면 안 된다");
    assert_eq!(tokio::fs::read(&자리1).await.unwrap(), "AAA".as_bytes());
    assert_eq!(tokio::fs::read(&자리2).await.unwrap(), "BBB".as_bytes());
}

#[tokio::test]
async fn 원본_삭제만_실패해도_이미_만든_백업_경로를_돌려준다() {
    // 작업 디렉터리에서 쓰기 권한을 없애면 rename도(디렉터리 항목 변경 필요), 원본 삭제(unlink)도
    // 막힌다. 파일 자체의 읽기 권한은 남아 있어 copy는 통과한다 — copy는 성공, 삭제만 실패다.
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

    // tempdir가 스스로를 지울 수 있도록 단언 전에 권한부터 되돌린다.
    tokio::fs::set_permissions(작업.path(), 원래_권한).await.unwrap();

    let 간_자리 = 결과.expect("copy는 성공했으니 백업 경로가 있어야 한다");
    assert_eq!(tokio::fs::read(&간_자리).await.unwrap(), "예전 것".as_bytes());
}

#[tokio::test]
async fn 심링크는_rename이_안_되면_복사하지_않고_포기한다() {
    // rename을 막는 데 권한(555)을 쓰면 원본 삭제(unlink)까지 같이 막혀서 "원본 삭제만 실패"
    // 케이스와 원인이 뒤섞인다. 여기서는 순수하게 심링크 역참조만 검증하려고, 작업 디렉터리를
    // /dev/shm에 둬서 보관 자리(/tmp 계열)와 다른 파일시스템이 되게 한다 — 진짜 EXDEV로
    // rename이 실패하지만 copy·삭제 권한은 멀쩡하다.
    let 보관 = tempfile::tempdir().unwrap();
    let 작업 = tempfile::tempdir_in("/dev/shm").unwrap();
    let 비밀_보관소 = tempfile::tempdir_in("/dev/shm").unwrap();
    let 비밀_파일 = 비밀_보관소.path().join("secret.txt");
    tokio::fs::write(&비밀_파일, "비밀".as_bytes()).await.unwrap();

    let 희생자 = 작업.path().join("link.pdf");
    std::os::unix::fs::symlink(&비밀_파일, &희생자).unwrap();

    let store = UndoStore::new(보관.path());
    let 결과 = store.stash(&희생자, 1).await;

    assert!(결과.is_none(), "심링크는 copy로 옮기면 가리키는 내용이 새어 나간다");
    assert!(희생자.exists(), "포기했으면 심링크는 그대로 남아 있어야 한다");
}
