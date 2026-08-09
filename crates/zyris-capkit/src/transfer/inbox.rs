//! Where received files land. **It is a jail.**
//!
//! `zyris-capkit`'s `path::resolve_under` is not one — its own comment says "the root is a
//! default, not a jail", absolute paths sail straight through, and symlinks are not considered at
//! all. What happens here is writing what someone else sent onto their machine, so that rule
//! cannot be reused.
//!
//! There are two defenses. Name washing (`super::name`) guards the ordinary path, and this file's
//! real-path checks catch whatever slipped past the washing. **Either one alone is not enough.**

use std::path::{Path, PathBuf};

use super::name::safe_name;

#[derive(Debug)]
pub enum InboxError {
    /// The final path is outside the root.
    Escaped,
    /// One of the path's components is a symbolic link.
    SymlinkInPath,
    Io(String),
}

impl std::fmt::Display for InboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InboxError::Escaped => write!(f, "목적지가 inbox 밖입니다"),
            InboxError::SymlinkInPath => write!(f, "경로에 심볼릭 링크가 있습니다"),
            InboxError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for InboxError {}

pub struct Inbox {
    root: PathBuf,
}

impl Inbox {
    pub fn new(root: impl Into<PathBuf>) -> Inbox {
        Inbox { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Settles the final path and creates its parent directory.
    ///
    /// `peer_slug` gets washed too — the peer names itself, so that is untrusted input as much as
    /// the file name is.
    pub async fn resolve(&self, peer_slug: &str, proposed: &str) -> Result<PathBuf, InboxError> {
        let 부모 = self.root.join(safe_name(peer_slug));
        tokio::fs::create_dir_all(&부모).await.map_err(|e| InboxError::Io(e.to_string()))?;

        let 뿌리 = tokio::fs::canonicalize(&self.root)
            .await
            .map_err(|e| InboxError::Io(e.to_string()))?;

        // canonicalize는 링크를 따라가므로 링크 너머가 루트 밖이든 안이든 "루트 안"으로
        // 보일 수 있다 — 뒤이은 escape 검사보다 먼저 조각마다 직접 봐야 한다. 특히 링크가
        // 루트 "안"의 다른 자리(다른 상대의 디렉터리 등)를 가리키면 escape 검사만으로는
        // 절대 못 잡는다.
        //
        // 걷는 기준은 **정규화하지 않은** `self.root`다. `뿌리`(정규화한 것)를 기준으로 걸으면
        // `self.root`의 조상 중 하나라도 심링크일 때(macOS의 `/var`→`/private/var`,
        // 심링크로 된 홈 디렉터리 등) `부모.strip_prefix(뿌리)`가 실패해 `unwrap_or`가 절대경로
        // 전체를 돌려주고, 걷기가 파일시스템 루트부터 다시 시작돼 inbox 바깥의 모든 조상까지
        // 심링크로 걸린다 — 정상적인 요청이 전부 거부된다. `부모`는 `self.root.join(..)`이라
        // `self.root` 기준으로는 항상 `strip_prefix`가 성립하므로, 걷기가 inbox 안에 새로
        // 붙인 조각만 본다. inbox 자체(또는 그 조상)가 심링크인 것은 받는 사람의 설정이라
        // 허용하고, inbox **안**의 조각이 심링크인 것만 거부한다.
        심링크_없는지(&self.root, &부모).await?;

        // 부모까지는 실재하므로 canonicalize로 확인한다. 목적지 자체는 아직 없을 수 있어
        // 별도로 본다.
        let 실제_부모 =
            tokio::fs::canonicalize(&부모).await.map_err(|e| InboxError::Io(e.to_string()))?;
        if !실제_부모.starts_with(&뿌리) {
            return Err(InboxError::Escaped);
        }

        let 길 = 실제_부모.join(safe_name(proposed));
        // 이 시점에 `실제_부모`는 이미 `뿌리` 안임이 확인됐고 `safe_name`은 구분자 없는 조각
        // 하나만 돌려주므로, 이 검사가 실제로 걸릴 길은 없다. 방어선을 하나 더 두는 것뿐이니
        // 나중에 이걸 보고 핵심 방어라고 착각하지 말 것 — 핵심은 위의 `심링크_없는지`다.
        if !길.starts_with(&뿌리) {
            return Err(InboxError::Escaped);
        }
        // 목적지가 이미 링크면 쓰는 순간 링크가 가리키는 곳에 쓰인다.
        if let Ok(정보) = tokio::fs::symlink_metadata(&길).await {
            if 정보.file_type().is_symlink() {
                return Err(InboxError::SymlinkInPath);
            }
        }
        Ok(길)
    }
}

/// `뿌리`부터 `길`까지 내려가며 조각마다 심링크인지 본다.
async fn 심링크_없는지(뿌리: &Path, 길: &Path) -> Result<(), InboxError> {
    let 나머지 = 길.strip_prefix(뿌리).unwrap_or(길);
    let mut 지금 = 뿌리.to_path_buf();
    for 조각 in 나머지.components() {
        지금.push(조각);
        let 정보 = tokio::fs::symlink_metadata(&지금)
            .await
            .map_err(|e| InboxError::Io(e.to_string()))?;
        if 정보.file_type().is_symlink() {
            return Err(InboxError::SymlinkInPath);
        }
    }
    Ok(())
}
