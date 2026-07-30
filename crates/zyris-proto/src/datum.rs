use base64::Engine;
use bytes::Bytes;
use schemars::{json_schema, JsonSchema};
use serde::de::{Error as DeError, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// How many bytes a [`Blob::Inline`] may carry before it should be an attachment instead.
///
/// Advisory: nothing in this crate enforces it, because the transport does not care — a websocket
/// frame is 16 MiB by default and msgpack `bin` costs one byte per byte. What cares is the far end
/// of a deployment. Attacca measures a node's tool result as `serde_json::to_vec(..).len()`
/// against `ZYRIS_MAX_RESULT_BYTES` (1,000,000 by default), and JSON means the blob is base64 —
/// four bytes out for every three in.
///
/// 512 KiB is the largest round number that survives that: 699,052 base64 bytes, leaving the
/// envelope most of a third of the budget. 1 MiB would encode to 1,398,101 and be rejected.
pub const INLINE_BLOB_MAX: usize = 512 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AttachmentRef {
    pub stream: u32,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default)]
    pub offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blob {
    Inline(Bytes),
    Attachment(AttachmentRef),
}

impl Blob {
    pub fn from_bytes(bytes: impl Into<Bytes>) -> Self {
        Blob::Inline(bytes.into())
    }

    pub fn len(&self) -> u64 {
        match self {
            Blob::Inline(b) => b.len() as u64,
            Blob::Attachment(a) => a.size,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn as_inline(&self) -> Option<&Bytes> {
        match self {
            Blob::Inline(b) => Some(b),
            Blob::Attachment(_) => None,
        }
    }

    pub fn attachment(&self) -> Option<&AttachmentRef> {
        match self {
            Blob::Inline(_) => None,
            Blob::Attachment(a) => Some(a),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct AttachmentEnvelope {
    attachment: AttachmentRef,
}

impl Serialize for Blob {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Blob::Inline(bytes) => {
                if serializer.is_human_readable() {
                    serializer
                        .serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
                } else {
                    serializer.serialize_bytes(bytes)
                }
            }
            Blob::Attachment(a) => {
                AttachmentEnvelope { attachment: a.clone() }.serialize(serializer)
            }
        }
    }
}

struct BlobVisitor;

impl<'de> Visitor<'de> for BlobVisitor {
    type Value = Blob;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("bytes, a base64 string, or an attachment object")
    }

    fn visit_bytes<E: DeError>(self, v: &[u8]) -> Result<Blob, E> {
        Ok(Blob::Inline(Bytes::copy_from_slice(v)))
    }

    fn visit_byte_buf<E: DeError>(self, v: Vec<u8>) -> Result<Blob, E> {
        Ok(Blob::Inline(Bytes::from(v)))
    }

    fn visit_str<E: DeError>(self, v: &str) -> Result<Blob, E> {
        base64::engine::general_purpose::STANDARD
            .decode(v)
            .map(|b| Blob::Inline(Bytes::from(b)))
            .map_err(|e| E::custom(format!("invalid base64 blob: {e}")))
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, map: A) -> Result<Blob, A::Error> {
        let env = AttachmentEnvelope::deserialize(
            serde::de::value::MapAccessDeserializer::new(map),
        )?;
        Ok(Blob::Attachment(env.attachment))
    }
}

impl<'de> Deserialize<'de> for Blob {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(BlobVisitor)
    }
}

impl JsonSchema for Blob {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Blob".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let attachment = generator.subschema_for::<AttachmentRef>();
        json_schema!({
            "oneOf": [
                { "type": "string", "contentEncoding": "base64" },
                {
                    "type": "object",
                    "properties": { "attachment": attachment },
                    "required": ["attachment"]
                }
            ]
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Chunk(pub Bytes);

impl Chunk {
    pub fn new(bytes: impl Into<Bytes>) -> Self {
        Chunk(bytes.into())
    }

    pub fn into_bytes(self) -> Bytes {
        self.0
    }
}

impl Serialize for Chunk {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(&self.0))
        } else {
            serializer.serialize_bytes(&self.0)
        }
    }
}

struct ChunkVisitor;

impl<'de> Visitor<'de> for ChunkVisitor {
    type Value = Chunk;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("bytes or a base64 string")
    }

    fn visit_bytes<E: DeError>(self, v: &[u8]) -> Result<Chunk, E> {
        Ok(Chunk(Bytes::copy_from_slice(v)))
    }

    fn visit_byte_buf<E: DeError>(self, v: Vec<u8>) -> Result<Chunk, E> {
        Ok(Chunk(Bytes::from(v)))
    }

    fn visit_str<E: DeError>(self, v: &str) -> Result<Chunk, E> {
        base64::engine::general_purpose::STANDARD
            .decode(v)
            .map(|b| Chunk(Bytes::from(b)))
            .map_err(|e| E::custom(format!("invalid base64 chunk: {e}")))
    }
}

impl<'de> Deserialize<'de> for Chunk {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ChunkVisitor)
    }
}

impl JsonSchema for Chunk {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Chunk".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        json_schema!({ "type": "string", "contentEncoding": "base64" })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Datum {
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<String>,
    },
    File {
        filename: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media_type: Option<String>,
        blob: Blob,
    },
    Image {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        media_type: String,
        blob: Blob,
    },
}
