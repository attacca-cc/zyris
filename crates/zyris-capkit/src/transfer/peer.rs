//! `peer_transfer`의 참조 구현. 한 타입이 양쪽 역할을 다 한다 — 보내는 쪽은 `pull`에 답하고,
//! 받는 쪽은 `push_offer`에 답한다.
//!
//! **무결성은 여기서 한다.** 엔진의 `s_end.trailer.sha256`은 받는 쪽이 버리므로(connection.rs가
//! `Envelope::SEnd { stream, .. }`로 구조분해한다) 믿을 수 없다.

use std::path::PathBuf;
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

const 청크: usize = 256 * 1024;

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

/// **손잡이는 나중에 꽂는다.** 받는 쪽이 `pull`을 되부르려면 `PeerTransferClient`가 있어야
/// 하는데, 그것은 `Node::accept`가 끝나고 상대가 capability를 announce한 뒤에야 생긴다.
/// 그런데 `Node`를 만들려면 이 구조체가 **먼저** 있어야 한다 — 순환이다. 그래서 `peer`는
/// 내부 가변이고 `set_peer`가 나중에 채운다. 이 순서를 모르고 생성자 인자로 만들려 하면
/// 컴파일이 안 되는 것이 아니라 배선이 불가능하다.
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
    /// 받는 쪽. 손잡이는 `set_peer`로 나중에 꽂는다.
    pub fn receiver_pending(config: TransferConfig, peer_slug: String) -> LocalPeerTransfer {
        LocalPeerTransfer {
            config,
            peer: Arc::new(std::sync::OnceLock::new()),
            peer_slug,
            pending: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        }
    }

    /// 상대에게 `pull`을 부를 손잡이를 꽂는다. 두 번째 호출은 조용히 무시된다 — 한 연결에
    /// 손잡이는 하나뿐이고, 덮어쓸 수 있으면 그것이 곧 갈아 끼우는 자리가 된다.
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

    /// 보내는 쪽이 무엇을 내줄지 예약한다. `send_to`가 `push_offer` 직전에 부른다.
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

        let 이미_있나 = tokio::fs::symlink_metadata(&목적지).await.is_ok();
        if 이미_있나 && !offer.overwrite {
            return Err(WireError::new(
                ErrorCode::InvalidParams,
                format!("{}이(가) 이미 있습니다. 덮으려면 overwrite를 켜세요", 목적지.display()),
            )
            .retriable(false));
        }

        // 임시 파일은 같은 디렉터리 안이어야 rename이 원자적이다.
        let 임시 = 목적지.with_extension("part");
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
        let 받은_offset = tokio::fs::metadata(&임시).await.map(|m| m.len()).unwrap_or(0);
        let 받은_offset = if 받은_offset > offer.size { 0 } else { 받은_offset };

        let mut 스트림 = peer.pull(offer.transfer_id.clone(), 받은_offset).await?;
        if 스트림.head.sha256 != offer.sha256 || 스트림.head.size != offer.size {
            return Err(WireError::internal(
                "보내는 쪽이 offer와 다른 것을 내주려 합니다".to_string(),
            ));
        }

        let mut 해시기 = Sha256::new();
        if 받은_offset > 0 {
            // 이어받기라면 이미 받아 둔 부분을 다시 읽어 해시에 넣는다.
            let 앞부분 = tokio::fs::read(&임시).await.map_err(io_오류)?;
            해시기.update(&앞부분);
        }
        let mut 파일 = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&임시)
            .await
            .map_err(io_오류)?;

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
            ));
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
