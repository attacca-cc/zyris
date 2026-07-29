use serde::{Deserialize, Serialize};

use crate::payload::Payload;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    ParseError,
    UnsupportedVersion,
    Unauthorized,
    ForbiddenScope,
    MethodNotFound,
    InvalidParams,
    CapabilityNotAnnounced,
    CapabilityRejected,
    CapabilityUnavailable,
    NodeOffline,
    ConnectionLost,
    StreamLagged,
    CreditViolation,
    PayloadTooLarge,
    Overloaded,
    Canceled,
    Timeout,
    Unsupported,
    Internal,
    #[serde(untagged)]
    Other(String),
}

impl ErrorCode {
    pub fn default_retriable(&self) -> bool {
        matches!(
            self,
            ErrorCode::NodeOffline
                | ErrorCode::ConnectionLost
                | ErrorCode::StreamLagged
                | ErrorCode::Overloaded
                | ErrorCode::Timeout
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct WireError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default)]
    pub retriable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Payload>,
}

impl WireError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        let retriable = code.default_retriable();
        Self { code, message: message.into(), retriable, data: None }
    }

    pub fn retriable(mut self, retriable: bool) -> Self {
        self.retriable = retriable;
        self
    }

    pub fn with_data(mut self, data: Payload) -> Self {
        self.data = Some(data);
        self
    }

    pub fn method_not_found(method: &str) -> Self {
        Self::new(ErrorCode::MethodNotFound, format!("unknown method {method}"))
    }

    pub fn invalid_params(detail: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidParams, detail)
    }

    pub fn canceled() -> Self {
        Self::new(ErrorCode::Canceled, "request canceled")
    }

    pub fn connection_lost() -> Self {
        Self::new(ErrorCode::ConnectionLost, "connection lost")
    }

    pub fn internal(detail: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, detail)
    }
}
