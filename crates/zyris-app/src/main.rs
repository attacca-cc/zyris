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
mod confirm;
mod gui;
mod headless;
mod hotkey;
mod tray;

use clap::Parser;
use tracing_subscriber::EnvFilter;
use zyris_runtime::EventBus;

/// How many events a subscriber may fall behind before it loses the oldest.
const EVENT_CAPACITY: usize = 64;

/// The record of what an agent asked of this machine, inside the instance's data directory.
const AUDIT_FILE: &str = "audit.jsonl";

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

    // Everything this run owns on disk lives here: the audit log, this machine's peer key, the
    // ledger of peers it has pinned, and the inbox. One directory, named by the instance, so a
    // `--server` run shares none of it with the production node.
    let data = data_dir(&instance);
    // Read once and shared: it is where a caller's relative paths start for `file_io` and
    // `terminal`, and the one directory `file_transfer` will read a file out of.
    let root = zyris_tools::default_root();

    // What this build and this machine can do about speech.
    //
    // **This is the same line in both feature states**, which is the whole point of it. The
    // audio stack is off by default — a cold build of it is two minutes against a warm 1.4
    // seconds — so almost every run of this program is the build that cannot listen, and a
    // build that cannot listen has to say so rather than being silently indistinguishable from
    // one that can. `zyris_voice::start` answers on every platform and never fails, exactly as
    // `hotkey::start` does. Nothing in this crate asks whether the audio stack was compiled in;
    // `tests/the_app_never_asks_whether_voice_is_compiled_in.rs` fails if anything ever starts.
    //
    // **It opens nothing.** Whether a microphone is opened is a person's answer, kept in
    // `data` beside everything else this instance owns, and acted on by `Voice::resume` — which
    // only the windowed branch calls, because `--headless` has no push-to-talk key for anybody
    // to hold. `data` rather than a shared directory for the reason the MCP server list is
    // scoped that way: a development run choosing to listen must not turn the production node's
    // microphone on.
    let voice = std::sync::Arc::new(zyris_voice::start(Some(&data)));
    tracing::info!(support = ?voice.describe(), "speech");

    // The slot a question about an unapproved peer waits in, built here because both ends of it
    // are built here: `peer_confirmer` below fills it from a tokio worker, and `gui::run` hands
    // the very same handle to the two commands the window answers through. A second `Pending`
    // would be a question nobody could answer.
    //
    // Built in both modes. Headless never installs the confirmer that fills it, so it stays empty
    // for that run — which costs an `Arc` and keeps this line out of the branch.
    let pending = confirm::Pending::new();

    // **Bound before the `Tools`, because what is announced depends on whether it bound.**
    // `block_on` rather than an async main: the GUI runtime owns the main thread synchronously,
    // and the endpoint's background work keeps running on the runtime's worker threads after
    // this returns.
    let transfers = match runtime.block_on(bind_transfers(mode, &data, root.clone(), &pending, &bus))
    {
        Ok(transfers) => Some(transfers),
        // How loudly this is said depends on *why* it failed, which is why it is not one line
        // here. See `report_no_peer_identity`.
        Err(error) => {
            report_no_peer_identity(&error);
            None
        }
    };

    // **Started before the `Tools`, for the same reason the endpoint is:** what is announced
    // depends on which of them are actually running, and a `Tools` is a list rather than a place
    // to start processes from. Nothing here can fail — a file that will not read and a command
    // that will not start are both logged where they happen and cost only themselves — so there
    // is no arm for it, and a machine with no MCP servers configured, which is most of them, gets
    // an empty list in silence.
    //
    // **`data`, so the list is the instance's.** Everything else `instance_name` reaches — the
    // keychain service, the lock, the audit log, the peer key — is scoped so that a `--server`
    // run is a different node from the production one. A server list read from anywhere shared
    // would put that back: a development run would start the machines the real account had
    // configured and announce them to a development server, and the other way round. That is why
    // `zyris_mcp::Config::path` takes a directory rather than finding one — the decision about
    // what this instance is belongs on this line, with the others.
    //
    // **Still before the window, and still one after another**, now that a server can join an
    // announcement that is already live. Letting them start in the background and announce
    // themselves as they came up would take the worst case — several misconfigured servers, ten
    // seconds each — off the startup path, and it was weighed rather than skipped. Three things
    // decided against it. `zyris.announce` is full replacement, so a node that grows capabilities
    // after connecting announces twice, and an agent that read the first one saw a machine with no
    // MCP tools and may already have acted on it; blocking is what makes the first announcement
    // the complete one. `Tools::announced()` is a snapshot taken once, on purpose, so that the
    // window reports what the node was given rather than a fresh look — servers arriving later
    // would make it permanently incomplete, and a screen that understates what this machine hands
    // out is worse than a slow start. And the delay is paid only by servers that are *broken*: a
    // server that works answers `initialize` in well under a second, and the ten is the ceiling
    // for one that never speaks at all.
    let started = runtime.block_on(zyris_mcp::config::start(&data));
    let promoted = started.running.clone();

    // Built here, once, for the same reason the connector is: the switch has to stop tools with
    // the window closed exactly as it does with it open, and a `Tools` per runtime would be two
    // switches and two logs that disagree.
    let mut tools = zyris_tools::Tools::new(
        zyris_tools::Gate::running(),
        zyris_tools::AuditLog::new(data.join(AUDIT_FILE)),
        root,
    )
    // Every call is published as well as written down. The bus is the only way the window and
    // the tray hear about a call while it happens; the file is what outlives the process.
    .with_bus(bus.clone())
    // Announced beside this machine's own, behind the same switch and the same log. Empty is the
    // ordinary case and says nothing. `as _` widens each `Promoted` to the trait `Tools` keeps
    // them by; what is dropped with the type — which tools a server did not announce, and why —
    // is the window's to show and belongs to whatever holds the servers themselves.
    .with_mcp(promoted.into_iter().map(|server| server as _).collect());
    if let Some(transfers) = &transfers {
        tools = tools.with_transfer(transfers);
    }
    // Built once, and the list below is named from it.
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

    // **The one handle on what this node announces**, and the reason it is built here rather than
    // inside the connector: the connector is not the only thing that changes it. A server a person
    // enables, one they turn off, and one whose process falls over all reach the same list, and
    // `run` builds a fresh node after recovering from a dead token — so a list captured at the
    // first build would have the second node announcing whatever was true at startup.
    let live = zyris_runtime::LiveCapabilities::new(capabilities);

    // What keeps that list honest about the MCP servers: a window's switch on one side, and a
    // process that died on the other. Built in **both** modes and watched in both — a headless
    // node is exactly the one nobody is looking at, and a capability announced over a process
    // that is gone is worse there than anywhere.
    let servers = zyris_tools::Servers::new(&tools, live.clone(), bus.clone(), started);
    runtime.spawn(servers.clone().watch());

    // The window's handle on the same list, taken before the connector takes its own. What the
    // Tools screen lists is read through this, so it cannot go on advertising a capability the
    // node has withdrawn.
    let window_live = live.clone();

    let mut connector = zyris_runtime::connection::Connector::new(identity, bus.clone())
        .with_capabilities(live);

    // The window's handle on file transfer, taken before the hook below consumes the value.
    //
    // A clone rather than a second `Transfers`: every clone is the same wiring — the same
    // endpoint, the same ledger, the same inbox — so what the Tools screen lists is the
    // capability's own answer rather than a second reader's idea of where files land. `None` is
    // the machine that has no peer identity, where there is no inbox to read and nothing can
    // arrive; the window says that rather than showing an empty list.
    let window_transfers = transfers.clone();

    // The other half of file transfer, and the reason `Connector` has a hook at all. There is
    // room for exactly one, which is why everything per-connection happens inside this one call:
    // replacing the rendezvous client, republishing where this machine can be reached, and — the
    // first time only — starting the accept loop.
    if let Some(transfers) = transfers {
        connector = connector.add_connect_hook(move |connection| {
            let transfers = transfers.clone();
            async move { transfers.on_connect(connection).await }
        });
    }

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
        mode @ (cli::Mode::Window | cli::Mode::WindowHidden) => gui::run(
            bus,
            runtime.handle().clone(),
            connector,
            tools,
            // What that `Tools` describes when the window asks. The same handle the supervisor
            // and the connector hold, so the screen and the node cannot disagree.
            window_live,
            // The other end of the slot `peer_confirmer` fills. The window reads and answers
            // through this handle; it is not a copy.
            pending,
            // What the Tools screen lists the inbox from, and the only reason the window has any
            // handle on transfer at all.
            window_transfers,
            // The MCP servers, so the window can list them and move their switches. A clone of
            // the same supervisor the watcher above is running, not a second one: two would be
            // two opinions about which server died.
            servers,
            // Speech. The window is the only place the switch that opens a microphone lives,
            // and the only place the key that starts a turn can be pressed.
            voice,
            instance,
            mode,
            // Not the URL, only whether there was one: the window needs this to decide whether
            // to register the single-instance plugin, and nothing else about the server.
            cli.server().is_some(),
        ),
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
/// Naming the lock is only half of that. The other half is in `gui.rs`: `tauri-plugin-single-
/// instance` keys on the bundle identifier rather than on this name, so a windowed `--server`
/// run skips registering it. Both halves are needed, and neither works alone.
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

/// Where everything this run owns on disk lives: beside the other per-user state, never beside
/// the binary. The audit log, this machine's peer key, the ledger of peers it has pinned, the
/// inbox and the undo stash are all under here.
///
/// Scoped by the instance for the same reason the keychain is — a `--server` run must not append
/// its calls to the production machine's history, nor read that history back as its own, nor
/// answer to the production machine's peer identity.
///
/// The fallbacks mirror `SecretStore`'s, and for the same reason — the current directory is `/`
/// under a systemd unit and whatever a shortcut set for a desktop launch, so anything written
/// there lands somewhere different every launch. The last resort is a directory rather than a
/// prefixed file name, because there is now more than one file to put in it.
fn data_dir(instance: &str) -> std::path::PathBuf {
    if let Some(dirs) = directories::ProjectDirs::from("cc", "attacca", instance) {
        return dirs.data_dir().to_path_buf();
    }
    if let Some(dirs) = directories::BaseDirs::new() {
        return dirs.home_dir().join(format!(".{instance}"));
    }
    // No directory the platform can name. `AuditLog` survives a path it cannot write — it says
    // so in the process log and never fails a tool call — so an absolute, OS-chosen path is a
    // better last resort than refusing to start.
    std::env::temp_dir().join(instance)
}

/// Says why this machine has no peer identity, at a level that matches what actually went wrong.
///
/// **The whole chain, not just its outermost link.** `zyris-tools` wraps the real reason in a
/// `with_context`, and `%error` is tracing's non-alternate `Display` sigil — it renders the
/// context alone, so the field read `could not load this machine's peer key from <path>` and the
/// words "permission", "644" and "0600" appeared under no `RUST_LOG` at all. `{:#}` renders the
/// context and its causes on one line, in order.
///
/// **And one level is wrong for all of them.** A machine with no network is an ordinary machine,
/// which is what `info!` was chosen for and is still right about; a private key this computer's
/// other users can read is not an ordinary machine, and neither is a key file that will not parse.
///
/// The second-order harm is why those two are `error!` and why both messages say *not* to delete
/// the file. An operator who only sees "no peer identity" reaches for `rm iroh-secret.key`, which
/// works, and makes this a **different peer permanently**: the key is the identity, so every
/// machine that pinned this one refuses it from then on — and only when it sends, which is a long
/// way from the thing that was deleted.
fn report_no_peer_identity(error: &anyhow::Error) {
    // The reason is a cause rather than the top of the chain, which is exactly why `{:#}` is used
    // to render it and `downcast_ref` to classify it.
    match error.downcast_ref::<zyris_tools::KeyError>() {
        Some(zyris_tools::KeyError::Permissions(mode)) => tracing::error!(
            error = format!("{error:#}"),
            mode = format!("{mode:04o}"),
            "this machine's peer key can be read by someone other than you, so it was not loaded and file_transfer is not announced; narrow the file's permissions to 0600 rather than deleting it — deleting it does stop the message, by making this machine a different peer that every machine which pinned it will refuse"
        ),
        Some(zyris_tools::KeyError::Malformed) => tracing::error!(
            error = format!("{error:#}"),
            "this machine's peer key file is not a key, so it was not loaded and file_transfer is not announced; restore it from wherever this machine's data directory is backed up — deleting it starts a new identity, and every machine that pinned this one will refuse it from then on, only when it sends"
        ),
        // Everything else: a socket that would not bind, a relay URL that is not one, an I/O error
        // reading the key. `info!`, not `warn!`, for the reason `zyris-tools` gives about a host
        // with no display server — a machine with no network is an ordinary machine rather than a
        // fault, and a warning on every launch of one teaches people to ignore warnings. Nothing
        // about transfer is announced, the other four capabilities are unaffected, and
        // `Tools::announced()` reports exactly that.
        _ => tracing::info!(
            error = format!("{error:#}"),
            "no peer identity, so file_transfer is not announced; everything else on this machine still works, but it can neither send a file to another of your machines nor receive one"
        ),
    }
}

/// File transfer, bound onto the confirmer this run is entitled to.
///
/// **One line of work, and it is a function so that the line is covered.** `Transfers::bind` is
/// what hands a confirmer to `LocalFileTransfer`, and until this existed the only expression that
/// decided *which* confirmer got there lived inside `main` — where nothing can reach it. Every
/// test built its own `peer_confirmer` and passed, so replacing this argument with
/// `Arc::new(DenyUnknown)` left the whole suite green while the windowed arm went dead: no
/// question published, no window raised, every send to an unpinned name refused, and nothing
/// anywhere to say so. `the_windowed_run_wires_its_own_confirmer_into_file_transfer` calls this
/// and asks the bound value itself who it would ask, which is the assertion that revert now fails.
///
/// Everything else about binding — the key, the socket, the ledger, the inbox — is
/// [`zyris_tools::Transfers::bind`]'s, and the caller still reports a failure through
/// [`report_no_peer_identity`] rather than this swallowing it.
async fn bind_transfers(
    mode: cli::Mode,
    data: &std::path::Path,
    root: std::path::PathBuf,
    pending: &confirm::Pending,
    bus: &EventBus,
) -> anyhow::Result<zyris_tools::Transfers> {
    zyris_tools::Transfers::bind(data, root, peer_confirmer(mode, pending, bus)).await
}

/// Who answers when this machine is about to **send** a file to a peer it has never pinned.
///
/// **Sending only, and that is the whole of its reach.** The confirmer goes to exactly one place
/// — the `LocalFileTransfer` behind `file_transfer` — and `TofuStore::authorize` consults it on
/// the dial, for the one case a pin cannot settle on its own. Nothing on the receiving side asks
/// it anything: `serve_peers` admits a connection whose key is on the account's node list and
/// closes one whose key is not, and it only ever *reads* the ledger. So this is not a door on
/// incoming files, and replacing `DenyUnknown` here will not make it one — a file from another
/// node of this account arrives whether or not that node has ever been pinned.
///
/// **`DenyUnknown` in headless, and that is a real limitation rather than a gap.** With nobody to
/// ask, refusing is the only safe answer: a peer must not become trusted merely because no one was
/// around to say no. A `--headless` run therefore cannot send a file to a machine it has not
/// already pinned, and no amount of waiting changes that.
///
/// With a window there *is* somebody to ask. The windowed arm installs [`confirm::WindowConfirmer`]
/// instead, which parks the question in `pending` and publishes it; `gui.rs` is what puts the
/// window on the screen when that event goes past, and the window answers back through
/// [`bridge::answer_peer`] into the same slot.
///
/// **The question travels on the bus rather than through an `AppHandle`, and that is not
/// incidental.** This function runs before Tauri exists — `Transfers::bind` needs a confirmer, and
/// the app is not built until `gui::run` — so there is no window to hold onto here. The bus is
/// already the one thing both halves of this program can reach, and publishing on it gets the
/// question to the webview through the forwarder that is already running, for nothing.
///
/// Transiently, for the reason [`zyris_runtime::CoreEvent::NeedsPeerApproval`] gives: a question
/// answered a minute ago must not be handed to every window that opens afterwards.
fn peer_confirmer(
    mode: cli::Mode,
    pending: &confirm::Pending,
    bus: &EventBus,
) -> std::sync::Arc<dyn zyris_tools::PeerConfirmer> {
    match mode {
        cli::Mode::Headless => std::sync::Arc::new(zyris_tools::DenyUnknown),
        cli::Mode::Window | cli::Mode::WindowHidden => {
            let bus = bus.clone();
            std::sync::Arc::new(confirm::WindowConfirmer::new(
                pending.clone(),
                std::sync::Arc::new(move |question: &confirm::Question| {
                    bus.publish_transient(bridge::peer_question_event(question));
                }),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn headless_refuses_an_unknown_peer_without_asking_anybody() {
        // The arm that must never grow a window. There is nobody at a `--headless` run to read a
        // fingerprint, so the answer is no — and it has to be *immediately* no: a headless
        // confirmer that parked the question somewhere would block an agent's `send_to` for three
        // quarters of a minute and then refuse it anyway.
        let bus = EventBus::new(8);
        let pending = confirm::Pending::new();
        let mut watching = bus.subscribe();

        let confirmer = peer_confirmer(cli::Mode::Headless, &pending, &bus);
        let answer = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            confirmer.confirm("kitchen-pi", "9F2A 41C7 0E83 BB15 6D04 A97E 22C1 5FB8"),
        )
        .await
        .expect("headless has to answer at once rather than wait for somebody");

        assert!(!answer, "nobody is there, so nothing may be approved");
        assert!(pending.question().is_none(), "headless must not park a question anywhere");
        assert!(
            watching.try_recv().is_err(),
            "there is no window to publish a question to, and a tray-less run has no surface"
        );
    }

    #[tokio::test]
    async fn a_windowed_run_asks_and_says_so_on_the_bus() {
        // The seam this step opens. Both halves are asserted because either alone is useless: a
        // question parked where the commands can reach it, and an event so that something raises
        // the window and draws it.
        const FINGERPRINT: &str = "9F2A 41C7 0E83 BB15 6D04 A97E 22C1 5FB8";
        let bus = EventBus::new(8);
        let pending = confirm::Pending::new();
        let mut watching = bus.subscribe();

        let confirmer = peer_confirmer(cli::Mode::WindowHidden, &pending, &bus);
        let asked = tokio::spawn(async move { confirmer.confirm("kitchen-pi", FINGERPRINT).await });

        let published = tokio::time::timeout(std::time::Duration::from_secs(1), watching.recv())
            .await
            .expect("a windowed run publishes the question rather than refusing it")
            .unwrap();
        let zyris_runtime::CoreEvent::NeedsPeerApproval { id, label, fingerprint } = published
        else {
            panic!("a question must be published as one: {published:?}");
        };
        assert_eq!(label, "kitchen-pi");
        assert_eq!(fingerprint, FINGERPRINT, "the person compares this character by character");

        // And the same question is reachable by the command a window that opened late calls, at
        // the same id the answer will name.
        let waiting = pending.question().expect("the question is parked for a late window");
        assert_eq!(waiting.id, id);
        assert_eq!(waiting.fingerprint, FINGERPRINT);

        assert!(pending.answer(id, true), "the id on the wire is the id an answer names");
        assert!(asked.await.unwrap(), "and the answer reaches the caller");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_windowed_run_wires_its_own_confirmer_into_file_transfer() {
        // **The wiring, not the factory.** `a_windowed_run_asks_and_says_so_on_the_bus` above
        // proves `peer_confirmer` builds the right thing; it says nothing about whether the value
        // `file_transfer` actually holds came from there. That was one expression inside `main`,
        // which no test could reach — so swapping it for `Arc::new(DenyUnknown)` killed the whole
        // feature and broke not one assertion. This asks the bound `Transfers` itself.
        let data = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let bus = EventBus::new(8);
        let pending = confirm::Pending::new();

        let transfers = bind_transfers(
            cli::Mode::WindowHidden,
            data.path(),
            root.path().to_path_buf(),
            &pending,
            &bus,
        )
        .await
        .expect("binding a peer identity in a temporary directory");

        // Behaviour rather than a type check: there is no way to ask an `Arc<dyn PeerConfirmer>`
        // what it is, and the thing that matters is what it *does* anyway. `DenyUnknown` answers
        // no at once and parks nothing; the windowed confirmer parks a question where the window's
        // commands can reach it and waits for a person.
        let confirmer = transfers.peering().confirmer();
        let asked = tokio::spawn(async move {
            confirmer.confirm("kitchen-pi", "9F2A 41C7 0E83 BB15 6D04 A97E 22C1 5FB8").await
        });

        let mut waiting = None;
        for _ in 0..2000 {
            if let Some(question) = pending.question() {
                waiting = Some(question);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        let waiting = waiting.expect(
            "the confirmer behind file_transfer refused without asking anybody, which is what a \
             windowed run installing DenyUnknown looks like from the outside",
        );
        assert_eq!(waiting.label, "kitchen-pi");

        assert!(pending.answer(waiting.id, true), "and the window's answer reaches it");
        assert!(asked.await.unwrap());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_headless_run_wires_the_refusal_into_file_transfer() {
        // The converse, so the test above cannot pass by asserting something true of both arms.
        // `--headless` has nobody to ask, and a confirmer that parked a question there would
        // block an agent's `send_to` for three quarters of a minute and refuse it anyway.
        let data = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let bus = EventBus::new(8);
        let pending = confirm::Pending::new();

        let transfers = bind_transfers(
            cli::Mode::Headless,
            data.path(),
            root.path().to_path_buf(),
            &pending,
            &bus,
        )
        .await
        .expect("binding a peer identity in a temporary directory");

        let answer = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            transfers.peering().confirmer().confirm("kitchen-pi", "9F2A 41C7 0E83 BB15 6D04 A97E 22C1 5FB8"),
        )
        .await
        .expect("headless has to answer at once rather than wait for somebody");

        assert!(!answer, "nobody is there, so nothing may be approved");
        assert!(pending.question().is_none(), "headless must not park a question anywhere");
    }

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
    fn each_instance_reads_its_own_mcp_server_list() {
        // The list of MCP servers is per-instance like everything else this run owns, and for the
        // same reason: a `--server` run must not start the production machine's servers and
        // announce them to a development server, nor the other way round. `Config::path` takes a
        // directory rather than finding one so that this is decided once, where the instance is.
        let production = zyris_mcp::Config::path(&data_dir(&instance_name(None)));
        let development = zyris_mcp::Config::path(&data_dir(&instance_name(Some(
            "ws://127.0.0.1:8080/zyris/v1/ws",
        ))));

        assert_ne!(production, development);
        // And it lands beside the rest of that instance's state rather than beside the binary or
        // in whatever directory Zyris happened to be started from.
        assert_eq!(production.parent().unwrap(), data_dir("zyris"));
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
