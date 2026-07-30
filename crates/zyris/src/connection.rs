use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use tokio::sync::{mpsc, oneshot, watch, Notify};
use zyris_proto::{
    attach, decode_binary, decode_text, encode_control, encode_stream_data, method_name,
    split_method, AckProtocol, AnnounceParams, AnnounceResult, AttachmentRef, CapabilityDescriptor,
    AttachmentTrailer, ClosingParams, Envelope, ErrorCode, HeartbeatConfig, Hello, HelloAck, HelloProtocol,
    IncomingFrame, Limits, Payload, RejectedCapability, Serialization, StreamDecl, WireError,
    WireMessage, CLOSE_NORMAL, CLOSE_UNSUPPORTED_VERSION, FEATURE_ATTACHMENTS, FEATURE_CANCEL,
    INLINE_BLOB_MAX, METHOD_ANNOUNCE, METHOD_CLOSING, PROTOCOL_MAJOR, PROTOCOL_MINOR,
};

use crate::capabilities::CapabilitySet;
use crate::error::{CloseReason, Result};
use crate::serve::{IncomingCall, Outgoing};
use crate::transport::{Transport, WireSink, WireStream};

/// How long an announced attachment waits to be claimed before the receiver cancels it. Longer
/// than any plausible gap between reading a response and deciding what to do with its bytes, short
/// enough that a caller which never looks does not pin the sender for the life of the connection.
const ATTACHMENT_CLAIM_GRACE: Duration = Duration::from_secs(120);

enum WriterCmd {
    Send(WireMessage),
    Close { code: u16, reason: String },
}

enum StreamEvent {
    Data(Bytes),
    End,
    Failed(WireError),
}

struct IncomingStreamEntry {
    tx: mpsc::UnboundedSender<StreamEvent>,
    next_seq: u32,
}

/// An attachment the peer announced in a payload, registered by the reader before that payload
/// reaches whoever asked for it. Holding the reference alongside the stream is what lets a claimer
/// check the size and digest it was promised against what actually arrives.
struct PendingAttachment {
    reference: AttachmentRef,
    rx: mpsc::UnboundedReceiver<StreamEvent>,
}

struct InflightCall {
    capability: String,
    abort: tokio::task::AbortHandle,
    replied: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    stream: Option<u32>,
}

struct CreditState {
    available: u64,
    canceled: bool,
}

pub(crate) struct CreditGate {
    state: Mutex<CreditState>,
    notify: Notify,
}

impl CreditGate {
    fn new(initial: u64) -> Arc<Self> {
        Arc::new(CreditGate {
            state: Mutex::new(CreditState { available: initial, canceled: false }),
            notify: Notify::new(),
        })
    }

    async fn acquire(&self, n: u64) -> Result<()> {
        loop {
            let notified = self.notify.notified();
            {
                let mut state = self.state.lock().unwrap();
                if state.canceled {
                    return Err(WireError::canceled());
                }
                if state.available >= n {
                    state.available -= n;
                    return Ok(());
                }
            }
            notified.await;
        }
    }

    fn add(&self, n: u64) {
        self.state.lock().unwrap().available += n;
        self.notify.notify_waiters();
    }

    fn cancel(&self) {
        self.state.lock().unwrap().canceled = true;
        self.notify.notify_waiters();
    }
}

#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    pub conn_id: String,
    pub node_id: String,
    pub resume_token: String,
    pub resumed: bool,
    pub serialization: Serialization,
    pub peer_agent: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Side {
    Dialer,
    Acceptor,
}

pub struct AcceptOptions {
    pub node_id: String,
    pub conn_id: String,
    pub resume_token: String,
    pub resumed: bool,
    pub limits: Limits,
    pub heartbeat: HeartbeatConfig,
    pub reserved_capabilities: Vec<String>,
    /// What this acceptor advertises it can do. Dropping [`FEATURE_ATTACHMENTS`] from the list
    /// keeps every blob inline in both directions on connections it accepts, which is the switch a
    /// deployment reaches for if it would rather bound a response by size than handle a stream.
    pub features: Vec<String>,
}

impl Default for AcceptOptions {
    fn default() -> Self {
        AcceptOptions {
            node_id: uuid::Uuid::new_v4().simple().to_string(),
            conn_id: uuid::Uuid::new_v4().simple().to_string(),
            resume_token: uuid::Uuid::new_v4().simple().to_string(),
            resumed: false,
            limits: Limits::default(),
            heartbeat: HeartbeatConfig::default(),
            reserved_capabilities: Vec::new(),
            features: vec![FEATURE_CANCEL.into(), FEATURE_ATTACHMENTS.into()],
        }
    }
}

pub(crate) struct Shared {
    out: mpsc::UnboundedSender<WriterCmd>,
    serialization: Serialization,
    limits: Limits,
    next_req_id: AtomicU64,
    next_stream_id: AtomicU32,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Payload>>>>,
    incoming: Mutex<HashMap<u32, IncomingStreamEntry>>,
    credits: Mutex<HashMap<u32, Arc<CreditGate>>>,
    attachments: Mutex<HashMap<u32, PendingAttachment>>,
    attachments_enabled: bool,
    inflight: Mutex<HashMap<u64, InflightCall>>,
    caps: Arc<CapabilitySet>,
    reserved: Vec<String>,
    peer_caps: RwLock<Vec<CapabilityDescriptor>>,
    announced: watch::Sender<u64>,
    graceful: Mutex<Option<String>>,
    close_tx: watch::Sender<Option<CloseReason>>,
    info: ConnectionInfo,
}

impl Shared {
    fn send_envelope(&self, envelope: &Envelope) -> Result<()> {
        let msg = encode_control(envelope, self.serialization)
            .map_err(|e| WireError::internal(format!("encode: {e}")))?;
        self.out
            .send(WriterCmd::Send(msg))
            .map_err(|_| WireError::connection_lost())
    }

    /// Send a response, moving any oversized blob in it onto its own stream first.
    ///
    /// The detach happens here rather than in the capability layer so it covers every response the
    /// connection sends — a unary result and a uni_stream head both arrive through this one call —
    /// and so no generated client or server code has to know attachments exist.
    fn reply_res(self: &Arc<Self>, id: u64, replied: &AtomicBool, mut result: Payload) {
        if replied.swap(true, Ordering::SeqCst) {
            return;
        }
        let detached = if self.attachments_enabled {
            let mut alloc = || self.next_stream_id.fetch_add(2, Ordering::Relaxed);
            attach::detach(&mut result.0, INLINE_BLOB_MAX, &mut alloc)
        } else {
            Vec::new()
        };

        // Register every gate before the response goes out. An `s_credit` for a stream with no gate
        // is dropped (`handle_control`), and the peer may top up the moment it reads the reference.
        for item in &detached {
            let gate = CreditGate::new(self.limits.initial_stream_credit as u64);
            self.credits.lock().unwrap().insert(item.stream, gate);
        }

        let _ = self.send_envelope(&Envelope::Res { id, result });

        for item in detached {
            let shared = self.clone();
            tokio::spawn(async move { shared.send_attachment(item).await });
        }
    }

    /// Push one detached blob out as STREAM_DATA, respecting the peer's credit, and close with the
    /// digest in the trailer.
    async fn send_attachment(self: Arc<Self>, item: zyris_proto::Detached) {
        let Some(gate) = self.credits.lock().unwrap().get(&item.stream).cloned() else { return };
        let chunk_size = (self.limits.max_chunk as usize).max(1);
        let mut seq = 0u32;
        let mut ok = true;
        for chunk in item.bytes.chunks(chunk_size) {
            if gate.acquire(chunk.len() as u64).await.is_err() {
                ok = false;
                break;
            }
            let frame = encode_stream_data(item.stream, seq, chunk);
            if self.out.send(WriterCmd::Send(WireMessage::Binary(frame))).is_err() {
                ok = false;
                break;
            }
            seq += 1;
        }
        if ok {
            let trailer = Payload::from_typed(&AttachmentTrailer { sha256: Some(item.sha256) })
                .unwrap_or_default();
            let _ = self.send_envelope(&Envelope::SEnd { stream: item.stream, trailer });
        }
        self.credits.lock().unwrap().remove(&item.stream);
    }

    fn reply_err(&self, id: u64, replied: &AtomicBool, error: WireError) {
        if !replied.swap(true, Ordering::SeqCst) {
            let _ = self.send_envelope(&Envelope::Err { id, error });
        }
    }

    /// Fail every call still running for a capability that has just been revoked, per §5.
    fn revoke_inflight(&self, capability: &str) {
        // A capability that revokes itself from inside one of its own tools would otherwise abort
        // the task awaiting the removal, which would then never return.
        let caller = tokio::task::try_id();
        let victims: Vec<(u64, InflightCall)> = {
            let mut inflight = self.inflight.lock().unwrap();
            let ids: Vec<u64> = inflight
                .iter()
                .filter(|(_, call)| call.capability == capability)
                .filter(|(_, call)| caller != Some(call.abort.id()))
                .map(|(id, _)| *id)
                .collect();
            ids.into_iter().filter_map(|id| inflight.remove(&id).map(|call| (id, call))).collect()
        };
        for (id, call) in victims {
            let error = WireError::new(
                ErrorCode::CapabilityUnavailable,
                format!("capability {capability} was revoked"),
            );
            if !call.replied.swap(true, Ordering::SeqCst) {
                let _ = self.send_envelope(&Envelope::Err { id, error });
            } else if let Some(stream) = call.stream {
                // The head is already out, so the caller is reading items rather than waiting on a
                // reply; the stream's own terminator is the only thing that reaches it.
                let _ = self.send_envelope(&Envelope::SErr { stream, error });
                self.credits.lock().unwrap().remove(&stream);
            }
            call.abort.abort();
        }
    }
}

#[derive(Clone)]
pub struct Connection {
    shared: Arc<Shared>,
}

pub trait CapabilityClient: Sized {
    const NAME: &'static str;
    const VERSION: u32;
    fn from_handle(handle: crate::handle::CapabilityHandle) -> Self;
}

impl Connection {
    pub(crate) fn from_shared(shared: Arc<Shared>) -> Connection {
        Connection { shared }
    }

    pub(crate) fn revoke_inflight(&self, capability: &str) {
        self.shared.revoke_inflight(capability);
    }

    pub fn info(&self) -> &ConnectionInfo {
        &self.shared.info
    }

    pub fn serialization(&self) -> Serialization {
        self.shared.serialization
    }

    pub fn peer_descriptors(&self) -> Vec<CapabilityDescriptor> {
        self.shared.peer_caps.read().unwrap().clone()
    }

    pub fn local_descriptors(&self) -> Vec<CapabilityDescriptor> {
        self.shared.caps.descriptors()
    }

    pub fn has_peer_capability(&self, name: &str, version: u32) -> bool {
        self.shared
            .peer_caps
            .read()
            .unwrap()
            .iter()
            .any(|c| c.name == name && c.version == version)
    }

    pub fn capability<C: CapabilityClient>(&self) -> Option<C> {
        if self.has_peer_capability(C::NAME, C::VERSION) {
            Some(C::from_handle(crate::handle::CapabilityHandle::new(self.clone(), C::NAME)))
        } else {
            None
        }
    }

    pub async fn wait_capability<C: CapabilityClient>(&self, timeout: Duration) -> Result<C> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut announced = self.shared.announced.subscribe();
        let mut closed = self.shared.close_tx.subscribe();
        loop {
            if let Some(client) = self.capability::<C>() {
                return Ok(client);
            }
            if closed.borrow().is_some() {
                return Err(WireError::connection_lost());
            }
            tokio::select! {
                changed = announced.changed() => {
                    if changed.is_err() {
                        return Err(WireError::connection_lost());
                    }
                }
                changed = closed.changed() => {
                    if changed.is_err() || closed.borrow().is_some() {
                        return Err(WireError::connection_lost());
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    return Err(WireError::new(
                        ErrorCode::CapabilityNotAnnounced,
                        format!("peer did not announce {} v{}", C::NAME, C::VERSION),
                    ));
                }
            }
        }
    }

    pub async fn call_raw(&self, method: &str, params: Payload) -> Result<Payload> {
        self.request(method, params, None).await
    }

    async fn request(
        &self,
        method: &str,
        params: Payload,
        stream: Option<StreamDecl>,
    ) -> Result<Payload> {
        let shared = &self.shared;
        let id = shared.next_req_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        shared.pending.lock().unwrap().insert(id, tx);
        let envelope = Envelope::Req { id, method: method.to_string(), params, stream };
        if let Err(e) = shared.send_envelope(&envelope) {
            shared.pending.lock().unwrap().remove(&id);
            return Err(e);
        }
        match rx.await {
            Ok(result) => result,
            Err(_) => Err(WireError::connection_lost()),
        }
    }

    pub async fn call_streaming_raw(
        &self,
        method: &str,
        params: Payload,
    ) -> Result<(Payload, RawIncomingStream)> {
        let shared = &self.shared;
        let stream_id = shared.next_stream_id.fetch_add(2, Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        shared
            .incoming
            .lock()
            .unwrap()
            .insert(stream_id, IncomingStreamEntry { tx, next_seq: 0 });
        match self.request(method, params, Some(StreamDecl { id: stream_id })).await {
            Ok(head) => Ok((
                head,
                RawIncomingStream {
                    shared: self.shared.clone(),
                    stream_id,
                    rx,
                    finished: false,
                },
            )),
            Err(e) => {
                shared.incoming.lock().unwrap().remove(&stream_id);
                Err(e)
            }
        }
    }

    /// Whether both ends agreed to move large blobs onto streams. False against a peer built
    /// before attachments existed, in which case every blob it sends and receives is inline.
    pub fn attachments_enabled(&self) -> bool {
        self.shared.attachments_enabled
    }

    /// Take the byte stream for one attachment a received payload referenced.
    ///
    /// `None` once it has been claimed, or for a stream this connection never saw announced.
    /// Dropping the returned stream cancels the transfer, which is the same contract every other
    /// incoming stream has.
    pub fn attachment(&self, stream: u32) -> Option<(AttachmentRef, RawIncomingStream)> {
        let pending = self.shared.attachments.lock().unwrap().remove(&stream)?;
        Some((
            pending.reference,
            RawIncomingStream {
                shared: self.shared.clone(),
                stream_id: stream,
                rx: pending.rx,
                finished: false,
            },
        ))
    }

    /// Collect one attachment into memory, checking it against what the reference promised.
    ///
    /// `max_bytes` is refused up front from the declared size rather than partway through the
    /// transfer: a caller that cannot hold the blob should not spend the bandwidth to find out.
    pub async fn collect_attachment(&self, stream: u32, max_bytes: usize) -> Result<Bytes> {
        let Some((reference, mut chunks)) = self.attachment(stream) else {
            return Err(WireError::new(
                ErrorCode::Other("attachment_gone".into()),
                format!("stream {stream} was never announced, or has already been claimed"),
            ));
        };
        if reference.size > max_bytes as u64 {
            drop(chunks);
            return Err(WireError::new(
                ErrorCode::PayloadTooLarge,
                format!("attachment is {} bytes, over the {max_bytes}-byte limit", reference.size),
            ));
        }

        let mut buf = Vec::with_capacity(reference.size as usize);
        while let Some(chunk) = chunks.next().await {
            buf.extend_from_slice(&chunk?);
            if buf.len() > max_bytes {
                return Err(WireError::new(
                    ErrorCode::PayloadTooLarge,
                    format!("attachment exceeded the {max_bytes}-byte limit mid-transfer"),
                ));
            }
        }

        // A blob that arrives short or altered is worse than one that fails: it becomes a corrupt
        // file nobody traces back to the wire.
        if buf.len() as u64 != reference.size {
            return Err(WireError::new(
                ErrorCode::Other("attachment_truncated".into()),
                format!("attachment declared {} bytes, {} arrived", reference.size, buf.len()),
            ));
        }
        if let Some(expected) = &reference.sha256 {
            let actual = attach::digest(&buf);
            if &actual != expected {
                return Err(WireError::new(
                    ErrorCode::Other("attachment_corrupt".into()),
                    format!("attachment sha256 {actual} does not match the declared {expected}"),
                ));
            }
        }
        Ok(Bytes::from(buf))
    }

    /// Pull every attachment a payload references back inline, leaving it as the sender wrote it.
    ///
    /// This is what a caller who just wants the bytes uses; a caller who would rather stream them
    /// somewhere reads the references itself and takes each with [`Connection::attachment`].
    pub async fn hydrate(&self, payload: &mut Payload, max_bytes: usize) -> Result<()> {
        for reference in attach::refs(&payload.0) {
            let bytes = self.collect_attachment(reference.stream, max_bytes).await?;
            attach::splice(&mut payload.0, reference.stream, &bytes);
        }
        Ok(())
    }

    pub fn notify(&self, method: &str, params: Payload) -> Result<()> {
        self.shared
            .send_envelope(&Envelope::Note { method: method.to_string(), params })
    }

    pub async fn announce(&self) -> Result<AnnounceResult> {
        let params = Payload::from_typed(&AnnounceParams {
            capabilities: self.shared.caps.descriptors(),
        })?;
        let result = self.call_raw(METHOD_ANNOUNCE, params).await?;
        result.to_typed()
    }

    pub fn close(&self, reason: &str) {
        let params = Payload::from_typed(&ClosingParams { reason: reason.to_string() })
            .unwrap_or_default();
        let _ = self
            .shared
            .send_envelope(&Envelope::Note { method: METHOD_CLOSING.to_string(), params });
        let _ = self.shared.out.send(WriterCmd::Close {
            code: CLOSE_NORMAL,
            reason: reason.to_string(),
        });
    }

    pub async fn closed(&self) -> CloseReason {
        let mut rx = self.shared.close_tx.subscribe();
        loop {
            if let Some(reason) = rx.borrow().clone() {
                return reason;
            }
            if rx.changed().await.is_err() {
                return CloseReason::Transport("connection dropped".into());
            }
        }
    }

    pub fn is_closed(&self) -> bool {
        self.shared.close_tx.subscribe().borrow().is_some()
    }
}

pub struct RawIncomingStream {
    shared: Arc<Shared>,
    stream_id: u32,
    rx: mpsc::UnboundedReceiver<StreamEvent>,
    finished: bool,
}

impl Stream for RawIncomingStream {
    type Item = Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.finished {
            return Poll::Ready(None);
        }
        match self.rx.poll_recv(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(StreamEvent::Data(bytes))) => {
                let _ = self.shared.send_envelope(&Envelope::SCredit {
                    stream: self.stream_id,
                    bytes: bytes.len() as u64,
                });
                Poll::Ready(Some(Ok(bytes)))
            }
            Poll::Ready(Some(StreamEvent::End)) => {
                self.finished = true;
                Poll::Ready(None)
            }
            Poll::Ready(Some(StreamEvent::Failed(e))) => {
                self.finished = true;
                Poll::Ready(Some(Err(e)))
            }
            Poll::Ready(None) => {
                self.finished = true;
                Poll::Ready(Some(Err(WireError::connection_lost())))
            }
        }
    }
}

impl Drop for RawIncomingStream {
    fn drop(&mut self) {
        if !self.finished {
            self.shared.incoming.lock().unwrap().remove(&self.stream_id);
            let _ = self
                .shared
                .send_envelope(&Envelope::SCancel { stream: self.stream_id });
        }
    }
}

/// The two feature lists a handshake produces. A feature is usable only when it appears in both:
/// §3.2 forbids sending frames gated behind something the other side did not advertise, and an
/// acceptor that withheld a feature must not use it merely because the dialer offered it.
struct Features {
    local: Vec<String>,
    peer: Vec<String>,
}

impl Features {
    fn agreed(&self, feature: &str) -> bool {
        self.local.iter().any(|f| f == feature) && self.peer.iter().any(|f| f == feature)
    }
}

pub(crate) enum Role {
    Dial { agent: String },
    Accept { options: AcceptOptions },
}

pub(crate) async fn establish(
    transport: Box<dyn Transport>,
    role: Role,
    capabilities: Arc<CapabilitySet>,
) -> Result<Connection> {
    let (mut sink, mut stream) = transport.split();

    let (serialization, limits, info, reserved, features) = match role {
        Role::Dial { agent } => {
            let local = vec![FEATURE_CANCEL.to_string(), FEATURE_ATTACHMENTS.to_string()];
            let hello = Envelope::Hello(Hello {
                protocol: HelloProtocol { major: PROTOCOL_MAJOR, minors_supported: vec![PROTOCOL_MINOR] },
                serialization: vec![Serialization::Msgpack, Serialization::Json],
                agent,
                features: local.clone(),
                resume: None,
            });
            send_handshake(&mut sink, &hello).await?;
            let ack = match read_handshake(&mut stream).await? {
                Envelope::HelloAck(ack) => ack,
                Envelope::Err { error, .. } => return Err(error),
                _ => {
                    return Err(WireError::new(
                        ErrorCode::ParseError,
                        "expected hello_ack",
                    ))
                }
            };
            if ack.protocol.major != PROTOCOL_MAJOR {
                return Err(WireError::new(
                    ErrorCode::UnsupportedVersion,
                    format!("server speaks v{}", ack.protocol.major),
                ));
            }
            let info = ConnectionInfo {
                conn_id: ack.conn_id.clone(),
                node_id: ack.node_id.clone(),
                resume_token: ack.resume_token.clone(),
                resumed: ack.resumed,
                serialization: ack.serialization,
                peer_agent: None,
            };
            (ack.serialization, ack.limits, info, Vec::new(), Features { local, peer: ack.features })
        }
        Role::Accept { options } => {
            let hello = match read_handshake(&mut stream).await? {
                Envelope::Hello(hello) => hello,
                _ => return Err(WireError::new(ErrorCode::ParseError, "expected hello")),
            };
            if hello.protocol.major != PROTOCOL_MAJOR {
                let err = Envelope::Err {
                    id: 0,
                    error: WireError::new(
                        ErrorCode::UnsupportedVersion,
                        format!("supported major {PROTOCOL_MAJOR}"),
                    ),
                };
                let _ = send_handshake(&mut sink, &err).await;
                let _ = sink
                    .close(CLOSE_UNSUPPORTED_VERSION, "unsupported version".into())
                    .await;
                return Err(WireError::new(
                    ErrorCode::UnsupportedVersion,
                    format!("peer speaks v{}", hello.protocol.major),
                ));
            }
            let serialization = hello
                .serialization
                .first()
                .copied()
                .unwrap_or(Serialization::Msgpack);
            let ack = HelloAck {
                protocol: AckProtocol { major: PROTOCOL_MAJOR, minor: PROTOCOL_MINOR },
                serialization,
                conn_id: options.conn_id.clone(),
                resume_token: options.resume_token.clone(),
                node_id: options.node_id.clone(),
                heartbeat: options.heartbeat,
                limits: options.limits,
                resumed: options.resumed,
                features: options.features.clone(),
            };
            send_handshake(&mut sink, &Envelope::HelloAck(ack)).await?;
            let info = ConnectionInfo {
                conn_id: options.conn_id,
                node_id: options.node_id,
                resume_token: options.resume_token,
                resumed: options.resumed,
                serialization,
                peer_agent: Some(hello.agent),
            };
            (
                serialization,
                options.limits,
                info,
                options.reserved_capabilities,
                Features { local: options.features, peer: hello.features },
            )
        }
    };

    let side = match info.peer_agent {
        Some(_) => Side::Acceptor,
        None => Side::Dialer,
    };
    let (out_tx, out_rx) = mpsc::unbounded_channel();
    let (close_tx, _) = watch::channel(None);
    let (announced, _) = watch::channel(0);
    let shared = Arc::new(Shared {
        out: out_tx,
        serialization,
        limits,
        next_req_id: AtomicU64::new(1),
        next_stream_id: AtomicU32::new(match side {
            Side::Dialer => 1,
            Side::Acceptor => 2,
        }),
        pending: Mutex::new(HashMap::new()),
        incoming: Mutex::new(HashMap::new()),
        credits: Mutex::new(HashMap::new()),
        attachments: Mutex::new(HashMap::new()),
        attachments_enabled: features.agreed(FEATURE_ATTACHMENTS),
        inflight: Mutex::new(HashMap::new()),
        caps: capabilities.clone(),
        reserved,
        peer_caps: RwLock::new(Vec::new()),
        announced,
        graceful: Mutex::new(None),
        close_tx,
        info,
    });

    // Registered before anything can be announced, so a capability change racing the handshake
    // reaches this connection rather than being announced only to the ones that came before it.
    capabilities.register(&shared);

    tokio::spawn(run_writer(out_rx, sink));
    tokio::spawn(run_reader(shared.clone(), stream));

    let conn = Connection { shared };
    if !conn.shared.caps.is_empty() {
        let announcer = conn.clone();
        tokio::spawn(async move {
            if let Err(e) = announcer.announce().await {
                tracing::debug!(error = %e, "zyris announce failed");
            }
        });
    }
    Ok(conn)
}

async fn send_handshake(sink: &mut Box<dyn WireSink>, envelope: &Envelope) -> Result<()> {
    let msg = encode_control(envelope, Serialization::Msgpack)
        .map_err(|e| WireError::internal(format!("encode handshake: {e}")))?;
    sink.send(msg).await.map_err(WireError::from)
}

async fn read_handshake(stream: &mut Box<dyn WireStream>) -> Result<Envelope> {
    match stream.next().await {
        None => Err(WireError::connection_lost()),
        Some(Err(e)) => Err(e.into()),
        Some(Ok(WireMessage::Binary(bytes))) => match decode_binary(bytes) {
            Ok(IncomingFrame::Control(envelope)) => Ok(envelope),
            Ok(_) => Err(WireError::new(ErrorCode::ParseError, "expected control frame")),
            Err(e) => Err(WireError::new(ErrorCode::ParseError, e.to_string())),
        },
        Some(Ok(WireMessage::Text(_))) => {
            Err(WireError::new(ErrorCode::ParseError, "handshake must be msgpack"))
        }
    }
}

async fn run_writer(mut rx: mpsc::UnboundedReceiver<WriterCmd>, mut sink: Box<dyn WireSink>) {
    while let Some(cmd) = rx.recv().await {
        match cmd {
            WriterCmd::Send(msg) => {
                if sink.send(msg).await.is_err() {
                    break;
                }
            }
            WriterCmd::Close { code, reason } => {
                let _ = sink.close(code, reason).await;
                break;
            }
        }
    }
}

async fn run_reader(shared: Arc<Shared>, mut stream: Box<dyn WireStream>) {
    let reason = loop {
        match stream.next().await {
            None => break CloseReason::Transport("connection closed".into()),
            Some(Err(e)) => break CloseReason::Transport(e.to_string()),
            Some(Ok(msg)) => {
                let frame = match msg {
                    WireMessage::Binary(bytes) => match decode_binary(bytes) {
                        Ok(frame) => frame,
                        Err(e) => break CloseReason::Protocol(e.to_string()),
                    },
                    WireMessage::Text(text) => match decode_text(&text) {
                        Ok(envelope) => IncomingFrame::Control(envelope),
                        Err(e) => break CloseReason::Protocol(e.to_string()),
                    },
                };
                handle_frame(&shared, frame);
            }
        }
    };
    shutdown(&shared, reason);
}

fn shutdown(shared: &Arc<Shared>, reason: CloseReason) {
    let reason = match shared.graceful.lock().unwrap().take() {
        Some(msg) => CloseReason::Graceful(msg),
        None => reason,
    };
    for (_, tx) in shared.pending.lock().unwrap().drain() {
        let _ = tx.send(Err(WireError::connection_lost()));
    }
    for (_, entry) in shared.incoming.lock().unwrap().drain() {
        let _ = entry.tx.send(StreamEvent::Failed(WireError::connection_lost()));
    }
    for (_, gate) in shared.credits.lock().unwrap().drain() {
        gate.cancel();
    }
    shared.attachments.lock().unwrap().clear();
    for (_, call) in shared.inflight.lock().unwrap().drain() {
        call.abort.abort();
    }
    shared.close_tx.send_replace(Some(reason.clone()));
    let _ = shared.out.send(WriterCmd::Close {
        code: CLOSE_NORMAL,
        reason: reason.to_string(),
    });
}

fn handle_frame(shared: &Arc<Shared>, frame: IncomingFrame) {
    match frame {
        IncomingFrame::Control(envelope) => handle_control(shared, envelope),
        IncomingFrame::StreamData { stream, seq, payload } => {
            let mut incoming = shared.incoming.lock().unwrap();
            if let Some(entry) = incoming.get_mut(&stream) {
                if seq != entry.next_seq {
                    let entry = incoming.remove(&stream).unwrap();
                    let _ = entry.tx.send(StreamEvent::Failed(WireError::new(
                        ErrorCode::StreamLagged,
                        format!("chunk gap: expected {} got {seq}", entry.next_seq),
                    )));
                    drop(incoming);
                    let _ = shared.send_envelope(&Envelope::SCancel { stream });
                    return;
                }
                entry.next_seq += 1;
                let _ = entry.tx.send(StreamEvent::Data(payload));
            }
        }
    }
}

fn handle_control(shared: &Arc<Shared>, envelope: Envelope) {
    match envelope {
        Envelope::Hello(_) | Envelope::HelloAck(_) => {}
        Envelope::Req { id, method, params, stream } => {
            handle_request(shared, id, method, params, stream)
        }
        Envelope::Res { id, result } => {
            // Before the payload leaves the reader. The peer starts sending chunks as soon as it
            // has written the response, and `handle_frame` drops stream data for an id it has no
            // entry for — so registering from the awakened caller's task would lose whatever
            // arrived in between.
            register_attachments(shared, &result);
            if let Some(tx) = shared.pending.lock().unwrap().remove(&id) {
                let _ = tx.send(Ok(result));
            }
        }
        Envelope::Err { id, error } => {
            if let Some(tx) = shared.pending.lock().unwrap().remove(&id) {
                let _ = tx.send(Err(error));
            }
        }
        Envelope::Prog { .. } => {}
        Envelope::Cancel { id } => {
            let call = shared.inflight.lock().unwrap().remove(&id);
            if let Some(call) = call {
                call.abort.abort();
                call.done.store(true, Ordering::SeqCst);
                if !call.replied.swap(true, Ordering::SeqCst) {
                    let _ = shared.send_envelope(&Envelope::Err {
                        id,
                        error: WireError::canceled(),
                    });
                } else if let Some(stream) = call.stream {
                    if shared.credits.lock().unwrap().remove(&stream).is_some() {
                        let _ = shared.send_envelope(&Envelope::SErr {
                            stream,
                            error: WireError::canceled(),
                        });
                    }
                }
            }
        }
        Envelope::Note { method, params } => {
            if method == METHOD_CLOSING {
                let reason = params
                    .to_typed::<ClosingParams>()
                    .map(|p| p.reason)
                    .unwrap_or_else(|_| "closing".to_string());
                *shared.graceful.lock().unwrap() = Some(reason);
            }
        }
        Envelope::SCredit { stream, bytes } => {
            let gate = shared.credits.lock().unwrap().get(&stream).cloned();
            if let Some(gate) = gate {
                gate.add(bytes);
            }
        }
        Envelope::SCancel { stream } => {
            let gate = shared.credits.lock().unwrap().remove(&stream);
            if let Some(gate) = gate {
                gate.cancel();
            }
        }
        Envelope::SEnd { stream, .. } => {
            if let Some(entry) = shared.incoming.lock().unwrap().remove(&stream) {
                let _ = entry.tx.send(StreamEvent::End);
            }
        }
        Envelope::SErr { stream, error } => {
            if let Some(entry) = shared.incoming.lock().unwrap().remove(&stream) {
                let _ = entry.tx.send(StreamEvent::Failed(error));
            }
        }
    }
}

fn handle_request(
    shared: &Arc<Shared>,
    id: u64,
    method: String,
    params: Payload,
    stream: Option<StreamDecl>,
) {
    if method == METHOD_ANNOUNCE {
        handle_announce(shared, id, params);
        return;
    }
    let Some((cap_name, tool)) = split_method(&method) else {
        let _ = shared.send_envelope(&Envelope::Err {
            id,
            error: WireError::method_not_found(&method),
        });
        return;
    };
    let Some(capability) = shared.caps.get(cap_name) else {
        let _ = shared.send_envelope(&Envelope::Err {
            id,
            error: WireError::new(
                ErrorCode::CapabilityNotAnnounced,
                format!("capability {cap_name} not available"),
            ),
        });
        return;
    };

    let replied = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let cap_name = cap_name.to_string();
    let tool = tool.to_string();
    let task_shared = shared.clone();
    let task_replied = replied.clone();
    let task_done = done.clone();
    let stream_id = stream.map(|s| s.id);
    let task = tokio::spawn(async move {
        let call = IncomingCall {
            tool,
            params,
            serialization: task_shared.serialization,
        };
        match capability.dispatch(call).await {
            Ok(Outgoing::Response(result)) => {
                task_shared.reply_res(id, &task_replied, result);
            }
            Ok(Outgoing::Stream { head, mut items }) => match stream_id {
                None => {
                    task_shared.reply_err(
                        id,
                        &task_replied,
                        WireError::invalid_params("streaming tool requires a stream declaration"),
                    );
                }
                Some(stream_id) => {
                    let gate =
                        CreditGate::new(task_shared.limits.initial_stream_credit as u64);
                    task_shared
                        .credits
                        .lock()
                        .unwrap()
                        .insert(stream_id, gate.clone());
                    task_shared.reply_res(id, &task_replied, head);
                    let mut seq = 0u32;
                    loop {
                        match items.next().await {
                            Some(Ok(bytes)) => {
                                if gate.acquire(bytes.len() as u64).await.is_err() {
                                    break;
                                }
                                let frame = encode_stream_data(stream_id, seq, &bytes);
                                if task_shared
                                    .out
                                    .send(WriterCmd::Send(WireMessage::Binary(frame)))
                                    .is_err()
                                {
                                    break;
                                }
                                seq += 1;
                            }
                            Some(Err(error)) => {
                                let _ = task_shared
                                    .send_envelope(&Envelope::SErr { stream: stream_id, error });
                                break;
                            }
                            None => {
                                let _ = task_shared.send_envelope(&Envelope::SEnd {
                                    stream: stream_id,
                                    trailer: Payload::default(),
                                });
                                break;
                            }
                        }
                    }
                    task_shared.credits.lock().unwrap().remove(&stream_id);
                }
            },
            Err(error) => {
                task_shared.reply_err(id, &task_replied, error);
            }
        }
        task_done.store(true, Ordering::SeqCst);
        task_shared.inflight.lock().unwrap().remove(&id);
    });

    let mut inflight = shared.inflight.lock().unwrap();
    if !done.load(Ordering::SeqCst) {
        inflight.insert(
            id,
            InflightCall {
                capability: cap_name,
                abort: task.abort_handle(),
                replied,
                done,
                stream: stream_id,
            },
        );
    }
}

/// Open a receive side for every attachment a payload references, so the chunks already in flight
/// land somewhere. Claiming one later is then just taking the entry back out.
fn register_attachments(shared: &Arc<Shared>, payload: &Payload) {
    for reference in attach::refs(&payload.0) {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut incoming = shared.incoming.lock().unwrap();
        if incoming.contains_key(&reference.stream) {
            continue;
        }
        incoming.insert(reference.stream, IncomingStreamEntry { tx, next_seq: 0 });
        drop(incoming);
        // The receiving half waits here until someone claims it; chunks queue on the channel in
        // the meantime, bounded by the credit the peer was granted rather than by the queue.
        let stream = reference.stream;
        shared
            .attachments
            .lock()
            .unwrap()
            .insert(stream, PendingAttachment { reference, rx });

        // Nobody is obliged to claim an attachment, and an unclaimed one holds the sender at its
        // credit ceiling forever. Give the caller a generous window, then cancel — a transfer the
        // application never asked about is not worth a wedged peer.
        let sweeper = shared.clone();
        tokio::spawn(async move {
            tokio::time::sleep(ATTACHMENT_CLAIM_GRACE).await;
            if sweeper.attachments.lock().unwrap().remove(&stream).is_some() {
                sweeper.incoming.lock().unwrap().remove(&stream);
                let _ = sweeper.send_envelope(&Envelope::SCancel { stream });
                tracing::debug!(stream, "cancelled an attachment nobody claimed");
            }
        });
    }
}

fn handle_announce(shared: &Arc<Shared>, id: u64, params: Payload) {
    let parsed: AnnounceParams = match params.to_typed() {
        Ok(p) => p,
        Err(e) => {
            let _ = shared.send_envelope(&Envelope::Err { id, error: e });
            return;
        }
    };
    let mut accepted = Vec::new();
    let mut rejected = Vec::new();
    let mut kept = Vec::new();
    for cap in parsed.capabilities {
        if shared.reserved.contains(&cap.name) {
            rejected.push(RejectedCapability { name: cap.name, reason: "reserved".into() });
        } else {
            accepted.push(cap.name.clone());
            kept.push(cap);
        }
    }
    *shared.peer_caps.write().unwrap() = kept;
    shared.announced.send_modify(|v| *v += 1);
    let result = AnnounceResult { accepted, rejected };
    match Payload::from_typed(&result) {
        Ok(result) => {
            let _ = shared.send_envelope(&Envelope::Res { id, result });
        }
        Err(e) => {
            let _ = shared.send_envelope(&Envelope::Err { id, error: e });
        }
    }
}

pub(crate) fn typed_method(capability: &str, tool: &str) -> String {
    method_name(capability, tool)
}
