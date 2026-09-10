//! Zyris: a desktop node for Attacca.
//!
//! This file picks a runtime and does nothing else. Both runtimes are handed the same
//! `EventBus`, because the difference between them is only whether anything is watching.

mod cli;
mod gui;
mod headless;

use clap::Parser;
use tracing_subscriber::EnvFilter;
use zyris_core::EventBus;

/// How many events a subscriber may fall behind before it loses the oldest.
const EVENT_CAPACITY: usize = 64;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "zyris=info".into()),
        )
        .init();

    let bus = EventBus::new(EVENT_CAPACITY);

    match cli::Cli::parse().mode() {
        // Not `#[tokio::main]`: the GUI runtime has to own the main thread, so the async
        // runtime is built only on the branch that needs one.
        cli::Mode::Headless => {
            tokio::runtime::Runtime::new()?.block_on(headless::run(bus))
        }
        cli::Mode::Gui => gui::run(bus),
    }
}
