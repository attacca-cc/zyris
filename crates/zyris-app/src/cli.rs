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
}

/// Which of the two runtimes this process is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Gui,
    Headless,
}

impl Cli {
    pub fn mode(&self) -> Mode {
        if self.headless { Mode::Headless } else { Mode::Gui }
    }

    pub fn server(&self) -> Option<&str> {
        self.server.as_deref()
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
}
