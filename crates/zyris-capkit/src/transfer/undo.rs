//! 덮기 전에 원본을 옮겨 둔다.
//!
//! 복사가 아니라 **이동**인 이유는 디스크를 두 배 먹지 않기 위해서다. 그리고 **보관에 실패해도
//! 전송은 진행한다** — zyris-code의 `code_edit`이 같은 규칙이다. 안전망이 없다고 일을 막으면
//! 고칠 수 없는 상태가 생긴다.
//!
//! 다만 "보관에 실패한다"가 "조용히 앞선 백업을 지운다"로 번지면 안 된다. 그래서 자리 이름에는
//! 밀리초 말고 프로세스 내 순번도 붙는다(같은 밀리초·같은 이름의 백업이 겹치지 않도록), 심링크는
//! rename이 안 통하면 copy로 대체하지 않는다(가리키는 대상의 내용이 새어 나가므로), 그리고
//! copy는 됐는데 원본 삭제만 실패한 경우는 `None`이 아니라 이미 만든 백업 경로를 돌려준다
//! (그래야 존재하는 백업을 잃어버리지 않는다).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub struct UndoStore {
    root: PathBuf,
}

/// 같은 밀리초에 같은 이름을 가진 다른 희생자가 들어와도 자리가 겹치지 않도록 붙이는 순번.
/// 프로세스 안에서만 유일하면 충분하다 — 재시작 사이에 같은 밀리초가 같은 순번과 다시 만날
/// 확률은 무시할 만하다.
static 다음_순번: AtomicU64 = AtomicU64::new(0);

impl UndoStore {
    pub fn new(root: impl Into<PathBuf>) -> UndoStore {
        UndoStore { root: root.into() }
    }

    /// 원본을 보관 자리로 옮기고 간 자리를 돌려준다.
    ///
    /// 옮길 것이 없으면(파일이 없으면) `None`. 보관 자체를 못 만들었거나 아무것도 못 옮겼으면
    /// `None`. **예외 하나**: rename이 안 통해 copy로 넘어갔다가 원본 삭제(unlink)만 실패한
    /// 경우 — 그때는 백업이 이미 완전한 상태로 존재하므로 그 경로를 돌려준다. `None`은
    /// "백업이 전혀 없다"는 뜻이지 "원본이 사라졌으니 걱정 말라"는 뜻이 아니다.
    pub async fn stash(&self, victim: &Path, now_ms: u64) -> Option<PathBuf> {
        let 메타 = tokio::fs::symlink_metadata(victim).await.ok()?;
        let 이름 = victim.file_name()?;

        let 순번 = 다음_순번.fetch_add(1, Ordering::Relaxed);
        let 자리 = self.root.join(format!("{now_ms}-{순번}"));
        tokio::fs::create_dir_all(&자리).await.ok()?;
        let 목적지 = 자리.join(이름);

        // 같은 파일시스템이면 rename이 싸고, 심링크도 가리키는 자리 그대로 옮긴다.
        if tokio::fs::rename(victim, &목적지).await.is_ok() {
            return Some(목적지);
        }

        // rename이 안 통했다(다른 파일시스템이거나 권한 문제). 심링크를 copy로 옮기면 복사되는
        // 건 링크가 아니라 가리키는 대상의 *내용*이다 — 원본이 감추고 있던 걸 보관 자리에
        // 그대로 노출하게 된다. inbox.rs가 심링크를 아예 거부하는 것과 같은 이유로, 여기서도
        // copy 대체 경로는 타지 않고 포기한다.
        if 메타.file_type().is_symlink() {
            return None;
        }

        tokio::fs::copy(victim, &목적지).await.ok()?;
        // 여기까지 왔으면 백업은 이미 완전하다. 원본 삭제가 실패해도 결과를 숨기지 않는다 —
        // 조용히 `None`을 돌려주면 이미 존재하는 백업을 호출자가 영영 모르게 된다.
        let _ = tokio::fs::remove_file(victim).await;
        Some(목적지)
    }
}
