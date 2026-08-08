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

#[tokio::test]
async fn 자리가_이미_있으면_다음_순번으로_넘어간다() {
    // 순번(`AtomicU64`)은 프로세스 안에서만 유일하다 — 다른 프로세스가 같은 undo 루트에
    // 같은 밀리초·같은 순번으로 이미 자리를 만들어 둔 상황은 같은 프로세스 안에서 순번을
    // 미리 소진시키는 식으로는 재현되지 않는다(카운터가 계속 앞으로 갈 뿐이다). 대신 "다른
    // 프로세스"가 이미 자리를 차지해 둔 것을 디렉터리로 직접 흉내 낸다: store를 한 번 먼저
    // 돌려 실제로 쓸 자리 이름을 알아낸 뒤, 바로 다음 자리를 미리 만들어 둔다.
    let 보관 = tempfile::tempdir().unwrap();
    let 작업 = tempfile::tempdir().unwrap();
    let now_ms = 20_260_809_000; // 이 파일의 다른 테스트와 안 겹치는 임의의 값.

    let 미끼_희생자 = 작업.path().join("미끼.pdf");
    tokio::fs::write(&미끼_희생자, "미끼".as_bytes()).await.unwrap();
    let store = UndoStore::new(보관.path());
    let 미끼_자리 = store.stash(&미끼_희생자, now_ms).await.unwrap();

    // 미끼가 실제로 쓴 자리 바로 다음 이름을, "다른 프로세스가 이미 써 둔 백업"처럼
    // 미리 만들어 둔다.
    let 미끼_부모 = 미끼_자리.parent().unwrap();
    let 미끼_부모_이름 = 미끼_부모.file_name().unwrap().to_str().unwrap();
    let (ms_부분, 순번_부분) = 미끼_부모_이름.rsplit_once('-').unwrap();
    let 다음_순번: u64 = 순번_부분.parse().unwrap();
    let 남의_자리 = 보관.path().join(format!("{ms_부분}-{}", 다음_순번 + 1));
    tokio::fs::create_dir_all(&남의_자리).await.unwrap();
    let 남의_파일 = 남의_자리.join("이미있음.txt");
    tokio::fs::write(&남의_파일, "다른 프로세스의 백업".as_bytes()).await.unwrap();

    let 진짜_희생자 = 작업.path().join("진짜.pdf");
    tokio::fs::write(&진짜_희생자, "진짜 내용".as_bytes()).await.unwrap();
    let 진짜_자리 = store.stash(&진짜_희생자, now_ms).await.unwrap();

    assert_ne!(
        진짜_자리.parent().unwrap(),
        남의_자리,
        "이미 있는 자리를 그대로 밀고 들어가면 안 된다 — 다음 순번으로 넘어가야 한다"
    );
    assert_eq!(
        tokio::fs::read(&남의_파일).await.unwrap(),
        "다른 프로세스의 백업".as_bytes(),
        "남의 백업이 지워지거나 덮이면 안 된다"
    );
    assert_eq!(tokio::fs::read(&진짜_자리).await.unwrap(), "진짜 내용".as_bytes());
}
