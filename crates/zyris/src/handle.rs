use futures_util::StreamExt;
use serde::de::DeserializeOwned;
use serde::Serialize;
use zyris_proto::{decode_item, Payload};

use crate::connection::{typed_method, Connection};
use crate::error::Result;
use crate::serve::Streaming;

/// How much a generated client will collect into memory to make an attachment look inline. Sized
/// for the datums a tool call plausibly returns — a screenshot, a document — rather than for bulk
/// transfer, which has `file_io.read` and its chunked stream.
pub const MAX_HYDRATE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone)]
pub struct CapabilityHandle {
    conn: Connection,
    capability: &'static str,
}

impl CapabilityHandle {
    pub fn new(conn: Connection, capability: &'static str) -> Self {
        CapabilityHandle { conn, capability }
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Call a unary tool.
    ///
    /// Any blob the peer moved onto a stream is pulled back inline before the response is
    /// deserialized, so a generated client hands its caller a `Datum` with bytes in it whether the
    /// peer inlined them or not — the whole attachment mechanism is invisible from here. The
    /// ceiling is [`MAX_HYDRATE_BYTES`]; a caller expecting something larger than that wants
    /// [`Connection::attachment`] and a destination to stream it into, not a `Vec` in memory.
    pub async fn call<Req: Serialize, Resp: DeserializeOwned>(
        &self,
        tool: &str,
        request: &Req,
    ) -> Result<Resp> {
        let params = Payload::from_typed(request)?;
        let mut result = self
            .conn
            .call_raw(&typed_method(self.capability, tool), params)
            .await?;
        self.conn.hydrate(&mut result, MAX_HYDRATE_BYTES).await?;
        result.to_typed()
    }

    pub async fn call_streaming<Req, Head, Item>(
        &self,
        tool: &str,
        request: &Req,
    ) -> Result<Streaming<Head, Item>>
    where
        Req: Serialize,
        Head: DeserializeOwned,
        Item: DeserializeOwned + Send + 'static,
    {
        let params = Payload::from_typed(request)?;
        let (head, raw) = self
            .conn
            .call_streaming_raw(&typed_method(self.capability, tool), params)
            .await?;
        let head = head.to_typed()?;
        let serialization = self.conn.serialization();
        let items =
            raw.map(move |chunk| chunk.and_then(|bytes| decode_item(&bytes, serialization)));
        Ok(Streaming { head, items: Box::pin(items) })
    }
}
