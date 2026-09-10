//! What the command line says this process should be.

use clap::Parser;

/// Zyris: keeps this computer connected to Attacca.
#[derive(Parser, Debug)]
#[command(name = "zyris", version, about)]
pub struct Cli {
    /// Run with no window and no tray. The node still connects and still speaks.
    #[arg(long)]
    pub headless: bool,
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
}
