//! What this machine offers, and how it is built.
//!
//! One place assembles every capability so there is one place to read to know what an agent on
//! the other end can reach. Each is wrapped in [`Guarded`], so the switch and the log are not
//! something a capability has to remember to ask for.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use zyris::ServeCapability;
use zyris::caps::{FileIoServer, InputServer, ScreenCaptureServer, TerminalServer};
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

/// Everything the window's Tools screen lists, in one answer.
///
/// The two paths travel with the capabilities because neither list can be read without them: an
/// audit line says `path=notes/x.txt` rather than the resolved path, so the root is what makes it
/// legible, and the file is the record the screen only shows a tail of. The root is **where a
/// relative path starts, not a boundary** — an absolute path addresses the host directly and a
/// command may `cd` anywhere — and anything rendering it has to say it that way.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Announcement {
    pub capabilities: Vec<Announced>,
    pub root: String,
    pub audit_log: String,
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
    /// What `into_capabilities` handed the node, recorded as it happened.
    ///
    /// Shared across clones so the window and the connector agree, and written once: a node
    /// rebuilt on a later dial announces the same list, and if it somehow could not, the first
    /// answer is still the one the user was told.
    announced: Arc<OnceLock<Vec<Announced>>>,
}

impl Tools {
    pub fn new(gate: Gate, log: AuditLog, root: PathBuf) -> Tools {
        Tools { gate, log, root, bus: None, announced: Arc::new(OnceLock::new()) }
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
        let capabilities = self.capabilities();
        // Remember what went out, so the window reports the node's answer rather than its own.
        // `OnceLock` rather than a plain field because `Tools` is `Clone` and this has to be the
        // same record in every clone; a second call leaves the first record standing, which is
        // what a node rebuilt on a later dial should see.
        let _ = self.announced.set(describe(&capabilities));
        capabilities
    }

    /// What was announced, for the window.
    ///
    /// **The snapshot taken when the capabilities were handed to the node, not a fresh look.**
    /// Two capabilities depend on a display server, and asking again can answer differently from
    /// what the node is actually serving: a display that went away mid-session would have the
    /// window report no `input` while every agent on the connection can still drive the pointer.
    /// A screen that states something false about what this machine is handing out is worse than
    /// a stale one, and rebuilding also reconnects to the display server on every ask.
    ///
    /// Empty before [`Self::into_capabilities`] has run, which is honest: nothing is announced
    /// until the node has them.
    pub fn announced(&self) -> Vec<Announced> {
        self.announced.get().cloned().unwrap_or_default()
    }

    /// [`Self::announced`] and the two paths that make it readable, for the window.
    pub fn announcement(&self) -> Announcement {
        Announcement {
            capabilities: self.announced(),
            root: self.root.display().to_string(),
            audit_log: self.log.path().display().to_string(),
        }
    }

    /// The first two are rooted explicitly. `PtyTerminal::default()` roots itself at
    /// `std::env::current_dir()` — `/` under a systemd unit and whatever a desktop launcher
    /// happened to set otherwise — and `LocalFileIo` has no default at all, so a relative path
    /// an agent sends would resolve somewhere different every launch.
    ///
    /// The other two need a display server, and they arrive together or not at all — see
    /// [`Self::screen_pair`]. That is why this returns a list built up rather than a literal: on
    /// a headless host it is two capabilities long, and that is a correct answer, not a failure.
    fn capabilities(&self) -> Vec<Arc<dyn ServeCapability>> {
        let mut capabilities = vec![
            self.guard(FileIoServer(zyris_fs::LocalFileIo::rooted(self.root.clone()))),
            self.guard(TerminalServer(zyris_terminal::PtyTerminal::rooted(self.root.clone()))),
        ];
        capabilities.extend(self.screen_pair());
        capabilities
    }

    /// The screen and the pointer, or neither of them.
    ///
    /// `screen_capture` enumerates the displays and `input` drives a pointer across them, in the
    /// same captured-pixel space: a point read off a screenshot is what `move_to` takes. An agent
    /// that can see the screen but not act on it is half useful, and one that can act but not see
    /// is guessing coordinates. So this is one decision, made once, and it is
    /// `EnigoInput::new` — it connects to the display server and fails when there is none. No
    /// separate probe: a second way of asking produces a second answer.
    ///
    /// The backend handed to [`zyris_screen::HostDisplays`] is the capture's own, not
    /// `HostDisplays::default()`. That default runs `ScreenBackend::detect()` a second time — a
    /// second decision where there should be one, and one that diverges the moment the capture's
    /// backend is overridden. The two must enumerate monitors through the same API or `move_to`
    /// aims at a layout the screenshot was not taken in.
    ///
    /// Called once per [`Self::capabilities`], which is once per `into_capabilities()` — so this
    /// connects to the display server when the node is built and not again. The window reads the
    /// snapshot [`Self::into_capabilities`] left rather than asking here a second time.
    fn screen_pair(&self) -> Vec<Arc<dyn ServeCapability>> {
        let capture = zyris_screen::HostScreenCapture::default();
        let backend = capture.backend();
        match zyris_input::EnigoInput::new(zyris_screen::HostDisplays(backend)) {
            Ok(input) => vec![
                self.guard(ScreenCaptureServer(capture)),
                self.guard(InputServer(input)),
            ],
            // `info!`, not `warn!`. A machine with no display server is an ordinary headless
            // server, not a fault, and a warning on every launch of a machine that will never
            // have a screen teaches people to ignore warnings. The error is named so a machine
            // that *should* have a display can say why it did not get one.
            Err(error) => {
                tracing::info!(
                    %error,
                    "no display server, so neither screen_capture nor input is announced; an agent on this node cannot see or touch a screen"
                );
                Vec::new()
            }
        }
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

/// A capability list as the window reads it.
fn describe(capabilities: &[Arc<dyn ServeCapability>]) -> Vec<Announced> {
    capabilities
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tools(dir: &Path) -> Tools {
        Tools::new(Gate::running(), AuditLog::new(dir.join("audit.jsonl")), dir.to_path_buf())
    }

    /// A `Tools` that has handed its capabilities to a node, which is the only state in which
    /// anything has been announced. `main` does this once at startup, before a window exists; a
    /// test asking `announced()` without it is asking what was announced before anything was,
    /// and the honest answer to that is nothing.
    fn announced_tools(dir: &Path) -> Tools {
        let tools = tools(dir);
        let _ = tools.clone().into_capabilities();
        tools
    }

    #[test]
    fn the_two_that_need_no_display_are_announced_with_their_tools() {
        let dir = tempfile::tempdir().unwrap();

        let announced = announced_tools(dir.path()).announced();

        let names: Vec<&str> = announced.iter().map(|c| c.name.as_str()).collect();
        // Not an equality any more: `screen_capture` and `input` follow these two on a host with
        // a display server and are absent on one without, which is the next test's subject. These
        // two depend on nothing outside the process, so they are always here and always first.
        assert!(names.starts_with(&["file_io", "terminal"]), "{names:?}");
        for capability in &announced {
            assert!(
                !capability.tools.is_empty(),
                "{} announced no tools; the window would show a capability with nothing in it",
                capability.name
            );
        }
    }

    #[test]
    fn the_announcement_is_shaped_the_way_the_window_reads_it() {
        // `ui/src/Tools.tsx` transcribes this rather than parsing it, so the field names are the
        // contract. Nothing else catches a rename on either side.
        let dir = tempfile::tempdir().unwrap();
        let announcement = announced_tools(dir.path()).announcement();

        let json = serde_json::to_value(&announcement).unwrap();

        assert!(json["capabilities"][0]["name"].is_string());
        assert!(json["capabilities"][0]["version"].is_number());
        assert!(json["capabilities"][0]["tools"].is_array());
        assert_eq!(json["root"], dir.path().display().to_string());
        assert!(json["auditLog"].as_str().unwrap().ends_with("audit.jsonl"));
    }

    #[test]
    fn the_screen_and_the_pointer_are_announced_together_or_not_at_all() {
        // Not a display test: it asserts the shape of the answer on whatever host runs it.
        // An agent that can see the screen but not act on it is half useful, and one that can
        // act but not see is guessing coordinates.
        let dir = tempfile::tempdir().unwrap();

        let names: Vec<String> =
            announced_tools(dir.path()).announced().into_iter().map(|a| a.name).collect();

        assert!(names.contains(&"terminal".to_string()));
        assert!(names.contains(&"file_io".to_string()));
        assert_eq!(
            names.contains(&"input".to_string()),
            names.contains(&"screen_capture".to_string()),
            "one of the pair was announced without the other: {names:?}"
        );
    }

    #[test]
    fn nothing_is_announced_until_the_node_has_the_capabilities() {
        // The window asks this, and before the node was built the true answer is an empty list.
        // It matters that this is not "go and look": two of the four need a display server, so a
        // fresh look can answer differently from what the node is actually serving, and a screen
        // reporting no pointer while every agent on the connection can still drive one states
        // something false about what this machine is handing out.
        let dir = tempfile::tempdir().unwrap();

        assert!(tools(dir.path()).announced().is_empty());
    }

    #[test]
    fn the_record_is_shared_with_every_clone() {
        // The connector is handed a clone and the window reads another. If the record were not
        // shared, the window would report nothing for the whole run.
        let dir = tempfile::tempdir().unwrap();
        let tools = tools(dir.path());
        let connector_copy = tools.clone();

        let _ = connector_copy.into_capabilities();

        assert!(
            !tools.announced().is_empty(),
            "the window's handle did not see what the connector's handle announced"
        );
    }
}
