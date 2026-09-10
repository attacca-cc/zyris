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
use zyris_runtime::EventBus;

/// How many events a subscriber may fall behind before it loses the oldest.
const EVENT_CAPACITY: usize = 64;

fn main() -> anyhow::Result<()> {
    // Parsed before anything else touches the system. `clap` prints help or version text and
    // exits the process by itself on `--help`/`--version`, and that has to work even while
    // another instance holds the lock (parsing after it meant a running instance made `--help`
    // print nothing and exit 0) and before `SecretStore::new` gets anywhere near the keychain,
    // which can raise an unlock dialog on some platforms.
    let cli = cli::Cli::parse();
    let mode = cli.mode();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "zyris=info".into()),
        )
        .init();

    // A second instance must not mint a second node token — but the two modes take the
    // instance lock in different places, because they need different things from a refusal.
    //
    // Headless takes it right here, before anything else does work (in particular, before
    // `SecretStore::new` below gets anywhere near the keychain, which can raise an unlock
    // dialog on some platforms): there is no plugin to reach and nothing to focus, so a second
    // headless launch should simply be refused as early as possible.
    //
    // The GUI takes the very same lock, by the same name, from inside `gui::run`'s own
    // `setup()` instead — after `tauri_plugin_single_instance` has had first refusal. Taking it
    // here would refuse a second GUI launch before it ever reached that plugin, which is what
    // is supposed to focus the already-running window; see `gui.rs`.
    //
    // Either way, whichever process actually acquires it holds the guard for the rest of its
    // run — its drop is what releases the lock.
    let _instance = if mode == cli::Mode::Headless {
        match zyris_runtime::lock::InstanceLock::acquire("zyris") {
            Ok(Some(lock)) => Some(lock),
            Ok(None) => {
                tracing::info!("another Zyris is already running on this machine; exiting");
                return Ok(());
            }
            Err(error) => {
                tracing::warn!(%error, "could not take the instance lock; continuing anyway");
                // A machine where the lock file cannot be created is a machine where refusing
                // to start would be worse than the risk the lock guards against.
                None
            }
        }
    } else {
        None
    };

    let bus = EventBus::new(EVENT_CAPACITY);
    // Not `#[tokio::main]`: the GUI runtime has to own the main thread synchronously, so the
    // runtime is built by hand and only driven with `block_on` on the branch that needs that.
    let runtime = tokio::runtime::Runtime::new()?;

    // Built once, here: both runtimes need the same connector, and building it in `main` keeps
    // `gui.rs` and `headless.rs` from each inventing their own.
    let identity =
        zyris_runtime::identity::Identity::new(zyris_runtime::secret::SecretStore::new("zyris"));

    // Built here, once, for the same reason the connector is: the switch has to stop tools with
    // the window closed exactly as it does with it open, and a `Tools` per runtime would be two
    // switches and two logs that disagree.
    let tools = zyris_tools::Tools::new(
        zyris_tools::Gate::running(),
        zyris_tools::AuditLog::new(audit_path()),
        zyris_tools::default_root(),
    )
    // Every call is published as well as written down. The bus is the only way the window and
    // the tray hear about a call while it happens; the file is what outlives the process.
    .with_bus(bus.clone());
    // Both paths, once, at startup. The root matters as much as the log's own path: an entry
    // records the caller's path string rather than the resolved one, so a line reading
    // `path=notes/x.txt` cannot be read without knowing what it resolved against.
    tracing::info!(
        audit_log = %tools.log().path().display(),
        capability_root = %tools.root().display(),
        "tools are announced: what ran is written here, and a relative path starts at the root"
    );

    let mut connector = zyris_runtime::connection::Connector::new(identity, bus.clone())
        .with_capabilities(tools.clone().into_capabilities());

    // Said out loud, in the first lines of output: a run pointed at a local server is a run
    // whose node and tokens live somewhere other than the real account, and a person who
    // forgets which one they are on will read every later line wrongly.
    if let Some(server) = cli.server() {
        tracing::info!(%server, "dialling this server instead of Attacca, as --server asked");
        connector = connector.with_server(server.to_string());
    }

    match mode {
        // Headless is handed no `Tools`: it has no surface to move the switch from, and the
        // gate it would need is already inside every capability the connector announces. The
        // window gets one so the tray and the Tools tab can reach the same gate and the same
        // log — the same ones, not copies, because `Tools` holds handles on shared state.
        cli::Mode::Headless => runtime.block_on(headless::run(bus, connector)),
        cli::Mode::Gui => gui::run(bus, runtime.handle().clone(), connector, tools),
    }
}

/// Where the record of what ran lives: beside the other per-user state, never beside the binary.
///
/// The fallbacks mirror `SecretStore`'s, and for the same reason — the current directory is `/`
/// under a systemd unit and whatever a shortcut set for a desktop launch, so a log written there
/// lands somewhere different every launch.
fn audit_path() -> std::path::PathBuf {
    if let Some(dirs) = directories::ProjectDirs::from("cc", "attacca", "zyris") {
        return dirs.data_dir().join("audit.jsonl");
    }
    if let Some(dirs) = directories::BaseDirs::new() {
        return dirs.home_dir().join(".zyris").join("audit.jsonl");
    }
    // No directory the platform can name. `AuditLog` survives a path it cannot write — it says
    // so in the process log and never fails a tool call — so an absolute, OS-chosen path is a
    // better last resort than refusing to start.
    std::env::temp_dir().join("zyris-audit.jsonl")
}
