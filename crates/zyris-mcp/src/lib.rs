//! Local MCP servers: one process each, asked what it has and then asked to do it.
//!
//! This crate is only ever an MCP **client**. It spawns a command somebody configured, speaks the
//! protocol to it over that process's stdin and stdout, and hands back a tool list. Turning that
//! list into something an agent on Attacca can call is [`promote`]'s job, and it is separate
//! because the two fail differently: a server that will not spawn is an operational problem a
//! person fixes, and a tool list that will not translate is a compatibility problem this code has
//! to decide about.
//!
//! **The compatibility problem turned out to be about names rather than schemas**, and the two
//! halves do not split where the design expected. A schema `rmcp` cannot hold does not reach
//! [`promote`] at all — it makes the whole `tools/list` answer undeserializable, so
//! [`Server::spawn`] fails and the server is absent. What [`Promoted`] actually has to decide
//! about is a server whose name will not make an addressable capability and a tool name offered
//! twice. Both are written up on [`promote`].

pub mod promote;
pub mod server;

pub use promote::{CAPABILITY_PREFIX, DroppedTool, PROMOTED_VERSION, Promoted};
pub use rmcp::model::Tool;
pub use server::{STARTUP_DEADLINE, Server, is_disconnected};
