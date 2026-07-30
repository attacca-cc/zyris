//! A minimal, complete Zyris node. Copy this crate as the starting point for your own.
//!
//! It does the two things every node does: it *announces* a capability (one `greet` tool, callable
//! by any agent the owning user runs) and it *consumes* the `attacca_api` capability the server
//! announces back — both over the same websocket, which is the point of the protocol.
//!
//! Everything else — reading configuration, getting a credential, dialing, backing off, rotating a
//! refused token, exiting with a code a supervisor understands — is `zyris::runtime`. That is why
//! this file is short: none of it was ever specific to *this* node.

mod greeter;

use std::process::ExitCode;
use std::time::Duration;

use greeter::{HelloServer, RandomGreeter};
use zyris::runtime::Runner;
use zyris::{Connection, ErrorCode, NodeKind};
use zyris_attacca::{AttaccaApi, AttaccaApiClient};
use zyris_caps::TerminalServer;
use zyris_capkit::PtyTerminal;

/// The server announces `attacca_api` immediately after the handshake; this is generous headroom.
const CONSUME_WAIT: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "zyris_hello=info,zyris=info".into()),
        )
        .init();

    let runner = Runner::from_env();
    // The greeter reports which node answered, so it needs the name the runner resolved — from
    // `$ZYRIS_NODE_NAME`, or this machine's hostname.
    let greeter = RandomGreeter::new(runner.node_name());

    runner
        .kind(NodeKind::Service)
        .request_scopes(["agents:read"])
        .capability(HelloServer(greeter))
        .capability(TerminalServer(PtyTerminal::default()))
        .on_connect(|conn| async move { report_server_capabilities(&conn).await })
        .run()
        .await
}

/// The consume half. A node is not only a tool provider: the server announces `attacca_api` on the
/// same connection, so this process can drive agents and sessions while serving `greet`. Failures
/// here are logged and ignored — a node whose token lacks `agents:read` should still serve tools.
///
/// The client is `zyris-attacca`'s, the crate that declares `attacca_api` — the consumer side of a
/// capability is a trait like any other, and importing one someone already wrote is the short path.
/// When you consume a capability nobody has published, declare the slice you call yourself: one
/// `#[zyris::capability]` trait naming the methods you use resolves against the real announcement,
/// because matching is by `(name, version)` and the announced tool list is never compared.
async fn report_server_capabilities(conn: &Connection) {
    match conn.wait_capability::<AttaccaApiClient>(CONSUME_WAIT).await {
        Ok(api) => {
            tracing::info!("server announced attacca_api; this node can call back into Attacca");
            match api.list_agents().await {
                Ok(agents) => tracing::info!(
                    count = agents.len(),
                    first = agents.first().map(|a| a.name.as_str()).unwrap_or("-"),
                    "attacca_api.list_agents ok"
                ),
                Err(e) if e.code == ErrorCode::ForbiddenScope => {
                    tracing::info!(
                        "this node's token has no agents:read scope; skipping the demo call"
                    )
                }
                Err(e) => tracing::warn!(error = %e, "attacca_api.list_agents failed"),
            }
        }
        Err(e) => tracing::warn!(error = %e, "server did not announce attacca_api"),
    }
}
