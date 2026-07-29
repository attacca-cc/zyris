use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transfer {
    Unary,
    UniStream,
    BiStream,
    Video,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    pub transfer: Transfer,
    pub request_schema: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_schema: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_schema: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilityDescriptor {
    pub name: String,
    pub version: u32,
    pub tools: Vec<ToolDescriptor>,
}

impl CapabilityDescriptor {
    pub fn tool(&self, name: &str) -> Option<&ToolDescriptor> {
        self.tools.iter().find(|t| t.name == name)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnnounceParams {
    pub capabilities: Vec<CapabilityDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RejectedCapability {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AnnounceResult {
    pub accepted: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rejected: Vec<RejectedCapability>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClosingParams {
    pub reason: String,
}

pub fn method_name(capability: &str, tool: &str) -> String {
    format!("{capability}.{tool}")
}

pub fn split_method(method: &str) -> Option<(&str, &str)> {
    method.split_once('.')
}
