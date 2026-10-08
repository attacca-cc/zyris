//! What `zyris` does with the window closed.
//!
//! Seven commands that do one job each and return — `up`, `down`, `status`, `login`, `config`,
//! `autostart`, `mcp` — plus the file a running node writes so that three of them have something
//! to read ([`state`]).
//!
//! # A console command is not a node
//!
//! It takes no instance lock, starts nothing, and owns no connection. That is not an optimisation
//! but the whole point: `zyris status` has to be able to answer *while* the node is running — that
//! is the only time it is interesting — and `zyris down` has to be able to reach a node that holds
//! the lock it would otherwise be refused by. The one command that does store something, `login`,
//! takes the lock itself and only because a second enrolment for one machine is a thing to avoid.
//!
//! What a command is about is named the same way a run is: `--server URL` picks the instance, and
//! with it the data directory, the settings files and the lock. So `zyris --server URL down` stops
//! a development node and never the real one, and `zyris --server URL config set …` edits that
//! run's settings.
//!
//! # Why this output is not the log
//!
//! A node reports through `tracing`; a console command reports with `println!`. The difference is
//! what the reader is doing. Nobody is watching the node's stream, so it is better as structured
//! lines with a level and a timestamp that `RUST_LOG` turns up and down. A command's output is a
//! *report to the person who just typed it* — a settings table, a status block, a code to enter —
//! and a timestamp on every row of a table is noise. It is the same reasoning `--help` already
//! follows: clap prints that, not `tracing`. Anything diagnostic still goes through `tracing`, so
//! `RUST_LOG=debug zyris down` shows how it got there.

pub mod commands;
pub mod state;

pub use commands::run;
