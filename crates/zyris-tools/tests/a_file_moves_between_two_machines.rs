//! Two machines, one account, one real file — driven through the whole of what
//! `zyris_tools::transfer` wired up.
//!
//! Everything here is real except the rendezvous. The endpoints are real iroh endpoints with real
//! UDP sockets and persisted keys; the pins are a real [`TofuStore`] on disk; the bytes travel over
//! a real QUIC connection through the real [`IrohPeerLink`] and are taken by the real `serve_peers`
//! accept loop. What stands in for Attacca is [`Rendezvous`] below: an `AttaccaApi` implementation
//! speaking to the node over `zyris::testing::duplex`, so that `Transfers::on_connect` receives a
//! genuine `AttaccaApiClient` off a genuine `zyris::Connection` — the same call `main` makes, with
//! the same type, taking the same path through `set_api` and `peer_publish`.
//!
//! # Which layer is driven
//!
//! The topmost one. A send is an [`IncomingCall`] dispatched at
//! `Guarded<FileTransferServer<LocalFileTransfer>>`, which is the object `Tools::into_capabilities`
//! hands the node — so the gate, the tool audit line and `send_to`'s own three guards are all in
//! the path, exactly as they are for an agent's call arriving over the websocket. Nothing here
//! reaches past that into `IrohPeerLink::open` or `push_offer` directly.
//!
//! # What this cannot cover, and what has to be checked by hand instead
//!
//! **The rendezvous itself.** `peer_lookup`, `peer_list` and `peer_publish` are answered here by a
//! stub that always agrees, so nothing in this file says whether Attacca's real answers have the
//! shape `send_to` expects, whether a node enrolled on an account actually appears in another
//! node's `peer_list`, or whether `peer_publish`'s addresses survive the round trip. Nor does it
//! exercise enrolment, the node token, or reconnection against a real server. Two machines on one
//! real account are still the only way to see any of that.
//!
//! **A real network.** Both endpoints are on this machine, so a transfer here never crosses a NAT,
//! never falls back to a relay, and never needs iroh's discovery — the published addresses always
//! work on the first try, which is the case `IrohPeerLink::dial`'s own comments say is *not* the
//! interesting one.
//!
//! # One upstream defect this file found and works around
//!
//! `send_to` is retried on exactly one condition, described at [`receiver_not_wired_up_yet`]: the
//! accepting side of `zyris-transfer` can answer a `push_offer` before it has finished wiring the
//! connection up, and reports a transient condition as a permanent `Internal` error. Roughly one
//! send in four hit it on this machine. Nothing else here is retried.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use zyris::caps::file_transfer::SendReceipt;
use zyris::{
    Datum, ErrorCode, IncomingCall, Node, NodeKind, Outgoing, Payload, Result, Serialization,
    ServeCapability, Streaming, WireError,
};
use zyris_attacca::{
    AttaccaApi, AttaccaApiServer, ZAgent, ZHistoryQuery, ZJob, ZJobFilter, ZJobUpdate, ZMe,
    ZNewAgent, ZNewJob, ZNewNode, ZNewProject, ZNewSession, ZNewWork, ZNode, ZPeerAddr, ZPeerEntry,
    ZProject, ZProjectUpdate, ZSession, ZSessionEvent, ZSessionFilter, ZTurnFrame, ZTurnStatus,
    ZUsage, ZWork, ZWorkFilter, ZWorkTasks, ZWorkUpdate,
};
use zyris_tools::{AuditLog, DenyUnknown, Gate, Tools, Transfers};

/// How long a file gets to appear before the test says what it was waiting for.
///
/// Generous on purpose: this bounds a failure, not a success. A working transfer between two
/// sockets on one machine lands in well under a second, and the only thing a shorter deadline
/// would buy is a flaky test on a loaded CI runner.
const ARRIVAL_DEADLINE: Duration = Duration::from_secs(20);

// -------------------------------------------------------------------------------------------
// The four things this file claims
// -------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_file_moves_between_two_machines_byte_for_byte() {
    let account = Account::new();
    let alpha_dirs = Dirs::new();
    let beta_dirs = Dirs::new();
    let alpha = Machine::boot(&alpha_dirs, "alpha", &account).await;
    let beta = Machine::boot(&beta_dirs, "beta", &account).await;

    // The pin a person would have made by comparing fingerprints out loud. `pin_preapproved` is
    // the affordance the library provides for exactly this, and the only one there is until the
    // confirmer window exists.
    beta.pin(&alpha).await;

    // Not "some bytes": a body with a length, a newline and a non-ASCII character in it, so a
    // transfer that truncated, re-encoded or line-ending-mangled the file would not compare equal.
    let sent = b"one file\r\nand a second line \xe2\x80\x94 with an em dash\n\x00\x01\x02".to_vec();
    beta.write_source("letter.txt", &sent);

    let receipt = beta.send("alpha", "letter.txt").await.expect("the send was refused");

    let landed = alpha.inbox().join("beta").join("letter.txt");
    let arrived = wait_for_file(&landed, "the file beta sent to alpha").await;
    assert_eq!(arrived, sent, "the bytes that landed are not the bytes that were sent");
    assert_eq!(
        receipt.written,
        landed.display().to_string(),
        "the receipt names a different file from the one that appeared"
    );
    assert_eq!(receipt.bytes, sent.len() as u64);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_pin_still_matches_after_both_machines_are_rebuilt() {
    // The whole reason the key is persisted, and the exact shape of the failure: a machine that
    // generated a fresh key each launch would still *receive* perfectly — the accept loop never
    // consults a pin — while every peer that had pinned it refused it from then on, and only when
    // sending. So what this asserts is a second send that was never re-pinned.
    //
    // `serve_peers` owns a clone of the endpoint and never returns, so the first pair's sockets are
    // still bound when the second pair comes up: this is a re-bind inside one process rather than a
    // restart of two. That does not weaken the claim. `authorize_peer` runs *before* the dial, so
    // which socket ends up answering has no bearing on whether the pin matched — and the pin is
    // decided by the endpoint id the ledger holds against the one the rendezvous now reports, which
    // is re-read from the freshly bound endpoint below.
    let account = Account::new();
    let alpha_dirs = Dirs::new();
    let beta_dirs = Dirs::new();

    let first_alpha = Machine::boot(&alpha_dirs, "alpha", &account).await;
    let first_beta = Machine::boot(&beta_dirs, "beta", &account).await;
    first_beta.pin(&first_alpha).await;
    first_beta.write_source("before.txt", b"before the restart");
    first_beta.send("alpha", "before.txt").await.expect("the first send was refused");
    wait_for_file(
        &first_alpha.inbox().join("beta").join("before.txt"),
        "the file sent before the restart",
    )
    .await;

    drop(first_alpha);
    drop(first_beta);

    // Same directories, so the same key file and the same ledger. Nothing is pinned this time.
    let alpha = Machine::boot(&alpha_dirs, "alpha", &account).await;
    let beta = Machine::boot(&beta_dirs, "beta", &account).await;
    beta.write_source("after.txt", b"after the restart");

    let receipt = match beta.send("alpha", "after.txt").await {
        Ok(receipt) => receipt,
        Err(error) => panic!(
            "the second send was refused with {:?}: {}. A `peer_key_changed` here is this \
             machine having become a different peer across a rebind — the pin beta made is \
             against the endpoint id alpha had before.",
            error.code, error.message
        ),
    };

    let landed = alpha.inbox().join("beta").join("after.txt");
    let arrived = wait_for_file(&landed, "the file sent after the restart").await;
    assert_eq!(arrived, b"after the restart");
    assert_eq!(receipt.written, landed.display().to_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_that_was_never_pinned_is_refused_and_says_so() {
    // An agent has to be able to tell "this machine is not trusted for that name" from "the network
    // is down". The first is a decision this node made and no retry will change; the second is
    // worth calling again. So the assertion is on the code, not on `is_err`.
    let account = Account::new();
    let alpha_dirs = Dirs::new();
    let gamma_dirs = Dirs::new();
    let alpha = Machine::boot(&alpha_dirs, "alpha", &account).await;
    let gamma = Machine::boot(&gamma_dirs, "gamma", &account).await;

    // Enrolled on the account and perfectly reachable — the rendezvous answers for it, and the
    // endpoint is bound and listening two sockets away. The one thing it does not have is a pin.
    gamma.write_source("uninvited.txt", b"this must never land");

    let refusal = gamma.send("alpha", "uninvited.txt").await.expect_err("an unpinned peer sent");

    assert_eq!(
        refusal.code,
        ErrorCode::Other("peer_not_confirmed".to_string()),
        "a refusal an agent cannot tell from a network failure is not a refusal: {} ({:?})",
        refusal.message,
        refusal.code
    );
    // And it really did not land. The code alone would be satisfied by a guard that refuses after
    // handing the peer the bytes.
    assert!(
        !alpha.inbox().join("gamma").exists(),
        "{} exists, so an unpinned peer reached the inbox anyway",
        alpha.inbox().join("gamma").display()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn both_logs_record_the_transfer_and_neither_records_its_contents() {
    // `transfers.jsonl` is the receiving side's record — a delivery never passes through `Guarded`,
    // because it does not arrive on the Attacca connection at all — and the tool audit line is the
    // sending side's. Between them they are the only account of what moved, and a log that carried
    // the file would be a second copy of it in a file meant to be handable to someone helping you.
    let account = Account::new();
    let alpha_dirs = Dirs::new();
    let beta_dirs = Dirs::new();
    let alpha = Machine::boot(&alpha_dirs, "alpha", &account).await;
    let beta = Machine::boot(&beta_dirs, "beta", &account).await;
    beta.pin(&alpha).await;

    const SECRET: &str = "hunter2-correct-horse-battery-staple";
    beta.write_source("credentials.txt", SECRET.as_bytes());

    beta.send("alpha", "credentials.txt").await.expect("the send was refused");

    // First: the secret really did travel. Without this the two absence assertions below would
    // both hold on a transfer that never happened, which is the way a test like this passes for
    // the wrong reason.
    let landed = alpha.inbox().join("beta").join("credentials.txt");
    let arrived = wait_for_file(&landed, "the file whose contents must stay out of the logs").await;
    assert_eq!(arrived, SECRET.as_bytes(), "the secret never arrived, so its absence proves nothing");

    let transfers_log =
        wait_for_file(&alpha.transfer_log(), "alpha's transfers.jsonl, one line per received file")
            .await;
    let transfers_log = String::from_utf8(transfers_log).expect("transfers.jsonl is not UTF-8");
    assert!(
        transfers_log.contains("credentials.txt"),
        "a received file with no line in transfers.jsonl is a file written to this machine with \
         no record that it happened: {transfers_log}"
    );
    assert!(
        !transfers_log.contains(SECRET),
        "transfers.jsonl carries the file's contents: {transfers_log}"
    );

    let tool_log = std::fs::read_to_string(beta.audit_log()).expect("beta wrote no audit log");
    assert!(
        tool_log.contains("send_to") && tool_log.contains("credentials.txt"),
        "the send is not in the tool audit log: {tool_log}"
    );
    assert!(!tool_log.contains(SECRET), "the tool audit line carries the file's contents: {tool_log}");
}

// -------------------------------------------------------------------------------------------
// A machine
// -------------------------------------------------------------------------------------------

/// The two directories a machine keeps, held apart from the machine itself so the same pair can be
/// handed to a second [`Machine::boot`] — which is what "restart" means here.
struct Dirs {
    data: tempfile::TempDir,
    root: tempfile::TempDir,
}

impl Dirs {
    fn new() -> Dirs {
        Dirs { data: tempfile::tempdir().unwrap(), root: tempfile::tempdir().unwrap() }
    }
}

/// One node: its peer identity, its announced `file_transfer`, and the connection to the stub
/// rendezvous that `Transfers::on_connect` was given.
struct Machine {
    /// The name a caller says to reach this machine. It is what the ledger is keyed on — never the
    /// endpoint id and never anything the server issued. See `LocalFileTransfer::authorize_peer`.
    slug: String,
    transfers: Transfers,
    /// The `Guarded<FileTransferServer<..>>` the node would have been handed, picked out of
    /// `Tools::into_capabilities` by descriptor name rather than by position.
    file_transfer: Arc<dyn ServeCapability>,
    audit_log: PathBuf,
    root: PathBuf,
    /// Both ends of the duplex to the stub rendezvous. Dropping either closes the connection, and
    /// a closed connection takes the `AttaccaApiClient` inside `Rendezvous` down with it — so they
    /// are held for as long as the machine is.
    _to_attacca: zyris::Connection,
    _at_attacca: zyris::Connection,
}

impl Machine {
    async fn boot(dirs: &Dirs, slug: &str, account: &Arc<Account>) -> Machine {
        let data = dirs.data.path().to_path_buf();
        let root = dirs.root.path().to_path_buf();

        let transfers = Transfers::bind(&data, root.clone(), Arc::new(DenyUnknown))
            .await
            .expect("this machine could not bind a peer endpoint");
        account.enrol(slug, &transfers.peering().endpoint().id().to_string());

        let audit_log = data.join("audit.jsonl");
        let tools = Tools::new(Gate::running(), AuditLog::new(audit_log.clone()), root.clone())
            .with_transfer(&transfers);
        let file_transfer = tools
            .into_capabilities()
            .into_iter()
            .find(|capability| capability.descriptor().name == "file_transfer")
            .expect("file_transfer was not announced");

        // Exactly what `main` does on every established connection, with a real client on a real
        // connection: `set_api`, `peer_publish`, and the accept loop once.
        let (to_attacca, at_attacca) = account.connect().await;
        transfers.on_connect(to_attacca.clone()).await;

        let published = account.published(slug);
        assert!(
            !published.is_empty(),
            "{slug} published no address, so nothing can be dialled at it — either \
             `Transfers::on_connect` did not reach `peer_publish` or the endpoint reported no \
             candidate addresses"
        );

        Machine {
            slug: slug.to_string(),
            transfers,
            file_transfer,
            audit_log,
            root,
            _to_attacca: to_attacca,
            _at_attacca: at_attacca,
        }
    }

    /// The pin a person makes by reading a fingerprint aloud — the one step of this that still has
    /// no window to do it in, which is why the test does it by hand.
    async fn pin(&self, peer: &Machine) {
        self.transfers
            .peering()
            .tofu()
            .pin_preapproved(&peer.slug, &peer.transfers.peering().endpoint().id().to_string())
            .await
            .expect("the pin could not be written");
    }

    fn write_source(&self, name: &str, bytes: &[u8]) {
        std::fs::write(self.root.join(name), bytes).expect("the source file could not be written");
    }

    /// One `file_transfer.send_to`, as an agent's call arrives: through the gate and the audit log
    /// and into the capability, with the parameters spelled the way the wire spells them.
    async fn send(&self, to: &str, path: &str) -> Result<SendReceipt> {
        let deadline = Instant::now() + ARRIVAL_DEADLINE;
        loop {
            match self.send_once(to, path).await {
                Err(error) if receiver_not_wired_up_yet(&error) && Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                settled => return settled,
            }
        }
    }

    async fn send_once(&self, to: &str, path: &str) -> Result<SendReceipt> {
        let call = IncomingCall {
            tool: "send_to".to_string(),
            params: Payload::from_json(serde_json::json!({ "node": to, "path": path })),
            serialization: Serialization::Json,
            meta: Payload::default(),
        };
        let receipt = match self.file_transfer.dispatch(call).await? {
            Outgoing::Response(payload) => {
                payload.to_typed::<SendReceipt>().expect("send_to answered something else")
            }
            Outgoing::Stream { .. } => panic!("send_to is not a streaming tool"),
        };
        // `send_to` answers `pending: true` rather than an error when its wire deadline runs out,
        // so a test that only looked at `Result` would read a timeout as a success. Between two
        // sockets on one machine this never happens — which is exactly why it is asserted rather
        // than hoped for.
        assert!(
            !receipt.pending,
            "send_to ran out of its wire deadline and asked to be called again ({}); nothing here \
             should ever take that long",
            receipt.next.as_deref().unwrap_or("no reason given")
        );
        Ok(receipt)
    }

    fn inbox(&self) -> PathBuf {
        self.transfers.inbox().to_path_buf()
    }

    /// Where the received-file log is, spelled out rather than asked for: `Transfers` keeps the
    /// name private and only the inbox is public. Getting it wrong fails as a deadline naming this
    /// path, which is a legible way to be wrong.
    fn transfer_log(&self) -> PathBuf {
        self.transfers.inbox().parent().unwrap().join("transfers.jsonl")
    }

    fn audit_log(&self) -> &Path {
        &self.audit_log
    }
}

/// **A race inside `zyris_transfer::listen::serve_one`, hit about one send in four on this
/// machine.** The accepting side plugs the handle `push_offer` pulls bytes back through — the
/// `set_peer` call — in on the line *after* `Node::accept` returns, and the connection's read loop
/// is already running by then on a task of its own. A sender that gets its `push_offer` onto the
/// wire before that task is next scheduled is answered by a `LocalPeerTransfer` whose `peer` slot
/// is still empty, and the answer is `Internal` / "this node is not set up as a receiver" with
/// `retriable: false` — a permanent-looking failure for a condition that clears in microseconds.
///
/// Retrying is safe and is what the protocol already prescribes for a transfer that did not
/// settle: the arguments are unchanged, so the `transfer_id` is the same one, and `push_offer`'s
/// `in_flight` claim is what keeps a repeat from racing the first attempt.
///
/// Matched on the message because upstream gives nothing narrower — the code is plain `Internal`,
/// which is also what a full disk or an unreadable inbox comes back as. Narrow on purpose: every
/// refusal this file is *about* carries an `ErrorCode::Other`, so none of them can be swallowed
/// here.
fn receiver_not_wired_up_yet(error: &WireError) -> bool {
    error.code == ErrorCode::Internal && error.message.contains("not set up as a receiver")
}

/// Waits for a file to exist and be readable, and says what it was waiting for when it never is.
///
/// Polled rather than slept through: the accept loop takes a connection on a task of its own, so
/// the only honest bound is a deadline.
async fn wait_for_file(path: &Path, what: &str) -> Vec<u8> {
    let deadline = Instant::now() + ARRIVAL_DEADLINE;
    let mut last = String::new();
    while Instant::now() < deadline {
        match std::fs::read(path) {
            Ok(bytes) => return bytes,
            Err(error) => last = error.to_string(),
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "waited {}s for {what} at {} and it never appeared: {last}",
        ARRIVAL_DEADLINE.as_secs(),
        path.display()
    );
}

// -------------------------------------------------------------------------------------------
// The stub rendezvous
// -------------------------------------------------------------------------------------------

/// One machine as the rendezvous knows it: the name a caller says, the key QUIC authenticates
/// against, and whatever the machine last published about where it can be reached.
#[derive(Clone)]
struct Enrolled {
    slug: String,
    endpoint_id: String,
    addrs: Vec<String>,
}

/// The account's node list, shared by every stub connection in one test.
///
/// It is deliberately credulous — it answers for every machine enrolled on it and refuses none —
/// because none of the four claims above is about what Attacca decides. What it does not fake is
/// the addresses: those arrive through `peer_publish`, from the connect hook, so a machine that
/// never published is a machine this directory cannot say where to find.
struct Account {
    machines: Mutex<Vec<Enrolled>>,
}

impl Account {
    fn new() -> Arc<Account> {
        Arc::new(Account { machines: Mutex::new(Vec::new()) })
    }

    /// Records a machine under a name, replacing whatever was there under it before — which is
    /// what a rebound endpoint looks like from the server's side.
    fn enrol(&self, slug: &str, endpoint_id: &str) {
        let mut machines = self.machines.lock().unwrap();
        machines.retain(|machine| machine.slug != slug);
        machines.push(Enrolled {
            slug: slug.to_string(),
            endpoint_id: endpoint_id.to_string(),
            addrs: Vec::new(),
        });
    }

    fn published(&self, slug: &str) -> Vec<String> {
        let machines = self.machines.lock().unwrap();
        machines
            .iter()
            .find(|machine| machine.slug == slug)
            .map(|machine| machine.addrs.clone())
            .unwrap_or_default()
    }

    /// A connection to the stub, of the same type and through the same handshake as the one the
    /// connector hands `Transfers::on_connect`.
    async fn connect(self: &Arc<Account>) -> (zyris::Connection, zyris::Connection) {
        let attacca = Node::builder()
            .name("attacca")
            .kind(NodeKind::Service)
            .capability(AttaccaApiServer(Rendezvous(self.clone())))
            .build()
            .expect("the stub rendezvous would not build");
        let node = Node::builder()
            .name("zyris")
            .kind(NodeKind::Desktop)
            .build()
            .expect("the node would not build");
        zyris::testing::duplex(&node, &attacca).await.expect("the stub handshake failed")
    }
}

/// Everything `Transfers` asks the rendezvous, and nothing else.
///
/// The other thirty-five tools on `attacca_api` have to exist for the trait to be implemented. They
/// answer with an error saying they are not part of this path rather than with a plausible stub
/// value, so a test that started depending on one of them fails loudly instead of quietly testing
/// this file's imagination.
struct Rendezvous(Arc<Account>);

fn unused<T>() -> Result<T> {
    Err(WireError::internal("this tool is not part of what a transfer asks the rendezvous".to_string()))
}

#[zyris::async_trait]
impl AttaccaApi for Rendezvous {
    async fn peer_publish(&self, endpoint_id: String, addrs: Vec<String>) -> Result<()> {
        let mut machines = self.0.machines.lock().unwrap();
        match machines.iter_mut().find(|machine| machine.endpoint_id == endpoint_id) {
            Some(machine) => {
                machine.addrs = addrs;
                Ok(())
            }
            None => Err(WireError::internal(format!("{endpoint_id} is not a node of this account"))),
        }
    }

    async fn peer_lookup(&self, slug: String) -> Result<ZPeerAddr> {
        let machines = self.0.machines.lock().unwrap();
        let machine = machines
            .iter()
            .find(|machine| machine.slug.eq_ignore_ascii_case(&slug))
            .ok_or_else(|| WireError::internal(format!("this account has no node called {slug}")))?;
        Ok(ZPeerAddr {
            node_id: format!("node-{}", machine.slug),
            slug: machine.slug.clone(),
            endpoint_id: machine.endpoint_id.clone(),
            addrs: machine.addrs.clone(),
            // The deployment's relay, which there is not one of here: both endpoints are on this
            // machine and reach each other by the addresses they published.
            relay_url: None,
            online: true,
        })
    }

    async fn peer_list(&self) -> Result<Vec<ZPeerEntry>> {
        let machines = self.0.machines.lock().unwrap();
        Ok(machines
            .iter()
            .map(|machine| ZPeerEntry {
                node_id: format!("node-{}", machine.slug),
                slug: machine.slug.clone(),
                endpoint_id: machine.endpoint_id.clone(),
                online: true,
            })
            .collect())
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
}
