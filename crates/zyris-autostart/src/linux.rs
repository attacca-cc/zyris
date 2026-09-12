//! Autostart on Linux: a systemd user unit.
//!
//! A unit file at `~/.config/systemd/user/zyris.service`, enabled into `default.target.wants`,
//! with lingering turned on so it outlives the login session. A `.desktop` file under
//! `~/.config/autostart` — what `tauri-plugin-autostart` writes — would have been a quarter of
//! this code, and it has no delayed start, no retry, and no life after logout.
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

        // Lingering is what makes this a daemon rather than a login program: without it systemd
        // stops the unit when the person logs out, and the machine they left switched on stops
        // answering. It needs no root where polkit allows it, and polkit policy varies by
        // distribution — so a failure here is **reported, never swallowed**, and it does not
        // undo the unit. Autostart really is on; it just will not outlive the session, which is
        // a smaller thing than "enabling autostart failed" and a different thing to say.
        // [`Backend::caveats`] repeats it every time the state is read, for the window; this
        // line is how a `--headless` run hears about it.
        if let Err(error) = enable_linger() {
            tracing::warn!(
                %error,
                "Zyris will start when you sign in, but it will stop when you log out",
            );
        }

        Ok(())
    }

    fn disable(&self) -> anyhow::Result<()> {
        if let Some(reason) = no_user_manager() {
            anyhow::bail!("cannot turn autostart off: {reason}");
        }

        let path = unit_path()?;
        let installed = path.exists();

        // `systemctl --user disable` is what removes the symlink under `default.target.wants`,
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

        // Lingering is deliberately left on. It is a property of the user rather than of Zyris
        // — another of their services may be relying on it — and nothing here can tell whether
        // this switch is what turned it on, because `loginctl` does not say who asked. Turning
        // off something we cannot prove we caused is the worse mistake of the two.
        Ok(())
    }

    fn caveats(&self) -> Vec<String> {
        // Only worth saying while the switch is on. What lingering costs a machine that does
        // not start Zyris at all is nothing.
        if !matches!(self.state(), Ok(State::Enabled)) {
            return Vec::new();
        }

        match linger_enabled() {
            Ok(true) => Vec::new(),
            Ok(false) => vec![
                "Zyris will start when you sign in, but it will stop when you log out: this \
                 user does not linger. `loginctl enable-linger` turns that on."
                    .to_owned(),
            ],
            Err(error) => {
                tracing::debug!(%error, "could not read whether this user lingers");
                Vec::new()
            }
        }
    }
}

/// The unit file, as a string.
///
/// Pure, so what goes into it can be checked without a systemd to check it against.
fn render_unit(exe: &Path) -> String {
    format!(
        "[Unit]\n\
         Description=Zyris — keeps this computer connected to Attacca\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={exe} --headless\n\
         Restart=always\n\
         RestartSec=30\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
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

/// Turn lingering on for this user, unless it already is.
///
/// Asking first keeps a machine that would put up a polkit prompt from putting one up for
/// nothing.
fn enable_linger() -> anyhow::Result<()> {
    if linger_enabled()? {
        return Ok(());
    }

    let user = this_user()?;
    let output = run("loginctl", &["enable-linger", &user])
        .context("could not run `loginctl enable-linger`")?;

    if !output.status.success() {
        anyhow::bail!("could not enable lingering for {user}: {}", complaint(&output));
    }

    tracing::info!(%user, "enabled lingering so Zyris survives logout");
    Ok(())
}

/// Whether this user's services survive them logging out.
fn linger_enabled() -> anyhow::Result<bool> {
    let user = this_user()?;
    let output = run("loginctl", &["show-user", &user, "--value", "--property=Linger"])
        .context("could not run `loginctl show-user`")?;

    if !output.status.success() {
        anyhow::bail!("could not read whether {user} lingers: {}", complaint(&output));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim() == "yes")
}

/// Who to name to `loginctl`.
///
/// It has to be named. `loginctl show-user` with no user shows the *login manager's* properties
/// rather than this user's, and since the manager has no `Linger` property it answers with an
/// empty string and exit 0 — which reads exactly like "does not linger". Measured on systemd
/// 260.
fn this_user() -> anyhow::Result<String> {
    match std::env::var("USER") {
        Ok(user) if !user.is_empty() => return Ok(user),
        _ => {}
    }

    // `$USER` is not set in every context a desktop application is launched from. `loginctl`
    // takes a numeric uid in place of a name, and `id -u` prints one and nothing else.
    let output = run("id", &["-u"]).context("could not run `id -u` to find out who this is")?;
    if !output.status.success() {
        anyhow::bail!("could not find out who this is: {}", complaint(&output));
    }

    let uid = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if uid.is_empty() {
        anyhow::bail!("could not find out who this is: `id -u` said nothing");
    }

    Ok(uid)
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
    fn the_unit_starts_the_headless_binary_by_absolute_path() {
        let unit = render_unit(Path::new("/usr/bin/zyris"));

        assert!(unit.contains("ExecStart=/usr/bin/zyris --headless"));
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

        assert!(unit.contains("After=network-online.target"));
        assert!(unit.contains("Wants=network-online.target"));
    }

    #[test]
    fn the_unit_installs_where_a_user_session_will_find_it() {
        assert!(render_unit(Path::new("/usr/bin/zyris")).contains("WantedBy=default.target"));
    }

    #[test]
    fn a_path_with_a_space_stays_one_word() {
        // `ExecStart` splits on whitespace, so an unquoted `/home/r/My Apps/zyris` is a command
        // called `/home/r/My` that does not exist. A checkout under such a directory is the
        // normal way to meet this, and the unit fails at logon where nobody is watching.
        let unit = render_unit(Path::new("/home/r/My Apps/zyris"));

        assert!(
            unit.contains(r#"ExecStart="/home/r/My Apps/zyris" --headless"#),
            "the path was not quoted: {unit}"
        );
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
    /// Ignored because it writes into the person's own `~/.config/systemd/user` and turns
    /// lingering on for their account. Run it deliberately, and know that it leaves lingering
    /// on — [`SystemdUser::disable`] does not turn that off, and neither does this:
    ///
    /// ```text
    /// cargo test -p zyris-autostart -- --ignored --exact linux::tests::the_round_trip
    /// loginctl disable-linger "$USER"     # if it was off before, put it back
    /// ```
    #[test]
    #[ignore = "writes a unit into this user's real systemd and turns lingering on"]
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
        assert!(
            std::fs::read_to_string(&path).unwrap().contains("--headless"),
            "the installed unit does not start the headless binary",
        );

        backend.disable().unwrap();

        assert!(!path.exists(), "{} outlived disable", path.display());
        assert_eq!(backend.state().unwrap(), State::Disabled);
    }
}
