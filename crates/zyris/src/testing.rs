use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::sync::mpsc;
use zyris_proto::WireMessage;

use crate::async_trait;
use crate::connection::{AcceptOptions, Connection};
use crate::error::{Result, TransportError};
use crate::node::Node;
use crate::transport::{ChannelTransport, Transport, WireSink, WireStream};

pub async fn duplex(dialer: &Node, acceptor: &Node) -> Result<(Connection, Connection)> {
    duplex_with(dialer, acceptor, AcceptOptions::default()).await
}

pub async fn duplex_with(
    dialer: &Node,
    acceptor: &Node,
    options: AcceptOptions,
) -> Result<(Connection, Connection)> {
    let (dial_transport, accept_transport) = ChannelTransport::pair();
    let accept = acceptor.accept(accept_transport, options);
    let dial = dialer.connect_over(dial_transport);
    tokio::try_join!(dial, accept)
}

/// The knobs a [`gated_transport_pair`] hands back, so a test can simulate the failure modes of a
/// real network: a peer that goes silent (frames dropped in one direction while the channel stays
/// open — the half-open condition the heartbeat exists to detect), or a peer whose socket dies on
/// the write side.
#[derive(Debug)]
pub struct Gates {
    /// When false, frames from the dialer never reach the acceptor — the dialer appears dead to it.
    pub dialer_to_acceptor: Arc<AtomicBool>,
    /// When false, frames from the acceptor never reach the dialer.
    pub acceptor_to_dialer: Arc<AtomicBool>,
    /// When true, the dialer's sink errors on its next write, as if its socket had died.
    pub dialer_sink_fails: Arc<AtomicBool>,
    /// When true, the acceptor's sink errors on its next write.
    pub acceptor_sink_fails: Arc<AtomicBool>,
    /// How many frames have been delivered end to end — lets a test assert that heartbeats are
    /// actually flowing on an otherwise idle connection.
    pub delivered: Arc<AtomicUsize>,
}

/// Like [`duplex`], but each direction runs through a gate the test can close, so one side can be
/// made to look silently dead without the channel underneath ever erroring.
pub async fn gated_duplex(
    dialer: &Node,
    acceptor: &Node,
    options: AcceptOptions,
) -> (Connection, Connection, Gates) {
    let (dial_transport, accept_transport, gates) = gated_transport_pair();
    let accept = acceptor.accept(accept_transport, options);
    let dial = dialer.connect_over(dial_transport);
    let (dial_conn, accept_conn) = tokio::join!(dial, accept);
    (
        dial_conn.expect("dialer handshake failed"),
        accept_conn.expect("acceptor handshake failed"),
        gates,
    )
}

/// The transports behind [`gated_duplex`], exposed for a caller that drives one end itself (e.g. a
/// server handler that is not a `Node`).
pub fn gated_transport_pair() -> (GatedTransport, GatedTransport, Gates) {
    let dialer_to_acceptor = Arc::new(AtomicBool::new(true));
    let acceptor_to_dialer = Arc::new(AtomicBool::new(true));
    let dialer_sink_fails = Arc::new(AtomicBool::new(false));
    let acceptor_sink_fails = Arc::new(AtomicBool::new(false));
    let delivered = Arc::new(AtomicUsize::new(0));

    // Dialer writes → acceptor reads, through a forwarder that drops frames while its gate is shut.
    let (dialer_tx, dialer_out) = mpsc::unbounded_channel();
    let (accept_rx_tx, accept_rx) = mpsc::unbounded_channel();
    forward(dialer_out, accept_rx_tx, dialer_to_acceptor.clone(), delivered.clone());

    // Acceptor writes → dialer reads.
    let (acceptor_tx, acceptor_out) = mpsc::unbounded_channel();
    let (dialer_rx_tx, dialer_rx) = mpsc::unbounded_channel();
    forward(acceptor_out, dialer_rx_tx, acceptor_to_dialer.clone(), delivered.clone());

    let gates = Gates {
        dialer_to_acceptor,
        acceptor_to_dialer,
        dialer_sink_fails,
        acceptor_sink_fails,
        delivered,
    };
    (
        GatedTransport {
            sink_tx: dialer_tx,
            sink_fails: gates.dialer_sink_fails.clone(),
            stream_rx: dialer_rx,
        },
        GatedTransport {
            sink_tx: acceptor_tx,
            sink_fails: gates.acceptor_sink_fails.clone(),
            stream_rx: accept_rx,
        },
        gates,
    )
}

fn forward(
    mut from: mpsc::UnboundedReceiver<WireMessage>,
    to: mpsc::UnboundedSender<WireMessage>,
    gate: Arc<AtomicBool>,
    delivered: Arc<AtomicUsize>,
) {
    tokio::spawn(async move {
        while let Some(msg) = from.recv().await {
            if gate.load(Ordering::Relaxed) {
                if to.send(msg).is_ok() {
                    delivered.fetch_add(1, Ordering::Relaxed);
                }
            }
            // Gated frames are dropped, like packets into a black hole: the sender believes they
            // were delivered and the receiver simply never sees them.
        }
    });
}

/// One half of a [`gated_transport_pair`]. Writing queues a frame toward the peer; reading yields
/// what the peer wrote. Whether a frame actually crosses depends on the gates in [`Gates`].
pub struct GatedTransport {
    sink_tx: mpsc::UnboundedSender<WireMessage>,
    sink_fails: Arc<AtomicBool>,
    stream_rx: mpsc::UnboundedReceiver<WireMessage>,
}

struct GateSink {
    tx: mpsc::UnboundedSender<WireMessage>,
    fails: Arc<AtomicBool>,
}

struct GateStream(mpsc::UnboundedReceiver<WireMessage>);

#[async_trait]
impl WireSink for GateSink {
    async fn send(&mut self, msg: WireMessage) -> Result<(), TransportError> {
        if self.fails.load(Ordering::Relaxed) {
            return Err(TransportError::Io("simulated dead socket".into()));
        }
        self.tx.send(msg).map_err(|_| TransportError::Closed)
    }

    async fn close(&mut self, _code: u16, _reason: String) -> Result<(), TransportError> {
        Ok(())
    }
}

#[async_trait]
impl WireStream for GateStream {
    async fn next(&mut self) -> Option<Result<WireMessage, TransportError>> {
        self.0.recv().await.map(Ok)
    }
}

impl Transport for GatedTransport {
    fn split(self: Box<Self>) -> (Box<dyn WireSink>, Box<dyn WireStream>) {
        (
            Box::new(GateSink { tx: self.sink_tx, fails: self.sink_fails }),
            Box::new(GateStream(self.stream_rx)),
        )
    }
}
