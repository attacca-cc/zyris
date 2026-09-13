//! The whole MCP path, joined up: a file on disk, a real process, a node, and an agent calling it.
//!
//! Everything underneath this has tests of its own. `zyris-mcp` proves that a server spawns and
//! that a tool list translates; `servers_come_and_go.rs` proves that servers arrive and leave
//! while a node is up. **What none of them does is start from the file a person edits and finish
//! at what an agent gets back**, and that is the one claim the README makes on the front page:
//! that a local MCP server's tools sit beside the built-in ones and an agent cannot tell the
//! difference.
//!
//! So every test here goes through the real pieces rather than arranging their results:
//! [`zyris_mcp::config::start`] reads an `mcp-servers.json` this test wrote, [`Tools::with_mcp`]
//! and `into_capabilities` guard and assemble what came up, a [`zyris::Node`] is built from that
//! through [`LiveCapabilities::install`], and **every assertion about what this machine offers is
//! read off the far end of a connection** — a peer's descriptors, a peer's call, a peer's error.
//! Nothing here asks this machine what it thinks it announced.
//!
//! Two things that are deliberately *not* rearranged into something easier:
//!
//! - **The server list is written as text, not as a `Config` value.** Half of what this file is
//!   for is the reading of that file — a default `enabled`, a field spelled wrong — and a test
//!   that handed `config::start` a struct would skip the only part a person actually touches.
//! - **The audit assertions read the file on disk**, not the entries this process happened to
//!   construct. What must not be written down is what must not be *on disk*.
//!
//! What this file does not reach: `zyris-app`'s `main`, which is what wires these pieces together
//! on a real launch, and Attacca, which is the other end of a real connection. The guide in
//! `README.md` under "Checking it against a real agent" covers what only the user can run.

use std::time::Duration;

use serde_json::json;
use zyris::{CapabilityDescriptor, ErrorCode, Payload, Transfer};
use zyris_mcp::{CAPABILITY_PREFIX, Started};
use zyris_runtime::{EventBus, LiveCapabilities};
use zyris_tools::{AuditLog, Gate, Outcome, Servers, Tools};

#[path = "support/probe.rs"]
mod probe;

use probe::probe_server;

/// A run of this machine, started from a server list on disk.
///
/// The same order `zyris-app`'s `main` does it in: read the file, start what it asks for, guard
/// and assemble everything this machine offers, then build the node out of that.
struct Machine {
    tools: Tools,
    gate: Gate,
    servers: Servers,
    live: LiveCapabilities,
    log: AuditLog,
    /// What this machine announces of its own, read off the announcement rather than written
    /// down. **Not a constant**: `input` and `screen_capture` are announced only where a display
    /// server answers and `file_transfer` only where an endpoint bound, so a fixed list would
    /// decide every assertion below on whether the machine running the suite has a screen.
    builtin: Vec<String>,
    /// The instance's data directory: the server list, the audit log, and the root relative paths
    /// resolve against all live here, which is also how `main` scopes them.
    dir: tempfile::TempDir,
}

impl Machine {
    /// Write `file` as this instance's `mcp-servers.json`, then start from it.
    async fn started_from(file: &str) -> Machine {
        let dir = tempfile::tempdir().expect("a data directory for this instance");
        std::fs::write(zyris_mcp::Config::path(dir.path()), file).expect("the server list writes");

        // The real reader and the real spawner. Nothing here decides which servers come up.
        let started: Started = zyris_mcp::config::start(dir.path()).await;

        let gate = Gate::running();
        let log = AuditLog::new(dir.path().join("audit.jsonl"));
        let tools = Tools::new(gate.clone(), log.clone(), dir.path().to_path_buf())
            .with_bus(EventBus::new(64))
            .with_mcp(started.running.iter().map(|server| server.clone() as _).collect());

        let live = LiveCapabilities::new(tools.clone().into_capabilities());
        let servers = Servers::new(&tools, live.clone(), EventBus::new(64), started);

        let builtin: Vec<String> = live
            .names()
            .await
            .into_iter()
            .filter(|name| !name.starts_with(CAPABILITY_PREFIX))
            .collect();
        // A machine that announced none of its own would make "beside the built-in ones"
        // vacuous, and these two are the two that need neither a display nor a network.
        for required in ["terminal", "file_io"] {
            assert!(
                builtin.iter().any(|name| name == required),
                "this machine announces none of its own {required}: {builtin:?}"
            );
        }

        Machine { tools, gate, servers, live, log, builtin, dir }
    }

    /// An agent on the other end of a real connection to a node built from what is announced.
    ///
    /// The node comes back with it because the connection dies with it.
    async fn agent(&self) -> (zyris::Connection, zyris::Node) {
        let node = self
            .live
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
            .expect("a node with nothing of its own");
        let (ours, _theirs) =
            zyris::testing::duplex(&peer, &node).await.expect("the two nodes connect");
        (ours, node)
    }

    /// What the peer can see, waited for rather than read once — an announcement crosses a wire.
    ///
    /// `promoted` is what should be there **in addition to** this machine's own, so this machine's
    /// own capabilities are asserted unchanged in every test that calls it.
    async fn peer_sees(&self, agent: &zyris::Connection, promoted: &[&str]) {
        let mut want: Vec<String> = self.builtin.clone();
        want.extend(promoted.iter().map(|name| (*name).to_string()));
        want.sort();

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let mut names: Vec<String> =
                agent.peer_descriptors().into_iter().map(|d| d.name).collect();
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
}

/// One capability as the agent on the other end reads it.
fn as_the_agent_sees_it(agent: &zyris::Connection, name: &str) -> CapabilityDescriptor {
    agent
        .peer_descriptors()
        .into_iter()
        .find(|descriptor| descriptor.name == name)
        .unwrap_or_else(|| panic!("`{name}` is not announced to the peer"))
}

fn tool_names(descriptor: &CapabilityDescriptor) -> Vec<&str> {
    descriptor.tools.iter().map(|tool| tool.name.as_str()).collect()
}

/// A server list naming the probe server, as a person would write one.
///
/// `enabled` is deliberately left out: absent means yes, and a test that spelled it would never
/// exercise the default the file actually relies on.
fn server_list(servers: &[(&str, &[&str])]) -> String {
    let entries: Vec<serde_json::Value> = servers
        .iter()
        .map(|(name, args)| json!({ "name": name, "command": probe_server(), "args": args }))
        .collect();
    serde_json::to_string_pretty(&json!({ "servers": entries })).expect("a server list")
}

#[tokio::test]
async fn a_tool_on_a_local_mcp_server_is_announced_and_answers_the_agent_that_calls_it() {
    // The whole claim in one test: a file, a process, an announcement, a call, an answer.
    let machine = Machine::started_from(&server_list(&[("desk-notes", &[])])).await;
    let (agent, _node) = machine.agent().await;

    machine.peer_sees(&agent, &["mcp_desk-notes"]).await;

    // What the agent reads is the server's own tool list — **both pages of it**. `tools/list` is
    // paginated and a client that stopped at page one would announce `echo` and `add` and lose
    // `explode` and `die`, which is a shape no assertion about a single tool can see.
    let promoted = as_the_agent_sees_it(&agent, "mcp_desk-notes");
    assert_eq!(tool_names(&promoted), ["echo", "add", "explode", "die"]);
    assert_eq!(promoted.version, zyris_mcp::PROMOTED_VERSION);

    let echo = promoted.tool("echo").expect("`echo` is announced");
    assert_eq!(echo.description, "Return the text it is given.");
    assert_eq!(echo.transfer, Transfer::Unary);
    // The server's schema, carried through rather than invented here: an agent that means to call
    // this has to learn the argument's name from the announcement.
    assert_eq!(echo.request_schema["properties"]["text"]["type"], "string");
    assert_eq!(echo.request_schema["required"], json!(["text"]));

    // The answer, compared against what only the server could have said.
    let answer = agent
        .call_raw("mcp_desk-notes.echo", Payload::from_json(json!({ "text": "over the wire" })))
        .await
        .expect("the promoted tool answers");
    assert_eq!(answer.to_json().unwrap()["content"][0]["text"], "over the wire");

    // A second tool, whose answer this test cannot have produced for it: the arguments have to
    // reach the process and the arithmetic has to come back. An echo alone would be satisfied by
    // a capability that returned its own input.
    let sum = agent
        .call_raw("mcp_desk-notes.add", Payload::from_json(json!({ "a": 19.5, "b": 22.5 })))
        .await
        .expect("`add` answers");
    assert_eq!(sum.to_json().unwrap()["structuredContent"]["sum"], 42.0);
    assert_eq!(sum.to_json().unwrap()["content"][0]["text"], "42");
}

#[tokio::test]
async fn a_promoted_tool_and_a_built_in_one_look_the_same_to_the_agent() {
    // The README's sentence, as an assertion. An agent has one way to learn what a machine offers
    // — the descriptors — and one way to use it — a call — and a promoted capability has to be
    // complete in both. A tool announced with no description, or with a schema an agent cannot
    // read, is one it will not call even though it is listed.
    let machine = Machine::started_from(&server_list(&[("desk-notes", &[])])).await;
    let (agent, _node) = machine.agent().await;
    machine.peer_sees(&agent, &["mcp_desk-notes"]).await;

    for name in ["file_io", "mcp_desk-notes"] {
        let descriptor = as_the_agent_sees_it(&agent, name);
        assert!(descriptor.version >= 1, "{name} announces no version");
        assert!(!descriptor.tools.is_empty(), "{name} announces no tools");
        for tool in &descriptor.tools {
            assert!(
                !tool.description.trim().is_empty(),
                "{name}.{} is announced with nothing said about it",
                tool.name
            );
            assert!(
                tool.request_schema.is_object(),
                "{name}.{} announces a schema an agent cannot read: {}",
                tool.name,
                tool.request_schema
            );
        }
    }

    // And both are reached the same way, on the same connection, with the same call.
    let promoted = agent
        .call_raw("mcp_desk-notes.echo", Payload::from_json(json!({ "text": "hello" })))
        .await
        .expect("the promoted tool answers");
    assert_eq!(promoted.to_json().unwrap()["content"][0]["text"], "hello");
    let built_in = agent
        .call_raw("file_io.stat", Payload::from_json(json!({ "path": "mcp-servers.json" })))
        .await
        .expect("the built-in answers");
    assert_eq!(built_in.to_json().unwrap()["is_dir"], false);
}

#[tokio::test]
async fn the_pause_switch_refuses_a_promoted_call_at_the_node() {
    // **The claim the README makes and that the layers were only tested apart.** The capability
    // behind this one was announced at startup, through `Tools::into_capabilities` — a different
    // line of code from the one a server enabled through the window goes through, which
    // `servers_come_and_go.rs` covers. A promoted capability that reached the node unguarded
    // would work perfectly until somebody hit pause and a third party's process kept being
    // called.
    let machine = Machine::started_from(&server_list(&[("desk-notes", &[])])).await;
    let (agent, _node) = machine.agent().await;
    machine.peer_sees(&agent, &["mcp_desk-notes"]).await;

    machine.gate.set_paused(true);

    let refused = agent
        .call_raw("mcp_desk-notes.echo", Payload::from_json(json!({ "text": "while paused" })))
        .await
        .expect_err("a paused machine does not reach somebody else's process");
    // The code alone would not settle it — a dead server answers with the same one — so the
    // switch's own wording is what says which of the two refused this.
    assert_eq!(refused.code, ErrorCode::CapabilityUnavailable);
    assert!(
        refused.message.contains("paused"),
        "the switch refused it, not the server: {}",
        refused.message
    );
    // The same refusal a built-in gets, word for word: an agent that could tell the two apart
    // could tell a promoted tool from a built-in one.
    let built_in = agent
        .call_raw("file_io.stat", Payload::from_json(json!({ "path": "mcp-servers.json" })))
        .await
        .expect_err("the built-ins are paused too");
    assert_eq!(built_in.code, refused.code);
    assert_eq!(built_in.message, refused.message);

    machine.gate.set_paused(false);

    let answer = agent
        .call_raw("mcp_desk-notes.echo", Payload::from_json(json!({ "text": "and again" })))
        .await
        .expect("unpausing lets it through");
    assert_eq!(answer.to_json().unwrap()["content"][0]["text"], "and again");
}

#[tokio::test]
async fn the_audit_log_records_a_promoted_call_and_not_what_was_asked_of_it() {
    // Both halves, because either alone is satisfied by a broken log. A line has to be written —
    // a promoted call is a call and the file is the record of what ran on this machine — and its
    // arguments must not be in it. The arguments here are named `path` and `command` on purpose:
    // those two spellings are on `summarize`'s allowlist, so a `Guarded` that treated a promoted
    // capability like a built-in would write a third party's tool arguments into the file, and
    // nothing about the log's shape would look wrong.
    const SECRET: &str = "hunter2-not-in-the-log";
    let machine = Machine::started_from(&server_list(&[("desk-notes", &[])])).await;
    let (agent, _node) = machine.agent().await;
    machine.peer_sees(&agent, &["mcp_desk-notes"]).await;

    agent
        .call_raw(
            "mcp_desk-notes.echo",
            Payload::from_json(json!({
                "text": SECRET,
                "path": "/etc/shadow",
                "command": "rm -rf /",
            })),
        )
        .await
        .expect("the server takes what it is given");
    agent
        .call_raw("file_io.stat", Payload::from_json(json!({ "path": "mcp-servers.json" })))
        .await
        .expect("a built-in call, for the control below");

    let entries = machine.log.recent(10).expect("the audit log reads");
    let promoted = entries
        .iter()
        .find(|entry| entry.capability == "mcp_desk-notes")
        .expect("the promoted call was recorded at all");
    assert_eq!(promoted.tool, "echo");
    assert_eq!(promoted.outcome, Outcome::Allowed);
    assert_eq!(promoted.detail, "", "a promoted call writes no argument detail: {promoted:?}");

    // The control. Without it, a log that had simply stopped writing details would pass the
    // assertion above and the file would say nothing about anything.
    let built_in = entries
        .iter()
        .find(|entry| entry.capability == "file_io")
        .expect("the built-in call was recorded");
    assert_eq!(built_in.detail, "path=mcp-servers.json");

    // And the file itself, which is the thing that actually has to be handable to somebody.
    let written = std::fs::read_to_string(machine.log.path()).expect("the log is on disk");
    for absent in [SECRET, "/etc/shadow", "rm -rf /"] {
        assert!(
            !written.contains(absent),
            "`{absent}` reached the audit file:\n{written}"
        );
    }
}

#[tokio::test]
async fn a_server_named_after_a_built_in_is_announced_beside_it_rather_than_over_it() {
    // Arranged rather than argued, and end to end rather than against a descriptor: a server
    // actually called `terminal`, in a file, promoted, announced on a live connection next to the
    // real one. What a collision would cost is not a shadowed tool but the whole machine —
    // `Served::build` refuses a duplicate name and the node then announces nothing at all.
    let machine = Machine::started_from(&server_list(&[("terminal", &[])])).await;
    let (agent, _node) = machine.agent().await;

    machine.peer_sees(&agent, &["mcp_terminal"]).await;

    let built_in = as_the_agent_sees_it(&agent, "terminal");
    assert!(tool_names(&built_in).contains(&"exec"), "{:?}", tool_names(&built_in));
    let promoted = as_the_agent_sees_it(&agent, "mcp_terminal");
    assert_eq!(tool_names(&promoted), ["echo", "add", "explode", "die"]);

    // Which name reaches which, decided by what answers rather than by what is listed.
    let answer = agent
        .call_raw("mcp_terminal.echo", Payload::from_json(json!({ "text": "the mcp one" })))
        .await
        .expect("the promoted server answers under its prefixed name");
    assert_eq!(answer.to_json().unwrap()["content"][0]["text"], "the mcp one");
    let missing = agent
        .call_raw("terminal.echo", Payload::from_json(json!({ "text": "the built-in one" })))
        .await
        .expect_err("the built-in terminal has no `echo`, so this name is still the real one");
    assert_eq!(missing.code, ErrorCode::MethodNotFound);
}

#[tokio::test]
async fn a_server_list_that_will_not_parse_leaves_everything_else_announced_and_callable() {
    // `"commnad"` rather than a mangled brace: the mistake this file actually collects is a
    // spelling one, and `deny_unknown_fields` is what makes it loud instead of leaving a person
    // with a server that starts with an empty command.
    let machine = Machine::started_from(
        r#"{ "servers": [ { "name": "desk-notes", "commnad": "/bin/true" } ] }"#,
    )
    .await;
    let (agent, _node) = machine.agent().await;

    machine.peer_sees(&agent, &[]).await;
    let announced: Vec<String> = agent.peer_descriptors().into_iter().map(|d| d.name).collect();
    assert!(
        announced.iter().all(|name| !name.starts_with(CAPABILITY_PREFIX)),
        "a file that could not be read announced an MCP server anyway: {announced:?}"
    );
    // Not merely announced: this machine still works.
    let stat = agent
        .call_raw("file_io.stat", Payload::from_json(json!({ "path": "mcp-servers.json" })))
        .await
        .expect("the built-ins are untouched by a file they have nothing to do with");
    assert_eq!(stat.to_json().unwrap()["is_dir"], false);

    // And the window is told why, rather than being handed an empty list to render as "you have
    // configured none".
    let view = machine.servers.view().await;
    assert!(view.servers.is_empty());
    let problem = view.problem.expect("an unreadable file is not the same answer as no file");
    assert!(
        problem.contains("commnad"),
        "the problem has to name what is wrong with the file: {problem}"
    );
    assert_eq!(view.path, zyris_mcp::Config::path(machine.dir.path()).display().to_string());
    assert!(
        machine.tools.announced().iter().all(|c| !c.name.starts_with(CAPABILITY_PREFIX)),
        "the Tools screen would list an MCP capability this machine never started"
    );
}
