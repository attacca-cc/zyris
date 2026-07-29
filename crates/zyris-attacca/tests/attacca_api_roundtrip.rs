//! Proves the pair the macro generates from this declaration still fits itself: a stub server on
//! one end of an in-memory duplex, the generated client on the other. The real implementation is
//! Attacca's; what is checked here is the shape both sides agree on.

use std::time::Duration;

use futures_util::StreamExt;
use zyris::{Datum, Node, NodeKind, Streaming, Transfer};
use zyris_attacca::{
    attacca_api_capability, AttaccaApi, AttaccaApiClient, AttaccaApiServer, ZAgent, ZDeltaKind,
    ZHistoryQuery, ZMe, ZNewAgent, ZNewSession, ZProject, ZScope, ZSession, ZSessionEvent,
    ZSessionFilter, ZTurnFrame, ZTurnStatus, ATTACCA_API_CAPABILITY,
};

struct StubApi;

#[zyris::async_trait]
impl AttaccaApi for StubApi {
    async fn me(&self) -> zyris::Result<ZMe> {
        Ok(ZMe {
            user_id: "user-1".into(),
            email: "ada@example.com".into(),
            display_name: "Ada".into(),
            scopes: vec![
                ZScope::AgentsRead.as_str().into(),
                ZScope::ProjectsRead.as_str().into(),
                // A scope this crate does not know: a newer deployment's grant, which must not
                // take the whole call down with it.
                "starships:read".into(),
            ],
        })
    }

    async fn list_projects(&self) -> zyris::Result<Vec<ZProject>> {
        Ok(vec![
            ZProject {
                id: "project-1".into(),
                name: "Default".into(),
                description: None,
                is_default: true,
            },
            ZProject {
                id: "project-2".into(),
                name: "Rollout".into(),
                description: Some("Q3".into()),
                is_default: false,
            },
        ])
    }

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
            preamble: None,
        }])
    }

    async fn create_session(
        &self,
        agent_id: String,
        title: Option<String>,
        project_id: Option<String>,
    ) -> zyris::Result<ZSession> {
        self.create_session_with(ZNewSession { agent_id, title, project_id, preamble: None })
            .await
    }

    async fn create_session_with(&self, session: ZNewSession) -> zyris::Result<ZSession> {
        Ok(ZSession {
            id: "session-2".into(),
            title: session.title,
            agent_id: Some(session.agent_id),
            project_id: session.project_id,
            running: false,
            preamble: session.preamble,
        })
    }

    async fn session_history(
        &self,
        _session_id: String,
        query: ZHistoryQuery,
    ) -> zyris::Result<Vec<ZSessionEvent>> {
        let all = vec![
            ZSessionEvent {
                seq: 1,
                cursor: 1,
                kind: "chat_user".into(),
                payload: serde_json::json!({ "kind": "chat_user", "content": "go" }),
                created_at: Some("2026-07-29T00:00:00Z".into()),
            },
            ZSessionEvent {
                seq: 2,
                cursor: 2,
                kind: "chat_agent".into(),
                payload: serde_json::json!({ "kind": "chat_agent", "content": "done" }),
                created_at: Some("2026-07-29T00:00:01Z".into()),
            },
        ];
        let after = query.after.unwrap_or(0);
        let mut out: Vec<ZSessionEvent> = all.into_iter().filter(|e| e.cursor > after).collect();
        if let Some(limit) = query.limit {
            out.truncate(limit as usize);
        }
        Ok(out)
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
                    created_at: None,
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
    assert_eq!(descriptor.tools.len(), 11);
    assert_eq!(descriptor.tool("list_agents").unwrap().transfer, Transfer::Unary);
    assert_eq!(descriptor.tool("me").unwrap().transfer, Transfer::Unary);
    assert_eq!(descriptor.tool("list_projects").unwrap().transfer, Transfer::Unary);
    assert_eq!(descriptor.tool("turn_events").unwrap().transfer, Transfer::UniStream);

    // Tools added within v1: a node discovers them here, and one built before them keeps calling
    // `create_session` unaffected.
    assert_eq!(descriptor.tool("create_session").unwrap().transfer, Transfer::Unary);
    assert_eq!(descriptor.tool("create_session_with").unwrap().transfer, Transfer::Unary);
    assert_eq!(descriptor.tool("session_history").unwrap().transfer, Transfer::Unary);
}

/// The steer toward an unset `title` — leave it off and Attacca's title agent names the session
/// from its first message — reaches a caller only as prose, so nothing but this test notices if it
/// is dropped. It travels by two different routes, hence two assertions: schemars carries a field
/// doc comment into the request schema, while a tool's own doc comment becomes its description. The
/// three-argument `create_session` has only the second route available, since the macro synthesizes
/// its request struct from the signature and arguments cannot carry doc comments.
#[test]
fn the_title_guidance_reaches_both_session_tools() {
    let descriptor = attacca_api_capability();

    // The one struct argument comes back as a `$ref`, so the field docs are under `$defs`.
    let schema = &descriptor.tool("create_session_with").unwrap().request_schema;
    let title = &schema["$defs"]["ZNewSession"]["properties"]["title"];
    let description = title["description"].as_str().unwrap_or_default();
    assert!(
        description.contains("unset") && description.contains("first message"),
        "ZNewSession::title lost its guidance: {description:?}",
    );

    let legacy = &descriptor.tool("create_session").unwrap().description;
    assert!(
        legacy.contains("null") && legacy.contains("first message"),
        "create_session lost its guidance: {legacy:?}",
    );
}

#[test]
fn scope_spellings_survive_a_round_trip() {
    for scope in ZScope::ALL {
        assert_eq!(ZScope::from_str(scope.as_str()), Some(scope));
        assert_eq!(scope.to_string(), scope.as_str());
        let json = serde_json::to_string(&scope).unwrap();
        assert_eq!(json, format!("\"{}\"", scope.as_str()), "serde and as_str disagree");
        assert_eq!(serde_json::from_str::<ZScope>(&json).unwrap(), scope);
    }
    assert_eq!(ZScope::from_str("starships:read"), None);
}

#[tokio::test]
async fn me_reports_identity_and_tolerates_an_unknown_scope() {
    let api = client().await;

    let me = api.me().await.unwrap();
    assert_eq!(me.email, "ada@example.com");
    assert_eq!(me.display_name, "Ada");
    assert!(me.has(ZScope::AgentsRead));
    assert!(!me.has(ZScope::SessionsWrite));

    // The unknown scope rides through as a string and drops out of the typed view.
    assert_eq!(me.scopes.len(), 3);
    assert_eq!(me.known_scopes(), vec![ZScope::AgentsRead, ZScope::ProjectsRead]);
}

#[tokio::test]
async fn list_projects_returns_the_default_and_the_rest() {
    let api = client().await;

    let projects = api.list_projects().await.unwrap();
    assert_eq!(projects.len(), 2);
    assert!(projects[0].is_default);
    assert_eq!(projects[0].name, "Default");
    assert_eq!(projects[1].description.as_deref(), Some("Q3"));
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
async fn create_session_with_carries_a_preamble() {
    let api = client().await;

    let session = api
        .create_session_with(ZNewSession {
            agent_id: "agent-1".into(),
            title: Some("Triage".into()),
            project_id: None,
            preamble: Some("Answer in exactly three words.".into()),
        })
        .await
        .unwrap();

    assert_eq!(session.agent_id.as_deref(), Some("agent-1"));
    assert_eq!(session.title.as_deref(), Some("Triage"));
    assert_eq!(session.preamble.as_deref(), Some("Answer in exactly three words."));

    // The shape a node should reach for by default: no title, so Attacca names the session itself.
    let untitled = api
        .create_session_with(ZNewSession {
            agent_id: "agent-1".into(),
            title: None,
            project_id: None,
            preamble: None,
        })
        .await
        .unwrap();
    assert_eq!(untitled.title, None);

    // The older three-argument tool still works and leaves the preamble unset.
    let plain = api.create_session("agent-1".into(), None, None).await.unwrap();
    assert_eq!(plain.preamble, None);
}

#[tokio::test]
async fn session_history_reads_the_timeline_back() {
    let api = client().await;

    let all = api.session_history("session-1".into(), ZHistoryQuery::default()).await.unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].kind, "chat_user");
    assert_eq!(all[0].payload["content"], "go");
    assert_eq!(all[1].kind, "chat_agent");
    assert!(all[1].created_at.is_some());

    // `after` is exclusive, and `limit` caps what comes back.
    let tail = api
        .session_history("session-1".into(), ZHistoryQuery { after: Some(1), limit: None })
        .await
        .unwrap();
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].cursor, 2);

    let capped = api
        .session_history("session-1".into(), ZHistoryQuery { after: None, limit: Some(1) })
        .await
        .unwrap();
    assert_eq!(capped.len(), 1);
    assert_eq!(capped[0].cursor, 1);
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
