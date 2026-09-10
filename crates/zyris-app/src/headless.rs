//! The windowless runtime. This is the whole program when `--headless` is passed.
//!
//! Starting and stopping the core goes through `zyris_runtime::lifecycle`, the same entry point
//! `gui.rs` calls — so nothing that matters can live only in one runtime's copy-pasted code.

use zyris_runtime::connection::Connector;
use zyris_runtime::{lifecycle, CoreEvent, EventBus};

/// Runs until interrupted. Ctrl-C is this program's decision, not the core's.
pub async fn run(bus: EventBus, connector: Connector) -> anyhow::Result<()> {
    // Subscribed before `lifecycle::start` publishes `Started` — `broadcast` never replays a
    // send to a subscriber that shows up late, so the logging loop below has to already exist
    // when that fires. Symmetric with the GUI runtime subscribing its bridge before its own
    // `lifecycle::start`; do not move this back below it.
    let mut events = bus.subscribe();
    lifecycle::start(&bus);
    tracing::info!("running headless");

    tokio::spawn(async move {
        loop {
            let event = match events.recv().await {
                Ok(event) => event,
                // A slow reader missed events. Dropping the oldest is the bus's intended
                // behaviour and the next event still arrives, so this must not end the loop —
                // `while let Ok(..)` would have stopped logging for the rest of the run, and
                // tool calls arrive fast enough to make that a real possibility rather than a
                // theoretical one.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::warn!(missed, "the log fell behind on core events");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            match event {
                CoreEvent::NeedsEnrolment => tracing::info!("this node is not enrolled yet"),
                CoreEvent::EnrolmentCode { user_code, verification_uri } => tracing::info!(
                    code = %user_code,
                    url = %verification_uri,
                    "authorize this node: open the url and enter the code"
                ),
                CoreEvent::EnrolmentFailed { reason } => {
                    tracing::error!(%reason, "enrolment failed; restart to try again")
                }
                CoreEvent::Connecting => tracing::info!("connecting"),
                CoreEvent::Connected { node_id, node_name } => {
                    tracing::info!(%node_id, %node_name, "connected")
                }
                CoreEvent::Disconnected { reason, retrying: true } => {
                    tracing::warn!(%reason, "disconnected; the link is redialling on its own")
                }
                CoreEvent::Disconnected { reason, retrying: false } => {
                    tracing::warn!(%reason, "disconnected; this will not retry, restart to reconnect")
                }
                CoreEvent::SetupFailed { reason } => {
                    tracing::error!(%reason, "setup failed; restart to try again")
                }
                CoreEvent::Paused { paused } => tracing::info!(paused, "the pause switch moved"),
                // `debug!`, not `info!`. An agent working through a task calls tools several
                // times a second, and at `info!` this log becomes a transcript of everything
                // that touched the machine. The durable record is the audit file, whose path
                // `main.rs` already logs once at startup.
                CoreEvent::ToolCall { capability, tool, detail, outcome } => {
                    tracing::debug!(%capability, %tool, %detail, %outcome, "a tool call")
                }
                CoreEvent::Started | CoreEvent::ShuttingDown => {}
            }
        }
    });

    // Spawned rather than awaited: the connector runs for the life of the process, and Ctrl-C
    // has to stay responsive while it does.
    let connection = tokio::spawn(connector.run());

    tokio::signal::ctrl_c().await?;

    tracing::info!("stopping");
    connection.abort();
    lifecycle::shutdown(&bus);
    Ok(())
}
