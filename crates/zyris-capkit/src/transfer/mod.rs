//! 노드 간 파일 전송의 받는 쪽 살림살이.
//!
//! 받는 쪽에 사람 확인이 없다 — 에이전트가 도구로 부르면 상대 머신에 파일이 놓인다. 그래서
//! **경로 감옥·크기 제한·감사 로그가 유일한 방어선이다.** 이 모듈이 그 셋을 나눠 맡는다.
//!
//! | 모듈 | 맡는 것 |
//! |---|---|
//! | [`name`] | 제안된 이름 → 경로 조각 하나. 파일시스템을 안 탄다 |
//! | [`inbox`] | 자리 계산과 실제 경로 확인. 심링크를 거부한다 |
//! | [`undo`] | 덮기 전 원본을 옮겨 둔다 |
//! | [`audit`] | 전송 한 줄을 남긴다 |
//! | [`peer`] | 위를 엮어 `peer_transfer`를 구현한다 |
//!
//! `name`이 `inbox`에서 갈라져 있는 것은 일부러다 — 이름 씻기는 순수 판정이라 테이블 테스트로
//! 수십 개를 순식간에 돌린다. 섞어 두면 그 테스트가 전부 `tempfile`을 잡는다.

pub mod audit;
pub mod inbox;
pub mod name;
pub mod peer;
pub mod undo;

pub use audit::{Audit, AuditLine};
pub use inbox::{Inbox, InboxError};
pub use name::safe_name;
pub use peer::{LocalPeerTransfer, TransferConfig};
pub use undo::UndoStore;
