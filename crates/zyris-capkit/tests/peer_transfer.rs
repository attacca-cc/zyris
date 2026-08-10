#![cfg(feature = "transfer")]

//! Watches the whole transfer flow **without a socket**. `zyris::testing::duplex` wires up two
//! real Connections.

use std::time::Duration;

use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use zyris::{Chunk, Node, NodeKind};
use zyris_caps::peer_transfer::{PeerTransfer, PeerTransferClient, PeerTransferServer, TransferOffer};
use zyris_capkit::transfer::{LocalPeerTransfer, TransferConfig, part_path};

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// The `.part` slot this transfer will use.
///
/// **Tests must not hardcode this name as a string literal.** The `.part` name has a marker
/// folded in that's derived from `transfer_id` (so concurrent transfers with the same name don't
/// overwrite each other's `.part`). If the naming scheme changes and a test still lays down the
/// old name, a resume test can silently leak into the "there's no resume debris at all" path and
/// still come out green.
fn partial_file(inbox_dir: &std::path::Path, transfer_id: &str, name: &str) -> std::path::PathBuf {
    part_path(&inbox_dir.join("a").join(name), transfer_id)
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
/// `hash` is left free to differ from the actual file content because of the sha256
/// mismatch test: that test sets the sha256 in the offer passed to `push_offer` and the sha256
/// registered on A to **the same wrong value**, so it passes the head-side comparison `pull`
/// returns (`stream.head.sha256 != offer.sha256`), and actually reaches the **later** check that
/// rehashes the bytes that were actually streamed and compares against that.
async fn wire_up_peers(
    transfer_id: &str,
    to_send: &std::path::Path,
    declared_size: u64,
    declared_hash: &str,
    config: TransferConfig,
) -> (zyris::Connection, zyris::Connection) {
    let receiver = LocalPeerTransfer::receiver_pending(config, "a".into());
    let sender = LocalPeerTransfer::sender(to_send.parent().unwrap().to_path_buf());
    sender
        .offer_file(transfer_id.to_string(), to_send.to_path_buf(), declared_size, declared_hash.to_string())
        .await;
    let a = Node::builder()
        .name("a")
        .kind(NodeKind::Cli)
        .capability(PeerTransferServer(sender))
        .build()
        .unwrap();
    let b = Node::builder()
        .name("b")
        .kind(NodeKind::Cli)
        .capability(PeerTransferServer(receiver.clone()))
        .build()
        .unwrap();
    let (a_conn, b_conn) = zyris::testing::duplex(&a, &b).await.unwrap();
    // The handle can only be plugged in here — after the Connection exists and A has announced.
    let a_client: PeerTransferClient = b_conn.wait_capability(Duration::from_secs(2)).await.unwrap();
    receiver.set_peer(a_client);
    (a_conn, b_conn)
}

#[tokio::test]
async fn file_lands_in_the_inbox_unchanged() {
    let source_dir = tempfile::tempdir().unwrap();
    let inbox_dir = tempfile::tempdir().unwrap();
    let undo = tempfile::tempdir().unwrap();
    let content = b"hello p2p".repeat(1000);
    let source = source_dir.path().join("report.pdf");
    tokio::fs::write(&source, &content).await.unwrap();

    let config = TransferConfig {
        inbox: inbox_dir.path().to_path_buf(),
        undo: undo.path().to_path_buf(),
        ..TransferConfig::default()
    };
    let (a_conn, _b_conn) =
        wire_up_peers("t1", &source, content.len() as u64, &hash(&content), config).await;

    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();
    let result = b
        .push_offer(TransferOffer {
            transfer_id: "t1".into(),
            name: "report.pdf".into(),
            size: content.len() as u64,
            sha256: hash(&content),
            overwrite: false,
        })
        .await
        .unwrap();

    assert_eq!(result.bytes, content.len() as u64);
    assert_eq!(result.sha256, hash(&content));
    assert!(!result.replaced);
    assert_eq!(tokio::fs::read(&result.written).await.unwrap(), content);
    assert!(std::path::Path::new(&result.written).starts_with(inbox_dir.path()));
}

#[tokio::test]
async fn discards_what_it_received_when_sha256_does_not_match() {
    let source_dir = tempfile::tempdir().unwrap();
    let inbox_dir = tempfile::tempdir().unwrap();
    let undo = tempfile::tempdir().unwrap();
    let content = "real content".as_bytes().to_vec();
    let source = source_dir.path().join("a.bin");
    tokio::fs::write(&source, &content).await.unwrap();

    let config = TransferConfig {
        inbox: inbox_dir.path().to_path_buf(),
        undo: undo.path().to_path_buf(),
        ..TransferConfig::default()
    };
    // The sha256 registered on A is also set to **the exact same wrong value** as what's passed
    // to push_offer. That way it passes `pull`'s head-side comparison (the earlier check for
    // whether it's about to hand out something different from what it declared), and actually
    // reaches the later check (what this test really watches) that rehashes the bytes actually
    // received and compares them.
    let wrong_hash = hash("different content".as_bytes());
    let (a_conn, _b) =
        wire_up_peers("t2", &source, content.len() as u64, &wrong_hash, config).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let result = b
        .push_offer(TransferOffer {
            transfer_id: "t2".into(),
            name: "a.bin".into(),
            size: content.len() as u64,
            sha256: wrong_hash.clone(), // deliberately wrong value
            overwrite: false,
        })
        .await;

    assert!(result.is_err(), "succeeded despite the mismatch");
    // If a partial file is left behind, the next resume will pick it up and never match.
    let mut remaining = tokio::fs::read_dir(inbox_dir.path().join("a")).await;
    if let Ok(ref mut d) = remaining {
        assert!(d.next_entry().await.unwrap().is_none(), "a partial file was left behind");
    }
}

#[tokio::test]
async fn does_not_overwrite_an_existing_file_when_overwrite_is_false() {
    let source_dir = tempfile::tempdir().unwrap();
    let inbox_dir = tempfile::tempdir().unwrap();
    let undo = tempfile::tempdir().unwrap();
    let content = "new content".as_bytes().to_vec();
    let source = source_dir.path().join("a.txt");
    tokio::fs::write(&source, &content).await.unwrap();

    tokio::fs::create_dir_all(inbox_dir.path().join("a")).await.unwrap();
    tokio::fs::write(inbox_dir.path().join("a").join("a.txt"), "old content".as_bytes()).await.unwrap();

    let config = TransferConfig {
        inbox: inbox_dir.path().to_path_buf(),
        undo: undo.path().to_path_buf(),
        ..TransferConfig::default()
    };
    // This test finishes at the "already exists but overwrite is off" check before `pull` is
    // ever called, so the registered value itself is never actually used — but a normal value
    // is passed anyway to satisfy the helper's contract.
    let (a_conn, _b) =
        wire_up_peers("t3", &source, content.len() as u64, &hash(&content), config).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let result = b
        .push_offer(TransferOffer {
            transfer_id: "t3".into(),
            name: "a.txt".into(),
            size: content.len() as u64,
            sha256: hash(&content),
            overwrite: false,
        })
        .await;

    assert!(result.is_err());
    assert_eq!(
        tokio::fs::read(inbox_dir.path().join("a").join("a.txt")).await.unwrap(),
        "old content".as_bytes(),
        "overwrote it even though it was set not to"
    );
}

#[tokio::test]
async fn overwrite_true_replaces_and_moves_the_original_to_the_undo_slot() {
    let source_dir = tempfile::tempdir().unwrap();
    let inbox_dir = tempfile::tempdir().unwrap();
    let undo = tempfile::tempdir().unwrap();
    let content = "new content".as_bytes().to_vec();
    let source = source_dir.path().join("a.txt");
    tokio::fs::write(&source, &content).await.unwrap();

    tokio::fs::create_dir_all(inbox_dir.path().join("a")).await.unwrap();
    tokio::fs::write(inbox_dir.path().join("a").join("a.txt"), "old content".as_bytes()).await.unwrap();

    let audit_dir = tempfile::tempdir().unwrap();
    let audit_path = audit_dir.path().join("transfers.log");
    let config = TransferConfig {
        inbox: inbox_dir.path().to_path_buf(),
        undo: undo.path().to_path_buf(),
        audit: Some(audit_path.clone()),
        ..TransferConfig::default()
    };
    let (a_conn, _b) =
        wire_up_peers("t4", &source, content.len() as u64, &hash(&content), config).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let result = b
        .push_offer(TransferOffer {
            transfer_id: "t4".into(),
            name: "a.txt".into(),
            size: content.len() as u64,
            sha256: hash(&content),
            overwrite: true,
        })
        .await
        .unwrap();

    assert!(result.replaced);
    assert_eq!(tokio::fs::read(&result.written).await.unwrap(), content);
    let to_undo = result.undo.expect("if it overwrote, there must be an undo slot");
    assert_eq!(tokio::fs::read(&to_undo).await.unwrap(), "old content".as_bytes());

    // An overwrite that can be undone and one that loses the original forever are distinguished
    // only by `undo` in the audit log.
    let text = tokio::fs::read_to_string(&audit_path).await.unwrap();
    let line: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(line["replaced"], true);
    assert_eq!(line["undo"], to_undo, "the log must record what was moved and to where");
}

#[tokio::test]
async fn rejects_a_file_that_exceeds_the_limit() {
    let source_dir = tempfile::tempdir().unwrap();
    let inbox_dir = tempfile::tempdir().unwrap();
    let undo = tempfile::tempdir().unwrap();
    let source = source_dir.path().join("big.bin");
    tokio::fs::write(&source, "small".as_bytes()).await.unwrap();

    let config = TransferConfig {
        inbox: inbox_dir.path().to_path_buf(),
        undo: undo.path().to_path_buf(),
        max_file_bytes: 10,
        ..TransferConfig::default()
    };
    // The size-limit check happens first, before `pull` is ever called, so the registered value
    // is never actually used.
    let (a_conn, _b) =
        wire_up_peers("t5", &source, "small".len() as u64, &hash("small".as_bytes()), config).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    // It must reject without consuming a single byte — it decides based only on the declared
    // size.
    let result = b
        .push_offer(TransferOffer {
            transfer_id: "t5".into(),
            name: "big.bin".into(),
            size: 99_999,
            sha256: hash("whatever".as_bytes()),
            overwrite: false,
        })
        .await;
    assert!(result.is_err());
}

/// Audit scope — outside the brief but an approved addition to scope: when `audit` is
/// configured, each successful transfer must leave one line, and that line must be parseable
/// JSON pointing at the slot that was actually used.
#[tokio::test]
async fn audit_configured_leaves_one_line_per_transfer() {
    let source_dir = tempfile::tempdir().unwrap();
    let inbox_dir = tempfile::tempdir().unwrap();
    let undo = tempfile::tempdir().unwrap();
    let audit_dir = tempfile::tempdir().unwrap();
    let content = "audited content".as_bytes().to_vec();
    let source = source_dir.path().join("audited.txt");
    tokio::fs::write(&source, &content).await.unwrap();

    let audit_path = audit_dir.path().join("transfers.log");
    let config = TransferConfig {
        inbox: inbox_dir.path().to_path_buf(),
        undo: undo.path().to_path_buf(),
        audit: Some(audit_path.clone()),
        ..TransferConfig::default()
    };
    let (a_conn, _b) =
        wire_up_peers("t6", &source, content.len() as u64, &hash(&content), config).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let result = b
        .push_offer(TransferOffer {
            transfer_id: "t6".into(),
            name: "audited.txt".into(),
            size: content.len() as u64,
            sha256: hash(&content),
            overwrite: false,
        })
        .await
        .unwrap();

    let text = tokio::fs::read_to_string(&audit_path).await.unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1, "exactly one line must be left for one successful transfer");
    let parsed: serde_json::Value =
        serde_json::from_str(lines[0]).expect("the audit line must be parseable JSON");
    assert_eq!(parsed["written"], result.written);
}

/// Review F2: `with_extension("part")` **changes** the extension — if the proposed name already
/// ends in `.part`, the temp path and the destination become the same slot. Then the file that
/// was already there gets mistaken for "resume debris," and the failure cleanup (`remove_file`)
/// ends up deleting the destination itself.
#[tokio::test]
async fn temp_path_does_not_collide_with_the_destination_even_when_the_name_already_ends_in_part() {
    let source_dir = tempfile::tempdir().unwrap();
    let inbox_dir = tempfile::tempdir().unwrap();
    let undo = tempfile::tempdir().unwrap();
    let new_content = "BRAND-NEW-CONTENT".as_bytes().to_vec();
    let source = source_dir.path().join("x.part");
    tokio::fs::write(&source, &new_content).await.unwrap();

    // The receiving side already has a file with the same name — old content unrelated to this
    // transfer.
    tokio::fs::create_dir_all(inbox_dir.path().join("a")).await.unwrap();
    tokio::fs::write(inbox_dir.path().join("a").join("x.part"), "OLD-ORIGINAL".as_bytes())
        .await
        .unwrap();

    let config = TransferConfig {
        inbox: inbox_dir.path().to_path_buf(),
        undo: undo.path().to_path_buf(),
        ..TransferConfig::default()
    };
    let (a_conn, _b) =
        wire_up_peers("t7", &source, new_content.len() as u64, &hash(&new_content), config).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let result = b
        .push_offer(TransferOffer {
            transfer_id: "t7".into(),
            name: "x.part".into(),
            size: new_content.len() as u64,
            sha256: hash(&new_content),
            overwrite: true,
        })
        .await
        .unwrap();

    assert_eq!(tokio::fs::read(&result.written).await.unwrap(), new_content);
    assert_eq!(result.sha256, hash(&new_content));
    assert!(result.replaced);
    let to_undo = result.undo.expect(
        "if it overwrote, there must be an undo slot — this shouldn't happen if the temp path collides with the destination",
    );
    assert_eq!(tokio::fs::read(&to_undo).await.unwrap(), "OLD-ORIGINAL".as_bytes());
}

/// A 255-byte name where the peer has **precomputed the suffix and planted it at the end of the
/// name.**
///
/// `part_path`'s suffix `.<sha256(transfer_id)[..16]>.part` comes from `transfer_id`, and the
/// sender picks both `transfer_id` and `name` — no permission at all is needed to construct a
/// name like this.
fn name_with_tail(transfer_id: &str) -> String {
    let marker = hex::encode(Sha256::digest(transfer_id.as_bytes()));
    let tail = format!(".{}.part", &marker[..16]);
    format!("{}{}", "A".repeat(255 - tail.len()), tail)
}

/// Review N1 (Important): because `part_path` truncates the stem to fit the 255-byte limit, for
/// the name above the truncated result **becomes the same as** the original name — `temp ==
/// dest` (temp == destination).
///
/// Then the destination that was already there gets caught as resume debris (`file_len` becomes
/// the destination's length), the hash can't possibly match, and the mismatch cleanup
/// `remove_file(&temp)` **deletes the destination itself.** On that branch, neither an undo
/// backup nor an audit line is left behind — the one spot where all three lines of defense are
/// empty at once.
#[tokio::test]
async fn does_not_delete_the_existing_original_even_with_a_planted_suffix_name() {
    let source_dir = tempfile::tempdir().unwrap();
    let inbox_dir = tempfile::tempdir().unwrap();
    let undo = tempfile::tempdir().unwrap();
    let name = name_with_tail("t-evil");
    let content: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
    let source = source_dir.path().join("payload.bin");
    tokio::fs::write(&source, &content).await.unwrap();

    // The destination already has an original that must not be deleted. Its length has to be
    // shorter than the offer's for it to take the branch where it gets mistaken for "resume
    // debris."
    tokio::fs::create_dir_all(inbox_dir.path().join("a")).await.unwrap();
    let dest = inbox_dir.path().join("a").join(&name);
    tokio::fs::write(&dest, "IRREPLACEABLE-ORIGINAL".as_bytes()).await.unwrap();

    let config = TransferConfig {
        inbox: inbox_dir.path().to_path_buf(),
        undo: undo.path().to_path_buf(),
        ..TransferConfig::default()
    };
    let (a_conn, _b) =
        wire_up_peers("t-evil", &source, content.len() as u64, &hash(&content), config).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let result = b
        .push_offer(TransferOffer {
            transfer_id: "t-evil".into(),
            name: name.clone(),
            size: content.len() as u64,
            sha256: hash(&content),
            overwrite: true,
        })
        .await;

    // Surface what went wrong first — if they collide, the destination is deleted **before**
    // the response fails.
    assert!(
        dest.exists(),
        "failure cleanup deleted the destination itself (temp collided with the destination)"
    );
    let result = result.expect("if they don't collide, this is just an ordinary overwrite");
    assert!(result.replaced);
    assert_eq!(result.written, dest.display().to_string());
    assert_eq!(tokio::fs::read(&dest).await.unwrap(), content);
    let to_undo = result.undo.expect("if it overwrote, the original must be in the undo slot");
    assert_eq!(
        tokio::fs::read(&to_undo).await.unwrap(),
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
    let source_dir = tempfile::tempdir().unwrap();
    let inbox_dir = tempfile::tempdir().unwrap();
    let undo = tempfile::tempdir().unwrap();
    let content = "NEW".as_bytes().to_vec();
    let source = source_dir.path().join("data.txt");
    tokio::fs::write(&source, &content).await.unwrap();

    // The receiving side already has `.part` debris unrelated to this transfer, much larger than
    // the offer's size (3 bytes) — for example, the remnant of an old large transfer that got
    // cut off.
    tokio::fs::create_dir_all(inbox_dir.path().join("a")).await.unwrap();
    tokio::fs::write(
        partial_file(inbox_dir.path(), "t8", "data.txt"),
        "STALE-GARBAGE-LONGER-THAN-NEW".as_bytes(),
    )
    .await
    .unwrap();

    let config = TransferConfig {
        inbox: inbox_dir.path().to_path_buf(),
        undo: undo.path().to_path_buf(),
        ..TransferConfig::default()
    };
    let (a_conn, _b) =
        wire_up_peers("t8", &source, content.len() as u64, &hash(&content), config).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let result = b
        .push_offer(TransferOffer {
            transfer_id: "t8".into(),
            name: "data.txt".into(),
            size: content.len() as u64,
            sha256: hash(&content),
            overwrite: false,
        })
        .await
        .unwrap();

    assert_eq!(result.bytes, content.len() as u64);
    assert_eq!(result.sha256, hash(&content));
    assert_eq!(
        tokio::fs::read(&result.written).await.unwrap(),
        content,
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
    let source_dir = tempfile::tempdir().unwrap();
    let inbox_dir = tempfile::tempdir().unwrap();
    let undo = tempfile::tempdir().unwrap();
    // Multibyte Hangul content (each character is 3 bytes in UTF-8): the byte offset below
    // (content.len() / 3) is not guaranteed to land on a character boundary, so this also
    // exercises that the resume path is byte-transparent and never tries to parse the data as
    // text.
    let content = "가나다라마바사아자차카타파하".repeat(50).into_bytes();
    let source = source_dir.path().join("resume.bin");
    tokio::fs::write(&source, &content).await.unwrap();

    let already_received_len = content.len() / 3;
    tokio::fs::create_dir_all(inbox_dir.path().join("a")).await.unwrap();
    tokio::fs::write(partial_file(inbox_dir.path(), "t9", "resume.bin"), &content[..already_received_len])
        .await
        .unwrap();

    let config = TransferConfig {
        inbox: inbox_dir.path().to_path_buf(),
        undo: undo.path().to_path_buf(),
        ..TransferConfig::default()
    };
    let (a_conn, _b) =
        wire_up_peers("t9", &source, content.len() as u64, &hash(&content), config).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let result = b
        .push_offer(TransferOffer {
            transfer_id: "t9".into(),
            name: "resume.bin".into(),
            size: content.len() as u64,
            sha256: hash(&content),
            overwrite: false,
        })
        .await
        .unwrap();

    assert_eq!(result.bytes, content.len() as u64);
    assert_eq!(result.sha256, hash(&content));
    assert_eq!(tokio::fs::read(&result.written).await.unwrap(), content);
}

/// Review 3-2: the `.part` slot is outside `Inbox::resolve`'s check (that only looks at
/// `dest`, the destination). push_offer must reject a symlink planted there in advance that
/// points at a target outside the jail, and must not touch the target. Two layers — the upfront
/// check (`symlink_metadata`) and `O_NOFOLLOW` (unix) — guard against this.
#[cfg(unix)]
#[tokio::test]
async fn rejects_a_symlink_planted_at_the_temp_slot_and_leaves_the_target_untouched() {
    let source_dir = tempfile::tempdir().unwrap();
    let inbox_dir = tempfile::tempdir().unwrap();
    let undo = tempfile::tempdir().unwrap();
    let target_dir = tempfile::tempdir().unwrap();
    let target = target_dir.path().join("victim.txt");
    tokio::fs::write(&target, "UNTOUCHED").await.unwrap();

    let content = "payload".as_bytes().to_vec();
    let source = source_dir.path().join("linked.bin");
    tokio::fs::write(&source, &content).await.unwrap();

    // Plants a symlink at the `.part` slot in advance, pointing outside the jail (at the
    // target).
    tokio::fs::create_dir_all(inbox_dir.path().join("a")).await.unwrap();
    std::os::unix::fs::symlink(&target, partial_file(inbox_dir.path(), "t10", "linked.bin")).unwrap();

    let config = TransferConfig {
        inbox: inbox_dir.path().to_path_buf(),
        undo: undo.path().to_path_buf(),
        ..TransferConfig::default()
    };
    let (a_conn, _b) =
        wire_up_peers("t10", &source, content.len() as u64, &hash(&content), config).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let result = b
        .push_offer(TransferOffer {
            transfer_id: "t10".into(),
            name: "linked.bin".into(),
            size: content.len() as u64,
            sha256: hash(&content),
            overwrite: false,
        })
        .await;

    assert!(result.is_err(), "succeeded even though the temp slot was a symlink");
    assert_eq!(tokio::fs::read(&target).await.unwrap(), b"UNTOUCHED");
}

/// Whether a file large enough to need multiple chunks actually arrives.
///
/// **Without this test, nobody noticed the feature was completely broken.** The largest payload
/// in the rest of the tests is 9,000 bytes, which fits entirely in one chunk, so the path that
/// crosses into a second chunk was never exercised even once. That path had a permanent hang in
/// it — `chunk` is the same size as the protocol's credit window, so after serialization
/// it overran the window, and the sender got stuck on the first chunk.
///
/// So **the size is the whole point.** Keep the payload solidly bigger than a chunk. If `chunk`
/// grows, this value has to grow with it.
#[tokio::test]
async fn a_file_spanning_multiple_chunks_arrives_completely() {
    let source_dir = tempfile::tempdir().unwrap();
    let inbox_dir = tempfile::tempdir().unwrap();
    let undo = tempfile::tempdir().unwrap();
    // Splits into roughly eleven pieces at 64 KiB per chunk.
    let content: Vec<u8> = (0..700_000u32).map(|i| (i % 251) as u8).collect();
    let source = source_dir.path().join("big.bin");
    tokio::fs::write(&source, &content).await.unwrap();

    let config = TransferConfig {
        inbox: inbox_dir.path().to_path_buf(),
        undo: undo.path().to_path_buf(),
        ..TransferConfig::default()
    };
    let (a_conn, _b) = wire_up_peers("big", &source, content.len() as u64, &hash(&content), config).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    // It's a hanging bug, so a plain assert won't catch it — it only shows up as a failure once
    // a timeout is applied.
    let result = tokio::time::timeout(
        Duration::from_secs(20),
        b.push_offer(TransferOffer {
            transfer_id: "big".into(),
            name: "big.bin".into(),
            size: content.len() as u64,
            sha256: hash(&content),
            overwrite: false,
        }),
    )
    .await
    .expect("did not finish within 20 seconds — a multi-chunk transfer hangs")
    .unwrap();

    assert_eq!(result.bytes, content.len() as u64);
    assert_eq!(tokio::fs::read(&result.written).await.unwrap(), content);
}

/// Distinguishes whether the receiving side **actually resumes**.
///
/// The prefix of the sender's file and the prefix already received differ from each other (only
/// the length matches). The proposed sha256 is that of `already_received_prefix ++ suffix`
/// (already-received-prefix ++ suffix). So:
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
    let source_dir = tempfile::tempdir().unwrap();
    let inbox_dir = tempfile::tempdir().unwrap();
    let undo = tempfile::tempdir().unwrap();

    let already_received_prefix: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let sender_prefix: Vec<u8> = vec![0xAAu8; 100_000];
    let suffix: Vec<u8> = (0..200_000u32).map(|i| ((i % 241) as u8) ^ 0x5A).collect();
    assert_ne!(
        already_received_prefix,
        sender_prefix,
        "if the two prefixes were the same, this test couldn't distinguish anything"
    );

    let mut sender_content = sender_prefix.clone();
    sender_content.extend_from_slice(&suffix);
    let source = source_dir.path().join("a.bin");
    tokio::fs::write(&source, &sender_content).await.unwrap();

    // The final content that can only result if it actually resumed.
    let mut expected = already_received_prefix.clone();
    expected.extend_from_slice(&suffix);

    tokio::fs::create_dir_all(inbox_dir.path().join("a")).await.unwrap();
    tokio::fs::write(partial_file(inbox_dir.path(), "resume", "a.bin"), &already_received_prefix)
        .await
        .unwrap();

    let config = TransferConfig {
        inbox: inbox_dir.path().to_path_buf(),
        undo: undo.path().to_path_buf(),
        ..TransferConfig::default()
    };
    let (a_conn, _b) =
        wire_up_peers("resume", &source, expected.len() as u64, &hash(&expected), config).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let result = b
        .push_offer(TransferOffer {
            transfer_id: "resume".into(),
            name: "a.bin".into(),
            size: expected.len() as u64,
            sha256: hash(&expected),
            overwrite: false,
        })
        .await
        .unwrap();

    assert_eq!(result.bytes, expected.len() as u64);
    assert_eq!(tokio::fs::read(&result.written).await.unwrap(), expected);
}

#[tokio::test]
async fn restarts_from_scratch_when_the_partial_data_does_not_match_reality() {
    let source_dir = tempfile::tempdir().unwrap();
    let inbox_dir = tempfile::tempdir().unwrap();
    let undo = tempfile::tempdir().unwrap();
    let content: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
    let source = source_dir.path().join("a.bin");
    tokio::fs::write(&source, &content).await.unwrap();

    // Garbage prefix. If it resumes, the sha256 must not match — otherwise the bug slips
    // through silently.
    tokio::fs::create_dir_all(inbox_dir.path().join("a")).await.unwrap();
    tokio::fs::write(partial_file(inbox_dir.path(), "bad-resume", "a.bin"), vec![0xFFu8; 10_000])
        .await
        .unwrap();

    let config = TransferConfig {
        inbox: inbox_dir.path().to_path_buf(),
        undo: undo.path().to_path_buf(),
        ..TransferConfig::default()
    };
    let (a_conn, _b) =
        wire_up_peers("bad-resume", &source, content.len() as u64, &hash(&content), config).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let result = b
        .push_offer(TransferOffer {
            transfer_id: "bad-resume".into(),
            name: "a.bin".into(),
            size: content.len() as u64,
            sha256: hash(&content),
            overwrite: false,
        })
        .await;

    // The first attempt fails on the mismatch and deletes the partial file.
    assert!(result.is_err());
    // Calling it again must receive from scratch and succeed.
    let second = b
        .push_offer(TransferOffer {
            transfer_id: "bad-resume".into(),
            name: "a.bin".into(),
            size: content.len() as u64,
            sha256: hash(&content),
            overwrite: false,
        })
        .await
        .unwrap();
    assert_eq!(tokio::fs::read(&second.written).await.unwrap(), content);
}

/// Isolates whether `pull` really streams starting from `offset` on the sending side, apart
/// from the receiving side's decision.
///
/// The two tests above only distinguish **the receiving side's decision** (the `received_offset`
/// calculation) — even if the sender ignored `offset` and always streamed from the start, the
/// final result could still come out right if the receiving side stitches the prefix it already
/// had together with the bytes that streamed in well enough (or they happen to match). So
/// `pull` is called directly here to look at the sending side alone.
///
/// `pull` is answered by the sending side (A). Waiting on `wait_capability` from the `b_conn`
/// (B's connection) that `wire_up_peers` returns gets the handle A announced — waiting on `a_conn`
/// instead gets B's (the receiver's) handle, and B has never received `offer_file`, so calling
/// `pull` on it gets rejected with "unknown transfer".
#[tokio::test]
async fn pull_streams_starting_from_the_offset() {
    let source_dir = tempfile::tempdir().unwrap();
    let inbox_dir = tempfile::tempdir().unwrap();
    let undo = tempfile::tempdir().unwrap();
    let content: Vec<u8> = (0..300_000u32).map(|i| (i % 253) as u8).collect();
    let source = source_dir.path().join("straight.bin");
    tokio::fs::write(&source, &content).await.unwrap();

    let config = TransferConfig {
        inbox: inbox_dir.path().to_path_buf(),
        undo: undo.path().to_path_buf(),
        ..TransferConfig::default()
    };
    let (_a_conn, b_conn) =
        wire_up_peers("straight", &source, content.len() as u64, &hash(&content), config).await;
    let a: PeerTransferClient = b_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let mut stream = a.pull("straight".to_string(), 100_000).await.unwrap();
    assert_eq!(
        stream.head.size,
        content.len() as u64,
        "head.size must be the whole file size, not the value with offset subtracted"
    );

    let mut received: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.items.next().await {
        let Chunk(bytes) = chunk.unwrap();
        received.extend_from_slice(&bytes);
    }
    assert_eq!(received, content[100_000..], "bytes must match from 100_000 to the end");
}
