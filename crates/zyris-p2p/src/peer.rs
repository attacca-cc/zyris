//! Dial and accept over an endpoint.

use std::time::Duration;

use crate::transport::{IrohTransport, ALPN};

#[derive(Debug, thiserror::Error)]
pub enum PeerError {
    #[error("could not connect: {0}")]
    Connect(String),
    #[error("could not open stream: {0}")]
    Stream(String),
    #[error("peer did not complete the zyris handshake within the deadline")]
    Timeout,
    #[error("peer negotiated ALPN {negotiated:?}, expected {expected:?}")]
    AlpnMismatch { negotiated: Vec<u8>, expected: &'static [u8] },
}

pub async fn dial(
    endpoint: &iroh::Endpoint,
    addr: iroh::EndpointAddr,
) -> Result<IrohTransport, PeerError> {
    let conn =
        endpoint.connect(addr, ALPN).await.map_err(|e| PeerError::Connect(e.to_string()))?;
    let (send, recv) = conn.open_bi().await.map_err(|e| PeerError::Stream(e.to_string()))?;
    Ok(IrohTransport::new(endpoint.clone(), conn, send, recv))
}

/// A connection iroh has accepted, before the zyris-level handshake has happened. Waiting for
/// one of these to arrive (`accept_next`) never blocks on anything the remote peer controls —
/// only turning it into a usable connection (`establish`) can, which is why the two are split.
///
/// Deliberately not named `Accepting` — `iroh::endpoint` already has a type by that name (the
/// result of `Incoming::accept()`, a different stage of the same handshake), and this crate
/// re-exports enough of `iroh` that the two would otherwise be genuinely ambiguous to a reader.
///
/// Dropping one of these without ever calling [`establish`] on it is a valid, deliberate way to
/// decline a connection (e.g. shedding load, or shutting down): iroh's own `Incoming::drop`
/// sends an explicit refusal to the peer rather than leaving the attempt to linger
/// (`noq-1.1.1/src/incoming.rs:111`, "Implicit reject, similar to Connection's implicit close").
/// See `dropping_a_pending_connection_without_establishing_rejects_the_peer` in
/// `tests/loopback.rs` for the peer-observable proof.
pub struct PendingConnection {
    incoming: iroh::endpoint::Incoming,
    endpoint: iroh::Endpoint,
}

/// Waits for iroh's next incoming connection attempt and returns immediately once one arrives.
///
/// Deliberately does **not** wait for the QUIC/TLS handshake to finish and does **not** wait for
/// the peer to open the zyris bi-stream — both of those depend on the remote actually
/// cooperating, and a peer that connects and then goes silent can stall either one
/// indefinitely. If either wait lived here, one such peer would wedge this function forever and
/// every other connection waiting behind it in an accept loop would never get a turn. Call
/// [`establish`] on the result — with its own deadline, and spawned per connection rather than
/// awaited inline — to do the part that can actually block on the peer.
pub async fn accept_next(endpoint: &iroh::Endpoint) -> Option<PendingConnection> {
    let incoming = endpoint.accept().await?;
    Some(PendingConnection { incoming, endpoint: endpoint.clone() })
}

/// Completes the QUIC handshake and opens the zyris bi-stream for a connection `accept_next`
/// handed back, bounded by `deadline` end to end. A peer that never finishes the handshake or
/// never opens a stream gets exactly `deadline` before this returns `Err(PeerError::Timeout)`,
/// at which point the caller drops everything this held, tearing down the QUIC connection.
///
/// **This deadline alone does not close the whole hole.** It stops once this function returns
/// `Ok` — a peer that opens the bi-stream, writes one byte, and then stalls satisfies it and
/// still leaves the caller with a live, unbounded read ahead of it (typically `Node::accept`'s
/// `read_handshake`, whose own heartbeat watchdog does not start until *after* that first read
/// succeeds). What actually closes that gap is one layer down: the returned `IrohTransport`'s
/// first read carries its own `FIRST_MESSAGE_DEADLINE` (see `transport.rs`), so a peer that goes
/// quiet at any point up through its first real message cannot pin a task regardless of what
/// this function's own `deadline` was set to.
pub async fn establish(
    accepting: PendingConnection,
    deadline: Duration,
) -> Result<(iroh::EndpointId, IrohTransport), PeerError> {
    tokio::time::timeout(deadline, establish_inner(accepting))
        .await
        .map_err(|_| PeerError::Timeout)?
}

async fn establish_inner(
    accepting: PendingConnection,
) -> Result<(iroh::EndpointId, IrohTransport), PeerError> {
    let PendingConnection { incoming, endpoint } = accepting;
    let conn = incoming.await.map_err(|e| PeerError::Connect(e.to_string()))?;
    // `endpoint.accept()` matches *any* ALPN the endpoint was configured with — if the same
    // `iroh::Endpoint` is ever shared with another protocol, a peer that negotiated that other
    // ALPN would otherwise sail straight through as if it were speaking zyris. Checked before
    // `accept_bi()` so a mismatched peer is rejected without also having to open a stream first.
    if conn.alpn() != ALPN {
        return Err(PeerError::AlpnMismatch {
            negotiated: conn.alpn().to_vec(),
            expected: ALPN,
        });
    }
    // `Connection::remote_id` on an established connection returns the `EndpointId` directly
    // (no `Result`) — only the zero-RTT connection states return `Result<EndpointId, _>`.
    let peer = conn.remote_id();
    let (send, recv) = conn.accept_bi().await.map_err(|e| PeerError::Stream(e.to_string()))?;
    Ok((peer, IrohTransport::new(endpoint, conn, send, recv)))
}
