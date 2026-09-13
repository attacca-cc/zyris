//! What this node announces, and the one place it changes.
//!
//! The protocol makes changing it easy: `zyris.announce` is full-replacement, and
//! [`zyris::Capabilities`] re-announces the whole set on every live connection and survives the
//! reconnects the link makes underneath. What is hard is **holding on to that handle**, and
//! getting that wrong has two shapes that both fail quietly.
//!
//! # Why a handle is not enough on its own
//!
//! `Node::capabilities()` exists only after `NodeBuilder::build()`, which happens inside
//! [`Connector::dial`](crate::connection::Connector::dial) — and `dial` runs **up to twice**: a
//! node token Attacca refuses outright is discarded, a fresh one minted, and a second node built
//! and dialled with it. So there can be two nodes in one run, and only the second one is
//! connected to anything. A handle stored once, at the first build, would re-announce onto a node
//! nobody is talking to, and every later change would be accepted and have no effect at all.
//!
//! The other half is the mirror of it. A change made while **no** node exists — a server that
//! fell over during startup, before the first dial, or between the two dials — has to survive
//! into whatever node is built next. A handle that is simply absent until a node exists silently
//! drops those, and the withdrawn server comes back announced.
//!
//! So this type holds **both**: the list, which is the authority and which every node is built
//! from, and the current node's handle, which is replaced on every build. A change edits the list
//! and, when there is a node, re-announces through it.
//!
//! # `with_connect_hook` is the wrong shape, and it is worth saying why
//!
//! [`Connector::with_connect_hook`](crate::connection::Connector::with_connect_hook) already runs
//! on every established connection and is how `file_transfer` republishes its address, so it
//! looks like the seam for this too. It is not, for three reasons that are each enough on their
//! own:
//!
//! - **It is handed a [`zyris::Connection`], not a `Node`.** A connection can say what it
//!   announced (`local_descriptors`) but has no way to change it; `Capabilities` is the node's.
//! - **It fires per *connection*, and the capability set is per *node*.** A node that has not
//!   connected yet — dialling, backing off, or refused — still has a capability set that has to
//!   be right when it does connect.
//! - **A dead server has to be withdrawn while this machine is offline.** If the only moment a
//!   handle could be captured were an established connection, then a node that lost its link and
//!   is backing off would announce the dead server the moment it got back — which is precisely
//!   the window in which nothing else can correct it.
//!
//! # Adding a duplicate name is the dangerous one
//!
//! `zyris-core`'s `Served::build` refuses a second capability with the same `(name, version)`,
//! and everything goes through it: [`zyris::Capabilities::replace`], `add_arc`, and
//! `NodeBuilder::build`. Measured rather than assumed (2026-09-14) — see this module's tests —
//! and the three failures are not equally survivable:
//!
//! - Through `replace`/`add_arc` on a live node: an `Err`, and **nothing is swapped**, so the set
//!   that was announced stays announced. Survivable.
//! - Through `NodeBuilder::build`: the node is never built, so `dial` publishes `Disconnected`
//!   and **this machine never connects at all** — not `terminal`, not `file_io`, nothing.
//!
//! A duplicate reaching the list while no node exists would therefore be stored quietly and take
//! the machine off the network at the next dial, which is why [`LiveCapabilities::add`] refuses
//! one **itself**, whether or not there is a node to ask.

use std::sync::Arc;

use tokio::sync::Mutex;
use zyris::{ErrorCode, ServeCapability, WireError};

/// What this node announces: the list, and a handle on whichever node is serving it.
///
/// Clone it freely — every clone is the same announcement, which is the point. `main` builds one,
/// hands it to the [`Connector`](crate::connection::Connector), and keeps a clone for whatever
/// changes it later.
#[derive(Clone, Default)]
pub struct LiveCapabilities(Arc<Mutex<State>>);

#[derive(Default)]
struct State {
    /// The authority. Every node is built from this and every change edits it, so a node built
    /// after a change announces the change without anybody having to remember to tell it.
    announced: Vec<Arc<dyn ServeCapability>>,
    /// The node currently serving that list, or `None` before the first one is built. Replaced on
    /// every build, so a handle on a node that has been given up on is dropped rather than
    /// written to.
    node: Option<zyris::Capabilities>,
}

impl LiveCapabilities {
    /// What this node announces when it starts.
    pub fn new(capabilities: Vec<Arc<dyn ServeCapability>>) -> LiveCapabilities {
        LiveCapabilities(Arc::new(Mutex::new(State { announced: capabilities, node: None })))
    }

    /// The names announced right now, in announcement order.
    pub async fn names(&self) -> Vec<String> {
        self.0.lock().await.announced.iter().map(|c| c.descriptor().name).collect()
    }

    /// Announce one more thing.
    ///
    /// Refused, changing nothing, when something with the same `(name, version)` is already
    /// announced — see [the module documentation](self#adding-a-duplicate-name-is-the-dangerous-one).
    /// The check is made here and not only by the node, because there may not be a node yet.
    pub async fn add(&self, capability: Arc<dyn ServeCapability>) -> zyris::Result<()> {
        let mut state = self.0.lock().await;
        let descriptor = capability.descriptor();
        if state
            .announced
            .iter()
            .any(|seen| {
                let seen = seen.descriptor();
                seen.name == descriptor.name && seen.version == descriptor.version
            })
        {
            return Err(WireError::new(
                ErrorCode::InvalidParams,
                format!(
                    "this node already announces `{}` v{}, and a node that announces one name \
                     twice announces nothing at all",
                    descriptor.name, descriptor.version
                ),
            ));
        }

        let mut next = state.announced.clone();
        next.push(capability);
        state.publish(next).await
    }

    /// Stop announcing `name`, and say whether it was there.
    ///
    /// A call already running against it is answered with
    /// [`ErrorCode::CapabilityUnavailable`] — `zyris-core` revokes it as part of the swap, so the
    /// caller is never left waiting on a capability the peer has been told is gone.
    pub async fn remove(&self, name: &str) -> bool {
        let mut state = self.0.lock().await;
        let next: Vec<Arc<dyn ServeCapability>> = state
            .announced
            .iter()
            .filter(|capability| capability.descriptor().name != name)
            .cloned()
            .collect();
        if next.len() == state.announced.len() {
            return false;
        }
        // A removal cannot produce a duplicate, so the only way this fails is the node's
        // connection being gone — which `propagate` already tolerates.
        state.publish(next).await.is_ok()
    }

    /// Build a node from what is announced and take its handle, with nothing able to slip in
    /// between.
    ///
    /// **The two steps are one step on purpose.** Reading the list, building the node and storing
    /// the handle as three separate operations leaves a gap: a capability added in the middle
    /// would go into the list, find no node to announce it on, and then be missing from the node
    /// that was about to be built out of a list read before it arrived. Taking the closure keeps
    /// the lock across all three.
    ///
    /// A build that fails leaves the previous handle in place rather than clearing it — the node
    /// that failed to build never served anything, and forgetting a node that is still up because
    /// a later one could not be built would be a worse answer than keeping it.
    pub async fn install<E>(
        &self,
        build: impl FnOnce(&[Arc<dyn ServeCapability>]) -> Result<zyris::Node, E>,
    ) -> Result<zyris::Node, E> {
        let mut state = self.0.lock().await;
        let node = build(&state.announced)?;
        state.node = Some(node.capabilities());
        Ok(node)
    }
}

impl State {
    /// Swap the list in, and tell the node if there is one.
    ///
    /// The list is written only when the node accepted the change, so the two can never disagree
    /// about what is announced. With no node the list is the whole answer, and the next one built
    /// will announce it.
    async fn publish(&mut self, next: Vec<Arc<dyn ServeCapability>>) -> zyris::Result<()> {
        if let Some(node) = &self.node {
            node.replace(next.clone()).await?;
        }
        self.announced = next;
        Ok(())
    }
}

impl std::fmt::Debug for LiveCapabilities {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Not the capabilities themselves: a descriptor carries every tool's JSON schema, and a
        // derived `Debug` would put all of them into any log line that formatted one. Locking is
        // out of the question here anyway — `Debug` is not async.
        f.debug_struct("LiveCapabilities").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::sync::Notify;
    use zyris::{
        CapabilityDescriptor, ErrorCode, IncomingCall, Node, NodeKind, Outgoing, Payload,
        ServeCapability, ToolDescriptor, Transfer, encode_response,
    };

    use super::LiveCapabilities;

    /// A capability that answers with its own name, so a test can tell which one a call reached.
    struct Named {
        name: String,
        /// Signalled the moment a call has actually started, so a test that means to interfere
        /// with a call in flight is not merely hoping one has begun.
        started: Arc<Notify>,
        /// Held until the test lets go. `None` answers at once.
        release: Option<Arc<Notify>>,
    }

    impl Named {
        fn new(name: &str) -> Arc<dyn ServeCapability> {
            Arc::new(Named { name: name.to_string(), started: Arc::new(Notify::new()), release: None })
        }
    }

    #[zyris::async_trait]
    impl ServeCapability for Named {
        fn descriptor(&self) -> CapabilityDescriptor {
            CapabilityDescriptor {
                name: self.name.clone(),
                version: 1,
                tools: vec![ToolDescriptor {
                    name: "who".to_string(),
                    description: "Answer with this capability's own name.".to_string(),
                    transfer: Transfer::Unary,
                    request_schema: serde_json::json!({ "type": "object" }),
                    response_schema: None,
                    item_schema: None,
                    call_limit: None,
                }],
            }
        }

        async fn dispatch(&self, _call: IncomingCall) -> zyris::Result<Outgoing> {
            self.started.notify_waiters();
            if let Some(release) = &self.release {
                release.notified().await;
            }
            encode_response(&serde_json::json!({ "who": self.name }))
        }
    }

    /// A node built the way [`crate::connection::Connector::dial`] builds one: through `install`,
    /// so the handle this module hands out is the one the test's node is actually serving.
    async fn build(live: &LiveCapabilities) -> Node {
        live.install(|capabilities| {
            let mut builder = Node::builder().name("this-machine").kind(NodeKind::Desktop);
            for capability in capabilities {
                builder = builder.capability_arc(capability.clone());
            }
            builder.build()
        })
        .await
        .expect("the node builds")
    }

    fn bare(name: &str) -> Node {
        Node::builder().name(name).kind(NodeKind::Cli).build().expect("a node with nothing on it")
    }

    /// What the other end can see, which is the only thing that matters.
    async fn announced_to_the_peer(connection: &zyris::Connection) -> Vec<String> {
        connection.peer_descriptors().into_iter().map(|d| d.name).collect()
    }

    /// The peer's view catches up over the wire, so this waits for it instead of reading once.
    /// The failure it must not hide is a change that never arrives, which is what the deadline is.
    async fn wait_until_announced(connection: &zyris::Connection, expected: &[&str]) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let mut names = announced_to_the_peer(connection).await;
            names.sort();
            let mut want: Vec<String> = expected.iter().map(|n| (*n).to_string()).collect();
            want.sort();
            if names == want {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the peer still sees {names:?}, expected {want:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn a_capability_added_before_any_node_exists_is_announced_by_the_first_one() {
        // The gap between `main` building the list and the first dial. A server that fell over —
        // or one somebody enabled — in that window has to be right in the node that is built
        // next, and there is no handle to tell.
        let live = LiveCapabilities::new(vec![Named::new("terminal")]);

        live.add(Named::new("mcp_notes")).await.expect("nothing is announced under that name yet");

        let node = build(&live).await;
        let peer = bare("agent");
        let (agent, _node_side) = zyris::testing::duplex(&peer, &node).await.expect("they connect");

        wait_until_announced(&agent, &["terminal", "mcp_notes"]).await;

    }

    #[tokio::test]
    async fn a_capability_removed_before_any_node_exists_is_not_announced_by_the_first_one() {
        let live = LiveCapabilities::new(vec![Named::new("terminal"), Named::new("mcp_notes")]);

        assert!(live.remove("mcp_notes").await, "it was announced");

        let node = build(&live).await;
        let peer = bare("agent");
        let (agent, _node_side) = zyris::testing::duplex(&peer, &node).await.expect("they connect");

        wait_until_announced(&agent, &["terminal"]).await;
    }

    #[tokio::test]
    async fn a_second_node_announces_what_changed_while_the_first_one_was_alive() {
        // `dial` runs up to twice: a refused node token is discarded and a second node built. A
        // change made while the first node was up must not be undone by the second one, and a
        // handle kept from the first must not be the one a later change reaches.
        let live = LiveCapabilities::new(vec![Named::new("terminal"), Named::new("mcp_notes")]);
        let first = build(&live).await;

        assert!(live.remove("mcp_notes").await);
        live.add(Named::new("mcp_calendar")).await.expect("a name nothing else uses");
        drop(first);

        let second = build(&live).await;
        let peer = bare("agent");
        let (agent, _node_side) = zyris::testing::duplex(&peer, &second).await.expect("they connect");

        wait_until_announced(&agent, &["terminal", "mcp_calendar"]).await;
    }

    #[tokio::test]
    async fn a_capability_added_while_connected_is_announced_without_a_reconnect() {
        let live = LiveCapabilities::new(vec![Named::new("terminal")]);
        let node = build(&live).await;
        let peer = bare("agent");
        let (agent, _node_side) = zyris::testing::duplex(&peer, &node).await.expect("they connect");
        wait_until_announced(&agent, &["terminal"]).await;

        live.add(Named::new("mcp_notes")).await.expect("a new name");

        wait_until_announced(&agent, &["terminal", "mcp_notes"]).await;
        // And it is callable on the very same connection, which is the half that announcing alone
        // does not prove.
        let answer = agent
            .call_raw("mcp_notes.who", Payload::default())
            .await
            .expect("the new capability answers on the connection it was added to");
        assert_eq!(answer.to_json().unwrap()["who"], "mcp_notes");
        assert!(!agent.is_closed(), "nothing reconnected");
    }

    #[tokio::test]
    async fn a_capability_removed_while_connected_stops_being_announced_and_refuses_a_later_call() {
        let live = LiveCapabilities::new(vec![Named::new("terminal"), Named::new("mcp_notes")]);
        let node = build(&live).await;
        let peer = bare("agent");
        let (agent, _node_side) = zyris::testing::duplex(&peer, &node).await.expect("they connect");
        wait_until_announced(&agent, &["terminal", "mcp_notes"]).await;

        assert!(live.remove("mcp_notes").await);

        wait_until_announced(&agent, &["terminal"]).await;
        let refused = agent
            .call_raw("mcp_notes.who", Payload::default())
            .await
            .expect_err("a withdrawn capability cannot be called");
        assert_eq!(refused.code, ErrorCode::CapabilityNotAnnounced);
        // The rest of the machine is untouched: a withdrawal is about one capability.
        let still_there = agent
            .call_raw("terminal.who", Payload::default())
            .await
            .expect("the others keep working");
        assert_eq!(still_there.to_json().unwrap()["who"], "terminal");
    }

    #[tokio::test]
    async fn removing_something_that_is_not_announced_changes_nothing_and_says_so() {
        let live = LiveCapabilities::new(vec![Named::new("terminal")]);

        assert!(!live.remove("mcp_notes").await, "it was never announced");
        assert_eq!(live.names().await, ["terminal"]);
    }

    #[tokio::test]
    async fn a_call_in_flight_when_its_capability_is_removed_is_told_the_capability_is_gone() {
        // What the agent on the other end actually sees, which the plan asked to be established
        // rather than assumed. It matters because the alternative is worse than an error: a call
        // that hangs on a capability the peer has already been told does not exist.
        let release = Arc::new(Notify::new());
        let started = Arc::new(Notify::new());
        let slow: Arc<dyn ServeCapability> = Arc::new(Named {
            name: "mcp_notes".to_string(),
            started: started.clone(),
            release: Some(release.clone()),
        });
        let live = LiveCapabilities::new(vec![slow]);
        let node = build(&live).await;
        let peer = bare("agent");
        let (agent, _node_side) = zyris::testing::duplex(&peer, &node).await.expect("they connect");
        wait_until_announced(&agent, &["mcp_notes"]).await;

        let waiting = started.notified();
        let calling = tokio::spawn(async move {
            agent.call_raw("mcp_notes.who", Payload::default()).await
        });
        // Not a sleep: the call has to have reached the capability, or this would be testing a
        // removal that raced ahead of the call rather than one that interrupted it.
        tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .expect("the call reaches the capability");

        assert!(live.remove("mcp_notes").await);

        let answer = tokio::time::timeout(Duration::from_secs(5), calling)
            .await
            .expect("the call is answered rather than left hanging")
            .expect("the calling task did not panic");
        let error = answer.expect_err("a revoked call cannot succeed");
        assert_eq!(error.code, ErrorCode::CapabilityUnavailable);
        assert!(
            error.message.contains("mcp_notes"),
            "the caller has to be told which capability went away: {}",
            error.message
        );
        // The call never finished, so nothing can be waiting on the release.
        release.notify_waiters();
    }

    #[tokio::test]
    async fn adding_a_name_that_is_already_announced_is_refused_and_changes_nothing() {
        // The live hazard. `Served::build` refuses a duplicate `(name, version)`, and a node that
        // trips it announces nothing at all — so an add that quietly stored one would take the
        // machine off the network at the next dial rather than at the moment of the mistake.
        let live = LiveCapabilities::new(vec![Named::new("terminal"), Named::new("mcp_notes")]);
        let node = build(&live).await;
        let peer = bare("agent");
        let (agent, _node_side) = zyris::testing::duplex(&peer, &node).await.expect("they connect");
        wait_until_announced(&agent, &["terminal", "mcp_notes"]).await;

        let refused = live
            .add(Named::new("mcp_notes"))
            .await
            .expect_err("a second capability of that name cannot be announced");
        assert!(
            refused.message.contains("mcp_notes"),
            "the refusal has to name it: {}",
            refused.message
        );

        // Nothing moved: not the list, and not what the peer can see and call.
        assert_eq!(live.names().await, ["terminal", "mcp_notes"]);
        wait_until_announced(&agent, &["terminal", "mcp_notes"]).await;
        let answer = agent
            .call_raw("mcp_notes.who", Payload::default())
            .await
            .expect("the one that was already there still answers");
        assert_eq!(answer.to_json().unwrap()["who"], "mcp_notes");
    }

    #[tokio::test]
    async fn adding_a_duplicate_with_no_node_yet_is_refused_too() {
        // The same rule where the node cannot enforce it. Storing one here would be stored
        // silently and spent at the next dial.
        let live = LiveCapabilities::new(vec![Named::new("mcp_notes")]);

        live.add(Named::new("mcp_notes")).await.expect_err("one name, one capability");

        assert_eq!(live.names().await, ["mcp_notes"]);
    }

    /// What a duplicate costs if one ever reaches a node, kept as a fact rather than as a belief.
    ///
    /// This is the reason [`LiveCapabilities::add`] refuses one itself. Both halves are asserted
    /// because they are different failures: `add_arc` on a live node is survivable and `build` is
    /// not — a node that will not build never connects, so `terminal` and `file_io` go with it.
    #[tokio::test]
    async fn the_protocol_refuses_a_duplicate_at_both_ends() {
        let one = Named::new("mcp_notes");
        let two = Named::new("mcp_notes");

        // Through the builder: no node at all.
        let built = Node::builder()
            .name("this-machine")
            .kind(NodeKind::Desktop)
            .capability_arc(one.clone())
            .capability_arc(two.clone())
            .build();
        let error = built.err().expect("a node cannot be built announcing one name twice");
        assert_eq!(error.code, ErrorCode::InvalidParams);

        // Through the live handle: an error, and the set that was announced survives it.
        let live = LiveCapabilities::new(vec![one]);
        let node = build(&live).await;
        let handle = node.capabilities();
        handle.add_arc(two).await.expect_err("the same name twice");
        assert_eq!(
            handle.descriptors().into_iter().map(|d| d.name).collect::<Vec<_>>(),
            ["mcp_notes"],
            "a refused add must leave the announcement alone"
        );
    }
}
