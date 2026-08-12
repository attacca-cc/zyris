# 노드 간 P2P 파일 전송 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 서로 다른 머신의 두 Zyris 노드가 attacca를 거치지 않고 파일을 직접 주고받는다.

**Architecture:** iroh(QUIC + 홀펀칭)로 A와 B 사이에 암호화된 전송로를 세우고, 그 위에 기존
`zyris::Connection`을 **코어 수정 없이** 그대로 얹는다. attacca는 주소록과 릴레이 역할만 한다.
바이트는 받는 쪽이 당긴다(pull) — 미는 경로가 와이어에 없기 때문이다.

**이 접근은 실측으로 확인됐다.** 리포 밖 크레이트에서 raw TCP `Transport` 어댑터를 47줄로 만들어
노드 둘이 전 과정(핸드셰이크·양방향 announce·4.2MB `uni_stream`·자동 detach와 sha256 재조립)을
`crates/zyris` 수정 없이 주고받는 것을 확인했다. 사본이 `~/zyris-p2p-probe-20260809/`에 있다 —
**Task 2.5를 시작하기 전에 읽어 볼 것.** 프레이밍과 양쪽 배선의 모양이 그대로 들어 있다.

**Tech Stack:** Rust, iroh 1.x, zyris/zyris-proto/zyris-macros, tokio, sha2, tempfile.

**설계 정본:** `docs/superpowers/specs/2026-08-09-p2p-file-transfer-design.md`. 여기 없는 "왜"는
전부 거기 있다.

## Global Constraints

- **커밋에 Claude를 저자로 남기지 않는다.** `Co-Authored-By: Claude …`·`Claude-Session:` 트레일러
  금지, 본문에 "Generated with Claude Code" 류 금지, PR 본문에 배너·세션 링크 금지.
  커밋 직전 `git log -1 --format=%B`로 눈으로 확인한다.
- **작업 전 upstream 최신을 받는다.** `git fetch origin && git switch main && git pull --ff-only`
  후 `git switch -c <브랜치>`. `main`에 직접 커밋하지 않는다.
- **zyris는 Conventional Commits를 훅이 강제한다** (`.cargo-husky/hooks/commit-msg`,
  `cargo test`가 처음 돌 때 설치됨). `--no-verify`로 우회하지 않는다.
- **attacca는 무조건 PR.** `main`에 직접 push 금지. zyris는 push 가능하되 브랜치를 딴 뒤
  `origin/main`에 새 커밋이 생겼으면 PR을 낸다.
- **attacca에서 `cargo fmt`를 돌리지 않는다** (`--check`도, `-p`로 좁혀도). `rustfmt.toml`이
  없는데 코드가 ~150컬럼이라 크레이트 전체가 재포맷된다. 새 코드는 주변 줄 폭에 눈으로 맞춘다.
  zyris에도 `rustfmt.toml`이 없다 — 같은 규칙이다.
- **`cargo`는 `-j2` 이하, `timeout`을 붙인다.** 이 머신은 RAM 3.6GB / 4스레드다. 기본 `-j4`는
  링크 단계에서 RAM을 다 먹는다. 고아 테스트 바이너리는 `pgrep -af 'deps/'`로 확인하고 죽인다.
- **배경 작업을 쌓지 않는다.** `cargo test`가 여럿 겹치면 머신이 통째로 멈춘다.
- **TDD.** 새 테스트는 **빨간불을 한 번 본다.** 중요한 불변식은 일부러 구현을 망가뜨려 테스트가
  진짜 무는지 확인한다.
- **코드에 쓰는 글은 영어다** (2026-08-09 변경, 이전 판의 "한국어" 규칙을 대체한다).
  - 주석(`//`·`///`·`//!`), 테스트 함수 이름, `assert!`/`expect` 메시지 — **영어**
  - `zyris-p2p`는 새 크레이트다. **변수·함수·타입 이름도 처음부터 영어로 쓴다.**
    기존 크레이트에 남은 한글 식별자를 흉내 내지 말 것.
  - **아래 태스크의 코드 블록에는 한글 주석과 한글 식별자가 그대로 남아 있다.** 플랜을 쓸 때의
    규칙이 그랬기 때문이다. **베끼지 말고 영어로 옮겨 쓴다.** 로직·값·구조는 그대로 둔다.
  - 예외 둘: 상대 노드나 사용자에게 가는 **오류 메시지 문자열**, 그리고 테스트가 실제로
    비교하는 **한국어 테스트 데이터**(`"진짜 내용".as_bytes()` 등)는 한국어를 유지한다.
  - 이 플랜과 스펙 등 `docs/` 아래 문서는 한국어 그대로다.
- **`zyris-p2p`는 기본 feature가 아니다.** P2P를 안 쓰는 노드가 iroh를 컴파일하지 않아야 한다.
- **`b"한글"`은 컴파일되지 않는다.** byte string 리터럴은 ASCII만 받는다 — `"한글".as_bytes()`.
- **한글 식별자에 대문자 ASCII 낱말을 섞지 말 것.** `..._None일_...` 같은 테스트 이름은
  `non_snake_case`에 걸린다. 한글만 있으면 안 걸린다.
- **`Node::builder()`의 도구 등록은 `.capability(...)`다** — `.serve(...)`는 없다. 그리고
  **`.build()`는 `Result<Node>`를 준다**(`.unwrap()`이나 `?`가 필요하다). 정본은
  `crates/zyris/tests/attachments.rs:62-73`이다.

### iroh 1.0.3 API — 실물에서 대조한 것 (2026-08-09)

**아래는 벤더된 소스를 직접 열어 확인했다.** 이 플랜에서 내가 기억으로 적은 시그니처가
여러 번 틀렸으므로, 여기 없는 iroh API를 쓰기 전에는 반드시 실물을 먼저 열어 볼 것:
`~/.cargo/registry/src/index.crates.io-*/iroh-1.0.3/src/`.

```rust
// 크레이트 루트에서 재노출된다 — iroh_base를 직접 의존할 필요가 없다.
iroh::{EndpointAddr, EndpointId, PublicKey, SecretKey, RelayUrl, Signature, TransportAddr}
iroh::{Endpoint, RelayMode}          // endpoint 모듈에서
iroh::{RelayConfig, RelayMap}        // iroh_relay에서

SecretKey::generate() -> SecretKey   // 인자 없음
secret.public()   -> PublicKey
secret.to_bytes() -> [u8; 32]
SecretKey::from_bytes(&[u8; 32]) -> SecretKey

enum RelayMode { Disabled, Default, Staging, Custom(RelayMap) }

Endpoint::id(&self) -> EndpointId    // ← `endpoint_id()`가 **아니다**
```

## 단계는 저마다 혼자 선다

| 단계 | 무엇을 만드나 | 무엇 없이 테스트되나 |
|---|---|---|
| 1 | 파일 전송 capability와 감옥 | **iroh도 attacca도 필요 없다.** `zyris::testing::duplex()`로 전부 검증된다 |
| 2 | `zyris-p2p` 전송로 | **attacca가 필요 없다.** 로컬 iroh 엔드포인트 둘로 검증된다 |
| 3 | attacca 랑데부 | 노드 변경과 독립. 배포가 먼저 가야 한다 |
| 4 | 배선과 라이브 검증 | 앞의 셋이 다 필요하다 |

**1과 2는 순서가 없다.** 3은 배포 리드타임이 있으므로 일찍 시작하는 편이 좋다.

---

## File Structure

### 단계 1 (zyris 리포)

| 파일 | 책임 |
|---|---|
| `crates/zyris-caps/src/peer_transfer.rs` | A↔B 와이어 선언. `push_offer`·`pull`과 그 타입 |
| `crates/zyris-caps/src/file_transfer.rs` | 에이전트가 부르는 선언. `send_to`·`inbox_list` |
| `crates/zyris-caps/src/lib.rs` | 모듈 등록과 re-export (수정) |
| `crates/zyris-capkit/src/transfer/name.rs` | **순수 함수.** 제안된 이름 → 안전한 파일 이름 |
| `crates/zyris-capkit/src/transfer/inbox.rs` | inbox 자리 계산, 경로 확인, 심링크 거부, 원자적 쓰기 |
| `crates/zyris-capkit/src/transfer/undo.rs` | 덮기 전 원본 이동, 보관 정리 |
| `crates/zyris-capkit/src/transfer/audit.rs` | 전송 한 줄 append |
| `crates/zyris-capkit/src/transfer/peer.rs` | `LocalPeerTransfer` — `push_offer`·`pull` 구현 |
| `crates/zyris-capkit/src/transfer/mod.rs` | 위를 묶고 `TransferConfig`를 둔다 |

**왜 `name.rs`가 따로인가**: 이름 씻기는 파일시스템을 안 타는 순수 판정이라 테이블 테스트로
수십 개를 순식간에 돌릴 수 있다. `inbox.rs`에 섞으면 그 테스트가 전부 `tempfile`을 잡는다.

### 단계 2 (zyris 리포)

| 파일 | 책임 |
|---|---|
| `crates/zyris-p2p/src/key.rs` | ed25519 키페어 생성·0600 저장·모드 검사 |
| `crates/zyris-p2p/src/frame.rs` | **순수 함수.** 길이 접두 프레이밍 인코드/디코드 |
| `crates/zyris-p2p/src/transport.rs` | `IrohTransport` — QUIC bi-stream ↔ zyris `Transport` |
| `crates/zyris-p2p/src/tofu.rs` | 상대 EndpointId 고정과 대조 |
| `crates/zyris-p2p/src/lookup.rs` | `AttaccaLookup` — iroh `AddressLookup` 구현 |
| `crates/zyris-p2p/src/peer.rs` | Endpoint 수립, 다이얼, 수락 루프 |

### 단계 3 (양쪽 리포)

| 파일 | 리포 | 책임 |
|---|---|---|
| `crates/zyris-attacca/src/lib.rs` | zyris | `peer_publish`·`peer_lookup`·`peer_list` 선언, `ZScope::PeersWrite` |
| `crates/attacca-repo-pg/migrations/XXXX_zyris_peer.sql` | attacca | 컬럼 셋 |
| `crates/attacca-domain/src/…` | attacca | `ApiScope::PeersWrite` |
| `crates/attacca-server/src/zyris_gateway.rs` | attacca | 세 메서드 구현 |
| `deploy/helm/attacca/…` | attacca | `iroh-relay` |

---

# 단계 1 — 파일 전송 capability와 감옥

## Task 1.1: 이름 씻기 (순수)

**Files:**
- Create: `crates/zyris-capkit/src/transfer/name.rs`
- Create: `crates/zyris-capkit/src/transfer/mod.rs`
- Modify: `crates/zyris-capkit/src/lib.rs`
- Modify: `crates/zyris-capkit/Cargo.toml`

**Interfaces:**
- Produces: `pub fn safe_name(proposed: &str) -> String` — 언제나 경로 조각 하나를 돌려준다.
  비거나 위험하면 `"file"`.

- [ ] **Step 1: 실패하는 테스트를 쓴다**

`crates/zyris-capkit/src/transfer/name.rs` 맨 아래에:

```rust
#[cfg(test)]
mod tests {
    use super::safe_name;

    #[test]
    fn 경로_조각_하나만_남는다() {
        let 표 = [
            ("report.pdf", "report.pdf"),
            // 구분자를 지우는 것이 아니라 **마지막 조각만 취한다.** 지우기만 하면
            // `../../etc/passwd`가 `....etcpasswd`가 되어 안전하지만 읽을 수 없다.
            ("../../etc/passwd", "passwd"),
            ("/etc/passwd", "passwd"),
            ("a/b/c.txt", "c.txt"),
            (r"C:\Windows\system32\cmd.exe", "cmd.exe"),
            ("..", "file"),
            (".", "file"),
            ("", "file"),
            ("   ", "file"),
            ("a\0b.txt", "ab.txt"),
            ("a\nb.txt", "ab.txt"),
            // 유니코드 전각 마침표는 `..`로 정규화되지 않지만 구분자도 아니다. 남긴다.
            ("．．", "．．"),
            // 윈도우 예약 이름은 확장자가 붙어도 예약이다.
            ("CON", "CON_"),
            ("con.txt", "con_.txt"),
            ("LPT1.log", "LPT1_.log"),
            // 끝의 마침표와 공백은 윈도우가 조용히 지운다.
            ("name.", "name"),
            ("name ", "name"),
        ];
        for (넣은_것, 나올_것) in 표 {
            assert_eq!(safe_name(넣은_것), 나올_것, "입력: {넣은_것:?}");
        }
    }

    #[test]
    fn 아무리_길어도_255바이트를_안_넘는다() {
        let 긴_것 = "가".repeat(300);
        let 나온_것 = safe_name(&긴_것);
        assert!(나온_것.len() <= 255, "{}바이트", 나온_것.len());
        // 문자 경계에서 잘라야 한다 — 바이트로 자르면 패닉한다.
        assert!(나온_것.chars().all(|c| c == '가'));
    }

    #[test]
    fn 확장자가_있으면_자를_때도_남긴다() {
        let 긴_것 = format!("{}.tar.gz", "a".repeat(300));
        let 나온_것 = safe_name(&긴_것);
        assert!(나온_것.len() <= 255);
        assert!(나온_것.ends_with(".gz"), "실제: {나온_것}");
    }
}
```

- [ ] **Step 2: 빨간불 확인**

```bash
cd /home/ruma/zyris
timeout 300 cargo test -j2 -p zyris-capkit --features transfer name:: 2>&1 | tail -20
```

기대: `cannot find function safe_name` 로 컴파일 실패.

- [ ] **Step 3: 구현**

`crates/zyris-capkit/src/transfer/name.rs` 위쪽에:

```rust
//! 보내는 쪽이 제안한 이름을 받는 쪽이 쓸 수 있는 경로 조각 하나로 씻는다.
//!
//! 거부가 아니라 제거인 이유: 이름 하나 때문에 전송이 통째로 실패할 이유가 없다. 씻어도
//! 남는 것이 없을 때만 기본 이름을 준다.
//!
//! **이것만으로 안전해지지 않는다.** 씻기는 정상 경로를 지키는 것이고, 씻기를 빠져나간 것은
//! `inbox::resolve`의 실제 경로 확인이 잡는다. 둘 다 있어야 한다.

/// 윈도우가 장치로 예약한 이름들. 확장자가 붙어도 예약이라 `con.txt`도 걸린다.
const 예약어: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

const 최대_바이트: usize = 255;

pub fn safe_name(proposed: &str) -> String {
    // **마지막 조각만 취한다.** 구분자를 지우기만 하면 `../../etc/passwd`가
    // `....etcpasswd`가 되는데, 안전하기는 해도 사람이 무엇인지 알아볼 수 없다.
    // 윈도우 경로도 같이 받으므로 `/`와 `\` 둘 다 구분자로 본다.
    let 마지막_조각 = proposed.rsplit(['/', '\\']).next().unwrap_or(proposed);
    let 씻은_것: String =
        마지막_조각.chars().filter(|&c| c != ':' && !c.is_control()).collect();
    let 씻은_것 = 씻은_것.trim();
    // 끝의 마침표와 공백은 윈도우가 조용히 지운다. 우리가 먼저 지워 양쪽 이름을 같게 둔다.
    let 씻은_것 = 씻은_것.trim_end_matches(['.', ' ']);

    if 씻은_것.is_empty() || 씻은_것 == "." || 씻은_것 == ".." {
        return "file".to_string();
    }

    let 씻은_것 = 예약어_회피(씻은_것);
    길이_맞추기(&씻은_것)
}

/// 예약어면 줄기 끝에 `_`를 붙인다. 확장자는 그대로 둔다 — 사람이 무엇이었는지 알아야 한다.
fn 예약어_회피(이름: &str) -> String {
    let (줄기, 확장자) = match 이름.split_once('.') {
        Some((줄기, 나머지)) => (줄기, Some(나머지)),
        None => (이름, None),
    };
    let 걸렸나 = 예약어.iter().any(|r| r.eq_ignore_ascii_case(줄기));
    if !걸렸나 {
        return 이름.to_string();
    }
    match 확장자 {
        Some(확장자) => format!("{줄기}_.{확장자}"),
        None => format!("{줄기}_"),
    }
}

/// 파일시스템 한도에 맞춘다. **문자 경계에서 자른다** — 바이트로 자르면 한글에서 패닉한다.
fn 길이_맞추기(이름: &str) -> String {
    if 이름.len() <= 최대_바이트 {
        return 이름.to_string();
    }
    // 확장자는 살린다. 무엇이었는지 알아보는 유일한 단서다.
    let 확장자 = 이름.rsplit_once('.').map(|(_, e)| format!(".{e}")).unwrap_or_default();
    let 확장자 = if 확장자.len() < 16 { 확장자 } else { String::new() };
    let 남길_바이트 = 최대_바이트 - 확장자.len();

    let mut 줄기 = String::new();
    for c in 이름.chars() {
        if 줄기.len() + c.len_utf8() > 남길_바이트 {
            break;
        }
        줄기.push(c);
    }
    format!("{줄기}{확장자}")
}
```

`crates/zyris-capkit/src/transfer/mod.rs`:

```rust
//! 노드 간 파일 전송의 받는 쪽 살림살이.

pub mod name;

pub use name::safe_name;
```

`crates/zyris-capkit/src/lib.rs`에 한 줄:

```rust
#[cfg(feature = "transfer")]
pub mod transfer;
```

`crates/zyris-capkit/Cargo.toml`의 `[features]`에:

```toml
# 노드 간 파일 전송의 받는 쪽. 파일시스템만 만지므로 그래픽 의존이 없다.
transfer = ["dep:sha2", "dep:hex"]
```

`[dependencies]`에:

```toml
sha2 = { version = "0.10", optional = true }
hex = { version = "0.4", optional = true }
```

- [ ] **Step 4: 초록불 확인**

```bash
timeout 300 cargo test -j2 -p zyris-capkit --features transfer name:: 2>&1 | tail -20
```

기대: 3 passed.

- [ ] **Step 5: 일부러 망가뜨려 테스트가 무는지 본다**

`마지막_조각`을 구하는 줄을 `let 마지막_조각 = proposed;`로 잠깐 바꾸고 다시 돌린다.
`../../etc/passwd` 줄이 `....etcpasswd`를 내며 실패해야 한다. 확인했으면 되돌린다.

- [ ] **Step 6: 커밋**

```bash
git add crates/zyris-capkit/src/transfer crates/zyris-capkit/src/lib.rs crates/zyris-capkit/Cargo.toml
git commit -m "feat(transfer): 제안된 파일 이름을 경로 조각 하나로 씻는다"
git log -1 --format=%B   # 트레일러가 안 붙었는지 눈으로 확인
```

---

## Task 1.2: inbox 감옥

**Files:**
- Create: `crates/zyris-capkit/src/transfer/inbox.rs`
- Create: `crates/zyris-capkit/tests/inbox_jail.rs`
- Modify: `crates/zyris-capkit/src/transfer/mod.rs`

**Interfaces:**
- Consumes: `name::safe_name`
- Produces:
  - `pub struct Inbox { root: PathBuf }`
  - `pub fn new(root: impl Into<PathBuf>) -> Inbox`
  - `pub async fn resolve(&self, peer_slug: &str, proposed: &str) -> Result<PathBuf, InboxError>`
    — 최종 경로. 부모 디렉터리는 만들어 두고 돌려준다.
  - `pub enum InboxError { Escaped, SymlinkInPath, Io(String) }`

- [ ] **Step 1: 실패하는 테스트를 쓴다**

`crates/zyris-capkit/tests/inbox_jail.rs`:

```rust
//! 감옥은 실제 파일시스템으로만 판정한다. 순수 함수가 못 보는 것이 여기 있다 — 심링크.

use zyris_capkit::transfer::inbox::{Inbox, InboxError};

fn 임시_inbox() -> (tempfile::TempDir, Inbox) {
    let 자리 = tempfile::tempdir().unwrap();
    let inbox = Inbox::new(자리.path());
    (자리, inbox)
}

#[tokio::test]
async fn 보낸_노드마다_제_디렉터리를_받는다() {
    let (자리, inbox) = 임시_inbox();
    let 길 = inbox.resolve("arch-zyris-code", "report.pdf").await.unwrap();
    assert_eq!(길, 자리.path().join("arch-zyris-code").join("report.pdf"));
    assert!(길.parent().unwrap().is_dir(), "부모를 미리 만들어 둬야 한다");
}

#[tokio::test]
async fn 경로_탈출은_이름_단계에서_이미_막힌다() {
    let (자리, inbox) = 임시_inbox();
    let 길 = inbox.resolve("peer", "../../etc/passwd").await.unwrap();
    assert!(길.starts_with(자리.path()), "실제: {}", 길.display());
    // `safe_name`이 마지막 조각만 취하므로 `passwd`다 (Task 1.1).
    assert_eq!(길.file_name().unwrap(), "passwd");
}

#[tokio::test]
async fn 보낸_노드_이름도_씻는다() {
    let (자리, inbox) = 임시_inbox();
    // 상대가 slug를 마음대로 부를 수 있으므로 그것도 조각 하나여야 한다.
    let 길 = inbox.resolve("../../..", "a.txt").await.unwrap();
    assert!(길.starts_with(자리.path()), "실제: {}", 길.display());
}

#[tokio::test]
async fn 부모가_심링크면_거부한다() {
    let (자리, inbox) = 임시_inbox();
    let 밖 = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(밖.path(), 자리.path().join("peer")).unwrap();

    let 결과 = inbox.resolve("peer", "a.txt").await;
    assert!(matches!(결과, Err(InboxError::SymlinkInPath)), "실제: {결과:?}");
}

#[tokio::test]
async fn 목적지_자체가_심링크여도_거부한다() {
    let (자리, inbox) = 임시_inbox();
    let 밖 = tempfile::tempdir().unwrap();
    std::fs::create_dir(자리.path().join("peer")).unwrap();
    std::os::unix::fs::symlink(밖.path().join("훔친다"), 자리.path().join("peer").join("a.txt"))
        .unwrap();

    let 결과 = inbox.resolve("peer", "a.txt").await;
    assert!(matches!(결과, Err(InboxError::SymlinkInPath)), "실제: {결과:?}");
}
```

`crates/zyris-capkit/Cargo.toml`의 `[dev-dependencies]`에 `tempfile.workspace = true`가 있는지
확인하고 없으면 더한다.

- [ ] **Step 2: 빨간불 확인**

```bash
timeout 300 cargo test -j2 -p zyris-capkit --features transfer --test inbox_jail 2>&1 | tail -20
```

기대: `unresolved import zyris_capkit::transfer::inbox` 로 컴파일 실패.

- [ ] **Step 3: 구현**

`crates/zyris-capkit/src/transfer/inbox.rs`:

```rust
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

        // 부모까지는 실재하므로 canonicalize로 확인한다. 목적지 자체는 아직 없을 수 있어
        // 별도로 본다.
        let 실제_부모 =
            tokio::fs::canonicalize(&부모).await.map_err(|e| InboxError::Io(e.to_string()))?;
        if !실제_부모.starts_with(&뿌리) {
            return Err(InboxError::Escaped);
        }
        // canonicalize는 링크를 따라가므로 "루트 안"이 되어 통과할 수 있다. 조각마다 직접 본다.
        //
        // **걷는 기준은 `self.root`(비정규)여야 한다.** `뿌리`(정규)를 주면, inbox 조상 어딘가에
        // 심링크가 있을 때 — macOS의 `/var → /private/var`, 심링크된 홈 — `strip_prefix`가
        // 실패해 `unwrap_or`가 절대경로 전체를 돌려주고, `PathBuf::push`가 `RootDir`에서
        // 경로를 갈아치워 **`/`부터 모든 조상을 검사한다.** 멀쩡한 전송이 전부 거부된다.
        // `부모`는 `self.root.join(..)`이라 `self.root`로는 strip_prefix가 언제나 성공한다.
        //
        // 결과로 inbox **조상**의 심링크는 허용되고(받는 사람 자기 설정이다) 루트 **아래**의
        // 심링크만 거부된다. 그것이 맞는 경계다.
        심링크_없는지(&self.root, &부모).await?;

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
```

`mod.rs`에 `pub mod inbox;`를 더한다.

- [ ] **Step 4: 초록불 확인**

```bash
timeout 300 cargo test -j2 -p zyris-capkit --features transfer --test inbox_jail 2>&1 | tail -20
```

기대: 5 passed.

- [ ] **Step 5: 일부러 망가뜨려 본다**

`심링크_없는지` 호출을 잠깐 주석 처리하고 돌린다. `부모가_심링크면_거부한다`가 실패해야 한다.
확인했으면 되돌린다. **이게 이 계획에서 제일 중요한 확인이다** — 이 검사가 없으면 남의 머신
어디에나 쓸 수 있다.

- [ ] **Step 6: 커밋**

```bash
git add crates/zyris-capkit/src/transfer crates/zyris-capkit/tests/inbox_jail.rs
git commit -m "feat(transfer): inbox를 감옥으로 만든다 — 심링크와 탈출을 막는다"
```

---

## Task 1.3: 덮어쓰기와 되돌리기

**Files:**
- Create: `crates/zyris-capkit/src/transfer/undo.rs`
- Create: `crates/zyris-capkit/tests/undo.rs`
- Modify: `crates/zyris-capkit/src/transfer/mod.rs`

**Interfaces:**
- Produces:
  - `pub struct UndoStore { root: PathBuf }`
  - `pub fn new(root: impl Into<PathBuf>) -> UndoStore`
  - `pub async fn stash(&self, victim: &Path, now_ms: u64) -> Option<PathBuf>` — 원본을 옮기고
    간 자리를 준다. **실패해도 `None`이지 오류가 아니다.**

- [ ] **Step 1: 실패하는 테스트를 쓴다**

`crates/zyris-capkit/tests/undo.rs`:

```rust
use zyris_capkit::transfer::undo::UndoStore;

#[tokio::test]
async fn 원본을_옮기고_간_자리를_알려_준다() {
    let 보관 = tempfile::tempdir().unwrap();
    let 작업 = tempfile::tempdir().unwrap();
    let 희생자 = 작업.path().join("a.pdf");
    tokio::fs::write(&희생자, "예전 것".as_bytes()).await.unwrap();

    let store = UndoStore::new(보관.path());
    let 간_자리 = store.stash(&희생자, 1_754_700_000_000).await.unwrap();

    assert!(!희생자.exists(), "복사가 아니라 이동이어야 한다");
    assert_eq!(tokio::fs::read(&간_자리).await.unwrap(), "예전 것".as_bytes());
    assert!(간_자리.starts_with(보관.path()));
}

#[tokio::test]
async fn 없는_파일은_옮길_것이_없다() {
    let 보관 = tempfile::tempdir().unwrap();
    let 작업 = tempfile::tempdir().unwrap();
    let store = UndoStore::new(보관.path());
    assert!(store.stash(&작업.path().join("없음"), 1).await.is_none());
}

#[tokio::test]
async fn 보관에_실패해도_None일_뿐_패닉하지_않는다() {
    // 쓸 수 없는 자리를 준다. 안전망이 없다고 전송을 막으면 고칠 수 없는 상태가 생긴다.
    let 작업 = tempfile::tempdir().unwrap();
    let 희생자 = 작업.path().join("a.pdf");
    tokio::fs::write(&희생자, b"x").await.unwrap();

    let store = UndoStore::new("/proc/못쓰는자리");
    assert!(store.stash(&희생자, 1).await.is_none());
    assert!(희생자.exists(), "못 옮겼으면 원본은 그대로 있어야 한다");
}
```

- [ ] **Step 2: 빨간불 확인**

```bash
timeout 300 cargo test -j2 -p zyris-capkit --features transfer --test undo 2>&1 | tail -20
```

- [ ] **Step 3: 구현**

`crates/zyris-capkit/src/transfer/undo.rs`:

```rust
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
```

`mod.rs`에 `pub mod undo;`를 더한다.

- [ ] **Step 4: 초록불 확인**

```bash
timeout 300 cargo test -j2 -p zyris-capkit --features transfer --test undo 2>&1 | tail -20
```

기대: 3 passed.

- [ ] **Step 5: 커밋**

```bash
git add crates/zyris-capkit/src/transfer crates/zyris-capkit/tests/undo.rs
git commit -m "feat(transfer): 덮기 전 원본을 되돌림 자리로 옮긴다"
```

---

## Task 1.4: `peer_transfer` 선언

**Files:**
- Create: `crates/zyris-caps/src/peer_transfer.rs`
- Modify: `crates/zyris-caps/src/lib.rs`

**Interfaces:**
- Produces: `peer_transfer_capability()`, `PeerTransfer`, `PeerTransferServer<T>`,
  `PeerTransferClient`, `TransferOffer`, `TransferDone`, `PullHead`.

- [ ] **Step 1: 선언을 쓴다**

`crates/zyris-caps/src/peer_transfer.rs`:

```rust
//! A와 B **사이의** 와이어. 피어 링크에서만 announce된다.
//!
//! 에이전트가 부르는 표면은 `file_transfer`이고 이것과 다르다. 둘을 갈라 둔 덕에 "피어 링크는
//! 이것 하나만 연다"가 필터링 로직이 아니라 사실이 된다.
//!
//! **바이트는 받는 쪽이 당긴다.** 벌크 데이터가 caller → callee로 가는 와이어 경로가 없기
//! 때문이다(요청 params의 첨부는 미구현). `pull`에서는 보내는 쪽이 callee이므로 이미 돌아가는
//! `uni_stream` 경로를 그대로 쓴다. 이어받기가 공짜로 따라오는 것도 그래서다.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zyris::{Chunk, Streaming};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TransferOffer {
    /// `(보내는 쪽 node_id, name, size, sha256)`에서 결정론적으로 만든다. 같은 전송을 다시
    /// 부르면 같은 값이 나와야 이어받기가 성립한다.
    pub transfer_id: String,
    /// 제안하는 파일 이름. 받는 쪽이 씻고, 디렉터리는 받는 쪽이 정한다.
    pub name: String,
    pub size: u64,
    /// 소문자 16진 64자.
    pub sha256: String,
    #[serde(default)]
    pub overwrite: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TransferDone {
    /// 받는 쪽의 최종 경로.
    pub written: String,
    pub bytes: u64,
    pub sha256: String,
    /// 있던 파일을 덮었나.
    pub replaced: bool,
    /// 덮었으면 원본이 있는 자리.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullHead {
    /// 파일 전체 크기. `offset`을 뺀 값이 아니다.
    pub size: u64,
    pub sha256: String,
}

#[zyris::capability(name = "peer_transfer", version = 1)]
pub trait PeerTransfer {
    /// 보내는 쪽이 알린다. 받는 쪽은 `pull`로 되당긴 뒤 결과를 이 호출의 답으로 준다.
    async fn push_offer(&self, offer: TransferOffer) -> zyris::Result<TransferDone>;

    /// 받는 쪽이 보내는 쪽에게서 바이트를 당긴다. 여기서는 **보내는 쪽이 callee**다.
    ///
    /// `offset`은 이미 받아 둔 바이트 수다. 0이면 처음부터.
    #[zyris(uni_stream)]
    async fn pull(
        &self,
        transfer_id: String,
        offset: u64,
    ) -> zyris::Result<Streaming<PullHead, Chunk>>;
}
```

`crates/zyris-caps/src/lib.rs`에:

```rust
pub mod peer_transfer;

pub use peer_transfer::{
    peer_transfer_capability, PeerTransfer, PeerTransferClient, PeerTransferServer, PullHead,
    TransferDone, TransferOffer,
};
```

- [ ] **Step 2: descriptor를 잠그는 테스트를 쓴다**

`crates/zyris-caps/src/peer_transfer.rs` 맨 아래에:

```rust
#[cfg(test)]
mod tests {
    use zyris::proto::Transfer;

    #[test]
    fn pull만_스트림이다() {
        let d = super::peer_transfer_capability();
        assert_eq!(d.name, "peer_transfer");
        assert_eq!(d.version, 1);
        assert_eq!(d.tool("push_offer").unwrap().transfer, Transfer::Unary);
        assert_eq!(d.tool("pull").unwrap().transfer, Transfer::UniStream);
        // 도구가 늘면 피어에게 열리는 표면이 늘어난다. 여기서 잡는다.
        let mut 이름들: Vec<_> = d.tools.iter().map(|t| t.name.as_str()).collect();
        이름들.sort();
        assert_eq!(이름들, ["pull", "push_offer"]);
    }
}
```

- [ ] **Step 3: 돌린다**

```bash
timeout 300 cargo test -j2 -p zyris-caps peer_transfer 2>&1 | tail -20
```

기대: 1 passed. 실패하면 `zyris::proto::Transfer`의 실제 경로를 `crates/zyris-attacca/tests/
attacca_api_roundtrip.rs:444`에서 확인해 맞춘다.

- [ ] **Step 4: 커밋**

```bash
git add crates/zyris-caps/src/peer_transfer.rs crates/zyris-caps/src/lib.rs
git commit -m "feat(caps): 피어 사이의 파일 전송 와이어를 선언한다"
```

---

## Task 1.5: 받는 쪽 구현 — `push_offer`

**Files:**
- Create: `crates/zyris-capkit/src/transfer/peer.rs`
- Create: `crates/zyris-capkit/tests/peer_transfer.rs`
- Modify: `crates/zyris-capkit/src/transfer/mod.rs`, `crates/zyris-capkit/Cargo.toml`

**Interfaces:**
- Consumes: `Inbox::resolve`, `UndoStore::stash`, `zyris_caps::peer_transfer::*`
- Produces:
  - `pub struct TransferConfig { pub inbox: PathBuf, pub undo: PathBuf, pub max_file_bytes: u64, pub max_inbox_bytes: u64 }`
  - `pub struct LocalPeerTransfer` — `PeerTransfer` 구현체.
  - `pub fn receiver(config: TransferConfig, peer_slug: String, peer: PeerTransferClient) -> LocalPeerTransfer`
  - `pub fn sender(root: PathBuf) -> LocalPeerTransfer` — `pull`만 답하는 쪽.

- [ ] **Step 1: 실패하는 테스트를 쓴다**

`crates/zyris-capkit/tests/peer_transfer.rs`:

```rust
//! 전송 전 과정을 **소켓 없이** 본다. `zyris::testing::duplex`가 진짜 Connection 둘을 잇는다.

use std::time::Duration;

use sha2::{Digest, Sha256};
use zyris::{Node, NodeKind};
use zyris_caps::peer_transfer::{PeerTransferClient, PeerTransferServer, TransferOffer};
use zyris_capkit::transfer::{LocalPeerTransfer, TransferConfig};

fn 해시(바이트: &[u8]) -> String {
    hex::encode(Sha256::digest(바이트))
}

/// A(보내는 쪽)와 B(받는 쪽)를 잇는다. 둘 다 `peer_transfer` 하나만 내준다.
///
/// **손잡이 꽂는 순서가 이 헬퍼의 요점이다.** B가 `pull`을 되부르려면 `PeerTransferClient`가
/// 있어야 하는데 그것은 `duplex`가 Connection을 준 뒤에야 만들 수 있고, `Node`를 만들려면
/// 받는 쪽 구현체가 그보다 **먼저** 있어야 한다. 그래서 `receiver_pending`으로 빈 채 만들고
/// 연결이 선 다음 `set_peer`로 채운다.
async fn 붙인다(
    보낼_것: &std::path::Path,
    설정: TransferConfig,
) -> (zyris::Connection, zyris::Connection) {
    let 받는_것 = LocalPeerTransfer::receiver_pending(설정, "a".into());
    let a = Node::builder()
        .name("a")
        .kind(NodeKind::Cli)
        .capability(PeerTransferServer(LocalPeerTransfer::sender(
            보낼_것.parent().unwrap().to_path_buf(),
        )))
        .build()
        .unwrap();
    let b = Node::builder()
        .name("b")
        .kind(NodeKind::Cli)
        .capability(PeerTransferServer(받는_것.clone()))
        .build()
        .unwrap();
    let (a_conn, b_conn) = zyris::testing::duplex(&a, &b).await.unwrap();
    // 손잡이는 여기서만 꽂을 수 있다 — Connection이 생기고 A가 announce한 뒤다.
    let a_client: PeerTransferClient =
        b_conn.wait_capability(Duration::from_secs(2)).await.unwrap();
    받는_것.set_peer(a_client);
    (a_conn, b_conn)
}

#[tokio::test]
async fn 파일이_inbox에_그대로_도착한다() {
    let 원본_자리 = tempfile::tempdir().unwrap();
    let 받는_자리 = tempfile::tempdir().unwrap();
    let 되돌림 = tempfile::tempdir().unwrap();
    let 내용 = b"hello p2p".repeat(1000);
    let 원본 = 원본_자리.path().join("report.pdf");
    tokio::fs::write(&원본, &내용).await.unwrap();

    let 설정 = TransferConfig {
        inbox: 받는_자리.path().to_path_buf(),
        undo: 되돌림.path().to_path_buf(),
        ..TransferConfig::default()
    };
    let (a_conn, _b_conn) = 붙인다(&원본, 설정).await;

    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();
    let 결과 = b
        .push_offer(TransferOffer {
            transfer_id: "t1".into(),
            name: "report.pdf".into(),
            size: 내용.len() as u64,
            sha256: 해시(&내용),
            overwrite: false,
        })
        .await
        .unwrap();

    assert_eq!(결과.bytes, 내용.len() as u64);
    assert_eq!(결과.sha256, 해시(&내용));
    assert!(!결과.replaced);
    assert_eq!(tokio::fs::read(&결과.written).await.unwrap(), 내용);
    assert!(std::path::Path::new(&결과.written).starts_with(받는_자리.path()));
}

#[tokio::test]
async fn sha256이_안_맞으면_받은_것을_버린다() {
    let 원본_자리 = tempfile::tempdir().unwrap();
    let 받는_자리 = tempfile::tempdir().unwrap();
    let 되돌림 = tempfile::tempdir().unwrap();
    let 내용 = "진짜 내용".as_bytes().to_vec();
    let 원본 = 원본_자리.path().join("a.bin");
    tokio::fs::write(&원본, &내용).await.unwrap();

    let 설정 = TransferConfig {
        inbox: 받는_자리.path().to_path_buf(),
        undo: 되돌림.path().to_path_buf(),
        ..TransferConfig::default()
    };
    let (a_conn, _b) = 붙인다(&원본, 설정).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let 결과 = b
        .push_offer(TransferOffer {
            transfer_id: "t2".into(),
            name: "a.bin".into(),
            size: 내용.len() as u64,
            sha256: 해시("다른 내용".as_bytes()),   // 일부러 틀린 값
            overwrite: false,
        })
        .await;

    assert!(결과.is_err(), "불일치인데 성공했다");
    // 부분 파일을 남기면 다음 재개가 그것을 이어받아 영영 안 맞는다.
    let mut 남은_것 = tokio::fs::read_dir(받는_자리.path().join("a")).await;
    if let Ok(ref mut d) = 남은_것 {
        assert!(d.next_entry().await.unwrap().is_none(), "부분 파일이 남았다");
    }
}

#[tokio::test]
async fn overwrite가_false면_있는_파일을_안_덮는다() {
    let 원본_자리 = tempfile::tempdir().unwrap();
    let 받는_자리 = tempfile::tempdir().unwrap();
    let 되돌림 = tempfile::tempdir().unwrap();
    let 내용 = "새 것".as_bytes().to_vec();
    let 원본 = 원본_자리.path().join("a.txt");
    tokio::fs::write(&원본, &내용).await.unwrap();

    tokio::fs::create_dir_all(받는_자리.path().join("a")).await.unwrap();
    tokio::fs::write(받는_자리.path().join("a").join("a.txt"), "예전 것".as_bytes()).await.unwrap();

    let 설정 = TransferConfig {
        inbox: 받는_자리.path().to_path_buf(),
        undo: 되돌림.path().to_path_buf(),
        ..TransferConfig::default()
    };
    let (a_conn, _b) = 붙인다(&원본, 설정).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let 결과 = b
        .push_offer(TransferOffer {
            transfer_id: "t3".into(),
            name: "a.txt".into(),
            size: 내용.len() as u64,
            sha256: 해시(&내용),
            overwrite: false,
        })
        .await;

    assert!(결과.is_err());
    assert_eq!(
        tokio::fs::read(받는_자리.path().join("a").join("a.txt")).await.unwrap(),
        "예전 것".as_bytes(),
        "덮지 않기로 했는데 덮었다"
    );
}

#[tokio::test]
async fn overwrite가_true면_덮고_원본을_되돌림_자리로_옮긴다() {
    let 원본_자리 = tempfile::tempdir().unwrap();
    let 받는_자리 = tempfile::tempdir().unwrap();
    let 되돌림 = tempfile::tempdir().unwrap();
    let 내용 = "새 것".as_bytes().to_vec();
    let 원본 = 원본_자리.path().join("a.txt");
    tokio::fs::write(&원본, &내용).await.unwrap();

    tokio::fs::create_dir_all(받는_자리.path().join("a")).await.unwrap();
    tokio::fs::write(받는_자리.path().join("a").join("a.txt"), "예전 것".as_bytes()).await.unwrap();

    let 설정 = TransferConfig {
        inbox: 받는_자리.path().to_path_buf(),
        undo: 되돌림.path().to_path_buf(),
        ..TransferConfig::default()
    };
    let (a_conn, _b) = 붙인다(&원본, 설정).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let 결과 = b
        .push_offer(TransferOffer {
            transfer_id: "t4".into(),
            name: "a.txt".into(),
            size: 내용.len() as u64,
            sha256: 해시(&내용),
            overwrite: true,
        })
        .await
        .unwrap();

    assert!(결과.replaced);
    assert_eq!(tokio::fs::read(&결과.written).await.unwrap(), 내용);
    let 되돌릴_것 = 결과.undo.expect("덮었으면 되돌림 자리가 있어야 한다");
    assert_eq!(tokio::fs::read(&되돌릴_것).await.unwrap(), "예전 것".as_bytes());
}

#[tokio::test]
async fn 상한을_넘는_파일은_거부한다() {
    let 원본_자리 = tempfile::tempdir().unwrap();
    let 받는_자리 = tempfile::tempdir().unwrap();
    let 되돌림 = tempfile::tempdir().unwrap();
    let 원본 = 원본_자리.path().join("big.bin");
    tokio::fs::write(&원본, "작다".as_bytes()).await.unwrap();

    let 설정 = TransferConfig {
        inbox: 받는_자리.path().to_path_buf(),
        undo: 되돌림.path().to_path_buf(),
        max_file_bytes: 10,
        ..TransferConfig::default()
    };
    let (a_conn, _b) = 붙인다(&원본, 설정).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    // 바이트를 한 개도 안 쓰고 거절해야 한다 — 선언된 크기만 보고 판단한다.
    let 결과 = b
        .push_offer(TransferOffer {
            transfer_id: "t5".into(),
            name: "big.bin".into(),
            size: 99_999,
            sha256: 해시("뭐든".as_bytes()),
            overwrite: false,
        })
        .await;
    assert!(결과.is_err());
}
```

- [ ] **Step 2: 빨간불 확인**

```bash
timeout 600 cargo test -j2 -p zyris-capkit --features transfer --test peer_transfer 2>&1 | tail -30
```

기대: `LocalPeerTransfer` 미정의로 컴파일 실패.

- [ ] **Step 3: 구현**

`crates/zyris-capkit/src/transfer/peer.rs`:

```rust
//! `peer_transfer`의 참조 구현. 한 타입이 양쪽 역할을 다 한다 — 보내는 쪽은 `pull`에 답하고,
//! 받는 쪽은 `push_offer`에 답한다.
//!
//! **무결성은 여기서 한다.** 엔진의 `s_end.trailer.sha256`은 받는 쪽이 버리므로(connection.rs가
//! `Envelope::SEnd { stream, .. }`로 구조분해한다) 믿을 수 없다.

use std::path::PathBuf;
use std::sync::Arc;

use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use zyris::{Chunk, ErrorCode, Result, Streaming, WireError};
use zyris_caps::peer_transfer::{
    PeerTransfer, PeerTransferClient, PullHead, TransferDone, TransferOffer,
};

use super::inbox::Inbox;
use super::undo::UndoStore;

/// 파일을 스트림 항목 하나에 얼마씩 실을지.
///
/// **프로토콜의 `initial_stream_credit`(256 KiB)와 같은 값을 쓰면 안 된다.** 항목은 와이어로
/// 나가기 전에 msgpack으로 감싸이므로 직렬화된 길이가 창을 몇 바이트 넘고, 보내는 쪽
/// `CreditGate::acquire`가 **첫 청크에서** 막힌다. 상대는 그 청크를 못 받았으니 credit을
/// 돌려줄 수 없다 — 영원히 멈춘다. (처음에 이 자리에 `256 * 1024`를 적었다가 실제로 그랬다.
/// 262,144는 정지, 262,080은 통과.)
const 청크: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct TransferConfig {
    pub inbox: PathBuf,
    pub undo: PathBuf,
    pub max_file_bytes: u64,
    pub max_inbox_bytes: u64,
}

impl Default for TransferConfig {
    fn default() -> TransferConfig {
        TransferConfig {
            inbox: PathBuf::from("."),
            undo: PathBuf::from("."),
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
                format!("{}바이트는 이 노드의 상한 {}바이트를 넘습니다", offer.size, self.config.max_file_bytes),
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

        Ok(TransferDone {
            written: 목적지.display().to_string(),
            bytes: 쓴_바이트,
            sha256: 실제,
            replaced: 이미_있나,
            undo: undo.map(|p| p.display().to_string()),
        })
    }

    async fn pull(
        &self,
        transfer_id: String,
        offset: u64,
    ) -> Result<Streaming<PullHead, Chunk>> {
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

        // 정본은 `zyris-capkit/src/file_io.rs:124-161`의 `read_stream`이다. **`stream::unfold`를
        // 쓴다** — `async_stream`을 새로 들이지 않는다. 여는 것은 스트림 **밖**에서 한다:
        // 못 여는 것은 스트림 중간의 실패가 아니라 호출 자체의 실패여야 한다.
        use tokio::io::{AsyncReadExt, AsyncSeekExt};

        let mut file = tokio::fs::File::open(&것.path).await.map_err(io_오류)?;
        if offset > 0 {
            file.seek(std::io::SeekFrom::Start(offset)).await.map_err(io_오류)?;
        }
        let head = PullHead { size: 것.size, sha256: 것.sha256.clone() };
        // 상태에 `끝났나`를 함께 들고 다닌다. **오류를 내보낸 뒤 상태를 죽이지 않으면 스트림이
        // 같은 오류를 영원히 뱉는다** — `file_io.rs`가 `remaining`을 `Some(0)`으로 만드는 것과
        // 같은 이유다. 오류를 한 번은 내보내야 한다: 여기서 그냥 `None`을 주면 받는 쪽은 파일이
        // 정상적으로 끝난 줄 알고 sha256이 왜 안 맞는지 알 길이 없다.
        let 항목 = futures_util::stream::unfold((file, false), |(mut file, 끝났나)| async move {
            if 끝났나 {
                return None;
            }
            let mut 버퍼 = vec![0u8; 청크];
            match file.read(&mut 버퍼).await {
                Ok(0) => None,
                Ok(n) => {
                    버퍼.truncate(n);
                    Some((Ok(Chunk::new(bytes::Bytes::from(버퍼))), (file, false)))
                }
                Err(e) => Some((Err(io_오류(e)), (file, true))),
            }
        });
        Ok(Streaming::new(head, 항목))
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
```

`Cargo.toml`의 `transfer` feature는 이미 필요한 것을 다 선언하고 있다. **`async-stream`만
지운다** — `stream::unfold`를 쓰기로 했으므로 안 쓰는 의존이 남는다. `dependencies`의
`async-stream` 줄과 feature 목록의 `"dep:async-stream"`을 둘 다 지우고
`cargo build -p zyris-capkit --no-default-features --features transfer`로 확인한다.

`mod.rs`:

```rust
pub mod inbox;
pub mod name;
pub mod peer;
pub mod undo;

pub use name::safe_name;
pub use peer::{LocalPeerTransfer, TransferConfig};
```

- [ ] **Step 4: 초록불 확인**

```bash
timeout 600 cargo test -j2 -p zyris-capkit --features transfer --test peer_transfer 2>&1 | tail -30
```

기대: 5 passed.

`peer` 손잡이가 `Arc<OnceLock<_>>`이고 `set_peer`로 나중에 꽂는 것은 **취향이 아니라 순환을
푸는 유일한 방법이다** — Step 1의 헬퍼 주석에 그 순서가 적혀 있다. 생성자 인자로 받으려 하면
배선이 아예 불가능하니, 컴파일이 안 된다고 구조를 되돌리지 말 것.

`LocalPeerTransfer`가 `Clone`을 파생하는 것도 같은 이유다 — 같은 것을 `Node`에 하나 주고
`set_peer`를 부를 손잡이로 하나 들고 있어야 한다. 안쪽이 전부 `Arc`라 clone은 얕다.

- [ ] **Step 5: 일부러 망가뜨려 본다**

`push_offer`의 sha256 대조를 `if false`로 잠깐 바꾼다. `sha256이_안_맞으면_받은_것을_버린다`가
실패해야 한다. 확인했으면 되돌린다.

- [ ] **Step 6: 커밋**

```bash
git add crates/zyris-capkit
git commit -m "feat(transfer): 받는 쪽이 당겨서 받고 sha256으로 검증한다"
```

---

## Task 1.6: 이어받기

**Files:**
- Modify: `crates/zyris-capkit/tests/peer_transfer.rs`

**Interfaces:** 새 것 없음. Task 1.5의 `offset` 경로를 잠근다.

- [ ] **Step 1: 실패하는 테스트를 쓴다**

`crates/zyris-capkit/tests/peer_transfer.rs`에 더한다:

> **함정 하나를 먼저 피한다.** "이미 받아 둔 앞부분"을 원본의 앞부분과 **같게** 깔면,
> 구현이 `.part`를 통째로 무시하고 처음부터 다시 받아도 최종 내용과 크기가 똑같이 나온다 —
> 테스트가 초록인데 이어받기는 한 번도 검증되지 않는다. 이 태스크에서만 세 번째로 만나는
> 위양성이다. 그래서 아래 테스트는 **보내는 쪽의 앞부분을 일부러 다르게** 만들어 둘을 가른다.

```rust
/// 받는 쪽이 **정말로 이어받는지**를 가른다.
///
/// 보내는 쪽 파일의 앞부분과 이미 받아 둔 앞부분이 서로 다르다(길이만 같다). 제안하는
/// sha256은 `이미_받은_앞부분 ++ 뒷부분`의 것이다. 그래서:
///
/// - 이어받으면 → 앞부분을 그대로 두고 100_000부터 당긴다 → 해시가 맞는다 → 성공
/// - 처음부터 받으면 → 보내는 쪽 앞부분(0xAA)이 온다 → 해시가 틀린다 → 실패
///
/// 보내는 쪽이 제안한 해시와 다른 내용을 갖고 있는 것은 현실에서 일어나지 않지만,
/// 받는 쪽의 판단만 떼어 보려면 이 방법뿐이다.
#[tokio::test]
async fn 받다_만_파일을_이어받는다() {
    let 원본_자리 = tempfile::tempdir().unwrap();
    let 받는_자리 = tempfile::tempdir().unwrap();
    let 되돌림 = tempfile::tempdir().unwrap();

    let 이미_받은_앞부분: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let 보내는_쪽_앞부분: Vec<u8> = vec![0xAAu8; 100_000];
    let 뒷부분: Vec<u8> = (0..200_000u32).map(|i| ((i % 241) as u8) ^ 0x5A).collect();
    assert_ne!(이미_받은_앞부분, 보내는_쪽_앞부분, "두 앞부분이 같으면 이 테스트는 아무것도 못 가른다");

    let mut 보내는_쪽_내용 = 보내는_쪽_앞부분.clone();
    보내는_쪽_내용.extend_from_slice(&뒷부분);
    let 원본 = 원본_자리.path().join("a.bin");
    tokio::fs::write(&원본, &보내는_쪽_내용).await.unwrap();

    // 이어받았을 때에만 나올 수 있는 최종 내용.
    let mut 기대 = 이미_받은_앞부분.clone();
    기대.extend_from_slice(&뒷부분);

    tokio::fs::create_dir_all(받는_자리.path().join("a")).await.unwrap();
    tokio::fs::write(받는_자리.path().join("a").join("a.bin.part"), &이미_받은_앞부분)
        .await
        .unwrap();

    let 설정 = TransferConfig {
        inbox: 받는_자리.path().to_path_buf(),
        undo: 되돌림.path().to_path_buf(),
        ..TransferConfig::default()
    };
    let (a_conn, _b) = 붙인다(&원본, 설정).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let 결과 = b
        .push_offer(TransferOffer {
            transfer_id: "resume".into(),
            name: "a.bin".into(),
            size: 기대.len() as u64,
            sha256: 해시(&기대),
            overwrite: false,
        })
        .await
        .unwrap();

    assert_eq!(결과.bytes, 기대.len() as u64);
    assert_eq!(tokio::fs::read(&결과.written).await.unwrap(), 기대);
}

#[tokio::test]
async fn 받다_만_것이_실제와_다르면_처음부터_받는다() {
    let 원본_자리 = tempfile::tempdir().unwrap();
    let 받는_자리 = tempfile::tempdir().unwrap();
    let 되돌림 = tempfile::tempdir().unwrap();
    let 내용: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
    let 원본 = 원본_자리.path().join("a.bin");
    tokio::fs::write(&원본, &내용).await.unwrap();

    // 쓰레기 앞부분. 이어받으면 sha256이 안 맞아야 한다 — 그래야 버그가 조용히 안 지나간다.
    tokio::fs::create_dir_all(받는_자리.path().join("a")).await.unwrap();
    tokio::fs::write(받는_자리.path().join("a").join("a.bin.part"), vec![0xFFu8; 10_000])
        .await
        .unwrap();

    let 설정 = TransferConfig {
        inbox: 받는_자리.path().to_path_buf(),
        undo: 되돌림.path().to_path_buf(),
        ..TransferConfig::default()
    };
    let (a_conn, _b) = 붙인다(&원본, 설정).await;
    let b: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let 결과 = b
        .push_offer(TransferOffer {
            transfer_id: "bad-resume".into(),
            name: "a.bin".into(),
            size: 내용.len() as u64,
            sha256: 해시(&내용),
            overwrite: false,
        })
        .await;

    // 첫 시도는 불일치로 실패하고 부분 파일을 지운다.
    assert!(결과.is_err());
    // 다시 부르면 처음부터 받아 성공해야 한다.
    let 두_번째 = b
        .push_offer(TransferOffer {
            transfer_id: "bad-resume".into(),
            name: "a.bin".into(),
            size: 내용.len() as u64,
            sha256: 해시(&내용),
            overwrite: false,
        })
        .await
        .unwrap();
    assert_eq!(tokio::fs::read(&두_번째.written).await.unwrap(), 내용);
}
```

- [ ] **Step 1b: 보내는 쪽의 `offset`도 따로 잠근다**

위 둘은 **받는 쪽의 판단**을 본다. `pull(transfer_id, offset)`이 정말 그 지점부터 흘리는지는
보내는 쪽의 일이라 따로 봐야 한다 — 받는 쪽이 offset을 옳게 계산해도 보내는 쪽이 무시하면
같은 증상이 난다.

`pull은_offset부터_흘린다`를 더한다. 하는 일:

1. 300_000바이트 파일을 만들고 `붙인다`로 잇는다(위 테스트들과 같은 방식).
2. `b.pull(<transfer_id>, 100_000)`을 직접 부른다.
3. `PullHead.size`가 **파일 전체 크기 300_000**인지 본다 — offset을 뺀 값이 아니다.
4. 흘러온 청크를 모아 `내용[100_000..]`과 바이트가 같은지 본다. 길이만 보지 말 것.

> `Streaming<PullHead, Chunk>`에서 head를 꺼내고 청크를 모으는 정확한 호출 모양은 여기 적지
> 않는다. **`crates/zyris-capkit/tests/peer_transfer.rs`에 이미 있는 방식을 그대로 따르고,
> 거기 없으면 `crates/zyris/tests/`의 스트림 테스트를 보라.** 내가 기억으로 적은 시그니처가
> 이 플랜에서 이미 세 번 틀렸다 — 저장소에 있는 것을 보고 쓰는 편이 확실하다.

- [ ] **Step 2: 빨간불 확인**

```bash
timeout 600 cargo test -j2 -p zyris-capkit --no-default-features --features transfer --test peer_transfer 2>&1 | tail -30
```

- [ ] **Step 3: 필요하면 구현을 고친다**

Task 1.5의 구현이 이미 `offset`을 다루므로 대개 그대로 통과한다. 안 되면 `받은_offset` 계산과
`앞부분` 해시 누적을 본다.

**주의 — `pending`의 수명.** 두 번째 테스트는 같은 `transfer_id`로 `push_offer`를 두 번 부른다.
첫 시도가 해시 불일치로 실패한 뒤 offer가 `pending`에서 사라지는 구현이면 두 번째 호출이
"unknown transfer"로 죽는다. **그때는 테스트를 고치지 말고 구현을 보라** — 한 번 실패했다고
제안이 증발하면 이어받기 자체가 성립하지 않는다. 재시도가 되는 것이 이 기능의 요구사항이다.

- [ ] **Step 4: 초록불 확인 후 커밋**

```bash
timeout 600 cargo test -j2 -p zyris-capkit --features transfer --test peer_transfer 2>&1 | tail -10
git add crates/zyris-capkit
git commit -m "test(transfer): 이어받기와 어긋난 부분 파일을 잠근다"
```

---

## Task 1.7: 감사 로그

**Files:**
- Create: `crates/zyris-capkit/src/transfer/audit.rs`
- Modify: `crates/zyris-capkit/src/transfer/peer.rs`, `mod.rs`

**Interfaces:**
- Produces: `pub struct Audit { path: PathBuf }`, `pub async fn record(&self, line: AuditLine)`,
  `pub struct AuditLine { pub at_ms: u64, pub peer_slug: String, pub peer_endpoint: String, pub name: String, pub bytes: u64, pub sha256: String, pub written: String, pub replaced: bool, pub direct: bool }`

- [ ] **Step 1: 실패하는 테스트를 쓴다**

`crates/zyris-capkit/src/transfer/audit.rs` 맨 아래에:

```rust
#[cfg(test)]
mod tests {
    use super::{Audit, AuditLine};

    fn 한_줄() -> AuditLine {
        AuditLine {
            at_ms: 1_754_700_000_000,
            peer_slug: "arch-zyris-code".into(),
            peer_endpoint: "abc123".into(),
            name: "report.pdf".into(),
            bytes: 4096,
            sha256: "de.ad".into(),
            written: "/home/x/inbox/a/report.pdf".into(),
            replaced: true,
            direct: false,
        }
    }

    #[tokio::test]
    async fn 한_전송이_한_줄로_쌓인다() {
        let 자리 = tempfile::tempdir().unwrap();
        let 길 = 자리.path().join("transfers.log");
        let audit = Audit::new(&길);
        audit.record(한_줄()).await;
        audit.record(한_줄()).await;

        let 글 = tokio::fs::read_to_string(&길).await.unwrap();
        assert_eq!(글.lines().count(), 2, "append여야 한다");
        let 첫_줄: serde_json::Value = serde_json::from_str(글.lines().next().unwrap()).unwrap();
        assert_eq!(첫_줄["peer_slug"], "arch-zyris-code");
        assert_eq!(첫_줄["replaced"], true);
    }

    #[tokio::test]
    async fn 못_써도_전송을_막지_않는다() {
        let audit = Audit::new("/proc/못쓰는자리/x.log");
        audit.record(한_줄()).await;   // 패닉하지 않으면 통과
    }
}
```

- [ ] **Step 2: 빨간불 확인**

```bash
timeout 300 cargo test -j2 -p zyris-capkit --features transfer audit:: 2>&1 | tail -20
```

- [ ] **Step 3: 구현**

`crates/zyris-capkit/src/transfer/audit.rs`:

```rust
//! 전송마다 한 줄. **사람 확인이 없는 흐름에서 사후에 무슨 일이 있었는지 아는 유일한 길이다.**
//!
//! 못 쓰더라도 전송을 막지 않는다 — 로그가 없다고 파일이 안 가야 할 이유가 없다.

use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct AuditLine {
    pub at_ms: u64,
    pub peer_slug: String,
    pub peer_endpoint: String,
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
    pub written: String,
    pub replaced: bool,
    /// 직접 연결이었나, 릴레이를 지났나. 릴레이 비율을 재는 씨앗이다.
    pub direct: bool,
}

pub struct Audit {
    path: PathBuf,
}

impl Audit {
    pub fn new(path: impl Into<PathBuf>) -> Audit {
        Audit { path: path.into() }
    }

    pub async fn record(&self, line: AuditLine) {
        if let Err(e) = self.write(line).await {
            tracing::warn!(error = %e, path = %self.path.display(), "감사 로그를 쓰지 못했습니다");
        }
    }

    async fn write(&self, line: AuditLine) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt;
        if let Some(부모) = self.path.parent() {
            tokio::fs::create_dir_all(부모).await?;
        }
        let mut 글 = serde_json::to_string(&line).unwrap_or_default();
        글.push('\n');
        let mut f =
            tokio::fs::OpenOptions::new().create(true).append(true).open(&self.path).await?;
        f.write_all(글.as_bytes()).await
    }
}
```

`peer.rs`의 `push_offer` 성공 직전에 `record`를 부른다. `TransferConfig`에
`pub audit: Option<PathBuf>`를 더하고, `None`이면 기록하지 않는다.

- [ ] **Step 4: 초록불 확인 후 커밋**

```bash
timeout 300 cargo test -j2 -p zyris-capkit --features transfer 2>&1 | tail -10
git add crates/zyris-capkit
git commit -m "feat(transfer): 전송마다 감사 로그를 한 줄 남긴다"
```

---

## Task 1.8: 단계 1 마무리 — clippy와 PR

- [ ] **Step 1: 전체 검사**

```bash
timeout 900 cargo test -j2 -p zyris-caps -p zyris-capkit --features zyris-capkit/transfer 2>&1 | tail -20
timeout 900 cargo clippy -j2 -p zyris-caps -p zyris-capkit --features zyris-capkit/transfer --all-targets 2>&1 | tail -20
```

기대: 테스트 전부 초록, clippy 경고 0.

**`cargo fmt`을 돌리지 않는다.** zyris에 `rustfmt.toml`이 없다.

- [ ] **Step 2: upstream이 움직였는지 본다**

```bash
git fetch origin
git log --oneline HEAD..origin/main
```

- [ ] **Step 3: 반영**

비어 있으면 push 후 병합, 새 커밋이 있으면 PR을 낸다.

```bash
git push -u origin feat/transfer-capability
gh pr create --repo attacca-cc/zyris \
  --title "feat(transfer): 노드 간 파일 전송의 받는 쪽" \
  --body-file <(cat <<'EOF'
## 무엇을 위한 것인가

노드 A가 노드 B에게 파일을 보내는 기능의 **받는 쪽 절반**이다. 전송로(iroh)와 랑데부(attacca)는
뒤따르는 변경이고, 이것만으로도 `zyris::testing::duplex`로 전 과정이 검증된다.

## 무엇을 만들었나

- `zyris-caps::peer_transfer` — A↔B 와이어 선언. `push_offer`(unary)와 `pull`(uni_stream) 둘뿐이다.
- `zyris-capkit::transfer` — 참조 구현. inbox 감옥, sha256 검증, 덮어쓰기와 되돌림, 이어받기,
  감사 로그.

`transfer`는 **기본 feature가 아니다.** 안 쓰는 노드는 아무것도 더 컴파일하지 않는다.

## 왜 미는 대신 당기는가

벌크 데이터가 caller → callee로 가는 와이어 경로가 없다 — 요청 params의 첨부는 미구현이고
(`docs/zyris-protocol.md:316`), `file_io.write_at`은 문서에만 있고 존재한 적이 없다. 받는 쪽이
`pull`로 당기면 `read_stream`과 같은, 이미 돌아가는 경로만 쓴다. **이어받기가 공짜로 따라온다.**

## 감옥은 처음부터 만들었다

`path::resolve_under`는 감옥이 아니고(주석부터가 "the root is a default, not a jail") 심링크를
전혀 다루지 않는다. 받은 것을 남의 머신에 쓰는 자리라 그 규칙을 쓸 수 없어, 이름 씻기와 실제
경로 확인을 따로 두었다. 둘 다 있어야 한다 — 씻기는 정상 경로를 지키고, 확인은 씻기를 빠져나간
것을 잡는다.

## 무결성은 이 층에서 한다

`s_end.trailer.sha256`은 보내는 쪽이 쓰지만 받는 쪽이 버린다(`connection.rs`가
`Envelope::SEnd { stream, .. }`로 구조분해한다). 그래서 `peer_transfer`가 자기 층에서 계산하고
대조한다. 불일치면 부분 파일을 **지운다** — 남기면 다음 재개가 그것을 이어받아 영영 안 맞는다.

## 검증

- `cargo test -p zyris-caps -p zyris-capkit --features zyris-capkit/transfer`
- 감옥과 무결성 검사는 일부러 망가뜨려 테스트가 무는 것을 확인했다.
EOF
)
```

---

# 단계 2 — `zyris-p2p` 전송로

## Task 2.1: 크레이트 뼈대와 iroh 무게 실측

**Files:**
- Create: `crates/zyris-p2p/Cargo.toml`, `crates/zyris-p2p/src/lib.rs`
- Modify: `Cargo.toml` (워크스페이스 members)

- [ ] **Step 1: 크레이트를 만든다**

`crates/zyris-p2p/Cargo.toml`:

```toml
[package]
name = "zyris-p2p"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true
description = "Zyris를 노드 사이 직접 연결 위에 얹는 전송로"

[dependencies]
async-trait.workspace = true
bytes.workspace = true
iroh = "1"   # 2026-08-09 crates.io 최신은 1.0.3. `iroh-base`도 같은 1.0.3으로 따라온다
serde = { workspace = true, features = ["derive"] }
serde_json.workspace = true
thiserror.workspace = true
tokio = { workspace = true, features = ["rt", "sync", "macros", "time", "io-util", "fs"] }
tracing.workspace = true
zyris = { version = "0.1.0", path = "../zyris" }
zyris-proto = { version = "0.1.0", path = "../zyris-proto" }

[dev-dependencies]
tempfile.workspace = true
tokio = { workspace = true, features = ["full"] }
```

루트 `Cargo.toml`의 `members`에 `"crates/zyris-p2p"`를 더한다.

`crates/zyris-p2p/src/lib.rs`:

```rust
//! Zyris를 **노드 사이 직접 연결** 위에 얹는다.
//!
//! zyris 코어는 한 줄도 고치지 않는다. `Transport` 트레잇이 주고받는 것은
//! `WireMessage::{Binary, Text}` 뿐이고, 클라이언트/서버 비대칭은 `Role::Dial`/`Role::Accept`
//! 하나뿐이라 — 누가 다이얼했는지만 정해지면 그대로 맞는다.
```

> **`pub mod frame;`을 여기서 적으면 안 된다.** `frame.rs`는 Task 2.2가 만든다. 지금 적으면
> 이 태스크가 컴파일되지 않아 무게 실측 자체를 못 한다. **모듈 선언은 그 파일을 만드는
> 태스크가 함께 넣는다.** (모듈 doc은 저장소 규약대로 영어로 쓴다.)

- [ ] **Step 2: 무게를 잰다**

```bash
timeout 1800 cargo build -j2 -p zyris-p2p 2>&1 | tail -5
du -sh target/debug
cargo tree -p zyris-p2p --depth 1 2>/dev/null | wc -l
```

**결과를 스펙의 §12에 적는다.** 이 머신에서 감당이 안 되면(빌드가 10분을 크게 넘거나 링크가
OOM으로 죽으면) 여기서 멈추고 사용자와 다시 이야기한다 — 뒤의 Task가 전부 이 위에 선다.

### 실측 결과 (2026-08-09, 이 머신: RAM 3.6GB / i3-7100U 4스레드 / SATA SSD)

**게이트 통과.** 우려했던 것보다 훨씬 가볍다.

| 항목 | 값 |
|---|---|
| 고유 크레이트 | **385개** (의존 트리 노드 889) |
| 콜드 빌드 (`-j2`) | **약 3분** (중간에 한 번 끊겨 100초 + 81초로 나뉘어 측정됨) |
| 증분 재빌드 (`lib.rs` 한 줄) | **2초** |
| 디스크 증가 | **+1 GB** (`target/debug` 12G → 13G) |
| 빌드 중 최소 여유 RAM | **816 MB** — 링크 단계에서도 압박 없음 |

`-j2`에서 여유 RAM이 800MB 아래로 내려간 적이 없다. **OOM 위험은 관측되지 않았다.**
디스크가 더 눈에 띄는 비용인데(전체 여유 33G 중 1G), 이것도 감당된다.

> 스펙 §12 반영은 단계 1 PR이 머지되어 스펙이 main에 올라온 뒤에 한다. 지금은 스펙 파일이
> `feat/transfer-capability` 브랜치에만 있다.

- [ ] **Step 3: 커밋**

```bash
git add crates/zyris-p2p Cargo.toml Cargo.lock
git commit -m "build(p2p): zyris-p2p 크레이트를 워크스페이스에 더한다"
```

---

## Task 2.2: 프레이밍 (순수)

**Files:**
- Create: `crates/zyris-p2p/src/frame.rs`

**Interfaces:**
- Produces:
  - `pub fn encode(msg: &WireMessage) -> Vec<u8>`
  - `pub fn decode_header(buf: &[u8; 5]) -> Result<(u8, usize), FrameError>`
  - `pub const MAX_FRAME: usize = 16 * 1024 * 1024;`
  - `pub enum FrameError { UnknownKind(u8), TooLarge(usize), NotUtf8 }`

- [ ] **Step 1: 실패하는 테스트를 쓴다**

`crates/zyris-p2p/src/frame.rs` 맨 아래에:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use zyris_proto::WireMessage;

    #[test]
    fn 바이너리는_kind_0으로_나간다() {
        let 나온_것 = encode(&WireMessage::Binary(bytes::Bytes::from_static(b"abc")));
        assert_eq!(나온_것[0], 0);
        assert_eq!(&나온_것[1..5], &3u32.to_be_bytes());
        assert_eq!(&나온_것[5..], b"abc");
    }

    #[test]
    fn 텍스트는_kind_1로_나간다() {
        let 나온_것 = encode(&WireMessage::Text("가".into()));
        assert_eq!(나온_것[0], 1);
        assert_eq!(&나온_것[1..5], &3u32.to_be_bytes()); // '가'는 UTF-8로 3바이트
    }

    #[test]
    fn 머리를_되읽는다() {
        let 나온_것 = encode(&WireMessage::Binary(bytes::Bytes::from_static(b"abcd")));
        let 머리: [u8; 5] = 나온_것[..5].try_into().unwrap();
        assert_eq!(decode_header(&머리).unwrap(), (0, 4));
    }

    #[test]
    fn 상한을_넘는_길이는_거부한다() {
        let mut 머리 = [0u8; 5];
        머리[0] = 0;
        머리[1..5].copy_from_slice(&((MAX_FRAME + 1) as u32).to_be_bytes());
        // 상한이 없으면 상대가 4GiB를 선언해 우리 메모리를 밀어 넣을 수 있다.
        assert!(matches!(decode_header(&머리), Err(FrameError::TooLarge(_))));
    }

    #[test]
    fn 모르는_kind는_거부한다() {
        let mut 머리 = [0u8; 5];
        머리[0] = 9;
        assert!(matches!(decode_header(&머리), Err(FrameError::UnknownKind(9))));
    }
}
```

- [ ] **Step 2: 빨간불 확인**

```bash
timeout 900 cargo test -j2 -p zyris-p2p frame:: 2>&1 | tail -20
```

- [ ] **Step 3: 구현**

```rust
//! QUIC은 바이트 스트림을 주고 zyris는 메시지를 원한다. 하나의 bi-stream 위에 길이 접두를 얹는다.
//!
//! ```text
//! [u8 kind][u32 BE len][payload …]      kind 0 = Binary, 1 = Text
//! ```
//!
//! **`len` 상한이 실질적인 메모리 방어선이다.** 프로토콜의 크레딧은 보내는 쪽만 묶고 받는 쪽
//! 회계가 없어(`CreditViolation`은 정의만 되고 미사용) 규약을 안 지키는 피어를 못 막는다.

use zyris_proto::WireMessage;

pub const MAX_FRAME: usize = 16 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("모르는 프레임 종류입니다: {0}")]
    UnknownKind(u8),
    #[error("{0}바이트는 상한 {MAX_FRAME}바이트를 넘습니다")]
    TooLarge(usize),
    #[error("텍스트 프레임이 UTF-8이 아닙니다")]
    NotUtf8,
}

pub fn encode(msg: &WireMessage) -> Vec<u8> {
    let (kind, payload): (u8, &[u8]) = match msg {
        WireMessage::Binary(b) => (0, b),
        WireMessage::Text(t) => (1, t.as_bytes()),
    };
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(kind);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

pub fn decode_header(buf: &[u8; 5]) -> Result<(u8, usize), FrameError> {
    let kind = buf[0];
    if kind > 1 {
        return Err(FrameError::UnknownKind(kind));
    }
    let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len));
    }
    Ok((kind, len))
}

pub fn body(kind: u8, bytes: Vec<u8>) -> Result<WireMessage, FrameError> {
    match kind {
        0 => Ok(WireMessage::Binary(bytes.into())),
        1 => String::from_utf8(bytes).map(WireMessage::Text).map_err(|_| FrameError::NotUtf8),
        other => Err(FrameError::UnknownKind(other)),
    }
}
```

- [ ] **Step 4: 초록불 확인 후 커밋**

```bash
timeout 900 cargo test -j2 -p zyris-p2p frame:: 2>&1 | tail -10
git add crates/zyris-p2p/src/frame.rs crates/zyris-p2p/src/lib.rs
git commit -m "feat(p2p): QUIC 바이트 스트림 위의 메시지 프레이밍"
```

---

## Task 2.3: 노드 키페어

**Files:**
- Create: `crates/zyris-p2p/src/key.rs`
- Create: `crates/zyris-p2p/tests/key.rs`

**Interfaces:**
- Produces:
  - `pub async fn load_or_create(path: &Path) -> Result<iroh::SecretKey, KeyError>`
  - `pub enum KeyError { Permissions(u32), Io(String), Malformed }`

- [ ] **Step 1: 실패하는 테스트를 쓴다**

`crates/zyris-p2p/tests/key.rs`:

```rust
use zyris_p2p::key::{load_or_create, KeyError};

#[tokio::test]
async fn 처음이면_만들고_0600으로_쓴다() {
    let 자리 = tempfile::tempdir().unwrap();
    let 길 = 자리.path().join("peer.key");
    let 키 = load_or_create(&길).await.unwrap();

    assert!(길.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let 모드 = std::fs::metadata(&길).unwrap().permissions().mode() & 0o777;
        assert_eq!(모드, 0o600, "실제: {모드:o}");
    }
    // 두 번째 호출은 같은 키를 준다 — 매번 새로 만들면 상대의 TOFU가 매번 물어 버린다.
    let 다시 = load_or_create(&길).await.unwrap();
    assert_eq!(키.public(), 다시.public());
}

#[tokio::test]
async fn 남이_읽을_수_있으면_거부한다() {
    let 자리 = tempfile::tempdir().unwrap();
    let 길 = 자리.path().join("peer.key");
    load_or_create(&길).await.unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&길, std::fs::Permissions::from_mode(0o644)).unwrap();
        let 결과 = load_or_create(&길).await;
        assert!(matches!(결과, Err(KeyError::Permissions(_))), "실제: {결과:?}");
    }
}
```

- [ ] **Step 2: 빨간불 확인**

```bash
timeout 900 cargo test -j2 -p zyris-p2p --test key 2>&1 | tail -20
```

- [ ] **Step 3: 구현**

`crates/zyris-p2p/src/key.rs`:

```rust
//! 노드의 ed25519 키페어. **개인키는 이 머신 밖으로 나가지 않는다.**
//!
//! 자격 파일과 같은 규칙이다 — `0600`이 아니면 거부한다. `FileCredentialStore`가 모드를
//! 검사하는 것과 같은 이유로, 남이 읽을 수 있는 키는 키가 아니다.

use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("키 파일 권한이 {0:o}입니다. 0600이어야 합니다")]
    Permissions(u32),
    #[error("{0}")]
    Io(String),
    #[error("키 파일을 읽을 수 없습니다")]
    Malformed,
}

pub async fn load_or_create(path: &Path) -> Result<iroh::SecretKey, KeyError> {
    match tokio::fs::read(path).await {
        Ok(바이트) => {
            검사(path).await?;
            let 배열: [u8; 32] = 바이트.as_slice().try_into().map_err(|_| KeyError::Malformed)?;
            Ok(iroh::SecretKey::from_bytes(&배열))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 만든다(path).await,
        Err(e) => Err(KeyError::Io(e.to_string())),
    }
}

// 이 블록에는 버그가 둘 있었다. Task 2.3 구현자가 잡았고 아래는 고친 판이다.
//   (1) `SecretKey::generate()`는 **인자를 받지 않는다**(iroh-base 1.0.3에서 확인).
//       그래서 `rand` 의존도 필요 없다.
//   (2) 원래는 `write` 뒤에 `set_permissions`로 좁혔는데, 이 태스크가 스스로 경고한
//       "먼저 만들고 나중에 좁히면 그 사이에 남이 읽는다"를 그대로 저지르고 있었다.
//       `OpenOptions::mode()`로 **파일이 생기는 openat 한 번에** 0600을 확정한다.
async fn 만든다(path: &Path) -> Result<iroh::SecretKey, KeyError> {
    use tokio::io::AsyncWriteExt as _;
    let 키 = iroh::SecretKey::generate();
    if let Some(부모) = path.parent() {
        tokio::fs::create_dir_all(부모).await.map_err(|e| KeyError::Io(e.to_string()))?;
    }
    let mut 열기 = tokio::fs::OpenOptions::new();
    열기.write(true).create_new(true); // create_new — 그 사이 누가 만들어 뒀으면 덮지 않는다
    #[cfg(unix)]
    열기.mode(0o600);
    let mut 파일 = 열기.open(path).await.map_err(|e| KeyError::Io(e.to_string()))?;
    파일.write_all(&키.to_bytes()).await.map_err(|e| KeyError::Io(e.to_string()))?;
    Ok(키)
}

async fn 검사(path: &Path) -> Result<(), KeyError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let 정보 = tokio::fs::metadata(path).await.map_err(|e| KeyError::Io(e.to_string()))?;
        let 모드 = 정보.permissions().mode() & 0o777;
        if 모드 & 0o077 != 0 {
            return Err(KeyError::Permissions(모드));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
```

**`rand` 의존은 필요 없다.** `SecretKey::generate()`가 안에서 알아서 쓴다.
(이 자리에 "`rand = "0.8"`을 더한다"고 적혀 있었는데 틀린 지시였다.)

`lib.rs`에 `pub mod key;`.

- [ ] **Step 4: 초록불 확인 후 커밋**

```bash
timeout 900 cargo test -j2 -p zyris-p2p --test key 2>&1 | tail -10
git add crates/zyris-p2p
git commit -m "feat(p2p): 노드 키페어를 로컬에서 만들고 0600으로 지킨다"
```

---

## Task 2.4: TOFU 고정

**Files:**
- Create: `crates/zyris-p2p/src/tofu.rs`
- Create: `crates/zyris-p2p/tests/tofu.rs`
- Modify: `crates/zyris-p2p/src/lib.rs` (`pub mod tofu;` 한 줄)

**Interfaces:**
- Produces:
  - `#[derive(Clone)] pub struct TofuStore` — **clone이 싸다**(`Arc` 둘). 연결마다 경로로
    다시 만들지 말고 clone할 것. 그래야 아래 쓰기 잠금이 실제로 같은 잠금이 된다.
  - `pub fn new(path: impl Into<PathBuf>) -> TofuStore`
  - `pub async fn check(&self, node_id: &str, endpoint_id: &str) -> Result<(), TofuError>`
    — 처음이면 통과(고정하지 않는다), 고정된 것과 같으면 통과, 다르면 `Changed`,
    **장부를 못 읽으면 `Malformed`/`Io`로 막는다**(뒤 "닫히는 쪽으로 고장난다" 참조).
  - `pub async fn pin(&self, node_id: &str, endpoint_id: &str) -> Result<(), TofuError>`
    — **성공한 뒤에** 고정한다. 이미 고정된 것은 덮지 않는다.
  - `pub enum TofuError { Changed { pinned, offered }, Malformed { path, reason }, Io(String) }`

> **`AsRef<Path>`는 두지 않는다.** Task 4.2가 경로를 꺼내 `TofuStore::new`로 다시 만들면
> 잠금이 인스턴스마다 따로 생겨 아래 쓰기 잠금이 무의미해진다. Task 4.2도 clone을 쓰도록
> 함께 고쳐 두었다.

> ⚠ **아래 Step 3 코드는 실제로 배포된 것과 다르다. 그대로 베끼지 말 것.**
> 정본은 `crates/zyris-p2p/src/tofu.rs`다. 이 코드로 시작해서 리뷰 네 라운드를 거치는 동안
> **Critical 셋**이 나왔고, 셋 다 아래 코드에 들어 있다.
> 1. 장부가 **열리는 쪽으로 고장난다** — `#[serde(default)]` 때문에 `{}`·`[]`·`peers`가 없는
>    객체가 전부 빈 장부로 파싱돼 모든 고정이 조용히 사라진다. `deny_unknown_fields`를 켜고
>    `default`를 뺀다.
> 2. 잠금이 **프로세스 안에서만** 유효하다 — `tokio::sync::Mutex`로는 다른 프로세스를 못 막고,
>    실측으로 8개 중 7개 고정이 에러 없이 사라졌다. 잠금 **파일**로 파일시스템에 심판을 맡긴다
>    (staleness 해제 + rename 직전 nonce 재확인까지 있어야 그 해제가 유실을 다시 열지 않는다).
> 3. 잠금 파일 nonce를 tokio 버퍼드 `write_all`로 쓰고 **flush를 안 한다** — 빈 문자열을 읽어
>    쓰는 사람이 자기뿐인데 "누가 가져갔다"며 중단한다(400회 중 3회).
> 아래 코드는 "무엇을 만들려 했는가"의 기록으로만 남긴다.

### 이 Task가 지켜야 하는 성질 셋

이 셋은 "있으면 좋은 것"이 아니라 **고정이라는 장치가 성립하는 조건**이다. 하나라도 빠지면
공격자가 고정을 조용히 무력화할 수 있고, 그러면 이 파일은 그냥 로그다.

1. **닫히는 쪽으로 고장난다.** 장부를 못 읽으면(깨졌다, 잘렸다, 권한이 없다) `check`는
   **통과시키지 않는다**. 못 읽는 것을 "고정된 게 없다"로 처리하면, 파일 하나 망가뜨리는
   것만으로 모든 고정이 사라진다 — 공격자가 키를 바꿔 끼우기 직전에 할 일이 정확히 그것이다.
   `serde_json::from_slice(..).unwrap_or_default()`가 바로 그 짓이므로 쓰지 않는다.
2. **고정을 잃지 않는다.** `pin`은 읽고-고쳐-쓴다. 두 연결이 동시에 다른 피어를 고정하면
   뒤에 쓴 쪽이 앞의 것을 덮어 **앞 피어의 고정이 사라진다** — 다음에 그 피어가 키를 바꿔도
   "처음 보는 상대"로 통과한다. 읽고-고쳐-쓰기 전체를 `tokio::sync::Mutex`로 감싼다.
   직렬화·쓰기 실패도 `unwrap_or_default()`로 삼키지 않는다. 빈 장부를 쓰면 전부 지워진다.
3. **쓴 것이 남는다.** 임시 파일에 쓰고 rename하되, **rename 전에 `sync_all()`**을 부른다.
   Task 2.3에서 정확히 이것 때문에 Critical이 났다 — `tokio::fs::File::write_all`은 실제
   write를 blocking 풀에 던지고 기다리지 않고 반환하므로, 내용이 0바이트인 파일이
   제자리로 rename될 수 있다(실측 474/500). rename 뒤에는 부모 디렉터리도 fsync한다.

- [ ] **Step 1: 실패하는 테스트를 쓴다**

`crates/zyris-p2p/tests/tofu.rs`:

```rust
use zyris_p2p::tofu::{TofuError, TofuStore};

#[tokio::test]
async fn an_unknown_peer_passes() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    assert!(store.check("node-b", "key-1").await.is_ok());
}

#[tokio::test]
async fn the_same_key_passes_after_pinning() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    store.pin("node-b", "key-1").await.unwrap();
    assert!(store.check("node-b", "key-1").await.is_ok());
}

#[tokio::test]
async fn a_changed_key_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    store.pin("node-b", "key-1").await.unwrap();

    match store.check("node-b", "key-2").await {
        Err(TofuError::Changed { pinned, offered }) => {
            assert_eq!(pinned, "key-1");
            assert_eq!(offered, "key-2");
        }
        other => panic!("a changed key must be refused, got {other:?}"),
    }
}

#[tokio::test]
async fn the_pin_survives_a_new_store_instance() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("peers.json");
    TofuStore::new(&path).pin("node-b", "key-1").await.unwrap();

    // A fresh instance: this has to come off disk.
    let result = TofuStore::new(&path).check("node-b", "key-2").await;
    assert!(matches!(result, Err(TofuError::Changed { .. })));
}

#[tokio::test]
async fn pinning_twice_keeps_the_first_key() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    store.pin("node-b", "key-1").await.unwrap();
    store.pin("node-b", "key-2").await.unwrap();

    // `pin` keeps the first value. If a later call could overwrite it, pinning would
    // mean nothing — the substitution we are trying to catch would pin itself.
    assert!(matches!(store.check("node-b", "key-2").await, Err(TofuError::Changed { .. })));
}

#[tokio::test]
async fn a_corrupt_pin_file_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("peers.json");
    let store = TofuStore::new(&path);
    store.pin("node-b", "key-1").await.unwrap();

    tokio::fs::write(&path, b"{ this is not json").await.unwrap();

    // Treating an unreadable ledger as "nothing is pinned" would let anyone erase every
    // pin by corrupting one file — which is exactly what you would do right before
    // swapping a key.
    let result = store.check("node-b", "key-2").await;
    assert!(matches!(result, Err(TofuError::Malformed { .. })), "got {result:?}");
    // Even the peer that IS pinned correctly must not slip through while we cannot read.
    let same = store.check("node-b", "key-1").await;
    assert!(matches!(same, Err(TofuError::Malformed { .. })), "got {same:?}");
}

#[tokio::test]
async fn pinning_a_second_peer_keeps_the_first() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    store.pin("node-a", "key-a").await.unwrap();
    store.pin("node-b", "key-b").await.unwrap();

    assert!(matches!(store.check("node-a", "other").await, Err(TofuError::Changed { .. })));
    assert!(matches!(store.check("node-b", "other").await, Err(TofuError::Changed { .. })));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_pins_all_survive() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));

    let mut tasks = Vec::new();
    for i in 0..8 {
        let store = store.clone();
        tasks.push(tokio::spawn(async move {
            store.pin(&format!("node-{i}"), &format!("key-{i}")).await.unwrap();
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }

    // A lost pin is not a lost log line: that peer is "unknown" again, so the next key
    // change for it passes unnoticed.
    for i in 0..8 {
        let result = store.check(&format!("node-{i}"), "someone-else").await;
        assert!(matches!(result, Err(TofuError::Changed { .. })), "node-{i} lost its pin");
    }
}
```

- [ ] **Step 2: 빨간불 확인**

```bash
timeout 900 cargo test -j2 -p zyris-p2p --test tofu 2>&1 | tail -20
```

- [ ] **Step 3: 구현**

`crates/zyris-p2p/src/tofu.rs`:

```rust
//! Pin a peer's `EndpointId` to **the first one we saw** (Trust On First Use).
//!
//! attacca issues node credentials and runs the rendezvous, so a "fake B" introduced to A
//! cannot be ruled out by cryptography alone. Pinning turns that into an attack that works
//! **once, never twice, and leaves a mark**.
//!
//! **There is no automatic way out.** A human has to edit the file. Accepting a changed key
//! quietly would make the pin worth nothing.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum TofuError {
    #[error("this node's key changed. pinned: {pinned}, offered: {offered}")]
    Changed { pinned: String, offered: String },
    #[error("the pin file at {path} could not be read ({reason}); refusing to continue")]
    Malformed { path: String, reason: String },
    #[error("{0}")]
    Io(String),
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Ledger {
    #[serde(default)]
    peers: HashMap<String, Entry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Entry {
    endpoint_id: String,
    /// Never read by this code. It is here for the human who opens the file after a
    /// `Changed` error and needs to know when the pin was taken.
    first_seen_ms: u64,
}

/// Clone is cheap and clones share the write lock, so hand out clones instead of
/// rebuilding a store from its path — two stores over one file would each take their own
/// lock and neither would exclude the other.
#[derive(Clone)]
pub struct TofuStore {
    path: Arc<PathBuf>,
    write_lock: Arc<tokio::sync::Mutex<()>>,
}

impl TofuStore {
    pub fn new(path: impl Into<PathBuf>) -> TofuStore {
        TofuStore { path: Arc::new(path.into()), write_lock: Arc::new(tokio::sync::Mutex::new(())) }
    }

    /// Checks the offered key against what is pinned. An unknown peer passes — pinning
    /// happens **after** a connection succeeds, not here.
    ///
    /// A ledger we cannot read is an error, not an empty ledger. See the module docs.
    pub async fn check(&self, node_id: &str, endpoint_id: &str) -> Result<(), TofuError> {
        let ledger = self.read().await?;
        match ledger.peers.get(node_id) {
            None => Ok(()),
            Some(entry) if entry.endpoint_id == endpoint_id => Ok(()),
            Some(entry) => Err(TofuError::Changed {
                pinned: entry.endpoint_id.clone(),
                offered: endpoint_id.to_string(),
            }),
        }
    }

    /// Pins the key of the first connection that succeeded. **Never overwrites a pin.**
    pub async fn pin(&self, node_id: &str, endpoint_id: &str) -> Result<(), TofuError> {
        // Held across read-modify-write. Without it two concurrent pins both read the old
        // ledger and the second write drops the first peer's pin — silently un-pinning it.
        let _guard = self.write_lock.lock().await;
        let mut ledger = self.read().await?;
        if ledger.peers.contains_key(node_id) {
            return Ok(());
        }
        ledger.peers.insert(
            node_id.to_string(),
            Entry { endpoint_id: endpoint_id.to_string(), first_seen_ms: now_ms() },
        );
        self.write(&ledger).await
    }

    async fn read(&self) -> Result<Ledger, TofuError> {
        match tokio::fs::read(self.path.as_path()).await {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| TofuError::Malformed {
                path: self.path.display().to_string(),
                reason: e.to_string(),
            }),
            // No file yet is the honest empty case: nothing has ever been pinned.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Ledger::default()),
            Err(e) => Err(TofuError::Io(e.to_string())),
        }
    }

    async fn write(&self, ledger: &Ledger) -> Result<(), TofuError> {
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| TofuError::Io(e.to_string()))?;
        }
        let text = serde_json::to_vec_pretty(ledger)
            .map_err(|e| TofuError::Io(format!("could not serialize the pin file: {e}")))?;

        // A fixed temp name would collide between processes sharing this file and each
        // would write into the other's half-written temp.
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let temp = self.path.with_extension(format!(
            "{}.{}.tmp",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));

        let result = self.write_temp(&temp, &text).await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&temp).await;
            return result;
        }
        tokio::fs::rename(&temp, self.path.as_path())
            .await
            .map_err(|e| TofuError::Io(e.to_string()))?;
        // The rename itself has to survive a crash, or the newest pin comes back as
        // "never seen" — a peer we already pinned would be trusted fresh.
        #[cfg(unix)]
        if let Some(parent) = self.path.parent() {
            if let Ok(dir) = tokio::fs::File::open(parent).await {
                let _ = dir.sync_all().await;
            }
        }
        Ok(())
    }

    async fn write_temp(&self, temp: &std::path::Path, text: &[u8]) -> Result<(), TofuError> {
        let io = |e: std::io::Error| TofuError::Io(e.to_string());
        #[cfg(unix)]
        {
            use tokio::io::AsyncWriteExt;
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(temp)
                .await
                .map_err(io)?;
            file.write_all(text).await.map_err(io)?;
            // `write_all` returning does not mean the bytes are on disk — tokio hands the
            // real write to a blocking pool and returns. Renaming before this lands puts a
            // zero-length ledger in place, wiping every pin. Task 2.3 measured 474/500.
            file.sync_all().await.map_err(io)?;
        }
        #[cfg(not(unix))]
        tokio::fs::write(temp, text).await.map_err(io)?;
        Ok(())
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
```

`lib.rs`에 `pub mod tofu;`.

- [ ] **Step 4: 초록불 확인**

```bash
timeout 900 cargo test -j2 -p zyris-p2p --test tofu 2>&1 | tail -10
timeout 900 cargo clippy -j2 -p zyris-p2p --all-targets -- -D warnings 2>&1 | tail -5
```

- [ ] **Step 5: 일부러 망가뜨려 본다 (네 군데, 한 번에 하나씩)**

고쳐서 초록이 된 것이 아니라 **테스트가 그 고장을 잡는지** 확인하는 단계다. 넷 다 해 보고
어느 테스트가 실패했는지 보고서에 적는다. 하나 확인할 때마다 되돌린다.

1. `pin`의 `contains_key` 조기 반환을 지운다 → `pinning_twice_keeps_the_first_key`만 실패해야.
2. `read`의 파싱 에러를 `.unwrap_or_default()`로 되돌린다 → `a_corrupt_pin_file_fails_closed`만.
3. `pin`의 `let _guard = ...` 줄을 지운다 → `concurrent_pins_all_survive`가 실패해야.
   **한 번에 안 잡히면 그 테스트를 여러 번 돌려 본다**(`--test-threads=1`로 20회 등).
   그래도 안 잡히면 잡히도록 테스트를 고친다 — 못 잡는 테스트는 없는 것과 같다.
4. `write_temp`의 `sync_all()`을 지운다 → 무엇이 실패하는지 본다. **아무것도 실패하지 않을
   수 있다**(rename 전 sync가 없어도 같은 프로세스 안에서는 대체로 보인다). 그러면 그렇게
   보고한다 — 없는 커버리지를 있다고 하지 말 것. 지어내지 말고 실제로 돌린 결과를 적는다.

- [ ] **Step 6: 커밋**

```bash
git add crates/zyris-p2p
git commit -m "feat(p2p): pin a peer's endpoint id on first use"
```

---

## Task 2.5: `IrohTransport`와 로컬 왕복

**Files:**
- Create: `crates/zyris-p2p/src/transport.rs`, `crates/zyris-p2p/src/peer.rs`
- Create: `crates/zyris-p2p/tests/loopback.rs`

**Interfaces:**
- Consumes: `frame::{encode, decode_header, body, MAX_FRAME}`
- Produces:
  - `pub struct IrohTransport` — `zyris::Transport` 구현
  - `pub const ALPN: &[u8] = b"zyris/1";`
  - `pub async fn dial(endpoint: &iroh::Endpoint, addr: iroh::EndpointAddr) -> Result<IrohTransport, PeerError>`
  - `pub async fn accept_next(endpoint: &iroh::Endpoint) -> Option<PendingConnection>`
    — **상대를 기다리지 않는다.** 핸드셰이크를 여기서 하면 붙고 침묵하는 상대 하나가
    리스너 전체를 막는다(Task 2.5 리뷰가 25초 초과·무한으로 실측).
  - `pub async fn establish(pending: PendingConnection, deadline: Duration) -> Result<(iroh::EndpointId, IrohTransport), PeerError>`
    — 핸드셰이크와 `accept_bi()`를 마감을 들고 한다. **연결마다 spawn해서 부른다.**
  - `pub struct PendingConnection` — 이름이 `Accepting`이 아닌 이유는 `iroh::endpoint::Accepting`과
    겹치기 때문이다.

- [ ] **Step 1: 실패하는 테스트를 쓴다**

`crates/zyris-p2p/tests/loopback.rs`:

```rust
//! 한 프로세스에서 iroh 엔드포인트 둘을 띄워 **진짜로** 붙인다. 릴레이 없이 루프백에서 붙으므로
//! CI에서 돈다.

use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zyris::{AcceptOptions, Node, NodeKind};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Echoed {
    pub said: String,
}

#[zyris::capability(name = "echo", version = 1)]
pub trait Echo {
    async fn say(&self, text: String) -> zyris::Result<Echoed>;
}

struct 메아리;

#[async_trait::async_trait]
impl Echo for 메아리 {
    async fn say(&self, text: String) -> zyris::Result<Echoed> {
        Ok(Echoed { said: text })
    }
}

#[tokio::test]
async fn 두_엔드포인트가_붙어_zyris를_말한다() {
    // `generate()`는 인자를 받지 않는다 (Global Constraints의 검증된 API 표 참조).
    // Task 2.3 브리프가 여기서 `rand`을 넘기라고 적었다가 틀렸다 — 같은 실수를 반복하지 말 것.
    let a_key = iroh::SecretKey::generate();
    let b_key = iroh::SecretKey::generate();

    let b_ep = iroh::Endpoint::builder()
        .secret_key(b_key.clone())
        .alpns(vec![zyris_p2p::transport::ALPN.to_vec()])
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .unwrap();
    let b_addr = b_ep.node_addr().await.unwrap();

    // B: 수락하고 acceptor 쪽 Connection을 세운다.
    let b_task = tokio::spawn(async move {
        let (peer, transport) =
            zyris_p2p::peer::accept_next(&b_ep).await.unwrap().unwrap();
        let node = Node::builder()
            .name("b")
            .kind(NodeKind::Cli)
            .capability(EchoServer(메아리))
            .build()
            .unwrap();
        // `AcceptOptions::default()`를 쓰면 안 된다 — 아래 "제약 셋"의 3번이다.
        // 테스트에서 기본값으로 두면 배선 실수가 라이브까지 안 드러난다.
        let opts = AcceptOptions { node_id: "b".into(), ..AcceptOptions::default() };
        let conn = node.accept(transport, opts).await.unwrap();
        (peer, conn)
    });

    let a_ep = iroh::Endpoint::builder()
        .secret_key(a_key)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .unwrap();
    let transport = zyris_p2p::peer::dial(&a_ep, b_addr).await.unwrap();
    let a_node = Node::builder().name("a").kind(NodeKind::Cli).build().unwrap();
    let a_conn = a_node.connect_over(transport).await.unwrap();

    let (붙은_상대, _b_conn) = b_task.await.unwrap();
    assert_eq!(붙은_상대, a_key.public(), "B가 A의 EndpointId를 알아야 한다");

    let echo: EchoClient = a_conn.wait_capability(Duration::from_secs(5)).await.unwrap();
    let 답 = echo.say("안녕".into()).await.unwrap();
    assert_eq!(답.said, "안녕");
}

#[tokio::test]
async fn 큰_메시지도_한_조각으로_왕복한다() {
    // 프레이밍이 청크 경계에서 안 깨지는지 본다. QUIC은 바이트 스트림이라 read_exact 한 번에
    // 다 오지 않는다 — 헤더가 5바이트씩 잘려 오거나 몸이 여러 번에 나뉘어 온다.
    // **이 테스트는 스텁이 아니다. 위 테스트와 같은 배선을 헬퍼로 뽑아 실제로 왕복시킨다.**
    let (a_conn, _b) = 붙인다().await;
    let echo: EchoClient = a_conn.wait_capability(Duration::from_secs(5)).await.unwrap();

    let 긴_말 = "가".repeat(300_000);   // UTF-8로 900,000 바이트
    assert!(긴_말.len() > 512 * 1024, "must exceed one QUIC read");
    let 답 = echo.say(긴_말.clone()).await.unwrap();
    assert_eq!(답.said.len(), 긴_말.len(), "length must survive the round trip");
    assert_eq!(답.said, 긴_말, "content must survive the round trip");
}
```

> **`붙인다()`(영어로는 `connect_pair()`)를 먼저 만든다.** 두 테스트가 같은 배선을 쓴다:
> 엔드포인트 둘을 띄우고, B가 `accept_next`로 받아 `EchoServer`를 얹고, A가 `dial`해서
> `connect_over`한다. 반환은 `(a_conn, b_conn)`이면 충분하다. 첫 테스트는 여기에
> `assert_eq!(붙은_상대, a_key.public())`가 더 필요하므로, 헬퍼가 상대 EndpointId도
> 함께 돌려주게 하거나 첫 테스트만 배선을 펼쳐 쓴다 — 어느 쪽이든 좋다.

`Cargo.toml`의 `[dev-dependencies]`에 `schemars.workspace = true`, `serde.workspace = true`,
`async-trait.workspace = true`를 더한다. **`rand`은 더하지 않는다** — `SecretKey::generate()`가
인자를 안 받으므로 필요 없다.

- [ ] **Step 2: 빨간불 확인**

```bash
timeout 900 cargo test -j2 -p zyris-p2p --test loopback 2>&1 | tail -30
```

- [ ] **Step 3: 구현**

`crates/zyris-p2p/src/transport.rs`:

```rust
//! iroh의 QUIC bi-stream을 zyris의 `Transport`로 감싼다.
//!
//! bi-stream 하나만 쓴다. zyris가 이미 자기 층에서 다중화하고 크레딧으로 head-of-line blocking을
//! 다루므로 QUIC 스트림을 더 열 이유가 없다.

use async_trait::async_trait;
use iroh::endpoint::{RecvStream, SendStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zyris::{Transport, TransportError, WireSink, WireStream};
use zyris_proto::WireMessage;

use crate::frame::{body, decode_header, encode};

pub const ALPN: &[u8] = b"zyris/1";

pub struct IrohTransport {
    conn: iroh::endpoint::Connection,
    send: SendStream,
    recv: RecvStream,
}

impl IrohTransport {
    pub fn new(
        conn: iroh::endpoint::Connection,
        send: SendStream,
        recv: RecvStream,
    ) -> IrohTransport {
        IrohTransport { conn, send, recv }
    }
}

pub struct IrohSink {
    conn: iroh::endpoint::Connection,
    send: SendStream,
}

pub struct IrohRead(RecvStream);

#[async_trait]
impl WireSink for IrohSink {
    async fn send(&mut self, msg: WireMessage) -> Result<(), TransportError> {
        self.send
            .write_all(&encode(&msg))
            .await
            .map_err(|e| TransportError::Io(e.to_string()))
    }

    async fn close(&mut self, code: u16, reason: String) -> Result<(), TransportError> {
        let _ = self.send.finish();
        self.conn.close((code as u32).into(), reason.as_bytes());
        Ok(())
    }
}

#[async_trait]
impl WireStream for IrohRead {
    async fn next(&mut self) -> Option<Result<WireMessage, TransportError>> {
        let mut 머리 = [0u8; 5];
        if let Err(e) = self.0.read_exact(&mut 머리).await {
            // 정상 종료와 오류를 가른다 — 끊긴 것을 오류로 올리면 재연결이 시끄러워진다.
            // (std::io::Error는 필드가 비공개라 패턴 매치가 안 된다. `kind()`로 본다.)
            return if 끝인가(&e) { None } else { Some(Err(TransportError::Io(e.to_string()))) };
        }
        let (kind, len) = match decode_header(&머리) {
            Ok(v) => v,
            Err(e) => return Some(Err(TransportError::Io(e.to_string()))),
        };
        let mut 몸 = vec![0u8; len];
        if let Err(e) = self.0.read_exact(&mut 몸).await {
            return Some(Err(TransportError::Io(e.to_string())));
        }
        Some(body(kind, 몸).map_err(|e| TransportError::Io(e.to_string())))
    }
}

fn 끝인가(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::UnexpectedEof
        || e.kind() == std::io::ErrorKind::ConnectionReset
}

impl Transport for IrohTransport {
    fn split(self: Box<Self>) -> (Box<dyn WireSink>, Box<dyn WireStream>) {
        (
            Box::new(IrohSink { conn: self.conn, send: self.send }),
            Box::new(IrohRead(self.recv)),
        )
    }
}
```

`crates/zyris-p2p/src/peer.rs`:

```rust
//! 엔드포인트를 다이얼하고 수락한다.

use crate::transport::{IrohTransport, ALPN};

#[derive(Debug, thiserror::Error)]
pub enum PeerError {
    #[error("붙지 못했습니다: {0}")]
    Connect(String),
    #[error("스트림을 열지 못했습니다: {0}")]
    Stream(String),
}

pub async fn dial(
    endpoint: &iroh::Endpoint,
    addr: iroh::EndpointAddr,
) -> Result<IrohTransport, PeerError> {
    let conn =
        endpoint.connect(addr, ALPN).await.map_err(|e| PeerError::Connect(e.to_string()))?;
    let (mut send, recv) =
        conn.open_bi().await.map_err(|e| PeerError::Stream(e.to_string()))?;
    Ok(IrohTransport::new(conn, send, recv))
}

pub async fn accept_next(
    endpoint: &iroh::Endpoint,
) -> Option<Result<(iroh::EndpointId, IrohTransport), PeerError>> {
    let incoming = endpoint.accept().await?;
    let conn = match incoming.await {
        Ok(c) => c,
        Err(e) => return Some(Err(PeerError::Connect(e.to_string()))),
    };
    let peer = match conn.remote_id() {
        Ok(id) => id,
        Err(e) => return Some(Err(PeerError::Connect(e.to_string()))),
    };
    let (send, recv) = match conn.accept_bi().await {
        Ok(v) => v,
        Err(e) => return Some(Err(PeerError::Stream(e.to_string()))),
    };
    Some(Ok((peer, IrohTransport::new(conn, send, recv))))
}
```

`lib.rs`에 `pub mod peer; pub mod transport;`.

> **iroh 1.x의 실제 이름을 확인할 것.** `node_addr()`·`remote_id()`·`EndpointAddr`는 1.0에서
> 이름이 바뀐 자리다(예전 `NodeId`/`NodeAddr`). 컴파일 오류가 나면
> `cargo doc -p iroh --open` 또는 `docs.rs/iroh`에서 맞춘다. **추측으로 고치지 말 것.**
> 벤더된 소스가 정본이다: `~/.cargo/registry/src/index.crates.io-*/iroh-1.0.3/src/`.
> `zyris`의 `Transport`·`WireSink`·`WireStream`·`AcceptOptions`도 마찬가지로 실물을 먼저 본다
> (`crates/zyris/src/`). 이 플랜이 기억으로 적은 시그니처는 이미 여러 번 틀렸다.

> ⚠ **누가 먼저 말하는지 확인하고 시작할 것 — 여기가 교착이 나는 자리다.**
> `accept_bi()`는 **다이얼한 쪽이 그 스트림에 바이트를 보내야** 돌아온다. 그래서
> `accept_next`는 다이얼러의 첫 프레임이 오기 전까지 `IrohTransport`를 만들지 못한다.
> zyris의 핸드셰이크에서 **수락하는 쪽이 먼저 말한다면 양쪽이 서로를 기다리며 멈춘다.**
> 코드를 쓰기 전에 `crates/zyris/src/connection.rs`에서 `connect_over`와 `accept`가 각각
> 처음에 무엇을 보내는지 읽고, 다이얼러가 먼저 보내는 것을 **확인한 뒤에** 진행한다.
> 아니라면 멈추고 보고할 것 — 설계가 바뀐다.
> (플랜 초안에는 여기 `send.write_all(&[])`이 있었다. 빈 슬라이스는 아무것도 보내지 않으니
> 교착을 막지 못한다. 지웠다. 한 바이트를 억지로 끼워 넣는 것도 답이 아니다 —
> 받는 쪽이 프레임 파서라 그 바이트가 헤더로 읽힌다.)

### 실측이 잡아낸 제약 셋 — 여기서 어기면 조용히 깨진다

1. **QUIC bi-stream을 하나만 쓴다.** 모든 프레임이 `WriterCmd` 채널 하나를 지나 sink를 혼자
   소유한 writer 태스크로 간다(`connection.rs:754, 787, 828-850`). `s_credit`·`s_end`가 자기가
   가리키는 STREAM_DATA를 추월하면 안 되므로 **제어와 데이터를 스트림 둘로 나누면 깨진다.**
   나뉜 상태로도 대개 돌다가 부하가 걸릴 때만 틀어지므로 테스트로 잡기 어렵다.
2. **순서가 뒤바뀌면 전송이 즉사한다.** `handle_frame`이 `seq != next_seq`면 `StreamLagged`로
   스트림을 죽인다(`connection.rs:962-971`). QUIC 한 스트림 안이면 보장되므로 1번을 지키면 된다.
3. **`AcceptOptions::default()`를 그대로 쓰면 안 된다.** `node_id`가 무작위 UUID라 양쪽이 서로의
   신원을 잘못 안다. **수락하는 쪽의 진짜 node_id를 채운다:**

```rust
let opts = AcceptOptions { node_id: 내_node_id.clone(), ..AcceptOptions::default() };
let conn = node.accept(transport, opts).await?;
```

   테스트에서도 마찬가지다 — 기본값으로 두면 배선 실수가 라이브까지 안 드러난다.

**`runtime::Runner`는 P2P에 못 쓴다.** `runner.rs:330`이 `node.connect(&url, &bearer)`로 웹소켓
다이얼러에 하드코딩되어 있다. Task 4.2의 수락 루프를 직접 돌린다.

- [ ] **Step 4: 초록불 확인**

```bash
timeout 1200 cargo test -j2 -p zyris-p2p --test loopback 2>&1 | tail -30
```

기대: 2 passed. `큰_메시지도_한_조각으로_왕복한다`의 배선을 헬퍼로 뽑아 실제로 채운다.

- [ ] **Step 5: 커밋**

```bash
git add crates/zyris-p2p
git commit -m "feat(p2p): iroh QUIC 위에 zyris Connection을 세운다"
```

---

## Task 2.6: 단계 2 마무리 — PR

- [ ] **Step 1: 전체 검사**

```bash
timeout 1200 cargo test -j2 -p zyris-p2p 2>&1 | tail -20
# --no-deps를 붙인다. 붙이지 않으면 `crates/zyris`의 **기존** 린트 둘에서 멈춘다
# (connection.rs · testing.rs, main에서도 실패하고 이 브랜치는 건드리지 않았다).
timeout 1200 cargo clippy -j2 -p zyris-p2p --no-deps --all-targets -- -D warnings 2>&1 | tail -20
# CI가 실제로 도는 명령은 이것 하나뿐이다(check.yml). 컨트롤러가 직접 돌린다.
timeout 3000 cargo test --workspace -j2 2>&1 | tail -20
```

- [ ] **Step 2: 반영**

**PR 제목과 본문은 영어다.** (Global Constraints의 언어 규칙.)

```bash
git fetch origin && git log --oneline HEAD..origin/main
git push -u origin feat/p2p-transport
gh pr create --repo attacca-cc/zyris \
  --title "feat(p2p): carry zyris over a direct node-to-node connection" \
  --body-file <(cat <<'EOF'
## What this is for

A transport that lets two nodes speak zyris to each other without going through attacca. §8
promised this path; not a line of it existed.

## The zyris core is untouched

The `Transport` trait only ever carries `WireMessage::{Binary, Text}`, and the only
client/server asymmetry is `Role::Dial` / `Role::Accept` — so once it is settled who dialled,
everything else already fit. `IrohTransport` is just the fifth `Transport` implementation.

## What is in it

- **`frame`** — length-prefixed framing over the QUIC byte stream. The 16 MiB cap bounds a
  *declared* length, and a declaration alone is cheap: the pages are lazy, so a header claiming
  16 MiB costs a few hundred kB resident, not 16 MiB. What is not cheap is a peer that
  **delivers** most of what it declared and then stops — measured at ~14 MB held for as long as
  the connection lives. So both a cap and a deadline are load-bearing here, and the deadlines
  are the part that was missing.
- **`key`** — the node's ed25519 keypair. Generated locally, created at `0600` in one `openat`
  rather than narrowed afterwards, and refused on load if anyone else can read it. `sync_all`
  before the call returns: without it the next read finds a zero-length file (measured 474 of
  500 runs), and losing this key makes us a different node that every peer who pinned us
  refuses.
- **`tofu`** — pins a peer's `EndpointId` to the first one seen. **There is no automatic way
  out** — a human edits the file. It fails closed when the ledger is unreadable, because
  treating an unreadable ledger as an empty one would let anyone erase every pin by corrupting
  one file. Writers are serialized by a lock file, not a process-local mutex: with a mutex, six
  child processes pinning ten peers each lost **42 of 60 pins with no error**, and a lost pin
  means the next key change for that peer passes unnoticed. A stale lock is broken on age,
  and the writer re-verifies its nonce immediately before publishing so that breaking one
  cannot re-open the loss it was preventing.
- **`transport` · `peer`** — dialling and accepting. `accept_next` returns without waiting for
  the peer and `establish` does the handshake under a deadline in a spawned task, because doing
  it inline meant one peer that connected and stayed silent wedged the entire listener. Closing
  waits for the peer to acknowledge buffered data before tearing the connection down — closing
  immediately after `finish()` dropped a 900 KB frame outright — and that wait is itself
  bounded, since a peer that vanishes without a QUIC close never resolves it.

`zyris-p2p` is a workspace member that **nothing depends on yet**, so no existing node pulls
iroh in by depending on it. Note this is weaker than it sounds: CI builds the workspace, so CI
does compile iroh. Making it a real opt-in feature of the crates that will consume it belongs
with the wiring in a later phase.

## What is not here

The rendezvous. Two nodes can find each other only if something already told them where to
look; attacca's side of that is the next phase. There is also no relay yet, so two nodes behind
symmetric NATs cannot reach each other.

## Verification

Two real iroh endpoints in one process, connected over real QUIC with relays disabled, speaking
real zyris — including a 900 KB payload, which is well past what one QUIC read returns and is
what proves the framing survives a stream that does not respect message boundaries.

Most invariants here were checked by breaking them and watching a test fail rather than by
reading the code. That was not ceremony: **twelve** tests on this branch were green while the
code under them was broken, and every one of them was caught that way — never by review alone.
EOF
)
```

---

# 단계 2.5 — 지문 확인 (zyris 리포)

> **왜 이 단계가 생겼나.** 단계 2 최종 리뷰가 "고정을 `node_id`에 걸면 attacca가 가짜를 새
> 노드로 소개해 통과시킨다"를 잡아 열쇠를 `slug`로 옮겼다. 그 뒤 attacca 실물을 조사한 결과
> **slug도 앵커가 못 된다**는 것이 확인됐다(2026-08-10):
>
> - `slug`는 `name`에서 파생되고(`attacca-domain/src/zyris_node.rs:137`), device-grant 등록에서
>   `name`의 기본값은 **등록하려는 기기가 스스로 보고한 `requested_name`**이다. 웹 대화상자가
>   그 값을 미리 채우고 Authorize가 그대로 받는다 — 사용자가 한 글자도 안 쳐도 된다.
>   비워 두면 서버가 `"New node"`로 채운다.
> - `slug`는 유일하지도 안정적이지도 않다. rename이 충돌 검사 없이 다시 계산하고 DB 제약이
>   없다(코드 주석이 "no unique constraint to race against"라고 직접 적어 두었다).
>   충돌하면 `created_at desc limit 1`로 최신 행이 이긴다. **revoke 후 같은 이름으로 다시
>   만드는 것이 의도된 동작**이고 `a_revoked_name_is_free_again` 테스트가 그것을 못박는다 —
>   정확히 "같은 이름, 다른 키"다.
>
> **그래서 서버가 발급하는 어떤 이름도 앵커가 될 수 없다.** attacca를 고쳐도 마찬가지다 —
> 서버가 적대적이라는 것이 위협 모델인데 그 서버의 DB를 믿는 것은 순환이다.
> 앵커는 **사람**에게서 와야 한다. 2026-08-10 사용자 결정: **지문을 한 번 확인한다.**

## 무엇이 바뀌나

고정의 의미가 바뀐다. 지금은 "처음 본 것을 말없이 고정"이고, 바뀐 뒤에는
**"사람이 한 번 확인한 것을 고정"**이다. 이름은 라벨로 내려앉고 **키가 곧 신원**이 된다.
SSH의 host key 확인, Signal의 safety number와 같은 자리다.

바뀌지 않는 것: 키가 바뀌면 거절하고 **자동으로 푸는 길을 두지 않는다**. 사람이 파일을 고쳐야 한다.

## Task 2.7: 지문과 확인 훅

**Files:**
- Create: `crates/zyris-p2p/src/fingerprint.rs`
- Modify: `crates/zyris-p2p/src/tofu.rs`, `crates/zyris-p2p/src/lib.rs`
- Modify: `crates/zyris-p2p/Cargo.toml` (`sha2` 추가)
- Test: `crates/zyris-p2p/tests/tofu.rs`, 인라인 유닛 테스트

**Interfaces:**
- Produces:
  - `pub fn fingerprint(endpoint_id: &str) -> String`
  - `pub trait PeerConfirmer` — `async fn confirm(&self, label: &str, fingerprint: &str) -> bool`
  - `pub struct DenyUnknown` — `PeerConfirmer` 구현. 항상 `false`.
  - `TofuStore::authorize(&self, confirmer: &dyn PeerConfirmer, peer_slug: &str, endpoint_id: &str) -> Result<(), TofuError>`
  - `TofuError::Refused`

### 지문은 128비트다 — 짧게 만들지 말 것

공격자는 키페어를 마음대로 만든다. 그래서 필요한 것은 **제2 원상 저항**이고, 64비트 지문은
2^64로 갈아 낼 수 있는 범위에 들어온다. `sha256(endpoint_id)`의 앞 **16바이트**를 쓴다.

사람이 읽고 비교하는 것이므로 4자씩 끊어 보여준다:

```
9F2A 41C7 0E83 BB15 6D04 A97E 22C1 5FB8
```

**고정되는 것은 지문이 아니라 `endpoint_id` 전체다.** 지문은 사람이 눈으로 맞춰 보라고
만드는 표현일 뿐이다. 장부에 지문을 저장하면 잘린 값에 고정하는 셈이 된다.

### 확인 중에는 잠금을 쥐지 않는다

`authorize`는 이렇게 돈다:

1. 잠금 없이 `check` — 아는 상대면 여기서 끝난다(같으면 `Ok`, 다르면 `Changed`).
2. 모르는 상대일 때만 `confirmer.confirm(label, fingerprint)`를 부른다. **사람을 기다리는
   동안 파일 잠금을 쥐면 안 된다** — 잠금은 60초면 stale로 간주되어 깨지도록 되어 있고
   (Task 2.4), 사람은 60초보다 오래 걸린다.
3. 거절하면 `Refused`. 수락하면 `pin`을 부른다. `pin`은 잠금을 잡고 **다시 확인**하므로,
   사람이 고민하는 사이에 다른 연결이 다른 키로 고정했다면 그때 `Changed`가 나온다
   (Task 2.5 fix round의 F3가 `pin`에 불일치 감지를 넣어 둔 덕에 공짜로 얻는다).

### 양쪽 다 확인한다

A가 B에 걸 때 A는 B의 지문을 본다. B가 A의 연결을 받을 때 B는 A의 지문을 본다.
**한쪽만 확인하면 확인하지 않은 쪽은 아무나 받는다.** 다이얼 경로와 수락 경로 양쪽에 건다.

### 사람이 없는 노드

`zyris-daemon`처럼 물어볼 사람이 없는 노드는 `DenyUnknown`을 쓴다 — **닫히는 쪽으로
고장난다.** 미리 승인하는 길(설정 파일에 지문을 적어 두는 등)은 그것을 쓰는 노드가 정한다.
`zyris-p2p`는 정책을 정하지 않고 훅만 낸다.

- [ ] **Step 1: 실패하는 테스트를 쓴다** — 지문이 128비트인지, 같은 키가 같은 지문을 주는지,
  한 비트만 달라도 지문이 달라지는지. `authorize`가 (a) 아는 같은 키는 확인 없이 통과시키고
  (b) 아는 다른 키는 확인을 **묻지도 않고** 거절하고 (c) 모르는 키는 묻고, 거절하면 고정하지
  않고, 수락하면 고정하는지. 확인기가 몇 번 불렸는지 세는 스텁으로 (a)·(b)를 잡는다.
- [ ] **Step 2~4:** 빨간불 → 구현 → 초록불.
- [ ] **Step 5: 일부러 망가뜨린다.** 지문 길이를 8바이트로 줄인다 / (b)에서 확인기를 부르게
  만든다 / 거절인데 고정하게 만든다 / 확인 중에 잠금을 쥐게 만든다. 각각 어느 테스트가
  무는지 적는다. **아무것도 안 물면 그것이 발견이다.**
- [ ] **Step 6:** 커밋. 설계 문서(`docs/superpowers/specs/…`)의 TOFU 절도 함께 고친다 —
  지금 문서는 조용한 TOFU를 서술하고 있어 사실과 다르다.

---

# 단계 3 — attacca 랑데부

> **이 단계는 배포가 먼저 가야 한다.** attacca가 모르는 스코프가 등록 요청에 하나라도 들어가면
> `/zyris/v1/device/authorize`가 **422로 통째로 거절한다.** 2026-08-03에 `nodes:write`로 실제로
> 그렇게 걸렸다. 단계 4는 이 단계가 **배포된 뒤에** 시작한다.

## Task 3.1: `attacca_api` 선언 (zyris 리포)

**Files:**
- Modify: `crates/zyris-attacca/src/lib.rs`
- Modify: `crates/zyris-attacca/tests/attacca_api_roundtrip.rs`

**Interfaces:**
- Produces: `ZScope::PeersWrite`, `ZPeerAddr`, `ZPeerEntry`, 그리고 `AttaccaApi`의 세 메서드.

- [ ] **Step 1: 실패하는 테스트를 쓴다**

`crates/zyris-attacca/tests/attacca_api_roundtrip.rs`에:

```rust
#[test]
fn 랑데부_도구_셋이_descriptor에_있다() {
    let d = attacca_api_capability();
    for 이름 in ["peer_publish", "peer_lookup", "peer_list"] {
        let t = d.tool(이름).unwrap_or_else(|| panic!("{이름}이 없다"));
        assert_eq!(t.transfer, Transfer::Unary, "{이름}");
    }
    // turn_events가 여전히 유일한 스트림이어야 한다.
    // (`&String`과 `&str`은 비교가 안 된다 — `as_str()`로 맞춘다.)
    let streams: Vec<&str> = d
        .tools
        .iter()
        .filter(|t| t.transfer == Transfer::UniStream)
        .map(|t| t.name.as_str())
        .collect();
    assert_eq!(streams, ["turn_events"]);
}

#[test]
fn peers_write_스코프가_있다() {
    let s: ZScope = serde_json::from_str("\"peers:write\"").unwrap();
    assert_eq!(s, ZScope::PeersWrite);
}
```

기존 테스트 파일의 `AttaccaApi` 구현 스텁에도 세 메서드를 더해야 컴파일된다.

- [ ] **Step 2: 빨간불 확인**

```bash
timeout 600 cargo test -j2 -p zyris-attacca 2>&1 | tail -20
```

- [ ] **Step 3: 구현**

`crates/zyris-attacca/src/lib.rs`의 `ZScope`에:

```rust
    /// 같은 계정 노드끼리의 P2P 랑데부 — 자기 주소를 올리고 형제의 주소를 묻는다.
    /// 파일 자체는 attacca를 지나지 않으므로 이 스코프가 여는 것은 **주소록뿐**이다.
    #[serde(rename = "peers:write")]
    PeersWrite,
```

DTO:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ZPeerAddr {
    pub node_id: String,
    pub slug: String,
    /// iroh EndpointId — ed25519 공개키. 상대가 이것으로 신원을 증명한다.
    pub endpoint_id: String,
    /// 홀펀칭 후보 주소들.
    #[serde(default)]
    pub addrs: Vec<String>,
    /// 배포가 운영하는 릴레이. 하드코딩하지 않으려고 여기 실어 보낸다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_url: Option<String>,
    pub online: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ZPeerEntry {
    pub node_id: String,
    /// **TOFU 고정의 열쇠가 이것이다.** 그래서 이 값은 **사용자가 정한 이름**이어야 하고
    /// 서버가 조용히 다시 발급할 수 있는 것이면 안 된다.
    ///
    /// 최종 브랜치 리뷰가 잡은 것: 열쇠를 `node_id`로 두면 attacca가 가짜 노드를 **새 노드**
    /// (새 node_id, 같은 slug)로 소개하는 것만으로 "처음 보는 상대"가 되어 통과하고 고정된다.
    /// 매번 통하고 흔적도 안 남으니 고정이 아무것도 막지 못한다.
    /// `node_name`(`enroll/protocol.rs`의 `TokenResponse.node_name`)도 같은 이유로 안 된다 —
    /// 그것도 attacca가 주는 값이고 검증되지 않는다.
    ///
    /// **단계 3에서 확인할 것:** attacca가 slug를 사용자 입력에서만 만드는지, 사용자가
    /// 덮어쓰지 않으면 서버 제안값이 그대로 굳는 경로가 없는지. 있으면 같은 붕괴가
    /// 한 층 위에서 되살아난다.
    pub slug: String,
    pub endpoint_id: String,
    pub online: bool,
}
```

트레이트에:

```rust
    /// 내 iroh 주소를 올린다. 주소가 바뀔 때마다. `peers:write`.
    ///
    /// `endpoint_id`는 **처음 올린 값이 계속 간다.** 다른 값으로 덮으려는 요청은 거절된다 —
    /// 서버가 조용히 키를 갈아 끼우는 자리를 만들지 않기 위해서다.
    async fn peer_publish(&self, endpoint_id: String, addrs: Vec<String>) -> zyris::Result<()>;

    /// 같은 계정의 다른 노드 주소를 묻는다. `peers:write`.
    ///
    /// **조회 열쇠는 slug다.** 사용자가 "내 노트북에 보내"라고 할 때 그 이름이 그대로 열쇠가
    /// 되어야, TOFU 고정이 같은 이름에 대해 같은 키를 요구한다. node_id로 조회하면
    /// 고정이 무엇을 지키는지가 사용자가 말한 것과 어긋난다 (단계 2 최종 리뷰).
    async fn peer_lookup(&self, slug: String) -> zyris::Result<ZPeerAddr>;

    /// 같은 계정의 노드 목록. 들어온 연결을 받아도 되는지 판정하는 데 쓴다. `peers:write`.
    async fn peer_list(&self) -> zyris::Result<Vec<ZPeerEntry>>;
```

- [ ] **Step 4: 초록불 확인 후 PR**

**커밋 메시지와 PR은 영어다.** 브랜치는 `feat/attacca-rendezvous`가 이미 만들어져 있다 —
새로 만들거나 갈아타지 말 것.

```bash
timeout 600 cargo test -j2 -p zyris-attacca 2>&1 | tail -10
git add crates/zyris-attacca
git commit -m "feat(attacca_api): declare the three peer rendezvous tools"
# push와 PR은 컨트롤러가 한다.
```

**이 PR이 병합되고 rev가 정해져야 attacca 쪽 작업을 시작할 수 있다.**

---

## Task 3.2: DB 마이그레이션 (attacca 리포)

**Files:**
- Create: `crates/attacca-repo-pg/migrations/<다음번호>_zyris_peer.sql`

- [ ] **Step 1: 최신을 받고 브랜치를 딴다**

```bash
cd /home/ruma/attacca
git fetch origin && git switch main && git pull --ff-only
git switch -c feat/zyris-peer-rendezvous
ls crates/attacca-repo-pg/migrations | tail -3   # 다음 번호 확인
```

다음 번호는 **`0119`**다 (2026-08-09 확인, `0118_files_pinned.sql`이 마지막). 파일명은
`0119_zyris_peer.sql`.

- [ ] **Step 2: 마이그레이션을 쓴다**

```sql
-- zyris 노드의 P2P 신원과 주소.
--
-- endpoint_id는 노드가 자기 머신에서 만든 ed25519 공개키다. 개인키는 서버에 오지 않는다.
-- 한 계정 안에서 유일해야 한다 — 두 노드가 같은 키를 주장하면 라우팅이 갈린다.
ALTER TABLE zyris_nodes
    ADD COLUMN endpoint_id   text,
    ADD COLUMN peer_addrs    jsonb NOT NULL DEFAULT '[]'::jsonb,
    ADD COLUMN peer_addrs_at timestamptz;

CREATE UNIQUE INDEX zyris_nodes_endpoint_id_per_owner
    ON zyris_nodes (owner_user_id, endpoint_id)
    WHERE endpoint_id IS NOT NULL;
```

- [ ] **Step 3: 어디서 검증할 것인가 — 이 머신에서는 못 한다**

> **2026-08-10 실측:** 이 머신에는 **Postgres 서버도 docker/podman도 없다.** `psql` 클라이언트만
> 있다(`/usr/bin/psql`, `initdb`·`pg_ctl`·`postgres` 없음). `.env.example`의 `DATABASE_URL`은
> 주석 처리된 `localhost:55432`이고 아무것도 듣고 있지 않다.
> 그래서 플랜 초안의 `docker run postgres:16`은 **여기서 돌지 않는다.**

`attacca-repo-pg`의 통합 테스트는 `#[sqlx::test(migrations = "./migrations")]`라 살아 있는
Postgres가 필요하고, CI의 `check.yml`에도 postgres 서비스가 없다. 즉 **지금 이대로면 이
마이그레이션은 프로덕션에서 처음 돌아간다.** 그건 받아들일 수 없다.

**그래서 CI에 검증을 붙인다.** 통합 테스트 전체를 CI에 들이지는 않는다 — 한 번도 CI에서 돌아
본 적이 없어 무엇이 깨져 있을지 모르고, 그걸 여기서 떠안으면 이 작업이 통째로 늘어난다.
**마이그레이션이 깨끗하게 적용되는지만** 본다:

- `check.yml`에 postgres 서비스가 붙은 잡을 하나 더한다.
- 빈 DB에 `sqlx migrate run`으로 `crates/attacca-repo-pg/migrations`를 전부 적용한다.
- 그것만 확인한다. 실패하면 PR이 막힌다.

이러면 이 마이그레이션의 첫 실행이 **프로덕션이 아니라 PR**이 된다.

> 사용자가 로컬 검증을 원하면 `sudo pacman -S postgresql` 후 `initdb`·`pg_ctl start`가 필요하다.
> **시스템 패키지 설치이므로 사용자가 직접 결정한다** — 에이전트가 알아서 깔지 않는다.

- [ ] **Step 4: 커밋**

**커밋 메시지는 영어다.**

```bash
git add crates/attacca-repo-pg/migrations .github/workflows/check.yml
git commit -m "feat(zyris): add the node's p2p identity and addresses"
```

> **attacca는 무조건 PR이다.** `main`에 직접 push하지 않는다.

---

## Task 3.3: 스코프와 게이트웨이 (attacca 리포)

**Files:**
- Modify: `crates/attacca-domain/src/…` (`ApiScope`)
- Modify: `crates/attacca-server/src/zyris_gateway.rs`
- Modify: `crates/attacca-service/src/…` (노드 서비스)
- Modify: `Cargo.toml` (zyris rev를 Task 3.1의 병합 커밋으로)

- [ ] **Step 1: 스코프를 더한다**

**자리는 넷이고 전부 확인해 뒀다** (2026-08-09, attacca `e3a7b1f` 기준). `NodesWrite`가 나오는
곳이 정확히 이 목록이다 — `grep -rn "NodesWrite" crates/ --include="*.rs"`로 언제든 다시 잰다.

| 파일:줄 | 무엇 |
|---|---|
| `attacca-domain/src/api_key.rs:57-58` | `#[serde(rename = "peers:write")]` + enum 변형 |
| `attacca-domain/src/api_key.rs:62,80` | **`pub const ALL: [ApiScope; 18]` — 18을 19로 올리고 항목을 더한다** |
| `attacca-domain/src/api_key.rs:102` | `as_db_str`의 match 갈래 |
| `attacca-zyris/src/dto.rs:162` | `ApiScope::PeersWrite => ZScope::PeersWrite` |

- **`from_db_str`은 안 고쳐도 된다** — `ALL`을 훑어 `as_db_str`과 대조하는 구현이라 위 셋만
  맞으면 따라온다(`api_key.rs:106-108`).
- **`ALL`이 고정 크기 배열이라 개수를 안 고치면 컴파일이 막힌다.** 잊어버릴 수 없게 되어 있다.
- `zyris_gateway.rs:589,624`가 기존 스코프를 쓰는 자리다. 새 메서드 셋도 같은 모양으로 막는다.

**`zyris_nodes`의 소유자 컬럼은 `owner_user_id`다**(migration `0093`). 다음 마이그레이션 번호는
**`0119`**다 (`0118_files_pinned.sql`이 마지막).

- [ ] **Step 2: 실패하는 테스트를 쓴다**

`crates/attacca-server/src/zyris_gateway.rs`가 있는 크레이트의 테스트에:

```rust
#[tokio::test]
async fn 스코프가_없으면_랑데부를_거절한다() {
    let gw = 게이트웨이_없는_스코프로();
    let r = gw.peer_list().await;
    assert!(matches!(r, Err(e) if e.code == ErrorCode::ForbiddenScope));
}

#[tokio::test]
async fn endpoint_id는_처음_올린_것이_계속_간다() {
    let gw = 게이트웨이_스코프_있음();
    gw.peer_publish("키1".into(), vec![]).await.unwrap();
    let r = gw.peer_publish("키2".into(), vec![]).await;
    assert!(r.is_err(), "서버가 조용히 키를 갈아 끼우면 안 된다");
    assert_eq!(gw.peer_lookup("나".into()).await.unwrap().endpoint_id, "키1");
}
```

- [ ] **Step 3: 빨간불 확인**

```bash
cd /home/ruma/attacca
timeout 900 cargo test -j2 -p attacca-server zyris_gateway 2>&1 | tail -20
```

- [ ] **Step 4: 구현**

`AttaccaApiGateway`에 세 메서드를 더한다. `register_node`/`list_nodes`와 달리 **device가 아니라
owner 범위**다 — 다른 물리 머신끼리 서로 보여야 하기 때문이다.

```rust
    async fn peer_publish(&self, endpoint_id: String, addrs: Vec<String>) -> ZResult<()> {
        self.require(ApiScope::PeersWrite)?;
        self.state
            .services
            .zyris_nodes
            .publish_peer_addr(self.actor.user_id, self.node_id, endpoint_id, addrs)
            .await
            .map_err(wire)
    }

    async fn peer_lookup(&self, node: String) -> ZResult<ZPeerAddr> {
        self.require(ApiScope::PeersWrite)?;
        let row = self
            .state
            .services
            .zyris_nodes
            .peer_by_slug_or_id(self.actor.user_id, &node)
            .await
            .map_err(wire)?;
        Ok(to_zpeeraddr(row, self.state.config.zyris_relay_url.clone()))
    }

    async fn peer_list(&self) -> ZResult<Vec<ZPeerEntry>> {
        self.require(ApiScope::PeersWrite)?;
        let rows = self
            .state
            .services
            .zyris_nodes
            .peers_for_owner(self.actor.user_id)
            .await
            .map_err(wire)?;
        Ok(rows.into_iter().map(to_zpeerentry).collect())
    }
```

`AttaccaApiGateway`에 `node_id: Uuid` 필드가 없으면 더한다 — `peer_publish`가 "누가 올리는가"를
알아야 한다. 생성자는 `routes/zyris.rs`가 부른다.

서비스 쪽 `publish_peer_addr`는 **`endpoint_id`가 이미 있고 다르면 오류**여야 한다:

```sql
UPDATE zyris_nodes
   SET endpoint_id   = COALESCE(endpoint_id, $3),
       peer_addrs    = $4,
       peer_addrs_at = now()
 WHERE id = $2 AND owner_user_id = $1
   AND (endpoint_id IS NULL OR endpoint_id = $3)
```

영향 행이 0이면 "키가 이미 다른 값으로 등록되어 있습니다"로 실패시킨다.

- [ ] **Step 5: sqlx 캐시를 다시 만든다**

```bash
export DATABASE_URL=…   # 살아 있는 Postgres
cargo sqlx prepare --workspace -- --all-targets
```

**안 하면 컴파일 타임 검증에서 막힌다.**

- [ ] **Step 6: 초록불 확인**

```bash
timeout 900 cargo test -j2 -p attacca-server -p attacca-domain 2>&1 | tail -20
timeout 1200 cargo build -j2 --workspace 2>&1 | tail -10
```

**`cargo fmt`을 돌리지 않는다.**

- [ ] **Step 7: 커밋과 PR**

```bash
git add -A
git commit -m "feat(zyris): 같은 계정 노드끼리의 P2P 랑데부를 연다"
git push -u origin feat/zyris-peer-rendezvous
gh pr create --repo attacca-cc/attacca \
  --title "feat(zyris): 같은 계정 노드끼리의 P2P 랑데부" \
  --body-file <(cat <<'EOF'
## 무엇을 위한 것인가

노드 A가 노드 B에게 파일을 직접 보낼 수 있게 하는 **주소록**이다. 파일 자체는 attacca를 지나지
않는다 — 이 PR이 여는 것은 "B의 iroh 주소가 무엇인가" 하나뿐이다.

## 무엇이 들었나

- 마이그레이션 하나 — `zyris_nodes`에 `endpoint_id`·`peer_addrs`·`peer_addrs_at`
- `ApiScope::PeersWrite` — 세 도구 전부 이것으로 막는다
- `peer_publish`·`peer_lookup`·`peer_list` — 기존 `list_nodes`와 달리 **owner 범위**다.
  device 범위로는 다른 물리 머신끼리 서로 안 보인다

## 서버가 키를 갈아 끼울 수 없게 했다

`endpoint_id`는 처음 올린 값이 계속 간다 — `COALESCE`와 `WHERE (endpoint_id IS NULL OR
endpoint_id = $3)`이 그 자리다. 노드 쪽은 상대의 키를 처음 본 것으로 고정하므로(TOFU),
서버가 조용히 바꾸면 다음 전송에서 드러난다. **여기서도 못 바꾸게 하는 것은 그 방어의 짝이다.**

## 배포 순서

**이 PR이 배포된 뒤에** 노드가 `peers:write`를 요청해야 한다. 배포 전에 요청하면
`/zyris/v1/device/authorize`가 422로 등록을 통째로 거절한다 — 2026-08-03에 `nodes:write`로
실제로 그렇게 걸렸다.

재는 법:

```bash
curl -s -o /dev/null -w '%{http_code}\n' -X POST \
  https://attacca.cc/api/zyris/v1/device/authorize -H 'content-type: application/json' \
  -d '{"name":"scope probe","platform":"linux","scopes":["peers:write"],"client_hint":{}}'
```

## 검증

- 스코프가 없으면 세 도구 전부 `forbidden_scope`
- `endpoint_id`를 다른 값으로 덮으려는 요청이 실패하고 첫 값이 남는 것
- 마이그레이션은 살아 있는 Postgres로 돌려 확인했다 (CI에는 postgres 서비스가 없다)
EOF
)
```

---

## Task 3.4: 릴레이 배포 (attacca 리포)

**Files:**
- Modify: `deploy/helm/attacca/templates/…`, `values.yaml`
- Modify: `.env.example`

> **2026-08-09 결정 — 자체 릴레이를 별도 배포한다.** 사용자가 정했다. n0 공개 릴레이도
> 암호문만 보지만(QUIC이 엔드포인트 사이에서 종단간 암호화된다) 메타데이터는 남는다.
> **attacca를 경유하는 폴백은 쓰지 않는다** — 그 경로는 TLS가 attacca에서 끊겨 파일 내용이
> 읽힌다. 릴레이보다 나쁘다.
>
> **우리 소스는 한 줄도 안 들어간다.** `iroh-relay`는 `server` feature에 기성 바이너리
> (`[[bin]] name = "iroh-relay"`)가 있고 TOML로 설정한다. 별도 리포도 필요 없다 —
> Dockerfile과 차트는 접근 확인 엔드포인트와 함께 움직이므로 attacca 리포에 둔다.

- [ ] **Step 1: 릴레이를 차트에 더한다**

`iroh-relay` Deployment + Service + Ingress 하나. `values.yaml`에:

```yaml
zyrisRelay:
  enabled: true
  image: ghcr.io/n0-computer/iroh-relay:1   # 없으면 cargo install 한 줄짜리 Dockerfile
  replicas: 1
  rateLimit:
    bytesPerSecond: 10485760
```

릴레이 설정 TOML은 **반드시 `access.http`를 쓴다.**

```toml
[access.http]
url = "https://<attacca>/internal/relay-access"
# bearer_token 은 IROH_RELAY_HTTP_BEARER_TOKEN 환경변수로 넣는다
```

> **`access`의 기본값은 `everyone`이다.** 그냥 띄우면 누구나 쓰는 열린 릴레이가 된다.
> 반드시 바꿔야 한다.

`access.http`는 새 연결마다 attacca에 POST를 보낸다. 헤더 `X-Iroh-Endpoint-Id`에 붙으려는
노드의 endpoint id가 실려 오고, **`200`에 본문이 `true`일 때만 통과**하며 나머지는 전부
거절이다. 다른 모드(`allowlist`·`denylist`·`shared_token`)는 노드가 드나들 때마다 설정을
고쳐야 해서 우리에게 안 맞는다.

- [ ] **Step 1b: attacca에 접근 확인 엔드포인트를 만든다**

`POST /internal/relay-access`. `X-Iroh-Endpoint-Id`를 읽어 `zyris_nodes`에 그 키로 등록된
노드가 있는지 보고 `true`/`false`를 준다. `Authorization: Bearer`로 릴레이 자신을 확인한다 —
**이 엔드포인트가 열려 있으면 남이 우리 노드 목록을 조회할 수 있다.**

TOFU 고정(Task 2.4)이 키가 바뀌는 것을 잡고, 이 엔드포인트는 **모르는 키가 대역폭을 쓰는 것**을
막는다. 둘은 서로를 대신하지 않는다.

- [ ] **Step 2: `.env.example`에 적는다**

```bash
# 노드끼리 직접 붙지 못했을 때 바이트를 중계하는 iroh 릴레이. peer_lookup 응답에 실려
# 나가므로 노드에 하드코딩되지 않는다. 비우면 릴레이 없이 직접 연결만 시도한다 —
# 대칭형 NAT 뒤의 노드는 그때 서로 못 붙는다.
ZYRIS_P2P_RELAY_URL=
```

**`.env.example`이 설정의 정본이다.** 여기 안 적으면 다음 사람이 이 변수의 존재를 모른다.

- [ ] **Step 3: 커밋**

```bash
git add deploy .env.example
git commit -m "build(deploy): iroh 릴레이를 배포에 더한다"
```

---

# 단계 4 — 배선과 라이브 검증

> **시작 조건: 단계 3이 프로덕션에 배포되어 있고, `peers:write` 스코프 프로브가 200을 준다.**

## Task 4.1: `file_transfer` 선언과 `send_to`

**Files:**
- Create: `crates/zyris-caps/src/file_transfer.rs`
- Create: `crates/zyris-capkit/src/transfer/send.rs`
- Modify: `crates/zyris-caps/src/lib.rs`, `crates/zyris-capkit/src/transfer/mod.rs`

**Interfaces:**
- Consumes: `zyris-p2p::{key, tofu, peer}`, `zyris_attacca::AttaccaApiClient`,
  `LocalPeerTransfer::offer_file`
- Produces: `file_transfer_capability()`, `FileTransfer`, `SendReceipt`, `InboxEntry`,
  `LocalFileTransfer`

- [ ] **Step 1: 선언을 쓴다**

`crates/zyris-caps/src/file_transfer.rs`:

```rust
//! 에이전트가 부르는 표면. attacca 링크에서 announce된다.
//!
//! 피어 링크의 `peer_transfer`와 다른 capability인 것이 요점이다 — "피어 링크는 파일 전송만
//! 연다"가 필터링 로직이 아니라 사실이 된다.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SendReceipt {
    pub node: String,
    /// 받는 쪽의 최종 경로. 아직 안 끝났으면 비어 있다.
    #[serde(default)]
    pub written: String,
    pub bytes: u64,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub replaced: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo: Option<String>,
    /// 직접 연결이었나, 릴레이를 지났나.
    #[serde(default)]
    pub direct: bool,
    /// 아직 끝나지 않았다. 오류가 아니다 — 같은 인자로 다시 부르면 이어받는다.
    #[serde(default)]
    pub pending: bool,
    /// 지금 무엇을 해야 하는지 한 줄. 없으면 끝난 것이다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InboxEntry {
    pub from: String,
    pub name: String,
    pub bytes: u64,
    pub path: String,
    pub received_unix_ms: u64,
}

#[zyris::capability(name = "file_transfer", version = 1)]
pub trait FileTransfer {
    /// 이 머신의 파일을 같은 계정의 다른 노드로 보낸다.
    ///
    /// 60초 안에 못 끝내면 **오류가 아니라** `pending: true`로 답하고 `next`가 다시 부르라고
    /// 말한다. attacca가 노드 호출을 60초에 `Timeout` 오류로 끊는데, 그 오류를 받은 에이전트는
    /// 같은 도구를 어차피 다시 부른다 — 실패의 모양을 만들지 않는 쪽이 낫다.
    async fn send_to(
        &self,
        node: String,
        path: String,
        name: Option<String>,
        overwrite: Option<bool>,
    ) -> zyris::Result<SendReceipt>;

    /// 이 머신의 inbox에 무엇이 들어와 있는지.
    async fn inbox_list(&self) -> zyris::Result<Vec<InboxEntry>>;
}
```

- [ ] **Step 2: descriptor 테스트**

```rust
#[cfg(test)]
mod tests {
    use zyris::proto::Transfer;

    #[test]
    fn 도구는_둘이고_둘_다_unary다() {
        let d = super::file_transfer_capability();
        let mut 이름들: Vec<_> = d.tools.iter().map(|t| t.name.as_str()).collect();
        이름들.sort();
        assert_eq!(이름들, ["inbox_list", "send_to"]);
        assert!(d.tools.iter().all(|t| t.transfer == Transfer::Unary));
    }
}
```

- [ ] **Step 3: 구현 — `LocalFileTransfer::send_to`**

`crates/zyris-capkit/src/transfer/send.rs`. 흐름은 정확히 이 순서다:

1. `path`를 노드 root 아래로 해석한다. 밖이면 거부.
2. 파일을 읽으며 sha256과 크기를 잰다.
3. `transfer_id = sha256(내 node_id ‖ name ‖ size ‖ sha256)`의 앞 16바이트를 16진으로.
4. `attacca.peer_lookup(node)` → `ZPeerAddr`.
5. `tofu.check(node_id, endpoint_id)` — 어긋나면 `peer_key_changed`(retriable=false)로 중단.
6. `peer::dial` → `Node::connect_over` — 이 노드는 **`peer_transfer`만** announce한다.
7. `offer_file(transfer_id, path, size, sha256)`로 예약.
8. `push_offer`를 부른다. **여기서 상대가 `pull`로 되당긴다.**
9. 성공하면 `tofu.pin(node_id, endpoint_id)`.
10. `SendReceipt`를 만든다. `direct`는 iroh 연결 타입에서 읽는다.

**전체를 `wire_deadline`(기본 55초) `timeout`으로 감싼다.** 시간이 다 되면 오류가 아니라
`pending: true`와 `next: Some("같은 인자로 다시 부르세요")`로 답한다.

- [ ] **Step 4: 커밋**

```bash
git add crates/zyris-caps crates/zyris-capkit
git commit -m "feat(transfer): 에이전트가 부르는 send_to를 붙인다"
```

---

## Task 4.2: 수락 루프

**Files:**
- Create: `crates/zyris-capkit/src/transfer/listen.rs`

**Interfaces:**
- Produces: `pub async fn serve_peers(endpoint: iroh::Endpoint, api: AttaccaApiClient, config: TransferConfig, tofu: TofuStore) -> !`

> **`TransferConfig`의 inbox·undo 루트는 이 플랜이 정하지 않는다.** `serve_peers`는 받아서
> 연결마다 clone할 뿐이고, 실제로 만드는 것은 이 크레이트를 품는 노드다(zyris-hello ·
> zyris-daemon · zyris-code). 그래서 **"같은 루트를 두 프로세스가 쓰는 일은 없다"를 전제로
> 삼을 수 없다** — 전제를 지킬 사람이 이 리포 밖에 있다. 안전은 `undo.rs`가 구조로
> 보장해야 한다(백업 자리를 `create_dir`로 잡아 파일시스템에 심판을 맡긴다).
> Task 1.5 리뷰가 이 결론을 냈고 fix round 1에서 반영했다.

- [ ] **Step 1: 구현**

```rust
//! 들어오는 피어 연결을 받는다. **아무나 받지 않는다.**
//!
//! 들어온 EndpointId가 내 계정의 노드 목록에 있어야 한다. 목록은 60초 캐시하고, 없으면 한 번
//! 갱신해 본다 — 다만 **모르는 피어가 재조회를 무한히 시키면 그 자체가 증폭 공격이므로**
//! 갱신에 최소 간격(10초)을 둔다.

pub async fn serve_peers(
    endpoint: iroh::Endpoint,
    api: AttaccaApiClient,
    config: TransferConfig,
    tofu: TofuStore,
) {
    // PeerCache는 여러 연결 태스크가 함께 쓴다 — Arc<Mutex<..>>로 감싸 clone해서 넘긴다.
    let 목록 = Arc::new(Mutex::new(PeerCache::new(
        api,
        Duration::from_secs(60),
        Duration::from_secs(10),
    )));
    loop {
        // Task 2.5 리뷰가 잡은 것: 여기서 상대를 기다리면 **붙고 침묵하는 상대 하나가
        // 리스너 전체를 막는다.** `accept_next`는 상대를 기다리지 않고 바로 돌아오고,
        // 핸드셰이크(`establish`)는 마감을 들고 연결마다 spawn된 태스크에서 한다.
        let Some(accepting) = zyris_p2p::peer::accept_next(&endpoint).await else { break };

        let config = config.clone();
        let 목록 = 목록.clone();
        // 경로를 꺼내 새로 만들지 않는다. clone은 쓰기 잠금을 공유하므로, 동시에 두 연결이
        // 고정해도 서로의 고정을 덮지 않는다 (Task 2.4).
        let tofu = tofu.clone();
        tokio::spawn(async move {
            let (상대, transport) = match zyris_p2p::peer::establish(
                accepting,
                Duration::from_secs(10),
            )
            .await
            {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "핸드셰이크가 끝나지 않아 닫습니다");
                    return;
                }
            };

            let Some(항목) = 목록.lock().await.find(&상대.to_string()).await else {
                tracing::warn!(peer = %상대, "내 계정 노드가 아니라 닫습니다");
                return;   // transport를 drop하면 연결이 닫힌다
            };
            let node_id = 항목.node_id.clone();
            let slug = 항목.slug.clone();
            // **고정의 열쇠는 slug다. node_id가 아니다.** node_id는 attacca가 발급한다 —
            // 고정으로 묶으려는 바로 그 주체다. node_id로 고정하면 attacca가 가짜 B를
            // **새 노드**(새 node_id, 같은 slug)로 소개하는 것만으로 "처음 보는 상대"가 되어
            // 통과하고, 그대로 고정된다. 두 번이고 세 번이고 통하며 흔적도 안 남는다.
            // 최종 브랜치 리뷰가 잡았다. slug는 사용자가 정한 이름이라 서버가 조용히
            // 다시 발급할 수 없다.
            if let Err(e) = tofu.check(&slug, &상대.to_string()).await {
                tracing::error!(peer = %상대, slug = %slug, error = %e, "키가 바뀌어 거절합니다");
                return;
            }
            // 받는 쪽도 peer_transfer 하나만 내준다.
            let 받는_것 = LocalPeerTransfer::receiver_pending(config, slug);
            let node = Node::builder()
                .name("peer")
                .kind(NodeKind::Cli)
                .capability(PeerTransferServer(받는_것.clone()))
                .build()
                .unwrap();
            // node_id를 기본값(무작위 UUID)으로 두면 상대가 우리 신원을 잘못 안다.
            let opts = AcceptOptions { node_id: 내_node_id.clone(), ..AcceptOptions::default() };
            let Ok(conn) = node.accept(transport, opts).await else { return };
            // 상대에게 pull을 부를 손잡이를 꽂는다.
            if let Ok(client) = conn.wait_capability(Duration::from_secs(5)).await {
                받는_것.set_peer(client);
            }
            if let Err(e) = tofu.pin(&slug, &상대.to_string()).await {
                // 고정을 못 남기면 다음 연결이 이 상대를 "처음 보는 상대"로 통과시킨다.
                // 연결은 이미 성립했으니 끊지는 않되, 조용히 넘기지도 않는다.
                tracing::error!(peer = %상대, error = %e, "키를 고정하지 못했습니다");
            }
            conn.closed().await;
        });
    }
}
```

- [ ] **Step 2: 테스트**

`PeerCache`의 판정만 순수하게 떼어 테스트한다 — 목록에 있나, 갱신 간격을 지키나.

- [ ] **Step 3: 커밋**

```bash
git add crates/zyris-capkit
git commit -m "feat(transfer): 내 계정 노드의 연결만 받는다"
```

---

## Task 4.3: 라이브 검증

**Files:**
- Create: `crates/zyris-capkit/examples/transfer_probe.rs`

- [ ] **Step 1: 프로브를 쓴다**

두 머신에서 각각 노드를 띄우고, 한쪽에서 `send_to`를 불러 다른 쪽에 파일이 생기는지 본다.

**판정은 부작용과 해시로만 한다.** 에이전트의 말은 근거가 아니다 — zyris-code에서 실제로 네 번
연속 "도구가 없다"는 답을 받았는데 도구는 내내 있었다.

```rust
//! 두 머신 사이의 실제 전송을 잰다. 로컬이 전부 초록이어도 NAT 통과는 여기서만 드러난다.
//!
//! 판정:
//!   1. 받는 머신의 inbox에 파일이 실제로 생겼는가
//!   2. 그 파일의 sha256이 보낸 것과 같은가
//!   3. `SendReceipt.direct`가 참인가 (거짓이면 릴레이를 지난 것 — 실패는 아니지만 기록한다)
```

- [ ] **Step 2: 두 머신에서 돌린다**

```bash
# 머신 B
cargo run -p zyris-capkit --features transfer --example transfer_probe -- listen

# 머신 A
cargo run -p zyris-capkit --features transfer --example transfer_probe -- send <B의 slug> ./큰파일.bin
```

- [ ] **Step 3: `direct` 비율을 기록한다**

여러 네트워크에서 열 번씩 돌려 직접 연결 비율을 적는다. **스펙 §12의 미해결 1번이 그것이다.**
비율이 낮으면 릴레이 용량 계획이 달라진다.

- [ ] **Step 4: 결과를 스펙에 적고 커밋**

```bash
git add crates/zyris-capkit/examples docs/superpowers/specs
git commit -m "test(transfer): 두 머신 사이 라이브 검증 프로브"
```

---

## Self-Review 결과

계획을 쓰고 나서 스펙과 대조한 결과다.

**스펙 커버리지** — 채운 것: §3 크레이트 배치(단계 1·2), §4.1 키페어(2.3), §4.2 TOFU(2.4),
§4.3 authorization(4.2), §5.1 프레이밍(2.2), §5.2 릴레이(3.4), §6.1 `file_transfer`(4.1),
§6.2 `peer_transfer`(1.4·1.5), §6.3 무결성(1.5), §6.4 상한(1.5), §7.1 감옥(1.2), §7.2
덮어쓰기(1.3), §7.3 감사(1.7), §8 attacca(3.1~3.4), §9 오류(1.5·4.1), §10 테스트(전반).

**빠진 것 둘, 여기 적어 둔다:**

1. **되돌림 보관 정리(30일/4GiB)** — 스펙 §7.2에 있으나 Task가 없다. Task 1.3의 `UndoStore`에
   `pub async fn sweep(&self, keep_days: u64, keep_bytes: u64)`를 더하고, `stash` 성공 뒤에
   부르는 것으로 처리한다. 테스트는 오래된 디렉터리를 손으로 만들어 지워지는지 본다.
2. **`max_inbox_bytes` 강제** — `TransferConfig`에 필드는 있으나 Task 1.5가 안 쓴다.
   `push_offer` 초입에서 inbox 총량을 세어 `offer.size`를 더한 값이 상한을 넘으면
   `payload_too_large`로 거절한다. 총량 세기는 디렉터리를 걷는 것이라 **결과를 캐시하고
   전송마다 증분으로 갱신한다** — 매 전송마다 32GiB 트리를 걷으면 그것이 곧 병목이다.

**타입 일관성** — 해소됨 (2026-08-09). `receiver_placeholder`(옛 Task 1.5)와
`receiver_pending`(Task 4.2)이 같은 것을 다르게 부르고 있었다. `receiver_pending(config,
peer_slug)` + `set_peer(client)` 하나로 통일하고 Task 1.5 본문에 반영했다.

---

## 실행 중 고친 것 (2026-08-09, 단계 1 진행하며)

계획대로 안 간 자리들이다. 같은 함정을 다시 밟지 않도록 남긴다.

1. **`b"한글"`은 컴파일되지 않는다.** byte string 리터럴은 ASCII만 받는다. Task 1.3 구현자가
   잡았고 12군데를 `"한글".as_bytes()`로 고쳤다.
2. **그 고침을 자동화한 정규식이 멀쩡한 코드를 넷 망가뜨렸다** — `b"hello p2p"`,
   `.name("b")` 둘, `.name("a")`. `b"([^"]*)"`가 `.name("b")`의 `b"` 뒤로 다음 따옴표까지
   삼켰기 때문이다. **줄 단위로 안 보고 문서 전체에 regex를 돌린 대가다.**
3. **`safe_name`의 기대값**이 Task 1.1 표와 Task 1.2 단언 두 곳에 있었는데 1.1만 고쳤다.
   basename을 취하므로 `../../etc/passwd` → `passwd`다.
4. **inbox의 검사 순서** — coarse escape 검사를 심링크 조각 걷기보다 먼저 하면 오류 코드가
   뒤바뀐다. Task 1.2 구현자가 잡아 순서를 뒤집었다.
