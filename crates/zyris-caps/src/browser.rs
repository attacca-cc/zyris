use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TargetInfo {
    pub target_id: String,
    pub url: String,
    #[serde(default)]
    pub title: String,
}

#[zyris::capability(name = "browser_chrome", version = 1)]
pub trait BrowserChrome {
    /// List open CDP targets (tabs).
    async fn targets(&self) -> zyris::Result<Vec<TargetInfo>>;

    /// Send a raw CDP command to a target and return the result.
    async fn send(
        &self,
        target_id: String,
        method: String,
        params: serde_json::Value,
    ) -> zyris::Result<serde_json::Value>;
}
