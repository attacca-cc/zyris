//! The servers a person listed on disk, started and promoted.
//!
//! Same rule as `one_server.rs` and `promoted_tools.rs`: every server here is a real process at
//! the end of a real pipe. What is under test is the step between a configuration file and a
//! capability, and every one of its failures — a file that is not there, a file that is not JSON,
//! a command that is not there, a name two entries share — is a thing that happens to somebody's
//! machine rather than a thing a fixture can be asked to pretend.
//!
//! **None of these may end in Zyris refusing to start.** That is the whole point of the task: the
//! MCP server list is the one piece of this machine's configuration a person edits by hand, and a
//! typo in it must cost them their MCP servers and nothing else.

use std::path::{Path, PathBuf};

use zyris::ServeCapability;
use zyris_mcp::config;

/// Where `cargo test` leaves the `probe_server` example. See `one_server.rs` for why this is
/// derived: there is no `CARGO_BIN_EXE_` for examples, and `cargo test --test <name>` does not
/// build them.
fn probe_server() -> String {
    let mut directory = std::env::current_exe().expect("the test binary knows its own path");
    directory.pop();
    if directory.ends_with("deps") {
        directory.pop();
    }
    let path: PathBuf = directory
        .join("examples")
        .join(format!("probe_server{}", std::env::consts::EXE_SUFFIX));
    assert!(
        path.is_file(),
        "the `probe_server` example is not at {}. `cargo test` builds examples; a narrower \
         selection such as `cargo test --test configured_servers` does not. Run `cargo test -p \
         zyris-mcp`.",
        path.display()
    );
    path.into_os_string()
        .into_string()
        .expect("a path cargo produced is UTF-8")
}

/// A command that is not there, spelled so that no machine could accidentally have one.
const MISSING_COMMAND: &str = "zyris-no-such-mcp-server-anywhere-on-this-machine";

/// Whatever a person might have left in the file, byte for byte — including things that are not
/// JSON at all.
fn write_text(dir: &Path, text: &str) {
    std::fs::write(config::Config::path(dir), text).expect("writing a config into a temp dir");
}

/// A well-formed file listing these servers.
fn write_config(dir: &Path, servers: Vec<serde_json::Value>) {
    write_text(dir, &serde_json::json!({ "servers": servers }).to_string());
}

/// One entry, as a person would write it.
fn entry(name: &str, command: &str) -> serde_json::Value {
    serde_json::json!({ "name": name, "command": command })
}

fn names(promoted: &[std::sync::Arc<zyris_mcp::Promoted>]) -> Vec<String> {
    promoted.iter().map(|p| p.name().to_string()).collect()
}

#[tokio::test]
async fn a_configured_server_is_started_and_promoted() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), vec![entry("desk-notes", &probe_server())]);

    let promoted = config::start(dir.path()).await;

    assert_eq!(names(&promoted), ["mcp_desk-notes"]);
    // Started and actually asked, rather than announced from the file: the tool names could only
    // have come from the process.
    let tools: Vec<String> = promoted[0]
        .descriptor()
        .tools
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    assert_eq!(tools, ["echo", "add", "explode", "die"]);
}

#[tokio::test]
async fn arguments_reach_the_command() {
    // Without this, a `start` that dropped `args` would pass every other test here: the probe
    // server starts either way. `--odd-tools` is the flag whose effect is visible in the tool
    // list, so the assertion is about what the process did rather than about what was configured.
    let dir = tempfile::tempdir().unwrap();
    let mut server = entry("desk-notes", &probe_server());
    server["args"] = serde_json::json!(["--odd-tools"]);
    write_config(dir.path(), vec![server]);

    let promoted = config::start(dir.path()).await;

    let tools: Vec<String> = promoted[0]
        .descriptor()
        .tools
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    assert_eq!(tools, ["search", "untitled", "titled", "anything", "nested.tool"]);
}

#[tokio::test]
async fn a_server_that_will_not_start_is_absent_and_the_others_are_not() {
    // The operator error this file exists for. A command that is not there must cost its own
    // server and nothing else — an agent that loses every MCP tool because one line is wrong has
    // no way to find out which line.
    let dir = tempfile::tempdir().unwrap();
    write_config(
        dir.path(),
        vec![entry("gone", MISSING_COMMAND), entry("desk-notes", &probe_server())],
    );

    let promoted = config::start(dir.path()).await;

    assert_eq!(names(&promoted), ["mcp_desk-notes"]);
}

#[tokio::test]
async fn a_server_whose_name_will_not_route_is_absent_and_the_others_are_not() {
    // A dot makes a capability nothing can address — `capability.tool` splits at the first one —
    // so this server cannot be announced whatever its process does. The other entry is the
    // assertion that matters: one unroutable name is not the whole file's problem.
    let dir = tempfile::tempdir().unwrap();
    let probe = probe_server();
    write_config(dir.path(), vec![entry("my.notes", &probe), entry("desk-notes", &probe)]);

    let promoted = config::start(dir.path()).await;

    assert_eq!(names(&promoted), ["mcp_desk-notes"]);
}

#[tokio::test]
async fn two_servers_with_one_name_start_nothing_at_all() {
    // `zyris-core`'s `Served::build` refuses a duplicate `(name, version)` and a node that trips
    // it announces *nothing* — not one MCP tool, and not `terminal` or `file_io` either. So the
    // duplicate is caught here, where the file is read and a person can be told which name to
    // change, and the whole file is refused rather than one of the two entries being picked.
    let dir = tempfile::tempdir().unwrap();
    let probe = probe_server();
    write_config(
        dir.path(),
        vec![entry("notes", &probe), entry("notes", &probe), entry("calendar", &probe)],
    );

    let promoted = config::start(dir.path()).await;

    assert!(promoted.is_empty(), "an ambiguous file was half-obeyed: {:?}", names(&promoted));
}

#[tokio::test]
async fn a_config_that_is_not_json_starts_nothing_and_does_not_fail() {
    let dir = tempfile::tempdir().unwrap();
    write_text(dir.path(), "{ \"servers\": [ oh dear");

    assert!(config::start(dir.path()).await.is_empty());
}

#[tokio::test]
async fn a_config_of_the_wrong_shape_starts_nothing_and_does_not_fail() {
    // Valid JSON, and nothing this code can act on. The distinction matters because the two are
    // different mistakes and the log line has to be able to say which.
    let dir = tempfile::tempdir().unwrap();
    write_text(dir.path(), r#"{ "servers": { "notes": { "command": "x" } } }"#);

    assert!(config::start(dir.path()).await.is_empty());
}

#[tokio::test]
async fn no_config_file_at_all_is_no_servers_and_no_failure() {
    // The ordinary state of a machine nobody has configured, which is most of them.
    let dir = tempfile::tempdir().unwrap();

    assert!(config::start(dir.path()).await.is_empty());
}

#[tokio::test]
async fn a_disabled_server_is_not_started() {
    let dir = tempfile::tempdir().unwrap();
    let mut server = entry("desk-notes", &probe_server());
    server["enabled"] = serde_json::json!(false);
    write_config(dir.path(), vec![server]);

    assert!(config::start(dir.path()).await.is_empty());
}
