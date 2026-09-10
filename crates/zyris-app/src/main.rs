//! Zyris: a desktop node for Attacca.
//!
//! This file picks a runtime and does nothing else. Both runtimes are handed the same
//! `EventBus`, because the difference between them is only whether anything is watching.
//!
//! The tokio runtime is built here, once, for both modes: from step 2 on, everything the core
//! owns — a websocket, reconnect, token refresh — is async, and in GUI mode it needs somewhere
//! to run since Tauri owns the main thread synchronously.

mod bridge;
mod cli;
mod gui;
mod headless;
mod tray;

use clap::Parser;
use tracing_subscriber::EnvFilter;
use zyris_core::EventBus;

/// How many events a subscriber may fall behind before it loses the oldest.
const EVENT_CAPACITY: usize = 64;

fn main() -> anyhow::Result<()> {
    // Parsed before anything else touches the system. `clap` prints help or version text and
    // exits the process by itself on `--help`/`--version`, and that has to work even while
    // another instance holds the lock (parsing after it meant a running instance made `--help`
    // print nothing and exit 0) and before `SecretStore::new` gets anywhere near the keychain,
    // which can raise an unlock dialog on some platforms.
    let mode = cli::Cli::parse().mode();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "zyris=info".into()),
        )
        .init();

    // Before anything else does work: a second instance must not mint a second node token.
    // The GUI's single-instance plugin only covers window-to-window; this covers every mode.
    // Held for the rest of `main` — its drop, at process exit, is what releases the lock.
    let _instance = match zyris_core::lock::InstanceLock::acquire("zyris") {
        Ok(Some(lock)) => Some(lock),
        Ok(None) => {
            tracing::info!("another Zyris is already running on this machine; exiting");
            return Ok(());
        }
        Err(error) => {
            tracing::warn!(%error, "could not take the instance lock; continuing anyway");
            // A machine where the lock file cannot be created is a machine where refusing to
            // start would be worse than the risk the lock guards against.
            None
        }
    };

    let bus = EventBus::new(EVENT_CAPACITY);
    // Not `#[tokio::main]`: the GUI runtime has to own the main thread synchronously, so the
    // runtime is built by hand and only driven with `block_on` on the branch that needs that.
    let runtime = tokio::runtime::Runtime::new()?;

    // Built once, here: both runtimes need the same connector, and building it in `main` keeps
    // `gui.rs` and `headless.rs` from each inventing their own.
    let identity =
        zyris_core::identity::Identity::new(zyris_core::secret::SecretStore::new("zyris"));
    let connector = zyris_core::connection::Connector::new(identity, bus.clone());

    match mode {
        cli::Mode::Headless => runtime.block_on(headless::run(bus, connector)),
        cli::Mode::Gui => gui::run(bus, runtime.handle().clone(), connector),
    }
}
