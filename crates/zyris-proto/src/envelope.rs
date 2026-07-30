use serde::{Deserialize, Serialize};

use crate::error::WireError;
use crate::payload::Payload;

pub const PROTOCOL_MAJOR: u16 = 1;
pub const PROTOCOL_MINOR: u16 = 0;

/// Advertised in `hello.features` / `hello_ack.features` by a peer that can move a large blob onto
/// a stream instead of inlining it. Both sides must list it before either detaches anything: §3.2
/// forbids sending frames gated behind a feature the other end did not advertise, and an older peer
/// that received an attachment reference would hand its caller a datum whose bytes never arrive.
pub const FEATURE_ATTACHMENTS: &str = "attachments";
pub const FEATURE_CANCEL: &str = "cancel";

pub const METHOD_ANNOUNCE: &str = "zyris.announce";
pub const METHOD_CLOSING: &str = "zyris.closing";
pub const METHOD_WEBRTC_SIGNAL: &str = "webrtc.signal";
pub const METHOD_WEBRTC_CLOSE: &str = "webrtc.close";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Serialization {
    Msgpack,
    Json,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelloProtocol {
    pub major: u16,
    #[serde(default)]
    pub minors_supported: Vec<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AckProtocol {
    pub major: u16,
    pub minor: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResumeInfo {
    pub conn_id: String,
    pub resume_token: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HeartbeatConfig {
    pub interval_s: u32,
    pub timeout_s: u32,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self { interval_s: 20, timeout_s: 45 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Limits {
    pub max_control_frame: u32,
    pub max_chunk: u32,
    pub max_inflight_reqs: u32,
    pub initial_stream_credit: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_control_frame: 1024 * 1024,
            max_chunk: 256 * 1024,
            max_inflight_reqs: 64,
            initial_stream_credit: 256 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: HelloProtocol,
    pub serialization: Vec<Serialization>,
    pub agent: String,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<ResumeInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelloAck {
    pub protocol: AckProtocol,
    pub serialization: Serialization,
    pub conn_id: String,
    pub resume_token: String,
    pub node_id: String,
    pub heartbeat: HeartbeatConfig,
    pub limits: Limits,
    #[serde(default)]
    pub resumed: bool,
    /// What the acceptor can do, answering `hello.features`. Defaulted rather than required so an
    /// acceptor built before this field parses here as advertising nothing, which is the truth.
    #[serde(default)]
    pub features: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamDecl {
    pub id: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Envelope {
    Hello(Hello),
    HelloAck(HelloAck),
    Req {
        id: u64,
        method: String,
        #[serde(default, skip_serializing_if = "Payload::is_nil")]
        params: Payload,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream: Option<StreamDecl>,
    },
    Res {
        id: u64,
        #[serde(default, skip_serializing_if = "Payload::is_nil")]
        result: Payload,
    },
    Err {
        id: u64,
        error: WireError,
    },
    Note {
        method: String,
        #[serde(default, skip_serializing_if = "Payload::is_nil")]
        params: Payload,
    },
    Prog {
        id: u64,
        #[serde(default, skip_serializing_if = "Payload::is_nil")]
        payload: Payload,
    },
    Cancel {
        id: u64,
    },
    SCredit {
        stream: u32,
        bytes: u64,
    },
    SEnd {
        stream: u32,
        #[serde(default, skip_serializing_if = "Payload::is_nil")]
        trailer: Payload,
    },
    SErr {
        stream: u32,
        error: WireError,
    },
    SCancel {
        stream: u32,
    },
}

pub const CLOSE_NORMAL: u16 = 1000;
pub const CLOSE_UNSUPPORTED_VERSION: u16 = 4400;
pub const CLOSE_UNAUTHORIZED: u16 = 4401;
pub const CLOSE_MALFORMED_FRAME: u16 = 4408;
pub const CLOSE_FLOW_VIOLATION: u16 = 4409;
