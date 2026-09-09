//! The windowless runtime. This is the whole program when `--headless` is passed, and it is also
//! what the GUI runtime wraps — so nothing that matters may live only in `gui.rs`.

use zyris_core::{CoreEvent, EventBus};

/// Runs until interrupted. Ctrl-C is this program's decision, not the core's.
pub async fn run(bus: EventBus) -> anyhow::Result<()> {
    bus.publish(CoreEvent::Started);
    tracing::info!("running headless");

    tokio::signal::ctrl_c().await?;

    tracing::info!("stopping");
    bus.publish(CoreEvent::ShuttingDown);
    Ok(())
}
