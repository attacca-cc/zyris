//! Local MCP servers: one process each, asked what it has and then asked to do it.
//!
//! This crate is only ever an MCP **client**. It spawns a command somebody configured, speaks the
//! protocol to it over that process's stdin and stdout, and hands back a tool list. Turning that
//! list into something an agent on Attacca can call is `promote.rs`'s job, and it is separate
//! because the two fail differently: a server that will not spawn is an operational problem a
//! person fixes, and a tool whose schema will not translate is a compatibility problem this code
//! has to decide about.

pub mod server;

pub use rmcp::model::Tool;
pub use server::{STARTUP_DEADLINE, Server, is_disconnected};
