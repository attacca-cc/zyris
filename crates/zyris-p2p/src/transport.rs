//! Wraps an iroh QUIC bi-stream as a zyris [`Transport`].
//!
//! Only ever one bi-stream. zyris already multiplexes at its own layer and paces with credit,
//! so there is no reason to open a second QUIC stream.

use async_trait::async_trait;
use iroh::endpoint::{ReadExactError, RecvStream, SendStream};
use zyris::transport::{Transport, WireSink, WireStream};
use zyris::TransportError;
use zyris_proto::WireMessage;

use crate::frame::{body, decode_header, encode};

pub const ALPN: &[u8] = b"zyris/1";

/// `iroh::Endpoint` is `Arc`-backed and cheap to clone, but a `Connection` does **not** keep its
/// owning `Endpoint` alive — dropping the last clone of the `Endpoint` aborts its socket
/// ungracefully out from under every `Connection` it produced, even ones still in active use.
/// Holding a clone here for as long as the transport lives is what stops a caller who drops
/// its own `Endpoint` handle (e.g. because it went out of scope after `dial`/`accept_next`
/// returned) from silently losing an otherwise-healthy connection.
pub struct IrohTransport {
    endpoint: iroh::Endpoint,
    conn: iroh::endpoint::Connection,
    send: SendStream,
    recv: RecvStream,
}

impl IrohTransport {
    pub fn new(
        endpoint: iroh::Endpoint,
        conn: iroh::endpoint::Connection,
        send: SendStream,
        recv: RecvStream,
    ) -> IrohTransport {
        IrohTransport { endpoint, conn, send, recv }
    }
}

pub struct IrohSink {
    // Never read directly — see the comment on `IrohTransport` for why it has to be here at
    // all. Prefixed with `_` so the dead-code lint doesn't flag it.
    _endpoint: iroh::Endpoint,
    conn: iroh::endpoint::Connection,
    send: SendStream,
}

pub struct IrohRead {
    _endpoint: iroh::Endpoint,
    recv: RecvStream,
}

#[async_trait]
impl WireSink for IrohSink {
    async fn send(&mut self, msg: WireMessage) -> Result<(), TransportError> {
        self.send
            .write_all(&encode(&msg))
            .await
            .map_err(|e| TransportError::Io(e.to_string()))
    }

    async fn close(&mut self, code: u16, reason: String) -> Result<(), TransportError> {
        // Deliberately does not call `Endpoint::close()`. The endpoint is shared — a node can
        // accept many connections over the same endpoint — so closing it here would tear down
        // every other connection using it, not just this one. `Connection::close` below is
        // scoped correctly: it ends this connection and this connection only. Dropping this
        // sink's `_endpoint` clone afterwards is harmless as long as at least one other clone
        // (the caller's original, or another connection's) is still alive somewhere.
        let _ = self.send.finish();
        self.conn.close((code as u32).into(), reason.as_bytes());
        Ok(())
    }
}

#[async_trait]
impl WireStream for IrohRead {
    async fn next(&mut self) -> Option<Result<WireMessage, TransportError>> {
        let mut header = [0u8; 5];
        match self.recv.read_exact(&mut header).await {
            Ok(()) => {}
            // Zero bytes arrived before the peer ended the stream: nothing was cut off
            // mid-frame, so this is a clean end of traffic, not a fault. Surfacing it as an
            // error would make every ordinary disconnect noisy.
            Err(ReadExactError::FinishedEarly(0)) => return None,
            Err(e) => return Some(Err(TransportError::Io(e.to_string()))),
        }
        let (kind, len) = match decode_header(&header) {
            Ok(v) => v,
            Err(e) => return Some(Err(TransportError::Io(e.to_string()))),
        };
        let mut payload = vec![0u8; len];
        if let Err(e) = self.recv.read_exact(&mut payload).await {
            return Some(Err(TransportError::Io(e.to_string())));
        }
        Some(body(kind, payload).map_err(|e| TransportError::Io(e.to_string())))
    }
}

impl Transport for IrohTransport {
    fn split(self: Box<Self>) -> (Box<dyn WireSink>, Box<dyn WireStream>) {
        (
            Box::new(IrohSink {
                _endpoint: self.endpoint.clone(),
                conn: self.conn,
                send: self.send,
            }),
            Box::new(IrohRead { _endpoint: self.endpoint, recv: self.recv }),
        )
    }
}
