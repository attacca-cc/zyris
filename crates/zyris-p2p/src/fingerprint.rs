//! A human-comparable rendering of an `EndpointId`, and the hook a node uses to have a person
//! confirm one before it gets pinned.
//!
//! `tofu.rs` pins on first use, which catches a substitution *after* it already happened once.
//! What it cannot do on its own is stop the *first* connection to an unknown peer from being
//! whoever attacca's rendezvous handed back — including a peer attacca itself introduced under
//! a false name. Closing that gap needs an anchor attacca has no channel into at all: a person,
//! reading a fingerprint out loud or over a second channel, the way SSH host keys and Signal
//! safety numbers work. This module is that comparison and that hook; `TofuStore::authorize`
//! (in `tofu.rs`) is what wires the two together with the ledger.

use sha2::{Digest, Sha256};

/// Bytes of the SHA-256 digest kept. **Do not shorten this.** An attacker who wants a
/// fingerprint collision does not have to break SHA-256 — they generate keypairs freely until
/// some `EndpointId` hashes to a fingerprint a person will mistake for the real one. That is a
/// second-preimage search, and its cost is `2^(bits)`, not the security level of the hash
/// underneath. At 64 bits (8 bytes) that search is `2^64`, which is inside range for grinding
/// on rented hardware; at 128 bits it is not. The 4-byte-per-group, 8-group rendering below
/// exists to make 128 bits of hex actually readable side by side, not to make it shorter.
const FINGERPRINT_BYTES: usize = 16;

/// Renders `endpoint_id` as a 128-bit fingerprint a person can read aloud or compare
/// side-by-side with what the peer displays on its own end — `9F2A 41C7 0E83 BB15 6D04 A97E
/// 22C1 5FB8`, groups of two bytes so a mistyped or misheard group is easy to isolate.
///
/// **This is a display value only. Nothing in this crate pins a fingerprint.** `TofuStore`
/// pins the full `endpoint_id` string it was given — truncating to a fingerprint and pinning
/// *that* would silently throw away the collision resistance the 128 bits above are paying for
/// and pin something an attacker only needs to match in 16 bytes, not however long the real
/// `EndpointId` is. The fingerprint exists solely so a human has something short enough to
/// compare; the ledger never sees it.
pub fn fingerprint(endpoint_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(endpoint_id.as_bytes());
    let digest = hasher.finalize();

    digest[..FINGERPRINT_BYTES]
        .chunks(2)
        .map(|pair| format!("{:02X}{:02X}", pair[0], pair[1]))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A node's hook for having a person confirm an unknown peer's fingerprint before
/// `TofuStore::authorize` pins it. `label` is the caller-chosen `peer_slug` — the name the
/// person picked for this peer, not anything attacca issued (see `tofu.rs`'s module docs for
/// why that distinction matters). Returns `true` to accept and pin, `false` to refuse.
///
/// Implementations decide *how* to ask — a terminal prompt, a UI dialog, whatever the node
/// has. `zyris-p2p` only defines the hook and one fail-closed default (`DenyUnknown`); it does
/// not decide policy for nodes that have no person to ask at all. See that struct's docs.
#[async_trait::async_trait]
pub trait PeerConfirmer: Send + Sync {
    async fn confirm(&self, label: &str, fingerprint: &str) -> bool;
}

/// A [`PeerConfirmer`] that refuses every unknown peer, unconditionally.
///
/// For a node with no person to ask — `zyris-daemon` running headless is the motivating case —
/// asking is not an option, so the only choice left is which way to fail. This fails closed:
/// an unknown peer is never trusted just because nobody was around to say no. A node that needs
/// to *pre*-approve a peer without a live human (e.g. an operator drops a fingerprint into a
/// config file ahead of time) has to build that as its own `PeerConfirmer` that consults
/// whatever it pre-approved and answers synchronously — `zyris-p2p` deliberately does not ship
/// that policy itself, since "what counts as pre-approved" is a decision for the node, not the
/// transport.
pub struct DenyUnknown;

#[async_trait::async_trait]
impl PeerConfirmer for DenyUnknown {
    async fn confirm(&self, _label: &str, _fingerprint: &str) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_128_bits() {
        let fp = fingerprint("some-endpoint-id");
        let hex_chars: String = fp.chars().filter(|c| !c.is_whitespace()).collect();
        assert_eq!(hex_chars.len(), 32, "128 bits is 32 hex chars, got {fp:?}");
        assert!(hex_chars.chars().all(|c| c.is_ascii_hexdigit()), "not all hex: {fp:?}");
    }

    #[test]
    fn the_same_endpoint_id_always_yields_the_same_fingerprint() {
        assert_eq!(fingerprint("abc123"), fingerprint("abc123"));
    }

    #[test]
    fn a_single_changed_character_yields_a_different_fingerprint() {
        // Not a full avalanche-effect proof (that is SHA-256's job, not this test's) — just
        // confirming this function does not, say, truncate or normalize its input in a way
        // that would make two distinct endpoint ids collide trivially.
        assert_ne!(fingerprint("abc123"), fingerprint("abc124"));
    }

    #[tokio::test]
    async fn deny_unknown_always_refuses() {
        assert!(!DenyUnknown.confirm("kitchen-pi", &fingerprint("key-1")).await);
        assert!(!DenyUnknown.confirm("anything", &fingerprint("key-2")).await);
    }
}
