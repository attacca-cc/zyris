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
        while let Ok(event) = events.recv().await {
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
