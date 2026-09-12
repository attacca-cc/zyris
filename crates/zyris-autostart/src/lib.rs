//! Whether this computer connects itself when it starts.
//!
//! Two implementations that share no logic: on Windows a Task Scheduler entry, on Linux a
//! systemd user unit. The spec chose those over the simpler options — a registry Run key and a
//! `.desktop` file — because both of these support a delayed start and a retry, and a machine
//! that dials a network service one second after logon is a machine that fails to dial.

use std::path::Path;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

/// Where autostart stands on this machine.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum State {
    Enabled,
    Disabled,
    /// This machine has no mechanism we can drive — no systemd user session, an unknown
    /// platform. The string says which, because a switch that will not move and does not say
    /// why is worse than no switch.
    Unsupported(String),
}

/// The one thing the rest of the program talks to.
///
/// Which mechanism is underneath is settled once, in [`Autostart::for_this_machine`]; nothing
/// above this line knows what platform it is on.
pub struct Autostart {
    inner: Box<dyn Backend>,
}

impl Autostart {
    /// Pick the backend this machine can actually drive.
    ///
    /// One arm per platform: a systemd user unit on Linux, a Task Scheduler entry on Windows.
    /// The last arm keeps [`Unavailable`] for good, because an operating system Zyris cannot
    /// start itself on is a state to report, not a fault to raise.
    pub fn for_this_machine() -> Self {
        #[cfg(target_os = "linux")]
        let inner: Box<dyn Backend> = Box::new(linux::SystemdUser);

        #[cfg(windows)]
        let inner: Box<dyn Backend> = Box::new(windows::TaskScheduler);

        #[cfg(not(any(target_os = "linux", windows)))]
        let inner: Box<dyn Backend> = Box::new(Unavailable::new(
            "Zyris does not know how to start itself on this operating system",
        ));

        Self { inner }
    }

    /// What the switch should read right now. Read back from the machine, never remembered.
    pub fn state(&self) -> anyhow::Result<State> {
        self.inner.state()
    }

    /// Start this executable when the person signs in.
    ///
    /// The path is the caller's decision, not this crate's: under a dev run
    /// `std::env::current_exe()` is `target/debug/zyris`, which does not survive a
    /// `cargo clean`, and guessing around that here would hide the problem rather than show it.
    pub fn enable(&self, exe: &Path) -> anyhow::Result<()> {
        self.inner.enable(exe)
    }

    /// Stop starting it. Succeeds when autostart was already off.
    pub fn disable(&self) -> anyhow::Result<()> {
        self.inner.disable()
    }

    /// What is — or would be — installed, named the way a person would find it by hand.
    ///
    /// "a systemd user unit named `zyris.service`"; "a Task Scheduler entry named `Zyris`".
    /// Somebody who wants to undo this without going through Zyris has to be told where to
    /// look, and the only honest source for that sentence is the backend that writes the thing.
    ///
    /// `None` on a machine whose autostart Zyris cannot drive: there is nothing there to name.
    pub fn mechanism(&self) -> Option<String> {
        self.inner.mechanism()
    }

    /// Everything true of this machine that leaves the switch weaker than "on" suggests.
    ///
    /// Empty on Windows, where a logon trigger is the whole story. Linux is the case this
    /// exists for: the unit starts a GUI, which needs a graphical session, so Zyris starts when
    /// somebody logs in to a desktop and not when the computer boots — and nothing about
    /// [`State::Enabled`] hints that a machine switched on with nobody logged in is offline.
    ///
    /// **Takes the state it is qualifying rather than going and reading it again.** Every
    /// caller wants both halves, and a caveat fetched against a second, later read describes a
    /// machine that may have moved in between — "on, and it is not running" is exactly the pair
    /// that must not come apart. It also spares Linux a `systemctl` call it used to make twice
    /// for one screen.
    ///
    /// What is *not* in that state — on Linux, whether the enabled unit is actually up — is
    /// still read off the machine here and never remembered: somebody can disable the unit or
    /// the task, or stop the process, from outside Zyris, and a sentence qualifying a switch
    /// that is no longer on is a sentence that has stopped being true.
    pub fn caveats(&self, state: &State) -> Vec<String> {
        self.inner.caveats(state)
    }
}

/// One platform's way of doing the same three things.
///
/// `Send + Sync` because the GUI holds an `Autostart` in Tauri state, which is shared across
/// the threads that serve commands.
trait Backend: Send + Sync {
    fn state(&self) -> anyhow::Result<State>;
    fn enable(&self, exe: &Path) -> anyhow::Result<()>;
    fn disable(&self) -> anyhow::Result<()>;

    /// See [`Autostart::mechanism`].
    ///
    /// The default is `None`, which is the right answer for the one backend that installs
    /// nothing at all.
    fn mechanism(&self) -> Option<String> {
        None
    }

    /// See [`Autostart::caveats`].
    ///
    /// The default is none, which is the honest answer for a mechanism that either works or
    /// fails. It exists because `enable` cannot say this: it half succeeded, so an `Err` would
    /// undo nothing and report the wrong thing, and an `Ok` on its own throws the sentence
    /// away.
    ///
    /// `state` is the answer [`Backend::state`] has just given, handed down rather than asked
    /// for a second time. See [`Autostart::caveats`].
    fn caveats(&self, _state: &State) -> Vec<String> {
        Vec::new()
    }
}

/// The backend for a machine whose autostart mechanism Zyris cannot drive.
///
/// It carries the reason and hands out the same one every way it is asked: `state` reports it,
/// and both `enable` and `disable` fail with it. A generic "not supported" would throw away the
/// half a person can act on.
///
/// `allow(dead_code)` because it is the arm for a platform that is neither Linux nor Windows,
/// and a build is always on one platform: on the two Zyris ships for, nothing constructs this
/// and the compiler is right that nothing does. Its tests are what keep it honest.
#[allow(dead_code)]
struct Unavailable {
    reason: String,
}

#[allow(dead_code)] // See the note on the struct.
impl Unavailable {
    fn new(reason: impl Into<String>) -> Self {
        Self { reason: reason.into() }
    }
}

impl Backend for Unavailable {
    fn state(&self) -> anyhow::Result<State> {
        Ok(State::Unsupported(self.reason.clone()))
    }

    fn enable(&self, _exe: &Path) -> anyhow::Result<()> {
        anyhow::bail!("cannot turn autostart on: {}", self.reason)
    }

    fn disable(&self) -> anyhow::Result<()> {
        anyhow::bail!("cannot turn autostart off: {}", self.reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_is_what_the_window_reads() {
        assert_eq!(serde_json::to_string(&State::Enabled).unwrap(), r#""enabled""#);
        assert_eq!(serde_json::to_string(&State::Disabled).unwrap(), r#""disabled""#);
        assert_eq!(
            serde_json::to_string(&State::Unsupported("no systemd".into())).unwrap(),
            r#"{"unsupported":"no systemd"}"#
        );
    }

    #[test]
    fn unsupported_carries_a_reason_a_person_can_act_on() {
        // A switch that will not move and does not say why is the worst of the three states.
        let State::Unsupported(reason) = State::Unsupported("no systemd user session".into())
        else {
            panic!("wrong variant");
        };

        assert!(!reason.is_empty());
    }

    #[test]
    fn a_machine_that_cannot_do_this_says_the_same_thing_however_it_is_asked() {
        // The reason is the feature. An `enable` that fails with a generic "not supported"
        // while `state` knows exactly what is missing throws away the half that helps.
        let unavailable = Unavailable::new("no systemd user session");

        let State::Unsupported(reason) = unavailable.state().unwrap() else {
            panic!("wrong variant");
        };
        assert_eq!(reason, "no systemd user session");

        assert_eq!(
            unavailable.mechanism(),
            None,
            "a machine that installs nothing has nothing to name",
        );

        let refused_on = unavailable.enable(Path::new("/usr/bin/zyris")).unwrap_err().to_string();
        let refused_off = unavailable.disable().unwrap_err().to_string();

        assert!(refused_on.contains("no systemd user session"), "{refused_on}");
        assert!(refused_off.contains("no systemd user session"), "{refused_off}");
    }
}
