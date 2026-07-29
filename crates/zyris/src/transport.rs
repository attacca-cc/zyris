use async_trait::async_trait;
use tokio::sync::mpsc;
use zyris_proto::WireMessage;

use crate::error::TransportError;

#[async_trait]
pub trait WireSink: Send {
    async fn send(&mut self, msg: WireMessage) -> Result<(), TransportError>;
    async fn close(&mut self, code: u16, reason: String) -> Result<(), TransportError>;
}

#[async_trait]
pub trait WireStream: Send {
    async fn next(&mut self) -> Option<Result<WireMessage, TransportError>>;
}

pub trait Transport: Send + 'static {
    fn split(self: Box<Self>) -> (Box<dyn WireSink>, Box<dyn WireStream>);
}

pub struct ChannelTransport {
    tx: mpsc::UnboundedSender<WireMessage>,
    rx: mpsc::UnboundedReceiver<WireMessage>,
}

impl ChannelTransport {
    pub fn pair() -> (ChannelTransport, ChannelTransport) {
        let (a_tx, a_rx) = mpsc::unbounded_channel();
        let (b_tx, b_rx) = mpsc::unbounded_channel();
        (ChannelTransport { tx: a_tx, rx: b_rx }, ChannelTransport { tx: b_tx, rx: a_rx })
    }
}

struct ChannelSink(mpsc::UnboundedSender<WireMessage>);
struct ChannelStream(mpsc::UnboundedReceiver<WireMessage>);

#[async_trait]
impl WireSink for ChannelSink {
    async fn send(&mut self, msg: WireMessage) -> Result<(), TransportError> {
        self.0.send(msg).map_err(|_| TransportError::Closed)
    }

    async fn close(&mut self, _code: u16, _reason: String) -> Result<(), TransportError> {
        Ok(())
    }
}

#[async_trait]
impl WireStream for ChannelStream {
    async fn next(&mut self) -> Option<Result<WireMessage, TransportError>> {
        self.0.recv().await.map(Ok)
    }
}

impl Transport for ChannelTransport {
    fn split(self: Box<Self>) -> (Box<dyn WireSink>, Box<dyn WireStream>) {
        (Box::new(ChannelSink(self.tx)), Box::new(ChannelStream(self.rx)))
    }
}

#[cfg(feature = "client")]
pub mod ws {
    use async_trait::async_trait;
    use futures_util::stream::{SplitSink, SplitStream};
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
    use tokio_tungstenite::tungstenite::protocol::CloseFrame;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
    use zyris_proto::WireMessage;

    use crate::error::TransportError;
    use crate::error::{Error, ErrorCode, WireError};

    type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

    pub struct WsTransport(WsStream);

    pub async fn connect(url: &str, bearer_token: &str) -> Result<WsTransport, Error> {
        let mut request = url
            .into_client_request()
            .map_err(|e| WireError::invalid_params(format!("invalid url: {e}")))?;
        let value = format!("Bearer {bearer_token}")
            .parse()
            .map_err(|_| WireError::invalid_params("invalid token"))?;
        request.headers_mut().insert("authorization", value);
        let (stream, _) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(connect_error)?;
        Ok(WsTransport(stream))
    }

    /// A rejected upgrade is not a transport blip. Mapping every failure to the retriable
    /// `ConnectionLost` would leave a node holding a revoked token reconnecting forever, so the
    /// HTTP statuses that mean "this credential or this version will never work" are surfaced as
    /// non-retriable codes a dialer can exit on.
    fn connect_error(err: tokio_tungstenite::tungstenite::Error) -> Error {
        use tokio_tungstenite::tungstenite::Error as WsError;
        if let WsError::Http(response) = &err {
            let status = response.status().as_u16();
            let code = match status {
                401 | 403 => Some(ErrorCode::Unauthorized),
                426 => Some(ErrorCode::UnsupportedVersion),
                _ => None,
            };
            if let Some(code) = code {
                return WireError::new(code, format!("connect rejected with HTTP {status}"))
                    .retriable(false);
            }
        }
        WireError::new(ErrorCode::ConnectionLost, format!("connect: {err}"))
    }

    pub struct WsSink(SplitSink<WsStream, Message>);
    pub struct WsRead(SplitStream<WsStream>);

    #[async_trait]
    impl crate::transport::WireSink for WsSink {
        async fn send(&mut self, msg: WireMessage) -> Result<(), TransportError> {
            let message = match msg {
                WireMessage::Binary(b) => Message::Binary(b),
                WireMessage::Text(t) => Message::Text(t.into()),
            };
            self.0.send(message).await.map_err(|e| TransportError::Io(e.to_string()))
        }

        async fn close(&mut self, code: u16, reason: String) -> Result<(), TransportError> {
            let frame = CloseFrame { code: CloseCode::from(code), reason: reason.into() };
            let _ = self.0.send(Message::Close(Some(frame))).await;
            Ok(())
        }
    }

    #[async_trait]
    impl crate::transport::WireStream for WsRead {
        async fn next(&mut self) -> Option<Result<WireMessage, TransportError>> {
            loop {
                match self.0.next().await {
                    None => return None,
                    Some(Err(e)) => return Some(Err(TransportError::Io(e.to_string()))),
                    Some(Ok(Message::Binary(b))) => return Some(Ok(WireMessage::Binary(b))),
                    Some(Ok(Message::Text(t))) => {
                        return Some(Ok(WireMessage::Text(t.to_string())))
                    }
                    Some(Ok(Message::Close(_))) => return None,
                    Some(Ok(_)) => continue,
                }
            }
        }
    }

    impl crate::transport::Transport for WsTransport {
        fn split(
            self: Box<Self>,
        ) -> (Box<dyn crate::transport::WireSink>, Box<dyn crate::transport::WireStream>) {
            let (sink, stream) = self.0.split();
            (Box::new(WsSink(sink)), Box::new(WsRead(stream)))
        }
    }
}

#[cfg(feature = "axum")]
pub mod axum_ws {
    use async_trait::async_trait;
    use axum::extract::ws::{CloseFrame, Message, WebSocket};
    use futures_util::stream::{SplitSink, SplitStream};
    use futures_util::{SinkExt, StreamExt};
    use zyris_proto::WireMessage;

    use crate::error::TransportError;

    pub struct AxumTransport(WebSocket);

    impl AxumTransport {
        pub fn new(socket: WebSocket) -> Self {
            AxumTransport(socket)
        }
    }

    pub struct AxumSink(SplitSink<WebSocket, Message>);
    pub struct AxumRead(SplitStream<WebSocket>);

    #[async_trait]
    impl crate::transport::WireSink for AxumSink {
        async fn send(&mut self, msg: WireMessage) -> Result<(), TransportError> {
            let message = match msg {
                WireMessage::Binary(b) => Message::Binary(b),
                WireMessage::Text(t) => Message::Text(t.into()),
            };
            self.0.send(message).await.map_err(|e| TransportError::Io(e.to_string()))
        }

        async fn close(&mut self, code: u16, reason: String) -> Result<(), TransportError> {
            let frame = CloseFrame { code, reason: reason.into() };
            let _ = self.0.send(Message::Close(Some(frame))).await;
            Ok(())
        }
    }

    #[async_trait]
    impl crate::transport::WireStream for AxumRead {
        async fn next(&mut self) -> Option<Result<WireMessage, TransportError>> {
            loop {
                match self.0.next().await {
                    None => return None,
                    Some(Err(e)) => return Some(Err(TransportError::Io(e.to_string()))),
                    Some(Ok(Message::Binary(b))) => return Some(Ok(WireMessage::Binary(b))),
                    Some(Ok(Message::Text(t))) => {
                        return Some(Ok(WireMessage::Text(t.to_string())))
                    }
                    Some(Ok(Message::Close(_))) => return None,
                    Some(Ok(_)) => continue,
                }
            }
        }
    }

    impl crate::transport::Transport for AxumTransport {
        fn split(
            self: Box<Self>,
        ) -> (Box<dyn crate::transport::WireSink>, Box<dyn crate::transport::WireStream>) {
            let (sink, stream) = self.0.split();
            (Box::new(AxumSink(sink)), Box::new(AxumRead(stream)))
        }
    }
}
