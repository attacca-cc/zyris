use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock, Weak};

use zyris_proto::CapabilityDescriptor;

use crate::connection::{Connection, Shared};
use crate::error::{Result, WireError};
use crate::serve::ServeCapability;

#[derive(Default)]
struct Served {
    capabilities: Vec<Arc<dyn ServeCapability>>,
    descriptors: Vec<CapabilityDescriptor>,
    by_name: HashMap<String, Arc<dyn ServeCapability>>,
}

impl Served {
    fn build(capabilities: Vec<Arc<dyn ServeCapability>>) -> Result<Served> {
        let mut descriptors: Vec<CapabilityDescriptor> = Vec::with_capacity(capabilities.len());
        let mut by_name = HashMap::new();
        for capability in &capabilities {
            let descriptor = capability.descriptor();
            if descriptors
                .iter()
                .any(|seen| seen.name == descriptor.name && seen.version == descriptor.version)
            {
                return Err(WireError::invalid_params(format!(
                    "duplicate capability {} v{}",
                    descriptor.name, descriptor.version
                )));
            }
            by_name.insert(descriptor.name.clone(), capability.clone());
            descriptors.push(descriptor);
        }
        Ok(Served { capabilities, descriptors, by_name })
    }
}

pub(crate) struct CapabilitySet {
    served: RwLock<Served>,
    live: Mutex<Vec<Weak<Shared>>>,
}

impl CapabilitySet {
    pub(crate) fn new() -> Arc<CapabilitySet> {
        Arc::new(CapabilitySet {
            served: RwLock::new(Served::default()),
            live: Mutex::new(Vec::new()),
        })
    }

    pub(crate) fn push(&self, capability: Arc<dyn ServeCapability>) -> Result<()> {
        let mut next = self.served.read().unwrap().capabilities.clone();
        next.push(capability);
        let built = Served::build(next)?;
        *self.served.write().unwrap() = built;
        Ok(())
    }

    pub(crate) fn get(&self, name: &str) -> Option<Arc<dyn ServeCapability>> {
        self.served.read().unwrap().by_name.get(name).cloned()
    }

    pub(crate) fn descriptors(&self) -> Vec<CapabilityDescriptor> {
        self.served.read().unwrap().descriptors.clone()
    }

    pub(crate) fn len(&self) -> usize {
        self.served.read().unwrap().capabilities.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(crate) fn register(&self, shared: &Arc<Shared>) {
        let mut live = self.live.lock().unwrap();
        live.retain(|held| held.strong_count() > 0);
        live.push(Arc::downgrade(shared));
    }

    fn live(&self) -> Vec<Arc<Shared>> {
        let mut live = self.live.lock().unwrap();
        live.retain(|held| held.strong_count() > 0);
        live.iter().filter_map(Weak::upgrade).collect()
    }
}

/// What this node offers, changeable while it is connected.
///
/// `zyris.announce` is full-replacement (protocol §5), so every change here re-announces the whole
/// set on every live connection. Dropping a capability revokes it: the peer stops seeing its tools,
/// later calls get `capability_not_announced`, and calls already running against it fail with
/// `capability_unavailable` rather than landing after the peer was told they could not happen.
///
/// The set outlives any one connection, so a change made while connected also survives the
/// reconnect loop in [`Runner`](crate::runtime::Runner).
#[derive(Clone)]
pub struct Capabilities(Arc<CapabilitySet>);

impl Capabilities {
    pub(crate) fn new(set: Arc<CapabilitySet>) -> Capabilities {
        Capabilities(set)
    }

    pub fn descriptors(&self) -> Vec<CapabilityDescriptor> {
        self.0.descriptors()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.0.get(name).is_some()
    }

    pub async fn add(&self, capability: impl ServeCapability) -> Result<()> {
        self.add_arc(Arc::new(capability)).await
    }

    pub async fn add_arc(&self, capability: Arc<dyn ServeCapability>) -> Result<()> {
        let mut next = self.0.served.read().unwrap().capabilities.clone();
        next.push(capability);
        self.replace(next).await
    }

    /// Stop offering `name`. Returns whether it was there to begin with.
    pub async fn remove(&self, name: &str) -> bool {
        let next: Vec<Arc<dyn ServeCapability>> = {
            let served = self.0.served.read().unwrap();
            if !served.by_name.contains_key(name) {
                return false;
            }
            served
                .capabilities
                .iter()
                .zip(served.descriptors.iter())
                .filter(|(_, descriptor)| descriptor.name != name)
                .map(|(capability, _)| capability.clone())
                .collect()
        };
        self.replace(next).await.is_ok()
    }

    /// The full-replacement primitive the other two are written in terms of.
    pub async fn replace(&self, capabilities: Vec<Arc<dyn ServeCapability>>) -> Result<()> {
        let built = Served::build(capabilities)?;
        // Swap before propagating, so a call that arrives during the re-announce is already
        // refused rather than accepted by a set the peer has been told is gone.
        let revoked = {
            let mut served = self.0.served.write().unwrap();
            let revoked: Vec<String> = served
                .by_name
                .keys()
                .filter(|name| !built.by_name.contains_key(*name))
                .cloned()
                .collect();
            *served = built;
            revoked
        };
        self.propagate(&revoked).await;
        Ok(())
    }

    async fn propagate(&self, revoked: &[String]) {
        for shared in self.0.live() {
            let conn = Connection::from_shared(shared);
            if conn.is_closed() {
                continue;
            }
            for name in revoked {
                conn.revoke_inflight(name);
            }
            if let Err(e) = conn.announce().await {
                tracing::debug!(error = %e, "re-announce after a capability change failed");
            }
        }
    }
}
