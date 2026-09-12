//! What the command line says this process should be.

use clap::Parser;

/// Zyris: keeps this computer connected to Attacca.
#[derive(Parser, Debug)]
#[command(name = "zyris", version, about)]
pub struct Cli {
    /// Run with no window and no tray. The node still connects and still speaks.
    #[arg(long)]
    pub headless: bool,

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

/// Which of the two runtimes this process is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Gui,
    Headless,
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
        if self.headless { Mode::Headless } else { Mode::Gui }
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

        assert_eq!(cli.mode(), Mode::Gui);
    }

    #[test]
    fn headless_flag_means_no_window() {
        let cli = Cli::parse_from(["zyris", "--headless"]);

        assert_eq!(cli.mode(), Mode::Headless);
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
