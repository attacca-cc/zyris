use std::sync::Arc;

use crate::capabilities::{Capabilities, CapabilitySet};
use crate::connection::{establish, AcceptOptions, Connection, Role};
use crate::error::Result;
use crate::serve::ServeCapability;
use crate::transport::Transport;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    Desktop,
    Server,
    Cli,
    Service,
}

impl NodeKind {
    fn as_str(&self) -> &'static str {
        match self {
            NodeKind::Desktop => "desktop",
            NodeKind::Server => "server",
            NodeKind::Cli => "cli",
            NodeKind::Service => "service",
        }
    }
}

pub struct Node {
    name: String,
    kind: NodeKind,
    capabilities: Arc<CapabilitySet>,
}

pub struct NodeBuilder {
    name: String,
    kind: NodeKind,
    capabilities: Vec<Arc<dyn ServeCapability>>,
}

impl Node {
    pub fn builder() -> NodeBuilder {
        NodeBuilder {
            name: "zyris-node".to_string(),
            kind: NodeKind::Service,
            capabilities: Vec::new(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn kind(&self) -> &NodeKind {
        &self.kind
    }

    #[cfg(feature = "runtime")]
    pub(crate) fn with_capabilities(
        name: String,
        kind: NodeKind,
        capabilities: Arc<CapabilitySet>,
    ) -> Node {
        Node { name, kind, capabilities }
    }

    /// What this node offers, changeable while it is connected. See [`Capabilities`].
    pub fn capabilities(&self) -> Capabilities {
        Capabilities::new(self.capabilities.clone())
    }

    fn agent(&self) -> String {
        format!(
            "zyris/{} ({}; {})",
            env!("CARGO_PKG_VERSION"),
            self.name,
            self.kind.as_str()
        )
    }

    pub async fn connect_over(&self, transport: impl Transport) -> Result<Connection> {
        establish(
            Box::new(transport),
            Role::Dial { agent: self.agent() },
            self.capabilities.clone(),
        )
        .await
    }

    #[cfg(feature = "client")]
    pub async fn connect(&self, url: &str, token: &str) -> Result<Connection> {
        let transport = crate::transport::ws::connect(url, token).await?;
        self.connect_over(transport).await
    }

    pub async fn accept(
        &self,
        transport: impl Transport,
        options: AcceptOptions,
    ) -> Result<Connection> {
        establish(
            Box::new(transport),
            Role::Accept { options },
            self.capabilities.clone(),
        )
        .await
    }
}

impl NodeBuilder {
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Name this node after the machine it runs on.
    ///
    /// A machine that has nothing usable to say about itself leaves the current name in place, so
    /// this is safe to chain after `name` as an override and before it as a default.
    #[cfg(feature = "hostname")]
    pub fn name_from_hostname(mut self) -> Self {
        if let Some(name) = crate::machine_name() {
            self.name = name;
        }
        self
    }

    pub fn kind(mut self, kind: NodeKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn capability(mut self, capability: impl ServeCapability) -> Self {
        self.capabilities.push(Arc::new(capability));
        self
    }

    pub fn capability_arc(mut self, capability: Arc<dyn ServeCapability>) -> Self {
        self.capabilities.push(capability);
        self
    }

    pub fn build(self) -> Result<Node> {
        let capabilities = CapabilitySet::new();
        for cap in self.capabilities {
            capabilities.push(cap)?;
        }
        Ok(Node { name: self.name, kind: self.kind, capabilities })
    }
}
