//! What this machine offers, and how it is built.
//!
//! One place assembles every capability so there is one place to read to know what an agent on
//! the other end can reach. Each is wrapped in [`Guarded`], so the switch and the log are not
//! something a capability has to remember to ask for.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use zyris::caps::{
    FileIoServer, FileTransferServer, InputServer, ScreenCaptureServer, TerminalServer,
};
use zyris::{CapabilityDescriptor, ServeCapability};
use zyris_runtime::{EventBus, LiveCapabilities};
use zyris_transfer::LocalFileTransfer;

use crate::guarded::Guarded;
use crate::transfer::Transfers;
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
    /// `file_transfer`, when this machine has a peer identity to serve it with.
    ///
    /// Optional because binding an endpoint can fail — no network, a key file that will not load
    /// — and a machine that cannot move files between its owner's computers is still a complete
    /// node with four working capabilities. Absent, nothing about transfer is announced at all,
    /// which is the same answer [`Self::screen_pair`] gives for a host with no display server and
    /// for the same reason: an agent can tell an absent tool from a broken one, and cannot tell a
    /// broken one from a working one.
    ///
    /// The value rather than the [`Transfers`] that made it, because this is the half that gets
    /// announced; the other half — the connect hook and the accept loop — belongs to `main`, and
    /// a `Tools` is not the place to hide a background task.
    transfer: Option<LocalFileTransfer>,
    /// The local MCP servers that started, already promoted, and empty on the ordinary machine
    /// that has none configured.
    ///
    /// **Finished values, for the same reason [`Self::transfer`] holds one.** Starting a server
    /// is asynchronous — a process, a handshake, a `tools/list` — and it fails in ways that are
    /// about the outside world rather than about this type: a command that is not there, a
    /// configuration file with a typo in it. `Tools::new` and [`Self::into_capabilities`] are
    /// neither async nor fallible, and making either of them so in order to start processes
    /// would put an outside-world failure in the one place that is supposed to be a list. So
    /// `zyris_mcp::config::start` runs in `main`, says what went wrong there, and what arrives
    /// here is whatever is actually running.
    ///
    /// `Arc<dyn ServeCapability>` rather than `Arc<zyris_mcp::Promoted>`, though this crate does
    /// name that one. The trait is what [`Self::capabilities`] needs, nothing here reads anything
    /// else about them — and a test can then put a capability behind this that records whether a
    /// call arrived, which is the only way to assert that the gate stopped one before it reached
    /// somebody else's process. The names are `Promoted`'s to choose and it prefixes every one of
    /// them unconditionally; nothing here re-checks that, because the check would be a second
    /// place the rule lives.
    mcp: Vec<Arc<dyn ServeCapability>>,
}

impl Tools {
    pub fn new(gate: Gate, log: AuditLog, root: PathBuf) -> Tools {
        Tools { gate, log, root, bus: None, transfer: None, mcp: Vec::new() }
    }

    /// Also publish every call, so the window and the tray see what is happening rather than
    /// having to poll the file.
    pub fn with_bus(mut self, bus: EventBus) -> Tools {
        self.bus = Some(bus);
        self
    }

    /// Also announce `file_transfer`, served by this machine's peer identity.
    ///
    /// Takes the whole of [`Transfers`] rather than a capability so there is one way to build
    /// this: the value announced here and the one the connect hook keeps current have to be the
    /// same wiring, and a caller that could pass a `LocalFileTransfer` of its own could pass one
    /// nothing ever calls `set_api` on — which refuses every send while looking announced.
    pub fn with_transfer(mut self, transfers: &Transfers) -> Tools {
        self.transfer = Some(transfers.capability());
        self
    }

    /// Also announce these promoted MCP servers, each behind the same switch and the same log as
    /// everything else this machine offers.
    ///
    /// Takes what is already running rather than a configuration file. See [`Self::mcp`] for why
    /// the starting happens outside — and note that nothing about this is conditional on the
    /// *machine*: `input` and `file_transfer` are absent when a display server or an endpoint
    /// will not answer, whereas a promoted server is absent only because nobody configured it or
    /// because it would not start, and either way it was said at the time.
    pub fn with_mcp(mut self, promoted: Vec<Arc<dyn ServeCapability>>) -> Tools {
        self.mcp = promoted;
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

    /// What is announced **right now**, and the two paths that make it readable, for the window.
    ///
    /// **The capability list is read out of `live` and is not this type's to remember.** It was a
    /// snapshot taken here as the capabilities were handed to the node, and that was right for
    /// exactly as long as an announcement could not change. Once a promoted MCP server could be
    /// turned off, turned on, or die mid-run, the snapshot meant the Tools screen went on
    /// offering agents a capability this node had withdrawn — and hid one it had added. A screen
    /// that states something false about what this machine is handing out is worse than a stale
    /// one, and a snapshot of a list that moves is *both*.
    ///
    /// Taking a [`LiveCapabilities`] as an argument rather than holding one is what stops that
    /// coming back: there is no second copy to update, so there is nothing to forget to update.
    /// The only thing that can answer this question is the list every node is built from.
    ///
    /// **It is still not a fresh look at the machine**, which is what the snapshot was really
    /// protecting and what [`LiveCapabilities::descriptors`] keeps: whether this host has a
    /// display server was decided once, by [`Self::screen_pair`], and the capability values that
    /// decision produced are what `live` holds. A display server that goes away mid-session does
    /// not take `input` out of this answer, because it does not take it off the node — every
    /// agent on the connection can still drive the pointer.
    ///
    /// Empty before the capabilities have reached a [`LiveCapabilities`], which is honest:
    /// nothing is announced until the node has them.
    pub async fn announcement(&self, live: &LiveCapabilities) -> Announcement {
        Announcement {
            capabilities: describe(&live.descriptors().await),
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
    /// [`Self::screen_pair`]. `file_transfer` needs an endpoint that bound. That is why this
    /// returns a list built up rather than a literal: on a headless host with no network it is
    /// two capabilities long, and that is a correct answer, not a failure.
    ///
    /// **`peer_transfer` is not here and does not belong here.** It is announced on the peer link
    /// by `zyris-transfer` itself; see [`crate::transfer`] for why announcing it on the Attacca
    /// connection would produce a tool that refuses every call.
    fn capabilities(&self) -> Vec<Arc<dyn ServeCapability>> {
        let mut capabilities = vec![
            self.guard(FileIoServer(zyris_fs::LocalFileIo::rooted(self.root.clone()))),
            self.guard(TerminalServer(zyris_terminal::PtyTerminal::rooted(self.root.clone()))),
        ];
        capabilities.extend(self.screen_pair());
        if let Some(transfer) = self.transfer.clone() {
            capabilities.push(self.guard(FileTransferServer(transfer)));
        }
        // Last, so the five this machine is stay first and in the order they have always been in
        // — the window lists what it is given, and a machine's own tools moving down the screen
        // because somebody installed a notes server would be a surprising way to learn that.
        //
        // **Through `self.guard`, exactly like the rest.** A promoted capability is somebody
        // else's code reached over a pipe, which makes it the one that most needs the switch in
        // front of it; and `Guarded` is also where the audit log's argument allowlist is turned
        // *off* for a promoted name, so one that reached the node unwrapped would quietly start
        // writing a third party's tool arguments into the file.
        capabilities.extend(self.mcp.iter().map(|promoted| self.guard(Shared(promoted.clone()))));
        capabilities
    }

    /// The screen and the pointer, or neither of them.
    ///
    /// `screen_capture` enumerates the displays and `input` drives a pointer across them, in the
    /// same captured-pixel space: a point read off a screenshot is what `move_to` takes. An agent
    /// that can see the screen but not act on it is half useful, and one that can act but not see
    /// is guessing coordinates. So this is one decision, made once, and it is `EnigoInput::new`.
    /// No separate probe: a second way of asking produces a second answer.
    ///
    /// **Except where `input` would lie.** A Wayland session on a non-wlroots compositor connects
    /// `EnigoInput::new` through Xwayland, whose XTEST moves nothing on screen; there the screen is
    /// announced alone (see [`input_reaches_the_screen`]). Half useful is better than a tool that
    /// reports every click as done.
    ///
    /// **That probe only detects absence on Linux, and the difference is worth knowing before
    /// trusting it.** There, `Enigo::new` tries each backend and returns
    /// `EstablishCon("no successful connection")` when none answers. On Windows the fork's
    /// `Enigo::new` does no syscall at all — it fills a struct and returns `Ok` — so the `Err`
    /// arm below is unreachable and both capabilities are announced whatever the session is.
    /// On a Windows host with no interactive desktop (a service, an SSH logon) `move_to` then
    /// fails honestly with "no displays are attached", but `click`, `scroll`, `type_text` and
    /// `key` reach `SendInput`, which returns the event count and so reports success with
    /// nothing having happened. That is the trap in CLAUDE.md — a tool an agent cannot tell
    /// apart from a working one — in its worse form, silent success rather than honest failure.
    ///
    /// Every mainline Windows path has a desktop (the window itself; the autostart task the spec
    /// gives a logon trigger), which is why this is recorded rather than fixed here. Closing it
    /// means a second, platform-specific question — `OpenInputDesktop` or
    /// `GetProcessWindowStation` — and that does not contradict "no separate probe": that rule
    /// is about not having two answers to one question, and here the first probe provably has no
    /// answer to give.
    ///
    /// The backend handed to [`zyris_screen::HostDisplays`] is the capture's own, not
    /// `HostDisplays::default()`. That default runs `ScreenBackend::detect()` a second time — a
    /// second decision where there should be one, and one that diverges the moment the capture's
    /// backend is overridden. The two must enumerate monitors through the same API or `move_to`
    /// aims at a layout the screenshot was not taken in.
    ///
    /// Called once per [`Self::capabilities`], which is once per `into_capabilities()` — so this
    /// connects to the display server when the node is built and not again. The window reads the
    /// capability values that produced through [`Self::announcement`], rather than asking here a
    /// second time and getting a second answer.
    fn screen_pair(&self) -> Vec<Arc<dyn ServeCapability>> {
        let capture = zyris_screen::HostScreenCapture::default();
        let backend = capture.backend();
        let reaches =
            input_reaches_the_screen(std::env::var_os("WAYLAND_DISPLAY").is_some(), backend);
        match zyris_input::EnigoInput::new(zyris_screen::HostDisplays(backend)) {
            Ok(_) if !reaches => {
                tracing::info!(
                    "input is not announced: this Wayland compositor offers no wlr-virtual-pointer, and XTEST into Xwayland reports success without moving anything a person can see; serving input here needs libei"
                );
                vec![self.guard(ScreenCaptureServer(capture))]
            }
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

    /// Put an already-shared capability behind this machine's switch and log.
    ///
    /// The way a promoted MCP server joins the announcement **after** startup — see
    /// [`crate::servers`]. It exists so that path cannot diverge from this one: a server enabled
    /// from the window has to go behind the same gate and write to the same file as one that was
    /// running when the node was built, and a second call site that built its own [`Guarded`]
    /// would be a second place for the audit log's MCP rule to be forgotten.
    pub(crate) fn guard_shared(
        &self,
        capability: Arc<dyn ServeCapability>,
    ) -> Arc<dyn ServeCapability> {
        self.guard(Shared(capability))
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

/// A capability that is already shared, made guardable.
///
/// [`Guarded`] takes a capability by value, and a promoted MCP server cannot be handed over by
/// value: [`Tools`] is `Clone` — the connector gets one copy and the window another — and the
/// thing behind a promoted capability is a running process, which there is exactly one of. So it
/// lives in an `Arc` and this carries a clone of the handle.
///
/// A newtype rather than an implementation of [`ServeCapability`] for `Arc<dyn ServeCapability>`,
/// because that would have to be written where the trait or the `Arc` is and is neither's to
/// write. It adds one virtual call in front of another and nothing else.
struct Shared(Arc<dyn ServeCapability>);

#[zyris::async_trait]
impl ServeCapability for Shared {
    fn descriptor(&self) -> zyris::CapabilityDescriptor {
        self.0.descriptor()
    }

    async fn dispatch(&self, call: zyris::IncomingCall) -> zyris::Result<zyris::Outgoing> {
        self.0.dispatch(call).await
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
fn describe(descriptors: &[CapabilityDescriptor]) -> Vec<Announced> {
    descriptors
        .iter()
        .map(|descriptor| Announced {
            name: descriptor.name.clone(),
            version: descriptor.version,
            tools: descriptor.tools.iter().map(|tool| tool.name.clone()).collect(),
        })
        .collect()
}

/// Whether `input` would move the pointer a person sees, rather than report success for events
/// that go nowhere.
///
/// enigo tries `wlr-virtual-pointer` first, then X11. In a Wayland session that is not wlroots —
/// GNOME, KDE — the first is absent and the second still connects, through Xwayland, whose XTEST
/// moves only Xwayland's own pointer: `move_to` returns `Ok` and `location()` even reads the new
/// point back, while the cursor on screen stays put. The screen backend stands in for "the
/// compositor is wlroots", because `ScreenBackend::Wayland` is chosen exactly when `zwlr_screencopy`
/// answers, and every mainstream wlroots compositor that offers it offers the virtual pointer too.
/// The one path that would serve those other sessions is libei, which this build does not enable.
fn input_reaches_the_screen(wayland_session: bool, backend: zyris_screen::ScreenBackend) -> bool {
    #[cfg(target_os = "linux")]
    {
        !wayland_session || matches!(backend, zyris_screen::ScreenBackend::Wayland)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (wayland_session, backend);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An X11 session drives the real pointer through XTEST, and a wlroots session through
    /// `wlr-virtual-pointer`. A Wayland session the screen reaches some other way — GNOME, KDE —
    /// would be left with XTEST into Xwayland, which moves nothing a person can see.
    #[cfg(target_os = "linux")]
    #[test]
    fn input_is_announced_only_where_it_reaches_the_screen() {
        use zyris_screen::ScreenBackend;
        assert!(input_reaches_the_screen(false, ScreenBackend::Xcap), "an X11 session");
        assert!(input_reaches_the_screen(true, ScreenBackend::Wayland), "a wlroots session");
        assert!(
            !input_reaches_the_screen(true, ScreenBackend::Xcap),
            "GNOME or KDE: XTEST into Xwayland reports success and moves nothing"
        );
    }

    /// How many tools each of this machine's own capabilities announces.
    ///
    /// Written down here so that the sentence in `README.md` — "between them that is
    /// twenty-five tools" — is checked by something rather than counted once by somebody. A
    /// capability that grows or loses a tool when the protocol stack moves fails
    /// [`the_readme_says_how_many_tools_this_machine_offers_and_it_is_still_right`] with the new
    /// number in the message, which is the one moment anybody would think to edit the README.
    ///
    /// A per-capability table rather than a single total, because the total is not knowable on
    /// every host: `screen_capture` and `input` are announced only where a display server
    /// answers. Each row is checked wherever its capability *is* announced, and between the two
    /// platforms CI runs on, every row is.
    const TOOLS_PER_CAPABILITY: &[(&str, usize)] = &[
        ("file_io", 8),
        ("terminal", 8),
        ("screen_capture", 2),
        ("input", 5),
        ("file_transfer", 2),
    ];

    /// The README's wording for `TOOLS_PER_CAPABILITY`'s total, as it is spelled there.
    ///
    /// **The words before the number are part of what is guarded**, and they are the half that
    /// went wrong. The sentence was rewritten to fold MCP promotion into its subject, and "Between
    /// them that is twenty-five tools" then read as counting the promoted servers too — whose tool
    /// count is whatever somebody installed. A guard that looked only for `twenty-five tools`
    /// could not see that: the number was right and its antecedent was not.
    ///
    /// So the phrase names the five explicitly, and the test below checks that "five" is still
    /// how many rows [`TOOLS_PER_CAPABILITY`] has. It moves when somebody rewrites that clause,
    /// which is exactly the edit that should send them back here.
    const README_TOTAL: (usize, &str) = (25, "those five that is twenty-five tools");

    fn tools(dir: &Path) -> Tools {
        Tools::new(Gate::running(), AuditLog::new(dir.join("audit.jsonl")), dir.to_path_buf())
    }

    /// What the node is serving, built the way `main` builds it: the capabilities go into a
    /// [`LiveCapabilities`], and that is the only thing anything asks afterwards.
    fn live(tools: &Tools) -> LiveCapabilities {
        LiveCapabilities::new(tools.clone().into_capabilities())
    }

    /// What the window's Tools screen would list for a machine with nothing but its own
    /// capabilities. `main` hands them to a [`LiveCapabilities`] once at startup, before a window
    /// exists; a test asking without that is asking what was announced before anything was, and
    /// the honest answer to that is nothing.
    async fn announced(dir: &Path) -> Vec<Announced> {
        let tools = tools(dir);
        tools.announcement(&live(&tools)).await.capabilities
    }

    #[tokio::test]
    async fn the_two_that_need_no_display_are_announced_with_their_tools() {
        let dir = tempfile::tempdir().unwrap();

        let announced = announced(dir.path()).await;

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

    #[tokio::test]
    async fn the_announcement_is_shaped_the_way_the_window_reads_it() {
        // `ui/src/Tools.tsx` transcribes this rather than parsing it, so the field names are the
        // contract. Nothing else catches a rename on either side.
        let dir = tempfile::tempdir().unwrap();
        let tools = tools(dir.path());
        let announcement = tools.announcement(&live(&tools)).await;

        let json = serde_json::to_value(&announcement).unwrap();

        assert!(json["capabilities"][0]["name"].is_string());
        assert!(json["capabilities"][0]["version"].is_number());
        assert!(json["capabilities"][0]["tools"].is_array());
        assert_eq!(json["root"], dir.path().display().to_string());
        assert!(json["auditLog"].as_str().unwrap().ends_with("audit.jsonl"));
    }

    #[tokio::test]
    async fn the_screen_and_the_pointer_are_announced_together_or_not_at_all() {
        // Not a display test: it asserts the shape of the answer on whatever host runs it.
        // An agent that can see the screen but not act on it is half useful, and one that can
        // act but not see is guessing coordinates.
        let dir = tempfile::tempdir().unwrap();

        let names: Vec<String> =
            announced(dir.path()).await.into_iter().map(|a| a.name).collect();

        assert!(names.contains(&"terminal".to_string()));
        assert!(names.contains(&"file_io".to_string()));
        assert_eq!(
            names.contains(&"input".to_string()),
            names.contains(&"screen_capture".to_string()),
            "one of the pair was announced without the other: {names:?}"
        );
    }

    #[tokio::test]
    async fn nothing_is_announced_until_the_node_has_the_capabilities() {
        // The window asks this, and before the node was built the true answer is an empty list.
        // It matters that this is not "go and look": two of the four need a display server, so a
        // fresh look can answer differently from what the node is actually serving, and a screen
        // reporting no pointer while every agent on the connection can still drive one states
        // something false about what this machine is handing out.
        let dir = tempfile::tempdir().unwrap();
        let tools = tools(dir.path());

        let empty = LiveCapabilities::default();

        assert!(tools.announcement(&empty).await.capabilities.is_empty());
    }

    #[tokio::test]
    async fn the_window_reads_what_the_node_announces_rather_than_looking_again() {
        // **The defect this replaced a snapshot to fix, in its smallest form.** What the Tools
        // screen lists has to be what the node is serving at the moment it is asked — not what it
        // was serving when the process started, and not what a fresh look at this machine would
        // say. A promoted MCP server turned off, one turned on, and one whose process fell over
        // all move that list while the node is up; `servers_come_and_go.rs` drives the whole of
        // that path, and this pins the property it rests on.
        //
        // Removing a built-in is how it is asserted rather than adding a promoted server,
        // because it is the assertion a re-look cannot pass: anything that answered by rebuilding
        // this machine's capabilities would hand `file_io` straight back.
        let dir = tempfile::tempdir().unwrap();
        let tools = tools(dir.path());
        let live = live(&tools);
        assert!(names(&tools.announcement(&live).await).contains(&"file_io".to_string()));

        assert!(live.remove("file_io").await, "it was announced");

        let after = names(&tools.announcement(&live).await);
        assert!(!after.contains(&"file_io".to_string()), "{after:?}");
        assert!(after.contains(&"terminal".to_string()), "only the one named goes: {after:?}");
    }

    fn names(announcement: &Announcement) -> Vec<String> {
        announcement.capabilities.iter().map(|c| c.name.clone()).collect()
    }

    #[tokio::test]
    async fn a_machine_with_no_peer_identity_announces_no_file_transfer() {
        // The honest answer when the endpoint would not bind. `send_to` needs somewhere to send
        // from, and one announced without it would refuse every call — which an agent reads as a
        // broken machine rather than as a machine that does not do this.
        let dir = tempfile::tempdir().unwrap();

        let names: Vec<String> =
            announced(dir.path()).await.into_iter().map(|a| a.name).collect();

        assert!(!names.contains(&"file_transfer".to_string()), "{names:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_machine_with_one_announces_file_transfer_and_nothing_else_new() {
        // Two claims in one test because they are one decision. `file_transfer` is the surface an
        // agent calls and it is announced; `peer_transfer` is the wire between two machines,
        // `zyris-transfer` announces it on the peer link itself, and one announced *here* would
        // have no peer to pull bytes from and would refuse every call it received.
        let dir = tempfile::tempdir().unwrap();
        let transfers = crate::Transfers::bind(
            dir.path(),
            dir.path().join("root"),
            Arc::new(crate::transfer::DenyUnknown),
        )
        .await
        .unwrap();
        let tools = tools(dir.path()).with_transfer(&transfers);

        let announced = tools.announcement(&live(&tools)).await.capabilities;

        let transfer = announced
            .iter()
            .find(|capability| capability.name == "file_transfer")
            .expect("a machine with a peer identity offers to send files");
        let mut tools_offered = transfer.tools.clone();
        tools_offered.sort();
        assert_eq!(tools_offered, ["inbox_list", "send_to"]);
        assert!(
            !announced.iter().any(|capability| capability.name == "peer_transfer"),
            "peer_transfer is announced on the peer link, never on this one: {announced:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_readme_says_how_many_tools_this_machine_offers_and_it_is_still_right() {
        // **The number in the README was written once and nothing has ever checked it.** It is
        // the front page's only quantity, and a wrong one is exactly the class of overclaim this
        // project has already shipped more than once — so it is pinned here, against the
        // descriptors the node is actually handed rather than against a count in a comment.
        let dir = tempfile::tempdir().unwrap();
        // With a peer identity, so `file_transfer`'s row is checked too; without one it is not
        // announced and would be the one row nothing ever looked at.
        let transfers = crate::Transfers::bind(
            dir.path(),
            dir.path().join("root"),
            Arc::new(crate::transfer::DenyUnknown),
        )
        .await
        .unwrap();
        let tools = tools(dir.path()).with_transfer(&transfers);
        let announced = tools.announcement(&live(&tools)).await.capabilities;

        for capability in announced {
            let (_, expected) = TOOLS_PER_CAPABILITY
                .iter()
                .find(|(name, _)| *name == capability.name)
                .unwrap_or_else(|| {
                    panic!(
                        "`{}` is announced and is not in the README's count; add it to \
                         TOOLS_PER_CAPABILITY and say so on the front page",
                        capability.name
                    )
                });
            assert_eq!(
                capability.tools.len(),
                *expected,
                "`{}` now announces {:?}. README.md says this machine offers {}; the new total is \
                 {}.",
                capability.name,
                capability.tools,
                README_TOTAL.1,
                TOOLS_PER_CAPABILITY.iter().map(|(_, n)| n).sum::<usize>() - expected
                    + capability.tools.len(),
            );
        }

        let total: usize = TOOLS_PER_CAPABILITY.iter().map(|(_, count)| count).sum();
        assert_eq!(total, README_TOTAL.0);
        // The subject of the count, not only the count. A sixth capability here without a
        // rewrite of that clause would leave the README saying "those five".
        assert_eq!(
            TOOLS_PER_CAPABILITY.len(),
            5,
            "README.md says `{}`; the table it is counting now has {} rows",
            README_TOTAL.1,
            TOOLS_PER_CAPABILITY.len()
        );
        let readme = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../README.md"),
        )
        .expect("the README is two directories up from this crate");
        assert!(
            readme.contains(README_TOTAL.1),
            "README.md no longer says `{}`, so this test is guarding a sentence that has moved",
            README_TOTAL.1
        );
    }

    /// A capability built at runtime under a name of its own, which records whether a call ever
    /// got to it.
    ///
    /// This is the shape `zyris-mcp`'s `Promoted` has — assembled from a tool list rather than by
    /// the capability macro, named from a configuration file — and it is a fake here rather than
    /// a real promoted server for one reason: **the assertions below are about calls that must
    /// not arrive.** A live MCP server cannot prove that the gate stopped a call before it
    /// reached the process; a capability that writes down every call it receives can.
    struct Promotable {
        name: String,
        reached: Arc<std::sync::atomic::AtomicBool>,
    }

    impl Promotable {
        fn new(name: &str) -> (Promotable, Arc<std::sync::atomic::AtomicBool>) {
            let reached = Arc::new(std::sync::atomic::AtomicBool::new(false));
            (Promotable { name: name.to_string(), reached: reached.clone() }, reached)
        }
    }

    #[zyris::async_trait]
    impl ServeCapability for Promotable {
        fn descriptor(&self) -> zyris::CapabilityDescriptor {
            zyris::CapabilityDescriptor {
                name: self.name.clone(),
                version: 1,
                tools: vec![zyris::ToolDescriptor {
                    name: "search".to_string(),
                    description: "Somebody else's tool.".to_string(),
                    transfer: zyris::Transfer::Unary,
                    request_schema: serde_json::json!({}),
                    response_schema: None,
                    item_schema: None,
                    call_limit: None,
                }],
            }
        }

        async fn dispatch(&self, _call: zyris::IncomingCall) -> zyris::Result<zyris::Outgoing> {
            self.reached.store(true, std::sync::atomic::Ordering::SeqCst);
            zyris::encode_response(&serde_json::json!({ "ok": true }))
        }
    }

    /// The one MCP server's name used below. Hyphenated and nothing like a built-in, so a
    /// capability list that hardcoded a plausible name would not satisfy these.
    const PROMOTED: &str = "mcp_desk-notes";

    fn call(tool: &str, params: serde_json::Value) -> zyris::IncomingCall {
        zyris::IncomingCall {
            tool: tool.to_string(),
            params: zyris::Payload::from_json(params),
            serialization: zyris::Serialization::Json,
            meta: zyris::Payload::default(),
        }
    }

    /// Arguments belonging to somebody else's tool, two of them spelled the way this machine's own
    /// allowlist spells its own, and one that is plainly a secret.
    fn foreign_arguments() -> serde_json::Value {
        serde_json::json!({
            "path": "/etc/shadow",
            "command": "psql -c 'select * from customers'",
            "passphrase": "hunter2-do-not-log-me",
        })
    }

    /// The announced capability under `name`, as the node would have it.
    fn announced_capability(
        capabilities: &[Arc<dyn ServeCapability>],
        name: &str,
    ) -> Arc<dyn ServeCapability> {
        capabilities
            .iter()
            .find(|capability| capability.descriptor().name == name)
            .unwrap_or_else(|| panic!("`{name}` was not announced"))
            .clone()
    }

    #[tokio::test]
    async fn a_promoted_capability_is_announced_beside_the_built_ins() {
        let dir = tempfile::tempdir().unwrap();
        let (promotable, _) = Promotable::new(PROMOTED);
        let tools = tools(dir.path()).with_mcp(vec![Arc::new(promotable)]);

        let announced = tools.announcement(&live(&tools)).await.capabilities;

        let promoted = announced
            .iter()
            .find(|capability| capability.name == PROMOTED)
            .expect("a configured MCP server is announced like anything else");
        assert_eq!(promoted.tools, ["search"]);
        // And it is an addition rather than a replacement: the window lists one machine.
        let names: Vec<&str> = announced.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"file_io") && names.contains(&"terminal"), "{names:?}");
    }

    #[tokio::test]
    async fn a_machine_with_no_mcp_servers_announces_none() {
        let dir = tempfile::tempdir().unwrap();

        let announced = announced(dir.path()).await;

        assert!(
            !announced.iter().any(|c| c.name.starts_with(crate::guarded::MCP_CAPABILITY_PREFIX)),
            "{announced:?}"
        );
    }

    #[tokio::test]
    async fn a_promoted_capability_is_behind_the_pause_switch() {
        // The claim that makes promotion safe: an MCP server is somebody else's code, and the
        // switch that stops this machine's own tools has to stop it too. Asserted by what the
        // capability *never saw* — a promoted capability announced without `guard` would answer
        // this call itself and nothing else would differ.
        let dir = tempfile::tempdir().unwrap();
        let (promotable, reached) = Promotable::new(PROMOTED);
        let tools = tools(dir.path()).with_mcp(vec![Arc::new(promotable)]);
        tools.gate().set_paused(true);

        let capabilities = tools.clone().into_capabilities();
        let refused = announced_capability(&capabilities, PROMOTED)
            .dispatch(call("search", foreign_arguments()))
            .await;

        assert!(refused.is_err(), "a paused machine ran somebody else's tool");
        assert!(
            !reached.load(std::sync::atomic::Ordering::SeqCst),
            "the call reached the MCP server while this machine was paused"
        );
        // And the refusal is on the record, which is what a person reads afterwards.
        let entry = &tools.log().recent(1).unwrap()[0];
        assert_eq!(entry.capability, PROMOTED);
        assert_eq!(entry.outcome, crate::Outcome::Refused);
    }

    #[tokio::test]
    async fn a_promoted_capabilitys_call_is_logged_without_its_arguments() {
        // Two halves of one decision, and both have to be asserted here rather than only in
        // `guarded.rs`: that a promoted capability is wrapped at all, and that being wrapped
        // writes no arguments down. `Guarded::new` decides the second from the capability's name,
        // so a promoted capability that reached the node unwrapped would take the audit exemption
        // with it and a third party's arguments would start landing in the file.
        let dir = tempfile::tempdir().unwrap();
        let (promotable, reached) = Promotable::new(PROMOTED);
        let tools = tools(dir.path()).with_mcp(vec![Arc::new(promotable)]);

        let capabilities = tools.clone().into_capabilities();
        announced_capability(&capabilities, PROMOTED)
            .dispatch(call("search", foreign_arguments()))
            .await
            .expect("the call runs");

        assert!(
            reached.load(std::sync::atomic::Ordering::SeqCst),
            "the call has to have actually run, or the assertions below prove nothing"
        );
        let entry = &tools.log().recent(1).unwrap()[0];
        assert_eq!(entry.capability, PROMOTED);
        assert_eq!(entry.tool, "search");
        assert_eq!(entry.outcome, crate::Outcome::Allowed);
        assert_eq!(
            entry.detail, "",
            "a promoted tool's arguments reached the audit log: {}",
            entry.detail
        );
        let written = std::fs::read_to_string(tools.log().path()).unwrap();
        assert!(!written.contains("hunter2"), "{written}");
        assert!(!written.contains("/etc/shadow"), "{written}");
    }

    #[tokio::test]
    async fn no_capability_this_machine_announces_itself_starts_with_the_promoted_prefix() {
        // The one standing condition behind `zyris-mcp`'s always-prefix rule, checked against the
        // real list rather than a copy of it. A sixth built-in called `mcp_anything` would put a
        // promoted server's name space inside this machine's own — and, through `Guarded`, would
        // silently stop its arguments being written to the audit log.
        let dir = tempfile::tempdir().unwrap();

        for capability in announced(dir.path()).await {
            assert!(
                !capability.name.starts_with(zyris_mcp::CAPABILITY_PREFIX),
                "the built-in `{}` starts with `{}`",
                capability.name,
                zyris_mcp::CAPABILITY_PREFIX
            );
        }
    }

    #[tokio::test]
    async fn the_window_and_the_connector_read_one_announcement() {
        // The connector is handed a clone of the `Tools` and the window keeps another, and what
        // each of them reports has to be the same list. It is, because neither of them holds a
        // list: the capabilities the connector's clone produced are the ones the window's clone
        // describes, through the [`LiveCapabilities`] between them.
        let dir = tempfile::tempdir().unwrap();
        let tools = tools(dir.path());
        let connector_copy = tools.clone();

        let live = LiveCapabilities::new(connector_copy.into_capabilities());

        assert!(
            !tools.announcement(&live).await.capabilities.is_empty(),
            "the window's handle did not see what the connector's handle announced"
        );
    }
}
