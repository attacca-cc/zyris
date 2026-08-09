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

/// 파일을 스트림 항목 하나에 얼마씩 실을지.
///
/// **프로토콜의 `initial_stream_credit`(256 KiB)보다 넉넉히 작아야 한다.** 항목은 와이어로
/// 나가기 전에 msgpack으로 감싸이므로, 창과 *똑같은* 크기로 실으면 직렬화된 길이가 창을 몇
/// 바이트 넘는다. 그러면 보내는 쪽 `CreditGate::acquire`가 **첫 청크에서** 막히고, 상대는 그
/// 청크를 못 받았으니 credit을 돌려줄 수 없다 — 양쪽이 서로를 기다리며 영원히 멈춘다.
/// 256 KiB로 뒀을 때 실제로 그랬다(262,144는 정지, 262,080은 통과).
///
/// 감싸는 몫은 크기와 무관한 상수다(msgpack `bin32` 헤더 5바이트). 64 KiB면 항목 하나가
/// 65,541바이트라 창 하나에 셋이 함께 흐른다.
///
/// **여백이 아니라 상수인 것이 약점이다.** `initial_stream_credit`은 받는 쪽이
/// `AcceptOptions::limits`로 정하는 협상값인데 `pull`은 그 값을 볼 수 없다. 누가 창을
/// 65,541보다 작게(예: 딱 64 KiB) 잡으면 같은 영구 정지가 돌아온다. 협상값에서 유도하도록
/// 고치는 것이 옳다 — 지금은 기본값이 256 KiB라 네 배 여유가 있어 미뤄 둔다.
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
/// fits inside the 255-byte name limit [`super::name`] enforces.
pub fn part_path(목적지: &Path, transfer_id: &str) -> PathBuf {
    let 표식 = hex::encode(Sha256::digest(transfer_id.as_bytes()));
    // `.` + 16글자 + `.part` = 22바이트. 입력이 무엇이든 길이가 변하지 않는다.
    let 꼬리 = format!(".{}.part", &표식[..16]);
    // `file_name()`이 `None`일 일은 없다 — `목적지`는 `Inbox::resolve`가 `safe_name`을 거쳐
    // 만든 경로라 항상 마지막 조각이 있다. 혹시라도 없다면 패닉 대신 "file"로 대체한다.
    let 이름 = 목적지
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    // `safe_name`이 이름을 이미 255바이트까지 채워 놨을 수 있다. 꼬리를 그냥 붙이면 한도를
    // 넘어 `ENAMETOOLONG`이 난다 — 꼬리 몫만큼 줄기를 **문자 경계에서** 잘라 낸다(바이트로
    // 자르면 한글에서 패닉한다).
    let 남길_바이트 = super::name::최대_바이트.saturating_sub(꼬리.len());
    let mut 줄기 = String::new();
    for c in 이름.chars() {
        if 줄기.len() + c.len_utf8() > 남길_바이트 {
            break;
        }
        줄기.push(c);
    }
    목적지.with_file_name(format!("{줄기}{꼬리}"))
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

/// 보내는 쪽이 `pull`에 답하려면 무엇을 보내기로 했는지 알아야 한다.
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
    /// 받는 쪽일 때만 채워진다. `push_offer`를 처리하며 상대에게 되당기는 손잡이다.
    peer: Arc<std::sync::OnceLock<PeerTransferClient>>,
    peer_slug: String,
    /// 보내는 쪽일 때 예약해 둔 것들.
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

        // 전송을 시작하기도 전에 빨리 거절하기 위한 판정이다. **계약을 지키는 판정은 이것이
        // 아니라 rename 직전의 재검사다** — 전송이 도는 동안 목적지가 생길 수 있다.
        if tokio::fs::symlink_metadata(&목적지).await.is_ok() && !offer.overwrite {
            return Err(WireError::new(
                ErrorCode::InvalidParams,
                format!("{}이(가) 이미 있습니다. 덮으려면 overwrite를 켜세요", 목적지.display()),
            )
            .retriable(false));
        }

        // 임시 파일은 같은 디렉터리 안이어야 rename이 원자적이다. 이름을 어떻게 짓는지와 왜
        // `transfer_id`가 섞여야 하는지는 `part_path`에 적혀 있다.
        let 임시 = part_path(&목적지, &offer.transfer_id);
        // `Inbox::resolve`가 확인한 것은 `목적지`뿐이다 — `.part`는 그 확인을 거치지 않은
        // 별도 경로다. 부모 디렉터리는 `resolve`가 이미 걸어서 심링크가 아님을 확인했으니
        // 안전하고(실제_부모), 마지막 조각인 `임시` 자체만 여기서 다시 본다. 확인하지 않고
        // 그냥 열면 미리 심어 둔 심링크를 열기가 그대로 따라가 버린다.
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
        // 부스러기가 이번 offer보다 크면 이어받을 수 없다 — 처음부터 다시 받는다. 되돌리는
        // 것은 이 판단(오프셋)뿐이면 안 된다. 파일 자체를 비우지 않고 `.append(true)`로 열면
        // 새 바이트가 부스러기 **뒤에** 그대로 붙는다 — 해시기는 새로 받은 바이트만 보므로
        // sha256 대조는 통과하는데 디스크에는 검증한 것과 다른 파일이 남는다. 그래서 오프셋을
        // 0으로 되돌리는 자리에서 열기 모드도 함께 truncate로 바꾼다(아래).
        let 받은_offset = if 파일_길이 > offer.size { 0 } else { 파일_길이 };

        let mut 스트림 = peer.pull(offer.transfer_id.clone(), 받은_offset).await?;
        if 스트림.head.sha256 != offer.sha256 || 스트림.head.size != offer.size {
            return Err(WireError::internal(
                "보내는 쪽이 offer와 다른 것을 내주려 합니다".to_string(),
            ));
        }

        let mut 해시기 = Sha256::new();
        if 받은_offset > 0 {
            // 이어받기라면 이미 받아 둔 부분을 다시 읽어 해시에 넣는다. 통째로 `Vec`에
            // 올리면 상한(기본 8 GiB)까지 그대로 할당된다 — 고정 크기 버퍼로 반복 read하며
            // 해시에만 먹인다.
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
            // 이어받는 게 아니면 있던 내용은 부스러기다 — 비우고 새로 쓴다. `append`로 열면
            // 그 부스러기가 그대로 남아 새 바이트 앞에 눌러앉는다.
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
            // 부분 파일을 남기면 다음 재개가 그것을 이어받아 영영 안 맞는다.
            let _ = tokio::fs::remove_file(&임시).await;
            return Err(WireError::new(
                ErrorCode::Internal,
                format!("sha256이 맞지 않습니다: {실제} ≠ {}", offer.sha256),
            )
            // 설계문 §9는 `integrity_mismatch`를 **재시도 가능**으로 정한다 — 망가진 것은
            // 이번에 흘러온 바이트지 요청 자체가 아니므로, 다시 받으면 맞을 수 있다.
            // `ErrorCode::Internal`의 기본값은 `false`라 명시하지 않으면 반대로 나간다.
            .retriable(true));
        }

        // **여기서 존재 여부를 다시 본다.** 위의 사전 판정은 전송이 시작되기 전의 상태였고,
        // 그 사이에 전송 시간 전체(12 MB면 수 초)만큼의 창이 벌어져 있었다. 그 창에 목적지가
        // 생기면 예전 코드는 `overwrite=false`인데도 덮었고, `replaced:false`·`undo:None`으로
        // 응답해 감사 로그에도 흔적을 남기지 않았다.
        //
        // **이래도 재검사와 `rename` 사이에 좁은 창이 남는다.** 없앤 것이 아니라 전송 시간
        // 전체에서 syscall 몇 개 사이로 좁힌 것이다. 정말로 닫으려면 `overwrite=false` 갈래를
        // `renameat2(RENAME_NOREPLACE)`(linux 전용)로 커널에 맡기고, 되돌림 보관까지 함께
        // 원자적으로 만들려면 목적지 단위 잠금이 있어야 한다 — 둘 다 이 브랜치 밖이다.
        let 이미_있나 = tokio::fs::symlink_metadata(&목적지).await.is_ok();
        if 이미_있나 && !offer.overwrite {
            // 받아 둔 `.part`를 남기면 다음 재개가 그것을 이어받는다. 이번 요청은 거절됐으니
            // 지운다.
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
                    // p2p 전송로(zyris-p2p)가 아직 없다 — Task 4.1이 실제 엔드포인트를 채운다.
                    peer_endpoint: String::new(),
                    name: offer.name.clone(),
                    bytes: 결과.bytes,
                    sha256: 결과.sha256.clone(),
                    written: 결과.written.clone(),
                    replaced: 결과.replaced,
                    // `replaced: true`인데 이것이 `None`이면 원본을 보관하지 못한 채 덮었다는
                    // 뜻이다 — 되돌릴 수 있게 덮은 것과 영영 잃은 것을 로그만 보고 가르는
                    // 것은 이 필드뿐이다.
                    undo: 결과.undo.clone(),
                    // 직접 연결인지 릴레이를 지났는지도 이 계층에서는 알 길이 없다.
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

        // 파일은 스트림 밖에서 연다 — 못 여는 파일은 "부르기 자체가 실패"여야지 스트림
        // 중간의 에러가 되면 안 된다. `unfold`의 상태에 담는 `끝났나` 플래그는
        // `file_io.rs::read_stream`의 `remaining = Some(0)`과 같은 역할이다: 한 번 에러를
        // 낸 다음 poll에서 `None`을 돌려주지 않으면 같은 에러를 영원히 내보낸다.
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

/// `임시`(`.part`)를 열 때 쓰는 `OpenOptions` 바탕. unix에서는 `O_NOFOLLOW`를 얹어 마지막
/// 조각이 심링크면 열기 자체가 실패하게 한다. 앞선 `symlink_metadata` 사전 검사와 이 열기
/// 사이에는 여전히 좁은 창이 있다(검사 뒤·열기 전에 누가 심링크를 심으면) — 이 플래그가
/// 그 창을 마저 닫는다. windows에도 `FILE_FLAG_OPEN_REPARSE_POINT`가 있기는 하지만 그것은
/// "따라가지 말고 열어라"라 의미가 반대다(실패시키는 것이 아니라 링크 자체를 연다). 그래서
/// 저쪽에서는 사전 검사만으로 방어하고 창이 남는다.
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

/// 받은 파일이 실행 가능할 이유가 없다.
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

    use super::part_path;

    #[test]
    fn 같은_전송은_같은_자리_다른_전송은_다른_자리를_쓴다() {
        let 목적지 = Path::new("/inbox/a/same.bin");
        // 이어받기가 사는 성질 — 재시도가 앞서 받아 둔 부스러기를 다시 찾아야 한다.
        assert_eq!(part_path(목적지, "t1"), part_path(목적지, "t1"));
        // 동시 전송이 서로의 `.part`를 덮지 않는 성질. 이게 깨지면 양쪽 다 sha256을 통과하고
        // 응답이 디스크에 없는 바이트를 가리킨다.
        assert_ne!(part_path(목적지, "t1"), part_path(목적지, "t2"));
        // 임시가 목적지와 겹치면 실패 정리(`remove_file`)가 목적지 자체를 지운다.
        assert_ne!(part_path(목적지, "t1"), 목적지);
        assert_ne!(part_path(Path::new("/inbox/a/x.part"), "t1"), Path::new("/inbox/a/x.part"));
        // rename이 원자적이려면 같은 디렉터리 안이어야 한다.
        assert_eq!(part_path(목적지, "t1").parent(), 목적지.parent());
    }

    #[test]
    fn 상대가_뭘_보내도_경로_조각이_하나로_남는다() {
        let 목적지 = Path::new("/inbox/a/x.bin");
        let 임시 = part_path(목적지, "../../etc/passwd\0\n/..");
        assert_eq!(임시.parent(), 목적지.parent(), "실제: {}", 임시.display());
        let 이름 = 임시.file_name().unwrap().to_str().unwrap();
        assert!(!이름.contains('/') && !이름.contains('\\'), "실제: {이름}");
    }

    #[test]
    fn 이름이_한도를_꽉_채워도_255바이트를_안_넘는다() {
        // `safe_name`은 255바이트까지 채워 줄 수 있다. 꼬리를 그냥 붙이면 한도를 넘어
        // `ENAMETOOLONG`이 난다.
        let 긴_이름 = "가".repeat(85); // 3바이트 × 85 = 255바이트
        let 목적지 = Path::new("/inbox/a").join(&긴_이름);
        let 이름 = part_path(&목적지, "t1").file_name().unwrap().to_str().unwrap().to_string();
        assert!(이름.len() <= 255, "{}바이트", 이름.len());
        assert!(이름.ends_with(".part"), "실제: {이름}");
    }
}
