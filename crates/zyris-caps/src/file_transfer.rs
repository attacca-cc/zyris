//! The surface an agent calls. Announced on the attacca link.
//!
//! The point is that this is a *different* capability from the peer link's `peer_transfer` — that
//! is what makes "the peer link opens file transfer and nothing else" a fact rather than a piece of
//! filtering logic.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SendReceipt {
    /// Echoes back the name the caller asked for, so a reply can be matched to its request.
    pub node: String,
    /// The final path on the receiving side. Empty while the transfer is still unfinished.
    #[serde(default)]
    pub written: String,
    /// What actually landed. **Zero on a `pending` receipt** — nothing has been confirmed written
    /// yet, and reporting the source file's size here would read as progress that has not happened.
    pub bytes: u64,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub replaced: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo: Option<String>,
    /// Whether the bytes took a direct path or went through the relay.
    #[serde(default)]
    pub direct: bool,
    /// Not finished yet. **Not an error** — calling again with the same arguments resumes.
    #[serde(default)]
    pub pending: bool,
    /// One line saying what to do now. Absent means there is nothing left to do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InboxEntry {
    pub from: String,
    pub name: String,
    pub bytes: u64,
    pub path: String,
    pub received_unix_ms: u64,
}

#[zyris::capability(name = "file_transfer", version = 1)]
pub trait FileTransfer {
    /// Send a file on this machine to another node on the same account.
    ///
    /// `node` is the name a user says — "send it to my laptop". It says which peer was meant and
    /// nothing more: it is not what the peer's key is trusted through. See `ZPeerEntry::slug` in
    /// `zyris-attacca` for why a name a server issues can never be the anchor.
    ///
    /// If it cannot finish within 60 seconds the reply is **not an error** — it comes back with
    /// `pending: true`, and `next` says to call again. Attacca cuts a node call off at 60 seconds
    /// with a `Timeout` error, and an agent that receives that error calls the same tool again
    /// anyway; not making the shape of a failure is the better answer.
    async fn send_to(
        &self,
        node: String,
        path: String,
        name: Option<String>,
        overwrite: Option<bool>,
    ) -> zyris::Result<SendReceipt>;

    /// What has arrived in this machine's inbox.
    async fn inbox_list(&self) -> zyris::Result<Vec<InboxEntry>>;
}

#[cfg(test)]
mod tests {
    use zyris::proto::Transfer;

    #[test]
    fn there_are_two_tools_and_both_are_unary() {
        let d = super::file_transfer_capability();
        assert_eq!(d.name, "file_transfer");
        assert_eq!(d.version, 1);
        // Adding a tool grows the surface an agent can reach. This is where that gets caught.
        let mut names: Vec<_> = d.tools.iter().map(|t| t.name.as_str()).collect();
        names.sort();
        assert_eq!(names, ["inbox_list", "send_to"]);
        assert!(d.tools.iter().all(|t| t.transfer == Transfer::Unary));
    }
}
