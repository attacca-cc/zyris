//! One MCP server, spawned and asked what it has.
//!
//! Everything here talks to a real process over a real pipe — `tests/support/probe_server.rs`,
//! built as the `probe_server` example. Nothing in `rmcp`'s client is stubbed, because the
//! question these tests answer is whether this crate and a server agree, and a stub only agrees
//! with whatever was believed when it was written.

use std::path::PathBuf;
use std::time::Duration;

use rmcp::service::ServiceError;
use serde_json::json;
use zyris_mcp::{STARTUP_DEADLINE, Server};

/// Where `cargo test` leaves the `probe_server` example.
///
/// The test binary is `target/<profile>/deps/<name>-<hash>`, and an example built alongside it is
/// `target/<profile>/examples/<name>`. There is no `CARGO_BIN_EXE_` for examples, so this is
/// derived rather than handed over — and asserted, because a missing file here has one cause
/// (a target selection narrow enough that cargo skipped examples, e.g. `cargo test --test
/// one_server`) and the assertion says so rather than letting a spawn failure imply the code is
/// broken.
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
         selection such as `cargo test --test one_server` does not. Run `cargo test -p zyris-mcp`.",
        path.display()
    );
    path.into_os_string()
        .into_string()
        .expect("a path cargo produced is UTF-8")
}

/// Deliberately nothing like the binary, the crate or the tool names: a `name()` that hardcoded
/// any of those would still satisfy a test that asserted a plausible one.
const SERVER_NAME: &str = "desk-notes";

async fn probe(args: &[&str]) -> Server {
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    Server::spawn(SERVER_NAME, &probe_server(), &args)
        .await
        .expect("the probe server starts")
}

#[tokio::test]
async fn a_server_reports_the_tools_it_has() {
    let server = probe(&[]).await;

    assert_eq!(server.name(), SERVER_NAME);
    // All four, and the last two are on the second page of `tools/list`: a client that stops at
    // the first response gets `["echo", "add"]` and nothing says so.
    let names: Vec<&str> = server.tools().iter().map(|t| t.name.as_ref()).collect();
    assert_eq!(names, ["echo", "add", "explode", "die"]);

    // The list is not just names: what makes a tool promotable is its schema, and it has to come
    // through the way the server wrote it.
    let echo = &server.tools()[0];
    assert_eq!(echo.description.as_deref(), Some("Return the text it is given."));
    assert_eq!(
        echo.schema_as_json_value(),
        json!({
            "type": "object",
            "properties": { "text": { "type": "string" } },
            "required": ["text"],
        })
    );

    // And a server that declares an output schema keeps it; `add` does, `echo` does not.
    assert!(server.tools()[1].output_schema.is_some());
    assert!(echo.output_schema.is_none());
}

#[tokio::test]
async fn a_tool_call_reaches_the_server_and_the_answer_comes_back() {
    let server = probe(&[]).await;

    // A value only this call could have produced, so a fixture or a cached answer cannot pass.
    let answer = server
        .call("echo", json!({ "text": "the quick brown fox" }))
        .await
        .expect("`echo` answers");
    assert_eq!(answer["content"][0]["type"], "text");
    assert_eq!(answer["content"][0]["text"], "the quick brown fox");

    // Arithmetic the test does not do itself, so the answer has to have come from the server.
    let answer = server
        .call("add", json!({ "a": 17, "b": 25 }))
        .await
        .expect("`add` answers");
    assert_eq!(answer["structuredContent"]["sum"], 42.0);
    assert_eq!(answer["content"][0]["text"], "42");

    // A tool that ran and failed is a successful call whose result says so. Flattening that into
    // an `Err` would lose the reason, so `call` reports it.
    let answer = server
        .call("explode", json!({}))
        .await
        .expect("a failing tool still answers");
    assert_eq!(answer["isError"], true);
    assert_eq!(answer["content"][0]["text"], "the tool refused");

    // A tool the server does not have is a protocol error, not a result. The server's own words
    // have to reach the caller — "no tool" is the probe server's phrasing, not this crate's — or
    // the only thing anyone learns is that some call failed.
    let missing = server
        .call("nonexistent", json!({}))
        .await
        .expect_err("a tool the server does not have is an error");
    let chain = format!("{missing:#}");
    assert!(
        chain.contains("nonexistent") && chain.contains("no tool"),
        "the server's own reason should survive: {chain}"
    );

    // And it is not a disconnection. Task 4 has to withdraw a capability when a server dies and
    // leave it alone when a tool merely refuses, so the two must not answer the same question the
    // same way.
    assert!(
        !missing
            .downcast_ref::<ServiceError>()
            .is_some_and(zyris_mcp::is_disconnected),
        "a refused tool is not a dead server: {chain}"
    );
}

#[tokio::test]
async fn a_command_that_does_not_exist_fails_rather_than_hanging() {
    // The common operator error. A spawn that hangs is worse than one that fails: nothing
    // downstream can tell "starting" from "never going to start".
    let start = std::time::Instant::now();
    let outcome = tokio::time::timeout(
        STARTUP_DEADLINE,
        Server::spawn("ghost", "zyris-no-such-command-exists", &[]),
    )
    .await
    .expect("spawning a missing command returns instead of waiting");

    let error = outcome.expect_err("a missing command is an error");
    assert!(
        error.to_string().contains("zyris-no-such-command-exists"),
        "the error should name the command: {error:#}"
    );
    // This one never reaches the handshake, so it must fail at once rather than sit out the
    // deadline. A second is a very long time for a failed `execve`.
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "a missing command should fail immediately, took {:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn a_server_that_starts_but_never_speaks_gives_up_rather_than_hanging() {
    // The hang the missing-command case cannot produce, and the one that actually happens: the
    // wrong binary, or a wrapper that prints its usage to stderr and waits on stdin. Nothing in
    // `rmcp` bounds the handshake, so this is `STARTUP_DEADLINE`'s only job.
    let start = std::time::Instant::now();
    let outcome = tokio::time::timeout(
        STARTUP_DEADLINE * 3,
        Server::spawn("mute", &probe_server(), &["--mute".to_owned()]),
    )
    .await
    .expect("a silent server is given up on, not waited for");

    let error = outcome.expect_err("a silent server is an error");
    assert!(
        error.to_string().contains("handshake"),
        "the error should say what was being waited for: {error:#}"
    );
    // Not before the deadline either: giving up early would break the slow-but-fine server that
    // the ten seconds exist for.
    assert!(
        start.elapsed() >= STARTUP_DEADLINE,
        "gave up after {:?}, before the {STARTUP_DEADLINE:?} deadline",
        start.elapsed()
    );
    // The assertion above is relative, so on its own it would stay green if somebody shortened
    // the deadline to make this test quick. The waiting is the point: a real server behind a cold
    // `npx` takes seconds, and a deadline tuned for a test suite would refuse it.
    assert!(
        STARTUP_DEADLINE >= Duration::from_secs(5),
        "{STARTUP_DEADLINE:?} is too short to let a real server start"
    );
}

#[tokio::test]
async fn a_server_that_dies_is_noticed() {
    let server = probe(&[]).await;

    // It answers first, so the failure below cannot be "it never worked".
    server
        .call("echo", json!({ "text": "still here" }))
        .await
        .expect("`echo` answers while the server lives");

    // `die` exits the process with the call still in flight. How that surfaces was one of the
    // plan's stated unknowns; it is `ServiceError::TransportClosed`, raised when `rmcp`'s service
    // loop reads end-of-file on the child's stdout and drops the pending responder.
    let died = tokio::time::timeout(Duration::from_secs(10), server.call("die", json!({})))
        .await
        .expect("a death is noticed rather than waited out")
        .expect_err("a call the server dies during is an error");

    let service_error = died
        .downcast_ref::<ServiceError>()
        .unwrap_or_else(|| panic!("a death should surface as an rmcp ServiceError: {died:#?}"));
    assert!(
        matches!(service_error, ServiceError::TransportClosed),
        "expected TransportClosed, got {service_error:?}"
    );
    assert!(
        zyris_mcp::is_disconnected(service_error),
        "a death has to be distinguishable from a tool that refused"
    );

    // And it stays noticed: the next call fails too, rather than hanging on a process that is not
    // there. This is what stops a withdrawn server looking like a slow one.
    let again = tokio::time::timeout(
        Duration::from_secs(10),
        server.call("echo", json!({ "text": "anybody there" })),
    )
    .await
    .expect("a call to a dead server returns")
    .expect_err("a call to a dead server is an error");
    assert!(
        again
            .downcast_ref::<ServiceError>()
            .is_some_and(|e| matches!(e, ServiceError::TransportClosed)),
        "expected TransportClosed on the second call, got {again:#}"
    );
}

#[tokio::test]
async fn a_server_that_is_answering_reports_itself_running() {
    let server = probe(&[]).await;

    assert!(server.is_running(), "a server that just started is running");
    server.call("echo", json!({ "text": "still here" })).await.expect("`echo` answers");
    assert!(server.is_running(), "a server that just answered is running");

    // Idle is not dead. Nothing is asked of it for a moment, which is the ordinary state of an
    // MCP server on a desktop, and a check that reported death for quiet would withdraw every
    // server on the machine within a second of it announcing them.
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(server.is_running(), "an idle server is not a dead one");
}

#[tokio::test]
async fn a_server_that_falls_over_with_nobody_asking_is_noticed_without_a_call() {
    // **The death the whole health check exists for**, and a different one from
    // `a_server_that_dies_is_noticed` above. There, `die` interrupts a call, so any client that
    // asked was told. Here the process falls over while the machine is idle — a crash, a parent
    // that killed it, an out-of-memory — and nothing asks it anything before or after. A signal
    // that only arrives on the next call cannot see this, and this is exactly the state that
    // leaves a capability announced with nothing behind it.
    let server = probe(&["--exit-after", "200"]).await;
    assert!(server.is_running(), "it is running before it falls over");

    let start = std::time::Instant::now();
    let mut noticed = None;
    while start.elapsed() < Duration::from_secs(10) {
        if !server.is_running() {
            noticed = Some(start.elapsed());
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let noticed = noticed.expect("a server that fell over has to stop reporting itself running");

    // Not merely eventually. Withdrawal is driven off this, and a signal that arrived seconds
    // after the process did leave a capability announced over nothing for that long.
    assert!(
        noticed < Duration::from_secs(1),
        "the death took {noticed:?} to show up, with the process gone after 200ms"
    );

    // And it stays noticed rather than flickering back.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!server.is_running());
}
