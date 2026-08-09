//! Reference implementation of `peer_transfer`. One type plays both roles — the sending side
//! answers `pull`, the receiving side answers `push_offer`.
//!
//! **Integrity is verified here.** The engine's `s_end.trailer.sha256` is discarded by the
//! receiving side (`connection.rs` destructures `Envelope::SEnd { stream, .. }`), so it cannot be
//! trusted.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use zyris::{Chunk, ErrorCode, Result, Streaming, WireError};
use zyris_caps::peer_transfer::{
    PeerTransfer, PeerTransferClient, PullHead, TransferDone, TransferOffer,
};

use super::audit::{Audit, AuditLine};
use super::inbox::Inbox;
use super::undo::UndoStore;

/// How much of the file to load into one stream item.
///
/// **Must stay comfortably smaller than the protocol's `initial_stream_credit` (256 KiB).**
/// Items get wrapped in msgpack before going out on the wire, so loading exactly the window's
/// size makes the serialized length overrun the window by a few bytes. The sending side's
/// `CreditGate::acquire` then blocks **on the very first chunk**, and the peer — never having
/// received that chunk — has no way to return credit for it: both sides wait on each other
/// forever. This actually happened at 256 KiB (262,144 hung, 262,080 got through).
///
/// The wrapping overhead is a constant independent of size (a 5-byte msgpack `bin32` header). At
/// 64 KiB, one item is 65,541 bytes, so three fit together in one window.
///
/// **Being a constant rather than a margin is the weak point.** `initial_stream_credit` is a
/// negotiated value the receiving side sets through `AcceptOptions::limits`, and `pull` cannot see
/// it. If anyone sets the window smaller than 65,541 (say, exactly 64 KiB), the same permanent
/// hang comes back. The right fix is to derive this from the negotiated value — deferred for now
/// because the default is 256 KiB, which leaves fourfold headroom.
const 청크: usize = 64 * 1024;

/// Where the in-progress `.part` file of one transfer lives, next to its destination.
///
/// Two properties have to hold **at the same time**:
///
/// - Retries of the *same* transfer must land on the *same* path, or resume is dead — the
///   received prefix would never be found again.
/// - *Different* transfers must never share a path. Two concurrent pushes of the same name used
///   to open one file with `truncate` and write over each other, while each hasher only ever saw
///   its own stream — so both passed the sha256 check and the reply named bytes that were not the
///   ones on disk.
///
/// `transfer_id` has exactly that shape (it is derived from the file, so a retry repeats it and a
/// different transfer does not). But **it is chosen by the peer**, so it never goes into a path
/// as-is. What goes into the name is the first 16 hex characters of its sha256: fixed width, no
/// separators, no reserved characters, nothing a caller can steer.
///
/// The suffix is appended to the file name rather than replacing an extension. `with_extension`
/// would make `a.txt` and `a.bin` share one `.part`, and would make a proposed name that already
/// ends in `.part` collide with the destination itself. The stem is shortened so the suffix still
/// fits inside the 255-byte name limit [`super::name`] enforces — and then checked, because the
/// shortening itself can hand back the destination's own name when the peer plants the suffix at
/// the end of a full-length name. **The result is never the destination**; a `.part` that is the
/// destination turns the failure cleanup into a delete of the file being replaced.
pub fn part_path(목적지: &Path, transfer_id: &str) -> PathBuf {
    let 표식 = hex::encode(Sha256::digest(transfer_id.as_bytes()));
    // `.` + 16 chars + `.part` = 22 bytes. The length never changes no matter what the input is.
    let 꼬리 = format!(".{}.part", &표식[..16]);
    // `file_name()` can never be `None` here — `목적지` is a path `Inbox::resolve` built by
    // passing it through `safe_name`, so it always has a last component. If it somehow were
    // absent anyway, fall back to "file" instead of panicking.
    let 이름 = 목적지
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    // `safe_name` may have already filled the name out to 255 bytes. Just appending the suffix
    // would overrun the limit and produce `ENAMETOOLONG` — so the stem is cut back by exactly the
    // suffix's share, and **at a character boundary** (cutting by byte panics on Korean text).
    let 남길_바이트 = super::name::최대_바이트.saturating_sub(꼬리.len());
    let mut 줄기 = String::new();
    for c in 이름.chars() {
        if 줄기.len() + c.len_utf8() > 남길_바이트 {
            break;
        }
        줄기.push(c);
    }
    let mut 임시 = 목적지.with_file_name(format!("{줄기}{꼬리}"));
    // **Truncation can fold `임시` back into `목적지` itself.** The suffix is derived from
    // `transfer_id`, and both `transfer_id` and `name` are chosen by the sending side — so
    // pre-computing the suffix and planting it at the end of a 255-byte name makes the truncated
    // result come out letter-for-letter identical to the original name. When that happens, an
    // existing destination gets mistaken for a resume leftover, its hash can never match, and the
    // mismatch cleanup `remove_file(&임시)` deletes the destination itself — leaving neither an
    // undo nor an audit trail behind.
    //
    // Shortening by just one more character necessarily makes the lengths diverge. In the
    // colliding case the stem sits in the 200-byte range, so there is always a character left to
    // drop (if the stem were the entire name, `임시` would already be longer than the name by the
    // suffix's length, and they would never have collided in the first place).
    if 임시 == 목적지 {
        줄기.pop();
        임시 = 목적지.with_file_name(format!("{줄기}{꼬리}"));
    }
    임시
}

#[derive(Debug, Clone)]
pub struct TransferConfig {
    pub inbox: PathBuf,
    pub undo: PathBuf,
    pub audit: Option<PathBuf>,
    pub max_file_bytes: u64,
    pub max_inbox_bytes: u64,
}

impl Default for TransferConfig {
    fn default() -> TransferConfig {
        TransferConfig {
            inbox: PathBuf::from("."),
            undo: PathBuf::from("."),
            audit: None,
            max_file_bytes: 8 * 1024 * 1024 * 1024,
            max_inbox_bytes: 32 * 1024 * 1024 * 1024,
        }
    }
}

/// For the sending side to answer `pull`, it has to know what it committed to sending.
#[derive(Clone)]
struct 보낼_것 {
    transfer_id: String,
    path: PathBuf,
    size: u64,
    sha256: String,
}

/// **The handle is plugged in later.** For the receiving side to call `pull` back, it needs a
/// `PeerTransferClient` — but that only exists after `Node::accept` returns and the peer
/// announces its capability. Building the `Node`, though, requires this struct to exist
/// **first** — a cycle. So `peer` is interior-mutable and `set_peer` fills it in afterward. Get
/// this ordering wrong and try to take it as a constructor argument instead, and the problem
/// isn't a compile error — the wiring is simply impossible.
#[derive(Clone)]
pub struct LocalPeerTransfer {
    config: TransferConfig,
    /// Only filled in on the receiving side. The handle used to call back to the peer while
    /// handling a `push_offer`.
    peer: Arc<std::sync::OnceLock<PeerTransferClient>>,
    peer_slug: String,
    /// What the sending side has reserved.
    pending: Arc<tokio::sync::Mutex<Vec<보낼_것>>>,
}

impl LocalPeerTransfer {
    /// The receiving side. The handle is plugged in later via `set_peer`.
    pub fn receiver_pending(config: TransferConfig, peer_slug: String) -> LocalPeerTransfer {
        LocalPeerTransfer {
            config,
            peer: Arc::new(std::sync::OnceLock::new()),
            peer_slug,
            pending: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        }
    }

    /// Plugs in the handle used to call `pull` on the peer. A second call is silently ignored —
    /// a connection has exactly one handle, and if it could be overwritten that would just
    /// become the slot swapped in instead.
    pub fn set_peer(&self, client: PeerTransferClient) {
        let _ = self.peer.set(client);
    }

    pub fn sender(root: PathBuf) -> LocalPeerTransfer {
        LocalPeerTransfer {
            config: TransferConfig { inbox: root.clone(), undo: root, ..Default::default() },
            peer: Arc::new(std::sync::OnceLock::new()),
            peer_slug: String::new(),
            pending: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        }
    }

    /// Reserves what the sending side will hand out. `send_to` calls this right before
    /// `push_offer`.
    pub async fn offer_file(&self, transfer_id: String, path: PathBuf, size: u64, sha256: String) {
        self.pending.lock().await.push(보낼_것 { transfer_id, path, size, sha256 });
    }
}

#[async_trait::async_trait]
impl PeerTransfer for LocalPeerTransfer {
    async fn push_offer(&self, offer: TransferOffer) -> Result<TransferDone> {
        if offer.size > self.config.max_file_bytes {
            return Err(WireError::new(
                ErrorCode::PayloadTooLarge,
                format!(
                    "{}바이트는 이 노드의 상한 {}바이트를 넘습니다",
                    offer.size, self.config.max_file_bytes
                ),
            )
            .retriable(false));
        }
        let peer = self.peer.get().ok_or_else(|| {
            WireError::internal("이 노드는 받는 쪽으로 세워지지 않았습니다".to_string())
        })?;

        let inbox = Inbox::new(&self.config.inbox);
        let 목적지 = inbox
            .resolve(&self.peer_slug, &offer.name)
            .await
            .map_err(|e| WireError::internal(e.to_string()))?;

        // This check exists to reject quickly, before the transfer even starts. **The check that
        // actually enforces the contract is not this one but the re-check right before rename** —
        // the destination can come into existence while the transfer is in flight.
        if tokio::fs::symlink_metadata(&목적지).await.is_ok() && !offer.overwrite {
            return Err(WireError::new(
                ErrorCode::InvalidParams,
                format!("{}이(가) 이미 있습니다. 덮으려면 overwrite를 켜세요", 목적지.display()),
            )
            .retriable(false));
        }

        // The temp file has to be in the same directory for rename to be atomic. How the name is
        // built and why `transfer_id` has to be mixed in is written up in `part_path`.
        let 임시 = part_path(&목적지, &offer.transfer_id);
        // What `Inbox::resolve` checked was `목적지` alone — `.part` is a separate path that
        // never went through that check. The parent directory is safe (`resolve` already walked
        // it and confirmed it is not a symlink, hence `실제_부모`), so only the last component,
        // `임시` itself, needs to be looked at again here. Skip this and just open it, and opening
        // would follow a symlink planted there ahead of time.
        if let Ok(정보) = tokio::fs::symlink_metadata(&임시).await {
            if 정보.file_type().is_symlink() {
                return Err(WireError::new(
                    ErrorCode::InvalidParams,
                    "임시 파일 자리가 심볼릭 링크입니다".to_string(),
                )
                .retriable(false));
            }
        }
        let 파일_길이 = tokio::fs::metadata(&임시).await.map(|m| m.len()).unwrap_or(0);
        // If the leftover is bigger than this offer, it cannot be resumed — start over from
        // scratch. That decision (the offset) cannot be the only thing that gets reverted, though.
        // Opening with `.append(true)` without also truncating the file leaves the new bytes
        // sitting right **after** the leftover — the hasher only sees the newly received bytes, so
        // the sha256 check passes, yet what is left on disk is not the file that was verified. So
        // the spot where the offset is reset to 0 also switches the open mode to truncate (below).
        let 받은_offset = if 파일_길이 > offer.size { 0 } else { 파일_길이 };

        let mut 스트림 = peer.pull(offer.transfer_id.clone(), 받은_offset).await?;
        if 스트림.head.sha256 != offer.sha256 || 스트림.head.size != offer.size {
            return Err(WireError::internal(
                "보내는 쪽이 offer와 다른 것을 내주려 합니다".to_string(),
            ));
        }

        let mut 해시기 = Sha256::new();
        if 받은_offset > 0 {
            // On a resume, re-read the part already received and feed it into the hash. Loading
            // it whole into a `Vec` would allocate straight up to the ceiling (8 GiB by default)
            // — instead, read repeatedly into a fixed-size buffer and feed only that to the hash.
            use tokio::io::AsyncReadExt as _;
            let mut 이미_받은 =
                임시_열기_바탕().read(true).open(&임시).await.map_err(io_오류)?;
            let mut 버퍼 = vec![0u8; 청크];
            loop {
                let n = 이미_받은.read(&mut 버퍼).await.map_err(io_오류)?;
                if n == 0 {
                    break;
                }
                해시기.update(&버퍼[..n]);
            }
        }
        let mut 열기 = 임시_열기_바탕();
        열기.create(true);
        if 받은_offset > 0 {
            열기.append(true);
        } else {
            // If this is not a resume, whatever is already there is a leftover — truncate it and
            // write fresh. Opening with `append` would leave that leftover sitting right in front
            // of the new bytes.
            열기.write(true).truncate(true);
        }
        let mut 파일 = 열기.open(&임시).await.map_err(io_오류)?;

        use tokio::io::AsyncWriteExt;
        let mut 쓴_바이트 = 받은_offset;
        while let Some(조각) = 스트림.items.next().await {
            let Chunk(바이트) = 조각?;
            쓴_바이트 += 바이트.len() as u64;
            if 쓴_바이트 > offer.size {
                let _ = tokio::fs::remove_file(&임시).await;
                return Err(WireError::internal("선언한 크기보다 많이 보냈습니다".to_string()));
            }
            해시기.update(&바이트);
            파일.write_all(&바이트).await.map_err(io_오류)?;
        }
        파일.flush().await.map_err(io_오류)?;
        drop(파일);

        let 실제 = hex::encode(해시기.finalize());
        if 실제 != offer.sha256 {
            // Leaving the partial file behind would make the next resume pick it up and never
            // match.
            let _ = tokio::fs::remove_file(&임시).await;
            return Err(WireError::new(
                ErrorCode::Internal,
                format!("sha256이 맞지 않습니다: {실제} ≠ {}", offer.sha256),
            )
            // Design doc §9 defines `integrity_mismatch` as **retriable** — what is broken is the
            // bytes that came through this time, not the request itself, so receiving again can
            // still succeed. `ErrorCode::Internal`'s default is `false`, so this comes out the
            // wrong way if left unstated.
            .retriable(true));
        }

        // **This is where existence gets checked again.** The earlier check was against the state
        // before the transfer started, and a window as wide as the whole transfer (several
        // seconds, for a 12 MB file) opened up in between. If the destination showed up in that
        // window, the old code overwrote it even with `overwrite=false`, and replied with
        // `replaced:false` and `undo:None` — leaving no trace even in the audit log.
        //
        // **A narrow window still remains between this re-check and `rename`.** It has not been
        // eliminated, only narrowed from the whole transfer down to a few syscalls. Closing it for
        // real would mean handing the `overwrite=false` branch to the kernel via
        // `renameat2(RENAME_NOREPLACE)` (Linux only), and making the undo stash atomic together
        // with it would need a per-destination lock — both are outside this branch.
        let 이미_있나 = tokio::fs::symlink_metadata(&목적지).await.is_ok();
        if 이미_있나 && !offer.overwrite {
            // Leaving the `.part` received so far would make the next resume pick it up. This
            // request was rejected, so delete it.
            let _ = tokio::fs::remove_file(&임시).await;
            return Err(WireError::new(
                ErrorCode::InvalidParams,
                format!(
                    "{}이(가) 전송 중에 생겼습니다. 덮으려면 overwrite를 켜세요",
                    목적지.display()
                ),
            )
            .retriable(false));
        }

        let undo = if 이미_있나 {
            UndoStore::new(&self.config.undo).stash(&목적지, 지금_ms()).await
        } else {
            None
        };
        tokio::fs::rename(&임시, &목적지).await.map_err(io_오류)?;
        실행_비트_제거(&목적지).await;

        let 결과 = TransferDone {
            written: 목적지.display().to_string(),
            bytes: 쓴_바이트,
            sha256: 실제,
            replaced: 이미_있나,
            undo: undo.map(|p| p.display().to_string()),
        };

        if let Some(감사_길) = &self.config.audit {
            Audit::new(감사_길)
                .record(AuditLine {
                    at_ms: 지금_ms(),
                    peer_slug: self.peer_slug.clone(),
                    // The p2p transport (zyris-p2p) does not exist yet — Task 4.1 fills in the
                    // actual endpoint.
                    peer_endpoint: String::new(),
                    name: offer.name.clone(),
                    bytes: 결과.bytes,
                    sha256: 결과.sha256.clone(),
                    written: 결과.written.clone(),
                    replaced: 결과.replaced,
                    // `replaced: true` together with this being `None` means the original was
                    // overwritten without ever being stashed — this field is the only thing that
                    // tells a reversible overwrite apart from a permanent loss when reading the
                    // log.
                    undo: 결과.undo.clone(),
                    // Whether this was a direct connection or went through a relay is not
                    // something this layer can know either.
                    direct: false,
                })
                .await;
        }

        Ok(결과)
    }

    async fn pull(&self, transfer_id: String, offset: u64) -> Result<Streaming<PullHead, Chunk>> {
        let 것 = self
            .pending
            .lock()
            .await
            .iter()
            .find(|p| p.transfer_id == transfer_id)
            .cloned()
            .ok_or_else(|| {
                WireError::new(ErrorCode::InvalidParams, format!("모르는 전송입니다: {transfer_id}"))
                    .retriable(false)
            })?;

        let head = PullHead { size: 것.size, sha256: 것.sha256.clone() };

        // The file is opened outside the stream — a file that cannot be opened has to be "the
        // call itself failed", not an error partway through the stream. The `끝났나` flag carried
        // in `unfold`'s state plays the same role as `file_io.rs::read_stream`'s
        // `remaining = Some(0)`: without returning `None` on the next poll after emitting one
        // error, the same error would be emitted forever.
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        let mut file = tokio::fs::File::open(&것.path).await.map_err(io_오류)?;
        if offset > 0 {
            file.seek(std::io::SeekFrom::Start(offset)).await.map_err(io_오류)?;
        }

        let items =
            futures_util::stream::unfold((file, false), move |(mut file, 끝났나)| async move {
                if 끝났나 {
                    return None;
                }
                let mut 버퍼 = vec![0u8; 청크];
                match file.read(&mut 버퍼).await {
                    Ok(0) => None,
                    Ok(n) => {
                        버퍼.truncate(n);
                        Some((Ok(Chunk(Bytes::from(버퍼))), (file, false)))
                    }
                    Err(e) => Some((Err(io_오류(e)), (file, true))),
                }
            });
        Ok(Streaming::new(head, items))
    }
}

fn io_오류(e: std::io::Error) -> WireError {
    WireError::new(ErrorCode::Internal, e.to_string())
}

/// The base `OpenOptions` used when opening `임시` (`.part`). On unix, `O_NOFOLLOW` is added so
/// that opening itself fails if the last component is a symlink. A narrow window still remains
/// between the earlier `symlink_metadata` pre-check and this open (if someone plants a symlink
/// after the check but before the open) — this flag closes that remaining window. Windows has
/// `FILE_FLAG_OPEN_REPARSE_POINT` too, but its meaning runs the other way ("open it without
/// following" — it opens the link itself instead of failing). So on that side, only the pre-check
/// defends, and the window remains.
#[cfg_attr(not(unix), allow(unused_mut))]
fn 임시_열기_바탕() -> tokio::fs::OpenOptions {
    let mut 옵션 = tokio::fs::OpenOptions::new();
    #[cfg(unix)]
    옵션.custom_flags(libc::O_NOFOLLOW);
    옵션
}

fn 지금_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// There is no reason a received file should be executable.
async fn 실행_비트_제거(길: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = tokio::fs::set_permissions(길, std::fs::Permissions::from_mode(0o600)).await;
    }
    #[cfg(not(unix))]
    let _ = 길;
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use sha2::{Digest, Sha256};

    use super::part_path;
    use super::super::name::{safe_name, 최대_바이트};

    /// A 255-byte name where the peer **pre-computed the suffix and planted it at the end**.
    ///
    /// The suffix `.<sha256(transfer_id)[..16]>.part` is derived from `transfer_id`, and both
    /// `transfer_id` and `name` are chosen by the sending side. So crafting a name like this
    /// requires no special privilege at all.
    fn 꼬리를_심은_이름(transfer_id: &str) -> String {
        let 표식 = hex::encode(Sha256::digest(transfer_id.as_bytes()));
        let 꼬리 = format!(".{}.part", &표식[..16]);
        format!("{}{}", "A".repeat(최대_바이트 - 꼬리.len()), 꼬리)
    }

    #[test]
    fn same_transfer_same_slot_different_transfer_different_slot() {
        let 목적지 = Path::new("/inbox/a/same.bin");
        // The property resume depends on — a retry has to find the leftover received earlier.
        assert_eq!(part_path(목적지, "t1"), part_path(목적지, "t1"));
        // The property that concurrent transfers do not overwrite each other's `.part`. If this
        // breaks, both pass the sha256 check and the reply points at bytes that are not on disk.
        assert_ne!(part_path(목적지, "t1"), part_path(목적지, "t2"));
        // If the temp file collides with the destination, failure cleanup (`remove_file`) deletes
        // the destination itself.
        assert_ne!(part_path(목적지, "t1"), 목적지);
        assert_ne!(part_path(Path::new("/inbox/a/x.part"), "t1"), Path::new("/inbox/a/x.part"));
        // For rename to be atomic, it has to stay in the same directory.
        assert_eq!(part_path(목적지, "t1").parent(), 목적지.parent());
    }

    #[test]
    fn whatever_the_peer_sends_exactly_one_path_component_survives() {
        let 목적지 = Path::new("/inbox/a/x.bin");
        let 임시 = part_path(목적지, "../../etc/passwd\0\n/..");
        assert_eq!(임시.parent(), 목적지.parent(), "actual: {}", 임시.display());
        let 이름 = 임시.file_name().unwrap().to_str().unwrap();
        assert!(!이름.contains('/') && !이름.contains('\\'), "actual: {이름}");
    }

    /// Review N1: length truncation can fold `임시` back into `목적지` **itself**.
    ///
    /// If the peer pre-computes the suffix and plants it at the end of a 255-byte name,
    /// `part_path` truncates the stem to 233 bytes and appends the same suffix back on, producing
    /// the original name. At that point an existing destination gets mistaken for a resume
    /// leftover, and the hash-mismatch cleanup (`remove_file(&임시)`) **deletes the destination
    /// itself** — disabling sha256 verification, the undo stash, and the audit log all at once.
    #[test]
    fn planting_the_suffix_at_the_end_of_the_name_still_keeps_temp_and_destination_apart() {
        let 이름 = 꼬리를_심은_이름("t-evil");
        assert_eq!(이름.len(), 최대_바이트, "must fill 255 bytes exactly for truncation to occur");
        // The wash lets this name straight through — it is not a separator, a control character,
        // or a reserved name.
        assert_eq!(safe_name(&이름), 이름);

        let 목적지 = Path::new("/inbox/a").join(&이름);
        let 임시 = part_path(&목적지, "t-evil");
        assert_ne!(임시, 목적지, "actual: {}", 임시.display());
        assert_eq!(임시.parent(), 목적지.parent());
        let 임시_이름 = 임시.file_name().unwrap().to_str().unwrap();
        assert!(임시_이름.len() <= 최대_바이트, "{} bytes", 임시_이름.len());
        assert!(임시_이름.ends_with(".part"), "actual: {임시_이름}");
    }

    #[test]
    fn name_filling_the_limit_exactly_still_stays_within_255_bytes() {
        // `safe_name` can fill a name out to 255 bytes. Just appending the suffix would overrun
        // the limit and produce `ENAMETOOLONG`.
        let 긴_이름 = "가".repeat(85); // 3 bytes × 85 = 255 bytes
        let 목적지 = Path::new("/inbox/a").join(&긴_이름);
        let 이름 = part_path(&목적지, "t1").file_name().unwrap().to_str().unwrap().to_string();
        assert!(이름.len() <= 255, "{} bytes", 이름.len());
        assert!(이름.ends_with(".part"), "actual: {이름}");
    }
}
