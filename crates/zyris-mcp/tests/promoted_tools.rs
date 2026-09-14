//! A real MCP server's tool list, promoted to something an agent on Attacca can call.
//!
//! Same rule as `one_server.rs`: every one of these talks to `tests/support/probe_server.rs` over
//! a real pipe. What is under test is a translation between two protocols, and a fixture standing
//! in for either side would only ever agree with what was believed while it was written.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};
use zyris::{ErrorCode, IncomingCall, Outgoing, Payload, ServeCapability, Serialization, Transfer};
use zyris_mcp::{CAPABILITY_PREFIX, Promoted, Server};

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
         selection such as `cargo test --test promoted_tools` does not. Run `cargo test -p \
         zyris-mcp`.",
        path.display()
    );
    path.into_os_string()
        .into_string()
        .expect("a path cargo produced is UTF-8")
}

/// Deliberately nothing like the binary, the crate or any tool name, and it contains a hyphen so
/// that a capability name assembled with anything other than plain concatenation shows up.
const SERVER_NAME: &str = "desk-notes";

/// What `SERVER_NAME` has to become. Spelled out rather than built from `CAPABILITY_PREFIX`, so a
/// change to the prefix fails here instead of quietly agreeing with itself.
const CAPABILITY: &str = "mcp_desk-notes";

/// The five the protocol stack announces from this machine, from `zyris-tools`'s `announce.rs`.
const BUILT_IN: &[&str] = &[
    "terminal",
    "file_io",
    "input",
    "screen_capture",
    "file_transfer",
];

async fn promoted(name: &str, args: &[&str]) -> Promoted {
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    let server = Server::spawn(name, &probe_server(), &args)
        .await
        .expect("the probe server starts");
    Promoted::new(Arc::new(server)).expect("the probe server's name promotes")
}

fn call(tool: &str, params: Value) -> IncomingCall {
    IncomingCall {
        tool: tool.to_string(),
        params: Payload::from_json(params),
        serialization: Serialization::Json,
        meta: Payload::default(),
    }
}

/// The answer an agent would see, as JSON.
fn answer(out: Outgoing) -> Value {
    match out {
        Outgoing::Response(payload) => payload.to_json().expect("the answer decodes"),
        Outgoing::Stream { .. } => {
            panic!("nothing about MCP streams; a promoted tool must answer with a response")
        }
    }
}

/// The error an agent would see.
///
/// Written out rather than reached with `expect_err`, because `Outgoing` is the protocol's type
/// and does not implement `Debug` — deliberately, since a response payload is exactly the thing
/// that must not end up in a panic message.
fn refusal(out: zyris::Result<Outgoing>, why: &str) -> zyris::WireError {
    match out {
        Ok(ok) => panic!("{why}, but the call succeeded with {:?}", answer(ok)),
        Err(error) => error,
    }
}

#[tokio::test]
async fn a_tool_list_becomes_a_descriptor_an_agent_can_read() {
    let promoted = promoted(SERVER_NAME, &[]).await;
    let descriptor = promoted.descriptor();

    assert_eq!(descriptor.name, CAPABILITY);
    assert_eq!(descriptor.version, 1);

    // Every tool, in the order the server listed them, including the two that are only reachable
    // by following `tools/list`'s cursor.
    let names: Vec<&str> = descriptor.tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["echo", "add", "explode", "die"]);

    let echo = descriptor.tool("echo").expect("`echo` is announced");
    assert_eq!(echo.description, "Return the text it is given.");
    // The server's schema, as the server wrote it. This is the whole reason an MCP tool can be
    // announced at all, so it is compared in full rather than probed field by field.
    assert_eq!(
        echo.request_schema,
        json!({
            "type": "object",
            "properties": { "text": { "type": "string" } },
            "required": ["text"],
        })
    );

    for tool in &descriptor.tools {
        // Nothing in MCP streams — `tools/call` is one request and one answer — so every promoted
        // tool is unary, and none of them declares a per-item schema.
        assert_eq!(tool.transfer, Transfer::Unary, "{} is not unary", tool.name);
        assert!(tool.item_schema.is_none(), "{} declares an item schema", tool.name);

        // The answer is an MCP result envelope whatever the tool is, and the agent has to be told
        // where a refusal lives or it cannot tell one from a success.
        let response = tool
            .response_schema
            .as_ref()
            .unwrap_or_else(|| panic!("{} declares no response schema", tool.name));
        assert!(
            response["properties"]["isError"].is_object(),
            "{}'s response schema does not mention isError: {response}",
            tool.name
        );
        assert!(
            response["properties"]["content"].is_object(),
            "{}'s response schema does not mention content: {response}",
            tool.name
        );
    }

    // `add` is the only tool with an `outputSchema`, and MCP's `outputSchema` describes
    // `structuredContent` — not the envelope around it. So it has to arrive nested, not in place
    // of the envelope, or an agent validating an answer against it would reject every one.
    let add = descriptor.tool("add").expect("`add` is announced");
    assert_eq!(
        add.response_schema.as_ref().expect("`add` has a response schema")
            ["properties"]["structuredContent"],
        json!({
            "type": "object",
            "properties": { "sum": { "type": "number" } },
            "required": ["sum"],
        })
    );
    // And a tool with no output schema still gets the envelope, with nothing claimed about the
    // structured half rather than a constraint invented here.
    assert_eq!(
        echo.response_schema.as_ref().expect("`echo` has a response schema")
            ["properties"]["structuredContent"],
        json!({})
    );
}

#[tokio::test]
async fn a_promoted_capability_cannot_take_a_built_in_name() {
    // The collision the plan singles out, arranged rather than argued: a server actually called
    // `terminal`, spawned and promoted.
    let named_terminal = promoted("terminal", &[]).await;
    assert_eq!(named_terminal.descriptor().name, "mcp_terminal");

    for built_in in BUILT_IN {
        assert_ne!(
            named_terminal.descriptor().name,
            *built_in,
            "a promoted capability took the built-in name `{built_in}`"
        );
        // Not just this one server: the rule is structural, and what makes it structural is that
        // no built-in name starts with the prefix. A sixth built-in called `mcp_anything` would
        // break that, and this is where it would be caught.
        assert!(
            !built_in.starts_with(CAPABILITY_PREFIX),
            "the built-in `{built_in}` starts with `{CAPABILITY_PREFIX}`, so the prefix no \
             longer keeps promoted names out of the built-ins' space"
        );
    }

    // A server that has already guessed the scheme does not land on another server's name
    // either: prefixing is plain concatenation, so it is injective, and two different servers can
    // never produce one capability.
    let sneaky = promoted("mcp_terminal", &[]).await;
    assert_eq!(sneaky.descriptor().name, "mcp_mcp_terminal");
    assert_ne!(sneaky.descriptor().name, named_terminal.descriptor().name);
}

#[tokio::test]
async fn a_dispatch_reaches_the_tool_it_names() {
    let promoted = promoted(SERVER_NAME, &[]).await;

    // A string only this call could have produced.
    let out = promoted
        .dispatch(call("echo", json!({ "text": "the quick brown fox" })))
        .await
        .expect("`echo` answers");
    let out = answer(out);
    assert_eq!(out["content"][0]["text"], "the quick brown fox");
    assert!(out["isError"].is_null(), "a tool that worked must not be flagged: {out}");

    // A different tool, reached by name rather than by position — arithmetic this test does not
    // do itself, so the answer has to have come from the server.
    let out = answer(
        promoted
            .dispatch(call("add", json!({ "a": 17, "b": 25 })))
            .await
            .expect("`add` answers"),
    );
    assert_eq!(out["structuredContent"]["sum"], 42.0);
}

#[tokio::test]
async fn a_call_for_a_tool_the_server_does_not_have_is_an_error_not_a_panic() {
    let promoted = promoted(SERVER_NAME, &[]).await;

    let error = refusal(
        promoted.dispatch(call("nonexistent", json!({}))).await,
        "a tool that is not announced is an error",
    );

    assert_eq!(error.code, ErrorCode::MethodNotFound);
    // The method as an agent spelled it, so the line says which capability as well as which tool.
    assert!(
        error.message.contains(CAPABILITY) && error.message.contains("nonexistent"),
        "the error should name the method: {error}"
    );
    assert!(!error.retriable, "a tool that does not exist will not start existing: {error}");
}

#[tokio::test]
async fn a_tool_that_ran_and_refused_is_an_answer_rather_than_an_error() {
    let promoted = promoted(SERVER_NAME, &[]).await;

    // MCP draws the line between a tool that ran and failed — a successful response saying so —
    // and a call that never happened. Keeping it means the agent gets the reason.
    let out = answer(
        promoted
            .dispatch(call("explode", json!({})))
            .await
            .expect("a refusal is still an answer"),
    );
    assert_eq!(out["isError"], true);
    assert_eq!(out["content"][0]["text"], "the tool refused");
}

#[tokio::test]
async fn the_three_ways_a_call_can_end_are_told_apart() {
    // The requirement in one place: an agent must be able to tell "this tool ran and refused"
    // from "this tool does not exist" from "this server is gone". Each is a different shape, so
    // no string parsing is needed to separate them.
    let promoted = promoted(SERVER_NAME, &[]).await;

    let refused = promoted.dispatch(call("explode", json!({}))).await;
    assert!(refused.is_ok(), "a refusal is a response");

    let missing = refusal(
        promoted.dispatch(call("nonexistent", json!({}))).await,
        "a missing tool is an error",
    );
    assert_eq!(missing.code, ErrorCode::MethodNotFound);

    // `die` exits the process mid-request.
    let gone = refusal(
        promoted.dispatch(call("die", json!({}))).await,
        "a server that exits mid-call is an error",
    );
    assert_eq!(gone.code, ErrorCode::CapabilityUnavailable);
    assert_ne!(gone.code, missing.code);

    // And it stays that way: the next call to a dead server is the same answer, not a hang and
    // not a different code. This is what lets Task 4 withdraw on the strength of one call.
    let again = refusal(
        promoted.dispatch(call("echo", json!({ "text": "anybody there" }))).await,
        "a call to a dead server is an error",
    );
    assert_eq!(again.code, ErrorCode::CapabilityUnavailable);
    assert!(
        again.message.contains(SERVER_NAME),
        "the error should name the server that is gone: {again}"
    );
}

#[tokio::test]
async fn a_duplicate_tool_name_is_dropped_and_said_so_rather_than_announced_twice() {
    let promoted = promoted(SERVER_NAME, &["--odd-tools"]).await;
    let descriptor = promoted.descriptor();

    // One `search`, not two. A second descriptor under the same name is unreachable by
    // construction — `CapabilityDescriptor::tool` takes the first — so announcing it would show
    // an agent a schema it can never call.
    let searches: Vec<_> = descriptor.tools.iter().filter(|t| t.name == "search").collect();
    assert_eq!(searches.len(), 1, "`search` is announced {} times", searches.len());
    // The one that survived is the first the server listed, identified by its schema rather than
    // by its description, because the schema is what an agent builds a call from.
    assert_eq!(
        searches[0].request_schema["properties"],
        json!({ "query": { "type": "string" } })
    );

    // Dropped, and said so. An agent cannot be told, but the person at the window can, and a
    // silent drop is the thing the plan rules out.
    //
    // **One record, though the server offers `search` three times.** A record per extra copy
    // carries the same name and the same sentence twice — a duplicate line in the window and, since
    // `ui/src/Mcp.tsx` renders these keyed by name, two children under one key.
    let dropped = promoted.dropped();
    assert_eq!(dropped.len(), 1, "expected one dropped tool, got {dropped:?}");
    assert_eq!(dropped[0].name, "search");
    let mut names: Vec<&str> = dropped.iter().map(|tool| tool.name.as_str()).collect();
    names.sort_unstable();
    let unique = names.len();
    names.dedup();
    assert_eq!(names.len(), unique, "two absences under one name: {dropped:?}");
    // The reason has to be actionable on its own, because it is what a person reads in the log
    // or the window with no other context: which server, which tool, and what the consequence is.
    let reason = &dropped[0].reason;
    assert!(
        reason.contains(SERVER_NAME) && reason.contains("search") && reason.contains("first"),
        "the reason should name the server, the tool and what happened: {reason:?}"
    );

    // And the tool that is announced is the one a call reaches.
    let out = answer(
        promoted
            .dispatch(call("search", json!({ "query": "ledger" })))
            .await
            .expect("`search` answers"),
    );
    assert_eq!(out["content"][0]["text"], "searched for ledger");
}

#[tokio::test]
async fn a_tool_the_server_described_thinly_still_says_something() {
    let promoted = promoted(SERVER_NAME, &["--odd-tools"]).await;
    let descriptor = promoted.descriptor();

    // MCP's `description` is optional and `ToolDescriptor`'s is not, so something has to fill it.
    // A `title` is the server's own wording and beats anything invented here.
    assert_eq!(
        descriptor.tool("titled").expect("`titled` is announced").description,
        "Say something about this tool"
    );

    // With neither, the gap is stated rather than left empty: an agent reading a blank
    // description cannot tell a terse tool from a field that was dropped in translation.
    let untitled = &descriptor.tool("untitled").expect("`untitled` is announced").description;
    assert!(
        untitled.contains(SERVER_NAME) && !untitled.is_empty(),
        "an undescribed tool should say which server it came from: {untitled:?}"
    );
}

#[tokio::test]
async fn an_empty_input_schema_is_announced_as_it_is_rather_than_dropped() {
    // The plan expected "a schema that will not translate" to be a case needing a decision. It
    // is not one: `{}` is valid JSON Schema meaning "anything", and it goes through untouched.
    let promoted = promoted(SERVER_NAME, &["--odd-tools"]).await;

    let anything = promoted
        .descriptor()
        .tool("anything")
        .expect("`anything` is announced")
        .clone();
    assert_eq!(anything.request_schema, json!({}));

    let out = answer(
        promoted
            .dispatch(call("anything", json!({ "whatever": 1 })))
            .await
            .expect("`anything` answers"),
    );
    assert_eq!(out["content"][0]["text"], "anything ran");
}

#[tokio::test]
async fn a_dot_is_carried_in_a_tool_name_and_refused_in_a_server_name() {
    // The protocol's method is `capability.tool`, split at the **first** dot. That asymmetry is
    // the whole rule, and it is what a server name has to be checked against.
    assert_eq!(
        zyris::proto::split_method("mcp_my.notes.search"),
        Some(("mcp_my", "notes.search")),
        "a dot in the capability name moves the split, and the capability `mcp_my` does not exist"
    );

    // So a dot in a tool name is fine and is carried.
    let promoted = promoted(SERVER_NAME, &["--odd-tools"]).await;
    assert!(promoted.descriptor().tool("nested.tool").is_some());
    let out = answer(
        promoted
            .dispatch(call("nested.tool", json!({})))
            .await
            .expect("a dotted tool answers"),
    );
    assert_eq!(out["content"][0]["text"], "the dotted tool ran");

    // And a dot in the *server* name is refused, because the capability it would make is
    // unroutable — every call to it would be addressed to a capability that was never announced.
    let unroutable = Server::spawn("my.notes", &probe_server(), &[])
        .await
        .expect("the probe server starts whatever it is called");
    let error = Promoted::new(Arc::new(unroutable))
        .expect_err("a server name with a dot in it cannot be promoted");
    let chain = format!("{error:#}");
    assert!(
        chain.contains("my.notes"),
        "the refusal should name the server: {chain}"
    );
}

#[tokio::test]
async fn arguments_that_are_not_an_object_are_refused_before_the_server_is_asked() {
    let promoted = promoted(SERVER_NAME, &[]).await;

    // MCP's `tools/call` carries an object or nothing, and every input schema describes an
    // object. Anything else is the caller's mistake and is named as such rather than travelling
    // to the server to be rejected in the server's words.
    let error = refusal(
        promoted.dispatch(call("echo", json!("the quick brown fox"))).await,
        "a scalar is not an argument object",
    );
    assert_eq!(error.code, ErrorCode::InvalidParams);

    // Nothing at all is the ordinary case for a tool that takes no arguments, and is not an
    // error: `IncomingCall::params` is nil when the caller sent none.
    let out = answer(
        promoted
            .dispatch(IncomingCall {
                tool: "explode".to_string(),
                params: Payload::default(),
                serialization: Serialization::Json,
                meta: Payload::default(),
            })
            .await
            .expect("a tool with no arguments can be called with none"),
    );
    assert_eq!(out["isError"], true);
}

#[tokio::test]
async fn one_tool_whose_schema_is_not_an_object_takes_the_whole_server_with_it() {
    // Measured, and not where the plan expected it. `rmcp`'s `Tool::input_schema` is an
    // `Arc<JsonObject>`, so a server that sends `"inputSchema": true` — valid JSON Schema, and
    // the one that means "anything" — makes the whole `tools/list` response undeserializable.
    // The server therefore never starts and nothing is promoted: `promote.rs` cannot have an
    // opinion about it, because it never sees a tool list at all.
    let error = Server::spawn("loose", &probe_server(), &["--bad-schema".to_owned()])
        .await
        .expect_err("a tool list `rmcp` cannot read is a spawn failure");
    let chain = format!("{error:#}");
    assert!(
        chain.contains("listing the tools") && chain.contains("loose"),
        "the failure should say which server and what it was doing: {chain}"
    );
}
