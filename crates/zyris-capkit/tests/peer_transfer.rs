//! 전송 전 과정을 **소켓 없이** 본다. `zyris::testing::duplex`가 진짜 Connection 둘을 잇는다.

use std::time::Duration;

use sha2::{Digest, Sha256};
use zyris::{Node, NodeKind};
use zyris_caps::peer_transfer::{PeerTransfer, PeerTransferClient, PeerTransferServer, TransferOffer};
use zyris_capkit::transfer::{LocalPeerTransfer, TransferConfig};

fn 해시(바이트: &[u8]) -> String {
    hex::encode(Sha256::digest(바이트))
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

    let 설정 = TransferConfig {
        inbox: 받는_자리.path().to_path_buf(),
        undo: 되돌림.path().to_path_buf(),
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
