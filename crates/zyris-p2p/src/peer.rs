//! Dial and accept over an endpoint.

use crate::transport::{IrohTransport, ALPN};

#[derive(Debug, thiserror::Error)]
pub enum PeerError {
    #[error("could not connect: {0}")]
    Connect(String),
    #[error("could not open stream: {0}")]
    Stream(String),
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

pub async fn accept_next(
    endpoint: &iroh::Endpoint,
) -> Option<Result<(iroh::EndpointId, IrohTransport), PeerError>> {
    let incoming = endpoint.accept().await?;
    let conn = match incoming.await {
        Ok(c) => c,
        Err(e) => return Some(Err(PeerError::Connect(e.to_string()))),
    };
    // `Connection::remote_id` on an established connection returns the `EndpointId` directly
    // (no `Result`) — only the zero-RTT connection states return `Result<EndpointId, _>`.
    let peer = conn.remote_id();
    let (send, recv) = match conn.accept_bi().await {
        Ok(v) => v,
        Err(e) => return Some(Err(PeerError::Stream(e.to_string()))),
    };
    Some(Ok((peer, IrohTransport::new(endpoint.clone(), conn, send, recv))))
}
