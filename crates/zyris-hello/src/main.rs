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
use zyris_capkit::PtyTerminal;
use zyris_caps::TerminalServer;

#[cfg(feature = "desktop")]
use zyris_capkit::{EnigoInput, HostDisplays, HostScreenCapture};
#[cfg(feature = "desktop")]
use zyris_caps::{ImageFormat, InputServer, ScreenCaptureServer};

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

    let runner = runner
        .kind(NodeKind::Service)
        .request_scopes(["agents:read"])
        .capability(HelloServer(greeter))
        .capability(TerminalServer(PtyTerminal::default()));

    #[cfg(feature = "desktop")]
    let runner = with_desktop(runner);

    runner
        .on_connect(|conn| async move { report_server_capabilities(&conn).await })
        .run()
        .await
}

/// Announce `screen_capture` and `input` — the two capabilities that need a display to exist.
///
/// This node sends screenshots at the display's own resolution: no default `max_width`, and
/// [`HostScreenCapture::without_budget`] to switch off the pass that re-encodes smaller until the
/// bytes fit inline. Neither cap was ever about what the wire can carry — a blob over
/// `zyris::proto::INLINE_BLOB_MAX` is detached onto its own credit-limited stream by the connection
/// itself, so the full-size image costs a stream rather than a refusal. What downscaling did cost
/// was a coordinate conversion: a scaled capture makes image coordinates stop being display
/// coordinates, and the model has to read the multiplier out of the image's description and apply
/// it before `input.move_to` will land where it meant. At 1:1 there is nothing to apply.
///
/// A caller that wants a smaller image can still pass `max_width` per call. JPEG stays because a
/// full-resolution PNG of a 4K display is several times the size for pixels nobody inspects that
/// closely; pass [`ImageFormat::Png`] instead if exactness matters more than bytes.
///
/// `input` is announced only if the display server actually accepts a connection. A node that
/// cannot type should not offer to: announcing it and failing every call is worse than never
/// appearing in the tool list, because an agent has no way to tell the two apart.
#[cfg(feature = "desktop")]
fn with_desktop(runner: Runner) -> Runner {
    let screen = HostScreenCapture::default()
        .with_format(ImageFormat::Jpeg)
        .without_budget();
    let backend = screen.backend();
    tracing::info!(backend = ?backend, "announcing screen_capture");
    let runner = runner.capability(ScreenCaptureServer(screen));

    // The same backend the screenshots come from: `move_to` takes display-local coordinates and
    // adds that display's origin, so it has to agree with `screenshot` about where a monitor is.
    match EnigoInput::new(HostDisplays(backend)) {
        Ok(input) => {
            tracing::info!("announcing input");
            runner.capability(InputServer(input))
        }
        Err(e) => {
            tracing::warn!(error = %e, "no display server for input; not announcing it");
            runner
        }
    }
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
