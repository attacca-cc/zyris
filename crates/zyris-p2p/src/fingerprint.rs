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

use std::str::FromStr;

use sha2::{Digest, Sha256};

/// Bytes of the SHA-256 digest kept. **Do not shorten this.** An attacker who wants a
/// fingerprint collision does not have to break SHA-256 — they generate keypairs freely until
/// some `EndpointId` hashes to a fingerprint a person will mistake for the real one. That is a
/// second-preimage search, and its cost is `2^(bits)`, not the security level of the hash
/// underneath. At 64 bits (8 bytes) that search is `2^64`, which is inside range for grinding
/// on rented hardware; at 128 bits it is not. The 4-byte-per-group, 8-group rendering below
/// exists to make 128 bits of hex actually readable side by side, not to make it shorter.
const FINGERPRINT_BYTES: usize = 16;

// The rendering below pairs bytes up two at a time (`chunks(2)`, each pair formatted as one
// 4-hex-digit group). An odd `FINGERPRINT_BYTES` would leave a trailing chunk of length 1 and
// panic on `pair[1]` the first time this function ever runs. Caught here, at compile time,
// instead of by whatever test happens to exercise that code path first.
const _: () = assert!(
    FINGERPRINT_BYTES.is_multiple_of(2),
    "FINGERPRINT_BYTES must be even to pair into hex groups"
);

/// A string that was supposed to be an `EndpointId` but does not parse as one.
///
/// Returned instead of a fingerprint — never as a fingerprint of the raw string. Hashing
/// whatever bytes a string happens to contain would let two different *spellings* of the same
/// key — `iroh::EndpointId::from_str` accepts both lowercase hex and RFC 4648 base32 — render
/// as two different fingerprints, which is the "same key, two fingerprints" failure that
/// trains a person to stop trusting, and stop reading, this check. Parsing first means the
/// fingerprint is always over the actual 32 key bytes, however the caller happened to spell
/// them.
#[derive(Debug, thiserror::Error)]
#[error("{endpoint_id:?} is not a valid endpoint id: {reason}")]
pub struct InvalidEndpointId {
    endpoint_id: String,
    reason: String,
}

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
///
/// Parses `endpoint_id` first and hashes the resulting 32 key bytes, not the string — see
/// [`InvalidEndpointId`] for why. `iroh::EndpointId::from_str` accepts both lowercase hex and
/// RFC 4648 base32, so this also normalizes two valid spellings of the same key to the same
/// fingerprint (see `two_spellings_of_the_same_key_yield_the_same_fingerprint` below).
pub fn fingerprint(endpoint_id: &str) -> Result<String, InvalidEndpointId> {
    let key = iroh::EndpointId::from_str(endpoint_id).map_err(|e| InvalidEndpointId {
        endpoint_id: endpoint_id.to_string(),
        reason: e.to_string(),
    })?;

    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    let digest = hasher.finalize();

    Ok(digest[..FINGERPRINT_BYTES]
        .chunks(2)
        .map(|pair| format!("{:02X}{:02X}", pair[0], pair[1]))
        .collect::<Vec<_>>()
        .join(" "))
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

    /// A fixed key with its expected rendering, asserted byte for byte — pins the actual
    /// convention (SHA-256, first 16 bytes, 2-byte groups, uppercase, space-separated) so a
    /// later refactor of `fingerprint` cannot silently change what a person is asked to
    /// compare. `ENDPOINT_ID` is `iroh::SecretKey::from_bytes(&[0u8; 32]).public().to_string()`
    /// — deterministic because ed25519 key derivation from a fixed seed always produces the
    /// same key. `EXPECTED` was computed independently (SHA-256 of that key's raw 32 bytes,
    /// first 16 bytes, hex) and is not derived by calling this module's own code.
    #[test]
    fn a_known_key_renders_to_its_known_fingerprint() {
        const ENDPOINT_ID: &str =
            "3b6a27bcceb6a42d62a3a8d02a6f0d73653215771de243a63ac048a18b59da29";
        const EXPECTED: &str = "139E 3940 E64B 5491 7220 88D9 A0D7 4162";
        assert_eq!(fingerprint(ENDPOINT_ID).unwrap(), EXPECTED);
    }

    #[test]
    fn fingerprint_is_128_bits() {
        let key = iroh::SecretKey::generate().public().to_string();
        let fp = fingerprint(&key).unwrap();
        let hex_chars: String = fp.chars().filter(|c| !c.is_whitespace()).collect();
        assert_eq!(hex_chars.len(), 32, "128 bits is 32 hex chars, got {fp:?}");
        assert!(hex_chars.chars().all(|c| c.is_ascii_hexdigit()), "not all hex: {fp:?}");
    }

    #[test]
    fn the_same_endpoint_id_always_yields_the_same_fingerprint() {
        let key = iroh::SecretKey::generate().public().to_string();
        assert_eq!(fingerprint(&key).unwrap(), fingerprint(&key).unwrap());
    }

    /// Minimal RFC 4648 base32 (no padding) encoder — just enough to construct a second, valid
    /// spelling of an `EndpointId` for the test below, matching what
    /// `iroh::EndpointId::from_str`'s non-hex branch actually decodes
    /// (`decode_base32_hex` in `iroh-base`, which uppercases before decoding, so the
    /// alphabet's case here does not matter to parsing). Not general-purpose — self-contained
    /// on purpose, to avoid pulling in `data_encoding` as a dependency for one test.
    fn base32_nopad(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
        let mut out = String::new();
        let mut buf: u64 = 0;
        let mut bits = 0u32;
        for &b in bytes {
            buf = (buf << 8) | b as u64;
            bits += 8;
            while bits >= 5 {
                bits -= 5;
                out.push(ALPHABET[((buf >> bits) & 0x1f) as usize] as char);
            }
        }
        if bits > 0 {
            out.push(ALPHABET[((buf << (5 - bits)) & 0x1f) as usize] as char);
        }
        out
    }

    #[test]
    fn two_spellings_of_the_same_key_yield_the_same_fingerprint() {
        // The whole point of parsing before hashing: hex and base32 are two encodings
        // `iroh::EndpointId::from_str` both accept for the identical 32 key bytes. If
        // `fingerprint` ever went back to hashing the string instead of the parsed key, this
        // is the test that would catch it.
        let key = iroh::SecretKey::generate().public();
        let hex = key.to_string();
        let base32 = base32_nopad(key.as_bytes());
        // Sanity check on the test's own setup: confirm this really is a second, independently
        // parseable spelling of the same key, not a string that happens to pass some other way.
        assert_eq!(base32.parse::<iroh::EndpointId>().unwrap(), key, "test setup: base32 did not round-trip to the same key");
        assert_eq!(fingerprint(&hex).unwrap(), fingerprint(&base32).unwrap());
    }

    #[test]
    fn different_keys_yield_different_fingerprints() {
        let a = iroh::SecretKey::generate().public().to_string();
        let b = iroh::SecretKey::generate().public().to_string();
        assert_ne!(fingerprint(&a).unwrap(), fingerprint(&b).unwrap());
    }

    #[test]
    fn an_unparseable_endpoint_id_is_an_error_not_a_fingerprint() {
        // A person must never be shown a fingerprint rendered from something that was not
        // even a valid key — that would be comparing against noise, not a promise.
        assert!(fingerprint("not a real endpoint id").is_err());
    }

    #[tokio::test]
    async fn deny_unknown_always_refuses() {
        // The fingerprint argument's exact value never matters to `DenyUnknown` — it refuses
        // unconditionally — so a plain placeholder is enough here.
        assert!(!DenyUnknown.confirm("kitchen-pi", "9F2A 41C7 0E83 BB15 6D04 A97E 22C1 5FB8").await);
        assert!(!DenyUnknown.confirm("anything", "does not matter").await);
    }
}
