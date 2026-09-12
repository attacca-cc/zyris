//! What the command line says this process should be.

use clap::Parser;

/// Zyris: keeps this computer connected to Attacca.
#[derive(Parser, Debug)]
#[command(name = "zyris", version, about)]
pub struct Cli {
    /// Run with no window and no tray. The node still connects and still speaks.
    #[arg(long)]
    pub headless: bool,

    /// Start with the window hidden, leaving the tray icon as the way in. This is what
    /// autostart installs: a window that opened by itself at every sign-in is a window nobody
    /// asked for, and a process with no tray at all is one nobody can reach.
    ///
    /// Refused beside `--headless`, which has no window to hide.
    #[arg(long, conflicts_with = "headless")]
    pub minimized: bool,

    /// Dial somewhere other than Attacca. For a local server during development; leave it unset
    /// and the node uses the address the protocol crate ships.
    #[arg(long, value_name = "URL")]
    pub server: Option<String>,

    /// Start Zyris whenever you sign in to this computer, then exit. It installs a systemd
    /// user unit on Linux and a Task Scheduler entry on Windows, for the copy of Zyris you
    /// ran this with.
    #[arg(long, conflicts_with = "uninstall_autostart")]
    pub install_autostart: bool,

    /// Stop starting Zyris when you sign in, then exit. It leaves a Zyris that is already
    /// running exactly where it is.
    #[arg(long)]
    pub uninstall_autostart: bool,
}

/// What this process is, said once so nothing downstream has to work it out again.
///
/// Three states rather than two booleans standing beside each other. `--headless --minimized`
/// is a pair with no meaning, and a pair that can be nonsense is a pair somebody resolves at
/// runtime — in two places, differently. Clap refuses that combination at parse time, and what
/// reaches the rest of the program is one of these three and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A window, on the screen, now.
    Window,
    /// The same window, built but not shown. The tray icon is the only thing on the screen
    /// until somebody asks for more.
    WindowHidden,
    /// No window and no tray at all.
    Headless,
}

impl Mode {
    /// Whether the window should be on the screen as soon as it exists.
    ///
    /// `tauri.conf.json` declares the main window `"visible": false`, and `gui.rs` shows it from
    /// `setup` when this says so. The other way round — declaring it visible and hiding it in
    /// `setup` — cannot work: Tauri builds a config-declared window *before* the setup closure
    /// runs, so the window would appear and then vanish at every sign-in.
    pub fn shows_a_window(self) -> bool {
        matches!(self, Mode::Window)
    }
}

/// What the autostart flags asked for, when one of them was given.
///
/// Neither starts a node: `main` acts on this and returns, before the instance lock is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutostartRequest {
    Install,
    Uninstall,
}

impl Cli {
    pub fn mode(&self) -> Mode {
        if self.headless {
            Mode::Headless
        } else if self.minimized {
            Mode::WindowHidden
        } else {
            Mode::Window
        }
    }

    pub fn server(&self) -> Option<&str> {
        self.server.as_deref()
    }

    /// Whether this run is a request to change autostart rather than to be a node.
    ///
    /// The two flags cannot both be given — clap refuses that and names them — so there is no
    /// order to settle here.
    pub fn autostart(&self) -> Option<AutostartRequest> {
        if self.install_autostart {
            Some(AutostartRequest::Install)
        } else if self.uninstall_autostart {
            Some(AutostartRequest::Uninstall)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn no_arguments_means_the_window() {
        let cli = Cli::parse_from(["zyris"]);

        assert_eq!(cli.mode(), Mode::Window);
        assert!(cli.mode().shows_a_window());
    }

    #[test]
    fn headless_flag_means_no_window() {
        let cli = Cli::parse_from(["zyris", "--headless"]);

        assert_eq!(cli.mode(), Mode::Headless);
    }

    #[test]
    fn the_minimized_flag_builds_the_window_without_showing_it() {
        // What autostart installs. The process is a full GUI — it has a tray, and it runs the
        // single-instance plugin, so launching Zyris again reaches it and gets a window. That
        // is the whole difference from `--headless`, which a second launch cannot reach.
        let cli = Cli::parse_from(["zyris", "--minimized"]);

        assert_eq!(cli.mode(), Mode::WindowHidden);
        assert!(!cli.mode().shows_a_window());
    }

    #[test]
    fn a_hidden_window_is_still_not_headless() {
        // The distinction the bug turned on: `--headless` has no tray and nothing listening,
        // so a machine that starts one is a machine nobody can open Zyris on.
        assert_ne!(
            Cli::parse_from(["zyris", "--minimized"]).mode(),
            Cli::parse_from(["zyris", "--headless"]).mode(),
        );
    }

    #[test]
    fn asking_for_no_window_and_a_hidden_one_at_once_is_refused() {
        // There is no window to hide in a headless run. Refused by clap, which names both
        // flags, rather than resolved here — two booleans that can both be true is a state
        // somebody has to invent an answer for, and two callers invent two answers.
        let refused = Cli::try_parse_from(["zyris", "--headless", "--minimized"]);

        assert!(refused.is_err());
    }

    #[test]
    fn the_server_flag_and_a_hidden_window_compose() {
        let cli = Cli::parse_from(["zyris", "--minimized", "--server", "ws://localhost:1/ws"]);

        assert_eq!(cli.mode(), Mode::WindowHidden);
        assert_eq!(cli.server(), Some("ws://localhost:1/ws"));
    }

    #[test]
    fn no_server_flag_means_the_default() {
        let cli = Cli::parse_from(["zyris"]);

        assert_eq!(cli.server(), None);
    }

    #[test]
    fn the_server_flag_is_read_back() {
        let cli = Cli::parse_from(["zyris", "--server", "ws://127.0.0.1:8080/zyris/v1/ws"]);

        assert_eq!(cli.server(), Some("ws://127.0.0.1:8080/zyris/v1/ws"));
    }

    #[test]
    fn the_server_flag_and_headless_compose() {
        let cli = Cli::parse_from(["zyris", "--headless", "--server", "ws://localhost:1/ws"]);

        assert_eq!(cli.mode(), Mode::Headless);
        assert_eq!(cli.server(), Some("ws://localhost:1/ws"));
    }

    #[test]
    fn an_ordinary_run_changes_nothing_about_autostart() {
        // Launching Zyris must never install anything. Turning it on is something a person
        // asks for, once, either here or on the Settings screen.
        assert_eq!(Cli::parse_from(["zyris"]).autostart(), None);
    }

    #[test]
    fn the_install_flag_is_read_back() {
        let cli = Cli::parse_from(["zyris", "--install-autostart"]);

        assert_eq!(cli.autostart(), Some(AutostartRequest::Install));
    }

    #[test]
    fn the_uninstall_flag_is_read_back() {
        let cli = Cli::parse_from(["zyris", "--uninstall-autostart"]);

        assert_eq!(cli.autostart(), Some(AutostartRequest::Uninstall));
    }

    #[test]
    fn asking_to_install_and_uninstall_at_once_is_refused() {
        // Rather than picking one by the order they happen to be written in. clap names both
        // flags in the refusal, which is the whole of what the person needs to know.
        let refused = Cli::try_parse_from(["zyris", "--install-autostart", "--uninstall-autostart"]);

        assert!(refused.is_err());
    }

    #[test]
    fn an_autostart_flag_is_honoured_whichever_runtime_was_named() {
        // `--headless --install-autostart` installs and exits rather than starting a node.
        // `main` reads this before it takes the instance lock, so it also works while a Zyris
        // is already running on this machine — being refused by your own running copy is not
        // a reasonable answer to "install autostart".
        let cli = Cli::parse_from(["zyris", "--headless", "--install-autostart"]);

        assert_eq!(cli.autostart(), Some(AutostartRequest::Install));
    }
}
