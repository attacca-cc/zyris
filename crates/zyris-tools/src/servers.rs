//! The local MCP servers this run owns: what each one is doing, and keeping the announcement
//! honest about it.
//!
//! [`announce`](crate::announce) builds the list this machine hands a node at startup and then has
//! nothing more to say. This is the part that keeps saying it: a server somebody turns off stops
//! being announced, one they turn back on joins, and **one that falls over is withdrawn without
//! anybody having called it**.
//!
//! # A death is noticed by asking a flag, not by calling
//!
//! The plan for this step expected there to be no cheap signal, and offered a watcher over
//! `RunningService::waiting()` (which consumes the service, so it cannot be reached through the
//! `Arc<Promoted>` this holds) or a periodic ping. **Neither is needed.**
//! [`zyris_mcp::Server::is_running`] reads a flag on the channel into `rmcp`'s service loop: no
//! round trip, nothing written to the child, nothing spawned. It was measured against a real
//! child that exits on a timer with no call ever made, and it flips within milliseconds; the four
//! readings and the two neighbouring `rmcp` primitives that do *not* answer this are written up on
//! [`zyris_mcp::server`].
//!
//! That collapses the interval into a question with only one side to it. Polling costs one flag
//! read per server, so there is nothing to trade against latency: what a longer interval buys is
//! a window that lists a server nobody can reach as running, and an agent whose calls fail against
//! a capability this machine is still advertising. What a shorter one costs is a timer tick.
//! [`HEALTH_INTERVAL`] is a second, which is below what a person reading a screen notices and is
//! not a wakeup budget anybody has to account for next to the websocket this process already
//! holds open. It is deliberately **not** tuned to a test: `servers_come_and_go.rs` asserts an
//! upper bound on it rather than a value, and waits several multiples.
//!
//! **What it does not cover, said rather than implied:** the transport is the process, so a server
//! that is alive but wedged — answering nothing, or answering nonsense — reads as running, and
//! correctly, because it is there. The only way to learn that a server has stopped being *useful*
//! is still to call it, and an agent's call is what does that.
//!
//! # Withdrawn and turned off are the same thing to an agent and must not be to a person
//!
//! Both end as "this capability is not announced", and for an agent that is the whole truth: there
//! is nothing it could do differently. For whoever is looking at the window there is everything to
//! do differently — one of the two is a process they can restart, and the other is a switch they
//! moved themselves. So the core publishes which it was, on the event bus, as
//! [`McpServerChange`], and keeps it in [`ServerState`] for a window that opened afterwards. The
//! distinction exists here because here is the only place that knows it; nothing downstream could
//! reconstruct it from an absence.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use zyris::ServeCapability;
use zyris_mcp::{DroppedTool, Promoted, ServerConfig, Started};
use zyris_runtime::{CoreEvent, EventBus, LiveCapabilities, McpServerChange};

use crate::announce::Tools;

/// How often a running server is asked whether its process is still there.
///
/// See [the module documentation](self#a-death-is-noticed-by-asking-a-flag-not-by-calling) for why
/// this is a latency decision with nothing on the other side of it.
pub const HEALTH_INTERVAL: Duration = Duration::from_secs(1);

/// How long [`Servers::stop_all`] waits for the processes to actually go before giving up.
///
/// On the way out of the program, so it is a ceiling on how long a misbehaving MCP server can
/// keep a window on the screen rather than a budget anything spends: a server that is working is
/// gone in milliseconds. Giving up is logged with the names, because a process that outlived this
/// one is the thing somebody would otherwise find in a task manager with no idea where it came
/// from.
pub const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(2);

/// Why an entry the file asked for is not running, when all this type knows is that it is not.
///
/// **Two sentences, because the two situations offer a person different things to do.** Whether
/// the name can be announced decides which: [`zyris_mcp::config::start_one`] checks it before it
/// spawns anything, so an entry whose name has a dot in it was never attempted and there is no
/// log line about it to go and read.
///
/// Both are written to be **the whole of what the row says**. The badge already reads "did not
/// start" and `ui/src/Mcp.tsx` renders this on its own, so a sentence beginning "it did not start"
/// would be the row saying that twice. And neither of them instructs an action the screen does not
/// offer: a [`ServerState::Failed`] row has one button on it and it says **Turn on**, and a name
/// that can never be announced has no button at all — its row carries the rename instead, which is
/// the only thing that would help.
fn startup_failure(can_be_announced: bool) -> String {
    if can_be_announced {
        "The reason was written to the log when Zyris started. Turn it on here to try again, and          this row will say what happened."
            .to_string()
    } else {
        "Nothing tried to start it, because its name cannot be announced.".to_string()
    }
}

/// What one configured MCP server is doing.
///
/// Tagged on `state` so the window switches on one field, and carrying its own detail where it has
/// any rather than putting a reason into a sentence nothing can read.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum ServerState {
    /// The process is there and its tools are announced.
    Running,
    /// Not running because somebody said not to — either in the file, or through the window while
    /// this run has been going. The process is stopped, not merely unannounced.
    Disabled,
    /// It was running and the process is gone, and nobody asked for that. **The state a person is
    /// meant to act on**, and the whole reason this is not a boolean.
    Died,
    /// It was asked to start and would not. `reason` is what to check, in the words the failure
    /// used.
    Failed { reason: String },
}

impl ServerState {
    /// The same fact, as the thing the bus carries.
    ///
    /// The one place these two vocabularies meet, so they cannot drift apart in silence: a state
    /// added here without a change to report it against will not compile.
    fn as_change(&self, capability: Option<&str>, tools: usize) -> McpServerChange {
        match self {
            ServerState::Running => McpServerChange::Announced {
                capability: capability.unwrap_or_default().to_string(),
                tools,
            },
            ServerState::Disabled => McpServerChange::Disabled,
            ServerState::Died => McpServerChange::Died,
            ServerState::Failed { reason } => McpServerChange::Failed { reason: reason.clone() },
        }
    }
}

/// One configured server, as a window lists it.
///
/// Built from what is actually running rather than from the file: `tools` is what the server
/// reported and this machine announced, and `dropped` is what it offered that no call could ever
/// reach. Neither is knowable from the entry a person typed.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerView {
    /// What the file calls it.
    pub name: String,
    /// The capability an agent addresses when this is running, or `None` for a name that could
    /// never make one — see [`zyris_mcp::capability_name`].
    pub capability: Option<String>,
    /// What is run, so a person can try it in a terminal.
    pub command: String,
    pub args: Vec<String>,
    pub state: ServerState,
    /// The tools an agent can call, in announcement order. Empty whenever the state is not
    /// [`ServerState::Running`], which is honest: nothing is announced.
    pub tools: Vec<String>,
    /// What the server offered and this machine did not announce, and why. Empty for every
    /// well-formed server.
    pub dropped: Vec<DroppedTool>,
}

/// Everything a window's MCP screen lists, in one answer.
///
/// **Three answers live in here and only one of them is a list.** `problem` set is a server list
/// that could not be read; `problem` clear with an empty `servers` is a machine nobody has
/// configured. Both run no servers, so for a long while they were one answer — and a screen handed
/// an empty list for an unreadable file tells the person who wrote that file that they have
/// configured nothing, which sends them looking for the file they are already looking at.
///
/// `path` travels with them rather than being spelled out by whatever renders this, because it is
/// the *instance's*: a `--server` run keeps its own list, and a path written into the window would
/// name the production one from a development window.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerList {
    /// The file this run reads its servers from, named so a person can go and edit it. Read once,
    /// at startup; nothing writes it.
    pub path: String,
    /// Why that file could not be read, or `None` — which includes the ordinary machine that has
    /// no such file at all.
    pub problem: Option<String>,
    /// Every configured server, in the order the file lists them. Empty is a real answer, and it
    /// means nobody has configured one.
    pub servers: Vec<ServerView>,
}

/// The MCP servers this run owns.
///
/// Clone it freely; every clone supervises the same servers. `main` keeps one for the window's
/// commands and hands another to the task that watches for deaths.
#[derive(Clone)]
pub struct Servers(Arc<Inner>);

struct Inner {
    /// Where the server list is, so anything asking a person to edit it can name it. Read once at
    /// startup and never again: see [`Servers::set_enabled`] for why nothing here writes it.
    path: std::path::PathBuf,
    /// Why the server list could not be read, when it could not.
    ///
    /// **Carried so that an empty list is never the answer to a broken file.** Both start nothing,
    /// and to this type they look identical — the difference exists only in
    /// [`zyris_mcp::Started`], and it is the difference between "you have configured no servers"
    /// and "the file you configured them in has a typo in it".
    problem: Option<String>,
    /// What this machine puts in front of a capability. Held rather than the pieces, because a
    /// server that joined mid-session has to go behind the very same switch and log as one that
    /// was there at startup — see [`Tools::guard_shared`].
    tools: Tools,
    /// What the node announces. Changing this is the whole job.
    live: LiveCapabilities,
    bus: EventBus,
    /// **One lock over the whole list, held across starting a process.** That means everything
    /// else waits while a server is starting, up to `zyris_mcp::STARTUP_DEADLINE` for one that
    /// never speaks. The trade is deliberate: the alternative is an in-progress state that every
    /// reader has to handle, and the failure it would prevent is a window that waits rather than a
    /// window that lies.
    ///
    /// **"Everything else" is not only [`Servers::list`].** [`Servers::reap`] takes this lock too,
    /// so a server that dies while an unrelated one is being started stays announced until that
    /// start finishes or gives up — ten seconds, worst case, of a capability advertised over a
    /// process that is gone. That is the real cost of the trade and it is worse than a window that
    /// waits, so it is written down here rather than left to be found. It is bounded by the same
    /// deadline and it ends by itself; what it is not is invisible.
    entries: Mutex<Vec<Entry>>,
}

struct Entry {
    config: ServerConfig,
    /// `None` for a name that cannot be announced. Computed once, because it is derived from the
    /// name and a second derivation is a second answer.
    capability: Option<String>,
    state: ServerState,
    /// The running server, whenever `state` is [`ServerState::Running`]. The two move together and
    /// only through [`Inner::set`], so there is no state in which one says running and the other
    /// holds nothing.
    running: Option<Arc<Promoted>>,
}

impl Servers {
    /// What `main` has after starting the configured servers: the file, and what came up.
    ///
    /// Entries the file disabled are carried as [`ServerState::Disabled`] — the window has to list
    /// them, or somebody who turned one off has no way back to it. An entry that was enabled and
    /// is not running is [`ServerState::Failed`]; the reason it failed was logged by
    /// [`zyris_mcp::config::start`] where it happened, and nothing carries it here yet.
    pub fn new(tools: &Tools, live: LiveCapabilities, bus: EventBus, started: Started) -> Servers {
        let Started { path, config, running, problem } = started;
        let entries = config
            .servers
            .into_iter()
            .map(|config| {
                let capability = zyris_mcp::capability_name(&config.name).ok();
                let promoted = running
                    .iter()
                    .find(|promoted| promoted.server().name() == config.name)
                    .cloned();
                let state = match (&promoted, config.enabled) {
                    (Some(_), _) => ServerState::Running,
                    (None, false) => ServerState::Disabled,
                    (None, true) => {
                        ServerState::Failed { reason: startup_failure(capability.is_some()) }
                    }
                };
                Entry { config, capability, state, running: promoted }
            })
            .collect();

        Servers(Arc::new(Inner {
            path,
            problem,
            tools: tools.clone(),
            live,
            bus,
            entries: Mutex::new(entries),
        }))
    }

    /// Every configured server and what it is doing, in the order the file lists them.
    pub async fn list(&self) -> Vec<ServerView> {
        self.0.entries.lock().await.iter().map(Entry::view).collect()
    }

    /// The same list with the two things a screen needs in order to say why it is empty.
    ///
    /// **Assembled here rather than by the window's command**, so the one place that knows whether
    /// the file was readable is the one place that says so. `problem` is fixed for the life of the
    /// run — the file is read once, at startup — so a screen showing it is showing why there is
    /// nothing to show, not a condition that might clear on its own.
    pub async fn view(&self) -> ServerList {
        ServerList {
            path: self.0.path.display().to_string(),
            problem: self.0.problem.clone(),
            servers: self.list().await,
        }
    }

    /// Turn one server on or off, and re-announce.
    ///
    /// **Nothing is written to disk, and that is now a decision rather than a gap.** The switch
    /// lasts as long as this run; the file is what the next one starts from. Three things decided
    /// it, and the window says so in as many words rather than letting a person find out at the
    /// next restart:
    ///
    /// - **The file is read once, at startup, and never again.** A write-back would serialize the
    ///   snapshot taken then over whatever the file says now — so a server somebody added by hand
    ///   at ten o'clock would be erased by a click at five past, on a row that had nothing to do
    ///   with it. Making that safe means re-reading, merging and deciding what to do about a file
    ///   that has since become invalid, which is a great deal of machinery to hang off a switch.
    /// - **The file is a person's document.** Nothing in Zyris writes it. `ServerConfig` derives
    ///   `Serialize`, so a rewrite would lose no *field* — but it would replace their key order,
    ///   their indentation and their grouping with `serde_json`'s, and write `"enabled": true`
    ///   onto every entry that had been content with the default.
    /// - **"Off for now" is a real thing to want**, and it is the one a window is good at:
    ///   stopping a server that is misbehaving, without editing anything, and getting it back by
    ///   restarting Zyris. "Off until I say otherwise" is a real thing to want too, and it already
    ///   has an answer — `"enabled": false` in the file — which the window points at.
    ///
    /// The trap this avoids is the reverse one, and it is worth naming: a switch that *silently*
    /// forgets is a screen that lied. What makes this defensible is entirely in the copy, so
    /// `ui/src/Mcp.tsx` says which run the switch lasts for and names the file that outlives it.
    ///
    /// Asking for the state it is already in is not an error and does nothing — a second click on
    /// a button that was never redrawn is not a fresh instruction.
    ///
    /// **`Err` is reserved for a request this cannot act on at all**, which is exactly one thing:
    /// a name the server list does not have. A server that was asked to start and would not is not
    /// that — it is an answer, and the answer is the row, carrying [`ServerState::Failed`] and the
    /// reason. Returning both an `Err` *and* that row put the same string on the screen twice: once
    /// under the row as its state, and again beside it as a rejected request.
    pub async fn set_enabled(&self, name: &str, enabled: bool) -> Result<ServerView, String> {
        let mut entries = self.0.entries.lock().await;
        let index = entries
            .iter()
            .position(|entry| entry.config.name == name)
            .ok_or_else(|| format!("there is no MCP server called `{name}` in the server list"))?;

        if enabled {
            if matches!(entries[index].state, ServerState::Running) {
                return Ok(entries[index].view());
            }
            let config = entries[index].config.clone();
            match zyris_mcp::config::start_one(&config).await {
                Ok(promoted) => {
                    let promoted = Arc::new(promoted);
                    let guarded = self.0.tools.guard_shared(promoted.clone() as Arc<dyn ServeCapability>);
                    if let Err(error) = self.0.live.add(guarded).await {
                        // The name is already announced. It cannot happen through the ordinary
                        // path — two entries with one name are refused when the file is read, and
                        // `mcp_` prefixing is injective — so this is reported rather than
                        // recovered from, and the process is dropped instead of being left
                        // running behind nothing.
                        let reason =
                            format!("It started but could not be announced: {}", error.message);
                        self.0.set(&mut entries[index], ServerState::Failed { reason }, None);
                        return Ok(entries[index].view());
                    }
                    self.0.set(&mut entries[index], ServerState::Running, Some(promoted));
                }
                Err(error) => {
                    // The row is the answer. It says which server, because it is that server's
                    // row, so the sentence only has to say what went wrong.
                    let reason = format!("It would not start: {error:#}");
                    self.0.set(&mut entries[index], ServerState::Failed { reason }, None);
                    return Ok(entries[index].view());
                }
            }
        } else {
            if !matches!(entries[index].state, ServerState::Running) {
                // Already off, or dead, or never started. Recording it as disabled either way
                // would tell a person they had turned off something that had crashed.
                return Ok(entries[index].view());
            }
            self.withdraw(&mut entries[index], ServerState::Disabled).await;
        }

        Ok(entries[index].view())
    }

    /// Withdraw every server whose process is gone, and say how many that was.
    ///
    /// Asked on a timer by [`Self::watch`]. Idempotent: a server already withdrawn is no longer
    /// running, so it is not counted or reported twice.
    ///
    /// **The whole tick is one re-announce.** `zyris.announce` is full replacement, so withdrawing
    /// three dead servers one at a time would put three lists on the wire, and the first two would
    /// still advertise servers this method had already found dead — an agent reading one of them
    /// acts on something this machine knows is untrue. Three deaths in one tick is not exotic:
    /// they are usually the same event, a parent that quit or a session that ended.
    pub async fn reap(&self) -> usize {
        let mut entries = self.0.entries.lock().await;
        let dead: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                entry.running.as_ref().is_some_and(|promoted| !promoted.is_running())
            })
            .map(|(index, _)| index)
            .collect();
        if dead.is_empty() {
            return 0;
        }

        for &index in &dead {
            tracing::warn!(
                server = entries[index].config.name,
                capability = entries[index].capability.as_deref().unwrap_or("<none>"),
                "an MCP server's process is gone, so its tools are no longer announced; nobody \
                 asked for this. Nothing else on this machine is affected."
            );
        }
        // The announcement first, all of it at once, for the reason [`Self::withdraw`] gives:
        // nothing new can be routed into a process that is about to be let go of.
        let names: Vec<String> =
            dead.iter().filter_map(|&index| entries[index].capability.clone()).collect();
        self.0.live.remove_all(&names).await;
        for &index in &dead {
            self.0.release(&mut entries[index], ServerState::Died);
        }
        dead.len()
    }

    /// Stop every server this run started, and wait for the processes to go.
    ///
    /// **For the one exit that runs no destructors.** Tauri's `App::run` ends the process with
    /// `std::process::exit`, so nothing managed by the app is ever dropped and the `Arc<Promoted>`
    /// values here are not either. Exiting closes the pipes, which is enough for a server that
    /// quits on end-of-file — but one that does not is left running, reparented, and a second copy
    /// of it is spawned by the next launch.
    ///
    /// Bounded, because this is on the way out and a server that will not go must not be able to
    /// keep the window on the screen. [`Server::stop`](zyris_mcp::Server::stop) returns as soon as
    /// it has cancelled: the teardown runs on `rmcp`'s own task, which closes the child's stdin,
    /// waits briefly, and then kills it — so what is waited for here is that task getting far
    /// enough to matter, which for a working server is milliseconds.
    ///
    /// Nothing is re-announced and no [`CoreEvent::McpServer`] is published. The node is going
    /// away and so is the window; a state change nobody can read is not a state change.
    pub async fn stop_all(&self) {
        let entries = self.0.entries.lock().await;
        let running: Vec<Arc<Promoted>> =
            entries.iter().filter_map(|entry| entry.running.clone()).collect();
        if running.is_empty() {
            return;
        }
        tracing::info!(servers = running.len(), "stopping the local MCP servers");
        for promoted in &running {
            promoted.stop();
        }

        let deadline = std::time::Instant::now() + SHUTDOWN_DEADLINE;
        while running.iter().any(|promoted| promoted.is_running()) {
            if std::time::Instant::now() >= deadline {
                let left: Vec<&str> = running
                    .iter()
                    .filter(|promoted| promoted.is_running())
                    .map(|promoted| promoted.server().name())
                    .collect();
                tracing::warn!(
                    servers = ?left,
                    "gave up waiting for these MCP servers to stop; they may outlive this process"
                );
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Ask after every server, for as long as this runs.
    ///
    /// Spawned by `main` in both runtimes. A withdrawal that only happened when somebody opened a
    /// window would not be a withdrawal: a headless node is exactly the one nobody is looking at.
    pub async fn watch(self) {
        let mut ticker = tokio::time::interval(HEALTH_INTERVAL);
        // A laptop that suspends for an hour comes back with an hour of missed ticks. `Burst`
        // would run them all at once, which is a flood of identical checks rather than the one
        // that is actually due; `Delay` runs one and carries on.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            self.reap().await;
        }
    }

    /// Take an entry's capability off the announcement, stop its process, and say why.
    ///
    /// **In that order.** The capability goes first, so nothing new can be routed into a process
    /// that is about to be stopped; stopping it first would leave a live announcement in front of
    /// a closed pipe for as long as the re-announce takes.
    ///
    /// The stop is asked for rather than left to the handle being dropped, and that is not
    /// belt-and-braces: `Tools` keeps its own `Arc` on every server that was running when the node
    /// was built, and that `Tools` outlives the process. A withdrawal that relied on the last
    /// handle going away would therefore withdraw the capability and leave the server running —
    /// unannounced, unlisted, unreachable, and still there. See [`Promoted::stop`].
    ///
    /// One method rather than two steps a caller sequences, because the two withdrawals — a
    /// person's and a death — differ in exactly one thing, which is the `state` they leave behind.
    async fn withdraw(&self, entry: &mut Entry, state: ServerState) {
        if let Some(capability) = &entry.capability {
            self.0.live.remove(capability).await;
        }
        self.0.release(entry, state);
    }
}

impl Inner {
    /// The half of a withdrawal that is about the entry rather than the announcement: stop the
    /// process, and record what happened.
    ///
    /// Split out because [`Servers::reap`] withdraws several capabilities in **one** re-announce
    /// and then does this for each of them, while [`Servers::withdraw`] does one of each. The stop
    /// is asked for rather than left to the last handle going away — see [`Servers::withdraw`].
    fn release(&self, entry: &mut Entry, state: ServerState) {
        if let Some(promoted) = &entry.running {
            promoted.stop();
        }
        self.set(entry, state, None);
    }

    /// Move an entry to a new state and tell everything watching, in that order.
    ///
    /// One place, so a state that changed without anybody being told is not something a caller can
    /// produce by forgetting a line.
    ///
    /// Published **transiently**: the bus's one-slot catch-up value belongs to the events that
    /// decide which screen the window renders, and a server change sitting in it would displace
    /// the `Connected` a window needs to leave its starting screen. A window that opened later
    /// asks [`Servers::list`] instead.
    fn set(&self, entry: &mut Entry, state: ServerState, running: Option<Arc<Promoted>>) {
        let tools = running.as_ref().map_or(0, |promoted| promoted.descriptor().tools.len());
        entry.running = running;
        entry.state = state;
        self.bus.publish_transient(CoreEvent::McpServer {
            server: entry.config.name.clone(),
            change: entry.state.as_change(entry.capability.as_deref(), tools),
        });
    }
}

impl Entry {
    fn view(&self) -> ServerView {
        let (tools, dropped) = match &self.running {
            Some(promoted) => (
                promoted.descriptor().tools.into_iter().map(|tool| tool.name).collect(),
                promoted.dropped().to_vec(),
            ),
            None => (Vec::new(), Vec::new()),
        };
        ServerView {
            name: self.config.name.clone(),
            capability: self.capability.clone(),
            command: self.config.command.clone(),
            args: self.config.args.clone(),
            state: self.state.clone(),
            tools,
            dropped,
        }
    }
}

/// The capability name and the states, and nothing else.
///
/// Not derived: a view carries every announced tool's name and a `Promoted` behind it carries
/// their schemas, and a derived `Debug` would put all of that into any log line that formatted
/// one.
impl std::fmt::Debug for Servers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Servers").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A supervisor over the entries a file listed, none of them running.
    fn listing(servers: Vec<ServerConfig>) -> Servers {
        let tools = Tools::new(
            crate::Gate::running(),
            crate::AuditLog::new(std::path::PathBuf::from("/nonexistent/audit.jsonl")),
            std::path::PathBuf::from("/"),
        );
        Servers::new(
            &tools,
            LiveCapabilities::new(Vec::new()),
            EventBus::new(4),
            Started {
                path: std::path::PathBuf::from("/data/zyris/mcp-servers.json"),
                config: zyris_mcp::Config { servers },
                running: Vec::new(),
                problem: None,
            },
        )
    }

    fn entry(name: &str) -> ServerConfig {
        ServerConfig {
            name: name.to_string(),
            command: "x".to_string(),
            args: Vec::new(),
            enabled: false,
        }
    }

    #[tokio::test]
    async fn a_name_that_could_never_be_announced_is_listed_as_having_no_capability() {
        // **The one thing the window can offer somebody in this state is the rename**, and it can
        // only offer it if this says so. A capability name computed from the name regardless —
        // `mcp_` glued to whatever is there — would list `mcp_my.notes` as though it were
        // addressable, and every call to it would be read as a call to `mcp_my`, which nothing
        // announced. The same check `zyris_mcp::config::start_one` makes before it runs anything.
        let listed = listing(vec![entry("notes"), entry("my.notes"), entry("")]).list().await;

        assert_eq!(listed[0].capability.as_deref(), Some("mcp_notes"));
        assert_eq!(listed[1].capability, None, "a dot makes a capability nothing can address");
        assert_eq!(listed[2].capability, None, "`mcp_` alone names nothing");
    }

    /// A supervisor over a file that was read, or was not, and started nothing either way.
    fn supervising(path: &str, problem: Option<&str>) -> Servers {
        let tools = Tools::new(
            crate::Gate::running(),
            crate::AuditLog::new(std::path::PathBuf::from("/nonexistent/audit.jsonl")),
            std::path::PathBuf::from("/"),
        );
        Servers::new(
            &tools,
            LiveCapabilities::new(Vec::new()),
            EventBus::new(4),
            Started {
                path: std::path::PathBuf::from(path),
                config: zyris_mcp::Config::default(),
                running: Vec::new(),
                problem: problem.map(str::to_string),
            },
        )
    }

    #[tokio::test]
    async fn a_list_that_could_not_be_read_is_not_a_machine_with_no_servers() {
        // Both of these run nothing, and to this type they are identical. The difference is the
        // whole of what a person needs: one of them means "you have not configured any", and the
        // other means "the file you configured them in has a typo in it".
        let unreadable = supervising("/data/zyris/mcp-servers.json", Some("expected value"))
            .view()
            .await;
        let nobody_configured_one =
            supervising("/data/zyris/mcp-servers.json", None).view().await;

        assert!(unreadable.servers.is_empty());
        assert!(nobody_configured_one.servers.is_empty());
        assert_eq!(unreadable.problem.as_deref(), Some("expected value"));
        assert_eq!(nobody_configured_one.problem, None);

        // And both name the file, because "go and fix it" and "go and write one" are each an
        // instruction only with the path in them.
        assert_eq!(unreadable.path, "/data/zyris/mcp-servers.json");
        assert_eq!(nobody_configured_one.path, "/data/zyris/mcp-servers.json");
    }

    #[test]
    fn the_mcp_screen_can_tell_those_two_apart_on_the_wire() {
        // The same distinction, as `ui/src/Mcp.tsx` receives it. A screen rendering `servers`
        // alone would have no way back to it.
        let unreadable = ServerList {
            path: "/data/zyris/mcp-servers.json".to_string(),
            problem: Some("expected value at line 1 column 1".to_string()),
            servers: Vec::new(),
        };
        let nobody_configured_one = ServerList { problem: None, ..unreadable.clone() };

        assert_eq!(
            serde_json::to_value(&unreadable).unwrap(),
            serde_json::json!({
                "path": "/data/zyris/mcp-servers.json",
                "problem": "expected value at line 1 column 1",
                "servers": [],
            })
        );
        assert_eq!(
            serde_json::to_value(&nobody_configured_one).unwrap()["problem"],
            serde_json::json!(null)
        );
        assert_ne!(
            serde_json::to_value(&unreadable).unwrap(),
            serde_json::to_value(&nobody_configured_one).unwrap()
        );
    }

    fn view(state: ServerState) -> ServerView {
        ServerView {
            name: "desk-notes".to_string(),
            capability: Some("mcp_desk-notes".to_string()),
            command: "notes-mcp".to_string(),
            args: vec!["--root".to_string(), "/home/ada/notes".to_string()],
            state,
            tools: Vec::new(),
            dropped: Vec::new(),
        }
    }

    #[test]
    fn the_mcp_screen_reads_these_field_names() {
        // `ui/src/Mcp.tsx` transcribes this shape rather than importing it — there is no way to
        // share a type across the IPC boundary — so this is the Rust half of that agreement, and
        // nothing checks it at build time.
        //
        // The nesting is the part worth pinning. `state` is an internally tagged enum inside a
        // field also called `state`, so the screen switches on `server.state.state`; a screen
        // reaching for `server.state` alone would compare an object against a string and every
        // server would render as the fallback.
        let mut running = view(ServerState::Running);
        running.tools = vec!["search".to_string(), "append".to_string()];
        running.dropped = vec![DroppedTool {
            name: "search".to_string(),
            reason: "this server offers two tools called `search`".to_string(),
        }];

        assert_eq!(
            serde_json::to_value(&running).unwrap(),
            serde_json::json!({
                "name": "desk-notes",
                "capability": "mcp_desk-notes",
                "command": "notes-mcp",
                "args": ["--root", "/home/ada/notes"],
                "state": { "state": "running" },
                "tools": ["search", "append"],
                "dropped": [{
                    "name": "search",
                    "reason": "this server offers two tools called `search`",
                }],
            })
        );

        // And the name that can never make a capability, which the screen renders as the one
        // thing a person can do about it. `null` rather than an empty string: `mcp_` prefixed to
        // nothing is a name too, and it would read as one.
        let mut unaddressable = view(ServerState::Failed { reason: "a dot".to_string() });
        unaddressable.capability = None;
        assert_eq!(
            serde_json::to_value(&unaddressable).unwrap()["capability"],
            serde_json::json!(null)
        );
    }

    #[test]
    fn a_server_that_died_does_not_look_like_one_somebody_turned_off() {
        // The distinction the core goes to real trouble to keep — it is the whole reason
        // `ServerState` is not a boolean — and it survives exactly as far as the last thing that
        // renders it. Both end as "not announced"; only one of them is a process to restart.
        let died = serde_json::to_value(view(ServerState::Died).state).unwrap();
        let disabled = serde_json::to_value(view(ServerState::Disabled).state).unwrap();

        assert_eq!(died, serde_json::json!({ "state": "died" }));
        assert_eq!(disabled, serde_json::json!({ "state": "disabled" }));
        assert_ne!(died, disabled);

        // And the third way a server is not running, which carries what to check with it.
        assert_eq!(
            serde_json::to_value(
                view(ServerState::Failed { reason: "no such file".to_string() }).state
            )
            .unwrap(),
            serde_json::json!({ "state": "failed", "reason": "no such file" })
        );
    }
}
