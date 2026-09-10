//! The windowless runtime. This is the whole program when `--headless` is passed.
//!
//! Starting and stopping the core goes through `zyris_core::lifecycle`, the same entry point
//! `gui.rs` calls — so nothing that matters can live only in one runtime's copy-pasted code.

use zyris_core::connection::Connector;
use zyris_core::{lifecycle, CoreEvent, EventBus};

/// Runs until interrupted. Ctrl-C is this program's decision, not the core's.
pub async fn run(bus: EventBus, connector: Connector) -> anyhow::Result<()> {
    // No subscriber wiring happens before this in headless mode, so there is no "after setup"
    // to wait for — this stays the earliest point, symmetric with the GUI runtime publishing
    // as soon as its own setup is done.
    lifecycle::start(&bus);
    tracing::info!("running headless");

    let mut events = bus.subscribe();
    tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            match event {
                CoreEvent::NeedsEnrolment => tracing::info!("this node is not enrolled yet"),
                CoreEvent::EnrolmentCode { user_code, verification_uri } => tracing::info!(
                    code = %user_code,
                    url = %verification_uri,
                    "authorize this node: open the url and enter the code"
                ),
                CoreEvent::EnrolmentFailed { reason } => tracing::error!(%reason, "enrolment failed"),
                CoreEvent::Connecting => tracing::info!("connecting"),
                CoreEvent::Connected { node_id, node_name } => {
                    tracing::info!(%node_id, %node_name, "connected")
                }
                CoreEvent::Disconnected { reason } => tracing::warn!(%reason, "disconnected"),
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
