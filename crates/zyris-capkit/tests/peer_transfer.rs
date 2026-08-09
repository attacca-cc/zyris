#![cfg(feature = "transfer")]

//! Watches the whole transfer flow **without a socket**. `zyris::testing::duplex` wires up two
//! real Connections.

use std::time::Duration;

use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use zyris::{Chunk, Node, NodeKind};
use zyris_caps::peer_transfer::{PeerTransfer, PeerTransferClient, PeerTransferServer, TransferOffer};
use zyris_capkit::transfer::{LocalPeerTransfer, TransferConfig, part_path};

fn 해시(바이트: &[u8]) -> String {
    hex::encode(Sha256::digest(바이트))
}

/// The `.part` slot this transfer will use.
///
/// **Tests must not hardcode this name as a string literal.** The `.part` name has a marker
/// folded in that's derived from `transfer_id` (so concurrent transfers with the same name don't
/// overwrite each other's `.part`). If the naming scheme changes and a test still lays down the
/// old name, a resume test can silently leak into the "there's no resume debris at all" path and
/// still come out green.
fn 부분_파일(받는_자리: &std::path::Path, transfer_id: &str, 이름: &str) -> std::path::PathBuf {
    part_path(&받는_자리.join("a").join(이름), transfer_id)
}

/// Wires up A (sender) and B (receiver). Each hands out only one `peer_transfer`.
///
/// **The order the handles get plugged in is the whole point of this helper.** For B to call
/// `pull` back, it needs a `PeerTransferClient`, which can only be built after `duplex` hands
/// back a Connection — but building the `Node` requires the receiving-side implementation to
/// exist **before** that. So it's built empty with `receiver_pending`, and filled in with
/// `set_peer` once the connection is up.
///
/// **`offer_file` is also pre-registered here.** A's (the sender's) `pull` looks up the
/// `transfer_id` that `push_offer` calls with in its own `pending` list — without registering it
/// first, it doesn't know what to hand out and rejects with "unknown transfer". The registered
/// `해시` (hash) is left free to differ from the actual file content because of the sha256
/// mismatch test: that test sets the sha256 in the offer passed to `push_offer` and the sha256
/// registered on A to **the same wrong value**, so it passes the head-side comparison `pull`
/// returns (`스트림.head.sha256 != offer.sha256`), and actually reaches the **later** check that
/// rehashes the bytes that were actually streamed and compares against that.
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
    // The handle can only be plugged in here — after the Connection exists and A has announced.
    let a_client: PeerTransferClient = b_conn.wait_capability(Duration::from_secs(2)).await.unwrap();
    받는_것.set_peer(a_client);
    (a_conn, b_conn)
}

#[tokio::test]
async fn file_lands_in_the_inbox_unchanged() {
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
async fn discards_what_it_received_when_sha256_does_not_match() {
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
    // The sha256 registered on A is also set to **the exact same wrong value** as what's passed
    // to push_offer. That way it passes `pull`'s head-side comparison (the earlier check for
    // whether it's about to hand out something different from what it declared), and actually
    // reaches the later check (what this test really watches) that rehashes the bytes actually
    // received and compares them.
    let 틀린_해시 = 해시("다른 내용".as_bytes());
    let (a_conn, _b) =
        붙인다("t2", &원본, 내용.len() as u64, &틀린_해시, 설정).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let 결과 = b
        .push_offer(TransferOffer {
            transfer_id: "t2".into(),
            name: "a.bin".into(),
            size: 내용.len() as u64,
            sha256: 틀린_해시.clone(), // deliberately wrong value
            overwrite: false,
        })
        .await;

    assert!(결과.is_err(), "succeeded despite the mismatch");
    // If a partial file is left behind, the next resume will pick it up and never match.
    let mut 남은_것 = tokio::fs::read_dir(받는_자리.path().join("a")).await;
    if let Ok(ref mut d) = 남은_것 {
        assert!(d.next_entry().await.unwrap().is_none(), "a partial file was left behind");
    }
}

#[tokio::test]
async fn does_not_overwrite_an_existing_file_when_overwrite_is_false() {
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
    // This test finishes at the "already exists but overwrite is off" check before `pull` is
    // ever called, so the registered value itself is never actually used — but a normal value
    // is passed anyway to satisfy the helper's contract.
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
        "overwrote it even though it was set not to"
    );
}

#[tokio::test]
async fn overwrite_true_replaces_and_moves_the_original_to_the_undo_slot() {
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
    let 되돌릴_것 = 결과.undo.expect("if it overwrote, there must be an undo slot");
    assert_eq!(tokio::fs::read(&되돌릴_것).await.unwrap(), "예전 것".as_bytes());

    // An overwrite that can be undone and one that loses the original forever are distinguished
    // only by `undo` in the audit log.
    let 글 = tokio::fs::read_to_string(&감사_길).await.unwrap();
    let 줄: serde_json::Value = serde_json::from_str(글.lines().next().unwrap()).unwrap();
    assert_eq!(줄["replaced"], true);
    assert_eq!(줄["undo"], 되돌릴_것, "the log must record what was moved and to where");
}

#[tokio::test]
async fn rejects_a_file_that_exceeds_the_limit() {
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
    // The size-limit check happens first, before `pull` is ever called, so the registered value
    // is never actually used.
    let (a_conn, _b) =
        붙인다("t5", &원본, "작다".len() as u64, &해시("작다".as_bytes()), 설정).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    // It must reject without consuming a single byte — it decides based only on the declared
    // size.
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

/// Audit scope — outside the brief but an approved addition to scope: when `audit` is
/// configured, each successful transfer must leave one line, and that line must be parseable
/// JSON pointing at the slot that was actually used.
#[tokio::test]
async fn audit_configured_leaves_one_line_per_transfer() {
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
    assert_eq!(줄들.len(), 1, "exactly one line must be left for one successful transfer");
    let 파싱: serde_json::Value =
        serde_json::from_str(줄들[0]).expect("the audit line must be parseable JSON");
    assert_eq!(파싱["written"], 결과.written);
}

/// Review F2: `with_extension("part")` **changes** the extension — if the proposed name already
/// ends in `.part`, the temp path and the destination become the same slot. Then the file that
/// was already there gets mistaken for "resume debris," and the failure cleanup (`remove_file`)
/// ends up deleting the destination itself.
#[tokio::test]
async fn temp_path_does_not_collide_with_the_destination_even_when_the_name_already_ends_in_part() {
    let 원본_자리 = tempfile::tempdir().unwrap();
    let 받는_자리 = tempfile::tempdir().unwrap();
    let 되돌림 = tempfile::tempdir().unwrap();
    let 새_내용 = "BRAND-NEW-CONTENT".as_bytes().to_vec();
    let 원본 = 원본_자리.path().join("x.part");
    tokio::fs::write(&원본, &새_내용).await.unwrap();

    // The receiving side already has a file with the same name — old content unrelated to this
    // transfer.
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
    let 되돌릴_것 = 결과.undo.expect(
        "if it overwrote, there must be an undo slot — this shouldn't happen if the temp path collides with the destination",
    );
    assert_eq!(tokio::fs::read(&되돌릴_것).await.unwrap(), "OLD-ORIGINAL".as_bytes());
}

/// A 255-byte name where the peer has **precomputed the suffix and planted it at the end of the
/// name.**
///
/// `part_path`'s suffix `.<sha256(transfer_id)[..16]>.part` comes from `transfer_id`, and the
/// sender picks both `transfer_id` and `name` — no permission at all is needed to construct a
/// name like this.
fn 꼬리를_심은_이름(transfer_id: &str) -> String {
    let 표식 = hex::encode(Sha256::digest(transfer_id.as_bytes()));
    let 꼬리 = format!(".{}.part", &표식[..16]);
    format!("{}{}", "A".repeat(255 - 꼬리.len()), 꼬리)
}

/// Review N1 (Important): because `part_path` truncates the stem to fit the 255-byte limit, for
/// the name above the truncated result **becomes the same as** the original name — `임시 ==
/// 목적지` (temp == destination).
///
/// Then the destination that was already there gets caught as resume debris (`파일_길이` becomes
/// the destination's length), the hash can't possibly match, and the mismatch cleanup
/// `remove_file(&임시)` **deletes the destination itself.** On that branch, neither an undo
/// backup nor an audit line is left behind — the one spot where all three lines of defense are
/// empty at once.
#[tokio::test]
async fn does_not_delete_the_existing_original_even_with_a_planted_suffix_name() {
    let 원본_자리 = tempfile::tempdir().unwrap();
    let 받는_자리 = tempfile::tempdir().unwrap();
    let 되돌림 = tempfile::tempdir().unwrap();
    let 이름 = 꼬리를_심은_이름("t-evil");
    let 내용: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
    let 원본 = 원본_자리.path().join("payload.bin");
    tokio::fs::write(&원본, &내용).await.unwrap();

    // The destination already has an original that must not be deleted. Its length has to be
    // shorter than the offer's for it to take the branch where it gets mistaken for "resume
    // debris."
    tokio::fs::create_dir_all(받는_자리.path().join("a")).await.unwrap();
    let 목적지 = 받는_자리.path().join("a").join(&이름);
    tokio::fs::write(&목적지, "IRREPLACEABLE-ORIGINAL".as_bytes()).await.unwrap();

    let 설정 = TransferConfig {
        inbox: 받는_자리.path().to_path_buf(),
        undo: 되돌림.path().to_path_buf(),
        ..TransferConfig::default()
    };
    let (a_conn, _b) =
        붙인다("t-evil", &원본, 내용.len() as u64, &해시(&내용), 설정).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let 결과 = b
        .push_offer(TransferOffer {
            transfer_id: "t-evil".into(),
            name: 이름.clone(),
            size: 내용.len() as u64,
            sha256: 해시(&내용),
            overwrite: true,
        })
        .await;

    // Surface what went wrong first — if they collide, the destination is deleted **before**
    // the response fails.
    assert!(
        목적지.exists(),
        "failure cleanup deleted the destination itself (temp collided with the destination)"
    );
    let 결과 = 결과.expect("if they don't collide, this is just an ordinary overwrite");
    assert!(결과.replaced);
    assert_eq!(결과.written, 목적지.display().to_string());
    assert_eq!(tokio::fs::read(&목적지).await.unwrap(), 내용);
    let 되돌릴_것 = 결과.undo.expect("if it overwrote, the original must be in the undo slot");
    assert_eq!(
        tokio::fs::read(&되돌릴_것).await.unwrap(),
        "IRREPLACEABLE-ORIGINAL".as_bytes(),
        "the original must be preserved intact"
    );
}

/// Review F1 (Critical): when leftover `.part` debris is larger than this offer's size, the
/// offset was reset to 0, but the file itself was never deleted or truncated — it was opened
/// with `.append(true)` and the new bytes were appended **after** the debris. The hasher only
/// sees the newly received bytes, so the sha256 comparison passes, and what's left on disk is a
/// different file from the one that was verified.
#[tokio::test]
async fn deletes_and_restarts_from_scratch_when_resume_debris_is_larger_than_the_offer() {
    let 원본_자리 = tempfile::tempdir().unwrap();
    let 받는_자리 = tempfile::tempdir().unwrap();
    let 되돌림 = tempfile::tempdir().unwrap();
    let 내용 = "NEW".as_bytes().to_vec();
    let 원본 = 원본_자리.path().join("data.txt");
    tokio::fs::write(&원본, &내용).await.unwrap();

    // The receiving side already has `.part` debris unrelated to this transfer, much larger than
    // the offer's size (3 bytes) — for example, the remnant of an old large transfer that got
    // cut off.
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
        "the debris must not stay in front and turn this into a different file"
    );
}

/// Review F4: loading the resumed prefix into memory all at once allocates straight up to the
/// limit (8 GiB by default) — on this machine (3.6GB RAM), one resume is an OOM. Correctness
/// alone can't distinguish a monolithic read from a chunked rehash (both feed the hasher the
/// same bytes in the same order), so this test only checks that "the result is right" — memory
/// usage itself is confirmed by code review (whether it repeatedly reads with a fixed-size
/// buffer). Before F2 was fixed, this path wasn't even exercised because the `.part` name was
/// different (review F3) — now `resume.bin.part` is actually used as the resume starting point.
#[tokio::test]
async fn resumed_prefix_plus_the_rest_reassembles_into_the_original() {
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

/// Review 3-2: the `.part` slot is outside `Inbox::resolve`'s check (that only looks at
/// `목적지`, the destination). push_offer must reject a symlink planted there in advance that
/// points at a target outside the jail, and must not touch the target. Two layers — the upfront
/// check (`symlink_metadata`) and `O_NOFOLLOW` (unix) — guard against this.
#[cfg(unix)]
#[tokio::test]
async fn rejects_a_symlink_planted_at_the_temp_slot_and_leaves_the_target_untouched() {
    let 원본_자리 = tempfile::tempdir().unwrap();
    let 받는_자리 = tempfile::tempdir().unwrap();
    let 되돌림 = tempfile::tempdir().unwrap();
    let 표적_자리 = tempfile::tempdir().unwrap();
    let 표적 = 표적_자리.path().join("victim.txt");
    tokio::fs::write(&표적, "UNTOUCHED").await.unwrap();

    let 내용 = "payload".as_bytes().to_vec();
    let 원본 = 원본_자리.path().join("linked.bin");
    tokio::fs::write(&원본, &내용).await.unwrap();

    // Plants a symlink at the `.part` slot in advance, pointing outside the jail (at the
    // target).
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

    assert!(결과.is_err(), "succeeded even though the temp slot was a symlink");
    assert_eq!(tokio::fs::read(&표적).await.unwrap(), b"UNTOUCHED");
}

/// Whether a file large enough to need multiple chunks actually arrives.
///
/// **Without this test, nobody noticed the feature was completely broken.** The largest payload
/// in the rest of the tests is 9,000 bytes, which fits entirely in one chunk, so the path that
/// crosses into a second chunk was never exercised even once. That path had a permanent hang in
/// it — `청크` (chunk) is the same size as the protocol's credit window, so after serialization
/// it overran the window, and the sender got stuck on the first chunk.
///
/// So **the size is the whole point.** Keep the payload solidly bigger than a chunk. If `청크`
/// grows, this value has to grow with it.
#[tokio::test]
async fn a_file_spanning_multiple_chunks_arrives_completely() {
    let 원본_자리 = tempfile::tempdir().unwrap();
    let 받는_자리 = tempfile::tempdir().unwrap();
    let 되돌림 = tempfile::tempdir().unwrap();
    // Splits into roughly eleven pieces at 64 KiB per chunk.
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

    // It's a hanging bug, so a plain assert won't catch it — it only shows up as a failure once
    // a timeout is applied.
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
    .expect("did not finish within 20 seconds — a multi-chunk transfer hangs")
    .unwrap();

    assert_eq!(결과.bytes, 내용.len() as u64);
    assert_eq!(tokio::fs::read(&결과.written).await.unwrap(), 내용);
}

/// Distinguishes whether the receiving side **actually resumes**.
///
/// The prefix of the sender's file and the prefix already received differ from each other (only
/// the length matches). The proposed sha256 is that of `이미_받은_앞부분 ++ 뒷부분`
/// (already-received-prefix ++ rest). So:
///
/// - If it resumes → the prefix is left as-is and it pulls from 100_000 onward → the hash
///   matches → success
/// - If it starts from scratch → the sender's prefix (0xAA) arrives → the hash doesn't match →
///   failure
///
/// The sender holding content different from the hash it proposed doesn't happen in reality, but
/// it's the only way to isolate the receiving side's decision on its own.
#[tokio::test]
async fn resumes_a_partially_received_file() {
    let 원본_자리 = tempfile::tempdir().unwrap();
    let 받는_자리 = tempfile::tempdir().unwrap();
    let 되돌림 = tempfile::tempdir().unwrap();

    let 이미_받은_앞부분: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let 보내는_쪽_앞부분: Vec<u8> = vec![0xAAu8; 100_000];
    let 뒷부분: Vec<u8> = (0..200_000u32).map(|i| ((i % 241) as u8) ^ 0x5A).collect();
    assert_ne!(
        이미_받은_앞부분,
        보내는_쪽_앞부분,
        "if the two prefixes were the same, this test couldn't distinguish anything"
    );

    let mut 보내는_쪽_내용 = 보내는_쪽_앞부분.clone();
    보내는_쪽_내용.extend_from_slice(&뒷부분);
    let 원본 = 원본_자리.path().join("a.bin");
    tokio::fs::write(&원본, &보내는_쪽_내용).await.unwrap();

    // The final content that can only result if it actually resumed.
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
async fn restarts_from_scratch_when_the_partial_data_does_not_match_reality() {
    let 원본_자리 = tempfile::tempdir().unwrap();
    let 받는_자리 = tempfile::tempdir().unwrap();
    let 되돌림 = tempfile::tempdir().unwrap();
    let 내용: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
    let 원본 = 원본_자리.path().join("a.bin");
    tokio::fs::write(&원본, &내용).await.unwrap();

    // Garbage prefix. If it resumes, the sha256 must not match — otherwise the bug slips
    // through silently.
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

    // The first attempt fails on the mismatch and deletes the partial file.
    assert!(결과.is_err());
    // Calling it again must receive from scratch and succeed.
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

/// Isolates whether `pull` really streams starting from `offset` on the sending side, apart
/// from the receiving side's decision.
///
/// The two tests above only distinguish **the receiving side's decision** (the `받은_offset`
/// calculation) — even if the sender ignored `offset` and always streamed from the start, the
/// final result could still come out right if the receiving side stitches the prefix it already
/// had together with the bytes that streamed in well enough (or they happen to match). So
/// `pull` is called directly here to look at the sending side alone.
///
/// `pull` is answered by the sending side (A). Waiting on `wait_capability` from the `b_conn`
/// (B's connection) that `붙인다` returns gets the handle A announced — waiting on `a_conn`
/// instead gets B's (the receiver's) handle, and B has never received `offer_file`, so calling
/// `pull` on it gets rejected with "unknown transfer".
#[tokio::test]
async fn pull_streams_starting_from_the_offset() {
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
        "head.size must be the whole file size, not the value with offset subtracted"
    );

    let mut 받은: Vec<u8> = Vec::new();
    while let Some(조각) = 스트림.items.next().await {
        let Chunk(바이트) = 조각.unwrap();
        받은.extend_from_slice(&바이트);
    }
    assert_eq!(받은, 내용[100_000..], "bytes must match from 100_000 to the end");
}
