//! Settling which rustls crypto provider the process uses.
//!
//! **A node that links this crate has two of them**, and rustls will not choose between them.
//! `iroh` brings `aws-lc-rs` by way of quinn; the websocket client a node talks to Attacca over
//! brings `ring`. iroh itself is fine either way — it builds its config from its own
//! `default_provider()` and never consults the process-wide one — but
//! `tokio_tungstenite::connect_async` does consult it, and with both features enabled rustls
//! cannot tell which was meant.
//!
//! It does not return an error. It panics:
//!
//! ```text
//! Could not automatically determine the process-level CryptoProvider from Rustls crate features.
//! Call CryptoProvider::install_default() before this point to select a provider manually, or
//! make sure exactly one of the 'aws-lc-rs' and 'ring' features is enabled.
//! ```
//!
//! So the node comes up, announces, binds a peer endpoint, prints a fingerprint — and dies on the
//! first connect to Attacca. Nothing before that moment hints at it, and no configuration reaches
//! it: the binary that links both providers is the one that has to pick.
//!
//! This is why the function is here rather than in each node. Pulling in peer transfer is what
//! creates the problem, so the crate that creates it is where the fix should be reachable from.

/// Makes this process's TLS choice explicit. Call it once, early in `main`, before anything
/// connects.
///
/// Uses whichever provider `iroh` selected, so one implementation serves both the QUIC side and
/// the websocket rather than two serving one each.
///
/// Calling it more than once is harmless — the first call wins and later ones are ignored, which
/// is the behaviour a `main` wants from something whose only job is to make sure a default exists.
pub fn install_default_provider() {
    // `install_default` takes the provider by value and returns the installed one back as an error
    // if there already is one. Both are dropped here: the point is that a provider is installed,
    // not that this call is the one that installed it.
    let _ = (*iroh::tls::default_provider()).clone().install_default();
}

#[cfg(test)]
mod tests {
    /// The panic this prevents happens on the first TLS connect, which a unit test has no way to
    /// reach — so the assertion is on the condition that decides it: whether rustls has a default
    /// to find at all. `None` is precisely the state that panics.
    #[test]
    fn a_default_provider_exists_afterwards() {
        super::install_default_provider();
        assert!(
            rustls::crypto::CryptoProvider::get_default().is_some(),
            "no process-wide provider, so the first websocket connect panics instead of connecting"
        );
    }

    /// A second call must not be a problem: a node may well call this and so may something it
    /// links, and neither should have to know about the other.
    #[test]
    fn calling_it_twice_is_not_a_problem() {
        super::install_default_provider();
        super::install_default_provider();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }
}
