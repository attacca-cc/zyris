//! Proves the pair the macro generates from this declaration still fits itself: a stub server on
//! one end of an in-memory duplex, the generated client on the other. The real implementation is
//! Attacca's; what is checked here is the shape both sides agree on.

use std::time::Duration;

use futures_util::StreamExt;
use zyris::{Datum, Node, NodeKind, Streaming, Transfer};
use zyris_attacca::{
    attacca_api_capability, AttaccaApi, AttaccaApiClient, AttaccaApiServer, ZAgent, ZDeltaKind,
    ZNewAgent, ZSession, ZSessionEvent, ZSessionFilter, ZTurnFrame, ZTurnStatus,
    ATTACCA_API_CAPABILITY,
};

struct StubApi;

#[zyris::async_trait]
impl AttaccaApi for StubApi {
    async fn list_agents(&self) -> zyris::Result<Vec<ZAgent>> {
        Ok(vec![ZAgent {
            id: "agent-1".into(),
            name: "Researcher".into(),
            description: None,
            model: Some("claude-opus-5".into()),
        }])
    }

    async fn create_agent(&self, agent: ZNewAgent) -> zyris::Result<ZAgent> {
        Ok(ZAgent {
            id: "agent-2".into(),
            name: agent.name,
            description: agent.description,
            model: agent.model,
        })
    }

    async fn list_sessions(&self, filter: ZSessionFilter) -> zyris::Result<Vec<ZSession>> {
        Ok(vec![ZSession {
            id: "session-1".into(),
            title: None,
            agent_id: Some("agent-1".into()),
            project_id: filter.project_id,
            running: false,
        }])
    }

    async fn create_session(
        &self,
        agent_id: String,
        title: Option<String>,
        project_id: Option<String>,
    ) -> zyris::Result<ZSession> {
        Ok(ZSession {
            id: "session-2".into(),
            title,
            agent_id: Some(agent_id),
            project_id,
            running: false,
        })
    }

    async fn send_message(
        &self,
        _session_id: String,
        _message: String,
        _data: Vec<Datum>,
    ) -> zyris::Result<()> {
        Ok(())
    }

    async fn cancel_turn(&self, _session_id: String) -> zyris::Result<()> {
        Ok(())
    }

    async fn turn_events(
        &self,
        session_id: String,
        after: Option<i64>,
    ) -> zyris::Result<Streaming<ZTurnStatus, ZTurnFrame>> {
        let head = ZTurnStatus { session_id, running: true, last_cursor: after };
        let frames = vec![
            Ok(ZTurnFrame::Event {
                cursor: 7,
                event: ZSessionEvent {
                    seq: 1,
                    cursor: 7,
                    kind: "assistant_message".into(),
                    payload: serde_json::json!({ "text": "hi" }),
                },
            }),
            Ok(ZTurnFrame::Delta { kind: ZDeltaKind::Assistant, text: "hi".into() }),
            Ok(ZTurnFrame::Status { running: false }),
        ];
        Ok(Streaming::new(head, futures_util::stream::iter(frames)))
    }
}

fn server_node() -> Node {
    Node::builder()
        .name("attacca")
        .kind(NodeKind::Service)
        .capability(AttaccaApiServer(StubApi))
        .build()
        .unwrap()
}

async fn client() -> AttaccaApiClient {
    let node = Node::builder().name("node").kind(NodeKind::Service).build().unwrap();
    let (_server_side, node_side) = zyris::testing::duplex(&server_node(), &node).await.unwrap();
    node_side.wait_capability(Duration::from_secs(2)).await.unwrap()
}

#[test]
fn descriptor_matches_the_reserved_name() {
    let descriptor = attacca_api_capability();
    assert_eq!(descriptor.name, ATTACCA_API_CAPABILITY);
    assert_eq!(descriptor.version, 1);
    assert_eq!(descriptor.tools.len(), 7);
    assert_eq!(descriptor.tool("list_agents").unwrap().transfer, Transfer::Unary);
    assert_eq!(descriptor.tool("turn_events").unwrap().transfer, Transfer::UniStream);
}

#[tokio::test]
async fn node_calls_the_unary_tools() {
    let api = client().await;

    let agents = api.list_agents().await.unwrap();
    assert_eq!(agents[0].name, "Researcher");

    let session = api.create_session("agent-1".into(), Some("Rollout".into()), None).await.unwrap();
    assert_eq!(session.agent_id.as_deref(), Some("agent-1"));
    assert_eq!(session.title.as_deref(), Some("Rollout"));

    api.send_message("session-1".into(), "go".into(), vec![]).await.unwrap();
    api.cancel_turn("session-1".into()).await.unwrap();
}

#[tokio::test]
async fn turn_events_streams_head_then_frames() {
    let api = client().await;

    let mut stream = api.turn_events("session-1".into(), Some(3)).await.unwrap();
    assert_eq!(stream.head.session_id, "session-1");
    assert!(stream.head.running);
    assert_eq!(stream.head.last_cursor, Some(3));

    let mut frames = Vec::new();
    while let Some(frame) = stream.items.next().await {
        frames.push(frame.unwrap());
    }
    assert_eq!(frames.len(), 3);
    assert!(matches!(frames[0], ZTurnFrame::Event { cursor: 7, .. }));
    assert!(matches!(frames[1], ZTurnFrame::Delta { kind: ZDeltaKind::Assistant, .. }));
    assert!(matches!(frames[2], ZTurnFrame::Status { running: false }));
}
