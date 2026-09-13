//! A small MCP server, over stdio, for `zyris-mcp`'s tests to talk to.
//!
//! **Why this exists rather than a mock of `rmcp`'s client.** The thing worth testing is that
//! this crate and a server agree on the wire; a mock only ever agrees with whatever was believed
//! when it was written.
//!
//! `rmcp` 3.3 exports no ready-made test server. It does support an in-process pair — any
//! `AsyncRead + AsyncWrite` is a transport, and its own suite runs a `serve_server` against a
//! `serve_client` over a `tokio::io::duplex` — and that was the obvious route until it was looked
//! at. Two things rule it out. It needs `rmcp`'s `server` feature, which this workspace otherwise
//! has no use for and deliberately turns off. And, decisively, **there is no child process in it**:
//! the three failures `Server::spawn` exists to bound are a command that is not there, one that
//! starts and never speaks, and one that exits mid-call, and a duplex pair has none of them.
//!
//! So: a real process speaking real newline-delimited JSON-RPC, with every byte going through
//! `rmcp`'s actual client, framing and handshake.
//!
//! It is deliberately hand-written and dependency-free beyond `serde_json`: the point is to be a
//! *foreign* server, not a second copy of the code under test. That also makes the two
//! misbehaviours the tests need trivial to arrange, which no well-behaved server would offer:
//!
//! - `--mute` answers nothing, ever. A command that starts and then says nothing is the hang
//!   `Server::spawn`'s deadline exists for, and it is a different failure from a command that is
//!   not there at all.
//! - the `die` tool exits the process mid-request, with the client still waiting on the answer.
//!
//! Everything else is three ordinary tools: `echo`, `add`, and `explode`, which reports a failure
//! the MCP way — `isError: true` in a perfectly successful response, not a JSON-RPC error. They
//! come back over two pages of `tools/list`, because one page is the shape that hides a client
//! which ignores `nextCursor`.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

/// The protocol version this answers with. Older than `rmcp`'s latest on purpose: a real machine
/// runs whatever servers a person installed, and the client has to cope with a server that is
/// behind it.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// The first page of `tools/list`, and the only one a client that ignores `nextCursor` will ever
/// see.
///
/// Paginating a four-tool list is not realistic, and that is the point: a one-page list is the
/// shape that hides a client which stops at the first response, and the tools it would drop are
/// exactly the ones a person would then have to guess about.
fn tools_page_one() -> Value {
    json!([
        {
            "name": "echo",
            "description": "Return the text it is given.",
            "inputSchema": {
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"],
            },
        },
        {
            "name": "add",
            "description": "Add two numbers.",
            "inputSchema": {
                "type": "object",
                "properties": { "a": { "type": "number" }, "b": { "type": "number" } },
                "required": ["a", "b"],
            },
            "outputSchema": {
                "type": "object",
                "properties": { "sum": { "type": "number" } },
                "required": ["sum"],
            },
        },
    ])
}

/// The rest, reachable only by following `nextCursor`.
fn tools_page_two() -> Value {
    json!([
        {
            "name": "explode",
            "description": "Fail, the way a tool is supposed to: a result that says so.",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "die",
            "description": "Exit the process without answering.",
            "inputSchema": { "type": "object", "properties": {} },
        },
    ])
}

fn main() {
    let mute = std::env::args().any(|arg| arg == "--mute");

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        // A notification carries no id and gets no answer. `notifications/initialized` is the one
        // that actually arrives.
        let Some(id) = message.get("id").cloned() else {
            continue;
        };
        if mute {
            continue;
        }

        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let params = message.get("params");

        let response = match method {
            "initialize" => ok(id, initialize()),
            "tools/list" => list(id, params),
            "tools/call" => call(id, params, &mut stdout),
            other => error(id, -32601, &format!("no method `{other}`")),
        };

        let mut encoded = serde_json::to_string(&response).expect("a response is serializable");
        encoded.push('\n');
        if stdout.write_all(encoded.as_bytes()).is_err() || stdout.flush().is_err() {
            break;
        }
    }
}

fn initialize() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "probe", "version": "0.0.0" },
    })
}

fn list(id: Value, params: Option<&Value>) -> Value {
    match params.and_then(|p| p.get("cursor")).and_then(Value::as_str) {
        None => ok(
            id,
            json!({ "tools": tools_page_one(), "nextCursor": "the-rest" }),
        ),
        Some("the-rest") => ok(id, json!({ "tools": tools_page_two() })),
        Some(other) => error(id, -32602, &format!("no cursor `{other}`")),
    }
}

fn call(id: Value, params: Option<&Value>, stdout: &mut std::io::Stdout) -> Value {
    let name = params
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let arguments = params.and_then(|p| p.get("arguments"));

    match name {
        "echo" => {
            let text = arguments
                .and_then(|a| a.get("text"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            ok(id, json!({ "content": [text_block(text)] }))
        }
        "add" => {
            let a = arguments.and_then(|x| x.get("a")).and_then(Value::as_f64);
            let b = arguments.and_then(|x| x.get("b")).and_then(Value::as_f64);
            match (a, b) {
                (Some(a), Some(b)) => {
                    let sum = a + b;
                    ok(
                        id,
                        json!({
                            "content": [text_block(&sum.to_string())],
                            "structuredContent": { "sum": sum },
                        }),
                    )
                }
                _ => error(id, -32602, "`add` wants two numbers, `a` and `b`"),
            }
        }
        "explode" => ok(
            id,
            json!({
                "content": [text_block("the tool refused")],
                "isError": true,
            }),
        ),
        "die" => {
            // Leave nothing half-written on the way out: the client must see a closed pipe, not a
            // truncated frame it could mistake for a protocol fault.
            let _ = stdout.flush();
            std::process::exit(9);
        }
        other => error(id, -32602, &format!("no tool `{other}`")),
    }
}

fn text_block(text: &str) -> Value {
    json!({ "type": "text", "text": text })
}

fn ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}
