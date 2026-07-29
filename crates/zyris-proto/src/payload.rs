use base64::Engine;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{ErrorCode, WireError};

#[derive(Debug, Clone, PartialEq)]
pub struct Payload(pub rmpv::Value);

impl Default for Payload {
    fn default() -> Self {
        Payload(rmpv::Value::Nil)
    }
}

impl Payload {
    pub fn is_nil(&self) -> bool {
        self.0.is_nil()
    }

    pub fn from_typed<T: Serialize>(value: &T) -> Result<Self, WireError> {
        let bytes = rmp_serde::to_vec_named(value)
            .map_err(|e| WireError::new(ErrorCode::Internal, format!("encode payload: {e}")))?;
        rmp_serde::from_slice::<rmpv::Value>(&bytes)
            .map(Payload)
            .map_err(|e| WireError::new(ErrorCode::Internal, format!("encode payload: {e}")))
    }

    pub fn from_json(value: serde_json::Value) -> Self {
        Payload(json_to_rmpv(value))
    }

    pub fn to_json(&self) -> Result<serde_json::Value, WireError> {
        rmpv_to_json(&self.0).map_err(|e| WireError::invalid_params(format!("decode payload: {e}")))
    }

    pub fn to_typed<T: DeserializeOwned>(&self) -> Result<T, WireError> {
        let mut buf = Vec::new();
        rmpv::encode::write_value(&mut buf, &self.0)
            .map_err(|e| WireError::invalid_params(format!("decode payload: {e}")))?;
        rmp_serde::from_slice(&buf)
            .map_err(|e| WireError::invalid_params(format!("decode payload: {e}")))
    }
}

impl Serialize for Payload {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            rmpv_to_json(&self.0).map_err(serde::ser::Error::custom)?.serialize(serializer)
        } else {
            self.0.serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for Payload {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        rmpv::Value::deserialize(deserializer).map(Payload)
    }
}

pub fn encode_item<T: Serialize>(
    item: &T,
    serialization: crate::envelope::Serialization,
) -> Result<Vec<u8>, WireError> {
    match serialization {
        crate::envelope::Serialization::Msgpack => rmp_serde::to_vec_named(item)
            .map_err(|e| WireError::internal(format!("encode item: {e}"))),
        crate::envelope::Serialization::Json => serde_json::to_vec(item)
            .map_err(|e| WireError::internal(format!("encode item: {e}"))),
    }
}

pub fn decode_item<T: DeserializeOwned>(
    bytes: &[u8],
    serialization: crate::envelope::Serialization,
) -> Result<T, WireError> {
    match serialization {
        crate::envelope::Serialization::Msgpack => rmp_serde::from_slice(bytes)
            .map_err(|e| WireError::invalid_params(format!("decode item: {e}"))),
        crate::envelope::Serialization::Json => serde_json::from_slice(bytes)
            .map_err(|e| WireError::invalid_params(format!("decode item: {e}"))),
    }
}

pub fn rmpv_to_json(value: &rmpv::Value) -> Result<serde_json::Value, String> {
    use serde_json::Value as J;
    Ok(match value {
        rmpv::Value::Nil => J::Null,
        rmpv::Value::Boolean(b) => J::Bool(*b),
        rmpv::Value::Integer(i) => {
            if let Some(v) = i.as_i64() {
                J::Number(v.into())
            } else if let Some(v) = i.as_u64() {
                J::Number(v.into())
            } else {
                return Err("integer out of range".into());
            }
        }
        rmpv::Value::F32(f) => serde_json::Number::from_f64(*f as f64)
            .map(J::Number)
            .unwrap_or(J::Null),
        rmpv::Value::F64(f) => serde_json::Number::from_f64(*f)
            .map(J::Number)
            .unwrap_or(J::Null),
        rmpv::Value::String(s) => J::String(s.as_str().unwrap_or_default().to_string()),
        rmpv::Value::Binary(b) => {
            J::String(base64::engine::general_purpose::STANDARD.encode(b))
        }
        rmpv::Value::Array(items) => {
            J::Array(items.iter().map(rmpv_to_json).collect::<Result<_, _>>()?)
        }
        rmpv::Value::Map(entries) => {
            let mut map = serde_json::Map::with_capacity(entries.len());
            for (k, v) in entries {
                let key = match k {
                    rmpv::Value::String(s) => s.as_str().unwrap_or_default().to_string(),
                    rmpv::Value::Integer(i) => i.to_string(),
                    other => return Err(format!("unsupported map key {other}")),
                };
                map.insert(key, rmpv_to_json(v)?);
            }
            J::Object(map)
        }
        rmpv::Value::Ext(_, _) => return Err("ext values are not representable".into()),
    })
}

pub fn json_to_rmpv(value: serde_json::Value) -> rmpv::Value {
    use serde_json::Value as J;
    match value {
        J::Null => rmpv::Value::Nil,
        J::Bool(b) => rmpv::Value::Boolean(b),
        J::Number(n) => {
            if let Some(v) = n.as_i64() {
                rmpv::Value::from(v)
            } else if let Some(v) = n.as_u64() {
                rmpv::Value::from(v)
            } else {
                rmpv::Value::F64(n.as_f64().unwrap_or(f64::NAN))
            }
        }
        J::String(s) => rmpv::Value::String(s.into()),
        J::Array(items) => rmpv::Value::Array(items.into_iter().map(json_to_rmpv).collect()),
        J::Object(map) => rmpv::Value::Map(
            map.into_iter()
                .map(|(k, v)| (rmpv::Value::String(k.into()), json_to_rmpv(v)))
                .collect(),
        ),
    }
}
