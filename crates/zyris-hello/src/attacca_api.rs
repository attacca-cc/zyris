//! The slice of the server's `attacca_api` capability this node actually calls.
//!
//! A consumer declares the capability it wants to *use*, not the one the peer implements. Matching
//! is by `(name, version)` — the announced tool list is never compared — so this trait may name one
//! method out of the dozen the server offers, and the generated `AttaccaApiClient` still resolves
//! against the real announcement. Declaring only what you call is the point: this node never lists
//! sessions, so it carries no session types.
//!
//! The same applies to the payload structs. `ZAgent` here has the two fields this file reads; the
//! server sends more, and serde ignores the rest.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ZAgent {
    pub id: String,
    pub name: String,
}

#[zyris::capability(name = "attacca_api", version = 1)]
pub trait AttaccaApi {
    /// List the caller's agents.
    async fn list_agents(&self) -> zyris::Result<Vec<ZAgent>>;
}
