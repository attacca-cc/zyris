//! Wraps an iroh QUIC bi-stream as a zyris [`Transport`].
//!
//! Only ever one bi-stream. zyris already multiplexes at its own layer and paces with credit,
//! so there is no reason to open a second QUIC stream.

use std::time::Duration;

use async_trait::async_trait;
use iroh::endpoint::{RecvStream, SendStream};
use zyris::transport::{Transport, WireSink, WireStream};
use zyris::TransportError;
use zyris_proto::WireMessage;

use crate::frame::{body, decode_header, encode};

pub const ALPN: &[u8] = b"zyris/1";

/// How long `close` waits for the peer to acknowledge the last of the buffered stream data
/// before tearing down the connection anyway. Bounds what would otherwise be an unbounded wait
/// on an unresponsive peer; on a healthy connection this resolves in well under a
/// round-trip-time, since it is only waiting for a QUIC ACK.
const CLOSE_STOPPED_WAIT: Duration = Duration::from_secs(5);

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
        //
        // Ordering matters here and used to be wrong: `finish()` only requests that no more
        // data be sent — it does not wait for what is already buffered to actually reach the
        // peer. Calling `Connection::close` immediately after `finish()` discards whatever
        // hadn't been acknowledged yet, which on a real transfer is not a hypothetical: it
        // dropped an entire in-flight frame in testing. `stopped()` resolves once the peer has
        // acknowledged receipt of everything the stream sent (`Ok(None)`) or told us it gave up
        // early (`Ok(Some(code))`); either way, once it resolves there is nothing further this
        // stream owes the peer, and closing the connection is safe. Bounded by
        // `CLOSE_STOPPED_WAIT` so a peer that never acknowledges can't hang our own shutdown.
        if self.send.finish().is_ok() {
            let _ = tokio::time::timeout(CLOSE_STOPPED_WAIT, self.send.stopped()).await;
        }
        self.conn.close((code as u32).into(), reason.as_bytes());
        Ok(())
    }
}

#[async_trait]
impl WireStream for IrohRead {
    async fn next(&mut self) -> Option<Result<WireMessage, TransportError>> {
        let mut header = [0u8; 5];
        // No "clean end of stream" branch here on purpose. An earlier version tried to treat a
        // zero-byte `FinishedEarly` specially so an ordinary disconnect wouldn't look like an
        // error, but `close` above always follows `finish()` with `Connection::close`, so a
        // peer hanging up through this code never produces a clean stream-EOF on this side —
        // it always shows up as a connection-level error instead, and that branch was dead.
        // It also bought nothing even where reachable: `run_reader` in `zyris/src/connection.rs`
        // maps a bare `None` and `Some(Err(_))` to the same `CloseReason::Transport` either way.
        if let Err(e) = self.recv.read_exact(&mut header).await {
            return Some(Err(TransportError::Io(e.to_string())));
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
