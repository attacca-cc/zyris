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
async fn 보관에_실패해도_None일_뿐_패닉하지_않는다() {
    // 쓸 수 없는 자리를 준다. 안전망이 없다고 전송을 막으면 고칠 수 없는 상태가 생긴다.
    let 작업 = tempfile::tempdir().unwrap();
    let 희생자 = 작업.path().join("a.pdf");
    tokio::fs::write(&희생자, b"x").await.unwrap();

    let store = UndoStore::new("/proc/못쓰는자리");
    assert!(store.stash(&희생자, 1).await.is_none());
    assert!(희생자.exists(), "못 옮겼으면 원본은 그대로 있어야 한다");
}
