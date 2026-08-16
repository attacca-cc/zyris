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
#[cfg(feature = "transfer")]
mod transfer;

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
pub(crate) const CONSUME_WAIT: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> ExitCode {
    // Before anything opens a TLS connection, and before the logger, because the panic this avoids
    // takes the whole node down on the first connect and leaves the reason in a backtrace rather
    // than in the log. `zyris_p2p::tls` explains what the two providers are and why neither this
    // node nor its configuration can settle it.
    #[cfg(feature = "transfer")]
    zyris_p2p::tls::install_default_provider();

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

    // `peers:write` is what lets a node publish its own peer address and look up another one on
    // the same account. Without it `peer_publish` comes back `ForbiddenScope`, nothing is ever
    // published, and `peer_lookup` has nothing to answer with — so no peer can find this node and
    // no transfer can start. That is not a degraded transfer, it is the absence of one, and the
    // only sign of it is a warning in the log at connect time.
    //
    // Asked for only when the feature is on, because the consent screen should name what this node
    // will actually do. A node built without transfer has no use for it. Turning the feature on
    // later means enrolling again — scopes are granted with the credential, not after it.
    #[cfg(feature = "transfer")]
    let scopes: &[&str] = &["agents:read", "peers:write"];
    #[cfg(not(feature = "transfer"))]
    let scopes: &[&str] = &["agents:read"];

    let runner = runner
        .kind(NodeKind::Service)
        .request_scopes(scopes.iter().copied())
        .capability(HelloServer(greeter))
        .capability(TerminalServer(PtyTerminal::default()));

    #[cfg(feature = "desktop")]
    let runner = with_desktop(runner);

    // Built before `run`, because a node announces what it can do before it has anywhere to say it.
    // The rendezvous client and the accept loop both arrive later, on the connection.
    #[cfg(feature = "transfer")]
    let transfer = match transfer::Transfer::bind(runner.node_name()).await {
        Ok(transfer) => Some(std::sync::Arc::new(transfer)),
        Err(error) => {
            tracing::warn!(%error, "could not bind the peer endpoint; file transfer is off");
            None
        }
    };
    #[cfg(feature = "transfer")]
    let runner = match &transfer {
        Some(transfer) => runner.capability(transfer.capability()),
        None => runner,
    };

    runner
        .on_connect(move |conn| {
            #[cfg(feature = "transfer")]
            let transfer = transfer.clone();
            async move {
                report_server_capabilities(&conn).await;
                #[cfg(feature = "transfer")]
                if let Some(transfer) = transfer {
                    transfer.on_connect(&conn).await;
                }
            }
        })
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
