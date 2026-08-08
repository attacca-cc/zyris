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

/// 같은 밀리초에 같은 이름을 가진 다른 희생자가 들어와도 자리가 겹치지 않도록 붙이는 시작
/// 순번. **유일성을 보장하는 것은 이 카운터가 아니라 `stash`의 `create_dir` 재시도다** — 이
/// 카운터는 그저 매번 0부터 다시 찾는 낭비를 줄이는 힌트다. 프로세스 하나 안에서는 겹치는
/// 후보를 골라도 재시도가 다음 순번으로 넘어가므로 안전하고, 프로세스가 여러 개라 이 값
/// 자체가 서로 다르더라도(각자 0에서 시작) 마찬가지로 안전하다 — 파일시스템의 `mkdir`가
/// 최종 심판을 보기 때문이다.
static 다음_순번: AtomicU64 = AtomicU64::new(0);

/// `stash` 한 번이 자리를 찾으려고 재시도하는 최대 횟수. 이 안에서 못 찾으면 포기한다 —
/// 같은 밀리초·같은 순번 범위에 이만큼 몰릴 정도면 다른 문제(시계가 멈췄다든지)를 의심해야
/// 한다.
const 최대_시도: u32 = 64;

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

        let 자리 = self.자리_잡기(now_ms).await?;
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

    /// `{now_ms}-{순번}` 자리 하나를 새로 만들어 그 경로를 돌려준다.
    ///
    /// **`create_dir_all`이 아니라 `create_dir`을 쓰는 것이 이 함수의 요점이다.**
    /// `create_dir_all`은 자리가 이미 있어도 조용히 성공한다 — 그래서 예전 구현은 다른
    /// 프로세스가 이미 만들어 둔 자리를 그냥 통과했고, 뒤이은 `rename`이 그 안의 내용을
    /// 말없이 덮어썼다(카운터가 프로세스 안에서만 유일하므로, 다른 프로세스의 카운터도 0에서
    /// 시작해 같은 밀리초·같은 순번이 흔히 겹친다). `create_dir`은 이미 있으면 `AlreadyExists`로
    /// 실패한다 — 그 실패를 신호 삼아 다음 순번으로 넘어간다. 파일시스템의 `mkdir`이 원자적
    /// 이므로, 프로세스가 몇 개든 정확히 하나만 특정 자리를 차지하게 된다.
    async fn 자리_잡기(&self, now_ms: u64) -> Option<PathBuf> {
        // 루트 자체는 여러 자리가 공유하니 매번 만들어도(이미 있으면 성공) 안전하다 —
        // 경합이 벌어지는 것은 그 밑의 `{now_ms}-{순번}` 리프뿐이다.
        tokio::fs::create_dir_all(&self.root).await.ok()?;

        for _ in 0..최대_시도 {
            let 순번 = 다음_순번.fetch_add(1, Ordering::Relaxed);
            let 후보 = self.root.join(format!("{now_ms}-{순번}"));
            match tokio::fs::create_dir(&후보).await {
                Ok(()) => return Some(후보),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return None,
            }
        }
        None
    }
}
