//! Autostart on Linux: a systemd user unit.
//!
//! A unit file at `~/.config/systemd/user/zyris.service`, enabled into `default.target.wants`. A
//! `.desktop` file under `~/.config/autostart` — what `tauri-plugin-autostart` writes — would
//! have been a quarter of this code, and it has no delayed start and no retry.
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
        //
        // It is not, however, the whole story: `is-enabled` says a symlink exists, not that
        // anything will ever follow it. What turns that into an honest answer on the screen is
        // [`SystemdUser::caveats`], which goes and asks whether the unit is actually running —
        // the same thing `windows.rs` reads the task document back for.
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

        // **No `systemctl --user start` here, deliberately.** Whoever is turning this on is
        // running Zyris already — that is where the switch is — and a second `--minimized`
        // launch reaches `tauri_plugin_single_instance`, which focuses the first window and
        // exits 0 — and `Restart=on-failure` above leaves that alone, so nothing loops. The
        // unit is installed and left alone, and it runs at the next sign-in. `caveats` says so
        // rather than leaving a switch reading "on" over a unit that is not up.
        //
        // **No `loginctl enable-linger` either.** Lingering exists so a user's units can run
        // with no session of their own, and this unit cannot: it starts a GUI, which needs the
        // `DISPLAY` or `WAYLAND_DISPLAY` only a graphical session has. Turning lingering on
        // would change a user-wide setting — one that keeps every other unit they own running
        // after logout too — and buy this one nothing. An earlier version of this file did it,
        // back when the unit started `--headless`.
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
        //
        // **One call covers every wants directory, including one an older Zyris wrote.** This
        // unit was installed into `graphical-session.target.wants` before it was installed into
        // `default.target.wants`, and `disable` removes symlinks to the unit from all of them —
        // measured on systemd 260 by installing both and disabling once. There is nothing here
        // for an upgrade path to sweep up.
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

    fn caveats(&self, state: &State) -> Vec<String> {
        // Asked only of a switch that is on. A unit nobody installed has no liveness worth
        // reporting, and this is the one place that spends two more subprocesses.
        if !matches!(state, State::Enabled) {
            return Vec::new();
        }

        caveats_for(state, liveness())
    }
}

/// What is true of an enabled unit that "on" does not say by itself.
///
/// Split out from [`SystemdUser::caveats`] so the sentences can be checked without a systemd to
/// ask; the method is only the part that has to go and look.
fn caveats_for(state: &State, liveness: Liveness) -> Vec<String> {
    // Only worth saying while the switch is on. What a login-time unit costs a machine that
    // does not start Zyris at all is nothing.
    if !matches!(state, State::Enabled) {
        return Vec::new();
    }

    let mut caveats = vec![NOT_AT_BOOT.to_owned()];

    // **The invariant `windows.rs` calls the one failure this crate exists to avoid.** There,
    // a task can be registered and switched off, so `state` reads the document back. Here, a
    // unit can be enabled and not up — because nothing has started it yet, or because it
    // started and failed — and `is-enabled` exits 0 for all of that. A person whose Zyris is
    // not running must not read "On." with nothing else on the screen.
    match liveness {
        Liveness::Running => {}
        Liveness::Failed => caveats.push(FAILED.to_owned()),
        Liveness::Idle => caveats.push(NOT_RUNNING.to_owned()),
    }

    caveats
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

/// Said when the unit is installed and nothing is up from it.
///
/// Two situations at once, because systemd does not separate them cheaply — see [`liveness`].
/// One is every moment between turning the switch on and the next sign-in, which is expected
/// and has to explain itself rather than alarm. The other is a unit that started, failed, and is
/// waiting out `RestartSec` to try again, which is not expected at all. The sentence covers both
/// and hands the second one somewhere to go.
const NOT_RUNNING: &str = "This is on, but nothing is running from it right now. Before your \
                           next sign-in that is expected: turning the switch on does not start \
                           Zyris, signing in does. After one, `systemctl --user status \
                           zyris.service` says what happened instead.";

/// Said when the unit ran and gave up. The one caveat that is a fault rather than a limit, and
/// the only sentence here that sends somebody to a command.
const FAILED: &str = "This is on, but Zyris failed the last time systemd started it, so it is \
                      not running now. `systemctl --user status zyris.service` says why.";

/// Whether an enabled unit is actually up, as the user manager sees it this second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Liveness {
    /// `is-active` exits 0: there is a Zyris running from this unit.
    Running,
    /// `is-failed` exits 0: it ran and gave up.
    Failed,
    /// Neither. Never started this session, stopped by hand, or between two restarts.
    Idle,
}

/// Ask systemd whether the unit is up, in the two questions whose exit codes mean one thing
/// each.
///
/// `is-failed` first: a failed unit is also not active, and "it failed" is the more useful half
/// of that pair. Neither word is read — `is-active` prints `active`, `inactive`, `activating`,
/// `deactivating`, `failed`, and which of those count is a list that grows, exactly as it is for
/// `is-enabled` above.
///
/// **A unit that is looping answers neither, and that is measured, not assumed.** Started,
/// failed 203/EXEC, waiting out `RestartSec`: systemd calls that `ActiveState=activating`,
/// `SubState=auto-restart`, and both `is-active` and `is-failed` print `activating` and exit
/// non-zero for it (systemd 260). `Restart=on-failure` with `RestartSec=30` means the unit never
/// reaches the start limit and so never latches into `failed`, which is what the ordinary
/// desktop case needs — so this is the state a rotted `ExecStart` actually sits in, not
/// [`Liveness::Failed`]. It lands in [`Liveness::Idle`], and [`NOT_RUNNING`] is written to be
/// true of it as well as of a unit nobody has started yet.
///
/// Telling the two apart would mean reading `SubState`, and every word systemd prints is a word
/// this file has decided not to turn a decision on. The sentence covers both instead.
fn liveness() -> Liveness {
    if exits_zero(&["--user", "is-failed", UNIT]) {
        return Liveness::Failed;
    }

    if exits_zero(&["--user", "is-active", UNIT]) {
        return Liveness::Running;
    }

    Liveness::Idle
}

/// Whether `systemctl` exited 0, with every reason it might not have collapsed into `false`.
///
/// Right for both callers: they ask a yes/no question whose "no" covers "the unit is not in that
/// state" and "there was nobody to ask", and both of those end in the same caveat.
fn exits_zero(args: &[&str]) -> bool {
    run("systemctl", args).is_ok_and(|output| output.status.success())
}

/// The unit file, as a string.
///
/// Pure, so what goes into it can be checked without a systemd to check it against.
///
/// **`WantedBy=default.target`, not `graphical-session.target`.** That target is only ever
/// activated by a session manager that explicitly binds it — GNOME and KDE do; a plain Wayland
/// compositor, i3, sway and Hyprland without uwsm, and a bare `startx` do not. Measured in a
/// live Hyprland session on systemd 260: `is-active graphical-session.target` is `inactive` and
/// nothing in the user manager wants it, while `default.target` is active. A unit installed into
/// a target nobody activates is enabled and never pulled in: `is-enabled` exits 0, the switch
/// reads "On", and Zyris never starts — not at login, not at boot, never. `default.target` is
/// reached by every user manager there is.
///
/// `After=` and `PartOf=` stay pointed at `graphical-session.target` anyway, and cost nothing
/// where it does not exist: `After=` orders against a unit only when both are in the same job
/// transaction, and `PartOf=` propagates stop and restart only. Where the target *is* activated
/// they are still worth having, and the second is what stops Zyris at the end of a session
/// rather than leaving a GUI process behind with nothing to draw on.
///
/// **What that costs, and why it is the cheaper side.** On a login with no display at all — an
/// SSH session on a machine with lingering on, say — `default.target` is reached, this unit
/// starts, Tauri fails to open a display, and `Restart=on-failure` tries again every thirty
/// seconds for as long as that session lasts. The retry is also what makes the ordinary case
/// work: on a desktop login the unit can be started before the compositor is up, and this is
/// what carries it over until it is.
///
/// **`on-failure` rather than `always`, and the difference is the tray's Quit item.** That item
/// is `app.exit(0)`; under `always` systemd would put Zyris back thirty seconds later, every
/// time, and a person would have no way to stop it short of turning this switch off. A failure
/// to reach a display is a non-zero exit and is still retried, so nothing above is lost — and
/// the second `--minimized` launch that `enable` describes, which exits 0 through the
/// single-instance plugin, stops being a restart loop too.
///
/// `StartLimitBurst` and `StartLimitIntervalSec` are deliberately left at their defaults, which
/// on this machine are 5 starts in 10 seconds (`DefaultStartLimitBurst=5`,
/// `DefaultStartLimitIntervalUSec=10s`, read off systemd 260). `RestartSec=30` puts at most one
/// start in any 10-second window, so the limit is never reached and the loop never latches into
/// `failed`. That is the behaviour wanted in both directions: the desktop case has to keep
/// retrying until the compositor appears, and the displayless case costs one process spawn every
/// thirty seconds, which is cheaper than a unit that gives up before a slow session manager is
/// ready.
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
         Restart=on-failure\n\
         RestartSec=30\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        exe = exec_start_word(exe),
    )
}

/// The executable path as one `ExecStart` word.
///
/// Two separate hazards, and only one of them is about quoting.
///
/// systemd splits `ExecStart` on whitespace and honours quotes, so a path containing a space or
/// a quote has to be quoted or it becomes a command plus arguments. Inside double quotes only
/// `"` and `\` need escaping.
///
/// **`%` is the other one, and quoting does not help.** systemd expands specifiers before it
/// splits words, so a `%` in a path is read as the start of one wherever it appears. Measured on
/// systemd 260: `ExecStart=/home/r/100%off/zyris` and `ExecStart="/home/r/100%off/zyris"` both
/// resolve to `/home/r/100nixosff/zyris`, because `%o` is the OS ID. `%%` is the escape, and it
/// works bare as well as quoted — which is why the replace below happens before the branch
/// rather than inside it, and why it, not the quotes, is what keeps such a path intact. The unit
/// fails 203/EXEC at every login otherwise, and the `info!` line `bridge.rs` prints to explain a
/// rotted autostart path logs the path *before* expansion, so the one diagnostic aimed at this
/// prints something that looks right.
///
/// Paths that need none of this are left bare, which is every installed build and most
/// checkouts.
fn exec_start_word(exe: &Path) -> String {
    let raw = exe.display().to_string();
    let needs_quoting = raw
        .chars()
        .any(|c| c.is_whitespace() || c == '"' || c == '\'' || c == '\\' || c == '%');

    // First, and on both branches: quoting does not suppress specifier expansion, so this is
    // the half that does the work. Before the escapes below because neither of those introduces
    // a `%` this would then double a second time.
    let escaped = raw.replace('%', "%%");

    if needs_quoting {
        format!("\"{}\"", escaped.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        escaped
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

        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("RestartSec=30"));
    }

    #[test]
    fn quitting_from_the_tray_is_not_something_to_undo() {
        // `Restart=always` would make Quit useless: the tray's item is `app.exit(0)`, and
        // systemd would put Zyris back thirty seconds later, every time, with no way for a
        // person to stop it short of turning the whole switch off.
        let unit = render_unit(Path::new("/usr/bin/zyris"));

        assert!(!unit.contains("Restart=always"), "a clean quit must stay quit");
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
    fn the_unit_is_installed_into_a_target_that_is_always_active() {
        // The regression this test exists for: `WantedBy=graphical-session.target` is enabled
        // successfully into a target that GNOME and KDE activate and nothing else does —
        // measured `inactive` in a live Hyprland session on systemd 260, with nothing in the
        // user manager wanting it. The symlink is written, `is-enabled` exits 0, and Zyris
        // never starts. `default.target` is reached by every user manager there is.
        let unit = render_unit(Path::new("/usr/bin/zyris"));

        assert!(unit.contains("WantedBy=default.target"), "{unit}");
        assert!(!unit.contains("WantedBy=graphical-session.target"), "{unit}");
        // Ordering and teardown still point at the graphical session, which costs nothing where
        // that target is never activated and is worth having where it is: `After=` so a desktop
        // login starts Zyris after the compositor when both are in one transaction, and
        // `PartOf=` so a GUI process is not left behind by the display server it drew on.
        assert!(unit.contains("After=graphical-session.target"), "{unit}");
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
    fn a_path_with_a_percent_sign_is_not_read_as_a_specifier() {
        // Measured on systemd 260: `/home/r/100%off/zyris` resolves to
        // `/home/r/100nixosff/zyris` — `%o` is the OS ID — and it does so inside double quotes
        // too, because specifiers are expanded before words are split. `%%` is the only escape,
        // and it is what has to survive here; the quotes are incidental.
        let unit = render_unit(Path::new("/home/r/100%off/zyris"));

        assert!(unit.contains("100%%off"), "the percent was not escaped: {unit}");
        assert!(
            !unit.contains("100%off"),
            "a bare percent survived, and systemd will expand it: {unit}",
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
        let caveats = caveats_for(&State::Enabled, Liveness::Running);

        assert_eq!(caveats.len(), 1, "{caveats:?}");
        assert!(caveats[0].contains("log in"), "{}", caveats[0]);
        assert!(
            !caveats[0].contains("linger"),
            "dead copy: nothing enables lingering any more: {}",
            caveats[0],
        );
    }

    #[test]
    fn an_enabled_unit_that_is_not_up_does_not_read_as_simply_on() {
        // The Linux half of the invariant `windows.rs` enforces by reading the task document
        // back. `is-enabled` exits 0 for a unit that has never run, so without this a person
        // whose Zyris is not running reads "On." and nothing else.
        let idle = caveats_for(&State::Enabled, Liveness::Idle);
        let failed = caveats_for(&State::Enabled, Liveness::Failed);

        assert_eq!(idle.len(), 2, "{idle:?}");
        assert!(idle[1].contains("nothing is running"), "{}", idle[1]);

        assert_eq!(failed.len(), 2, "{failed:?}");
        assert!(failed[1].contains("failed"), "{}", failed[1]);
        // The one caveat that is a fault rather than a limit, so it is the one that names a
        // command to run.
        assert!(failed[1].contains("systemctl --user status"), "{}", failed[1]);
    }

    #[test]
    fn a_switch_that_is_off_has_nothing_to_qualify() {
        // Every liveness, because a disabled unit can still be `failed` from earlier in the
        // session, and none of that is worth saying about a switch that is off.
        for liveness in [Liveness::Running, Liveness::Idle, Liveness::Failed] {
            assert!(caveats_for(&State::Disabled, liveness).is_empty());
            assert!(
                caveats_for(&State::Unsupported("no systemd".into()), liveness).is_empty()
            );
        }
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
    /// cargo test -p zyris-autostart -- --ignored --nocapture --exact linux::tests::the_round_trip
    /// ```
    ///
    /// Everything above this test checks a string. This is the only thing that can tell us the
    /// unit is actually *reached*, which is the one failure the string tests cannot see: the
    /// previous version of this test asserted `WantedBy=graphical-session.target` was in the
    /// installed file, which is the bug spelled out as an assertion. Thirteen tests passed
    /// green over a feature that never started Zyris on this machine once.
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

        // **Everything is read before anything is asserted, and `disable` runs in between.**
        // A failing assertion is a panic, and a panic between `enable` and `disable` would
        // leave a real unit enabled in somebody's own systemd — which is the one thing the
        // doc comment above promises this test does not do.
        let written = path.exists();
        let state_while_installed = backend.state().unwrap();
        let installed = std::fs::read_to_string(&path).unwrap_or_default();
        // `WantedBy` read back off the symlinks `enable` wrote, rather than off the file it
        // wrote them from, plus which of those targets this machine actually activates.
        let installed_into = wanted_by();
        let live: Vec<String> = installed_into
            .iter()
            .filter(|target| is_active(target))
            .cloned()
            .collect();
        println!("--- {UNIT} is wanted by {installed_into:?}, of which {live:?} are active ---");

        backend.disable().unwrap();

        assert!(written, "{} was not written", path.display());
        assert_eq!(state_while_installed, State::Enabled);
        assert!(
            installed.contains("--minimized"),
            "the installed unit does not start a Zyris anybody can open: {installed}",
        );

        // **The assertion this test is for.** Not what the file says — what systemd did with
        // it. At least one of the targets holding a symlink has to be a target this machine
        // actually activates, or the unit is enabled and unreachable.
        assert!(
            !installed_into.is_empty(),
            "nothing wants {UNIT}; it is enabled and will never be started",
        );
        assert!(
            !live.is_empty(),
            "{UNIT} is only wanted by {installed_into:?}, and none of those is active on this \
             machine: `systemctl --user is-enabled` exits 0 and Zyris never starts",
        );

        assert!(!path.exists(), "{} outlived disable", path.display());
        assert_eq!(backend.state().unwrap(), State::Disabled);
        assert!(
            wanted_by().is_empty(),
            "a wants symlink outlived disable: {:?}",
            wanted_by(),
        );

        // Turning off what is already off is the job already done, not a failure.
        backend.disable().unwrap();
    }

    /// The targets whose `.wants` directory currently holds a symlink to the unit, as systemd
    /// computes it — rather than as this crate guesses from the file it wrote.
    fn wanted_by() -> Vec<String> {
        let output =
            run("systemctl", &["--user", "show", "--property=WantedBy", "--value", UNIT])
                .unwrap();

        String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .map(str::to_owned)
            .collect()
    }

    /// Whether a unit — here always a target — is active in this user manager right now.
    fn is_active(unit: &str) -> bool {
        run("systemctl", &["--user", "is-active", unit])
            .unwrap()
            .status
            .success()
    }
}
