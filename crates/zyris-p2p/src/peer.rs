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
pub struct Accepting {
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
pub async fn accept_next(endpoint: &iroh::Endpoint) -> Option<Accepting> {
    let incoming = endpoint.accept().await?;
    Some(Accepting { incoming, endpoint: endpoint.clone() })
}

/// Completes the QUIC handshake and opens the zyris bi-stream for a connection `accept_next`
/// handed back, bounded by `deadline` end to end. A peer that never finishes the handshake or
/// never opens a stream gets exactly `deadline` before this gives up on it — the caller can
/// then drop everything this held (which tears down the QUIC connection) instead of leaking a
/// task on an attacker who just holds the socket open.
pub async fn establish(
    accepting: Accepting,
    deadline: Duration,
) -> Result<(iroh::EndpointId, IrohTransport), PeerError> {
    tokio::time::timeout(deadline, establish_inner(accepting))
        .await
        .map_err(|_| PeerError::Timeout)?
}

async fn establish_inner(
    accepting: Accepting,
) -> Result<(iroh::EndpointId, IrohTransport), PeerError> {
    let Accepting { incoming, endpoint } = accepting;
    let conn = incoming.await.map_err(|e| PeerError::Connect(e.to_string()))?;
    // `Connection::remote_id` on an established connection returns the `EndpointId` directly
    // (no `Result`) — only the zero-RTT connection states return `Result<EndpointId, _>`.
    let peer = conn.remote_id();
    let (send, recv) = conn.accept_bi().await.map_err(|e| PeerError::Stream(e.to_string()))?;
    Ok((peer, IrohTransport::new(endpoint, conn, send, recv)))
}
