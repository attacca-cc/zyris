//! Autostart on Linux: a systemd user unit.
//!
//! A unit file at `~/.config/systemd/user/zyris.service`, enabled into
//! `graphical-session.target.wants`. A `.desktop` file under `~/.config/autostart` — what
//! `tauri-plugin-autostart` writes — would have been a quarter of this code, and it has no
//! delayed start and no retry.
//!
//! **This starts a desktop session program, not a daemon, and that is a deliberate trade.** The
//! unit runs `zyris --minimized`: a full GUI with a tray icon, whose window simply is not on the
//! screen. It has to be, because the alternative does not work — `--headless` takes the instance
//! lock and runs neither a tray nor the single-instance plugin, so a machine with autostart on
//! had no window, no tray icon, and no way to get either. What it costs is the thing lingering
//! used to buy: a machine switched on with nobody logged in is not connected, because there is
//! no display server for a GUI to start into. [`SystemdUser::caveats`] says so on the Settings
//! screen rather than leaving it to be discovered.
//!
//! Nothing here remembers anything. Every question is answered by asking systemd again, because
//! a person can turn this off with `systemctl --user disable` without telling Zyris.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::Context as _;

use crate::{Backend, State};

/// The unit's name, which is also what a person types to look at it by hand. Worth keeping
/// recognisable: someone scanning `systemctl --user list-unit-files` should know what it is.
const UNIT: &str = "zyris.service";

/// Said the same way wherever it comes up, because it is the one case a person can fix by
/// installing something.
const NO_SYSTEMCTL: &str = "this machine has no `systemctl`, so it has no systemd to start Zyris";

/// Autostart through a systemd user unit.
pub(crate) struct SystemdUser;

impl Backend for SystemdUser {
    fn state(&self) -> anyhow::Result<State> {
        let output = match run("systemctl", &["--user", "is-enabled", UNIT]) {
            Ok(output) => output,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Ok(State::Unsupported(NO_SYSTEMCTL.to_owned()));
            }
            Err(error) => {
                return Err(error).context("could not run `systemctl --user is-enabled`");
            }
        };

        // **The exit code, never the word.** `is-enabled` answers with one of a dozen words
        // — `enabled`, `enabled-runtime`, `linked`, `generated`, `not-found` — and which of
        // them mean on is a list that grows with systemd. Zero is the whole answer.
        if output.status.success() {
            return Ok(State::Enabled);
        }

        // A non-zero exit is "not enabled" *or* "there was nobody to ask": with no session bus
        // `systemctl --user is-enabled` also exits 1, measured. Those are not the same state
        // and a switch must not show `Disabled` for the second, so ask something only a
        // reachable user manager can answer before settling on it.
        if let Some(reason) = no_user_manager() {
            return Ok(State::Unsupported(reason));
        }

        Ok(State::Disabled)
    }

    fn enable(&self, exe: &Path) -> anyhow::Result<()> {
        if let Some(reason) = no_user_manager() {
            anyhow::bail!("cannot turn autostart on: {reason}");
        }

        let path = unit_path()?;
        let dir = path
            .parent()
            .context("the systemd user unit path has no directory to write it into")?;
        std::fs::create_dir_all(dir)
            .with_context(|| format!("could not create {}", dir.display()))?;
        std::fs::write(&path, render_unit(exe))
            .with_context(|| format!("could not write {}", path.display()))?;
        tracing::info!(
            unit = %path.display(),
            executable = %exe.display(),
            "wrote the systemd user unit",
        );

        systemctl(&["--user", "daemon-reload"])?;
        systemctl(&["--user", "enable", UNIT])?;

        // **No `loginctl enable-linger` here, deliberately.** Lingering exists so a user's
        // units can run with no session of their own, and this unit cannot: it starts a GUI,
        // which needs the `DISPLAY` or `WAYLAND_DISPLAY` only a graphical session has. Turning
        // lingering on would change a user-wide setting — one that keeps every other unit they
        // own running after logout too — and buy this one nothing. An earlier version of this
        // file did it, back when the unit started `--headless`.
        //
        // What that costs is said out loud rather than left to be discovered, both here and on
        // the Settings screen through [`SystemdUser::caveats`].
        tracing::info!(
            "Zyris will start when you log in to a desktop; a machine switched on with nobody \
             logged in is not connected",
        );

        Ok(())
    }

    fn disable(&self) -> anyhow::Result<()> {
        if let Some(reason) = no_user_manager() {
            anyhow::bail!("cannot turn autostart off: {reason}");
        }

        let path = unit_path()?;
        let installed = path.exists();

        // `systemctl --user disable` is what removes the symlink under the wants directory,
        // and it is also what clears a dangling one left behind by a unit file somebody deleted
        // by hand — so it runs either way. With no unit file it exits 1, measured, and that is
        // not a failure when the job was to make sure there is nothing there.
        let output = run("systemctl", &["--user", "disable", UNIT])
            .context("could not run `systemctl --user disable`")?;
        if installed && !output.status.success() {
            anyhow::bail!("could not disable {UNIT}: {}", complaint(&output));
        }

        if let Err(error) = std::fs::remove_file(&path) {
            if error.kind() != ErrorKind::NotFound {
                return Err(error)
                    .with_context(|| format!("could not remove {}", path.display()));
            }
        }

        systemctl(&["--user", "daemon-reload"])?;

        // **Lingering is deliberately left exactly as it is, including on a machine where an
        // older Zyris turned it on.** This version never enables it, so `disable` cannot even
        // claim to be undoing its own work — and `loginctl` does not record who asked, so
        // nothing here can tell a linger that Zyris caused from one the person set for their
        // own services. Turning off a user-wide setting we cannot prove we caused is the worse
        // mistake of the two. Somebody who wants it off types `loginctl disable-linger`.
        Ok(())
    }

    fn mechanism(&self) -> Option<String> {
        // Built from `UNIT` rather than written out, so the sentence a person is told to look
        // for cannot drift away from the file this actually writes.
        Some(format!("a systemd user unit named {UNIT}"))
    }

    fn caveats(&self) -> Vec<String> {
        caveats_for(&self.state())
    }
}

/// What is true of an enabled unit that "on" does not say by itself.
///
/// Split out from [`SystemdUser::caveats`] so the sentence can be checked without a systemd to
/// ask; the method is only the part that has to go and look.
fn caveats_for(state: &anyhow::Result<State>) -> Vec<String> {
    // Only worth saying while the switch is on. What a login-time unit costs a machine that
    // does not start Zyris at all is nothing.
    if !matches!(state, Ok(State::Enabled)) {
        return Vec::new();
    }

    vec![NOT_AT_BOOT.to_owned()]
}

/// The one thing a Linux user gives up by turning this on, in the words the Settings screen
/// and `--install-autostart` both print.
///
/// This used to be about lingering, which was the opposite trade: a headless unit that ran with
/// no session at all. The unit starts a GUI now, so it needs a session, and a session is
/// something only a logged-in person has.
const NOT_AT_BOOT: &str = "Zyris starts when you log in to a desktop, not when this computer \
                           boots: the window and the tray icon need a graphical session to \
                           start into. A machine that is switched on with nobody logged in is \
                           not connected.";

/// The unit file, as a string.
///
/// Pure, so what goes into it can be checked without a systemd to check it against.
fn render_unit(exe: &Path) -> String {
    format!(
        "[Unit]\n\
         Description=Zyris — keeps this computer connected to Attacca\n\
         After=graphical-session.target network-online.target\n\
         Wants=network-online.target\n\
         PartOf=graphical-session.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={exe} --minimized\n\
         Restart=always\n\
         RestartSec=30\n\
         \n\
         [Install]\n\
         WantedBy=graphical-session.target\n",
        exe = exec_start_word(exe),
    )
}

/// The executable path as one `ExecStart` word.
///
/// systemd splits `ExecStart` on whitespace and honours quotes, so a path containing a space or
/// a quote has to be quoted or it becomes a command plus arguments. Inside double quotes only
/// `"` and `\` need escaping. Paths that need none of this are left bare, which is every
/// installed build and most checkouts.
fn exec_start_word(exe: &Path) -> String {
    let raw = exe.display().to_string();
    let needs_quoting = raw
        .chars()
        .any(|c| c.is_whitespace() || c == '"' || c == '\'' || c == '\\');

    if needs_quoting {
        format!("\"{}\"", raw.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        raw
    }
}

/// `$XDG_CONFIG_HOME/systemd/user/zyris.service`, falling back to `~/.config` — the same rule
/// the user manager itself uses to decide where to look.
fn unit_path() -> anyhow::Result<PathBuf> {
    let dirs = directories::BaseDirs::new()
        .context("could not work out this user's configuration directory")?;

    Ok(dirs.config_dir().join("systemd").join("user").join(UNIT))
}

/// `None` when there is a user manager to talk to, and otherwise why there is not.
///
/// A machine with no systemd is not a fault to raise. It is a machine this switch cannot drive,
/// which is one of the three things [`State`] can say.
fn no_user_manager() -> Option<String> {
    // Any question that needs the session bus would do; this one needs no unit to exist and
    // changes nothing.
    match run("systemctl", &["--user", "show", "--property=Version"]) {
        Ok(output) if output.status.success() => None,
        Ok(_) => Some(
            "there is no systemd user session here for Zyris to start from".to_owned(),
        ),
        Err(error) if error.kind() == ErrorKind::NotFound => Some(NO_SYSTEMCTL.to_owned()),
        Err(error) => Some(format!("could not ask systemd anything: {error}")),
    }
}

/// Run `systemctl` and fail loudly, quoting what it said.
fn systemctl(args: &[&str]) -> anyhow::Result<()> {
    let output = run("systemctl", args)
        .with_context(|| format!("could not run `systemctl {}`", args.join(" ")))?;

    if output.status.success() {
        return Ok(());
    }

    anyhow::bail!("`systemctl {}` failed: {}", args.join(" "), complaint(&output));
}

/// Capture a command's output rather than letting it onto the terminal.
///
/// `Command::output` also gives the child no stdin, which is what keeps a polkit agent from
/// stopping a headless run at a password prompt nobody will answer.
fn run(program: &str, args: &[&str]) -> std::io::Result<Output> {
    Command::new(program).args(args).output()
}

/// What a failed command said, for an error message a person reads.
///
/// Quoted, never parsed — some of this is translated on a localized machine, so no decision may
/// turn on its wording.
fn complaint(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();

    if stderr.is_empty() {
        match output.status.code() {
            Some(code) => format!("it exited {code} and said nothing"),
            None => "it was killed by a signal".to_owned(),
        }
    } else {
        stderr.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_unit_starts_a_zyris_somebody_can_still_open() {
        // `--minimized`, never `--headless`. A headless process takes the instance lock and
        // has no tray and no single-instance plugin, so on a machine with autostart on there
        // would be no window, no tray icon, and no way to get either: launching Zyris would
        // find the lock held and exit without a word. `--minimized` is a full GUI that happens
        // not to be on the screen, and a second launch reaches it.
        let unit = render_unit(Path::new("/usr/bin/zyris"));

        assert!(unit.contains("ExecStart=/usr/bin/zyris --minimized"));
        assert!(!unit.contains("--headless"), "{unit}");
    }

    #[test]
    fn the_unit_waits_before_retrying() {
        // A node that redials a network service 100ms after boot is a node that fails to dial.
        let unit = render_unit(Path::new("/usr/bin/zyris"));

        assert!(unit.contains("Restart=always"));
        assert!(unit.contains("RestartSec=30"));
    }

    #[test]
    fn the_unit_waits_for_the_network() {
        let unit = render_unit(Path::new("/usr/bin/zyris"));

        // One `After=` names both targets — systemd takes a space-separated list there, and two
        // lines would read as the second replacing the first even though it does not.
        assert!(
            unit.contains("After=graphical-session.target network-online.target"),
            "{unit}"
        );
        assert!(unit.contains("Wants=network-online.target"));
    }

    #[test]
    fn the_unit_hangs_off_the_graphical_session() {
        // The unit starts a GUI, so it needs a session with a `DISPLAY` or a `WAYLAND_DISPLAY`
        // to start into, and `graphical-session.target` is the documented place to hang one.
        // `default.target` — where this used to go — is reached before any of that exists.
        let unit = render_unit(Path::new("/usr/bin/zyris"));

        assert!(unit.contains("WantedBy=graphical-session.target"), "{unit}");
        assert!(!unit.contains("WantedBy=default.target"), "{unit}");
        assert!(unit.contains("After=graphical-session.target"), "{unit}");
        // Stopped with the session rather than left behind by it: a GUI process outliving the
        // display server it was started into is a process with nothing to draw on.
        assert!(unit.contains("PartOf=graphical-session.target"), "{unit}");
    }

    #[test]
    fn the_unit_does_not_ask_for_lingering() {
        // Lingering exists to run a unit with no session. This one needs a session, so turning
        // it on would change a user-wide setting for no benefit at all.
        assert!(!render_unit(Path::new("/usr/bin/zyris")).contains("linger"));
    }

    #[test]
    fn a_path_with_a_space_stays_one_word() {
        // `ExecStart` splits on whitespace, so an unquoted `/home/r/My Apps/zyris` is a command
        // called `/home/r/My` that does not exist. A checkout under such a directory is the
        // normal way to meet this, and the unit fails at logon where nobody is watching.
        let unit = render_unit(Path::new("/home/r/My Apps/zyris"));

        assert!(
            unit.contains(r#"ExecStart="/home/r/My Apps/zyris" --minimized"#),
            "the path was not quoted: {unit}"
        );
    }

    #[test]
    fn the_mechanism_names_the_file_somebody_would_go_looking_for() {
        // The Settings screen prints this sentence and nothing else about how autostart works.
        // A person who wants to remove it by hand has to be able to find the thing from it.
        let mechanism = SystemdUser.mechanism().unwrap();

        assert!(mechanism.contains(UNIT), "{mechanism}");
    }

    #[test]
    fn an_enabled_unit_says_what_it_does_not_cover() {
        // A person who turned this on to keep a machine connected while they are away has to
        // learn here that it does not, rather than from its absence.
        let caveats = caveats_for(&Ok(State::Enabled));

        assert_eq!(caveats.len(), 1, "{caveats:?}");
        assert!(caveats[0].contains("log in"), "{}", caveats[0]);
        assert!(
            !caveats[0].contains("linger"),
            "dead copy: nothing enables lingering any more: {}",
            caveats[0],
        );
    }

    #[test]
    fn a_switch_that_is_off_has_nothing_to_qualify() {
        assert!(caveats_for(&Ok(State::Disabled)).is_empty());
        assert!(caveats_for(&Ok(State::Unsupported("no systemd".into()))).is_empty());
        assert!(caveats_for(&Err(anyhow::anyhow!("could not ask"))).is_empty());
    }

    #[test]
    fn the_unit_goes_where_systemd_looks_for_it() {
        let path = unit_path().unwrap();

        assert!(
            path.ends_with("systemd/user/zyris.service"),
            "a user manager will never read {}",
            path.display()
        );
    }

    /// The whole round trip against this machine's real systemd.
    ///
    /// Ignored because it writes into the person's own `~/.config/systemd/user` and enables a
    /// unit there. It puts both back. Nothing here touches lingering any more — neither
    /// [`SystemdUser::enable`] nor this test — so the machine is left exactly as it was found:
    ///
    /// ```text
    /// cargo test -p zyris-autostart -- --ignored --exact linux::tests::the_round_trip
    /// ```
    #[test]
    #[ignore = "writes a unit into this user's real systemd and enables it"]
    fn the_round_trip() {
        let backend = SystemdUser;
        let path = unit_path().unwrap();

        assert!(
            !path.exists(),
            "{} is already there; this test will not write over it",
            path.display()
        );
        assert_eq!(backend.state().unwrap(), State::Disabled);

        backend.enable(Path::new("/usr/bin/zyris")).unwrap();

        assert!(path.exists(), "{} was not written", path.display());
        assert_eq!(backend.state().unwrap(), State::Enabled);
        let installed = std::fs::read_to_string(&path).unwrap();
        assert!(
            installed.contains("--minimized"),
            "the installed unit does not start a Zyris anybody can open: {installed}",
        );
        assert!(
            installed.contains("WantedBy=graphical-session.target"),
            "the installed unit would be started before there is a display server: {installed}",
        );

        backend.disable().unwrap();

        assert!(!path.exists(), "{} outlived disable", path.display());
        assert_eq!(backend.state().unwrap(), State::Disabled);
    }
}
