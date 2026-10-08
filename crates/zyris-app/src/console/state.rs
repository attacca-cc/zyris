//! What the running node tells a console command about itself.
//!
//! **A file, because there is no socket to ask.** The node is one process and `zyris status` is
//! another, and the only thing this program has ever shared between two of them is the instance
//! lock — which answers "is one running" and nothing else. So the node writes what it knows into
//! its instance's data directory as it learns it, and the console reads that.
//!
//! It is a **report, never an authority**: nothing decides anything by it. Whether a node is
//! running is [`zyris_runtime::lock::InstanceLock::is_held`], which cannot be stale, while this
//! file can be — a node that was killed leaves its last line behind, and `zyris status` says so
//! rather than reading a stale `connected` as a live connection. The one thing the console takes
//! from here is the pid, and only to name it to a person.
//!
//! Written only on change. A connected node is idle, and there is nothing to say about it that
//! saying again would improve; `updated_unix_ms` is therefore the last time something happened,
//! not a heartbeat.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::runtime::Handle;
use zyris_runtime::{CoreEvent, EventBus};

use crate::cli::Mode;

/// What the state file is called, inside the instance's data directory.
pub const STATE_FILE: &str = "node-state.json";

/// Where the state file is for a run whose state is in `data`.
pub fn path(data: &Path) -> PathBuf {
    data.join(STATE_FILE)
}

/// The code a person enters to authorize this machine.
///
/// In the file as well as on the screen, because `zyris login` is exactly "show me the code" and
/// the machine may already be enrolling: a node that is up and waiting for a person has the code,
/// and a console command that started a *second* enrolment to produce one would leave the account
/// with two pending requests for one machine. Not a secret: this is the string the person types
/// into a browser, and it is useless without the account that asked for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Code {
    pub user_code: String,
    pub verification_uri: String,
}

/// Where this node is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Phase {
    /// The process is up and has not said anything else yet.
    Starting,
    /// No credential, and either waiting for a person to authorize this machine or asking.
    Enrolling,
    /// A dial is in flight — including every reconnect the link makes on its own.
    Connecting,
    Connected,
    /// The link is down. `retrying` is the same distinction `CoreEvent::Disconnected` draws: the
    /// link is backing off to dial again, or the actor has stopped for good.
    Disconnected { retrying: bool },
    /// Something ended this run's attempt to connect, terminally.
    Failed,
}

impl Phase {
    /// One word for a console, which is what `zyris status` prints.
    pub fn label(self) -> &'static str {
        match self {
            Phase::Starting => "starting",
            Phase::Enrolling => "not authorized yet",
            Phase::Connecting => "connecting",
            Phase::Connected => "connected",
            Phase::Disconnected { retrying: true } => "disconnected, redialling",
            Phase::Disconnected { retrying: false } => "disconnected",
            Phase::Failed => "failed",
        }
    }
}

/// Everything a console command can learn about the running node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeState {
    /// Which instance wrote this, so a state file found under the wrong name is not read as this
    /// machine's.
    pub instance: String,
    pub pid: u32,
    /// [`Mode::name`] of the run that wrote it.
    pub mode: String,
    /// The server this run dials, which for a `--server` run is not the shipped one.
    pub server: String,
    pub started_unix_ms: u64,
    /// When the phase last changed. Not a heartbeat — see the module.
    pub updated_unix_ms: u64,
    pub phase: Phase,
    /// Why, when the phase is one of the unhappy ones: the reason out of the event.
    pub detail: Option<String>,
    /// The identity the server gave this node, once it has one.
    pub node_id: Option<String>,
    pub node_name: Option<String>,
    /// The code to show a person, while there is one.
    pub enrolment: Option<Code>,
}

impl NodeState {
    /// The first line of a run that is holding — or about to hold — its instance's lock.
    pub fn new(instance: &str, mode: Mode, server: &str) -> NodeState {
        let now = now_ms();
        NodeState {
            instance: instance.to_string(),
            pid: std::process::id(),
            mode: mode.name().to_string(),
            server: server.to_string(),
            started_unix_ms: now,
            updated_unix_ms: now,
            phase: Phase::Starting,
            detail: None,
            node_id: None,
            node_name: None,
            enrolment: None,
        }
    }

    /// Fold one core event in, and answer whether that changed anything.
    ///
    /// Everything the node does that a person would want to see from a console is in this match.
    /// What is deliberately not: a tool call, the pause switch, a peer question, an MCP server
    /// going up or down. Those are things happening to an agent on a live connection, they can be
    /// several a second, and a file rewritten for each of them would be a file whose last line
    /// says nothing about whether the node is connected — which is the question it is here to
    /// answer. The audit log and the window's Tools screen are where those belong.
    pub fn observe(&mut self, event: &CoreEvent) -> bool {
        let mut next = self.clone();
        match event {
            CoreEvent::NeedsEnrolment => {
                next.phase = Phase::Enrolling;
                // The code the last attempt carried belongs to that attempt: a renewed enrolment
                // sends a new `EnrolmentCode`, and until it does there is nothing to show.
                next.enrolment = None;
                next.detail = None;
            }
            CoreEvent::EnrolmentCode { user_code, verification_uri } => {
                next.phase = Phase::Enrolling;
                next.enrolment =
                    Some(Code { user_code: user_code.clone(), verification_uri: verification_uri.clone() });
            }
            CoreEvent::EnrolmentFailed { reason } => {
                next.phase = Phase::Failed;
                next.enrolment = None;
                next.detail = Some(reason.clone());
            }
            CoreEvent::Connecting => {
                next.phase = Phase::Connecting;
                // An identity is true of a connection, and this is the moment there is not one:
                // the node id the server handed out belongs to the link that just went down.
                next.node_id = None;
                next.node_name = None;
                next.detail = None;
            }
            CoreEvent::Connected { node_id, node_name } => {
                next.phase = Phase::Connected;
                next.node_id = Some(node_id.clone());
                next.node_name = Some(node_name.clone());
                next.detail = None;
                next.enrolment = None;
            }
            CoreEvent::Disconnected { reason, retrying } => {
                next.phase = Phase::Disconnected { retrying: *retrying };
                next.node_id = None;
                next.node_name = None;
                next.detail = Some(reason.clone());
            }
            CoreEvent::SetupFailed { reason } => {
                next.phase = Phase::Failed;
                next.detail = Some(reason.clone());
            }
            // The rest. `Started` and `ShuttingDown` are the core's own edges rather than
            // anything about the link; the others are covered by the paragraph above.
            CoreEvent::Started
            | CoreEvent::ShuttingDown
            | CoreEvent::Paused { .. }
            | CoreEvent::ToolCall { .. }
            | CoreEvent::NeedsPeerApproval { .. }
            | CoreEvent::McpServer { .. } => return false,
        }

        if *self == next {
            return false;
        }
        next.updated_unix_ms = now_ms();
        *self = next;
        true
    }

    /// Write it where the console reads it.
    ///
    /// Through a temporary file and renamed, like `zyris-voice`'s settings writer and for the same
    /// reason: a console command may read this while it is being written, and half a JSON document
    /// is not a state. Failure is the caller's to report — nothing a console convenience does may
    /// take the node down.
    pub fn save(&self, data: &Path) -> io::Result<()> {
        std::fs::create_dir_all(data)?;
        let path = path(data);
        let part = path.with_extension(format!("part-{}", std::process::id()));
        let mut bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        bytes.push(b'\n');
        std::fs::write(&part, &bytes)?;
        std::fs::rename(&part, &path).inspect_err(|_| {
            let _ = std::fs::remove_file(&part);
        })
    }

    /// What the last node to run on this machine wrote, if anything is readable.
    ///
    /// `None` is "nothing there" and "nothing readable" at once, and deliberately: a person
    /// running `zyris status` has the lock's answer to whether a node is running, and the file is
    /// the detail rather than the answer. A file that will not parse is logged and reads as
    /// nothing.
    pub fn read(data: &Path) -> Option<NodeState> {
        let text = match std::fs::read_to_string(path(data)) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
            Err(error) => {
                tracing::warn!(%error, "could not read the console's state file");
                return None;
            }
        };
        match serde_json::from_str(&text) {
            Ok(state) => Some(state),
            Err(error) => {
                tracing::warn!(%error, "the console's state file could not be parsed");
                None
            }
        }
    }
}

/// Keeps the state file current for as long as the node runs.
///
/// Built in `main`, for both runtimes, and **started by whichever runtime takes the instance
/// lock** — `main` for a headless run, `gui.rs`'s `setup` for a windowed one. That is later than
/// it looks it should be in the windowed case, and it is the point: a second window launch is
/// refused by its lock and exits, and one that had already written a state file naming its own pid
/// would leave a file pointing at a process that is gone.
pub struct Watcher {
    bus: EventBus,
    data: PathBuf,
    state: NodeState,
}

impl Watcher {
    pub fn new(bus: &EventBus, data: &Path, instance: &str, mode: Mode, server: Option<&str>) -> Watcher {
        Watcher {
            bus: bus.clone(),
            data: data.to_path_buf(),
            state: NodeState::new(
                instance,
                mode,
                server.unwrap_or(zyris_runtime::DEFAULT_SERVER_URL),
            ),
        }
    }

    /// Write the first line, take a subscription, and keep it current until the process ends.
    ///
    /// **Subscribed before the first write and before anything else runs**, because `broadcast`
    /// never replays: the connector publishes `NeedsEnrolment` or a `Connecting` within
    /// microseconds of being spawned, and a watcher that arrived after that would write the first
    /// line and then never correct it.
    pub fn start(mut self, runtime: &Handle) {
        // Written before the task exists, and not inside it: `zyris down` reads this file to
        // report which process it asked, and a node that is up and connected but has not yet been
        // scheduled a task is still a node that is up.
        if let Err(error) = self.state.save(&self.data) {
            tracing::warn!(%error, "could not write the console's state file");
        }

        let mut events = self.bus.subscribe();
        runtime.spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) => {
                        if self.state.observe(&event) {
                            if let Err(error) = self.state.save(&self.data) {
                                tracing::warn!(%error, "could not write the console's state file");
                            }
                        }
                    }
                    // Falling behind costs nothing here, unlike the window's own forwarder: what
                    // this file holds is the latest of each thing, and the next event of the same
                    // kind says it again. `Connecting` and `Connected` in particular arrive as a
                    // pair, so a missed `Connecting` is corrected by the `Connected` after it.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                        tracing::debug!(missed, "the console's state file fell behind");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        });
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or_default()
}

/// How long ago `at_unix_ms` was, in the words a person would use: `3s`, `12m`, `4h 2m`, `3d`.
///
/// In the `status` block rather than an absolute time, because the question it answers is "when
/// did this last happen", and a timestamp makes the reader do the subtraction. `None` for a time
/// in the future, which is a clock that moved rather than an age.
pub fn age(at_unix_ms: u64) -> Option<String> {
    let seconds = (now_ms().saturating_sub(at_unix_ms)) / 1000;
    if at_unix_ms > now_ms() {
        return None;
    }
    Some(match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m", seconds / 60),
        3600..=86_399 => {
            let (hours, minutes) = (seconds / 3600, (seconds % 3600) / 60);
            if minutes == 0 { format!("{hours}h") } else { format!("{hours}h {minutes}m") }
        }
        _ => format!("{}d", seconds / 86_400),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A state to fold events into, with the fields that do not matter here filled in.
    fn starting() -> NodeState {
        NodeState::new("zyris", Mode::WindowHidden, "wss://example.invalid/ws")
    }

    #[test]
    fn the_first_line_names_this_process_and_nothing_else() {
        let state = starting();

        assert_eq!(state.instance, "zyris");
        assert_eq!(state.pid, std::process::id());
        assert_eq!(state.mode, "window-hidden");
        assert_eq!(state.phase, Phase::Starting);
        assert_eq!(state.node_id, None);
    }

    #[test]
    fn a_connection_and_the_identity_it_gave_are_recorded() {
        let mut state = starting();

        assert!(state.observe(&CoreEvent::NeedsEnrolment));
        assert_eq!(state.phase, Phase::Enrolling);

        assert!(state.observe(&CoreEvent::EnrolmentCode {
            user_code: "WDJB-MJHT".into(),
            verification_uri: "https://attacca.cc/device".into(),
        }));
        assert_eq!(
            state.enrolment,
            Some(Code {
                user_code: "WDJB-MJHT".into(),
                verification_uri: "https://attacca.cc/device".into(),
            }),
            "the code is in the file because `zyris login` may be the only way to see it"
        );

        assert!(state.observe(&CoreEvent::Connecting));
        assert!(state.observe(&CoreEvent::Connected {
            node_id: "n_01H".into(),
            node_name: "ruma/zyris/desktop".into(),
        }));
        assert_eq!(state.phase, Phase::Connected);
        assert_eq!(state.phase, Phase::Connected);
        assert_eq!(state.node_name.as_deref(), Some("ruma/zyris/desktop"));
        assert_eq!(state.enrolment, None, "there is no code to show once it is granted");
    }

    #[test]
    fn a_redial_forgets_an_identity_that_belonged_to_the_dead_link() {
        // The node id belongs to a connection. Keeping it through a redial would make `status`
        // name a node path that nothing is answering on, which is the one thing a status command
        // must not do.
        let mut state = starting();
        state.observe(&CoreEvent::Connected { node_id: "n_01H".into(), node_name: "here".into() });

        assert!(state.observe(&CoreEvent::Disconnected {
            reason: "the link was closed".into(),
            retrying: true,
        }));

        assert_eq!(state.phase, Phase::Disconnected { retrying: true });
        assert_ne!(state.phase, Phase::Connected);
        assert_eq!(state.node_id, None);
        assert_eq!(state.detail.as_deref(), Some("the link was closed"));
    }

    #[test]
    fn the_two_reasons_a_run_can_be_over_are_both_failures() {
        let mut state = starting();
        state.observe(&CoreEvent::EnrolmentFailed { reason: "declined".into() });
        assert_eq!(state.phase, Phase::Failed);
        assert_eq!(state.detail.as_deref(), Some("declined"));

        let mut state = starting();
        state.observe(&CoreEvent::SetupFailed { reason: "no keychain".into() });
        assert_eq!(state.phase, Phase::Failed);
        assert_eq!(state.detail.as_deref(), Some("no keychain"));
    }

    #[test]
    fn an_event_that_says_nothing_about_the_link_changes_nothing() {
        // A tool call several times a second must not rewrite this file, or the file's own
        // timestamp would stop meaning "something happened to the connection".
        let mut state = starting();
        let written = state.updated_unix_ms;

        for event in [
            CoreEvent::Started,
            CoreEvent::Paused { paused: true },
            CoreEvent::ToolCall {
                capability: "terminal".into(),
                tool: "exec".into(),
                detail: "uname -a".into(),
                outcome: "ok".into(),
            },
            CoreEvent::ShuttingDown,
        ] {
            assert!(!state.observe(&event), "{event:?} should not be a change");
        }
        assert_eq!(state.phase, Phase::Starting);
        assert_eq!(state.updated_unix_ms, written);
    }

    #[test]
    fn saying_the_same_thing_twice_is_not_a_change() {
        let mut state = starting();
        assert!(state.observe(&CoreEvent::Connecting));
        assert!(!state.observe(&CoreEvent::Connecting), "the same phase twice is not news");

        assert!(state.observe(&CoreEvent::Connected { node_id: "n".into(), node_name: "a".into() }));
        assert!(!state.observe(&CoreEvent::Connected { node_id: "n".into(), node_name: "a".into() }));
        assert!(
            state.observe(&CoreEvent::Connected { node_id: "n".into(), node_name: "b".into() }),
            "a different node name is"
        );
    }

    #[test]
    fn what_is_written_is_what_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = starting();
        state.observe(&CoreEvent::Connected { node_id: "n_01H".into(), node_name: "here".into() });

        state.save(dir.path()).unwrap();

        assert_eq!(NodeState::read(dir.path()), Some(state));
    }

    #[test]
    fn a_file_that_is_not_a_state_is_not_read_as_one() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(path(dir.path()), b"{ not json").unwrap();

        assert_eq!(NodeState::read(dir.path()), None);
        assert_eq!(NodeState::read(&dir.path().join("nowhere")), None);
    }

    #[test]
    fn an_age_is_said_the_way_a_person_would_say_it() {
        let now = now_ms();
        assert_eq!(age(now).as_deref(), Some("0s"));
        assert_eq!(age(now - 12_000).as_deref(), Some("12s"));
        assert_eq!(age(now - 12 * 60_000).as_deref(), Some("12m"));
        assert_eq!(age(now - 4 * 3_600_000).as_deref(), Some("4h"));
        assert_eq!(age(now - (4 * 3_600_000 + 2 * 60_000)).as_deref(), Some("4h 2m"));
        assert_eq!(age(now - 3 * 86_400_000).as_deref(), Some("3d"));
        assert_eq!(age(now + 60_000), None, "a time in the future is not an age");
    }
}
