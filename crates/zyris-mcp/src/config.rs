//! Which MCP servers this machine runs, read from a file a person edits.
//!
//! One JSON file, [`CONFIG_FILE`], in the directory the rest of this instance's state lives in.
//! Nothing writes it yet; a person creates it, and a machine that never has one is the ordinary
//! case rather than an unconfigured one.
//!
//! # Where it lives, and why that is an argument rather than a constant
//!
//! [`Config::path`] takes the data directory rather than finding one, because **`zyris-app`'s
//! `data_dir` is scoped by instance**: a `--server` run names itself `zyris-dev-<server>` and
//! keeps its own credentials, lock, audit log and peer identity under that name, precisely so it
//! cannot act as the production node. A server list that was *not* scoped the same way would undo
//! a piece of that: a development run would start the production machine's servers and announce
//! them to whatever server it was pointed at, and a production run would start a development
//! machine's. So the caller passes the directory it already derived, the same way
//! `zyris-tools`'s `Transfers::bind` is handed one, and there is one decision about what this
//! instance is rather than two that can disagree.
//!
//! # What a bad file costs
//!
//! **Never the machine.** A file that will not parse, will not read, or contradicts itself costs
//! its own contents and nothing else: [`start`] logs the reason and returns nothing, `terminal`,
//! `file_io` and the rest are announced exactly as they would have been, and Zyris starts. This
//! is the shape `zyris-tools`'s `Transfers::bind` and `screen_pair` already use for a failure
//! that is about one capability rather than about the node.
//!
//! Which failures cost the *whole file* and which cost one entry is a real decision, and the two
//! halves are not arbitrary:
//!
//! - **The document is refused when it is ambiguous.** Two entries sharing a name is the case, and
//!   it is caught in [`Config::parse`] rather than at startup. Two servers called `notes` both
//!   promote to `mcp_notes`; `zyris-core`'s `Served::build` refuses a duplicate `(name, version)`
//!   and a node that trips it announces **nothing at all** — not one MCP tool, and not `terminal`
//!   or `file_io` either. A typo in this file must not be able to disarm the machine, so it is
//!   caught before anything is started. Picking one of the two instead — the way `promote.rs`
//!   keeps the first of two tools sharing a name — would be wrong here for a reason that does not
//!   apply there: a second tool of the same name is unreachable *by construction*, so keeping the
//!   first costs nothing, whereas two entries called `notes` are two different processes and
//!   choosing by file order routes an agent's call into whichever one happens to be written first.
//!   A call that lands on the wrong server is worse than a call that lands nowhere.
//! - **One entry is refused when only that entry is wrong.** A command that is not there, a
//!   command that will not speak MCP, a name with a dot in it that no call could be addressed to.
//!   Each costs its own server, is logged with what to check, and leaves the others running. This
//!   is the same reasoning the rest of this workspace applies to a missing display server: a
//!   partial answer is a correct answer, and an agent can tell an absent tool from a broken one.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::promote::{Promoted, capability_name};
use crate::server::Server;

/// The file [`Config::read`] looks for, inside the instance's data directory.
pub const CONFIG_FILE: &str = "mcp-servers.json";

/// One MCP server a person asked for.
///
/// `args` and `enabled` may be left out; `name` and `command` may not, because neither has a
/// defensible default — a server with no name has no capability name, and one with no command is
/// not a server.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
// **Rejecting a field nobody defined is the point, not pedantry.** This file is hand-edited, and
// the mistakes it collects are spelling ones: `"commnad"` or `"arg"` would otherwise be dropped
// in silence and leave a person reading an error about a command that is empty, or a server that
// starts with none of the arguments it was given. The cost is that a field added in a later
// version of Zyris makes an older one refuse the file, which is a cost this can afford — the file
// is local, small, and read by exactly the binary sitting next to it.
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// What this server is called here, and — prefixed — the capability an agent addresses.
    pub name: String,
    /// The command to run. Whatever a shell would find on `PATH`, or an absolute path.
    pub command: String,
    /// What to pass it. Each element is one argument, already split: nothing here runs a shell,
    /// so quoting rules never enter into it.
    #[serde(default)]
    pub args: Vec<String>,
    /// Whether to start it. Absent means yes: the entry exists because somebody wanted the
    /// server, and a file where every entry has to say `"enabled": true` teaches people to
    /// copy a line they have not read.
    #[serde(default = "enabled_unless_said_otherwise")]
    pub enabled: bool,
}

fn enabled_unless_said_otherwise() -> bool {
    true
}

/// The whole file.
///
/// An array of entries under one key rather than an object keyed by name, which is the shape
/// several other MCP clients use. The reason is the duplicate rule above: `serde_json`'s object
/// parser keeps the **last** of two identical keys and says nothing, so an object would make the
/// one mistake this file has to be loud about the one mistake it could not see. An array of
/// entries that each name themselves can be checked.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub servers: Vec<ServerConfig>,
}

impl Config {
    /// Where the server list lives for an instance whose state is in `data_dir`.
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join(CONFIG_FILE)
    }

    /// Read the server list, or say why it could not be read.
    ///
    /// **A file that is not there is not a failure**: it is what every machine looks like until
    /// somebody configures one, and it answers with no servers and no complaint. Every other way
    /// reading can go wrong — a directory in the file's place, a permission that says no, a file
    /// that is not JSON, JSON that is not this shape, two entries with one name — is an error
    /// with the path in it, because each of those is a state somebody has to be told about.
    ///
    /// An empty file is one of those. Zero bytes is not valid JSON, and treating it as "no
    /// servers" would read a half-finished write as a decision.
    pub fn read(data_dir: &Path) -> anyhow::Result<Config> {
        let path = Config::path(data_dir);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Config::default());
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("reading the MCP server list at {}", path.display()));
            }
        };
        Config::parse(&text)
            .with_context(|| format!("reading the MCP server list at {}", path.display()))
    }

    /// The file's text as a server list, or why it is not one.
    ///
    /// Separate from [`Self::read`] so the parsing rules can be tested without a directory, and
    /// because the two fail differently: this one's errors are all things a person typed.
    pub fn parse(text: &str) -> anyhow::Result<Config> {
        let config: Config = serde_json::from_str(text).context(
            "this file has to be a JSON object with a `servers` array in it, each entry naming a \
             `name` and a `command` and optionally `args` and `enabled`",
        )?;
        config.refuse_duplicate_names()?;
        Ok(config)
    }

    /// The servers that are meant to run, in the order the file lists them.
    pub fn enabled(&self) -> impl Iterator<Item = &ServerConfig> {
        self.servers.iter().filter(|server| server.enabled)
    }

    /// Two entries under one name make one capability name, which is why this is fatal to the
    /// whole file. See [the module documentation](self#what-a-bad-file-costs).
    ///
    /// Exact equality, not a case-insensitive or trimmed comparison: what collides is the
    /// capability name, `notes` and `Notes` promote to two different ones, and a rule stricter
    /// than the collision it guards against would refuse a file that works.
    ///
    /// Disabled entries count. A name is ambiguous whether or not one of the two is running
    /// today, and a person who fixes this by disabling one of them has left the trap armed for
    /// whoever turns it back on.
    fn refuse_duplicate_names(&self) -> anyhow::Result<()> {
        for (index, server) in self.servers.iter().enumerate() {
            if let Some(earlier) =
                self.servers[..index].iter().position(|other| other.name == server.name)
            {
                anyhow::bail!(
                    "two MCP servers are called `{}` (entries {} and {}), and both would be \
                     announced as `{}{}`. A node that announces one capability name twice \
                     announces nothing at all, so no MCP server is started until one of them is \
                     renamed.",
                    server.name,
                    earlier + 1,
                    index + 1,
                    crate::CAPABILITY_PREFIX,
                    server.name
                );
            }
        }
        Ok(())
    }
}

/// What [`start`] found on disk and what it managed to run.
///
/// **Both halves, because they answer different questions.** `running` is what gets announced;
/// `config` is what a person wrote, and it is the only way anything downstream can know that an
/// entry exists at all — a server the file disables, or one that would not start, is absent from
/// `running` and indistinguishable there from a server nobody ever configured. Whatever supervises
/// these afterwards has to be able to tell those apart.
///
/// A file that could not be read gives [`Default`]: no entries and nothing running. That is the
/// same answer as a machine with no file at all, which is deliberate here — the difference is
/// already logged by `start`, loudly, and nothing downstream can act on it differently.
#[derive(Debug, Default)]
pub struct Started {
    /// The file, as it was read, disabled entries included.
    pub config: Config,
    /// The servers that are running, in the order the file lists them.
    pub running: Vec<Arc<Promoted>>,
}

/// Read the configured servers, start them, and hand back the file and the ones that are running.
///
/// **This cannot fail**, by design: every way it can go wrong is logged and costs only what it
/// has to. See [the module documentation](self#what-a-bad-file-costs) for which failures cost the
/// whole file and which cost one server.
///
/// One after another rather than all at once. Two reasons, and neither is that it is simpler:
/// a real MCP server is usually `npx` or a Python interpreter, so starting four together on a
/// machine with a few gigabytes of memory is a spike at exactly the moment the window is trying
/// to appear; and the announcement's order then follows the file's, which is the order the person
/// wrote and the order the window will list. The cost is named: a server that starts and never
/// speaks holds this up for [`crate::STARTUP_DEADLINE`], and several of them hold it up for that
/// many multiples. The deadline is what keeps that bounded.
pub async fn start(data_dir: &Path) -> Started {
    let config = match Config::read(data_dir) {
        Ok(config) => config,
        // `error!`, unlike a machine with no display server or no network: this file exists only
        // because somebody wrote it, so a file that cannot be read is always a mistake somebody
        // can fix, and nothing about it is the ordinary state of an ordinary machine.
        Err(error) => {
            tracing::error!(
                error = format!("{error:#}"),
                "the MCP server list could not be read, so no MCP server is started and none of \
                 their tools are announced; everything else on this machine is unaffected. Fix \
                 the file and restart Zyris."
            );
            return Started::default();
        }
    };

    let mut running = Vec::new();
    for server in config.enabled() {
        match start_one(server).await {
            Ok(one) => running.push(Arc::new(one)),
            Err(error) => tracing::error!(
                server = server.name,
                command = server.command,
                args = ?server.args,
                error = format!("{error:#}"),
                "an MCP server did not start, so none of its tools are announced; the other \
                 servers and this machine's own capabilities are unaffected. Check that the \
                 command runs from a terminal and speaks MCP over its standard input and output, \
                 that its arguments are right, and — if it starts fine by hand — that every tool \
                 it lists has an object for its `inputSchema`, because one that is not an object \
                 makes the whole tool list unreadable and the error above will name neither the \
                 tool nor the field."
            ),
        }
    }
    Started { config, running }
}

/// One entry, from a line in a file to a capability.
///
/// The name is checked **before** the command runs. A server called `my.notes` can never be
/// announced whatever it does — `capability.tool` splits at the first dot, so every call to it
/// would be addressed to `mcp_my`, which nothing announced — and starting a process only to drop
/// it is a side effect nobody asked for on the way to an error that was knowable without it.
pub async fn start_one(config: &ServerConfig) -> anyhow::Result<Promoted> {
    capability_name(&config.name)?;
    let server = Server::spawn(&config.name, &config.command, &config.args).await?;
    Promoted::new(Arc::new(server))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_that_is_not_there_is_no_servers_rather_than_a_failure() {
        // What every machine looks like until somebody configures one.
        let dir = tempfile::tempdir().unwrap();

        let config = Config::read(dir.path()).expect("an absent file is not a failure");

        assert!(config.servers.is_empty());
    }

    #[test]
    fn a_file_that_cannot_be_read_is_a_failure_rather_than_no_servers() {
        // A directory where the file should be stands in for every way the filesystem can say no
        // — a permission, a broken link, a mount that went away. What matters is that it does not
        // read as "this machine has no MCP servers", which is the answer that would leave
        // somebody staring at an empty list with no idea why.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(Config::path(dir.path())).unwrap();

        let error = Config::read(dir.path()).expect_err("a file that will not read is an error");

        assert!(
            format!("{error:#}").contains(CONFIG_FILE),
            "the error has to name the file: {error:#}"
        );
    }

    #[test]
    fn a_file_that_is_not_json_says_so_and_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(Config::path(dir.path()), "servers: notes").unwrap();

        let error = Config::read(dir.path()).expect_err("this is not JSON");

        assert!(format!("{error:#}").contains(CONFIG_FILE), "{error:#}");
    }

    #[test]
    fn an_empty_file_is_refused_rather_than_read_as_no_servers() {
        // Zero bytes is a half-finished write as often as it is a decision, and there is a way to
        // say "no servers" that cannot be confused with one: `{}`.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(Config::path(dir.path()), "").unwrap();

        assert!(Config::read(dir.path()).is_err());
    }

    #[test]
    fn a_file_with_no_servers_in_it_is_read_as_no_servers() {
        assert!(Config::parse("{}").unwrap().servers.is_empty());
        assert!(Config::parse(r#"{ "servers": [] }"#).unwrap().servers.is_empty());
    }

    #[test]
    fn an_entry_is_read_the_way_it_was_written() {
        let config = Config::parse(
            r#"{ "servers": [ { "name": "desk-notes", "command": "notes-mcp",
                               "args": ["--root", "/home/me/notes"] } ] }"#,
        )
        .unwrap();

        assert_eq!(
            config.servers,
            [ServerConfig {
                name: "desk-notes".to_string(),
                command: "notes-mcp".to_string(),
                args: vec!["--root".to_string(), "/home/me/notes".to_string()],
                enabled: true,
            }]
        );
    }

    #[test]
    fn a_server_runs_unless_the_file_says_not_to() {
        // Both halves, because the default is the interesting one: an entry exists because
        // somebody wanted the server.
        let on = Config::parse(r#"{ "servers": [ { "name": "a", "command": "x" } ] }"#).unwrap();
        assert!(on.servers[0].enabled);
        assert_eq!(on.enabled().count(), 1);

        let off = Config::parse(
            r#"{ "servers": [ { "name": "a", "command": "x", "enabled": false } ] }"#,
        )
        .unwrap();
        assert!(!off.servers[0].enabled);
        assert_eq!(off.enabled().count(), 0);
    }

    #[test]
    fn an_entry_that_names_no_command_is_refused() {
        // There is no default for this and no honest way to invent one.
        assert!(Config::parse(r#"{ "servers": [ { "name": "a" } ] }"#).is_err());
        assert!(Config::parse(r#"{ "servers": [ { "command": "x" } ] }"#).is_err());
    }

    #[test]
    fn a_misspelled_field_is_refused_rather_than_ignored() {
        // The mistake this file actually collects. Without `deny_unknown_fields` this reads as a
        // server with no arguments at all, and the person who typed it has nothing to go on.
        let error = Config::parse(
            r#"{ "servers": [ { "name": "a", "command": "x", "arg": ["--root"] } ] }"#,
        )
        .expect_err("`arg` is not a field");

        assert!(format!("{error:#}").contains("arg"), "{error:#}");
    }

    #[test]
    fn valid_json_of_the_wrong_shape_is_refused() {
        // Each of these is JSON and none of them is a server list. The object-keyed-by-name shape
        // is the one worth refusing loudly: it is what several other MCP clients use, so somebody
        // will paste one in.
        for wrong in [
            r#"[ { "name": "a", "command": "x" } ]"#,
            r#"{ "servers": { "notes": { "command": "x" } } }"#,
            r#"{ "servers": [ "notes" ] }"#,
            r#"{ "servers": [ { "name": 7, "command": "x" } ] }"#,
            "42",
        ] {
            assert!(Config::parse(wrong).is_err(), "this should not have parsed: {wrong}");
        }
    }

    #[test]
    fn two_servers_with_one_name_are_refused_when_the_file_is_read() {
        // Not at startup, and not by picking one. See the module documentation: a node that
        // announces one capability name twice announces nothing at all, so this is caught where a
        // person can be told which of the two entries to rename.
        let error = Config::parse(
            r#"{ "servers": [ { "name": "notes", "command": "one" },
                              { "name": "calendar", "command": "two" },
                              { "name": "notes", "command": "three" } ] }"#,
        )
        .expect_err("two servers cannot share a name");

        let message = format!("{error:#}");
        assert!(message.contains("notes"), "the name has to be in it: {message}");
        // And which two entries, because a file with twenty in it is not searchable by eye.
        assert!(message.contains('1') && message.contains('3'), "{message}");
        assert!(
            !message.contains("calendar"),
            "the entry that is fine should not be named: {message}"
        );
    }

    #[test]
    fn two_names_that_merely_look_alike_are_not_duplicates() {
        // The rule guards a collision, so it has to be exactly as wide as one. `notes` and
        // `Notes` promote to two different capability names and a machine may well run both.
        Config::parse(
            r#"{ "servers": [ { "name": "notes", "command": "one" },
                              { "name": "Notes", "command": "two" },
                              { "name": "notes ", "command": "three" } ] }"#,
        )
        .expect("these are three different capability names");
    }

    #[test]
    fn a_disabled_entry_still_counts_as_a_duplicate() {
        // Turning one of the two off is not a fix, it is a trap left armed for whoever turns it
        // back on.
        assert!(
            Config::parse(
                r#"{ "servers": [ { "name": "notes", "command": "one" },
                                  { "name": "notes", "command": "two", "enabled": false } ] }"#
            )
            .is_err()
        );
    }

    /// A command that is not there, spelled so that no machine could accidentally have one.
    const MISSING_COMMAND: &str = "zyris-no-such-mcp-server-anywhere-on-this-machine";

    fn entry(name: &str, command: &str) -> ServerConfig {
        ServerConfig {
            name: name.to_string(),
            command: command.to_string(),
            args: Vec::new(),
            enabled: true,
        }
    }

    #[tokio::test]
    async fn a_server_that_will_not_start_says_which_one_and_what_it_ran() {
        // This error is the whole of what the log line above carries, and that line is all a
        // person gets: a machine with four servers on it and one of them silent is not a puzzle
        // anybody can solve from "an MCP server did not start".
        let error = start_one(&entry("desk-notes", MISSING_COMMAND))
            .await
            .expect_err("this command is not on any machine");

        let message = format!("{error:#}");
        assert!(message.contains("desk-notes"), "which server: {message}");
        assert!(message.contains(MISSING_COMMAND), "what it ran: {message}");
    }

    #[tokio::test]
    async fn a_name_that_cannot_be_announced_is_refused_before_the_command_runs() {
        // Starting a process in order to drop it is a side effect nobody asked for on the way to
        // an error that was knowable without it. The command here is one that does not exist, so
        // an implementation that spawned first would fail with *its* name in the message instead
        // — which is what the second assertion is watching for.
        let error = start_one(&entry("my.notes", MISSING_COMMAND))
            .await
            .expect_err("a dot makes a capability nothing can address");

        let message = format!("{error:#}");
        assert!(message.contains("my.notes"), "{message}");
        assert!(
            !message.contains(MISSING_COMMAND),
            "the command was run before the name was checked: {message}"
        );
    }

    #[test]
    fn the_file_sits_in_the_directory_it_is_given() {
        // The instance scoping, which is the whole reason this takes a directory. A `--server`
        // run must not start the production machine's servers, and it is `zyris-app`'s per-
        // instance data directory that keeps it from doing so.
        let production = Config::path(Path::new("/data/zyris"));
        let development = Config::path(Path::new("/data/zyris-dev-localhost"));

        assert_ne!(production, development);
        assert!(production.starts_with("/data/zyris"));
        assert_eq!(production.file_name().unwrap(), CONFIG_FILE);
    }
}
