//! What this machine calls itself, for a node that would rather not be configured.
//!
//! A node's name is the thing an agent sees in `zyris__{slug}__{capability}__{tool}`, so leaving it
//! at a crate-wide default means every node on every machine announces the same one. The hostname is
//! the only identifier already true of the box, already meaningful to whoever set it, and already
//! distinct between machines — which is exactly the shape of a good default.

/// This machine's short name, or `None` when it has nothing usable to say.
///
/// `$HOSTNAME` wins over the kernel's answer. In a container the kernel hostname is a truncated
/// container id, while the orchestrator sets `$HOSTNAME` to the pod name — the one a human would
/// recognise on a node card.
///
/// The result is cut at the first `.`, so `laptop.local` and `build-01.internal.example` become
/// `laptop` and `build-01`. The domain half says where the machine is, not which machine it is, and
/// the slug that carries this into tool names only has 16 characters to spend.
pub fn machine_name() -> Option<String> {
    std::env::var("HOSTNAME")
        .ok()
        .and_then(|value| short_name(&value))
        .or_else(|| short_name(&gethostname::gethostname().to_string_lossy()))
}

fn short_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let short = trimmed.split('.').next().unwrap_or(trimmed).trim();
    (!short.is_empty()).then(|| short.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_domain_half_is_dropped() {
        assert_eq!(short_name("laptop.local").as_deref(), Some("laptop"));
        assert_eq!(short_name("build-01.internal.example").as_deref(), Some("build-01"));
        assert_eq!(short_name("  plain  ").as_deref(), Some("plain"));
    }

    /// An empty or dot-only hostname must read as "nothing to say" rather than as an empty name,
    /// which would otherwise be rejected downstream by node-name validation.
    #[test]
    fn nothing_usable_is_none() {
        assert_eq!(short_name(""), None);
        assert_eq!(short_name("   "), None);
        assert_eq!(short_name(".local"), None);
    }

    /// Whatever this machine is called, the answer has to be usable as a node name.
    #[test]
    fn this_machine_answers_with_something_usable_or_nothing() {
        if let Some(name) = machine_name() {
            assert!(!name.is_empty());
            assert!(!name.contains('.'));
            assert_eq!(name.trim(), name);
        }
    }
}
