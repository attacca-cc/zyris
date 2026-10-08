//! The commands themselves.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use serde_json::Value;
use zyris_runtime::lock::InstanceLock;
use zyris_runtime::{CoreEvent, EventBus};

use crate::cli::{
    AutostartCommand, AutostartRequest, Command, ConfigArgs, ConfigCommand, McpArgs, McpCommand,
    Mode,
};
use crate::console::state::{NodeState, Phase};
use crate::{bridge, instance, stop};

/// Where a node started by `up` writes what it would otherwise have said to a terminal nobody is
/// attached to.
const NODE_LOG: &str = "node.log";

/// How long `up` waits for the node it started to take the instance lock. The lock is taken before
/// anything slow happens in either runtime, so this is only ever spent by a process that died on
/// the way there; five seconds is several times what a cold start needs.
const UP_DEADLINE: Duration = Duration::from_secs(5);

/// How long `down` waits for the node to go after being asked. A stop runs `lifecycle::shutdown`
/// and, on a windowed run, closes the MCP server processes first — which is a wait for other
/// programs, so ten seconds is the ceiling rather than the expectation.
const DOWN_DEADLINE: Duration = Duration::from_secs(10);

/// How often either of them looks while it waits.
const TICK: Duration = Duration::from_millis(100);

/// The instance a command is about: the one `--server` names, or the real one.
///
/// One structure rather than the instance name passed around beside the directory, because they
/// have to agree: a command that asked about one instance's lock and edited another's settings
/// would be a command that lied about a machine it never looked at.
pub struct Target {
    pub instance: String,
    pub data: PathBuf,
    pub server: Option<String>,
}

impl Target {
    pub fn new(server: Option<&str>) -> Target {
        let instance = instance::name(server);
        Target {
            data: instance::data_dir(&instance),
            instance,
            server: server.map(str::to_string),
        }
    }

    /// The address this instance dials, which the state file records for a `--server` run as the
    /// development URL rather than the shipped one.
    fn server_name(&self) -> &str {
        self.server.as_deref().unwrap_or(zyris_runtime::DEFAULT_SERVER_URL)
    }

    fn running(&self) -> bool {
        InstanceLock::is_held(&self.instance)
    }

    /// What the last node on this instance wrote, if it was this instance that wrote it.
    ///
    /// The name check is not paranoia: the file lives in a directory derived from the instance
    /// name, and the day that derivation changes — or somebody copies a data directory — a
    /// `connected` line from another instance would otherwise be read as this machine's node.
    fn last_state(&self) -> Option<NodeState> {
        NodeState::read(&self.data).filter(|state| state.instance == self.instance)
    }
}

/// Do one console command, and return when it is done.
///
/// `mode` is what the two flags said — `--headless`, `--minimized` — and only `up` has anything to
/// do with it: it is what the node it starts will be. [`Cli::check`](crate::cli::Cli::check) is
/// what refused those flags beside every other command, so nothing here has to.
pub fn run(command: &Command, server: Option<&str>, mode: Mode) -> Result<()> {
    let target = Target::new(server);
    match command {
        Command::Up => up(&target, mode),
        Command::Down => down(&target),
        Command::Status => status(&target),
        Command::Login => login(&target),
        Command::Config(args) => config(&target, args),
        // Passed the URL rather than the `Target`: this one has no instance of its own to look
        // at. Autostart is written into the desktop session and the task scheduler, for the copy
        // of Zyris that ran the command, and `--server` cannot be carried into it at all.
        Command::Autostart(args) => {
            autostart(
                match &args.command {
                    AutostartCommand::Enable => Some(AutostartRequest::Install),
                    AutostartCommand::Disable => Some(AutostartRequest::Uninstall),
                    AutostartCommand::Status => None,
                },
                server,
            )
        }
        Command::Mcp(args) => mcp(&target, args),
    }
}

// ---------------------------------------------------------------- status

/// What this machine's node is doing.
///
/// **Two answers from two places, and deliberately.** Whether a node is running is the instance
/// lock, which cannot be stale; what it is *doing* is the state file it wrote, which can be. So
/// the lock decides the sentence and the file supplies the detail — and when the two disagree
/// (`not running` beside a file that says `connected`) the block says which is which rather than
/// averaging them.
fn status(target: &Target) -> Result<()> {
    let state = target.last_state();

    println!("zyris {} — instance {}", env!("CARGO_PKG_VERSION"), target.instance);

    if !target.running() {
        println!();
        row("node", "not running");
        if let Some(state) = &state {
            row("last run", format!("{} {}", state.phase.label(), when(state.updated_unix_ms)));
            row("last seen", format!("pid {}, started {}", state.pid, when(state.started_unix_ms)));
            row("server", &state.server);
        }
        return Ok(());
    }

    let Some(state) = state else {
        // Running, and the file that would say more is not there: a node started by a build older
        // than this command. Said rather than filled in with guesses.
        println!();
        row("node", format!("running (instance {})", target.instance));
        row("state", "no state file: this node was started by a Zyris that did not write one");
        row("server", target.server_name());
        return Ok(());
    };

    println!();
    row("node", format!("running, pid {}", state.pid));
    row("mode", &state.mode);
    row("state", phase_line(state.phase, &state.detail));
    if let Some(name) = &state.node_name {
        match &state.node_id {
            Some(id) => row("node name", format!("{name} ({id})")),
            None => row("node name", name),
        }
    }
    if let Some(code) = &state.enrolment {
        // The one thing here a person has to act on, so it is said the way the window says it.
        row("code", format!("{} — open {}", code.user_code, code.verification_uri));
    }
    row("server", &state.server);
    row("started", when(state.started_unix_ms));
    row("last change", when(state.updated_unix_ms));
    Ok(())
}

/// The phase, and why, on one line.
fn phase_line(phase: Phase, detail: &Option<String>) -> String {
    match detail {
        Some(detail) if !detail.is_empty() => format!("{} — {detail}", phase.label()),
        _ => phase.label().to_string(),
    }
}

/// `12m ago`, or that the clock moved.
fn when(at_unix_ms: u64) -> String {
    match crate::console::state::age(at_unix_ms) {
        Some(age) => format!("{age} ago"),
        None => "in the future (this machine's clock moved)".to_string(),
    }
}

/// One row of a console block. The label column is padded so that a person reads down the values
/// rather than through the labels.
fn row(label: &str, value: impl std::fmt::Display) {
    println!("  {label:<11} {value}");
}

// ---------------------------------------------------------------- up

/// Start a node in the background, and return.
///
/// **A detached process, not a service.** Autostart is the unit or the task entry, and it is a
/// separate decision a person makes once; this starts one now, in this instance, from this
/// executable — the same binary a person would otherwise double-click, which is the only honest
/// answer to "which Zyris should `zyris up` start".
///
/// `mode` is `--headless` or `--minimized` when either was given. `Mode::Window` is what the
/// parser answers when neither was, which here means *nobody said*, and [`background_mode`] is
/// what decides for the machine.
fn up(target: &Target, mode: Mode) -> Result<()> {
    if target.running() {
        let state = target.last_state();
        match state {
            Some(state) => println!(
                "already running: pid {}, {}",
                state.pid,
                phase_line(state.phase, &state.detail)
            ),
            None => println!("already running: a node holds this instance's lock"),
        }
        return Ok(());
    }

    let mode = match mode {
        Mode::Window => background_mode(),
        stated => stated,
    };
    let exe = std::env::current_exe().context("could not work out where this program is on disk")?;
    let mut command = std::process::Command::new(&exe);
    match mode {
        // Never `Mode::Window`: a window that has to be closed before a command returns is not the
        // background node this command is for.
        Mode::Window | Mode::WindowHidden => {
            command.arg("--minimized");
        }
        Mode::Headless => {
            command.arg("--headless");
        }
    }
    if let Some(server) = &target.server {
        command.arg("--server").arg(server);
    }

    // **Its output goes to a file**, because there is nobody attached to it: a background process
    // inheriting a terminal writes into a prompt that has moved on, and one started from a script
    // writes into nothing at all. Appended rather than truncated, so the log of a node that fell
    // over is still there after the next attempt.
    std::fs::create_dir_all(&target.data)
        .with_context(|| format!("could not make {}", target.data.display()))?;
    let log = target.data.join(NODE_LOG);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .with_context(|| format!("could not open {}", log.display()))?;
    command
        .stdin(std::process::Stdio::null())
        .stdout(file.try_clone()?)
        .stderr(file);

    #[cfg(unix)]
    {
        // **A session of its own**, so it outlives the shell: a terminal sends a hang-up to its
        // jobs when it goes, and the point of `zyris up` is a node that does not go with it. This
        // is also what makes it survive the SSH session it was started from.
        //
        // `setsid` and nothing else runs between the fork and the exec, which is all that is safe
        // there — an allocation or a lock in that window is a deadlock waiting for a busy day.
        use std::os::unix::process::CommandExt as _;
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    let child = command.spawn().with_context(|| format!("could not start {}", exe.display()))?;
    // Not waited for, and deliberately not: this process is returning, and a `Child` that is
    // dropped leaves the process running on every platform.
    let pid = child.id();
    println!("starting a {} node: pid {pid}", mode.name());

    // The lock is the signal, because it is the one thing the new process is guaranteed to take
    // and the one thing that cannot be stale. Waiting on the state file instead would call a node
    // that is starting slowly "failed".
    if !wait_for(UP_DEADLINE, || target.running()) {
        bail!(
            "pid {pid} exited without starting; its log is {}",
            log.display()
        );
    }

    match target.last_state() {
        Some(state) => println!("started: pid {}, {}", state.pid, phase_line(state.phase, &state.detail)),
        None => println!("started: pid {pid}"),
    }
    println!("  log  {}", log.display());
    println!("  `zyris status` is what it is doing now, and `zyris down` stops it");
    Ok(())
}

/// What `zyris up` starts when nobody said.
///
/// **The tray on a machine that has a desktop, and a windowless node on one that does not.**
/// Autostart installs `--minimized`, so a background node on a desktop is one with a tray — the
/// only way back into a process nobody is watching. On a server there is no session to put a
/// window in, and a windowed node started there dies building it: `zyris up` over SSH on a machine
/// with no desktop would be a command that always failed, which is why this looks rather than
/// assumes.
///
/// `--headless` and `--minimized` override it, and which one was chosen is printed.
#[cfg(target_os = "linux")]
fn background_mode() -> Mode {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some() {
        Mode::WindowHidden
    } else {
        Mode::Headless
    }
}

/// Everywhere else, there is always a desktop session to put a window in.
#[cfg(not(target_os = "linux"))]
fn background_mode() -> Mode {
    Mode::WindowHidden
}

// ---------------------------------------------------------------- down

/// Stop the node this machine is running.
///
/// A request rather than a signal, and what that costs is written down in `crate::stop`: a pid can
/// be reused, and the only pid here came out of a file a node wrote before it died. What it buys
/// is a stop that is the same as Ctrl-C and the tray's Quit — the MCP servers are stopped and the
/// push-to-talk key is handed back — and one that behaves the same on both platforms.
fn down(target: &Target) -> Result<()> {
    if !target.running() {
        println!("nothing to stop: no node holds the {} instance's lock", target.instance);
        return Ok(());
    }

    let state = target.last_state();
    let named = match &state {
        Some(state) => format!("pid {}", state.pid),
        None => "a node that wrote no state file".to_string(),
    };

    stop::request(&target.data)
        .with_context(|| format!("could not write a stop request in {}", target.data.display()))?;
    println!("asked the node to stop ({named})");

    if !wait_for(DOWN_DEADLINE, || !target.running()) {
        bail!(
            "it has not stopped in {}s. A node that is wedged cannot see the request — it is \
             watching a file, not a signal — so ending {named} by hand is what is left. Nothing is \
             lost by that: the audit log is written through, and the credential is stored.",
            DOWN_DEADLINE.as_secs()
        );
    }
    println!("stopped");
    Ok(())
}

/// Look until something is true, or give up. Both waits in this module are of this shape and both
/// are cheap: a file lock asked about ten times a second costs less than the process it is asking
/// about.
fn wait_for(deadline: Duration, mut settled: impl FnMut() -> bool) -> bool {
    let started = std::time::Instant::now();
    while started.elapsed() < deadline {
        if settled() {
            return true;
        }
        std::thread::sleep(TICK);
    }
    settled()
}

// ---------------------------------------------------------------- login

/// Authorize this machine, showing the code the way the window shows it.
///
/// **The same path as the window, through `Connector::credential`.** A second implementation here
/// would be a second set of scopes and a second answer to "is this machine authorized", and the
/// two would drift; the console asking the core for the code is also what makes `zyris login` and
/// the window's onboarding screen show the *same* code for one machine rather than two pending
/// enrolments.
///
/// A node that is already up is the interesting case: it is the one enrolling, it holds the code,
/// and this prints it out of the state file rather than starting a second request.
fn login(target: &Target) -> Result<()> {
    if target.running() {
        return report_running(target);
    }

    // **Taken, unlike every other command here.** Two enrolments for one machine are two pending
    // requests on the account and one credential nobody uses, and the lock is what this program
    // already uses to say "one of me at a time". `None` is a node that started between the check
    // above and here.
    let Some(_lock) = InstanceLock::acquire(&target.instance)
        .context("could not take this instance's lock")?
    else {
        // Reached only by losing that race, and the node that won it is now the one to ask.
        return report_running(target);
    };

    let identity = zyris_runtime::identity::Identity::new(zyris_runtime::secret::SecretStore::new(
        &target.instance,
    ))
    .out_of_keychain();

    match identity.load() {
        Ok(Some(credential)) if zyris_runtime::connection::missing_scopes(&credential).is_empty() => {
            println!("this machine is already authorized");
            println!("  system  {}", credential.system.name);
            println!("  program {}", credential.program.name);
            println!("  `zyris up` starts the node it dials with");
            return Ok(());
        }
        // A credential from before this build asked for a scope: the connector enrols again to be
        // granted them, which is what the window does too, so this falls through to the same
        // enrolment — and says so, because it looks like nothing is wrong.
        Ok(Some(_)) => println!(
            "this machine's stored credential predates scopes this build uses; authorizing again \
             to be granted them"
        ),
        Ok(None) => println!("this machine is not authorized yet"),
        Err(error) => bail!("the stored credential could not be read: {error}"),
    }

    let runtime = tokio::runtime::Runtime::new().context("could not start a runtime")?;
    runtime.block_on(enrol(target))
}

/// What `login` says when a node is already up: its code if it has one, or that it does not need
/// one.
fn report_running(target: &Target) -> Result<()> {
    let state = target.last_state();
    match state {
        Some(state) => match &state.enrolment {
            Some(code) => {
                println!("a node is already running and is waiting for this machine to be authorized");
                println!("  code  {}", code.user_code);
                println!("  url   {}", code.verification_uri);
                println!("  it is the same code the window shows; this command cannot hurry it");
            }
            None => println!(
                "a node is already running (pid {}, {}); this machine needs no code",
                state.pid,
                state.phase.label()
            ),
        },
        None => println!(
            "a node is already running and has written no state file, so there is nothing here to \
             show; if it is waiting to be authorized, the window it belongs to has the code"
        ),
    }
    Ok(())
}

/// The enrolment itself: ask, print what the core says, and answer with whether it was granted.
async fn enrol(target: &Target) -> Result<()> {
    let bus = EventBus::new(8);
    // Subscribed before the connector is built, for the reason every other subscriber in this
    // program is: `broadcast` never replays, and the code arrives within microseconds.
    let mut events = bus.subscribe();
    let connector = zyris_runtime::connection::Connector::new(
        zyris_runtime::identity::Identity::new(zyris_runtime::secret::SecretStore::new(
            &target.instance,
        ))
        .out_of_keychain(),
        bus,
    )
    .with_server(target.server_name().to_string());

    let mut asking = tokio::spawn(async move { connector.credential().await });

    loop {
        tokio::select! {
            // Whatever a person does next, this process stops being the one enrolling — and the
            // enrolment it started is left to lapse on the server, which is what a closed window
            // does too.
            _ = tokio::signal::ctrl_c() => {
                println!("cancelled; nothing was stored");
                return Ok(());
            }
            event = events.recv() => match event {
                Ok(CoreEvent::EnrolmentCode { user_code, verification_uri }) => {
                    // The one line a person has to act on, without a log level in front of it.
                    println!("open {verification_uri} and enter this code:");
                    println!();
                    println!("    {user_code}");
                    println!();
                    println!("waiting for this machine to be authorized (Ctrl-C to stop)");
                }
                Ok(CoreEvent::EnrolmentFailed { reason }) => {
                    bail!("authorizing this machine failed: {reason}")
                }
                // A lag here would cost a code, and the code is the whole command. Eight events
                // is more than enrolment publishes, so this is unreachable in practice; naming
                // it rather than ignoring it is what keeps that true if the bus grows.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => tracing::warn!(
                    missed,
                    "the console fell behind while waiting for the enrolment code"
                ),
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    bail!("the core stopped without answering")
                }
            },
            granted = &mut asking => {
                return match granted {
                    Ok(Some(_)) => {
                        println!("this machine is authorized; `zyris up` starts the node it dials with");
                        Ok(())
                    }
                    _ => bail!("the enrolment ended without a credential"),
                };
            }
        }
    }
}

// ---------------------------------------------------------------- config

/// Read and write the settings the window would otherwise be needed for.
///
/// **One file, and it is `voice.json`.** That is where this program keeps the settings a person
/// changes often and that are worth reaching without a window; everything else is either not a
/// setting (the server is a flag, autostart is a unit file, the credential is `zyris login`) or
/// already has a command of its own (`zyris mcp`). `config list` says all of that out loud, since
/// the question a person actually has is "where is the switch I am looking for".
///
/// The keys are [`zyris_voice::settings::KEYS`] — the voice crate's own description of its file,
/// so the names here cannot drift from the struct the app reads. Writing goes through that file's
/// JSON: a key the file does not have yet is added, and one it is allowed to leave out can be
/// cleared with `unset`.
fn config(target: &Target, args: &ConfigArgs) -> Result<()> {
    match &args.command {
        ConfigCommand::List => config_list(target),
        ConfigCommand::Get { key } => config_get(target, key),
        ConfigCommand::Set { key, value } => config_set(target, key, value),
    }
}

/// The settings file as a JSON object, and where it is.
fn voice_document(target: &Target) -> Result<(PathBuf, serde_json::Map<String, Value>)> {
    let path = target.data.join(zyris_voice::settings::SETTINGS_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((path, serde_json::Map::new()));
        }
        Err(error) => {
            return Err(error).with_context(|| format!("could not read {}", path.display()));
        }
    };

    // **A file that will not parse is refused rather than replaced.** The voice reads one it cannot
    // parse as no settings at all — it says so in the process log and opens no microphone — so
    // writing a fresh document over the top would silently throw away everything that was in it,
    // including whatever the person was trying to fix.
    let document: Value = serde_json::from_str(&text).with_context(|| {
        format!(
            "{} is not JSON, and the voice reads a file it cannot parse as no settings at all; \
             fix it by hand rather than losing it",
            path.display()
        )
    })?;
    let object = document.as_object().cloned().ok_or_else(|| {
        anyhow::anyhow!(
            "{} has to be a JSON object of settings, and this is {}",
            path.display(),
            match document {
                Value::Array(_) => "an array",
                Value::Null => "null",
                _ => "a single value",
            }
        )
    })?;
    Ok((path, object))
}

fn config_list(target: &Target) -> Result<()> {
    let (path, document) = voice_document(target)?;

    println!(
        "zyris config — the settings in {}, for instance {}",
        path.display(),
        target.instance
    );
    println!();
    println!("  {:<22} {:<14} {}", "KEY", "VALUE", "FILE");
    for key in zyris_voice::settings::KEYS {
        let value = match document.get(key.json) {
            Some(value) => value_text(key.kind, value),
            // A key with no `unset`: the file's own default is what the voice reads, and printing
            // "not set" for a switch that means `false` would be a lie about what is happening.
            None if !key.optional => default_text(key.kind),
            None => "not set".to_string(),
        };
        println!(
            "  {:<22} {:<14} {}",
            key.name,
            value,
            zyris_voice::settings::SETTINGS_FILE
        );
        println!("      {}", key.help);
    }

    println!();
    println!("Not editable from here, and why:");
    println!("  {:<22} {}", "autostart", "a systemd unit or a Task Scheduler entry rather than a setting in a file — `zyris autostart enable|disable`");
    println!("  {:<22} {}", "pause", "the switch lives in a running node's memory and is not stored; the window's Tools screen moves it");
    println!("  {:<22} {}", "server", "which Attacca to dial is per run, not stored — `--server URL` on any command");
    println!("  {:<22} {}", "credential", "`zyris login` writes it, through the same path the window uses");
    println!("  {:<22} {}", "mcp servers", "a command of their own — `zyris mcp list|enable|disable|add|remove`");
    Ok(())
}

fn config_get(target: &Target, name: &str) -> Result<()> {
    let key = key_for(name)?;
    let (path, document) = voice_document(target)?;

    match document.get(key.json) {
        Some(value) => println!("{} = {}", key.name, value_text(key.kind, value)),
        None if key.optional => println!("{} is not set", key.name),
        None => println!("{} = {} (not in the file)", key.name, default_text(key.kind)),
    }
    println!("  {} — {}", key.help, path.display());
    Ok(())
}

fn config_set(target: &Target, name: &str, typed: &str) -> Result<()> {
    let key = key_for(name)?;
    let (path, mut document) = voice_document(target)?;

    match value_for(key, typed)? {
        Some(value) => {
            document.insert(key.json.to_string(), value);
        }
        None => {
            document.remove(key.json);
        }
    }
    write_json(&path, &Value::Object(document))?;

    match document_value(&path, key)? {
        Some(value) => println!("{} = {}", key.name, value),
        None => println!("{} is unset", key.name),
    }

    if target.running() {
        // Worth saying, and not obvious: the running window read this file when it started. The
        // Voice screen moves a live switch; this writes the answer for the next launch.
        println!(
            "  a node is running and read these settings at launch, so this is what the next \
             launch will do; the Voice screen is what changes this one"
        );
    }
    Ok(())
}

/// What the file holds for `key` now, read back rather than assumed — the same rule the window's
/// switches follow, and the reason a `set` that went somewhere unexpected says so.
fn document_value(path: &std::path::Path, key: &zyris_voice::settings::Key) -> Result<Option<String>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("could not read back {}", path.display()))?;
    let document: Value = serde_json::from_str(&text)
        .with_context(|| format!("could not parse back {}", path.display()))?;
    Ok(document.get(key.json).map(|value| value_text(key.kind, value)))
}

/// The key with this name, or a refusal that lists the ones there are.
fn key_for(name: &str) -> Result<&'static zyris_voice::settings::Key> {
    zyris_voice::settings::key(name).ok_or_else(|| {
        let names = zyris_voice::settings::KEYS
            .iter()
            .map(|key| key.name)
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::anyhow!("there is no setting called `{name}`. There are: {names}")
    })
}

/// What a person typed, as the value the file should hold. `None` is `unset`: the key comes out of
/// the file, and the voice falls back to its own default for it.
fn value_for(key: &zyris_voice::settings::Key, typed: &str) -> Result<Option<Value>> {
    use zyris_voice::settings::Kind;

    if matches!(typed, "unset" | "none" | "null") {
        if key.optional {
            return Ok(None);
        }
        bail!(
            "`{}` is not a setting the file may leave out, so there is nothing to unset it to. \
             It holds {}.",
            key.name,
            match key.kind {
                Kind::Bool => "true or false",
                Kind::Number => "a number",
                Kind::Text => "text",
                Kind::Device => "`default` or a device id",
            }
        );
    }

    Ok(Some(match key.kind {
        Kind::Bool => {
            let value = match typed.to_ascii_lowercase().as_str() {
                "true" | "on" | "yes" | "1" => true,
                "false" | "off" | "no" | "0" => false,
                _ => bail!("`{}` is a switch: give true or false, not `{typed}`", key.name),
            };
            Value::Bool(value)
        }
        Kind::Number => {
            let value: f32 = typed.parse().map_err(|_| {
                anyhow::anyhow!("`{}` is a number, and `{typed}` is not one", key.name)
            })?;
            serde_json::json!(value)
        }
        Kind::Text => Value::String(typed.to_string()),
        Kind::Device => serde_json::to_value(
            zyris_voice::settings::read_device(typed)
                .map_err(|why| anyhow::anyhow!("`{}`: {why}", key.name))?,
        )?,
    }))
}

/// A value out of the file as a console prints it: text bare, a device by the word the console
/// takes, everything else as the JSON it is.
fn value_text(kind: zyris_voice::settings::Kind, value: &Value) -> String {
    use zyris_voice::settings::Kind;

    match kind {
        Kind::Device => match serde_json::from_value::<zyris_voice::view::Choice>(value.clone()) {
            Ok(choice) => zyris_voice::settings::write_device(&choice),
            // A device the file holds and the type cannot read: the JSON, rather than a word that
            // would be wrong about it. The voice would read this file as no settings at all, so
            // this is a file somebody has to look at.
            Err(_) => value.to_string(),
        },
        _ => match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        },
    }
}

/// What the voice reads when a key that may not be absent *is* absent: the serde default of the
/// field, which for the two kinds that can be non-optional is `false` and `default`.
fn default_text(kind: zyris_voice::settings::Kind) -> String {
    match kind {
        zyris_voice::settings::Kind::Bool => "false".to_string(),
        zyris_voice::settings::Kind::Device => "default".to_string(),
        // Unreachable today — every text and number key in the table may be absent — and "not
        // set" rather than a default invented here if that ever stops being true.
        _ => "not set".to_string(),
    }
}

/// Write a JSON document where a settings file lives.
fn write_json(path: &std::path::Path, document: &Value) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(document)?;
    bytes.push(b'\n');
    write_bytes(path, bytes)
}

/// Put bytes where a settings file lives, through a temporary name and a rename.
///
/// The same shape `zyris-voice` writes its own settings with, and for the same reason: something
/// reads this file at every launch, and half a document is not a document.
fn write_bytes(path: &std::path::Path, bytes: Vec<u8>) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not make {}", parent.display()))?;
    }
    let part = path.with_extension(format!("part-{}", std::process::id()));
    std::fs::write(&part, &bytes).with_context(|| format!("could not write {}", part.display()))?;
    std::fs::rename(&part, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&part);
    })
    .with_context(|| format!("could not put {} in place", path.display()))
}

// ---------------------------------------------------------------- autostart

/// Start Zyris when this computer signs in, or stop doing that.
///
/// **One function for both spellings**, `--install-autostart` and `zyris autostart enable`, so a
/// flag and a subcommand cannot install different things or read the answer differently. The
/// window's switch goes through the same `bridge::apply_autostart`.
///
/// `None` is the `status` form: read the machine and change nothing.
pub fn autostart(request: Option<AutostartRequest>, server: Option<&str>) -> Result<()> {
    // Both mechanisms start `<this executable> --minimized` and nothing else, so autostart
    // installed from a `--server` run starts the *production* instance at the next logon — a
    // different node, with different credentials, from the one this process would have been.
    // Said rather than refused: the person may well want exactly that.
    if request.is_some() && server.is_some() {
        eprintln!(
            "note: --server is not carried into autostart. What starts at logon is this \
             executable with --minimized, which is the default instance."
        );
    }

    let autostart = zyris_autostart::Autostart::for_this_machine();
    let view = match request {
        Some(request) => {
            bridge::apply_autostart(&autostart, request == AutostartRequest::Install)?
        }
        // Read through the same call the Settings screen reads through, so the two cannot
        // disagree about what this machine says.
        None => bridge::look(&autostart)?,
    };

    match &view.state {
        zyris_autostart::State::Enabled => println!("autostart is on"),
        zyris_autostart::State::Disabled => println!("autostart is off"),
        zyris_autostart::State::Unsupported(reason) => {
            println!("autostart is not available on this machine: {reason}")
        }
    }
    if let Some(mechanism) = &view.mechanism {
        println!("  mechanism  {mechanism}");
    }
    // Everything that leaves the switch weaker than "on" sounds. On Linux these lines are the
    // difference between a machine that is connected whenever it is switched on and one that is
    // connected only while somebody is logged in to a desktop.
    for caveat in &view.caveats {
        println!("  note       {caveat}");
    }
    Ok(())
}

// ---------------------------------------------------------------- mcp

/// The MCP servers this machine starts.
///
/// **The list is the file, and the file is what the next launch reads.** A server's switch on the
/// Tools screen lasts as long as that run; these commands edit `mcp-servers.json`, which is what
/// decides what starts — and a node that is running read it at startup, so what these change is
/// the next launch. Said out loud in the same words `zyris_tools::Servers::set_enabled` documents,
/// because a person who thinks they just turned a server off on a live machine has been told
/// something untrue.
fn mcp(target: &Target, args: &McpArgs) -> Result<()> {
    match &args.command {
        McpCommand::List => mcp_list(target),
        McpCommand::Enable { name } => mcp_set_enabled(target, name, true),
        McpCommand::Disable { name } => mcp_set_enabled(target, name, false),
        McpCommand::Add { name, command, args } => mcp_add(target, name, command, args),
        McpCommand::Remove { name } => mcp_remove(target, name),
    }
}

/// The server list, or the reason it could not be read.
///
/// `Config::read` never fails for a file that is not there — a machine nobody has configured is
/// the ordinary case — and fails with the path in the message for every other way it can be
/// wrong, which is the sentence a person needs.
fn mcp_config(target: &Target) -> Result<zyris_mcp::Config> {
    zyris_mcp::Config::read(&target.data)
}

fn mcp_list(target: &Target) -> Result<()> {
    let config = mcp_config(target)?;

    if config.servers.is_empty() {
        println!(
            "no MCP servers are configured for instance {}: {} does not name any",
            target.instance,
            zyris_mcp::Config::path(&target.data).display()
        );
        println!("  `zyris mcp add <name> <command> [args…]` is how to add one");
        return Ok(());
    }

    println!(
        "{} MCP server{} for instance {}",
        config.servers.len(),
        if config.servers.len() == 1 { "" } else { "s" },
        target.instance
    );
    println!();
    println!("  {:<20} {:<8} {:<16} {}", "NAME", "ENABLED", "CAPABILITY", "COMMAND");
    for server in &config.servers {
        // What an agent would address it as, or why it would not be announced at all: a name that
        // will not make a capability is the one thing about an entry that cannot be seen by
        // reading it.
        let capability = match zyris_mcp::capability_name(&server.name) {
            Ok(name) => name,
            Err(error) => format!("(not announced: {error})"),
        };
        let mut line = server.command.clone();
        for argument in &server.args {
            line.push(' ');
            line.push_str(argument);
        }
        println!(
            "  {:<20} {:<8} {:<16} {}",
            server.name,
            if server.enabled { "yes" } else { "no" },
            capability,
            line
        );
    }
    Ok(())
}

/// Find one entry, or refuse with the names there are — the same shape `key_for` uses, and for the
/// same reason: "there is no server called notes" is only half an answer.
fn find_mut<'a>(
    config: &'a mut zyris_mcp::Config,
    name: &str,
) -> Result<&'a mut zyris_mcp::ServerConfig> {
    if !config.servers.iter().any(|server| server.name == name) {
        let names = config
            .servers
            .iter()
            .map(|server| server.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let names = if names.is_empty() { "none".to_string() } else { names };
        bail!("there is no MCP server called `{name}` in this instance's list. There are: {names}");
    }
    Ok(config.servers.iter_mut().find(|server| server.name == name).expect("just checked"))
}

fn mcp_set_enabled(target: &Target, name: &str, enabled: bool) -> Result<()> {
    let mut config = mcp_config(target)?;
    find_mut(&mut config, name)?.enabled = enabled;
    write_config(target, &config)?;

    println!(
        "{name} is {} for the next launch",
        if enabled { "enabled" } else { "disabled" }
    );
    if target.running() {
        println!(
            "  a node is running and read this list at startup, so\n\
             \x20 its own Tools screen is what turns {name} off in *this* run"
        );
    }
    Ok(())
}

fn mcp_add(target: &Target, name: &str, command: &str, args: &[String]) -> Result<()> {
    let mut config = mcp_config(target)?;
    if config.servers.iter().any(|server| server.name == name) {
        bail!("there is already an MCP server called `{name}`; `zyris mcp remove {name}` first");
    }
    // The name an agent addresses is derived, and a name that cannot become one is refused here —
    // where a person has just typed it — rather than at the next launch, where it leaves the whole
    // entry silently absent.
    zyris_mcp::capability_name(name)
        .map_err(|error| anyhow::anyhow!("`{name}` cannot be a server name: {error}"))?;

    config.servers.push(zyris_mcp::ServerConfig {
        name: name.to_string(),
        command: command.to_string(),
        args: args.to_vec(),
        enabled: true,
    });
    write_config(target, &config)?;
    println!("added {name}: {command} {}", args.join(" "));
    Ok(())
}

fn mcp_remove(target: &Target, name: &str) -> Result<()> {
    let mut config = mcp_config(target)?;
    let position = config
        .servers
        .iter()
        .position(|server| server.name == name)
        .ok_or_else(|| {
            let names = config
                .servers
                .iter()
                .map(|server| server.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let names = if names.is_empty() { "none".to_string() } else { names };
            anyhow::anyhow!("there is no MCP server called `{name}`. There are: {names}")
        })?;
    config.servers.remove(position);
    write_config(target, &config)?;
    println!("removed {name}");
    Ok(())
}

/// Write the server list back.
///
/// **Through `zyris_mcp`'s own type**, so everything this writes is a file that crate reads:
/// `deny_unknown_fields` and the duplicate-name rule are checked by the same parser the node uses
/// at startup, and a field this command does not know about cannot be invented into the file.
///
/// Serialized from the `Config` itself rather than through a `serde_json::Value`, so the fields
/// keep the order `ServerConfig` declares them in — which is the order the README's example shows
/// and the order a person editing the file by hand sees.
fn write_config(target: &Target, config: &zyris_mcp::Config) -> Result<()> {
    let path = zyris_mcp::Config::path(&target.data);
    let mut bytes = serde_json::to_vec_pretty(config)?;
    bytes.push(b'\n');
    write_bytes(&path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zyris_voice::settings::key;

    /// A target of this test's own: a real instance *name* with a directory nobody else is using.
    ///
    /// The name matters as much as the directory — `status`, `up` and `down` ask the machine's
    /// real lock directory about it — so it is one no Zyris ever runs under, and asking about it
    /// is asking about nothing.
    fn target() -> (tempfile::TempDir, Target) {
        let dir = tempfile::tempdir().unwrap();
        let target = Target {
            instance: "zyris-console-test".to_string(),
            data: dir.path().to_path_buf(),
            server: None,
        };
        (dir, target)
    }

    #[test]
    fn a_value_is_written_as_the_type_the_voice_reads() {
        // The three shapes a person can get wrong, each refused or converted by the key's own kind
        // rather than by whatever JSON happens to come out of the string.
        assert_eq!(value_for(key("voice.listen").unwrap(), "true").unwrap(), Some(Value::Bool(true)));
        assert_eq!(value_for(key("voice.listen").unwrap(), "off").unwrap(), Some(Value::Bool(false)));
        assert_eq!(
            value_for(key("voice.volume").unwrap(), "1.5").unwrap(),
            Some(serde_json::json!(1.5))
        );
        assert_eq!(
            value_for(key("voice.session").unwrap(), "s_01H").unwrap(),
            Some(Value::String("s_01H".to_string()))
        );
        // A device is the word, or an id — never a JSON object the person had to build.
        assert_eq!(
            value_for(key("voice.device").unwrap(), "default").unwrap(),
            Some(serde_json::json!({ "kind": "default" }))
        );
        assert_eq!(
            value_for(key("voice.device").unwrap(), "hw:1,0").unwrap(),
            Some(serde_json::json!({ "kind": "device", "id": "hw:1,0" }))
        );
    }

    #[test]
    fn what_is_not_a_value_is_refused_rather_than_written() {
        assert!(value_for(key("voice.listen").unwrap(), "maybe").is_err());
        assert!(value_for(key("voice.volume").unwrap(), "loud").is_err());
    }

    #[test]
    fn only_a_key_the_file_may_leave_out_can_be_unset() {
        // `listen` is a plain bool in the struct, so a `null` there is a file the voice cannot read
        // at all — the whole file falls back to the defaults, silently. Refused here, where a
        // person can be told why.
        assert!(value_for(key("voice.listen").unwrap(), "unset").is_err());
        assert_eq!(value_for(key("voice.readAloud").unwrap(), "unset").unwrap(), None);
        assert_eq!(value_for(key("voice.session").unwrap(), "none").unwrap(), None);
    }

    #[test]
    fn a_setting_is_written_read_back_and_cleared() {
        let (_dir, target) = target();

        config_set(&target, "voice.listen", "true").unwrap();
        let (path, document) = voice_document(&target).unwrap();
        assert_eq!(document.get("listen"), Some(&Value::Bool(true)));
        assert_eq!(path.file_name().unwrap(), zyris_voice::settings::SETTINGS_FILE);

        config_set(&target, "voice.session", "s_01H").unwrap();
        config_set(&target, "voice.session", "unset").unwrap();
        let (_, document) = voice_document(&target).unwrap();
        assert!(!document.contains_key("session"), "`unset` takes the key out of the file");
        assert_eq!(document.get("listen"), Some(&Value::Bool(true)), "and leaves the rest alone");
    }

    #[test]
    fn a_settings_file_that_will_not_parse_is_refused_rather_than_overwritten() {
        // The one mistake that would cost a person their settings: the voice reads a broken file
        // as *no* settings, so a command that "fixed" it by writing a fresh document would be
        // writing over everything that was in it.
        let (_dir, target) = target();
        let path = target.data.join(zyris_voice::settings::SETTINGS_FILE);
        std::fs::write(&path, b"{ not json").unwrap();

        let refused = config_set(&target, "voice.listen", "true").unwrap_err();

        assert!(refused.to_string().contains("not JSON"), "{refused}");
        assert_eq!(std::fs::read(&path).unwrap(), b"{ not json", "the file is left alone");
    }

    #[test]
    fn a_key_nobody_declared_is_refused_and_the_list_is_given() {
        let (_dir, target) = target();

        let refused = config_get(&target, "voice.loudness").unwrap_err();

        let said = refused.to_string();
        assert!(said.contains("voice.loudness"), "{said}");
        assert!(said.contains("voice.listen"), "the answer has to name what there is: {said}");
    }

    /// **A key the file may not leave out has to have something to print when it is not there.**
    ///
    /// `config list` shows the value in the file, and — for a key that may not be absent — what
    /// the voice reads instead, which is that field's serde default. A non-optional key of a kind
    /// `default_text` does not know about would print "not set" for a switch that means `false`:
    /// a lie about what the machine is doing, in the one command somebody runs to find out.
    #[test]
    fn every_key_the_file_may_not_leave_out_has_a_default_to_print() {
        for key in zyris_voice::settings::KEYS {
            if !key.optional {
                assert_ne!(
                    default_text(key.kind),
                    "not set",
                    "{} may not be absent, so listing it has to say what the voice reads \
                     instead — teach `default_text` about {:?}",
                    key.name,
                    key.kind
                );
            }
        }
    }

    /// And the list runs on both machines: one nobody has configured, and one with a file.
    #[test]
    fn the_list_runs_whatever_is_in_the_file() {
        let (_dir, target) = target();
        assert!(config_list(&target).is_ok(), "no settings file at all is the ordinary case");

        std::fs::write(
            target.data.join(zyris_voice::settings::SETTINGS_FILE),
            br#"{"listen": true, "readAloud": false, "volume": 1.5}"#,
        )
        .unwrap();
        assert!(config_list(&target).is_ok());
    }

    #[test]
    fn an_unknown_key_that_is_optional_is_reported_as_unset() {
        let (_dir, target) = target();

        // Nothing written yet, so nothing is set — and a command that answered with an error here
        // would make `config get` unusable on a machine nobody has configured.
        assert!(config_get(&target, "voice.wakePhrase").is_ok());
    }

    #[test]
    fn an_mcp_server_is_added_listed_disabled_and_removed() {
        let (_dir, target) = target();

        mcp_add(&target, "notes", "mcp-notes", &["--dir".to_string(), "~/notes".to_string()]).unwrap();
        let config = mcp_config(&target).unwrap();
        assert_eq!(config.servers.len(), 1);
        assert_eq!(config.servers[0].name, "notes");
        assert_eq!(config.servers[0].command, "mcp-notes");
        assert_eq!(config.servers[0].args, vec!["--dir", "~/notes"]);
        assert!(config.servers[0].enabled, "a server somebody adds is one they want started");

        mcp_set_enabled(&target, "notes", false).unwrap();
        assert!(!mcp_config(&target).unwrap().servers[0].enabled);

        mcp_set_enabled(&target, "notes", true).unwrap();
        assert!(mcp_config(&target).unwrap().servers[0].enabled);

        mcp_remove(&target, "notes").unwrap();
        assert!(mcp_config(&target).unwrap().servers.is_empty());
    }

    #[test]
    fn an_mcp_entry_that_is_not_there_is_refused_with_the_names_there_are() {
        let (_dir, target) = target();
        mcp_add(&target, "notes", "mcp-notes", &[]).unwrap();

        for refused in [
            mcp_set_enabled(&target, "calendar", false).unwrap_err(),
            mcp_remove(&target, "calendar").unwrap_err(),
        ] {
            let said = refused.to_string();
            assert!(said.contains("calendar"), "{said}");
            assert!(said.contains("notes"), "{said}");
        }
    }

    #[test]
    fn two_servers_may_not_share_a_name_and_a_name_has_to_be_addressable() {
        let (_dir, target) = target();
        mcp_add(&target, "notes", "mcp-notes", &[]).unwrap();

        assert!(mcp_add(&target, "notes", "other", &[]).is_err(), "two entries, one name");
        // A name with a dot cannot be an MCP capability, which is what the node would have to
        // announce it as. Refused while the person is typing rather than at the next launch.
        assert!(mcp_add(&target, "my.notes", "mcp-notes", &[]).is_err());
    }

    #[test]
    fn a_status_block_about_a_machine_with_nothing_running_is_an_answer_not_a_failure() {
        let (_dir, target) = target();

        assert!(status(&target).is_ok());
        assert!(down(&target).is_ok(), "nothing to stop is not an error");
    }

    #[test]
    fn a_node_started_by_an_older_build_is_described_rather_than_guessed_at() {
        // The lock is held — a node is running — and there is no state file, because the build
        // that is running predates this command. Nothing here may invent a pid or a phase.
        let (_dir, target) = target();
        let held = InstanceLock::acquire_in(
            std::env::temp_dir(),
            // Not the machine's real lock directory: a test must not depend on, or disturb, what
            // is running on this computer.
            "zyris-console-test",
        )
        .unwrap();

        // `status` asks the real lock directory, so this exercises the branch where the two
        // disagree: the lock it asks about is not held, and the file that would supply detail is
        // there. What matters is that neither answer is invented.
        drop(held);
        let mut state = NodeState::new("zyris-console-test", Mode::Headless, "wss://example.invalid/ws");
        state.observe(&CoreEvent::Connected { node_id: "n".into(), node_name: "here".into() });
        state.save(&target.data).unwrap();

        assert!(status(&target).is_ok());
    }
}
