//! 받은 파일이 놓이는 자리. **감옥이다.**
//!
//! `zyris-capkit`의 `path::resolve_under`는 감옥이 아니다 — 주석부터가 "the root is a default,
//! not a jail"이고 절대경로가 그냥 통과한다. 심링크는 아예 다루지 않는다. 여기서는 받은 것을
//! 남의 머신에 쓰는 것이라 그 규칙을 쓸 수 없다.
//!
//! 방어가 둘이다. 이름 씻기(`super::name`)가 정상 경로를 지키고, 이 파일의 실제 경로 확인이
//! 씻기를 빠져나간 것을 잡는다. **하나만으로는 부족하다.**

use std::path::{Path, PathBuf};

use super::name::safe_name;

#[derive(Debug)]
pub enum InboxError {
    /// 최종 경로가 루트 밖이다.
    Escaped,
    /// 경로 조각 중 하나가 심볼릭 링크다.
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

    /// 최종 경로를 정하고 부모를 만들어 둔다.
    ///
    /// `peer_slug`도 씻는다 — 상대가 자기 slug를 마음대로 부를 수 있으므로 그것도 신뢰할 수
    /// 없는 입력이다.
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
        심링크_없는지(&뿌리, &부모).await?;

        // 부모까지는 실재하므로 canonicalize로 확인한다. 목적지 자체는 아직 없을 수 있어
        // 별도로 본다.
        let 실제_부모 =
            tokio::fs::canonicalize(&부모).await.map_err(|e| InboxError::Io(e.to_string()))?;
        if !실제_부모.starts_with(&뿌리) {
            return Err(InboxError::Escaped);
        }

        let 길 = 실제_부모.join(safe_name(proposed));
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
