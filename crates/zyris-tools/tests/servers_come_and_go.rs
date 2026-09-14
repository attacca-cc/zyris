//! MCP servers arriving and leaving while the node is up.
//!
//! Every server here is a real process — `zyris-mcp`'s probe server, built as this crate's
//! `mcp_probe_server` example — and nearly every assertion is about what a **peer** can see and
//! call, read off a real connection rather than off this machine's own bookkeeping. Both halves
//! matter. A supervisor that updated its own list and never touched the node would satisfy any
//! test that asked it what it thought, and would leave an agent calling into a process that is
//! not there.

use std::sync::Arc;
use std::time::Duration;

use zyris::Payload;
use zyris_mcp::{Config, ServerConfig, Started};
use zyris_runtime::{CoreEvent, EventBus, LiveCapabilities, McpServerChange};
use zyris_tools::{AuditLog, Gate, ServerState, ServerView, Servers, Tools};

#[path = "support/probe.rs"]
mod probe;

use probe::probe_server;

/// A command spelled so that no machine could accidentally have one.
const MISSING_COMMAND: &str = "zyris-no-such-mcp-server-anywhere";

fn entry(name: &str, args: &[&str]) -> ServerConfig {
    ServerConfig {
        name: name.to_string(),
        command: probe_server(),
        args: args.iter().map(|a| (*a).to_string()).collect(),
        enabled: true,
    }
}

fn disabled(mut server: ServerConfig) -> ServerConfig {
    server.enabled = false;
    server
}

fn with_command(mut server: ServerConfig, command: &str) -> ServerConfig {
    server.command = command.to_string();
    server
}

/// Everything a run of this machine owns, wired the way `main` wires it.
struct Machine {
    servers: Servers,
    /// The switch, so a test can assert that a server enabled mid-session went behind it.
    gate: Gate,
    /// The servers that were running when this machine started, kept so a test can ask after a
    /// process **after** the supervisor has let go of it. Nothing in production holds these; that
    /// is the point — a withdrawal that only worked because the test was the last one holding on
    /// would be a withdrawal that did not work.
    started: Vec<Arc<zyris_mcp::Promoted>>,
    live: LiveCapabilities,
    bus: EventBus,
    /// The window's handle. Kept so a test can ask what the Tools screen would list, which is a
    /// different question from what the supervisor thinks and from what the peer can see — and
    /// was, for one commit, a different *answer*.
    tools: Tools,
    /// What this machine announces of its own, read off the announcement rather than written down
    /// here. **Not a constant**: `input` and `screen_capture` are announced only where a display
    /// server answers and `file_transfer` only where an endpoint bound, so a fixed list would make
    /// every assertion below pass or fail on whether the machine running the suite has a screen.
    builtin: Vec<String>,
    _log: tempfile::TempDir,
}

impl Machine {
    /// Start what `entries` asks for, announce whatever came up beside this machine's own
    /// capabilities, and hand back the handles a supervisor and a window would hold.
    ///
    /// The same shape `main` uses, and deliberately tolerant in the same way: an entry the file
    /// disables is not started, and one that will not start costs itself and nothing else.
    async fn with(entries: Vec<ServerConfig>) -> Machine {
        let log = tempfile::tempdir().expect("a directory for the audit log");
        let bus = EventBus::new(64);

        let mut running = Vec::new();
        for server in entries.iter().filter(|server| server.enabled) {
            if let Ok(started) = zyris_mcp::config::start_one(server).await {
                running.push(Arc::new(started));
            }
        }

        let gate = Gate::running();
        let tools = Tools::new(
            gate.clone(),
            AuditLog::new(log.path().join("audit.jsonl")),
            log.path().to_path_buf(),
        )
        .with_bus(bus.clone())
        .with_mcp(running.iter().map(|server| server.clone() as _).collect());

        let live = LiveCapabilities::new(tools.clone().into_capabilities());
        let servers = Servers::new(
            &tools,
            live.clone(),
            bus.clone(),
            Started {
                // A file that was read, in a directory this test owns. `problem` is `None`
                // because these entries came from somewhere readable; the case where it is not is
                // `zyris-mcp`'s to prove, and what it costs here is only what the window says.
                path: zyris_mcp::Config::path(log.path()),
                config: Config { servers: entries },
                running: running.clone(),
                problem: None,
            },
        );

        let builtin = live
            .names()
            .await
            .into_iter()
            .filter(|name| !name.starts_with(zyris_mcp::CAPABILITY_PREFIX))
            .collect();

        Machine { servers, live, bus, gate, tools, started: running, builtin, _log: log }
    }

    /// What the peer can see, waited for rather than read once — a re-announce crosses a wire.
    ///
    /// `promoted` is what should be announced **in addition to** this machine's own, so a test
    /// says what it is about, and so this machine's own capabilities are asserted unchanged in
    /// every one of them: a withdrawal that took `terminal` with it would fail here.
    async fn peer_sees(&self, connection: &zyris::Connection, promoted: &[&str]) {
        let mut want: Vec<String> = self.builtin.clone();
        want.extend(promoted.iter().map(|name| (*name).to_string()));
        want.sort();

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let mut names: Vec<String> =
                connection.peer_descriptors().into_iter().map(|d| d.name).collect();
            names.sort();
            if names == want {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the peer still sees {names:?}, expected {want:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// What the window's Tools screen would list, through the command that feeds it.
    ///
    /// Not `live.names()`: the point is the path a person actually sees, which goes through
    /// [`Tools::announcement`].
    async fn window_lists(&self) -> Vec<String> {
        self.tools
            .announcement(&self.live)
            .await
            .capabilities
            .into_iter()
            .map(|capability| capability.name)
            .collect()
    }

    async fn state(&self, name: &str) -> ServerState {
        let view: Vec<ServerView> = self.servers.list().await;
        view.iter()
            .find(|server| server.name == name)
            .unwrap_or_else(|| panic!("`{name}` is not in {view:?}"))
            .state
            .clone()
    }
}

/// A node built from what is announced, connected to an agent that has nothing of its own.
///
/// Returns the agent's side — the only side that can answer "what does the other end see" — and
/// the node, which the caller has to keep alive for the connection to stay up.
async fn agent_connected_to(live: &LiveCapabilities) -> (zyris::Connection, zyris::Node) {
    let node = live
        .install(|capabilities| {
            let mut builder =
                zyris::Node::builder().name("this-machine").kind(zyris::NodeKind::Desktop);
            for capability in capabilities {
                builder = builder.capability_arc(capability.clone());
            }
            builder.build()
        })
        .await
        .expect("the node builds");
    let peer = zyris::Node::builder()
        .name("agent")
        .kind(zyris::NodeKind::Cli)
        .build()
        .expect("a node with nothing on it");
    let (ours, _theirs) = zyris::testing::duplex(&peer, &node).await.expect("they connect");
    (ours, node)
}

/// Everything the bus carried since it was last looked at.
fn drain(events: &mut tokio::sync::broadcast::Receiver<CoreEvent>) -> Vec<CoreEvent> {
    let mut seen = Vec::new();
    while let Ok(event) = events.try_recv() {
        seen.push(event);
    }
    seen
}

fn changes(events: Vec<CoreEvent>) -> Vec<(String, McpServerChange)> {
    events
        .into_iter()
        .filter_map(|event| match event {
            CoreEvent::McpServer { server, change } => Some((server, change)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_server_that_falls_over_is_withdrawn_and_reported_as_having_died() {
    // The requirement the spec singles out. The process goes away with nobody calling it, and the
    // capability has to stop being announced — to the peer, not merely in this machine's notes.
    let machine = Machine::with(vec![entry("desk-notes", &["--exit-after", "150"])]).await;
    let mut events = machine.bus.subscribe();
    let (agent, _node) = agent_connected_to(&machine.live).await;
    machine.peer_sees(&agent, &["mcp_desk-notes"]).await;

    // Let it fall over, then run the check the supervisor runs.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(machine.servers.reap().await, 1, "one server was withdrawn");

    machine.peer_sees(&agent, &[]).await;
    let refused = agent
        .call_raw("mcp_desk-notes.echo", Payload::from_json(serde_json::json!({ "text": "x" })))
        .await
        .expect_err("a withdrawn capability cannot be called");
    assert_eq!(refused.code, zyris::ErrorCode::CapabilityNotAnnounced);

    // And the person is told which of the two withdrawals this was.
    assert_eq!(
        changes(drain(&mut events)),
        [("desk-notes".to_string(), McpServerChange::Died)]
    );
    assert_eq!(machine.state("desk-notes").await, ServerState::Died);

    // Asked again, it does not withdraw the same server twice or say so twice.
    assert_eq!(machine.servers.reap().await, 0);
    assert!(changes(drain(&mut events)).is_empty());
}

#[tokio::test]
async fn a_server_a_person_turned_off_is_withdrawn_and_does_not_look_like_a_crash() {
    // The other half of the same requirement. An agent gets the same answer for both — the
    // capability is not announced — and that is accepted. A person must not.
    let machine = Machine::with(vec![entry("desk-notes", &[])]).await;
    let mut events = machine.bus.subscribe();
    let (agent, _node) = agent_connected_to(&machine.live).await;
    machine.peer_sees(&agent, &["mcp_desk-notes"]).await;

    machine.servers.set_enabled("desk-notes", false).await.expect("turning one off works");

    machine.peer_sees(&agent, &[]).await;
    assert_eq!(
        changes(drain(&mut events)),
        [("desk-notes".to_string(), McpServerChange::Disabled)],
        "nothing crashed, and the event has to say so"
    );
    assert_eq!(machine.state("desk-notes").await, ServerState::Disabled);
    assert_ne!(
        machine.state("desk-notes").await,
        ServerState::Died,
        "the two withdrawals stay distinguishable in what the window reads, too"
    );

    // The process really is stopped, not merely unannounced: a server switched off that keeps
    // running is a process nobody can see and nobody will ever stop.
    assert!(machine.servers.reap().await == 0, "a server that is off is not a server that died");
}

#[tokio::test]
async fn a_server_that_was_turned_off_can_be_turned_back_on_while_connected() {
    let machine = Machine::with(vec![entry("desk-notes", &[])]).await;
    let (agent, _node) = agent_connected_to(&machine.live).await;
    machine.servers.set_enabled("desk-notes", false).await.expect("off");
    machine.peer_sees(&agent, &[]).await;

    let mut events = machine.bus.subscribe();
    machine.servers.set_enabled("desk-notes", true).await.expect("on again");

    machine.peer_sees(&agent, &["mcp_desk-notes"]).await;
    // Announced is not enough: the process behind it has to be a new one that answers.
    let answer = agent
        .call_raw(
            "mcp_desk-notes.echo",
            Payload::from_json(serde_json::json!({ "text": "back again" })),
        )
        .await
        .expect("the restarted server answers");
    assert_eq!(answer.to_json().unwrap()["content"][0]["text"], "back again");

    assert_eq!(
        changes(drain(&mut events)),
        [(
            "desk-notes".to_string(),
            McpServerChange::Announced {
                capability: "mcp_desk-notes".to_string(),
                // Four, because that is what the probe server has — and reading it off the server
                // rather than off the entry is the difference between reporting what was
                // announced and reporting what was asked for.
                tools: 4,
            }
        )]
    );
    assert_eq!(machine.state("desk-notes").await, ServerState::Running);
}

#[tokio::test]
async fn turning_on_a_server_that_will_not_start_costs_that_server_and_nothing_else() {
    let machine = Machine::with(vec![
        entry("desk-notes", &[]),
        disabled(with_command(entry("calendar", &[]), MISSING_COMMAND)),
    ])
    .await;
    let mut events = machine.bus.subscribe();
    let (agent, _node) = agent_connected_to(&machine.live).await;
    machine.peer_sees(&agent, &["mcp_desk-notes"]).await;

    let failure = machine
        .servers
        .set_enabled("calendar", true)
        .await
        .expect_err("a command that is not there cannot be started");
    assert!(
        failure.contains("calendar") && failure.contains(MISSING_COMMAND),
        "the failure has to say which server and what it ran: {failure}"
    );

    // The one that was running is untouched, and so is this machine's own.
    machine.peer_sees(&agent, &["mcp_desk-notes"]).await;
    let answer = agent
        .call_raw(
            "mcp_desk-notes.echo",
            Payload::from_json(serde_json::json!({ "text": "unaffected" })),
        )
        .await
        .expect("the other server keeps working");
    assert_eq!(answer.to_json().unwrap()["content"][0]["text"], "unaffected");

    match machine.state("calendar").await {
        ServerState::Failed { reason } => {
            assert!(reason.contains(MISSING_COMMAND), "what it ran: {reason}");
        }
        other => panic!("expected a failure with a reason in it, got {other:?}"),
    }
    assert_eq!(
        changes(drain(&mut events)),
        [(
            "calendar".to_string(),
            McpServerChange::Failed {
                reason: match machine.state("calendar").await {
                    ServerState::Failed { reason } => reason,
                    other => panic!("{other:?}"),
                }
            }
        )],
        "the event and the state have to carry the same reason, or the window shows one of two"
    );
}

#[tokio::test]
async fn a_server_that_is_running_is_left_alone() {
    // The mutation this test exists to catch is a reaper that withdraws everything. A health
    // check wrong in that direction takes every MCP server off the machine within a second of it
    // starting, and nothing else in the suite would say so.
    let machine = Machine::with(vec![entry("desk-notes", &[]), entry("calendar", &[])]).await;
    let mut events = machine.bus.subscribe();
    let (agent, _node) = agent_connected_to(&machine.live).await;
    machine.peer_sees(&agent, &["mcp_desk-notes", "mcp_calendar"]).await;

    for _ in 0..5 {
        assert_eq!(machine.servers.reap().await, 0, "nothing has died");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    machine.peer_sees(&agent, &["mcp_desk-notes", "mcp_calendar"]).await;
    assert!(drain(&mut events).is_empty(), "a machine where nothing changed says nothing");
    let answer = agent
        .call_raw(
            "mcp_calendar.echo",
            Payload::from_json(serde_json::json!({ "text": "still here" })),
        )
        .await
        .expect("both are still callable");
    assert_eq!(answer.to_json().unwrap()["content"][0]["text"], "still here");
}

#[tokio::test]
async fn the_watcher_withdraws_a_dead_server_with_nobody_asking_it_to() {
    // `reap` on its own proves the decision; this proves something actually runs it. A withdrawal
    // that only happens when a person opens a window is not a withdrawal.
    let machine = Machine::with(vec![
        entry("desk-notes", &["--exit-after", "150"]),
        entry("calendar", &[]),
    ])
    .await;
    let (agent, _node) = agent_connected_to(&machine.live).await;
    machine.peer_sees(&agent, &["mcp_desk-notes", "mcp_calendar"]).await;

    let watching = tokio::spawn(machine.servers.clone().watch());

    // Nothing is called, here or anywhere: the point is that the supervisor noticed on its own.
    // The wait is generous against the interval so a slow machine does not fail this; the
    // interval itself is asserted separately, below.
    tokio::time::timeout(
        zyris_tools::HEALTH_INTERVAL * 6,
        machine.peer_sees(&agent, &["mcp_calendar"]),
    )
    .await
    .expect("the watcher withdrew the dead server on its own");

    watching.abort();

    // And the interval is a decision rather than whatever a test happened to tolerate. The check
    // costs a flag read, so there is no argument on the other side of this; what a longer one
    // buys is a window that lists a server nobody can reach as running.
    assert!(
        zyris_tools::HEALTH_INTERVAL <= Duration::from_secs(5),
        "{:?} is long enough for a person to watch a window lie about a dead server",
        zyris_tools::HEALTH_INTERVAL
    );
}

#[tokio::test]
async fn a_server_the_file_disabled_is_listed_as_off_rather_than_missing() {
    // Task 3's file-level `"enabled": false` reaches here as an entry with no process. It has to
    // read as "off" — not as "crashed", and not as absent: the window lists what the file asks
    // for, and somebody who turned one off has to be able to find it again to turn it back on.
    let machine =
        Machine::with(vec![entry("desk-notes", &[]), disabled(entry("calendar", &[]))]).await;
    let (agent, _node) = agent_connected_to(&machine.live).await;

    machine.peer_sees(&agent, &["mcp_desk-notes"]).await;
    assert_eq!(machine.state("calendar").await, ServerState::Disabled);
    assert_eq!(machine.state("desk-notes").await, ServerState::Running);

    // And it can be turned on from there, which is the whole reason it is listed.
    machine.servers.set_enabled("calendar", true).await.expect("it starts");
    machine.peer_sees(&agent, &["mcp_desk-notes", "mcp_calendar"]).await;
}

#[tokio::test]
async fn what_the_window_reads_says_which_tools_and_which_were_dropped() {
    // The list is what Task 5 renders, so the two things a person cannot get anywhere else have
    // to be in it: which tools an agent can actually reach, and which the server offered that
    // this machine did not announce. `--odd-tools` offers the same name twice, and the second is
    // unreachable by construction.
    let machine = Machine::with(vec![entry("desk-notes", &["--odd-tools"])]).await;

    let view = machine.servers.list().await;
    let server = view.iter().find(|s| s.name == "desk-notes").expect("it is listed");

    assert_eq!(server.capability.as_deref(), Some("mcp_desk-notes"));
    assert_eq!(server.tools, ["search", "untitled", "titled", "anything", "nested.tool"]);
    assert_eq!(server.dropped.len(), 1, "the second `search` cannot be announced");
    assert_eq!(server.dropped[0].name, "search");
    assert!(
        server.dropped[0].reason.contains("search"),
        "the reason has to name it: {}",
        server.dropped[0].reason
    );
}

#[tokio::test]
async fn a_server_enabled_from_the_window_is_behind_the_pause_switch_like_everything_else() {
    // **The one that would fail silently.** A server started at startup goes through
    // `Tools::into_capabilities`, which guards it; one enabled afterwards is announced by a
    // different line of code, and a capability handed to the node unwrapped would work perfectly
    // — right up until somebody hit pause and a third party's process kept being called. The
    // audit log's MCP rule lives in the same wrapper, so it would go too.
    let machine =
        Machine::with(vec![disabled(entry("desk-notes", &[]))]).await;
    let (agent, _node) = agent_connected_to(&machine.live).await;
    machine.servers.set_enabled("desk-notes", true).await.expect("it starts");
    machine.peer_sees(&agent, &["mcp_desk-notes"]).await;

    machine.gate.set_paused(true);

    let refused = agent
        .call_raw(
            "mcp_desk-notes.echo",
            Payload::from_json(serde_json::json!({ "text": "while paused" })),
        )
        .await
        .expect_err("a paused machine does not reach somebody else's process");
    // The code alone would not settle it — a dead server answers with the same one — so the
    // switch's own wording is what says which of the two refused this.
    assert_eq!(refused.code, zyris::ErrorCode::CapabilityUnavailable);
    assert!(refused.message.contains("paused"), "the gate refused it, not the server: {}", refused.message);

    machine.gate.set_paused(false);
    let answer = agent
        .call_raw(
            "mcp_desk-notes.echo",
            Payload::from_json(serde_json::json!({ "text": "and again" })),
        )
        .await
        .expect("unpausing lets it through");
    assert_eq!(answer.to_json().unwrap()["content"][0]["text"], "and again");
}

#[tokio::test]
async fn a_server_that_would_not_start_reads_as_failed_rather_than_as_turned_off() {
    // Three absences that must not collapse into one. This machine has a server the file asks
    // for and that did not start; that is not the same sentence as one somebody switched off, and
    // a person told the wrong one goes looking for a switch they never moved.
    let machine = Machine::with(vec![
        with_command(entry("calendar", &[]), MISSING_COMMAND),
        disabled(entry("desk-notes", &[])),
    ])
    .await;

    assert_eq!(machine.state("desk-notes").await, ServerState::Disabled);
    match machine.state("calendar").await {
        ServerState::Failed { reason } => {
            assert!(reason.contains("calendar"), "which server: {reason}");
        }
        other => panic!("an entry that was asked for and did not start is not off: {other:?}"),
    }
}

#[tokio::test]
async fn a_server_that_is_turned_off_is_stopped_rather_than_merely_unannounced() {
    // **The one that leaves nothing to see.** Withdrawing the capability makes a server invisible
    // to every agent and to the window; if the process is still running after that, nothing in
    // this program lists it, nothing can reach it, and nothing will ever stop it. The handle here
    // is the test's own, held on purpose: relying on the supervisor's last `Arc` going away is
    // exactly the assumption that fails, because `Tools` keeps one of its own for the life of the
    // process.
    let machine = Machine::with(vec![entry("desk-notes", &[])]).await;
    let held = machine.started[0].clone();
    assert!(held.is_running(), "it is running to begin with");

    machine.servers.set_enabled("desk-notes", false).await.expect("off");

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while held.is_running() {
        assert!(
            std::time::Instant::now() < deadline,
            "the server is no longer announced and its process is still running"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn turning_on_a_server_that_is_already_on_leaves_it_exactly_as_it_was() {
    // A second click on a button that was never redrawn is not a fresh instruction. Acting on it
    // would stop a working server and start a new one — losing whatever state it held — and the
    // start would then be refused, because its capability is still announced by the one that was
    // already running. A redundant click would turn a working server into a failed one.
    let machine = Machine::with(vec![entry("desk-notes", &[])]).await;
    let held = machine.started[0].clone();
    let mut events = machine.bus.subscribe();
    let (agent, _node) = agent_connected_to(&machine.live).await;
    machine.peer_sees(&agent, &["mcp_desk-notes"]).await;

    let view = machine.servers.set_enabled("desk-notes", true).await.expect("it is already on");

    assert_eq!(view.state, ServerState::Running);
    assert!(held.is_running(), "the process that was running is the one still running");
    assert!(changes(drain(&mut events)).is_empty(), "nothing changed, so nothing is announced");
    machine.peer_sees(&agent, &["mcp_desk-notes"]).await;
    let answer = agent
        .call_raw(
            "mcp_desk-notes.echo",
            Payload::from_json(serde_json::json!({ "text": "untouched" })),
        )
        .await
        .expect("it still answers");
    assert_eq!(answer.to_json().unwrap()["content"][0]["text"], "untouched");
}

#[tokio::test]
async fn turning_off_a_server_that_is_already_off_is_not_an_error_and_changes_nothing() {
    let machine = Machine::with(vec![disabled(entry("desk-notes", &[]))]).await;
    let mut events = machine.bus.subscribe();

    let view = machine.servers.set_enabled("desk-notes", false).await.expect("already off");

    assert_eq!(view.state, ServerState::Disabled);
    assert!(changes(drain(&mut events)).is_empty());
}

#[tokio::test]
async fn a_server_the_file_never_mentioned_cannot_be_switched() {
    // A window can only list what the file asks for, so this is a stale screen or a typo rather
    // than a state anything should invent an entry for.
    let machine = Machine::with(vec![entry("desk-notes", &[])]).await;

    let error = machine
        .servers
        .set_enabled("calendar", true)
        .await
        .expect_err("there is no such server");

    assert!(error.contains("calendar"), "{error}");
    assert_eq!(machine.servers.list().await.len(), 1, "nothing was invented");
}

#[tokio::test]
async fn the_tools_screen_lists_what_is_announced_now_and_not_what_was_announced_at_startup() {
    // **The screen half of everything above, and the half that was wrong.** A peer stops seeing a
    // withdrawn capability because the node re-announces; the window has no wire to watch, and for
    // one commit it read a snapshot taken when the process started. So this walks the three ways
    // the announcement moves — off, on, and a death nobody asked for — and asserts each of them
    // against what a person would be looking at, not against what the supervisor believes.
    //
    // A screen that goes on advertising a capability this machine has withdrawn is worse than one
    // that is merely behind: it tells somebody their agents can reach a process that is gone.
    let machine = Machine::with(vec![
        entry("desk-notes", &[]),
        disabled(entry("calendar", &[])),
        entry("clock", &["--exit-after", "150"]),
    ])
    .await;
    let (agent, _node) = agent_connected_to(&machine.live).await;
    machine.peer_sees(&agent, &["mcp_desk-notes", "mcp_clock"]).await;

    let listed = machine.window_lists().await;
    assert!(listed.contains(&"mcp_desk-notes".to_string()), "{listed:?}");
    assert!(!listed.contains(&"mcp_calendar".to_string()), "{listed:?}");

    // Turned off from the window. The row it was clicked on is gone from the Tools screen too.
    machine.servers.set_enabled("desk-notes", false).await.expect("it was running");
    let listed = machine.window_lists().await;
    assert!(
        !listed.contains(&"mcp_desk-notes".to_string()),
        "the Tools screen still offers a capability this machine has withdrawn: {listed:?}"
    );

    // Turned on from the window. A server the file disabled was announced to nobody and has to
    // appear here the moment it is.
    machine.servers.set_enabled("calendar", true).await.expect("the probe server starts");
    let listed = machine.window_lists().await;
    assert!(
        listed.contains(&"mcp_calendar".to_string()),
        "the Tools screen omits a capability this machine is announcing: {listed:?}"
    );

    // And the withdrawal nobody clicked.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(machine.servers.reap().await, 1, "the one that exits on a timer");
    let listed = machine.window_lists().await;
    assert!(
        !listed.contains(&"mcp_clock".to_string()),
        "the Tools screen still lists a server whose process is gone: {listed:?}"
    );

    // Throughout, this machine's own capabilities are exactly where they were. The screen is
    // reporting one node, and a promoted server coming or going is about that server.
    for builtin in &machine.builtin {
        assert!(listed.contains(builtin), "`{builtin}` left the Tools screen: {listed:?}");
    }
    // The last word belongs to the peer: the screen and the wire agree.
    machine.peer_sees(&agent, &["mcp_calendar"]).await;
    let mut announced = machine.window_lists().await;
    let mut on_the_wire: Vec<String> =
        agent.peer_descriptors().into_iter().map(|d| d.name).collect();
    announced.sort();
    on_the_wire.sort();
    assert_eq!(announced, on_the_wire);
}
