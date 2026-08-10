//! The wire **between** A and B. Announced only on the peer link.
//!
//! The surface an agent calls is `file_transfer`, and that is a different thing from this. Keeping
//! the two apart is what makes "the peer link opens exactly this one capability" a fact rather than
//! a piece of filtering logic.
//!
//! **The receiving side pulls the bytes.** There is no wire path for bulk data to travel
//! caller → callee (attachments on request params are not implemented). In `pull` the sending side
//! is the callee, so it reuses the `uni_stream` path that already works. Resume comes along for
//! free because of that.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zyris::{Chunk, Streaming};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TransferOffer {
    /// Derived deterministically from `(sending node_id, name, size, sha256)`. Calling the same
    /// transfer again must produce the same value for resume to work.
    pub transfer_id: String,
    /// The proposed file name. The receiving side sanitizes it; the directory is the receiving
    /// side's choice.
    pub name: String,
    pub size: u64,
    /// Lowercase hex, 64 characters.
    pub sha256: String,
    #[serde(default)]
    pub overwrite: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TransferDone {
    /// The final path on the receiving side.
    pub written: String,
    pub bytes: u64,
    pub sha256: String,
    /// Whether an existing file was overwritten.
    pub replaced: bool,
    /// Where the original sits if it was overwritten.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullHead {
    /// The file's full size, not the size minus `offset`.
    pub size: u64,
    pub sha256: String,
}

#[zyris::capability(name = "peer_transfer", version = 1)]
pub trait PeerTransfer {
    /// The sending side announces. The receiving side pulls the bytes back with `pull` and returns
    /// the result as this call's reply.
    async fn push_offer(&self, offer: TransferOffer) -> zyris::Result<TransferDone>;

    /// The receiving side pulls bytes from the sending side. Here the **sending side is the
    /// callee**.
    ///
    /// `offset` is how many bytes have already been received. 0 to start from the beginning.
    #[zyris(uni_stream)]
    async fn pull(
        &self,
        transfer_id: String,
        offset: u64,
    ) -> zyris::Result<Streaming<PullHead, Chunk>>;
}

#[cfg(test)]
mod tests {
    use zyris::proto::Transfer;

    #[test]
    fn only_pull_is_a_stream() {
        let d = super::peer_transfer_capability();
        assert_eq!(d.name, "peer_transfer");
        assert_eq!(d.version, 1);
        assert_eq!(d.tool("push_offer").unwrap().transfer, Transfer::Unary);
        assert_eq!(d.tool("pull").unwrap().transfer, Transfer::UniStream);
        // Adding a tool grows the surface opened to the peer. This is where that gets caught.
        let mut names: Vec<_> = d.tools.iter().map(|t| t.name.as_str()).collect();
        names.sort();
        assert_eq!(names, ["pull", "push_offer"]);
    }
}
