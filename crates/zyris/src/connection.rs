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
    decode_binary, decode_text, encode_control, encode_stream_data, method_name, split_method,
    AckProtocol, AnnounceParams, AnnounceResult, CapabilityDescriptor, ClosingParams, Envelope,
    ErrorCode, HeartbeatConfig, Hello, HelloAck, HelloProtocol, IncomingFrame, Limits, Payload,
    RejectedCapability, Serialization, StreamDecl, WireError, WireMessage, CLOSE_NORMAL,
    CLOSE_UNSUPPORTED_VERSION, METHOD_ANNOUNCE, METHOD_CLOSING, PROTOCOL_MAJOR, PROTOCOL_MINOR,
};

use crate::error::{CloseReason, Result};
use crate::serve::{IncomingCall, Outgoing, ServeCapability};
use crate::transport::{Transport, WireSink, WireStream};

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

struct InflightCall {
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
    inflight: Mutex<HashMap<u64, InflightCall>>,
    local: HashMap<String, Arc<dyn ServeCapability>>,
    local_descriptors: Vec<CapabilityDescriptor>,
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

    fn reply_res(&self, id: u64, replied: &AtomicBool, result: Payload) {
        if !replied.swap(true, Ordering::SeqCst) {
            let _ = self.send_envelope(&Envelope::Res { id, result });
        }
    }

    fn reply_err(&self, id: u64, replied: &AtomicBool, error: WireError) {
        if !replied.swap(true, Ordering::SeqCst) {
            let _ = self.send_envelope(&Envelope::Err { id, error });
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
        self.shared.local_descriptors.clone()
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

    pub fn notify(&self, method: &str, params: Payload) -> Result<()> {
        self.shared
            .send_envelope(&Envelope::Note { method: method.to_string(), params })
    }

    pub async fn announce(&self) -> Result<AnnounceResult> {
        let params = Payload::from_typed(&AnnounceParams {
            capabilities: self.shared.local_descriptors.clone(),
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

pub(crate) enum Role {
    Dial { agent: String },
    Accept { options: AcceptOptions },
}

pub(crate) async fn establish(
    transport: Box<dyn Transport>,
    role: Role,
    capabilities: Vec<Arc<dyn ServeCapability>>,
) -> Result<Connection> {
    let (mut sink, mut stream) = transport.split();

    let (serialization, limits, info, reserved) = match role {
        Role::Dial { agent } => {
            let hello = Envelope::Hello(Hello {
                protocol: HelloProtocol { major: PROTOCOL_MAJOR, minors_supported: vec![PROTOCOL_MINOR] },
                serialization: vec![Serialization::Msgpack, Serialization::Json],
                agent,
                features: vec!["cancel".into()],
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
            (ack.serialization, ack.limits, info, Vec::new())
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
            (serialization, options.limits, info, options.reserved_capabilities)
        }
    };

    let side = match info.peer_agent {
        Some(_) => Side::Acceptor,
        None => Side::Dialer,
    };
    let mut local = HashMap::new();
    let mut local_descriptors = Vec::new();
    for cap in capabilities {
        let descriptor = cap.descriptor();
        local_descriptors.push(descriptor.clone());
        local.insert(descriptor.name, cap);
    }

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
        inflight: Mutex::new(HashMap::new()),
        local,
        local_descriptors,
        reserved,
        peer_caps: RwLock::new(Vec::new()),
        announced,
        graceful: Mutex::new(None),
        close_tx,
        info,
    });

    tokio::spawn(run_writer(out_rx, sink));
    tokio::spawn(run_reader(shared.clone(), stream));

    let conn = Connection { shared };
    if !conn.shared.local_descriptors.is_empty() {
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
    let Some(capability) = shared.local.get(cap_name).cloned() else {
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
            InflightCall { abort: task.abort_handle(), replied, done, stream: stream_id },
        );
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
