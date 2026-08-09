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

/// How long a freshly-split [`IrohRead`] waits for its *first* message before giving up.
/// Applies once per stream, to the first call to [`WireStream::next`] only — not to every
/// read. This closes a gap one layer above `accept_bi()`'s own deadline: a peer can satisfy
/// `peer::establish`'s deadline by opening the bi-stream and writing a single byte, then stall
/// forever, and nothing bounds `Node::accept`'s subsequent read of that peer's `Hello` — its own
/// heartbeat watchdog does not start until *after* that read succeeds. A peer that goes quiet at
/// any point up through its first real message cannot pin a task this way, no matter which
/// caller (`dial` or `establish`) produced this transport. Once a first message has arrived, an
/// established connection's own heartbeat (zyris's, layered above this transport) is what
/// detects a peer that goes silent later — this deadline does not re-apply to every read.
///
/// **Must stay well under zyris's own heartbeat interval (20s, `HeartbeatConfig::default` in
/// `zyris-proto/src/envelope.rs`).** The two are coupled only by convention — nothing enforces
/// it — but if this ever grew to meet or exceed the heartbeat interval, an idle-but-healthy
/// connection legitimately waiting for its *first* message after a longer-than-usual pause
/// could be killed by this deadline before the heartbeat ever gets a say. `first_read` (below)
/// is what keeps this deadline from ever applying past the first read; if the heartbeat
/// interval is ever retuned, re-check this constant against it too.
const FIRST_MESSAGE_DEADLINE: Duration = Duration::from_secs(10);

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
    pub(crate) fn new(
        endpoint: iroh::Endpoint,
        conn: iroh::endpoint::Connection,
        send: SendStream,
        recv: RecvStream,
    ) -> IrohTransport {
        IrohTransport { endpoint, conn, send, recv }
    }
}

pub(crate) struct IrohSink {
    // Never read directly — see the comment on `IrohTransport` for why it has to be here at
    // all. Prefixed with `_` so the dead-code lint doesn't flag it.
    _endpoint: iroh::Endpoint,
    conn: iroh::endpoint::Connection,
    send: SendStream,
}

pub(crate) struct IrohRead {
    _endpoint: iroh::Endpoint,
    recv: RecvStream,
    /// Consumed by the first call to `next`, regardless of outcome — see
    /// `FIRST_MESSAGE_DEADLINE`.
    first_read: bool,
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
        if self.first_read {
            self.first_read = false;
            return match tokio::time::timeout(FIRST_MESSAGE_DEADLINE, self.read_one()).await {
                Ok(result) => result,
                Err(_) => Some(Err(TransportError::Io(format!(
                    "peer sent nothing usable within the {FIRST_MESSAGE_DEADLINE:?} \
                     first-message deadline"
                )))),
            };
        }
        self.read_one().await
    }
}

impl IrohRead {
    async fn read_one(&mut self) -> Option<Result<WireMessage, TransportError>> {
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
        // Bytes the peer actually delivers here are real, resident memory — unlike a bare
        // declared `len` (see `frame.rs`'s module doc), `read_exact` writes into this buffer as
        // data arrives, so a peer that sends most of a large frame and then stalls mid-body
        // holds that memory for as long as the read is in flight. `FIRST_MESSAGE_DEADLINE`
        // bounds that for the first read on a stream; an established connection's heartbeat
        // bounds it afterward.
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
            Box::new(IrohRead { _endpoint: self.endpoint, recv: self.recv, first_read: true }),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use zyris::transport::Transport;
    use zyris_proto::WireMessage;

    use crate::peer::{accept_next, dial, establish};
    use crate::transport::ALPN;

    /// Regression for F6: replacing `self.conn.close(...)` in `IrohSink::close` with a no-op
    /// left every other test in this crate green — `finish()` alone still ends the sender's
    /// send stream cleanly, and message delivery still works, so nothing else in the suite
    /// noticed. This is a unit test (not an integration test under `tests/`) specifically
    /// because it needs an independent handle to the *peer's own* `Connection` to check — the
    /// public API deliberately never exposes one (see `IrohTransport`'s fields), so proving
    /// `Connection::close` genuinely ran requires reaching in from inside this module.
    /// `Connection` is documented as `Clone`-able to "obtain another handle to the same
    /// connection" (`iroh-1.0.3/src/endpoint/connection.rs:735`), which is what makes this
    /// possible without changing any public surface.
    ///
    /// B's `IrohTransport` is deliberately kept alive, unsplit, until after the assertion below
    /// — an earlier version of this test split it and read a message on B's side first, and
    /// that raced its own `IrohSink`'s conn/endpoint clones dropping (an *implicit* close, from
    /// the endpoint-lifetime fix earlier in this crate) against A's explicit `close()`, so B
    /// sometimes observed `LocallyClosed` from its own teardown instead of the `ApplicationClosed`
    /// A's `close()` actually sent. Never splitting B's side removes that race entirely.
    #[tokio::test]
    async fn close_actually_closes_the_quic_connection_not_just_the_stream() {
        tokio::time::timeout(Duration::from_secs(20), async {
            let b_key = iroh::SecretKey::generate();
            let b_ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
                .secret_key(b_key)
                .alpns(vec![ALPN.to_vec()])
                .relay_mode(iroh::RelayMode::Disabled)
                .bind()
                .await
                .unwrap();
            let b_addr = b_ep.addr();

            let b_task = tokio::spawn(async move {
                let pending = accept_next(&b_ep).await.unwrap();
                let (_peer, b_transport) =
                    establish(pending, Duration::from_secs(5)).await.unwrap();
                // Cloned, not split: the independent handle to the same connection that lets
                // the test watch it close from the outside, the way a real peer would, without
                // B's own `IrohSink`/`IrohRead` (and their conn/endpoint clones) being in play.
                let b_conn = b_transport.conn.clone();
                (b_transport, b_conn)
            });

            let a_ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
                .relay_mode(iroh::RelayMode::Disabled)
                .bind()
                .await
                .unwrap();
            let a_transport = dial(&a_ep, b_addr).await.unwrap();
            let (mut a_sink, _a_read) = Box::new(a_transport).split();

            // Unblocks B's `accept_bi()` inside `establish` — the dialer has to write first.
            a_sink.send(WireMessage::Text("one message".into())).await.unwrap();

            // Wait for B to have fully established before A closes: `b_transport` comes back
            // still unsplit, so nothing on B's side can race its own teardown against what
            // happens next.
            let (b_transport, b_conn) = b_task.await.unwrap();

            a_sink.close(7, "bye from a".into()).await.unwrap();

            // The actual check: B's *own* connection handle must observe the connection
            // closing, not merely time out waiting for something that never arrives. A no-op in
            // place of `conn.close()` leaves this pending forever (only `finish()` ran, which
            // ends the stream, not the connection), so this is the assertion, not a formality.
            let reason = tokio::time::timeout(Duration::from_secs(3), b_conn.closed())
                .await
                .expect(
                    "the peer's own Connection must observe an actual close, not hang \
                     waiting for one that a no-op `conn.close()` would never send",
                );
            assert!(
                matches!(reason, iroh::endpoint::ConnectionError::ApplicationClosed(_)),
                "expected the peer to see an application close (from `Connection::close`), \
                 got {reason:?}"
            );
            drop(b_transport);
        })
        .await
        .expect("test exceeded its deadline");
    }
}
