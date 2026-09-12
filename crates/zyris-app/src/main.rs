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

    // **Before the instance lock, and before anything reaches the keychain.** Turning autostart
    // on while Zyris is already running is an ordinary thing to do — it is what somebody does
    // the day after they install it — and being refused by your own running copy is not an
    // answer. These flags do one thing and exit; neither of them starts a node.
    //
    // After `tracing` is up, because what they have to say is said through it.
    if let Some(request) = cli.autostart() {
        return run_autostart(request, cli.server());
    }

    // Everything that names this instance on this machine — the keychain service, the instance
    // lock, the directory the audit log lands in — comes from this one string, and `--server`
    // changes it. A run pointed at a development server is a different node in every sense that
    // matters: its account credential and its node token belong to that server, so it must not
    // read or write the production ones. The sharp edge is `connection.rs`'s
    // `recover_from_dead_token`, whose first action on a permanently refused token is to forget
    // the stored one — a dev server answering 401 would otherwise destroy the real node's token.
    // Derived here, before any of the three names is used, because all three have to agree.
    let instance = instance_name(cli.server());
    if let Some(server) = cli.server() {
        // Said out loud, in the first lines of output: a run pointed at a local server is a run
        // whose node, tokens, lock and audit log all live somewhere other than the real
        // account's, and a person who forgets which one they are on will read every later line
        // wrongly.
        tracing::info!(
            %server,
            %instance,
            "dialling this server instead of Attacca, as --server asked; this run keeps its own credentials, lock and audit log under that instance name"
        );
    }

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
    // The name is the instance's, not the product's: a `--server` run is a separate node with
    // separate credentials, and refusing to start it because the production instance holds the
    // lock would defeat the flag.
    //
    // Either way, whichever process actually acquires it holds the guard for the rest of its
    // run — its drop is what releases the lock.
    let _lock = if mode == cli::Mode::Headless {
        match zyris_runtime::lock::InstanceLock::acquire(&instance) {
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
    // Named by the instance, so a `--server` run reads and writes its own `account-credential`
    // and `node-token` rather than the production ones. The file fallback follows for free:
    // `secret.rs`'s `default_file_dir` derives its directory from this same string.
    let identity = zyris_runtime::identity::Identity::new(
        zyris_runtime::secret::SecretStore::new(&instance),
    );

    // Built here, once, for the same reason the connector is: the switch has to stop tools with
    // the window closed exactly as it does with it open, and a `Tools` per runtime would be two
    // switches and two logs that disagree.
    let tools = zyris_tools::Tools::new(
        zyris_tools::Gate::running(),
        zyris_tools::AuditLog::new(audit_path(&instance)),
        zyris_tools::default_root(),
    )
    // Every call is published as well as written down. The bus is the only way the window and
    // the tray hear about a call while it happens; the file is what outlives the process.
    .with_bus(bus.clone());
    // Built once and named from that same list. `announced()` answers from what this call
    // records, so it has to run before `gui::run` takes the `Tools` or the window would have
    // nothing to report.
    let capabilities = tools.clone().into_capabilities();
    // Which ones actually made it, said out loud. `input` and `screen_capture` are absent on a
    // machine with no display server, and this line plus the one `zyris-tools` logs when it is
    // refused is the whole explanation of why an agent cannot see the screen — which someone
    // will ask.
    let announced = capabilities
        .iter()
        .map(|capability| capability.descriptor().name)
        .collect::<Vec<_>>()
        .join(", ");
    // Both paths, once, at startup. The root matters as much as the log's own path: an entry
    // records the caller's path string rather than the resolved one, so a line reading
    // `path=notes/x.txt` cannot be read without knowing what it resolved against.
    tracing::info!(
        %announced,
        audit_log = %tools.log().path().display(),
        capability_root = %tools.root().display(),
        "tools are announced: what ran is written here, and a relative path starts at the root"
    );

    let mut connector = zyris_runtime::connection::Connector::new(identity, bus.clone())
        .with_capabilities(capabilities);

    // Announced further up, beside the instance name the same flag changes.
    if let Some(server) = cli.server() {
        connector = connector.with_server(server.to_string());
    }

    match mode {
        // Headless is handed no `Tools`: it has no surface to move the switch from, and the
        // gate it would need is already inside every capability the connector announces. The
        // window gets one so the tray and the Tools tab can reach the same gate and the same
        // log — the same ones, not copies, because `Tools` holds handles on shared state.
        cli::Mode::Headless => runtime.block_on(headless::run(bus, connector)),
        // Both windowed modes are the same runtime; the mode goes along so `setup` knows
        // whether to put the window on the screen. The instance name goes with it too: the GUI
        // takes its lock inside `setup`, and it has to be the same name this function derived
        // for the keychain and the log.
        mode @ (cli::Mode::Window | cli::Mode::WindowHidden) => {
            gui::run(bus, runtime.handle().clone(), connector, tools, instance, mode)
        }
    }
}

/// Turn autostart on or off from the command line, and say where that left the machine.
///
/// The window's switch goes through the same `bridge::apply_autostart`, so a flag and a click
/// cannot install different things or read the answer differently.
///
/// Everything it has to report goes through `tracing` rather than `println!`, like the rest of
/// this program: a person running this on a server is reading the same stream either way, and
/// `RUST_LOG` is what turns the detail up.
fn run_autostart(request: cli::AutostartRequest, server: Option<&str>) -> anyhow::Result<()> {
    // Both mechanisms start `<this executable> --minimized` and nothing else, so autostart
    // installed from a `--server` run starts the *production* instance at the next logon — a
    // different node, with different credentials, from the one this process would have been.
    // Said rather than refused: the person may well want exactly that.
    if server.is_some() {
        tracing::warn!(
            "--server is not carried into autostart: what starts at logon is this executable with --minimized, which is the default instance"
        );
    }

    let autostart = zyris_autostart::Autostart::for_this_machine();
    let view =
        bridge::apply_autostart(&autostart, request == cli::AutostartRequest::Install)?;

    // Read back, never assumed — the same rule the window follows.
    match &view.state {
        zyris_autostart::State::Enabled => tracing::info!(
            mechanism = view.mechanism.as_deref().unwrap_or("an unnamed mechanism"),
            "Zyris will start when you sign in",
        ),
        zyris_autostart::State::Disabled => {
            tracing::info!("Zyris will not start when you sign in");
        }
        // Reachable after a successful call only in the strangest circumstances, and the
        // honest thing to print when it happens.
        zyris_autostart::State::Unsupported(reason) => {
            tracing::warn!(%reason, "this machine cannot start Zyris by itself");
        }
    }

    // At `warn`, because every one of these is a way the switch is weaker than "on" sounds.
    // On Linux this line is the difference between a machine that is connected whenever it is
    // switched on and one that is connected only while somebody is logged in to a desktop.
    for caveat in &view.caveats {
        tracing::warn!("{caveat}");
    }

    Ok(())
}

/// What this run calls itself on this machine: the keychain service, the instance lock's name,
/// and the directory the audit log lands in. All three from one string, so they cannot disagree.
///
/// A default run is `zyris`, exactly as it has always been, so nothing about an existing install
/// moves. A `--server` run earns a name of its own, because it is a different node: its
/// credentials belong to that server, its calls are not this machine's real history, and it has
/// to be able to run *beside* a production instance rather than be turned away by its lock —
/// which is the point of the flag.
///
/// Every character that is not `[0-9A-Za-z]` is replaced, so the result is usable as a directory
/// name, a file name and a keychain service on every platform. Two servers that differ only in
/// punctuation collide into one name; that is chosen over hashing, because a person who finds
/// `zyris-dev-ws---127-0-0-1-8080-zyris-v1-ws` in their data directory can tell what it is.
fn instance_name(server: Option<&str>) -> String {
    match server {
        None => "zyris".to_string(),
        Some(url) => {
            format!("zyris-dev-{}", url.replace(|c: char| !c.is_ascii_alphanumeric(), "-"))
        }
    }
}

/// Where the record of what ran lives: beside the other per-user state, never beside the binary.
///
/// Scoped by the instance for the same reason the keychain is — a `--server` run must not append
/// its calls to the production machine's history, nor read that history back as its own.
///
/// The fallbacks mirror `SecretStore`'s, and for the same reason — the current directory is `/`
/// under a systemd unit and whatever a shortcut set for a desktop launch, so a log written there
/// lands somewhere different every launch.
fn audit_path(instance: &str) -> std::path::PathBuf {
    if let Some(dirs) = directories::ProjectDirs::from("cc", "attacca", instance) {
        return dirs.data_dir().join("audit.jsonl");
    }
    if let Some(dirs) = directories::BaseDirs::new() {
        return dirs.home_dir().join(format!(".{instance}")).join("audit.jsonl");
    }
    // No directory the platform can name. `AuditLog` survives a path it cannot write — it says
    // so in the process log and never fails a tool call — so an absolute, OS-chosen path is a
    // better last resort than refusing to start.
    std::env::temp_dir().join(format!("{instance}-audit.jsonl"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_instance_keeps_the_name_an_existing_install_already_uses() {
        // This exact string is the keychain service, the lock's name and the audit directory on
        // every machine already running Zyris. Changing it orphans their stored credentials and
        // mints a second node on the next launch.
        assert_eq!(instance_name(None), "zyris");
    }

    #[test]
    fn a_server_run_is_a_different_instance_from_the_default_one() {
        // Otherwise a development run reads and writes the production account credential and
        // node token — and a dev server that refuses that token makes this app forget it.
        assert_ne!(
            instance_name(None),
            instance_name(Some("ws://127.0.0.1:8080/zyris/v1/ws"))
        );
    }

    #[test]
    fn two_servers_are_two_instances() {
        assert_ne!(
            instance_name(Some("ws://127.0.0.1:8080/zyris/v1/ws")),
            instance_name(Some("ws://127.0.0.1:9090/zyris/v1/ws"))
        );
    }

    #[test]
    fn an_instance_name_is_safe_as_a_file_name_and_as_a_keychain_service() {
        let name = instance_name(Some("ws://127.0.0.1:8080/zyris/v1/ws"));

        assert!(
            name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "a name carrying a path separator would land the log somewhere else entirely: {name}"
        );
    }
}
