#![cfg(feature = "transfer-send")]

//! The sending side, driven end to end **without a socket**.
//!
//! The rendezvous is a real `AttaccaApiClient` speaking to a stub server over
//! `zyris::testing::duplex`, and the peer link is a [`LoopbackLink`] that runs the real
//! `push_offer`/`pull` exchange between two real `LocalPeerTransfer`s over a second `duplex`. So a
//! file genuinely leaves one directory and arrives in another; the only thing missing is QUIC.
//!
//! **Every refusal test asserts the dial counter is still zero.** A guard that refuses *after*
//! connecting has already handed the peer a connection it should never have had, and a test that
//! only looks at the returned error cannot tell the two apart.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use zyris::{Chunk, Datum, ErrorCode, Node, NodeKind, Result, Streaming, WireError};
use zyris_attacca::{
    AttaccaApi, AttaccaApiClient, AttaccaApiServer, ZAgent, ZHistoryQuery, ZJob, ZJobFilter,
    ZJobUpdate, ZMe, ZNewAgent, ZNewJob, ZNewNode, ZNewProject, ZNewSession, ZNewWork, ZNode,
    ZPeerAddr, ZPeerEntry, ZProject, ZProjectUpdate, ZSession, ZSessionEvent, ZSessionFilter,
    ZTurnFrame, ZTurnStatus, ZUsage, ZWork, ZWorkFilter, ZWorkTasks, ZWorkUpdate,
};
use zyris_caps::file_transfer::{FileTransfer, SendReceipt};
use zyris_caps::peer_transfer::{
    PeerTransfer, PeerTransferClient, PeerTransferServer, PullHead, TransferDone, TransferOffer,
};
use zyris_capkit::transfer::send::{FileTransferConfig, LocalFileTransfer, PeerLink, PeerSession};
use zyris_capkit::transfer::{LocalPeerTransfer, TransferConfig};
use zyris_p2p::fingerprint::PeerConfirmer;
use zyris_p2p::iroh;
use zyris_p2p::tofu::TofuStore;

// ---------------------------------------------------------------------------------------------
// The rendezvous stub
// ---------------------------------------------------------------------------------------------

/// Everything `send_to` asks the rendezvous, and nothing else. The 36 other tools on
/// `attacca_api` have to be present for the trait to be implemented, but a test that reached one
/// of them would be testing something this file does not claim to cover — so they answer with an
/// error that says exactly that rather than a plausible-looking stub value.
struct StubRendezvous {
    answer: Result<ZPeerAddr>,
}

fn unused<T>() -> Result<T> {
    Err(WireError::internal("this tool is not part of the send path".to_string()))
}

#[async_trait::async_trait]
impl AttaccaApi for StubRendezvous {
    async fn peer_lookup(&self, _slug: String) -> Result<ZPeerAddr> {
        self.answer.clone()
    }

    async fn me(&self) -> Result<ZMe> {
        unused()
    }
    async fn list_agents(&self) -> Result<Vec<ZAgent>> {
        unused()
    }
    async fn create_agent(&self, _agent: ZNewAgent) -> Result<ZAgent> {
        unused()
    }
    async fn list_projects(&self) -> Result<Vec<ZProject>> {
        unused()
    }
    async fn get_project(&self, _project_id: String) -> Result<ZProject> {
        unused()
    }
    async fn create_project(&self, _project: ZNewProject) -> Result<ZProject> {
        unused()
    }
    async fn update_project(&self, _id: String, _update: ZProjectUpdate) -> Result<ZProject> {
        unused()
    }
    async fn delete_project(&self, _project_id: String) -> Result<()> {
        unused()
    }
    async fn list_sessions(&self, _filter: ZSessionFilter) -> Result<Vec<ZSession>> {
        unused()
    }
    async fn create_session(
        &self,
        _agent_id: String,
        _title: Option<String>,
        _project_id: Option<String>,
    ) -> Result<ZSession> {
        unused()
    }
    async fn create_session_with(&self, _session: ZNewSession) -> Result<ZSession> {
        unused()
    }
    async fn session_history(
        &self,
        _session_id: String,
        _query: ZHistoryQuery,
    ) -> Result<Vec<ZSessionEvent>> {
        unused()
    }
    async fn session_usage(&self, _session_id: String) -> Result<ZUsage> {
        unused()
    }
    async fn send_message(&self, _s: String, _m: String, _d: Vec<Datum>) -> Result<()> {
        unused()
    }
    async fn cancel_turn(&self, _session_id: String) -> Result<()> {
        unused()
    }
    async fn list_jobs(&self, _filter: ZJobFilter) -> Result<Vec<ZJob>> {
        unused()
    }
    async fn get_job(&self, _job_id: String) -> Result<ZJob> {
        unused()
    }
    async fn create_job(&self, _job: ZNewJob) -> Result<ZJob> {
        unused()
    }
    async fn update_job(&self, _job_id: String, _update: ZJobUpdate) -> Result<ZJob> {
        unused()
    }
    async fn delete_job(&self, _job_id: String) -> Result<()> {
        unused()
    }
    async fn list_works(&self, _filter: ZWorkFilter) -> Result<Vec<ZWork>> {
        unused()
    }
    async fn get_work(&self, _work_id: String) -> Result<ZWork> {
        unused()
    }
    async fn create_work(&self, _work: ZNewWork) -> Result<ZWork> {
        unused()
    }
    async fn update_work(&self, _work_id: String, _update: ZWorkUpdate) -> Result<ZWork> {
        unused()
    }
    async fn delete_work(&self, _work_id: String) -> Result<()> {
        unused()
    }
    async fn approve_work_goal(&self, _work_id: String) -> Result<ZWork> {
        unused()
    }
    async fn approve_work_plan(&self, _work_id: String) -> Result<ZWork> {
        unused()
    }
    async fn work_tasks(&self, _work_id: String) -> Result<ZWorkTasks> {
        unused()
    }
    async fn stop_work(&self, _work_id: String) -> Result<()> {
        unused()
    }
    async fn continue_work(&self, _work_id: String) -> Result<ZWork> {
        unused()
    }
    async fn work_message(&self, _w: String, _m: String, _d: Vec<Datum>) -> Result<()> {
        unused()
    }
    async fn turn_events(
        &self,
        _session_id: String,
        _after: Option<i64>,
    ) -> Result<Streaming<ZTurnStatus, ZTurnFrame>> {
        unused()
    }
    async fn register_node(&self, _request: ZNewNode) -> Result<ZNode> {
        unused()
    }
    async fn list_nodes(&self) -> Result<Vec<ZNode>> {
        unused()
    }
    async fn delete_node(&self, _node_id: String) -> Result<()> {
        unused()
    }
    async fn peer_publish(&self, _endpoint_id: String, _addrs: Vec<String>) -> Result<()> {
        unused()
    }
    async fn peer_list(&self) -> Result<Vec<ZPeerEntry>> {
        unused()
    }
}

/// A live `AttaccaApiClient` whose `peer_lookup` gives back `answer`.
///
/// The nodes are dropped when this returns and the connection keeps working: `Node::connect_over`
/// clones the capability set into the connection, so nothing here has to outlive the handshake.
async fn rendezvous(answer: Result<ZPeerAddr>) -> AttaccaApiClient {
    let server = Node::builder()
        .name("attacca")
        .kind(NodeKind::Server)
        .capability(AttaccaApiServer(StubRendezvous { answer }))
        .build()
        .unwrap();
    let caller = Node::builder().name("node").kind(NodeKind::Cli).build().unwrap();
    let (caller_conn, _server_conn) = zyris::testing::duplex(&caller, &server).await.unwrap();
    caller_conn.wait_capability(Duration::from_secs(2)).await.unwrap()
}

// ---------------------------------------------------------------------------------------------
// The peer link stub
// ---------------------------------------------------------------------------------------------

/// A [`PeerLink`] that wires the sender to a real receiving `LocalPeerTransfer` over
/// `zyris::testing::duplex` — the same in-memory pair `tests/peer_transfer.rs` uses, so `push_offer`
/// and `pull` run for real and the bytes land on a real disk.
struct LoopbackLink {
    receiver: TransferConfig,
    /// The name the receiving side files the transfer under, exactly as the accept loop will pass
    /// it (Task 4.2).
    from: String,
    /// How many times a link was actually opened. **This is what the refusal tests assert on**: a
    /// guard is only doing its job if it stops the dial, not merely the reply.
    dials: Arc<AtomicUsize>,
    direct: bool,
    /// The receiving end of each connection, parked so it is not dropped while the transfer runs.
    held: tokio::sync::Mutex<Vec<zyris::Connection>>,
}

impl LoopbackLink {
    fn new(receiver: TransferConfig, dials: Arc<AtomicUsize>) -> LoopbackLink {
        LoopbackLink {
            receiver,
            from: "a".to_string(),
            dials,
            direct: true,
            held: tokio::sync::Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl PeerLink for LoopbackLink {
    async fn open(&self, _addr: &ZPeerAddr, sender: LocalPeerTransfer) -> Result<PeerSession> {
        self.dials.fetch_add(1, Ordering::SeqCst);
        let receiving = LocalPeerTransfer::receiver_pending(self.receiver.clone(), self.from.clone());
        let a = Node::builder()
            .name("a")
            .kind(NodeKind::Cli)
            .capability(PeerTransferServer(sender))
            .build()?;
        let b = Node::builder()
            .name("b")
            .kind(NodeKind::Cli)
            .capability(PeerTransferServer(receiving.clone()))
            .build()?;
        let (a_conn, b_conn) = zyris::testing::duplex(&a, &b).await?;
        // The receiving side can only be handed its `pull` handle once the connection exists —
        // the same ordering `LocalPeerTransfer::set_peer` documents.
        let back: PeerTransferClient = b_conn.wait_capability(Duration::from_secs(2)).await?;
        receiving.set_peer(back);
        let client: PeerTransferClient = a_conn.capability().expect("b announced peer_transfer");
        self.held.lock().await.push(b_conn);
        Ok(PeerSession { client, connection: a_conn })
    }

    async fn is_direct(&self, _endpoint_id: &str) -> bool {
        self.direct
    }
}

// ---------------------------------------------------------------------------------------------
// Confirmers
// ---------------------------------------------------------------------------------------------

/// Says yes to every unknown peer and counts how often it was asked. A person is what a real one
/// consults; the count is how these tests tell "asked again" from "the pin held".
struct AlwaysConfirm {
    asked: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl PeerConfirmer for AlwaysConfirm {
    async fn confirm(&self, _label: &str, _fingerprint: &str) -> bool {
        self.asked.fetch_add(1, Ordering::SeqCst);
        true
    }
}

// ---------------------------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------------------------

/// A key that is a real `EndpointId`, derived from a fixed seed so a test can name the same peer
/// twice. `TofuStore` parses everything it is given, so a placeholder string will not do.
fn endpoint_id(seed: u8) -> String {
    iroh::SecretKey::from_bytes(&[seed; 32]).public().to_string()
}

fn peer(slug: &str, seed: u8) -> ZPeerAddr {
    ZPeerAddr {
        node_id: format!("node-{slug}"),
        slug: slug.to_string(),
        endpoint_id: endpoint_id(seed),
        addrs: Vec::new(),
        relay_url: None,
        online: true,
    }
}

struct Fixture {
    transfer: LocalFileTransfer,
    root: tempfile::TempDir,
    inbox: tempfile::TempDir,
    _undo: tempfile::TempDir,
    _pins: tempfile::TempDir,
    ledger: PathBuf,
    dials: Arc<AtomicUsize>,
    asked: Arc<AtomicUsize>,
}

impl Fixture {
    async fn new(answer: Result<ZPeerAddr>) -> Fixture {
        Fixture::build(answer, Duration::from_secs(30), None, None).await
    }

    /// `confirmer` and `link` fall back to the defaults every other test wants — a confirmer that
    /// accepts and counts, and a loopback link. A test that has to vary one of them says so here
    /// rather than assembling a second `LocalFileTransfer` of its own.
    async fn build(
        answer: Result<ZPeerAddr>,
        wire_deadline: Duration,
        confirmer: Option<Arc<dyn PeerConfirmer>>,
        link: Option<Arc<dyn PeerLink>>,
    ) -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let inbox = tempfile::tempdir().unwrap();
        let undo = tempfile::tempdir().unwrap();
        let pins = tempfile::tempdir().unwrap();
        let ledger = pins.path().join("peers.json");

        let dials = Arc::new(AtomicUsize::new(0));
        let asked = Arc::new(AtomicUsize::new(0));
        let receiver = TransferConfig {
            inbox: inbox.path().to_path_buf(),
            undo: undo.path().to_path_buf(),
            ..TransferConfig::default()
        };
        let transfer = LocalFileTransfer::new(
            FileTransferConfig {
                root: root.path().to_path_buf(),
                inbox: inbox.path().to_path_buf(),
                node_id: "sender-node".to_string(),
                wire_deadline,
            },
            rendezvous(answer).await,
            TofuStore::new(&ledger),
            confirmer.unwrap_or_else(|| Arc::new(AlwaysConfirm { asked: asked.clone() })),
            link.unwrap_or_else(|| Arc::new(LoopbackLink::new(receiver, dials.clone()))),
        );
        Fixture { transfer, root, inbox, _undo: undo, _pins: pins, ledger, dials, asked }
    }

    async fn write(&self, name: &str, content: &[u8]) -> PathBuf {
        let path = self.root.path().join(name);
        tokio::fs::write(&path, content).await.unwrap();
        path
    }

    async fn send(&self, node: &str, path: &str) -> Result<SendReceipt> {
        self.transfer.send_to(node.to_string(), path.to_string(), None, None).await
    }

    async fn pins(&self) -> String {
        tokio::fs::read_to_string(&self.ledger).await.unwrap_or_default()
    }

    fn dialed(&self) -> usize {
        self.dials.load(Ordering::SeqCst)
    }
}

fn code_of(error: &WireError) -> String {
    match &error.code {
        ErrorCode::Other(code) => code.clone(),
        other => format!("{other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------
// The happy path
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn the_file_arrives_and_the_receipt_describes_it() {
    let fixture = Fixture::new(Ok(peer("laptop", 1))).await;
    let content = b"the whole report".repeat(500);
    fixture.write("report.pdf", &content).await;

    let receipt = fixture.send("laptop", "report.pdf").await.unwrap();

    assert!(!receipt.pending, "a finished transfer must not look unfinished: {receipt:?}");
    assert_eq!(receipt.next, None);
    assert_eq!(receipt.node, "laptop");
    assert_eq!(receipt.bytes, content.len() as u64);
    assert!(receipt.direct);
    assert!(!receipt.replaced);
    let landed = fixture.inbox.path().join("a").join("report.pdf");
    assert_eq!(receipt.written, landed.display().to_string());
    assert_eq!(tokio::fs::read(&landed).await.unwrap(), content);
    assert_eq!(
        receipt.sha256,
        hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&content)),
        "the receipt must name the hash of what actually landed"
    );
}

/// Korean, deliberately: a name whose characters are three bytes each is what proves the whole
/// path — the proposed name, the receiving side's washing, and the `.part` slot's length
/// truncation — cuts on character boundaries rather than bytes. Cutting a 3-byte character in
/// half panics, and an ASCII-only test never reaches that code at all.
#[tokio::test]
async fn a_multibyte_name_survives_the_whole_path() {
    let fixture = Fixture::new(Ok(peer("laptop", 1))).await;
    let content = b"multibyte".repeat(100);
    fixture.write("report.pdf", &content).await;

    let receipt = fixture
        .transfer
        .send_to(
            "laptop".to_string(),
            "report.pdf".to_string(),
            Some("분기별 보고서.pdf".to_string()),
            None,
        )
        .await
        .unwrap();

    let landed = fixture.inbox.path().join("a").join("분기별 보고서.pdf");
    assert_eq!(receipt.written, landed.display().to_string());
    assert_eq!(tokio::fs::read(&landed).await.unwrap(), content);
}

#[tokio::test]
async fn inbox_list_reports_what_arrived_and_hides_what_is_still_in_flight() {
    let fixture = Fixture::new(Ok(peer("laptop", 1))).await;
    fixture.write("report.pdf", b"contents").await;
    fixture.send("laptop", "report.pdf").await.unwrap();
    // A transfer still in flight leaves one of these behind. It is not something that arrived.
    tokio::fs::write(fixture.inbox.path().join("a").join("half.bin.abc123.part"), b"partial")
        .await
        .unwrap();

    let listed = fixture.transfer.inbox_list().await.unwrap();

    assert_eq!(listed.len(), 1, "only the finished file should be listed: {listed:?}");
    assert_eq!(listed[0].name, "report.pdf");
    assert_eq!(listed[0].from, "a");
    assert_eq!(listed[0].bytes, 8);
}

#[tokio::test]
async fn an_inbox_that_does_not_exist_yet_lists_nothing_rather_than_failing() {
    let fixture = Fixture::new(Ok(peer("laptop", 1))).await;
    tokio::fs::remove_dir_all(fixture.inbox.path()).await.unwrap();
    assert_eq!(fixture.transfer.inbox_list().await.unwrap(), Vec::new());
}

// ---------------------------------------------------------------------------------------------
// Guard: the read jail
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn an_absolute_path_outside_the_root_is_refused_and_never_dialed() {
    let fixture = Fixture::new(Ok(peer("laptop", 1))).await;
    let elsewhere = tempfile::tempdir().unwrap();
    let secret = elsewhere.path().join("id_ed25519");
    tokio::fs::write(&secret, b"a private key").await.unwrap();

    let error = fixture.send("laptop", secret.to_str().unwrap()).await.unwrap_err();

    assert_eq!(code_of(&error), "path_outside_root", "{error:?}");
    assert!(!error.retriable, "a path outside the root will not become inside it: {error:?}");
    assert_eq!(fixture.dialed(), 0, "nothing outside the root may reach a peer link");
}

#[tokio::test]
async fn a_symlink_pointing_out_of_the_root_is_refused_and_never_dialed() {
    let fixture = Fixture::new(Ok(peer("laptop", 1))).await;
    let elsewhere = tempfile::tempdir().unwrap();
    let secret = elsewhere.path().join("id_ed25519");
    tokio::fs::write(&secret, b"a private key").await.unwrap();
    // The relative path stays inside the root; only following the link leaves it. This is the case
    // `resolve_under` alone cannot see, since it never touches the filesystem.
    std::os::unix::fs::symlink(&secret, fixture.root.path().join("innocent.txt")).unwrap();

    let error = fixture.send("laptop", "innocent.txt").await.unwrap_err();

    assert_eq!(code_of(&error), "path_outside_root", "{error:?}");
    assert_eq!(fixture.dialed(), 0);
}

#[tokio::test]
async fn dot_dot_climbing_out_of_the_root_is_refused() {
    let fixture = Fixture::new(Ok(peer("laptop", 1))).await;
    let elsewhere = tempfile::tempdir().unwrap();
    tokio::fs::write(elsewhere.path().join("outside.txt"), b"not yours").await.unwrap();
    let climb = format!(
        "../{}/outside.txt",
        elsewhere.path().strip_prefix(elsewhere.path().parent().unwrap()).unwrap().display()
    );

    // Both temp directories live under the same parent, so this is a genuine escape by `..`
    // rather than a path that happens not to exist.
    let error = fixture.send("laptop", &climb).await.unwrap_err();

    assert_eq!(code_of(&error), "path_outside_root", "{error:?}");
    assert_eq!(fixture.dialed(), 0);
}

#[tokio::test]
async fn a_directory_is_not_a_file_to_send() {
    let fixture = Fixture::new(Ok(peer("laptop", 1))).await;
    tokio::fs::create_dir(fixture.root.path().join("papers")).await.unwrap();
    let error = fixture.send("laptop", "papers").await.unwrap_err();
    assert_eq!(code_of(&error), "not_a_file", "{error:?}");
    assert_eq!(fixture.dialed(), 0);
}

// ---------------------------------------------------------------------------------------------
// Guard: the rendezvous answered about the peer that was named
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn an_answer_about_a_different_node_is_refused_and_never_dialed() {
    // The caller said "laptop" and the rendezvous came back describing "desktop". Sending anyway
    // puts the file on a machine nobody named, and — because the pin is keyed on the name that was
    // said — would surface as "this peer's key changed", a false alarm at the one place a real one
    // has to be believed.
    let fixture = Fixture::new(Ok(peer("desktop", 2))).await;
    fixture.write("report.pdf", b"contents").await;

    let error = fixture.send("laptop", "report.pdf").await.unwrap_err();

    assert_eq!(code_of(&error), "peer_lookup_mismatch", "{error:?}");
    assert!(!error.retriable);
    assert_eq!(fixture.dialed(), 0);
    assert_eq!(fixture.asked.load(Ordering::SeqCst), 0, "nobody should be asked about a peer we refused to name");
}

#[tokio::test]
async fn a_canonicalized_spelling_of_the_same_name_is_still_the_peer_that_was_named() {
    // A deployment that lowercases slugs answers a lookup for "Laptop" with "laptop" and means the
    // same machine. Refusing that would be a false alarm of its own.
    let fixture = Fixture::new(Ok(peer("laptop", 1))).await;
    fixture.write("report.pdf", b"contents").await;
    assert!(fixture.send("Laptop", "report.pdf").await.is_ok());
}

#[tokio::test]
async fn an_offline_peer_is_refused_before_anything_is_dialed() {
    let fixture = Fixture::new(Ok(ZPeerAddr { online: false, ..peer("laptop", 1) })).await;
    fixture.write("report.pdf", b"contents").await;

    let error = fixture.send("laptop", "report.pdf").await.unwrap_err();

    assert_eq!(code_of(&error), "peer_offline", "{error:?}");
    assert!(error.retriable, "a peer that is offline now can be online later: {error:?}");
    assert_eq!(fixture.dialed(), 0);
}

// ---------------------------------------------------------------------------------------------
// Guard: the pin
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_changed_peer_key_is_refused_and_never_dialed() {
    let fixture = Fixture::new(Ok(peer("laptop", 9))).await;
    fixture.write("report.pdf", b"contents").await;
    // What a person confirmed earlier, under the name they said.
    TofuStore::new(&fixture.ledger).pin_preapproved("laptop", &endpoint_id(1)).await.unwrap();

    let error = fixture.send("laptop", "report.pdf").await.unwrap_err();

    assert_eq!(code_of(&error), "peer_key_changed", "{error:?}");
    assert!(
        !error.retriable,
        "a changed key must not invite a second, quieter attempt at the same slot: {error:?}"
    );
    assert_eq!(fixture.dialed(), 0, "a peer whose key changed must not be connected to at all");
    assert_eq!(
        fixture.asked.load(Ordering::SeqCst),
        0,
        "a changed key is not a judgment call to put to a person"
    );
    assert!(
        fixture.pins().await.contains(&endpoint_id(1)),
        "the pin that was already there must survive the refusal"
    );
}

#[tokio::test]
async fn an_unknown_peer_the_confirmer_refuses_is_not_dialed_and_not_pinned() {
    struct RefuseEverything;
    #[async_trait::async_trait]
    impl PeerConfirmer for RefuseEverything {
        async fn confirm(&self, _label: &str, _fingerprint: &str) -> bool {
            false
        }
    }

    let fixture = Fixture::build(
        Ok(peer("laptop", 1)),
        Duration::from_secs(30),
        Some(Arc::new(RefuseEverything)),
        None,
    )
    .await;
    fixture.write("report.pdf", b"contents").await;

    let error = fixture.send("laptop", "report.pdf").await.unwrap_err();

    assert_eq!(code_of(&error), "peer_not_confirmed", "{error:?}");
    assert_eq!(fixture.dialed(), 0);
    assert!(
        !fixture.pins().await.contains("laptop"),
        "a peer nobody approved must not end up in the ledger"
    );
}

#[tokio::test]
async fn the_pin_is_keyed_on_the_name_the_caller_said_not_the_one_the_server_returned() {
    // The caller said "Laptop"; the rendezvous answered with its own spelling, "laptop". The ledger
    // slot has to be the caller's word — attacca can respell, reissue and reassign every string it
    // sends back, and a slot it can move is a slot a substituted peer can arrive in as a stranger.
    let fixture = Fixture::new(Ok(peer("laptop", 1))).await;
    fixture.write("report.pdf", b"contents").await;

    fixture.send("Laptop", "report.pdf").await.unwrap();

    let pins = fixture.pins().await;
    assert!(pins.contains("\"Laptop\""), "the ledger should be keyed on what the caller said: {pins}");
    assert!(
        !pins.contains("\"laptop\""),
        "keying on the server's spelling puts the slot back under the server's control: {pins}"
    );
}

#[tokio::test]
async fn a_peer_already_pinned_is_not_put_to_a_person_a_second_time() {
    let fixture = Fixture::new(Ok(peer("laptop", 1))).await;
    fixture.write("report.pdf", b"contents").await;

    fixture.send("laptop", "report.pdf").await.unwrap();
    assert_eq!(fixture.asked.load(Ordering::SeqCst), 1, "the first send asks");

    fixture
        .transfer
        .send_to("laptop".into(), "report.pdf".into(), Some("again.pdf".into()), None)
        .await
        .unwrap();
    assert_eq!(fixture.asked.load(Ordering::SeqCst), 1, "the pin held, so nobody was asked again");
}

// ---------------------------------------------------------------------------------------------
// The deadline
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn running_out_of_time_is_a_pending_receipt_not_an_error() {
    /// Never answers. Standing in for a transfer that is simply still going when the clock runs
    /// out — which is what a large file over a slow link is.
    struct NeverOpens;
    #[async_trait::async_trait]
    impl PeerLink for NeverOpens {
        async fn open(&self, _addr: &ZPeerAddr, _sender: LocalPeerTransfer) -> Result<PeerSession> {
            std::future::pending().await
        }
    }

    let fixture = Fixture::build(
        Ok(peer("laptop", 1)),
        Duration::from_millis(80),
        None,
        Some(Arc::new(NeverOpens)),
    )
    .await;
    fixture.write("report.pdf", b"contents").await;

    let receipt = fixture.send("laptop", "report.pdf").await.unwrap();

    assert!(receipt.pending, "running out of time is not a failure: {receipt:?}");
    assert_eq!(receipt.node, "laptop");
    assert_eq!(receipt.bytes, 0, "nothing was confirmed written, so nothing may be reported as written");
    assert_eq!(receipt.written, "");
    assert!(!receipt.direct);
    // Stuck in `open`, so the peer has nothing yet — the line must not claim there is something to
    // resume. It stays a `pending` receipt because a lookup and a dial are both worth retrying.
    let next = receipt.next.expect("a pending receipt has to say what to do now");
    assert!(next.contains("not been reached"), "{next}");
}

/// The one place "resume" is a true word: the link is open and `push_offer` is in flight, so bytes
/// may already be sitting on the peer as a `.part`. Nothing stood on this branch before — the test
/// above is named for it but never gets past `open`.
#[tokio::test]
async fn running_out_of_time_while_the_bytes_are_moving_promises_a_resume() {
    /// A peer that accepts the link and then never answers `push_offer`.
    #[derive(Clone)]
    struct StallsOnPush;
    #[async_trait::async_trait]
    impl PeerTransfer for StallsOnPush {
        async fn push_offer(&self, _offer: TransferOffer) -> Result<TransferDone> {
            std::future::pending().await
        }
        async fn pull(
            &self,
            _transfer_id: String,
            _offset: u64,
        ) -> Result<Streaming<PullHead, Chunk>> {
            Err(WireError::internal("the sender is the one pulled from".to_string()))
        }
    }

    struct OpensThenStalls {
        dials: Arc<AtomicUsize>,
        held: tokio::sync::Mutex<Vec<zyris::Connection>>,
    }
    #[async_trait::async_trait]
    impl PeerLink for OpensThenStalls {
        async fn open(&self, _addr: &ZPeerAddr, sender: LocalPeerTransfer) -> Result<PeerSession> {
            self.dials.fetch_add(1, Ordering::SeqCst);
            let a = Node::builder()
                .name("a")
                .kind(NodeKind::Cli)
                .capability(PeerTransferServer(sender))
                .build()?;
            let b = Node::builder()
                .name("b")
                .kind(NodeKind::Cli)
                .capability(PeerTransferServer(StallsOnPush))
                .build()?;
            let (a_conn, b_conn) = zyris::testing::duplex(&a, &b).await?;
            // `wait_capability`, not `capability`: the announce arrives during the handshake, and
            // reading it synchronously right after `duplex` returns races that.
            let client: PeerTransferClient = a_conn.wait_capability(Duration::from_secs(2)).await?;
            self.held.lock().await.push(b_conn);
            Ok(PeerSession { client, connection: a_conn })
        }
    }

    let dials = Arc::new(AtomicUsize::new(0));
    let link = Arc::new(OpensThenStalls {
        dials: dials.clone(),
        held: tokio::sync::Mutex::new(Vec::new()),
    });
    let fixture =
        Fixture::build(Ok(peer("laptop", 1)), Duration::from_millis(200), None, Some(link)).await;
    fixture.write("report.pdf", b"contents").await;

    let receipt = fixture.send("laptop", "report.pdf").await.unwrap();

    assert!(receipt.pending, "{receipt:?}");
    assert_eq!(dials.load(Ordering::SeqCst), 1, "the link has to have opened for this to be the case");
    let next = receipt.next.expect("a pending receipt has to say what to do now");
    assert!(next.contains("resume"), "{next}");
}

/// The other side of the same clock. Running out of time *before the file has even been measured*
/// leaves nothing on the peer to come back to, so answering `pending` would send the caller — an
/// agent, which does what `next` tells it — into a retry that begins the same read from zero and
/// ends in the same place, forever.
///
/// Staying in that phase takes a file that genuinely cannot be read inside the deadline, so this
/// uses a sparse one: `set_len` costs no disk, and hashing still has to pull every one of those
/// bytes through. 256 MiB against a 1 ms budget is ~2 orders of magnitude of margin either way.
///
/// (A zero deadline does *not* work, which is worth recording: `timeout` keeps polling the inner
/// future until its timer fires, so a small file finishes hashing and moves on before the expiry
/// ever lands.)
#[tokio::test]
async fn running_out_of_time_before_the_file_is_measured_is_refused_not_called_resumable() {
    let fixture =
        Fixture::build(Ok(peer("laptop", 1)), Duration::from_millis(1), None, None).await;
    let sparse = fixture.write("report.pdf", b"").await;
    tokio::fs::File::options()
        .write(true)
        .open(&sparse)
        .await
        .unwrap()
        .set_len(256 * 1024 * 1024)
        .await
        .unwrap();

    let error = fixture.send("laptop", "report.pdf").await.unwrap_err();

    assert_eq!(code_of(&error), "source_too_slow_to_measure", "{error:?}");
    assert!(
        !error.retriable,
        "a retry repeats the same read and fails in the same place, so it must not be invited"
    );
    assert_eq!(fixture.dialed(), 0, "nothing may be dialed before the file has been measured");
}

// ---------------------------------------------------------------------------------------------
// The production link
// ---------------------------------------------------------------------------------------------

/// Everything above swaps `PeerLink` out for an in-memory pair, which is what makes the guard tests
/// instant. That leaves `IrohPeerLink` — the one a real node actually uses — executed by nothing at
/// all, so this test drives a whole transfer through two real iroh endpoints on loopback, with the
/// relay disabled. It is the only test here that opens a socket.
///
/// **The ordering it relies on:** B calls `set_peer` as soon as its own handshake finishes, and A
/// cannot send `push_offer` until it has received B's announcement — which B sends *during* that
/// handshake. So B's local `set_peer` always lands before A's first call crosses the wire.
#[tokio::test]
async fn the_iroh_link_carries_a_real_transfer() {
    tokio::time::timeout(Duration::from_secs(40), async {
        let root = tempfile::tempdir().unwrap();
        let inbox = tempfile::tempdir().unwrap();
        let undo = tempfile::tempdir().unwrap();
        let pins = tempfile::tempdir().unwrap();
        let content = b"over quic".repeat(2000);
        tokio::fs::write(root.path().join("report.pdf"), &content).await.unwrap();

        let receiving_key = iroh::SecretKey::generate();
        let receiving_id = receiving_key.public().to_string();
        let receiving_endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(receiving_key)
            .alpns(vec![zyris_p2p::transport::ALPN.to_vec()])
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let addrs: Vec<String> =
            receiving_endpoint.addr().ip_addrs().map(|a| a.to_string()).collect();
        assert!(!addrs.is_empty(), "the receiving endpoint has to be dialable on loopback");

        let receiver_config = TransferConfig {
            inbox: inbox.path().to_path_buf(),
            undo: undo.path().to_path_buf(),
            ..TransferConfig::default()
        };
        let accepting = receiving_endpoint.clone();
        let receiver_task = tokio::spawn(async move {
            let pending = zyris_p2p::peer::accept_next(&accepting).await.unwrap();
            let (_peer, transport) =
                zyris_p2p::peer::establish(pending, Duration::from_secs(10)).await.unwrap();
            let receiving = LocalPeerTransfer::receiver_pending(receiver_config, "a".to_string());
            let node = Node::builder()
                .name("b")
                .kind(NodeKind::Cli)
                .capability(PeerTransferServer(receiving.clone()))
                .build()
                .unwrap();
            let conn = node.accept(transport, zyris::AcceptOptions::default()).await.unwrap();
            let back: PeerTransferClient =
                conn.wait_capability(Duration::from_secs(5)).await.unwrap();
            receiving.set_peer(back);
            // Handed back rather than dropped: dropping it here would close the link while the
            // transfer is still running.
            conn
        });

        let sending_endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .unwrap();

        let transfer = LocalFileTransfer::new(
            FileTransferConfig {
                root: root.path().to_path_buf(),
                inbox: inbox.path().to_path_buf(),
                node_id: "sender-node".to_string(),
                wire_deadline: Duration::from_secs(30),
            },
            rendezvous(Ok(ZPeerAddr {
                node_id: "node-laptop".to_string(),
                slug: "laptop".to_string(),
                endpoint_id: receiving_id,
                addrs,
                relay_url: None,
                online: true,
            }))
            .await,
            TofuStore::new(pins.path().join("peers.json")),
            Arc::new(AlwaysConfirm { asked: Arc::new(AtomicUsize::new(0)) }),
            Arc::new(zyris_capkit::transfer::IrohPeerLink::new(sending_endpoint.clone())),
        );

        let receipt =
            transfer.send_to("laptop".into(), "report.pdf".into(), None, None).await.unwrap();
        let _receiving_conn = receiver_task.await.unwrap();

        assert!(!receipt.pending, "{receipt:?}");
        assert_eq!(receipt.bytes, content.len() as u64);
        let landed = inbox.path().join("a").join("report.pdf");
        assert_eq!(tokio::fs::read(&landed).await.unwrap(), content);
        // With the relay disabled there is no other path this could have taken, so a `direct` of
        // false would mean `is_direct` is reading iroh's address table wrongly rather than that the
        // bytes went somewhere else.
        assert!(receipt.direct, "a loopback dial with no relay configured is direct by construction");
        drop(sending_endpoint);
        drop(receiving_endpoint);
    })
    .await
    .expect("the iroh-backed transfer exceeded its deadline");
}
