//! 덮기 전에 원본을 옮겨 둔다.
//!
//! 복사가 아니라 **이동**인 이유는 디스크를 두 배 먹지 않기 위해서다. 그리고 **보관에 실패해도
//! 전송은 진행한다** — zyris-code의 `code_edit`이 같은 규칙이다. 안전망이 없다고 일을 막으면
//! 고칠 수 없는 상태가 생긴다.

use std::path::{Path, PathBuf};

pub struct UndoStore {
    root: PathBuf,
}

impl UndoStore {
    pub fn new(root: impl Into<PathBuf>) -> UndoStore {
        UndoStore { root: root.into() }
    }

    /// 원본을 보관 자리로 옮기고 간 자리를 돌려준다. 옮길 것이 없거나 못 옮기면 `None`.
    pub async fn stash(&self, victim: &Path, now_ms: u64) -> Option<PathBuf> {
        if tokio::fs::symlink_metadata(victim).await.is_err() {
            return None;
        }
        let 이름 = victim.file_name()?;
        let 자리 = self.root.join(now_ms.to_string());
        tokio::fs::create_dir_all(&자리).await.ok()?;
        let 목적지 = 자리.join(이름);

        // 같은 파일시스템이면 rename이 싸다. 다르면 복사 후 지운다.
        if tokio::fs::rename(victim, &목적지).await.is_ok() {
            return Some(목적지);
        }
        tokio::fs::copy(victim, &목적지).await.ok()?;
        tokio::fs::remove_file(victim).await.ok()?;
        Some(목적지)
    }
}
