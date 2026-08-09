#![cfg(feature = "transfer")]

//! 전송 전 과정을 **소켓 없이** 본다. `zyris::testing::duplex`가 진짜 Connection 둘을 잇는다.

use std::time::Duration;

use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use zyris::{Chunk, Node, NodeKind};
use zyris_caps::peer_transfer::{PeerTransfer, PeerTransferClient, PeerTransferServer, TransferOffer};
use zyris_capkit::transfer::{LocalPeerTransfer, TransferConfig, part_path};

fn 해시(바이트: &[u8]) -> String {
    hex::encode(Sha256::digest(바이트))
}

/// 이번 전송이 쓸 `.part` 자리.
///
/// **테스트가 이 이름을 문자열로 깔면 안 된다.** `.part` 이름에는 `transfer_id`에서 유도한
/// 표식이 섞여 있다(같은 이름의 동시 전송이 서로의 `.part`를 덮지 않도록). 이름 규칙이
/// 바뀌었는데 테스트가 옛 이름을 깔면, 이어받기 테스트가 "이어받을 부스러기가 아예 없는"
/// 경로로 조용히 새어 나가면서도 초록이 난다.
fn 부분_파일(받는_자리: &std::path::Path, transfer_id: &str, 이름: &str) -> std::path::PathBuf {
    part_path(&받는_자리.join("a").join(이름), transfer_id)
}

/// A(보내는 쪽)와 B(받는 쪽)를 잇는다. 둘 다 `peer_transfer` 하나만 내준다.
///
/// **손잡이 꽂는 순서가 이 헬퍼의 요점이다.** B가 `pull`을 되부르려면 `PeerTransferClient`가
/// 있어야 하는데 그것은 `duplex`가 Connection을 준 뒤에야 만들 수 있고, `Node`를 만들려면
/// 받는 쪽 구현체가 그보다 **먼저** 있어야 한다. 그래서 `receiver_pending`으로 빈 채 만들고
/// 연결이 선 다음 `set_peer`로 채운다.
///
/// **`offer_file`도 여기서 미리 등록해 둔다.** A(보내는 쪽)의 `pull`은 `push_offer`가 부르는
/// `transfer_id`를 자기 `pending` 목록에서 찾는다 — 등록해 두지 않으면 무엇을 내줄지 몰라
/// "모르는 전송입니다"로 거절한다. 등록할 `해시`가 실제 파일 내용과 다를 수도 있게 열어 둔
/// 이유는 sha256 불일치 테스트 때문이다: 그 테스트는 `push_offer`에 건네는 offer의 sha256과
/// A에 등록해 둔 sha256을 **똑같이 틀리게** 맞춰서, `pull`이 돌려주는 head 쪽 대조
/// (`스트림.head.sha256 != offer.sha256`)를 통과시키고 실제로 스트리밍된 바이트를 재해시해
/// 대조하는 **뒤쪽** 검사까지 가 닿게 한다.
async fn 붙인다(
    transfer_id: &str,
    보낼_것: &std::path::Path,
    등록할_크기: u64,
    등록할_해시: &str,
    설정: TransferConfig,
) -> (zyris::Connection, zyris::Connection) {
    let 받는_것 = LocalPeerTransfer::receiver_pending(설정, "a".into());
    let 보내는_것 = LocalPeerTransfer::sender(보낼_것.parent().unwrap().to_path_buf());
    보내는_것
        .offer_file(transfer_id.to_string(), 보낼_것.to_path_buf(), 등록할_크기, 등록할_해시.to_string())
        .await;
    let a = Node::builder()
        .name("a")
        .kind(NodeKind::Cli)
        .capability(PeerTransferServer(보내는_것))
        .build()
        .unwrap();
    let b = Node::builder()
        .name("b")
        .kind(NodeKind::Cli)
        .capability(PeerTransferServer(받는_것.clone()))
        .build()
        .unwrap();
    let (a_conn, b_conn) = zyris::testing::duplex(&a, &b).await.unwrap();
    // 손잡이는 여기서만 꽂을 수 있다 — Connection이 생기고 A가 announce한 뒤다.
    let a_client: PeerTransferClient = b_conn.wait_capability(Duration::from_secs(2)).await.unwrap();
    받는_것.set_peer(a_client);
    (a_conn, b_conn)
}

#[tokio::test]
async fn 파일이_inbox에_그대로_도착한다() {
    let 원본_자리 = tempfile::tempdir().unwrap();
    let 받는_자리 = tempfile::tempdir().unwrap();
    let 되돌림 = tempfile::tempdir().unwrap();
    let 내용 = b"hello p2p".repeat(1000);
    let 원본 = 원본_자리.path().join("report.pdf");
    tokio::fs::write(&원본, &내용).await.unwrap();

    let 설정 = TransferConfig {
        inbox: 받는_자리.path().to_path_buf(),
        undo: 되돌림.path().to_path_buf(),
        ..TransferConfig::default()
    };
    let (a_conn, _b_conn) =
        붙인다("t1", &원본, 내용.len() as u64, &해시(&내용), 설정).await;

    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();
    let 결과 = b
        .push_offer(TransferOffer {
            transfer_id: "t1".into(),
            name: "report.pdf".into(),
            size: 내용.len() as u64,
            sha256: 해시(&내용),
            overwrite: false,
        })
        .await
        .unwrap();

    assert_eq!(결과.bytes, 내용.len() as u64);
    assert_eq!(결과.sha256, 해시(&내용));
    assert!(!결과.replaced);
    assert_eq!(tokio::fs::read(&결과.written).await.unwrap(), 내용);
    assert!(std::path::Path::new(&결과.written).starts_with(받는_자리.path()));
}

#[tokio::test]
async fn sha256이_안_맞으면_받은_것을_버린다() {
    let 원본_자리 = tempfile::tempdir().unwrap();
    let 받는_자리 = tempfile::tempdir().unwrap();
    let 되돌림 = tempfile::tempdir().unwrap();
    let 내용 = "진짜 내용".as_bytes().to_vec();
    let 원본 = 원본_자리.path().join("a.bin");
    tokio::fs::write(&원본, &내용).await.unwrap();

    let 설정 = TransferConfig {
        inbox: 받는_자리.path().to_path_buf(),
        undo: 되돌림.path().to_path_buf(),
        ..TransferConfig::default()
    };
    // A에 등록해 두는 sha256도 push_offer에 건네는 것과 **똑같이 틀리게** 맞춘다. 그래야
    // `pull`의 head 대조(선언과 다른 것을 내주려는지 보는 앞쪽 검사)를 통과해서, 실제로 받은
    // 바이트를 재해시해 대조하는 뒤쪽 검사(이 테스트가 실제로 보려는 것)까지 가 닿는다.
    let 틀린_해시 = 해시("다른 내용".as_bytes());
    let (a_conn, _b) =
        붙인다("t2", &원본, 내용.len() as u64, &틀린_해시, 설정).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let 결과 = b
        .push_offer(TransferOffer {
            transfer_id: "t2".into(),
            name: "a.bin".into(),
            size: 내용.len() as u64,
            sha256: 틀린_해시.clone(), // 일부러 틀린 값
            overwrite: false,
        })
        .await;

    assert!(결과.is_err(), "불일치인데 성공했다");
    // 부분 파일을 남기면 다음 재개가 그것을 이어받아 영영 안 맞는다.
    let mut 남은_것 = tokio::fs::read_dir(받는_자리.path().join("a")).await;
    if let Ok(ref mut d) = 남은_것 {
        assert!(d.next_entry().await.unwrap().is_none(), "부분 파일이 남았다");
    }
}

#[tokio::test]
async fn overwrite가_false면_있는_파일을_안_덮는다() {
    let 원본_자리 = tempfile::tempdir().unwrap();
    let 받는_자리 = tempfile::tempdir().unwrap();
    let 되돌림 = tempfile::tempdir().unwrap();
    let 내용 = "새 것".as_bytes().to_vec();
    let 원본 = 원본_자리.path().join("a.txt");
    tokio::fs::write(&원본, &내용).await.unwrap();

    tokio::fs::create_dir_all(받는_자리.path().join("a")).await.unwrap();
    tokio::fs::write(받는_자리.path().join("a").join("a.txt"), "예전 것".as_bytes()).await.unwrap();

    let 설정 = TransferConfig {
        inbox: 받는_자리.path().to_path_buf(),
        undo: 되돌림.path().to_path_buf(),
        ..TransferConfig::default()
    };
    // 이 테스트는 "이미 있는데 overwrite가 꺼져 있다" 검사에서 `pull`을 부르기도 전에
    // 끝나므로 등록 값 자체는 실제로 안 쓰이지만, 헬퍼 계약을 맞추려 정상 값을 넘긴다.
    let (a_conn, _b) =
        붙인다("t3", &원본, 내용.len() as u64, &해시(&내용), 설정).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let 결과 = b
        .push_offer(TransferOffer {
            transfer_id: "t3".into(),
            name: "a.txt".into(),
            size: 내용.len() as u64,
            sha256: 해시(&내용),
            overwrite: false,
        })
        .await;

    assert!(결과.is_err());
    assert_eq!(
        tokio::fs::read(받는_자리.path().join("a").join("a.txt")).await.unwrap(),
        "예전 것".as_bytes(),
        "덮지 않기로 했는데 덮었다"
    );
}

#[tokio::test]
async fn overwrite가_true면_덮고_원본을_되돌림_자리로_옮긴다() {
    let 원본_자리 = tempfile::tempdir().unwrap();
    let 받는_자리 = tempfile::tempdir().unwrap();
    let 되돌림 = tempfile::tempdir().unwrap();
    let 내용 = "새 것".as_bytes().to_vec();
    let 원본 = 원본_자리.path().join("a.txt");
    tokio::fs::write(&원본, &내용).await.unwrap();

    tokio::fs::create_dir_all(받는_자리.path().join("a")).await.unwrap();
    tokio::fs::write(받는_자리.path().join("a").join("a.txt"), "예전 것".as_bytes()).await.unwrap();

    let 감사_자리 = tempfile::tempdir().unwrap();
    let 감사_길 = 감사_자리.path().join("transfers.log");
    let 설정 = TransferConfig {
        inbox: 받는_자리.path().to_path_buf(),
        undo: 되돌림.path().to_path_buf(),
        audit: Some(감사_길.clone()),
        ..TransferConfig::default()
    };
    let (a_conn, _b) =
        붙인다("t4", &원본, 내용.len() as u64, &해시(&내용), 설정).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let 결과 = b
        .push_offer(TransferOffer {
            transfer_id: "t4".into(),
            name: "a.txt".into(),
            size: 내용.len() as u64,
            sha256: 해시(&내용),
            overwrite: true,
        })
        .await
        .unwrap();

    assert!(결과.replaced);
    assert_eq!(tokio::fs::read(&결과.written).await.unwrap(), 내용);
    let 되돌릴_것 = 결과.undo.expect("덮었으면 되돌림 자리가 있어야 한다");
    assert_eq!(tokio::fs::read(&되돌릴_것).await.unwrap(), "예전 것".as_bytes());

    // 되돌릴 수 있게 덮은 것과 원본을 영영 잃은 것은 감사 로그에서 `undo`로만 갈린다.
    let 글 = tokio::fs::read_to_string(&감사_길).await.unwrap();
    let 줄: serde_json::Value = serde_json::from_str(글.lines().next().unwrap()).unwrap();
    assert_eq!(줄["replaced"], true);
    assert_eq!(줄["undo"], 되돌릴_것, "무엇을 어디로 치웠는지가 로그에 남아야 한다");
}

#[tokio::test]
async fn 상한을_넘는_파일은_거부한다() {
    let 원본_자리 = tempfile::tempdir().unwrap();
    let 받는_자리 = tempfile::tempdir().unwrap();
    let 되돌림 = tempfile::tempdir().unwrap();
    let 원본 = 원본_자리.path().join("big.bin");
    tokio::fs::write(&원본, "작다".as_bytes()).await.unwrap();

    let 설정 = TransferConfig {
        inbox: 받는_자리.path().to_path_buf(),
        undo: 되돌림.path().to_path_buf(),
        max_file_bytes: 10,
        ..TransferConfig::default()
    };
    // 크기 상한 검사는 `pull`을 부르기 전 가장 먼저 일어나므로 등록 값은 실제로 안 쓰인다.
    let (a_conn, _b) =
        붙인다("t5", &원본, "작다".len() as u64, &해시("작다".as_bytes()), 설정).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    // 바이트를 한 개도 안 쓰고 거절해야 한다 — 선언된 크기만 보고 판단한다.
    let 결과 = b
        .push_offer(TransferOffer {
            transfer_id: "t5".into(),
            name: "big.bin".into(),
            size: 99_999,
            sha256: 해시("뭐든".as_bytes()),
            overwrite: false,
        })
        .await;
    assert!(결과.is_err());
}

/// 감사 범위 — 브리프 밖이지만 승인된 추가 범위: `audit`이 설정되면 성공한 전송마다 한 줄이
/// 남고, 그 줄이 실제로 쓰인 자리를 가리키는 파싱 가능한 JSON이어야 한다.
#[tokio::test]
async fn audit이_설정되면_전송_한_줄이_남는다() {
    let 원본_자리 = tempfile::tempdir().unwrap();
    let 받는_자리 = tempfile::tempdir().unwrap();
    let 되돌림 = tempfile::tempdir().unwrap();
    let 감사_자리 = tempfile::tempdir().unwrap();
    let 내용 = "감사 대상".as_bytes().to_vec();
    let 원본 = 원본_자리.path().join("audited.txt");
    tokio::fs::write(&원본, &내용).await.unwrap();

    let 감사_길 = 감사_자리.path().join("transfers.log");
    let 설정 = TransferConfig {
        inbox: 받는_자리.path().to_path_buf(),
        undo: 되돌림.path().to_path_buf(),
        audit: Some(감사_길.clone()),
        ..TransferConfig::default()
    };
    let (a_conn, _b) =
        붙인다("t6", &원본, 내용.len() as u64, &해시(&내용), 설정).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let 결과 = b
        .push_offer(TransferOffer {
            transfer_id: "t6".into(),
            name: "audited.txt".into(),
            size: 내용.len() as u64,
            sha256: 해시(&내용),
            overwrite: false,
        })
        .await
        .unwrap();

    let 글 = tokio::fs::read_to_string(&감사_길).await.unwrap();
    let 줄들: Vec<&str> = 글.lines().collect();
    assert_eq!(줄들.len(), 1, "성공한 전송 하나에 한 줄만 남아야 한다");
    let 파싱: serde_json::Value =
        serde_json::from_str(줄들[0]).expect("감사 줄이 파싱 가능한 JSON이어야 한다");
    assert_eq!(파싱["written"], 결과.written);
}

/// 리뷰 F2: `with_extension("part")`는 확장자를 **바꾼다** — 제안된 이름이 이미 `.part`로
/// 끝나면 임시 경로와 목적지가 같은 자리가 된다. 그러면 이미 있던 파일이 "이어받기 부스러기"로
/// 오인되고, 실패 정리(`remove_file`)가 곧 목적지 자체를 지운다.
#[tokio::test]
async fn 이름이_이미_part로_끝나도_임시_경로가_목적지와_안_겹친다() {
    let 원본_자리 = tempfile::tempdir().unwrap();
    let 받는_자리 = tempfile::tempdir().unwrap();
    let 되돌림 = tempfile::tempdir().unwrap();
    let 새_내용 = "BRAND-NEW-CONTENT".as_bytes().to_vec();
    let 원본 = 원본_자리.path().join("x.part");
    tokio::fs::write(&원본, &새_내용).await.unwrap();

    // 받는 쪽에 같은 이름의 파일이 이미 있다 — 이번 전송과는 무관한 옛 내용이다.
    tokio::fs::create_dir_all(받는_자리.path().join("a")).await.unwrap();
    tokio::fs::write(받는_자리.path().join("a").join("x.part"), "OLD-ORIGINAL".as_bytes())
        .await
        .unwrap();

    let 설정 = TransferConfig {
        inbox: 받는_자리.path().to_path_buf(),
        undo: 되돌림.path().to_path_buf(),
        ..TransferConfig::default()
    };
    let (a_conn, _b) =
        붙인다("t7", &원본, 새_내용.len() as u64, &해시(&새_내용), 설정).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let 결과 = b
        .push_offer(TransferOffer {
            transfer_id: "t7".into(),
            name: "x.part".into(),
            size: 새_내용.len() as u64,
            sha256: 해시(&새_내용),
            overwrite: true,
        })
        .await
        .unwrap();

    assert_eq!(tokio::fs::read(&결과.written).await.unwrap(), 새_내용);
    assert_eq!(결과.sha256, 해시(&새_내용));
    assert!(결과.replaced);
    let 되돌릴_것 = 결과.undo.expect("덮었으면 되돌림 자리가 있어야 한다 — 임시가 목적지와 겹치면 안 생긴다");
    assert_eq!(tokio::fs::read(&되돌릴_것).await.unwrap(), "OLD-ORIGINAL".as_bytes());
}

/// 리뷰 F1(Critical): 남은 `.part`가 이번 offer의 크기보다 크면 offset은 0으로 되돌리면서도
/// 파일 자체는 지우지도 자르지도 않았다 — `.append(true)`로 열어서 새 바이트를 부스러기
/// **뒤에** 이어 붙였다. 해시기는 새로 받은 바이트만 보므로 sha256 대조는 통과하고, 디스크에는
/// 검증한 것과 다른 파일이 남는다.
#[tokio::test]
async fn 이어받기_부스러기가_offer보다_크면_지우고_처음부터_받는다() {
    let 원본_자리 = tempfile::tempdir().unwrap();
    let 받는_자리 = tempfile::tempdir().unwrap();
    let 되돌림 = tempfile::tempdir().unwrap();
    let 내용 = "NEW".as_bytes().to_vec();
    let 원본 = 원본_자리.path().join("data.txt");
    tokio::fs::write(&원본, &내용).await.unwrap();

    // 받는 쪽에 이번 전송과 무관한, offer.size(3바이트)보다 훨씬 큰 부스러기 `.part`가
    // 이미 있다 — 예를 들어 예전의 큰 전송이 끊긴 자리다.
    tokio::fs::create_dir_all(받는_자리.path().join("a")).await.unwrap();
    tokio::fs::write(
        부분_파일(받는_자리.path(), "t8", "data.txt"),
        "STALE-GARBAGE-LONGER-THAN-NEW".as_bytes(),
    )
    .await
    .unwrap();

    let 설정 = TransferConfig {
        inbox: 받는_자리.path().to_path_buf(),
        undo: 되돌림.path().to_path_buf(),
        ..TransferConfig::default()
    };
    let (a_conn, _b) =
        붙인다("t8", &원본, 내용.len() as u64, &해시(&내용), 설정).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let 결과 = b
        .push_offer(TransferOffer {
            transfer_id: "t8".into(),
            name: "data.txt".into(),
            size: 내용.len() as u64,
            sha256: 해시(&내용),
            overwrite: false,
        })
        .await
        .unwrap();

    assert_eq!(결과.bytes, 내용.len() as u64);
    assert_eq!(결과.sha256, 해시(&내용));
    assert_eq!(
        tokio::fs::read(&결과.written).await.unwrap(),
        내용,
        "부스러기가 앞에 그대로 남아 다른 파일이 되면 안 된다"
    );
}

/// 리뷰 F4: 이어받기 앞부분을 통째로 메모리에 올리면 상한(기본 8 GiB)까지 그대로 할당된다 —
/// 이 머신(RAM 3.6GB)에서는 재개 한 번이 OOM이다. 정확성만으로는 통짜 읽기와 청크 재해시를
/// 구분할 수 없으니(둘 다 같은 바이트를 같은 순서로 해시에 넣는다), 이 테스트는 "결과가
/// 옳다"만 본다 — 메모리 사용량 자체은 코드 리뷰(고정 크기 버퍼로 반복 read하는지)로 확인한다.
/// F2가 고쳐지기 전에는 `.part` 이름이 달라 이 경로를 아예 타지 않았다(리뷰 F3) — 지금은
/// `resume.bin.part`가 실제로 이어받기 시작점으로 쓰인다.
#[tokio::test]
async fn 이어받기_앞부분과_나머지가_합쳐져_원본과_같아진다() {
    let 원본_자리 = tempfile::tempdir().unwrap();
    let 받는_자리 = tempfile::tempdir().unwrap();
    let 되돌림 = tempfile::tempdir().unwrap();
    let 내용 = "가나다라마바사아자차카타파하".repeat(50).into_bytes();
    let 원본 = 원본_자리.path().join("resume.bin");
    tokio::fs::write(&원본, &내용).await.unwrap();

    let 이미_받은_길이 = 내용.len() / 3;
    tokio::fs::create_dir_all(받는_자리.path().join("a")).await.unwrap();
    tokio::fs::write(부분_파일(받는_자리.path(), "t9", "resume.bin"), &내용[..이미_받은_길이])
        .await
        .unwrap();

    let 설정 = TransferConfig {
        inbox: 받는_자리.path().to_path_buf(),
        undo: 되돌림.path().to_path_buf(),
        ..TransferConfig::default()
    };
    let (a_conn, _b) =
        붙인다("t9", &원본, 내용.len() as u64, &해시(&내용), 설정).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let 결과 = b
        .push_offer(TransferOffer {
            transfer_id: "t9".into(),
            name: "resume.bin".into(),
            size: 내용.len() as u64,
            sha256: 해시(&내용),
            overwrite: false,
        })
        .await
        .unwrap();

    assert_eq!(결과.bytes, 내용.len() as u64);
    assert_eq!(결과.sha256, 해시(&내용));
    assert_eq!(tokio::fs::read(&결과.written).await.unwrap(), 내용);
}

/// 리뷰 3-2: `.part` 자리는 `Inbox::resolve`의 확인 밖이다(그건 `목적지`만 본다). 거기 미리
/// 심어 둔, 감옥 밖 표적을 가리키는 심링크를 push_offer가 거부해야 하고 표적은 손대지 않아야
/// 한다. 사전 검사(`symlink_metadata`)와 `O_NOFOLLOW`(unix) 두 겹이 이걸 막는다.
#[cfg(unix)]
#[tokio::test]
async fn 임시_자리에_심어_둔_심링크는_거부하고_표적을_안_건드린다() {
    let 원본_자리 = tempfile::tempdir().unwrap();
    let 받는_자리 = tempfile::tempdir().unwrap();
    let 되돌림 = tempfile::tempdir().unwrap();
    let 표적_자리 = tempfile::tempdir().unwrap();
    let 표적 = 표적_자리.path().join("victim.txt");
    tokio::fs::write(&표적, "UNTOUCHED").await.unwrap();

    let 내용 = "payload".as_bytes().to_vec();
    let 원본 = 원본_자리.path().join("linked.bin");
    tokio::fs::write(&원본, &내용).await.unwrap();

    // `.part` 자리에 감옥 밖(표적)을 가리키는 심링크를 미리 심어 둔다.
    tokio::fs::create_dir_all(받는_자리.path().join("a")).await.unwrap();
    std::os::unix::fs::symlink(&표적, 부분_파일(받는_자리.path(), "t10", "linked.bin")).unwrap();

    let 설정 = TransferConfig {
        inbox: 받는_자리.path().to_path_buf(),
        undo: 되돌림.path().to_path_buf(),
        ..TransferConfig::default()
    };
    let (a_conn, _b) =
        붙인다("t10", &원본, 내용.len() as u64, &해시(&내용), 설정).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let 결과 = b
        .push_offer(TransferOffer {
            transfer_id: "t10".into(),
            name: "linked.bin".into(),
            size: 내용.len() as u64,
            sha256: 해시(&내용),
            overwrite: false,
        })
        .await;

    assert!(결과.is_err(), "임시 자리가 심링크인데 성공했다");
    assert_eq!(tokio::fs::read(&표적).await.unwrap(), b"UNTOUCHED");
}

/// 청크를 여러 개 써야 하는 크기의 파일이 실제로 도착하는지.
///
/// **이 테스트가 없어서 기능이 통째로 안 되는 것을 아무도 못 봤다.** 나머지 테스트의 가장 큰
/// 짐이 9,000바이트라 전부 청크 하나에 들어갔고, 두 번째 청크로 넘어가는 길은 한 번도 지나간
/// 적이 없었다. 그 길에 영구 정지가 있었다 — `청크`가 프로토콜의 credit 창과 같은 크기라
/// 직렬화 뒤 창을 넘겼고, 보내는 쪽이 첫 청크에서 막혔다.
///
/// 그래서 **크기가 요점이다.** 짐을 청크보다 확실히 크게 유지할 것. `청크`를 키우면 이 값도
/// 같이 키워야 한다.
#[tokio::test]
async fn 청크_여러_개짜리_파일도_끝까지_도착한다() {
    let 원본_자리 = tempfile::tempdir().unwrap();
    let 받는_자리 = tempfile::tempdir().unwrap();
    let 되돌림 = tempfile::tempdir().unwrap();
    // 64 KiB 청크로 열한 조각쯤 난다.
    let 내용: Vec<u8> = (0..700_000u32).map(|i| (i % 251) as u8).collect();
    let 원본 = 원본_자리.path().join("big.bin");
    tokio::fs::write(&원본, &내용).await.unwrap();

    let 설정 = TransferConfig {
        inbox: 받는_자리.path().to_path_buf(),
        undo: 되돌림.path().to_path_buf(),
        ..TransferConfig::default()
    };
    let (a_conn, _b) = 붙인다("big", &원본, 내용.len() as u64, &해시(&내용), 설정).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    // 멈추는 버그라 assert로는 안 잡힌다 — 시간을 걸어야 실패로 나타난다.
    let 결과 = tokio::time::timeout(
        Duration::from_secs(20),
        b.push_offer(TransferOffer {
            transfer_id: "big".into(),
            name: "big.bin".into(),
            size: 내용.len() as u64,
            sha256: 해시(&내용),
            overwrite: false,
        }),
    )
    .await
    .expect("20초 안에 안 끝났다 — 청크가 여러 개인 전송이 멈춘다")
    .unwrap();

    assert_eq!(결과.bytes, 내용.len() as u64);
    assert_eq!(tokio::fs::read(&결과.written).await.unwrap(), 내용);
}

/// 받는 쪽이 **정말로 이어받는지**를 가른다.
///
/// 보내는 쪽 파일의 앞부분과 이미 받아 둔 앞부분이 서로 다르다(길이만 같다). 제안하는
/// sha256은 `이미_받은_앞부분 ++ 뒷부분`의 것이다. 그래서:
///
/// - 이어받으면 → 앞부분을 그대로 두고 100_000부터 당긴다 → 해시가 맞는다 → 성공
/// - 처음부터 받으면 → 보내는 쪽 앞부분(0xAA)이 온다 → 해시가 틀린다 → 실패
///
/// 보내는 쪽이 제안한 해시와 다른 내용을 갖고 있는 것은 현실에서 일어나지 않지만,
/// 받는 쪽의 판단만 떼어 보려면 이 방법뿐이다.
#[tokio::test]
async fn 받다_만_파일을_이어받는다() {
    let 원본_자리 = tempfile::tempdir().unwrap();
    let 받는_자리 = tempfile::tempdir().unwrap();
    let 되돌림 = tempfile::tempdir().unwrap();

    let 이미_받은_앞부분: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let 보내는_쪽_앞부분: Vec<u8> = vec![0xAAu8; 100_000];
    let 뒷부분: Vec<u8> = (0..200_000u32).map(|i| ((i % 241) as u8) ^ 0x5A).collect();
    assert_ne!(이미_받은_앞부분, 보내는_쪽_앞부분, "두 앞부분이 같으면 이 테스트는 아무것도 못 가른다");

    let mut 보내는_쪽_내용 = 보내는_쪽_앞부분.clone();
    보내는_쪽_내용.extend_from_slice(&뒷부분);
    let 원본 = 원본_자리.path().join("a.bin");
    tokio::fs::write(&원본, &보내는_쪽_내용).await.unwrap();

    // 이어받았을 때에만 나올 수 있는 최종 내용.
    let mut 기대 = 이미_받은_앞부분.clone();
    기대.extend_from_slice(&뒷부분);

    tokio::fs::create_dir_all(받는_자리.path().join("a")).await.unwrap();
    tokio::fs::write(부분_파일(받는_자리.path(), "resume", "a.bin"), &이미_받은_앞부분)
        .await
        .unwrap();

    let 설정 = TransferConfig {
        inbox: 받는_자리.path().to_path_buf(),
        undo: 되돌림.path().to_path_buf(),
        ..TransferConfig::default()
    };
    let (a_conn, _b) =
        붙인다("resume", &원본, 기대.len() as u64, &해시(&기대), 설정).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let 결과 = b
        .push_offer(TransferOffer {
            transfer_id: "resume".into(),
            name: "a.bin".into(),
            size: 기대.len() as u64,
            sha256: 해시(&기대),
            overwrite: false,
        })
        .await
        .unwrap();

    assert_eq!(결과.bytes, 기대.len() as u64);
    assert_eq!(tokio::fs::read(&결과.written).await.unwrap(), 기대);
}

#[tokio::test]
async fn 받다_만_것이_실제와_다르면_처음부터_받는다() {
    let 원본_자리 = tempfile::tempdir().unwrap();
    let 받는_자리 = tempfile::tempdir().unwrap();
    let 되돌림 = tempfile::tempdir().unwrap();
    let 내용: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
    let 원본 = 원본_자리.path().join("a.bin");
    tokio::fs::write(&원본, &내용).await.unwrap();

    // 쓰레기 앞부분. 이어받으면 sha256이 안 맞아야 한다 — 그래야 버그가 조용히 안 지나간다.
    tokio::fs::create_dir_all(받는_자리.path().join("a")).await.unwrap();
    tokio::fs::write(부분_파일(받는_자리.path(), "bad-resume", "a.bin"), vec![0xFFu8; 10_000])
        .await
        .unwrap();

    let 설정 = TransferConfig {
        inbox: 받는_자리.path().to_path_buf(),
        undo: 되돌림.path().to_path_buf(),
        ..TransferConfig::default()
    };
    let (a_conn, _b) =
        붙인다("bad-resume", &원본, 내용.len() as u64, &해시(&내용), 설정).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let 결과 = b
        .push_offer(TransferOffer {
            transfer_id: "bad-resume".into(),
            name: "a.bin".into(),
            size: 내용.len() as u64,
            sha256: 해시(&내용),
            overwrite: false,
        })
        .await;

    // 첫 시도는 불일치로 실패하고 부분 파일을 지운다.
    assert!(결과.is_err());
    // 다시 부르면 처음부터 받아 성공해야 한다.
    let 두_번째 = b
        .push_offer(TransferOffer {
            transfer_id: "bad-resume".into(),
            name: "a.bin".into(),
            size: 내용.len() as u64,
            sha256: 해시(&내용),
            overwrite: false,
        })
        .await
        .unwrap();
    assert_eq!(tokio::fs::read(&두_번째.written).await.unwrap(), 내용);
}

/// `pull`이 보내는 쪽에서 정말로 `offset`부터 흘리는지를 받는 쪽 판단과 떼어 본다.
///
/// 위 두 테스트는 **받는 쪽의 판단**(`받은_offset` 계산)만 가른다 — 보내는 쪽이 `offset`을
/// 무시하고 항상 처음부터 흘려도, 받는 쪽이 이미 갖고 있던 앞부분과 흘러온 바이트를 잘 짜맞추면
/// (혹은 우연히 같으면) 최종 결과가 맞아떨어질 수 있다. 그래서 `pull`을 직접 불러 보내는
/// 쪽만 따로 본다.
///
/// `pull`은 보내는 쪽(A)이 답한다. `붙인다`가 돌려주는 `b_conn`(B의 연결)에서
/// `wait_capability`를 기다리면 A가 announce한 손잡이가 나온다 — `a_conn`에서 기다리면
/// 반대로 B(받는 쪽)의 손잡이가 나오는데, B는 `offer_file`을 받은 적이 없어 `pull`을 불러도
/// "모르는 전송입니다"로 거절한다.
#[tokio::test]
async fn pull은_offset부터_흘린다() {
    let 원본_자리 = tempfile::tempdir().unwrap();
    let 받는_자리 = tempfile::tempdir().unwrap();
    let 되돌림 = tempfile::tempdir().unwrap();
    let 내용: Vec<u8> = (0..300_000u32).map(|i| (i % 253) as u8).collect();
    let 원본 = 원본_자리.path().join("straight.bin");
    tokio::fs::write(&원본, &내용).await.unwrap();

    let 설정 = TransferConfig {
        inbox: 받는_자리.path().to_path_buf(),
        undo: 되돌림.path().to_path_buf(),
        ..TransferConfig::default()
    };
    let (_a_conn, b_conn) =
        붙인다("straight", &원본, 내용.len() as u64, &해시(&내용), 설정).await;
    let a: PeerTransferClient = b_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let mut 스트림 = a.pull("straight".to_string(), 100_000).await.unwrap();
    assert_eq!(
        스트림.head.size,
        내용.len() as u64,
        "head.size는 offset을 뺀 값이 아니라 파일 전체 크기여야 한다"
    );

    let mut 받은: Vec<u8> = Vec::new();
    while let Some(조각) = 스트림.items.next().await {
        let Chunk(바이트) = 조각.unwrap();
        받은.extend_from_slice(&바이트);
    }
    assert_eq!(받은, 내용[100_000..], "100_000부터 끝까지와 바이트가 같아야 한다");
}
