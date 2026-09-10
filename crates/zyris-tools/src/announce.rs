//! What this machine offers, and how it is built.
//!
//! One place assembles every capability so there is one place to read to know what an agent on
//! the other end can reach. Each is wrapped in [`Guarded`], so the switch and the log are not
//! something a capability has to remember to ask for.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use zyris::ServeCapability;
use zyris::caps::{FileIoServer, TerminalServer};
use zyris_runtime::EventBus;

use crate::guarded::Guarded;
use crate::{AuditLog, Gate};

/// One announced capability, as the window lists it.
///
/// Read from the descriptor rather than written out by hand, so the Tools screen cannot drift
/// from what was actually announced.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Announced {
    pub name: String,
    pub version: u32,
    pub tools: Vec<String>,
}

/// Everything this machine announces, behind one switch and one log.
///
/// `Clone` because the connector may rebuild its node on every dial. What survives a rebuild is
/// not the capability values — those are made fresh here — but the [`Gate`] and the [`AuditLog`]
/// inside them: both are handles on shared state, so the switch and the log never drift apart.
#[derive(Clone)]
pub struct Tools {
    gate: Gate,
    log: AuditLog,
    root: PathBuf,
    /// Handed to every [`Guarded`] so a call announces itself as it happens. Optional for the
    /// same reason it is optional there: the audit file is the record, and a `Tools` with no bus
    /// is a complete one.
    bus: Option<EventBus>,
}

impl Tools {
    pub fn new(gate: Gate, log: AuditLog, root: PathBuf) -> Tools {
        Tools { gate, log, root, bus: None }
    }

    /// Also publish every call, so the window and the tray see what is happening rather than
    /// having to poll the file.
    pub fn with_bus(mut self, bus: EventBus) -> Tools {
        self.bus = Some(bus);
        self
    }

    pub fn gate(&self) -> &Gate {
        &self.gate
    }

    pub fn log(&self) -> &AuditLog {
        &self.log
    }

    /// Where a caller's relative paths start. **Not a boundary** — an absolute path addresses the
    /// host directly and `..` pops out of it, by decision.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// What `NodeBuilder::capability_arc` takes.
    ///
    /// `Arc<dyn ServeCapability>` does not itself implement `ServeCapability`, so these do not go
    /// through `NodeBuilder::capability`.
    pub fn into_capabilities(self) -> Vec<Arc<dyn ServeCapability>> {
        self.capabilities()
    }

    /// The announced capabilities, for the window.
    pub fn announced(&self) -> Vec<Announced> {
        self.capabilities()
            .iter()
            .map(|capability| {
                let descriptor = capability.descriptor();
                Announced {
                    name: descriptor.name,
                    version: descriptor.version,
                    tools: descriptor.tools.into_iter().map(|tool| tool.name).collect(),
                }
            })
            .collect()
    }

    /// Both capabilities are rooted explicitly. `PtyTerminal::default()` roots itself at
    /// `std::env::current_dir()` — `/` under a systemd unit and whatever a desktop launcher
    /// happened to set otherwise — and `LocalFileIo` has no default at all, so a relative path
    /// an agent sends would resolve somewhere different every launch.
    fn capabilities(&self) -> Vec<Arc<dyn ServeCapability>> {
        vec![
            self.guard(FileIoServer(zyris_fs::LocalFileIo::rooted(self.root.clone()))),
            self.guard(TerminalServer(zyris_terminal::PtyTerminal::rooted(self.root.clone()))),
        ]
    }

    /// The one place a capability is put behind the switch and the log, so a capability added
    /// later cannot quietly be announced without them.
    fn guard<C: ServeCapability>(&self, inner: C) -> Arc<dyn ServeCapability> {
        let guarded = Guarded::new(inner, self.gate.clone(), self.log.clone());
        match self.bus.clone() {
            Some(bus) => guarded.with_bus(bus),
            None => guarded,
        }
        .into_arc()
    }
}

/// Where relative paths start when nobody says otherwise: the user's home directory.
///
/// Never the current directory, for the same reason the secret store does not use it — it is `/`
/// under a systemd unit and whatever a shortcut set for a desktop launch, so the same relative
/// path from the same agent would mean a different file every launch.
pub fn default_root() -> PathBuf {
    if let Some(dirs) = directories::BaseDirs::new() {
        return dirs.home_dir().to_path_buf();
    }
    // No home directory the platform can name. `temp_dir` is at least an absolute, OS-chosen
    // path, and a person needs to be told which one their agent is working in.
    let fallback = std::env::temp_dir();
    tracing::warn!(
        root = %fallback.display(),
        "no home directory; relative paths from an agent will start here instead"
    );
    fallback
}
