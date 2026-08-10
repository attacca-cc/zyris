# 노드 간 P2P 파일 전송 설계 (2026-08-09)

**목표:** 서로 다른 사용자 머신에 있는 두 Zyris 노드가 **파일을 직접 주고받는다.** attacca는 둘을
소개하기만 하고 바이트는 지나가지 않는다.

**범위:** 에이전트가 도구로 호출하는 전송. 사람이 수락 버튼을 누르는 흐름은 이번 범위가 아니다.

---

## 1. 지금 무엇이 있고 무엇이 없는가

설계의 출발점이 되는 조사 결과다. **문서가 있다고 말하지만 코드에 없는 것들이 있어**, 그 위에
설계를 세우면 구현 중에 무너진다.

### 없다

| | 근거 |
|---|---|
| 노드 A → 노드 B 경로 | 와이어 타입에 peer id·라우팅 개념 자체가 없다 |
| P2P 전송로 (WebRTC·QUIC·홀펀칭·STUN/TURN·mDNS) | `METHOD_WEBRTC_SIGNAL`/`METHOD_WEBRTC_CLOSE` 상수 두 개가 전부다 (`zyris-proto/src/envelope.rs:26-27`). 어디서도 참조되지 않는다 |
| TLS 밖의 암호 | ed25519·x25519·aead·chacha·hkdf·noise 전부 0건. 노드 키페어도 서명도 없다 |
| `file_io.write_at` | 존재한 적이 없다. `docs/zyris-protocol.md:313`이 있다고 말할 뿐이다 |
| 요청 params의 첨부 | 미구현. `attach::detach`는 `reply_res`에서만 불린다 (`connection.rs:207`) |
| 인바운드 `note` 처리 | `zyris.closing` 외 **전부 조용히 버려진다** (`connection.rs:1021-1028`, else 가지 없음). 등록 가능한 핸들러도 없다 |
| `prog` 프레임 | 디코드 후 버려진다 (`connection.rs:1000`). 진행률 보고 불가 |
| 경로 감옥 | `resolve_under`의 주석이 *"the root is a default, not a jail"* (`zyris-capkit/src/path.rs:7`). 절대경로 통과. `canonicalize`·`symlink_metadata`·`read_link`·`O_NOFOLLOW` 호출 0건 |
| `s_end.trailer.sha256` 검증 | 보내는 쪽은 쓰지만 (`connection.rs:247`) **받는 쪽이 버린다** — `Envelope::SEnd { stream, .. }` |
| 크레딧 강제 | `CreditGate`는 보내는 쪽만 묶는다. `ErrorCode::CreditViolation`·close `4409`는 정의만 되고 미사용 |
| 첨부 이어받기 | `AttachmentRef.offset`은 있으나 `offset: 0` 하드코딩 (`zyris-proto/src/attach.rs:76`) |

### 있다 — 그리고 이게 설계를 가능하게 한다

**`Transport` 트레잇이 얇다** (`zyris/src/transport.rs:18-20`):

```rust
pub trait Transport: Send + 'static {
    fn split(self: Box<Self>) -> (Box<dyn WireSink>, Box<dyn WireStream>);
}
```

주고받는 것은 `WireMessage::{Binary(Bytes), Text(String)}` 뿐이다. **메시지 단위 바이트 전송로면
무엇이든 zyris를 실어 나른다.** 이미 넷이 붙어 있다 — 인메모리 채널, tungstenite 클라이언트,
axum 서버, 테스트용 `GatedTransport`.

**클라이언트/서버 비대칭은 `Role` 하나뿐이다.** `Node::connect_over(transport)` → `Role::Dial`,
`Node::accept(transport, opts)` → `Role::Accept`. 스트림 id 패리티도 거기서만 갈린다
(`connection.rs:762`). 어느 쪽이 다이얼했는지만 정해지면 되므로 **P2P 링크에 그대로 맞는다.**

> **결론: zyris 코어를 한 줄도 고치지 않고 P2P 링크 위에 zyris를 얹을 수 있다.**

**이것은 추론이 아니라 실측이다.** 조사 중에 리포 밖 크레이트에서 raw TCP `Transport` 어댑터를
**47줄**로 만들어(프레이밍 코덱 포함) 확인했다 — 웹소켓·TLS·서버·베어러 토큰 없이 노드 둘이
전 과정을 주고받았다: 핸드셰이크와 feature 협상, 양방향 capability announce와 dispatch,
4,204,303바이트 `uni_stream`(256 KiB 크레딧 창과 `chunk_seq` 검증을 지나감), 그리고 자동 detach →
크레딧 페이싱 → sha256 검증 → 재조립까지. **첫 실행에 통했고 `crates/zyris`는 수정하지 않았다.**
`cargo check -p zyris --no-default-features`도 통과한다 — 엔진 전체가 웹소켓·TLS·HTTP 의존 없이
컴파일된다. 프로브 사본: `~/zyris-p2p-probe-20260809/`.

그 실측이 같이 잡아낸 제약 셋 — **설계가 지켜야 한다**:

1. **하나의 양방향 스트림이어야 한다.** 모든 프레임이 `WriterCmd` 채널 하나를 지나 sink를 혼자
   소유한 writer 태스크로 간다(`connection.rs:754, 787, 828-850`). `s_credit`·`s_end`가 자기가
   가리키는 STREAM_DATA를 추월하면 안 되므로 **제어와 데이터를 QUIC 스트림 둘로 나누면 깨진다.**
2. **신뢰성과 순서가 필요하다.** `handle_frame`이 `seq != next_seq`면 `StreamLagged`로 스트림을
   즉사시킨다(`connection.rs:962-971`). 한 조각만 순서가 뒤바뀌어도 전송이 죽는다.
3. **`AcceptOptions.node_id`를 진짜 신원으로 채워야 한다.** 기본값은 무작위 UUID라, 안 채우면
   양쪽이 서로의 node_id를 잘못 안다.

그리고 **`runtime::Runner`는 P2P에 못 쓴다** — `runner.rs:330`이 `node.connect(url, bearer)`로
웹소켓 다이얼러에 하드코딩되어 있다. P2P 쪽은 자기 수락 루프를 직접 돌린다.

**테스트 하네스가 인메모리다.** `zyris::testing::duplex(dialer, acceptor)`가 진짜 `Connection` 둘을
채널로 이어 준다. 새 기능의 프로토콜 동작은 소켓 없이 전부 테스트할 수 있다.

**attacca는 이미 노드를 부를 수 있다.** `NodeRouter::call` (`attacca-zyris/src/route.rs:136`),
unary 전용, 기본 60초, 결과 8MiB 상한.

---

## 2. 결정 사항

| 항목 | 결정 |
|---|---|
| 누가 시작하나 | **에이전트가 도구로 호출한다.** 받는 쪽에 사람 확인은 없다 |
| 신뢰 경계 | **TOFU 키 고정 + 사람의 지문 확인.** attacca는 랑데부만 하고 신뢰 앵커가 아니다 — 처음 보는 상대는 사람이 128비트 지문을 out-of-band로 맞춰 본 뒤에만 고정되고, 이후 바뀌면 (다시 묻지 않고) 거부한다 |
| 전송 계층 | **iroh** (1.0, 2026-06). QUIC + 홀펀칭 + 릴레이 폴백, EndpointId = ed25519 공개키 |
| 릴레이 | **attacca가 자체 호스팅한다.** n0 공개 릴레이는 메타데이터가 제3자에 샌다 |
| 피어 링크의 capability | **`peer_transfer` 하나만.** 노드의 다른 도구는 피어에게 열리지 않는다 |
| 바이트 방향 | **받는 쪽이 당긴다(pull).** 미는 경로는 와이어에 없다 |
| 목적지 | **고정 inbox 감옥.** 보내는 쪽은 파일 이름만 제안한다 |
| 덮어쓰기 | **`overwrite` 인자, 기본 `false`.** 덮기 전 원본을 undo 자리로 옮긴다 |

---

## 3. 아키텍처 — 무엇이 어디에 사는가

```
zyris-p2p        [새 크레이트]  전송로
  · 노드 키페어 (ed25519, 로컬 생성·저장)
  · iroh Endpoint 수립·수락 루프
  · AttaccaLookup: iroh AddressLookup 구현 — attacca를 주소록으로 쓴다
  · TofuStore: 상대 EndpointId 고정과 검증
  · IrohTransport: QUIC bi-stream ↔ zyris Transport 어댑터
  capkit이 아니다 — capkit은 "capability의 참조 구현"이고 이것은 전송로다.
  zyris 본체에 안 넣는 이유는 iroh 의존 트리가 커서다. capkit이 OS 의존을
  격리하는 것과 같은 이유로 격리한다.

zyris-caps       선언만 (기존 규칙 그대로, OS를 안 만진다)
  · file_transfer  v1 — 에이전트가 부르는 도구
  · peer_transfer  v1 — A와 B 사이의 와이어

zyris-capkit     구현 (파일시스템을 만진다 — 정확히 capkit의 자리)
  · LocalFileTransfer  — send_to·inbox_list
  · LocalPeerTransfer  — push_offer·pull, inbox 감옥, sha256, undo

zyris-attacca    attacca_api에 랑데부 도구 추가 (선언)
attacca          그 구현 + iroh-relay 운영 + DB 마이그레이션
```

**capability가 둘인 이유**: 같은 이름 하나를 링크마다 필터링하는 것보다 두 개로 나누는 쪽이 안전하다.
"피어 링크는 `peer_transfer`만 announce한다"가 필터링 로직이 아니라 **사실**이 된다. zyris-code가
`tools/readonly.rs`로 descriptor를 걸러 내는 방식은 dispatch도 같이 막아야 해서 자리가 둘이다 —
한쪽만 고치면 조용히 샌다.

---

## 4. 신원과 신뢰

### 4.1 노드 키페어

노드는 첫 실행에 ed25519 키페어를 **로컬에서** 만든다. iroh의 `SecretKey`이고, 그 `PublicKey`가
곧 `EndpointId`다. **개인키는 머신 밖으로 나가지 않는다.**

- 자리: 자격 파일 옆, `0600`. 그룹·전체 읽기 가능하면 거부한다 (`FileCredentialStore`와 같은 규칙).
- 등록: enrollment 직후 `attacca_api.peer_publish`로 **공개키만** 올린다.
- 회전: 이번 범위 밖. 키가 바뀌면 상대의 TOFU가 물어 전송이 막힌다 — 그것이 의도된 동작이다.

### 4.2 TOFU 고정 — 조용히 고정하지 않는다

**이전 판은 여기서 "attacca가 준 B의 EndpointId를 그대로 쓴다 → 성공하면 고정"이라고 적었고,
그건 틀렸다.** attacca는 랑데부만 하는 게 아니라 `node_id`·`node_name` 둘 다 자기가 발급한다
(2026-08-10 실측, `crates/zyris-p2p/src/tofu.rs` 모듈 문서). 그 값을 키로 삼아 첫 성공에 조용히
고정하면, attacca가 "가짜 B"를 소개해도 그 가짜가 처음 보는 슬롯에 처음 성공한 연결로 들어와
똑같이 고정된다 — 막으려던 바로 그 대체를 막지 못한다. attacca가 적대적일 수 있다는 게 애초
위협 모델이라, attacca의 DB를 믿는 앵커는 원을 그리며 도는 것과 같다.

그래서 앵커는 사람이다. SSH 호스트 키·Signal 안전 번호와 같은 방식 — 처음 보는 상대는 **사람이
128비트 지문을 out-of-band로 맞춰 본 뒤에만** 고정된다:

```
A가 B에게 처음 연다     → B가 모르는 상대 → A가 지문을 사람에게 보여주고 확인받는다
                          → 거절하면 고정하지 않고 중단
                          → 수락하면 B의 EndpointId를 고정
A가 B에게 다시 연다      → 고정해 둔 것과 같으면 통과 (다시 묻지 않는다)
                          → 다르면 → err peer_key_changed, 확인도 없이 즉시 중단
```

- **키는 `peer_slug` — 사용자가 고른 이름이고, attacca가 재발급할 수 있는 어떤 문자열도 아니다.**
  `node_id`도 `node_name`도 안 된다. 이름은 이제 사람이 붙이는 라벨일 뿐이고, 신원의 앵커는 키 그
  자체다(`crates/zyris-p2p/src/tofu.rs`, `fingerprint.rs`).
- **지문은 SHA-256(EndpointId)의 앞 128비트**, `9F2A 41C7 0E83 BB15 6D04 A97E 22C1 5FB8`처럼
  4자씩 8묶음으로 보여준다. 짧게 자르면 공격자가 키페어를 갈아서 맞는 지문을 찾아낼 수 있는
  범위(제2 원상 저항)로 들어오므로 줄이지 않는다. **고정되는 값은 지문이 아니라 EndpointId
  전체다** — 지문은 사람이 눈으로 비교하라고 만드는 표현일 뿐 장부에는 안 들어간다.
- **알고 있는 상대가 키를 바꿔 왔으면 사람에게 묻지 않고 그 자리에서 거절한다.** 확인 절차는
  "처음 보는 상대"에만 붙는다 — 이미 고정된 슬롯에 다른 키가 오는 것은 판단할 일이 아니라
  막을 일이고, 물어보면 공격자에게 조용한 재시도 기회를 하나 더 주는 셈이다.
- **양쪽 다 확인한다.** A가 B에 걸 때 A는 B의 지문을 보고, B가 A의 연결을 받을 때 B는 A의
  지문을 본다. 한쪽만 확인하면 확인하지 않은 쪽은 아무나 받는다.
- **확인을 기다리는 동안 파일 잠금을 쥐지 않는다.** `peers.json`의 잠금 파일(`<ledger>.lock`)은
  60초 넘게 방치되면 죽은 프로세스의 것으로 간주해 깨지도록 되어 있고(`LOCK_STALE_THRESHOLD`),
  사람이 지문을 맞춰 보는 데는 그보다 오래 걸린다. 그래서 확인 전에는 잠금 없이 조회만 하고,
  수락한 뒤에야 잠금을 잡고 **다시 확인**한다 — 사람이 고민하는 사이 다른 연결이 다른 키로
  먼저 고정했다면 그때 가서 실패로 드러난다.
- **사람이 없는 노드(예: zyris-daemon 헤드리스 실행)는 `DenyUnknown`을 쓴다** — 처음 보는 상대를
  항상 거절한다. 닫히는 쪽으로 고장난다. 사람 없이도 미리 승인해 두고 싶은 노드는 그 정책 —
  예를 들어 설정 파일에 지문을 미리 적어 두고 그것과 대조하는 `PeerConfirmer`를 직접 구현한다.
  `zyris-p2p`는 훅만 내고 그 정책은 정하지 않는다.
- 자리: `~/.local/share/zyris/peers.json` (0600). `{"peers": {"<peer_slug>": {"endpoint_id":
  "…", "first_seen_ms": …}}}`.
- 바뀌었을 때 자동으로 푸는 길은 두지 않는다. 사람이 파일에서 지워야 한다 — 조용히 받아들이면
  고정하는 의미가 없다.

**이 설계가 막는 것**: 네트워크 도청, 릴레이의 내용 열람, 수동 로깅, 전송 중 변조, 그리고
attacca(또는 attacca를 흉내 낸 무언가)가 첫 연결에서 가짜 상대를 소개하는 것 — 사람이 지문을
실제로 맞춰 본다면 그 자리에서 드러난다.
**남는 것**: 사람이 지문을 형식적으로만 수락하는 경우(러버스탬핑)와, `DenyUnknown` 대신 미리
승인 정책을 쓰는 헤드리스 노드가 그 승인 목록 자체를 안전하지 않은 경로로 채운 경우. 둘 다
이 모듈이 아니라 그것을 쓰는 쪽의 책임이다.

### 4.3 authorization — B는 아무나 받지 않는다

**zyris 핸드셰이크는 신원을 하나도 증명하지 않는다.** `Node::connect_over`/`accept`는 자격을
인자로 받지 않고(`node.rs:80-106`), `Hello`가 나르는 것은 protocol·serialization·agent·features·
resume뿐이다. 스택 전체에서 자격이 와이어에 닿는 자리는 웹소켓 다이얼러의
`Authorization` 헤더 한 줄(`transport.rs:85-88`)이 유일하다. **그러므로 P2P 링크에서는 인증이
전적으로 전송로의 몫이다** — 아무 `Transport`나 꽂으면 아무하고나 핸드셰이크가 끝난다.

iroh가 그 자리를 채운다. QUIC 연결이 성립한 시점에 상대는 자기 EndpointId에 대응하는 개인키를
가졌음을 이미 증명했다. 우리가 할 일은 **그 증명된 신원이 받아도 되는 상대인지** 보는 것이다.

들어온 연결의 EndpointId가 **내 계정의 노드 목록에 있어야** 한다.

```
B: attacca_api.peer_list() → [{ node_id, slug, endpoint_id }, …]  (캐시, TTL 60초)
B: 들어온 EndpointId가 목록에 없으면 → 연결을 즉시 닫는다
```

캐시에 없으면 한 번 갱신해 보고, 그래도 없으면 닫는다. **모르는 피어에게 재조회를 무한히
시키면 그 자체가 증폭 공격이 되므로** 갱신은 최소 간격(10초)을 둔다.

---

## 5. 연결이 성립하는 방식

```
B (상시 수신)                                A (보내는 쪽)
 │ iroh Endpoint 가동, ALPN "zyris/1"          │ 에이전트: file_transfer.send_to(node=B, …)
 │ 주소가 바뀌면 peer_publish                  │
 │                                             ├─ attacca_api.peer_lookup(B)
 │                                             │   ← { endpoint_id, addrs, relay_url }
 │                                             │   TOFU 대조 — 다르면 여기서 중단
 │                     ◀── QUIC 홀펀칭 ────────┤ iroh가 릴레이로 조율 후 직접 연결
 ├─ EndpointId가 내 계정 노드인가? 아니면 close│
 │                                             │
 │              ◀══ zyris Connection ═════════▶│  IrohTransport 어댑터
 │  Node::accept(…)                            │  Node::connect_over(…)
 │                                             │
 └─ 이 링크에서 announce: peer_transfer 하나   └─ 이 링크에서 announce: peer_transfer 하나
```

### 5.1 IrohTransport

iroh는 QUIC **바이트 스트림**을 주고 zyris는 **메시지**를 원한다. 어댑터가 하나의 bi-stream 위에
길이 접두 프레이밍을 얹는다.

```
[u8 kind][u32 BE len][payload …]
  kind 0 = Binary, 1 = Text
```

- **`len`에 상한을 건다** (기본 16 MiB). 크레딧이 권고사항이라 이 상한이 실질적인 메모리 방어선이다.
  넘으면 연결을 닫는다.
- bi-stream 하나만 쓴다. zyris가 이미 자기 층에서 다중화하고 크레딧으로 head-of-line blocking을
  다루므로 QUIC 스트림을 더 열 이유가 없다. 나중에 필요해지면 스트림당 zyris 스트림으로 넓힐 수
  있는 자리로 남겨 둔다.
- `close(code, reason)`은 QUIC의 애플리케이션 close로 옮긴다.

### 5.2 릴레이

`RelayMode::Custom(RelayMap)`으로 attacca가 띄운 릴레이만 쓴다. **`RelayMode::Default`(n0)를 쓰지
않는다** — 누가 누구와 언제 통신하는지가 제3자에게 남는다.

릴레이 URL은 하드코딩하지 않고 `peer_lookup` 응답에 실려 온다. 배포가 릴레이를 옮겨도 노드를 다시
배포할 필요가 없다.

**홀펀칭이 실패하면 iroh가 자동으로 릴레이로 바이트를 흘린다.** 암호문이라 릴레이는 내용을 못
보지만 **그때는 트래픽이 우리 서버를 지나간다.** 실무에서 대략 10~20%다. 서버 부하를 없애는 것이
아니라 크게 줄이는 것이다.

---

## 6. 파일 전송 프로토콜

### 6.1 `file_transfer` v1 — 에이전트가 부르는 것 (attacca 링크)

```rust
#[zyris::capability(name = "file_transfer", version = 1)]
pub trait FileTransfer {
    /// 이 머신의 파일을 같은 계정의 다른 노드로 보낸다.
    async fn send_to(
        &self,
        node: String,            // 대상 노드의 slug 또는 node_id
        path: String,            // 이 머신에서 읽을 경로
        name: Option<String>,    // 받는 쪽에 제안할 이름. 없으면 파일 이름
        overwrite: Option<bool>, // 기본 false
    ) -> zyris::Result<SendReceipt>;

    /// 이 머신의 inbox에 무엇이 들어와 있는지.
    async fn inbox_list(&self) -> zyris::Result<Vec<InboxEntry>>;
}

pub struct SendReceipt {
    pub node: String,
    pub written: String,   // 받는 쪽의 최종 경로
    pub bytes: u64,
    pub sha256: String,
    pub replaced: bool,
    pub undo: Option<String>,  // 덮었으면 원본이 있는 자리
    pub direct: bool,          // 직접 연결이었나, 릴레이를 지났나
}
```

**`path`를 무엇이 막는가**: 보내는 쪽의 읽기다. capkit은 `LocalFileTransfer`를 만들 때 받은 root
아래로만 읽고, 그 밖은 거부한다 — `resolve_under`의 "감옥이 아니다"를 여기서는 감옥으로 쓴다.
zyris-code처럼 게이트를 가진 노드는 그 위에 자기 판정을 한 겹 더 얹는다.

`send_to`는 **unary**다. attacca의 노드 호출 타임아웃이 기본 60초라(`route.rs:15`) 큰 파일은
그 안에 못 끝난다. 그래서:

- 60초 안에 끝나면 완료된 `SendReceipt`를 준다.
- 안 끝나면 **오류가 아니라 성공**으로 답하고 `pending: true`와 "같은 인자로 다시 부르세요"를
  담은 `next`를 준다. zyris-code의 `wait.until`이 쓰는 계약과 같다 — 실패의 모양을 만들지 않는
  것이 요점이다. 재호출은 이미 받은 만큼부터 이어받는다.

### 6.2 `peer_transfer` v1 — A와 B 사이 (피어 링크 전용)

```rust
#[zyris::capability(name = "peer_transfer", version = 1)]
pub trait PeerTransfer {
    /// 보내는 쪽이 알린다. 받는 쪽은 pull로 되당긴 뒤 결과를 여기 답으로 준다.
    async fn push_offer(&self, offer: TransferOffer) -> zyris::Result<TransferDone>;

    /// 받는 쪽이 보내는 쪽에게서 바이트를 당긴다. 여기서는 보내는 쪽이 callee다.
    #[zyris(uni_stream)]
    async fn pull(
        &self,
        transfer_id: String,
        offset: u64,
    ) -> zyris::Result<Streaming<PullHead, Chunk>>;
}
```

흐름:

```
A → B : push_offer { transfer_id, name, size, sha256, overwrite }
B     : inbox에서 받다 만 파일을 찾는다 → offset 결정
B → A : pull { transfer_id, offset }
        ← res  PullHead { size, sha256 }
        ← STREAM_DATA 청크 …
        ← s_end
B     : sha256 검증 → 임시 파일 → 원자적 rename으로 inbox에 놓는다
B → A : push_offer의 res  TransferDone { written, bytes, sha256, replaced, undo }
```

**왜 미는 대신 당기는가**: 벌크 데이터가 caller → callee로 가는 와이어 경로가 없다(§1). 당기면
`file_io.read_stream`과 똑같이 이미 돌아가는 경로만 쓴다. 그리고 **이어받기가 공짜로 따라온다** —
링크가 끊기면 A가 `push_offer`를 다시 부르고 B가 `offset`부터 당긴다.

대안은 요청 params의 첨부를 상류에 구현하는 것이다(`docs/zyris-protocol.md:316`이 예고한 빈칸).
더 일반적이지만 훨씬 큰 변경이고 v1에 필요하지 않다. **나중에 그것이 생기면 `pull`은 그대로 두고
`push_offer`에 바이트를 실을 수 있다** — 프로토콜을 바꾸지 않고 최적화가 된다.

### 6.3 무결성

**엔진의 `s_end.trailer.sha256`을 믿지 않는다.** 받는 쪽이 그것을 버리기 때문이다(§1). 그러므로:

- A가 보내기 전에 파일 전체의 sha256을 계산해 `push_offer`에 담는다.
- B가 받으면서 sha256을 계산하고, `s_end` 뒤에 `push_offer`가 말한 값과 대조한다.
- 이어받기라면 이미 받아 둔 부분의 해시를 다시 계산해 누적한다.
- 불일치면 받은 것을 **버리고** `err integrity_mismatch`. 부분 파일을 남기면 다음 재개가 그것을
  이어받아 영영 안 맞는다.

### 6.4 크기와 속도

| 항목 | 기본값 | 이유 |
|---|---|---|
| 파일 최대 | 8 GiB | 넘으면 `err payload_too_large`. 상한이 없으면 디스크가 방어선이 된다 |
| 청크 | 64 KiB | **`max_chunk`(256 KiB)와 같게 두면 안 된다.** 그 값이 `initial_stream_credit`과도 같은데, 항목이 msgpack `bin32`로 감싸이며 정확히 5바이트가 붙어 창을 넘는다. 그러면 보내는 쪽이 첫 청크에서 막히고 받는 쪽은 credit을 돌려줄 수 없어 **영구 정지한다** (구현 중 실제로 겪었다: 262,139 통과 / 262,140 정지). 창이 협상값이라는 것도 유의 — 받는 쪽이 `AcceptOptions::limits`로 정한다 |
| 프레임 최대 | 16 MiB | 크레딧이 권고사항이라 이것이 실질 방어선 |
| inbox 총량 | 32 GiB | 넘으면 새 전송을 거부한다 |
| 동시 수신 | 4 | 넘으면 `err overloaded` (retriable) |

전부 설정으로 바꿀 수 있고, 기본값은 보수적으로 잡는다.

---

## 7. 받는 쪽의 안전장치

사람 확인이 없으므로 **이것이 유일한 방어선이다.**

### 7.1 inbox 감옥

목적지는 `$XDG_DATA_HOME/zyris/inbox/<보낸-노드-slug>/` 하나다. **보내는 쪽은 디렉터리를 정하지
못한다** — 이름만 제안한다.

capkit의 `resolve_under`는 감옥이 아니고 심링크를 전혀 다루지 않으므로(§1) **여기서 처음부터
만든다**:

1. 제안된 이름에서 경로 구분자·`..`·NUL·제어문자를 **제거**한다. 거부가 아니라 제거 — 이름 하나
   때문에 전송이 실패할 이유가 없다. 남는 것이 없으면 `file`.
2. 윈도우 예약 이름(`CON`, `PRN`, `AUX`, `NUL`, `COM1`…, `LPT1`…)과 끝의 `.`·공백을 처리한다.
3. inbox 루트를 `canonicalize`해 둔다.
4. 쓰기 직전 최종 경로를 `canonicalize`(부모까지)해 **루트 아래인지 다시 확인한다.**
5. 부모 경로의 어느 조각이라도 심링크면 거부한다 (`symlink_metadata`로 조각마다 확인).
6. 임시 파일에 쓰고 `rename`으로 원자적으로 놓는다. 임시 파일도 같은 디렉터리 안이어야 rename이
   원자적이다.
7. 실행 비트를 제거한다 (`0600`). 받은 파일이 실행 가능할 이유가 없다.
8. macOS면 `com.apple.quarantine`을 붙인다.

**이름 씻기와 경로 확인 둘 다 한다.** 하나만으로는 부족하다 — 씻기는 정상 경로를 지키고, 확인은
씻기를 빠져나간 것을 잡는다. zyris-code가 와이어 이름에서 배운 것과 같다: **판정은 언제나 실제로
이어 붙여 보는 것이다.**

### 7.2 덮어쓰기와 되돌리기

```
send_to(node=B, path="a.pdf")                  → 이미 있으면 err file_exists
send_to(node=B, path="a.pdf", overwrite=true)  → 덮는다
```

덮기 직전 원본을 `$XDG_CACHE_HOME/zyris/inbox-undo/<unix_ms>/<이름>`으로 **옮긴다**(복사가 아니라
이동 — 디스크를 두 배 먹지 않는다). `TransferDone.undo`에 그 자리를 실어 보내 에이전트가 사람에게
말할 수 있게 한다.

zyris-code의 `code_edit`이 `~/.cache/zyris-code/undo/`에 원본을 남기는 것과 같은 규칙이다.
**백업에 실패해도 전송은 진행한다** — 안전망이 없다고 일을 막으면 고칠 수 없는 상태가 생긴다.

되돌림 보관은 30일 또는 4 GiB 중 먼저 닿는 쪽에서 오래된 것부터 지운다.

### 7.3 감사 로그

전송마다 한 줄을 append한다: 시각, 상대 node_id·slug·EndpointId, 이름, 크기, sha256, 최종 경로,
덮었는지, 직접 연결이었는지. `$XDG_STATE_HOME/zyris/transfers.log`.

**사람 확인이 없는 흐름에서 이것이 사후에 무슨 일이 있었는지 아는 유일한 길이다.**

---

## 8. attacca 쪽 변경

### 8.1 `attacca_api`에 도구 셋

```rust
/// 내 iroh 주소를 올린다. 주소가 바뀔 때마다.
async fn peer_publish(&self, endpoint_id: String, addrs: Vec<String>) -> Result<()>;

/// 상대의 주소를 묻는다.
async fn peer_lookup(&self, node: String) -> Result<PeerAddr>;
//   PeerAddr { node_id, slug, endpoint_id, addrs, relay_url, online }

/// 내 계정의 노드 목록 — authorization 판정용.
async fn peer_list(&self) -> Result<Vec<PeerEntry>>;
//   PeerEntry { node_id, slug, endpoint_id, online }
```

**범위는 소유자(owner) 단위다.** 기존 `list_nodes`는 같은 device grant의 형제만 보므로
(`zyris_gateway.rs:623`) 다른 물리 머신끼리는 서로 안 보인다 — 요구사항을 못 채운다.

### 8.2 스코프

새 스코프 `peers:write` 하나. 셋 다 이 스코프로 막는다.

> **배포 순서가 강제된다.** attacca가 모르는 스코프가 하나라도 등록 요청에 들어가면
> `/zyris/v1/device/authorize`가 **422로 통째로 거절한다** — axum의 `Json` 추출기가 열거형을
> 못 읽는 것이라 승인 화면까지 가지도 못한다. 2026-08-03에 `nodes:write`로 실제로 그렇게 걸렸다.
> **attacca를 먼저 배포하고, 그 다음에 노드가 이 스코프를 요청한다.**

재는 법은 `curl` 한 줄이다. 자격이 필요 없고, 승인하지 않으면 600초 뒤 만료된다:

```bash
curl -s -o /dev/null -w '%{http_code}\n' -X POST \
  https://attacca.cc/api/zyris/v1/device/authorize -H 'content-type: application/json' \
  -d '{"name":"scope probe","platform":"linux","scopes":["peers:write"],"client_hint":{}}'
# 200이면 배포됐고, 422면 아직이다
```

### 8.3 DB

`zyris_nodes`에 컬럼 셋을 더하는 마이그레이션 하나: `endpoint_id`(text, nullable, 계정 안에서
unique), `peer_addrs`(jsonb), `peer_addrs_at`(timestamptz).

- `endpoint_id`는 **처음 올린 값이 계속 간다.** 다른 값으로 덮으려는 요청은 거부한다 — 서버가
  조용히 키를 갈아 끼우는 자리를 만들지 않기 위해서다. 바꿔야 하면 노드를 다시 등록한다.
- `peer_addrs` 쓰기는 60초 이상 간격으로 throttle한다. `last_seen_at`이 이미 그렇게 한다.
- sqlx 오프라인 캐시(`crates/attacca-repo-pg/.sqlx/`)를 다시 만들어야 컴파일이 통과한다.

### 8.4 릴레이

`iroh-relay`를 배포에 더한다. Helm 차트에 서비스 하나, `ZYRIS_P2P_RELAY_URL` 환경변수 하나.
`peer_lookup` 응답이 그 값을 실어 나른다.

**릴레이는 노드를 인증하지 않는다.** iroh 릴레이는 EndpointId로 라우팅할 뿐이고 누구나 붙을 수
있다. 그것이 안전한 이유는 릴레이가 암호문만 보고, 실제 authorization은 B가 §4.3에서 하기
때문이다. 남는 위험은 대역폭 남용이므로 **릴레이에 rate limit을 건다.**

---

## 9. 오류 처리

| 코드 | 언제 | retriable |
|---|---|---|
| `peer_offline` | B가 접속해 있지 않다 | 예 |
| `peer_unreachable` | 홀펀칭·릴레이 모두 실패 | 예 |
| `peer_key_changed` | TOFU 불일치 | **아니오** — 사람이 봐야 한다 |
| `peer_not_authorized` | 내 계정의 노드가 아니다 | 아니오 |
| `file_exists` | `overwrite`가 false인데 있다 | 아니오 |
| `integrity_mismatch` | sha256 불일치 | 예 (한 번은 재시도할 값이 있다) |
| `payload_too_large` | 파일·프레임·inbox 상한 초과 | 아니오 |
| `overloaded` | 동시 수신 한도 | 예 |

**연결이 끊기면 재개는 애플리케이션의 일이다** (`docs/zyris-protocol.md:181`). `send_to`를 같은
인자로 다시 부르는 것이 곧 재개다. `transfer_id`는 `(보내는 쪽 node_id, name, size, sha256)`에서
결정론적으로 만들어, 다시 불렀을 때 **같은 전송으로 이어지게** 한다. 받는 쪽의 최종 경로를 재료로
쓰면 안 된다 — 그것은 B가 정하는 값이라 A가 미리 알 수 없다.

---

## 10. 테스트 전략

**단위·프로토콜 — 소켓 없이.** `zyris::testing::duplex()`로 A와 B를 인메모리로 잇는다. 여기서
`push_offer`/`pull`의 전 과정, 이어받기, 무결성 불일치, `overwrite` 갈래를 전부 본다. iroh가
필요 없다.

**감옥 — 순수하게.** 이름 씻기와 경로 확인은 파일시스템을 안 타는 순수 함수로 떼어 둔다.
`../../etc/passwd`, `/etc/passwd`, `C:\Windows\…`, `CON`, `a\0b`, 빈 이름, 유니코드 우회
(`．．/`), 심링크 부모 — 전부 테이블 테스트.

**심링크는 진짜 파일시스템으로 한 번 더.** 순수 함수가 못 보는 자리라 `tempfile`로 실제 링크를
만들어 확인한다.

**iroh 통합 — 로컬 두 엔드포인트.** 한 프로세스에서 Endpoint 둘을 띄워 실제로 붙인다.
릴레이 없이 루프백에서 붙으므로 CI에서 돈다.

**라이브 검증 — 진짜 두 머신.** 로컬이 전부 초록이어도 NAT 통과는 라이브에서만 드러난다.
`examples/transfer_probe.rs`를 두고, **판정은 부작용으로만 한다** — 파일이 실제로 그 자리에
생겼는가, sha256이 맞는가. 에이전트의 말은 근거가 아니다.

**CI.** zyris의 `check.yml`이 도는 것 그대로. iroh 의존이 늘어 빌드가 무거워지므로 `zyris-p2p`는
**기본 feature에서 뺀다** — 노드가 P2P를 안 쓰면 iroh를 컴파일하지 않는다.

---

## 11. 안 하는 것

- **사람이 수락하는 흐름.** 이번 결정은 에이전트 주도다. 나중에 붙일 자리는 §4.3의 authorization
  판정 한 곳이다.
- **계정 간 전송.** 같은 소유자의 노드끼리만. 다른 사용자에게 보내려면 초대·차단·신고가 따라오고
  그것은 별개의 프로젝트다.
- **디렉터리 전송.** 파일 하나씩. 여러 개는 호출을 여러 번 한다.
- **요청 params의 첨부 구현.** §6.2에서 피했다.
- **키 회전.** 키가 바뀌면 TOFU가 막는 것이 지금은 옳은 동작이다.
- **`prog` 프레임으로 진행률.** 엔진이 버리므로 불가능하다. 필요하면 상류를 먼저 고쳐야 한다.
- **문서-코드 불일치 정리.** §1에 여럿 나왔지만(TLS 강제, 하트비트, resume, 인라인 blob 상한,
  `write_at`) 이 작업의 범위가 아니다. 별도 이슈로 남긴다.

---

## 12. 미해결

1. **`iroh-relay` 운영 비용.** 릴레이 폴백이 얼마나 자주 일어나는지는 실사용 전에는 모른다.
   메트릭을 처음부터 넣어 `direct` 비율을 재야 한다 — `SendReceipt.direct`가 그 씨앗이다.
2. **iroh 의존 트리 크기.** 이 머신(RAM 3.6GB)에서 빌드가 얼마나 무거워지는지 실측이 필요하다.
   Task 1의 첫 단계가 그것이다.
3. **`send_to`의 60초 계약.** attacca의 `ZYRIS_CALL_TIMEOUT_SECS`를 늘리는 쪽이 더 깨끗할 수
   있으나 그것은 모든 노드 호출에 걸린다. 우선 `pending: true` 계약으로 가고, 실사용에서 재호출이
   너무 잦으면 그때 다시 본다.
