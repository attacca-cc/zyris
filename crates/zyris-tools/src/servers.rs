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

/// The MCP servers this run owns.
///
/// Clone it freely; every clone supervises the same servers. `main` keeps one for the window's
/// commands and hands another to the task that watches for deaths.
#[derive(Clone)]
pub struct Servers(Arc<Inner>);

struct Inner {
    /// What this machine puts in front of a capability. Held rather than the pieces, because a
    /// server that joined mid-session has to go behind the very same switch and log as one that
    /// was there at startup — see [`Tools::guard_shared`].
    tools: Tools,
    /// What the node announces. Changing this is the whole job.
    live: LiveCapabilities,
    bus: EventBus,
    /// **One lock over the whole list, held across starting a process.** That means a `list()`
    /// waits while a server is starting, up to `zyris_mcp::STARTUP_DEADLINE` for one that never
    /// speaks. The trade is deliberate: the alternative is an in-progress state that every reader
    /// has to handle, and the failure it would prevent is a window that waits rather than a window
    /// that lies.
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
        let Started { config, running } = started;
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
                    (None, true) => ServerState::Failed {
                        reason: format!(
                            "`{}` did not start when Zyris did; the reason was written to the log \
                             at the time. Turning it off and on again here will say why.",
                            config.name
                        ),
                    },
                };
                Entry { config, capability, state, running: promoted }
            })
            .collect();

        Servers(Arc::new(Inner { tools: tools.clone(), live, bus, entries: Mutex::new(entries) }))
    }

    /// Every configured server and what it is doing, in the order the file lists them.
    pub async fn list(&self) -> Vec<ServerView> {
        self.0.entries.lock().await.iter().map(Entry::view).collect()
    }

    /// Turn one server on or off, and re-announce.
    ///
    /// **Nothing is written to disk.** The file is what this machine starts from, and a switch
    /// that edited it would mean a person could not tell a server they turned off for the
    /// afternoon from one they removed. Task 5 owns whatever writing there is to do.
    ///
    /// Asking for the state it is already in is not an error and does nothing — a second click on
    /// a button that was never redrawn is not a fresh instruction.
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
                        let reason = format!(
                            "`{name}` started but could not be announced: {}",
                            error.message
                        );
                        self.0.set(&mut entries[index], ServerState::Failed { reason: reason.clone() }, None);
                        return Err(reason);
                    }
                    self.0.set(&mut entries[index], ServerState::Running, Some(promoted));
                }
                Err(error) => {
                    let reason = format!("{error:#}");
                    self.0.set(&mut entries[index], ServerState::Failed { reason: reason.clone() }, None);
                    return Err(reason);
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
    pub async fn reap(&self) -> usize {
        let mut entries = self.0.entries.lock().await;
        let mut withdrawn = 0;
        for entry in entries.iter_mut() {
            let gone = entry
                .running
                .as_ref()
                .is_some_and(|promoted| !promoted.is_running());
            if !gone {
                continue;
            }
            tracing::warn!(
                server = entry.config.name,
                capability = entry.capability.as_deref().unwrap_or("<none>"),
                "an MCP server's process is gone, so its tools are no longer announced; nobody \
                 asked for this. Nothing else on this machine is affected."
            );
            self.withdraw(entry, ServerState::Died).await;
            withdrawn += 1;
        }
        withdrawn
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
        if let Some(promoted) = &entry.running {
            promoted.stop();
        }
        self.0.set(entry, state, None);
    }
}

impl Inner {
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
