//! What the command line says this process should be.
//!
//! **Two shapes, and the difference between them is whether a node runs.** With no subcommand
//! this process *is* the node — a window, the tray, or `--headless` — and everything below the
//! parse is about keeping it connected. With a subcommand it does one job, says what happened, and
//! exits without taking the instance lock, reading the keychain or starting a node: `zyris status`
//! has to work *while* a node is running, and `zyris down` has to work on one.
//!
//! The flags that say what kind of node to run (`--headless`, `--minimized`) and the two that
//! move the autostart switch are read by the node run and by `zyris up`, which starts one in the
//! background. With any other subcommand they are refused rather than ignored — see [`Cli::check`]
//! — because `zyris --headless status` reads like it means something and does not.

use clap::{Args, CommandFactory as _, Parser, Subcommand};

/// Zyris: keeps this computer connected to Attacca.
#[derive(Parser, Debug)]
#[command(name = "zyris", version, about)]
pub struct Cli {
    /// What to do with the window closed. Absent, this process is the node.
    #[command(subcommand)]
    command: Option<Command>,

    /// Run with no window and no tray. The node still connects and still speaks.
    ///
    /// **Read by `zyris` on its own — the node — and by `zyris up`, which starts one in the
    /// background.** Every other subcommand refuses it rather than ignoring it, so this shows in
    /// their help only because the flags are parsed beside any command, which is what lets
    /// `--server` be written on either side of the command.
    #[arg(long, global = true)]
    pub headless: bool,

    /// Start with the window hidden, leaving the tray icon as the way in. This is what
    /// autostart installs: a window that opened by itself at every sign-in is a window nobody
    /// asked for, and a process with no tray at all is one nobody can reach.
    ///
    /// Refused beside `--headless`, which has no window to hide. Read by the same two forms
    /// `--headless` is — `zyris` on its own, and `zyris up`.
    #[arg(long, global = true, conflicts_with = "headless")]
    pub minimized: bool,

    /// Dial somewhere other than Attacca. For a local server during development; leave it unset
    /// and the node uses the address the protocol crate ships.
    ///
    /// It names the instance as well as the address — the keychain service, the instance lock and
    /// the data directory all follow it — so a console command pointed at a development server
    /// reads and writes that run's settings and no others.
    #[arg(long, global = true, value_name = "URL")]
    pub server: Option<String>,

    /// Start Zyris whenever you sign in to this computer, then exit. It installs a systemd
    /// user unit on Linux and a Task Scheduler entry on Windows, for the copy of Zyris you
    /// ran this with.
    ///
    /// `zyris autostart enable` is the same switch.
    #[arg(long, conflicts_with = "uninstall_autostart")]
    pub install_autostart: bool,

    /// Stop starting Zyris when you sign in, then exit. It leaves a Zyris that is already
    /// running exactly where it is.
    ///
    /// `zyris autostart disable` is the same switch.
    #[arg(long)]
    pub uninstall_autostart: bool,
}

/// One job, done with the window closed.
///
/// Every one of these is a command a person types at a terminal on a machine whose window they
/// cannot reach — a server, an SSH session, a desktop where the tray is not answering. The order
/// they are written in is the order they are used in: authorize the machine, start it, look at it,
/// stop it.
#[derive(Subcommand, Debug, PartialEq)]
pub enum Command {
    /// Start a node in the background and return.
    ///
    /// The tray is the way into it on a desktop; `--headless` starts one with no window at all.
    /// With neither, it is `--minimized` on a machine with a desktop session and `--headless` on
    /// one without.
    Up,

    /// Stop the node this machine is running. It asks the node to stop and waits for it.
    Down,

    /// What this machine's node is doing, and when it last said so.
    Status,

    /// Authorize this machine: print the code to enter on Attacca, and wait for it.
    Login,

    /// Read and write the settings the window would otherwise be needed for.
    Config(ConfigArgs),

    /// Start Zyris when this computer signs in, or stop doing that.
    Autostart(AutostartArgs),

    /// The MCP servers this machine starts.
    Mcp(McpArgs),
}

impl Command {
    /// What this command is typed as, for the refusals in [`Cli::check`].
    pub fn label(&self) -> &'static str {
        match self {
            Command::Up => "up",
            Command::Down => "down",
            Command::Status => "status",
            Command::Login => "login",
            Command::Config(_) => "config",
            Command::Autostart(_) => "autostart",
            Command::Mcp(_) => "mcp",
        }
    }
}

#[derive(Args, Debug, PartialEq)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

/// `zyris config` — what a console can change without the window.
#[derive(Subcommand, Debug, PartialEq)]
pub enum ConfigCommand {
    /// Every setting this command can change, what it is now, and what it means.
    List,

    /// Print one setting's value.
    Get {
        /// The setting, spelled as `config list` prints it — `voice.listen`.
        key: String,
    },

    /// Write one setting.
    Set {
        /// The setting, spelled as `config list` prints it — `voice.listen`.
        key: String,

        /// What to write: `true`/`false`, a number, a line of text, or `unset` to clear a setting
        /// the file is allowed to leave out.
        value: String,
    },
}

#[derive(Args, Debug, PartialEq)]
pub struct AutostartArgs {
    #[command(subcommand)]
    pub command: AutostartCommand,
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum AutostartCommand {
    /// Start Zyris when this computer signs in.
    Enable,

    /// Stop starting Zyris when this computer signs in.
    Disable,

    /// What the machine says right now, without changing anything.
    Status,
}

#[derive(Args, Debug, PartialEq)]
pub struct McpArgs {
    #[command(subcommand)]
    pub command: McpCommand,
}

/// `zyris mcp` — the local servers this node runs and announces as capabilities.
#[derive(Subcommand, Debug, PartialEq)]
pub enum McpCommand {
    /// The servers in this instance's list, and what each is doing on disk.
    List,

    /// Start one of them, from the next launch on.
    Enable {
        /// The name it is configured under.
        name: String,
    },

    /// Stop starting one of them, from the next launch on.
    Disable {
        /// The name it is configured under.
        name: String,
    },

    /// Add a server to the list.
    ///
    /// Everything after the command is handed to that server as its arguments, so this program's
    /// own flags go **before** the subcommand: `zyris --server URL mcp add notes mcp-notes --dir
    /// ~/notes` adds a server, while `… mcp add notes mcp-notes --server URL` would pass
    /// `--server URL` to the server.
    Add {
        /// What this server is called here, and — prefixed — the capability an agent addresses.
        name: String,

        /// The command to run. Whatever a shell would find on `PATH`, or an absolute path.
        command: String,

        /// What to pass it. Each one is a single argument; nothing here runs a shell.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Take a server out of the list.
    Remove {
        /// The name it is configured under.
        name: String,
    },
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

    /// What this mode is called in the console's state file, and printed as by `zyris status`.
    ///
    /// One spelling for both, so a person who reads `mode window-hidden` in a status block and
    /// `"mode": "window-hidden"` in the file under it is looking at one answer rather than two.
    pub fn name(self) -> &'static str {
        match self {
            Mode::Window => "window",
            Mode::WindowHidden => "window-hidden",
            Mode::Headless => "headless",
        }
    }
}

/// What the autostart flags asked for, when one of them was given.
///
/// Neither starts a node: `main` acts on this and returns, before the instance lock is taken. The
/// `autostart` subcommand asks for the same two things, and `console::commands::autostart` is
/// where both spellings meet, so a flag and a subcommand cannot install different things.
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

    /// The subcommand this run is, if it is one. `None` means this process is the node.
    ///
    /// **Not `command`**, which is what the `CommandFactory` trait already calls the thing that
    /// builds this parser — and which this crate's tests need, to render the help a person reads
    /// and to assert that the parser is well formed. One name, one meaning.
    pub fn subcommand(&self) -> Option<&Command> {
        self.command.as_ref()
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

    /// Refuse a flag that has nothing to say to the subcommand it was given with.
    ///
    /// **Clap parses these in either order**, which is what makes this necessary rather than
    /// tidy: `zyris --headless login` and `zyris login --headless` both parse — the flags are
    /// global so that `--server` works beside any command — and without this the first would run a
    /// login while the person who typed it believed they had asked for a windowless run.
    ///
    /// The refusal is a `clap::Error` rather than a sentence of our own so that it is printed with
    /// the usage line, the way every other wrong thing typed at this program is.
    pub fn check(&self) -> Result<(), clap::Error> {
        let Some(command) = self.command.as_ref() else { return Ok(()) };

        if (self.headless || self.minimized) && !matches!(command, Command::Up) {
            let flag = if self.headless { "--headless" } else { "--minimized" };
            return Err(conflict(format!(
                "`{flag}` says what kind of node to run, and `zyris {}` does not run one. The two \
                 flags are read by `zyris` on its own — the node — and by `zyris up`, which starts \
                 one in the background.",
                command.label()
            )));
        }

        if self.install_autostart || self.uninstall_autostart {
            let flag = if self.install_autostart { "--install-autostart" } else { "--uninstall-autostart" };
            let same = if self.install_autostart { "enable" } else { "disable" };
            return Err(conflict(format!(
                "`{flag}` and `zyris autostart {same}` are the same switch. Use the subcommand \
                 beside `zyris {}`.",
                command.label()
            )));
        }

        Ok(())
    }
}

fn conflict(message: String) -> clap::Error {
    clap::Error::raw(clap::error::ErrorKind::ArgumentConflict, message).with_cmd(&Cli::command())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};

    #[test]
    fn no_arguments_means_the_window() {
        let cli = Cli::parse_from(["zyris"]);

        assert_eq!(cli.mode(), Mode::Window);
        assert!(cli.mode().shows_a_window());
        assert_eq!(cli.subcommand(), None);
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

    /// Every command the step asks for, parsed the way a person would type it.
    #[test]
    fn each_console_command_is_parsed() {
        assert_eq!(Cli::parse_from(["zyris", "up"]).subcommand(), Some(&Command::Up));
        assert_eq!(Cli::parse_from(["zyris", "down"]).subcommand(), Some(&Command::Down));
        assert_eq!(Cli::parse_from(["zyris", "status"]).subcommand(), Some(&Command::Status));
        assert_eq!(Cli::parse_from(["zyris", "login"]).subcommand(), Some(&Command::Login));
        assert_eq!(
            Cli::parse_from(["zyris", "config", "list"]).subcommand(),
            Some(&Command::Config(ConfigArgs { command: ConfigCommand::List }))
        );
        assert_eq!(
            Cli::parse_from(["zyris", "config", "get", "voice.listen"]).subcommand(),
            Some(&Command::Config(ConfigArgs {
                command: ConfigCommand::Get { key: "voice.listen".to_string() }
            }))
        );
        assert_eq!(
            Cli::parse_from(["zyris", "config", "set", "voice.listen", "true"]).subcommand(),
            Some(&Command::Config(ConfigArgs {
                command: ConfigCommand::Set {
                    key: "voice.listen".to_string(),
                    value: "true".to_string()
                }
            }))
        );
    }

    /// And one with the flag that every command accepts, in either order.
    #[test]
    fn the_server_flag_reaches_a_console_command_whichever_side_it_is_written_on() {
        // Both orders, because both are typed. `--server` names the *instance*, so a console
        // command pointed at a development server reads and writes that run's settings and
        // asks about that run's lock — which is what makes `zyris --server URL down` meaningful.
        for argv in [
            ["zyris", "--server", "ws://127.0.0.1:1/ws", "status"],
            ["zyris", "status", "--server", "ws://127.0.0.1:1/ws"],
        ] {
            let cli = Cli::parse_from(argv);
            assert_eq!(cli.subcommand(), Some(&Command::Status));
            assert_eq!(cli.server(), Some("ws://127.0.0.1:1/ws"));
            assert!(cli.check().is_ok(), "`--server` is for every command");
        }
    }

    #[test]
    fn a_mode_flag_beside_a_command_that_does_not_run_a_node_is_refused() {
        // Both orders, because both are typed, and one sentence either way. Silently ignoring the
        // flag would leave somebody who typed `zyris --headless status` believing they had asked
        // for something.
        for argv in [["zyris", "--headless", "status"], ["zyris", "status", "--headless"]] {
            let cli = Cli::parse_from(argv);
            let refused = cli.check().expect_err("a mode flag with `status` has to be refused");
            assert!(
                refused.to_string().contains("--headless"),
                "the refusal has to name the flag: {refused}"
            );
            assert!(
                refused.to_string().contains("status"),
                "and the command: {refused}"
            );
        }
    }

    #[test]
    fn a_mode_flag_says_what_up_should_start() {
        // `up` is the one command a mode flag has anything to say to: it is what the node it
        // starts will be.
        let cli = Cli::parse_from(["zyris", "up", "--headless"]);

        assert!(cli.check().is_ok());
        assert_eq!(cli.mode(), Mode::Headless);
        assert_eq!(cli.subcommand(), Some(&Command::Up));
    }

    #[test]
    fn an_autostart_flag_beside_a_command_is_refused_and_points_at_the_subcommand() {
        let cli = Cli::parse_from(["zyris", "--install-autostart", "status"]);

        let refused = cli.check().expect_err("the flags and the subcommand are one switch");
        let said = refused.to_string();
        assert!(said.contains("--install-autostart"), "{said}");
        assert!(said.contains("zyris autostart enable"), "{said}");
    }

    /// The refusals are clap's, so they are printed with the usage line rather than as a sentence
    /// of our own — which is the only reason this builds a `clap::Error` instead of returning an
    /// `anyhow::Error`.
    #[test]
    fn a_refusal_is_a_usage_error_naming_this_program() {
        let refused = Cli::parse_from(["zyris", "status", "--minimized"])
            .check()
            .expect_err("refused");

        assert_eq!(refused.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    /// **The one thing clap cannot tell us by parsing.** A global argument that conflicts with
    /// another one, a subcommand whose name collides with an argument's, a required argument no
    /// caller can supply — every one of those is a panic at parse time in a program whose only
    /// entry point is `main`, and this is where it becomes a failing test instead.
    #[test]
    fn the_command_line_is_well_formed() {
        Cli::command().debug_assert();
    }

    /// The help a person reads has to name every command, and each command's own help has to
    /// describe itself. Rendered rather than parsed: `--help` is output, and output is what is
    /// being asked about.
    #[test]
    fn every_command_is_in_the_help_and_has_help_of_its_own() {
        let top = Cli::command().render_help().to_string();
        for name in ["up", "down", "status", "login", "config", "autostart", "mcp"] {
            assert!(top.contains(name), "`zyris --help` does not list `{name}`:\n{top}");

            // And each command's own help describes it. Read off the same `clap::Command` the
            // help is rendered from, so this is the doc comment on the variant arriving where a
            // person can read it rather than a sentence repeated in the test.
            let mut command = Cli::command();
            let sub = command.find_subcommand_mut(name).expect("just listed");
            let about = sub.get_about().map(|about| about.to_string()).unwrap_or_default();
            assert!(!about.is_empty(), "`zyris {name}` has no description at all");
            let help = sub.render_help().to_string();
            let first = about.split('.').next().expect("a sentence").trim().to_string();
            assert!(
                help.contains(&first),
                "`zyris {name} --help` does not say what it is:\n{help}"
            );
        }
    }

    #[test]
    fn the_config_help_lists_the_three_forms_and_the_keys_they_take() {
        let mut command = Cli::command();
        let config = command.find_subcommand_mut("config").expect("listed");
        let help = config.render_help().to_string();

        for form in ["list", "get", "set"] {
            assert!(help.contains(form), "`zyris config --help` does not offer `{form}`:\n{help}");
        }
        // And what a key looks like, because `voice.listen` is the one thing about this command
        // that a person cannot guess.
        let mut command = Cli::command();
        let get = command
            .find_subcommand_mut("config")
            .and_then(|config| config.find_subcommand_mut("get"))
            .expect("listed")
            .render_help()
            .to_string();
        assert!(get.contains("voice.listen"), "`config get --help` does not show a key:\n{get}");
    }

    #[test]
    fn the_mcp_help_shows_the_five_things_it_does() {
        let mut command = Cli::command();
        let mcp = command.find_subcommand_mut("mcp").expect("listed");
        let help = mcp.render_help().to_string();

        for form in ["list", "enable", "disable", "add", "remove"] {
            assert!(help.contains(form), "`zyris mcp --help` does not offer `{form}`:\n{help}");
        }
    }

    /// A command nobody has heard of is refused the way every other wrong input is, with clap's
    /// own suggestion machinery rather than silence.
    #[test]
    fn an_unknown_command_is_refused() {
        assert!(Cli::try_parse_from(["zyris", "statuss"]).is_err());
        // And a config form that does not exist, which is the same mistake one level down.
        assert!(Cli::try_parse_from(["zyris", "config", "delete", "voice.listen"]).is_err());
        // `config` on its own is not a form: it has to say which.
        assert!(Cli::try_parse_from(["zyris", "config"]).is_err());
    }

    #[test]
    fn the_mode_names_are_the_ones_the_state_file_uses() {
        // One spelling for the console's report and for the file under it.
        assert_eq!(Mode::Window.name(), "window");
        assert_eq!(Mode::WindowHidden.name(), "window-hidden");
        assert_eq!(Mode::Headless.name(), "headless");
    }
}
