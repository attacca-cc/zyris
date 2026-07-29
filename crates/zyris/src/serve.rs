use std::pin::Pin;

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::Stream;
use zyris_proto::{CapabilityDescriptor, Payload, Serialization, WireError};

use crate::error::Result;

pub type ItemStream<I> = Pin<Box<dyn Stream<Item = Result<I>> + Send>>;
pub type RawItemStream = Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>;

pub struct Streaming<H, I> {
    pub head: H,
    pub items: ItemStream<I>,
}

impl<H, I> Streaming<H, I> {
    pub fn new(head: H, items: impl Stream<Item = Result<I>> + Send + 'static) -> Self {
        Streaming { head, items: Box::pin(items) }
    }
}

pub struct IncomingCall {
    pub tool: String,
    pub params: Payload,
    pub serialization: Serialization,
}

pub enum Outgoing {
    Response(Payload),
    Stream { head: Payload, items: RawItemStream },
}

#[async_trait]
pub trait ServeCapability: Send + Sync + 'static {
    fn descriptor(&self) -> CapabilityDescriptor;
    async fn dispatch(&self, call: IncomingCall) -> Result<Outgoing>;
}

pub fn encode_streaming<H, I>(
    streaming: Streaming<H, I>,
    serialization: Serialization,
) -> Result<Outgoing>
where
    H: serde::Serialize,
    I: serde::Serialize + Send + 'static,
{
    use futures_util::StreamExt;
    let head = Payload::from_typed(&streaming.head)?;
    let items = streaming.items.map(move |item| {
        item.and_then(|value| {
            zyris_proto::encode_item(&value, serialization).map(Bytes::from)
        })
    });
    Ok(Outgoing::Stream { head, items: Box::pin(items) })
}

pub fn decode_call<T: serde::de::DeserializeOwned>(call: &IncomingCall) -> Result<T> {
    call.params.to_typed()
}

pub fn encode_response<T: serde::Serialize>(value: &T) -> Result<Outgoing> {
    Ok(Outgoing::Response(Payload::from_typed(value)?))
}

pub fn unknown_tool(capability: &str, tool: &str) -> WireError {
    WireError::method_not_found(&format!("{capability}.{tool}"))
}
