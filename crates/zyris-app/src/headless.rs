//! The windowless runtime. This is the whole program when `--headless` is passed.
//!
//! Starting and stopping the core goes through `zyris_core::lifecycle`, the same entry point
//! `gui.rs` calls — so nothing that matters can live only in one runtime's copy-pasted code.

use zyris_core::{lifecycle, EventBus};

/// Runs until interrupted. Ctrl-C is this program's decision, not the core's.
pub async fn run(bus: EventBus) -> anyhow::Result<()> {
    // No subscriber wiring happens before this in headless mode, so there is no "after setup"
    // to wait for — this stays the earliest point, symmetric with the GUI runtime publishing
    // as soon as its own setup is done.
    lifecycle::start(&bus);
    tracing::info!("running headless");

    tokio::signal::ctrl_c().await?;

    tracing::info!("stopping");
    lifecycle::shutdown(&bus);
    Ok(())
}
