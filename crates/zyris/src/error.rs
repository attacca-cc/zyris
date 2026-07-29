pub use zyris_proto::{ErrorCode, WireError};

pub type Error = WireError;
pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Clone, thiserror::Error)]
pub enum TransportError {
    #[error("transport closed")]
    Closed,
    #[error("transport error: {0}")]
    Io(String),
}

impl From<TransportError> for WireError {
    fn from(err: TransportError) -> Self {
        match err {
            TransportError::Closed => WireError::connection_lost(),
            TransportError::Io(msg) => {
                WireError::new(ErrorCode::ConnectionLost, msg).retriable(true)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseReason {
    Graceful(String),
    Transport(String),
    Protocol(String),
}

impl std::fmt::Display for CloseReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CloseReason::Graceful(r) => write!(f, "graceful close: {r}"),
            CloseReason::Transport(r) => write!(f, "transport closed: {r}"),
            CloseReason::Protocol(r) => write!(f, "protocol error: {r}"),
        }
    }
}
