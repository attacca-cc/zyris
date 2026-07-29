use futures_util::StreamExt;
use serde::de::DeserializeOwned;
use serde::Serialize;
use zyris_proto::{decode_item, Payload};

use crate::connection::{typed_method, Connection};
use crate::error::Result;
use crate::serve::Streaming;

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

    pub async fn call<Req: Serialize, Resp: DeserializeOwned>(
        &self,
        tool: &str,
        request: &Req,
    ) -> Result<Resp> {
        let params = Payload::from_typed(request)?;
        let result = self
            .conn
            .call_raw(&typed_method(self.capability, tool), params)
            .await?;
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
