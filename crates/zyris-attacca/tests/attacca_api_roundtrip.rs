//! Proves the pair the macro generates from this declaration still fits itself: a stub server on
//! one end of an in-memory duplex, the generated client on the other. The real implementation is
//! Attacca's; what is checked here is the shape both sides agree on.

use std::time::Duration;

use futures_util::StreamExt;
use zyris::{Datum, Node, NodeKind, Streaming, Transfer};
use zyris_attacca::{
    attacca_api_capability, AttaccaApi, AttaccaApiClient, AttaccaApiServer, ZAgent, ZDeltaKind,
    ZHistoryQuery, ZJob, ZJobFilter, ZJobState, ZJobUpdate, ZMe, ZNewAgent, ZNewJob, ZNewNode,
    ZNewProject, ZNewSession, ZNewWork, ZNode, ZPeerAddr, ZPeerEntry, ZProject, ZProjectUpdate,
    ZScope, ZSession, ZSessionEvent, ZSessionFilter, ZTask, ZTaskDep, ZTaskState, ZTurnFrame,
    ZTurnStatus, ZUsage, ZWork, ZWorkFilter, ZWorkState, ZWorkTasks, ZWorkUpdate,
    ATTACCA_API_CAPABILITY,
};

struct StubApi;

fn stub_job(id: &str, state: ZJobState) -> ZJob {
    ZJob {
        id: id.into(),
        title: "Summarize the changelog".into(),
        description: None,
        state,
        project_id: Some("project-1".into()),
        agent_id: Some("agent-1".into()),
        session_id: Some("session-1".into()),
        last_activity: Some("reading CHANGELOG.md".into()),
        failure_reason: None,
        planning: false,
        created_at: Some("2026-07-29T00:00:00Z".into()),
        updated_at: Some("2026-07-29T00:00:01Z".into()),
    }
}

fn stub_work(id: &str, state: ZWorkState) -> ZWork {
    ZWork {
        id: id.into(),
        title: "Port the parser".into(),
        description: None,
        state,
        project_id: Some("project-1".into()),
        agent_id: Some("agent-1".into()),
        planner_session_id: Some("session-9".into()),
        final_goal: Some("every fixture round-trips".into()),
        requirements_report: None,
        integration_branch: Some("work/work-1/integration".into()),
        failure_reason: None,
        last_activity: None,
        created_at: Some("2026-07-29T00:00:00Z".into()),
        updated_at: Some("2026-07-29T00:00:01Z".into()),
    }
}

fn stub_task(id: &str, work_id: &str, state: ZTaskState) -> ZTask {
    ZTask {
        id: id.into(),
        work_id: work_id.into(),
        title: "Move the lexer".into(),
        description: "Lift it out of the old crate".into(),
        state,
        phase_id: None,
        main_session_id: Some("session-10".into()),
        branch: Some(format!("work/{work_id}/task/{id}")),
        micro_measurables: serde_json::json!([{ "text": "tests pass", "met": false }]),
        verification_result: None,
        failure_reason: None,
        last_activity: None,
        created_at: Some("2026-07-29T00:00:00Z".into()),
        updated_at: Some("2026-07-29T00:00:01Z".into()),
    }
}

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
            plan: Some("pro".into()),
            credits: Some("42.50".into()),
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

    async fn get_project(&self, project_id: String) -> zyris::Result<ZProject> {
        Ok(ZProject {
            id: project_id,
            name: "Rollout".into(),
            description: Some("Q3".into()),
            is_default: false,
        })
    }

    async fn create_project(&self, project: ZNewProject) -> zyris::Result<ZProject> {
        Ok(ZProject {
            id: "project-3".into(),
            name: project.name,
            description: project.description,
            is_default: false,
        })
    }

    /// Echoes the three-valued description back as it arrived, which is the only way the test can
    /// tell "clear it" apart from "leave it alone".
    async fn update_project(
        &self,
        project_id: String,
        update: ZProjectUpdate,
    ) -> zyris::Result<ZProject> {
        Ok(ZProject {
            id: project_id,
            name: update.name.unwrap_or_else(|| "Rollout".into()),
            description: update.description.unwrap_or(Some("Q3".into())),
            is_default: false,
        })
    }

    async fn delete_project(&self, _project_id: String) -> zyris::Result<()> {
        Ok(())
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

    /// Reports some of what it meters and not the rest, which is the shape a node has to cope
    /// with: `credits_used` is left absent on purpose.
    async fn session_usage(&self, _session_id: String) -> zyris::Result<ZUsage> {
        Ok(ZUsage {
            model: Some("claude-opus-5".into()),
            context_tokens: Some(12_400),
            input_tokens: Some(9_100),
            output_tokens: Some(3_300),
            total_tokens: Some(12_400),
            credits_used: None,
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

    async fn list_jobs(&self, filter: ZJobFilter) -> zyris::Result<Vec<ZJob>> {
        let mut out = vec![stub_job("job-1", ZJobState::Processing), stub_job("job-2", ZJobState::Done)];
        if let Some(project_id) = filter.project_id {
            out.retain(|job| job.project_id.as_deref() == Some(project_id.as_str()));
        }
        if let Some(limit) = filter.limit {
            out.truncate(limit as usize);
        }
        Ok(out)
    }

    async fn get_job(&self, job_id: String) -> zyris::Result<ZJob> {
        Ok(stub_job(&job_id, ZJobState::Processing))
    }

    async fn create_job(&self, job: ZNewJob) -> zyris::Result<ZJob> {
        let mut created = stub_job("job-3", ZJobState::Processing);
        created.description = Some(job.message);
        created.project_id = job.project_id;
        created.planning = job.planning;
        Ok(created)
    }

    async fn update_job(&self, job_id: String, update: ZJobUpdate) -> zyris::Result<ZJob> {
        let mut job = stub_job(&job_id, ZJobState::Processing);
        if let Some(title) = update.title {
            job.title = title;
        }
        job.description = update.description.or(job.description);
        job.project_id = update.project_id.or(job.project_id);
        Ok(job)
    }

    async fn delete_job(&self, _job_id: String) -> zyris::Result<()> {
        Ok(())
    }

    async fn list_works(&self, filter: ZWorkFilter) -> zyris::Result<Vec<ZWork>> {
        let mut out = vec![
            stub_work("work-1", ZWorkState::AwaitingGoalApproval),
            stub_work("work-2", ZWorkState::Executing),
        ];
        if let Some(project_id) = filter.project_id {
            out.retain(|work| work.project_id.as_deref() == Some(project_id.as_str()));
        }
        if let Some(limit) = filter.limit {
            out.truncate(limit as usize);
        }
        Ok(out)
    }

    async fn get_work(&self, work_id: String) -> zyris::Result<ZWork> {
        Ok(stub_work(&work_id, ZWorkState::Executing))
    }

    /// Comes back at gate 1, which is where a real deployment leaves it — a node that treats
    /// creation as "it is running now" is the mistake this stub is shaped to expose.
    async fn create_work(&self, work: ZNewWork) -> zyris::Result<ZWork> {
        let mut created = stub_work("work-3", ZWorkState::AwaitingGoalApproval);
        created.description = Some(work.message);
        created.project_id = work.project_id;
        Ok(created)
    }

    async fn update_work(&self, work_id: String, update: ZWorkUpdate) -> zyris::Result<ZWork> {
        let mut work = stub_work(&work_id, ZWorkState::Executing);
        if let Some(title) = update.title {
            work.title = title;
        }
        work.description = update.description.or(work.description);
        work.project_id = update.project_id.or(work.project_id);
        Ok(work)
    }

    async fn delete_work(&self, _work_id: String) -> zyris::Result<()> {
        Ok(())
    }

    async fn approve_work_goal(&self, work_id: String) -> zyris::Result<ZWork> {
        Ok(stub_work(&work_id, ZWorkState::Planning))
    }

    async fn approve_work_plan(&self, work_id: String) -> zyris::Result<ZWork> {
        Ok(stub_work(&work_id, ZWorkState::Executing))
    }

    async fn work_tasks(&self, work_id: String) -> zyris::Result<ZWorkTasks> {
        Ok(ZWorkTasks {
            tasks: vec![
                stub_task("task-1", &work_id, ZTaskState::Done),
                stub_task("task-2", &work_id, ZTaskState::Creating),
            ],
            deps: vec![ZTaskDep {
                upstream_task_id: "task-1".into(),
                downstream_task_id: "task-2".into(),
            }],
        })
    }

    async fn stop_work(&self, _work_id: String) -> zyris::Result<()> {
        Ok(())
    }

    async fn continue_work(&self, work_id: String) -> zyris::Result<ZWork> {
        Ok(stub_work(&work_id, ZWorkState::Executing))
    }

    async fn work_message(
        &self,
        _work_id: String,
        _message: String,
        _data: Vec<Datum>,
    ) -> zyris::Result<()> {
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

    async fn register_node(&self, request: ZNewNode) -> zyris::Result<ZNode> {
        Ok(ZNode {
            node_id: "sibling-1".into(),
            name: request.name,
            slug: "sibling-1".into(),
            platform: request.platform.unwrap_or_else(|| "linux".into()),
            scopes: request.scopes,
            connected: false,
            token: Some("znt_sibling-one-time".into()),
            last_seen_at: None,
            created_at: Some("2026-08-03T00:00:00Z".into()),
        })
    }

    async fn list_nodes(&self) -> zyris::Result<Vec<ZNode>> {
        Ok(vec![ZNode {
            node_id: "sibling-1".into(),
            name: "coder-a".into(),
            slug: "sibling-1".into(),
            platform: "linux".into(),
            scopes: vec![],
            connected: true,
            token: None,
            last_seen_at: Some("2026-08-03T00:00:01Z".into()),
            created_at: Some("2026-08-03T00:00:00Z".into()),
        }])
    }

    async fn peer_publish(&self, _endpoint_id: String, _addrs: Vec<String>) -> zyris::Result<()> {
        Ok(())
    }

    /// Echoes the requested slug back on the answer, which is what a node calling this with the
    /// name a user typed should get: the same name it asked with, not one the server substituted.
    async fn peer_lookup(&self, slug: String) -> zyris::Result<ZPeerAddr> {
        Ok(ZPeerAddr {
            node_id: "sibling-1".into(),
            slug,
            endpoint_id: "ed25519:sibling-endpoint".into(),
            addrs: vec!["203.0.113.5:4433".into()],
            relay_url: Some("https://relay.attacca.cc".into()),
            online: true,
        })
    }

    async fn peer_list(&self) -> zyris::Result<Vec<ZPeerEntry>> {
        Ok(vec![ZPeerEntry {
            node_id: "sibling-1".into(),
            slug: "laptop".into(),
            endpoint_id: "ed25519:sibling-endpoint".into(),
            online: true,
        }])
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
    assert_eq!(descriptor.tools.len(), 37);
    assert_eq!(descriptor.tool("list_agents").unwrap().transfer, Transfer::Unary);
    assert_eq!(descriptor.tool("me").unwrap().transfer, Transfer::Unary);
    assert_eq!(descriptor.tool("list_projects").unwrap().transfer, Transfer::Unary);
    assert_eq!(descriptor.tool("turn_events").unwrap().transfer, Transfer::UniStream);

    // Tools added within v1: a node discovers them here, and one built before them keeps calling
    // `create_session` unaffected.
    assert_eq!(descriptor.tool("create_session").unwrap().transfer, Transfer::Unary);
    assert_eq!(descriptor.tool("create_session_with").unwrap().transfer, Transfer::Unary);
    assert_eq!(descriptor.tool("session_history").unwrap().transfer, Transfer::Unary);
    assert_eq!(descriptor.tool("session_usage").unwrap().transfer, Transfer::Unary);
    assert_eq!(descriptor.tool("create_project").unwrap().transfer, Transfer::Unary);
    assert_eq!(descriptor.tool("create_job").unwrap().transfer, Transfer::Unary);
    assert_eq!(descriptor.tool("create_work").unwrap().transfer, Transfer::Unary);
    assert_eq!(descriptor.tool("approve_work_plan").unwrap().transfer, Transfer::Unary);
    assert_eq!(descriptor.tool("work_tasks").unwrap().transfer, Transfer::Unary);

    // `turn_events` stays the only stream: everything added since answers once.
    let streams: Vec<&str> = descriptor
        .tools
        .iter()
        .filter(|tool| tool.transfer == Transfer::UniStream)
        .map(|tool| tool.name.as_str())
        .collect();
    assert_eq!(streams, ["turn_events"]);
}

/// The three rendezvous tools added for P2P peer discovery: all unary (a lookup is a single
/// request/response, not a feed), and `turn_events` must still be the only streamed tool.
#[test]
fn rendezvous_tools_are_in_the_descriptor() {
    let descriptor = attacca_api_capability();
    for name in ["peer_publish", "peer_lookup", "peer_list"] {
        let tool = descriptor.tool(name).unwrap_or_else(|| panic!("{name} is missing"));
        assert_eq!(tool.transfer, Transfer::Unary, "{name}");
    }

    let streams: Vec<&str> = descriptor
        .tools
        .iter()
        .filter(|tool| tool.transfer == Transfer::UniStream)
        .map(|tool| tool.name.as_str())
        .collect();
    assert_eq!(streams, ["turn_events"]);
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

/// Every field is optional, so what matters is that a partially-reporting deployment round-trips
/// with the absences intact rather than defaulted into zeroes a node would then display.
#[tokio::test]
async fn session_usage_reports_what_the_deployment_meters() {
    let api = client().await;

    let usage = api.session_usage("session-1".into()).await.unwrap();
    assert_eq!(usage.model.as_deref(), Some("claude-opus-5"));
    assert_eq!(usage.context_tokens, Some(12_400));
    assert_eq!(usage.input_tokens, Some(9_100));
    assert_eq!(usage.output_tokens, Some(3_300));
    assert_eq!(usage.credits_used, None);

    // A deployment that meters nothing is a legitimate answer, not an error.
    assert_eq!(serde_json::from_str::<ZUsage>("{}").unwrap(), ZUsage::default());
}

/// The account-level half of the same surface, and for the same reason: a deployment that does not
/// bill leaves both absent rather than sending an empty string a node would render as a blank row.
#[tokio::test]
async fn me_carries_the_accounts_plan_and_balance() {
    let api = client().await;

    let me = api.me().await.unwrap();
    assert_eq!(me.plan.as_deref(), Some("pro"));
    assert_eq!(me.credits.as_deref(), Some("42.50"));

    let bare: ZMe = serde_json::from_str(
        r#"{"user_id":"u","email":"e","display_name":"d"}"#,
    )
    .unwrap();
    assert_eq!(bare.plan, None);
    assert_eq!(bare.credits, None);
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

#[tokio::test]
async fn project_crud_round_trips() {
    let api = client().await;

    let created = api
        .create_project(ZNewProject { name: "Rollout".into(), description: Some("Q3".into()) })
        .await
        .unwrap();
    assert_eq!((created.name.as_str(), created.description.as_deref()), ("Rollout", Some("Q3")));
    assert!(!created.is_default);

    assert_eq!(api.get_project("project-2".into()).await.unwrap().id, "project-2");

    let renamed = api
        .update_project(
            "project-2".into(),
            ZProjectUpdate { name: Some("Rollout 2".into()), ..ZProjectUpdate::default() },
        )
        .await
        .unwrap();
    assert_eq!(renamed.name, "Rollout 2");

    api.delete_project("project-2".into()).await.unwrap();
}

/// The one thing a single `Option` could not express. An omitted `description` has to leave the
/// project's alone while an explicit `null` clears it, and the two are indistinguishable by the time
/// they reach the deployment unless the double wrapping survives the wire — which is what this
/// checks, since the stub echoes whatever it was handed.
#[tokio::test]
async fn clearing_a_description_is_distinct_from_leaving_it_alone() {
    let api = client().await;

    let untouched =
        api.update_project("project-2".into(), ZProjectUpdate::default()).await.unwrap();
    assert_eq!(untouched.description.as_deref(), Some("Q3"));

    let cleared = api
        .update_project(
            "project-2".into(),
            ZProjectUpdate { description: Some(None), ..ZProjectUpdate::default() },
        )
        .await
        .unwrap();
    assert_eq!(cleared.description, None);

    // And the same distinction on the way in, since that is where serde has to be told about it.
    let omitted: ZProjectUpdate = serde_json::from_str("{}").unwrap();
    assert_eq!(omitted.description, None);
    let explicit_null: ZProjectUpdate = serde_json::from_str(r#"{"description":null}"#).unwrap();
    assert_eq!(explicit_null.description, Some(None));
}

#[tokio::test]
async fn job_crud_round_trips() {
    let api = client().await;

    let created = api
        .create_job(ZNewJob {
            message: "Summarize the changelog".into(),
            agent_id: None,
            project_id: Some("project-1".into()),
            timezone: Some("Europe/Berlin".into()),
            planning: false,
            plan_mode: false,
            data: Vec::new(),
        })
        .await
        .unwrap();
    assert_eq!(created.description.as_deref(), Some("Summarize the changelog"));
    assert_eq!(created.state, ZJobState::Processing);
    // The session the node then streams with the tools it already had.
    assert_eq!(created.session_id.as_deref(), Some("session-1"));

    assert_eq!(api.get_job("job-1".into()).await.unwrap().id, "job-1");

    let all = api.list_jobs(ZJobFilter::default()).await.unwrap();
    assert_eq!(all.len(), 2);
    let capped = api.list_jobs(ZJobFilter { limit: Some(1), ..ZJobFilter::default() }).await.unwrap();
    assert_eq!(capped.len(), 1);

    let renamed = api
        .update_job("job-1".into(), ZJobUpdate { title: Some("Renamed".into()), ..ZJobUpdate::default() })
        .await
        .unwrap();
    assert_eq!(renamed.title, "Renamed");

    api.delete_job("job-1".into()).await.unwrap();
}

/// A created work is not a running work: it stops at gate 1 and stays there. The two approvals are
/// the whole reason works have tools of their own rather than being jobs with extra fields.
#[tokio::test]
async fn a_work_is_created_then_let_through_both_gates() {
    let api = client().await;

    let created = api
        .create_work(ZNewWork {
            message: "Port the parser\nand keep the fixtures green".into(),
            agent_id: None,
            project_id: Some("project-1".into()),
        })
        .await
        .unwrap();
    assert_eq!(created.state, ZWorkState::AwaitingGoalApproval);
    // What gate 1 is actually deciding on, and the session to argue with it over.
    assert_eq!(created.final_goal.as_deref(), Some("every fixture round-trips"));
    assert_eq!(created.planner_session_id.as_deref(), Some("session-9"));

    api.work_message(created.id.clone(), "narrow it to the lexer".into(), Vec::new()).await.unwrap();

    let planning = api.approve_work_goal(created.id.clone()).await.unwrap();
    assert_eq!(planning.state, ZWorkState::Planning);

    let executing = api.approve_work_plan(created.id.clone()).await.unwrap();
    assert_eq!(executing.state, ZWorkState::Executing);
}

#[tokio::test]
async fn work_tasks_come_back_as_a_graph() {
    let api = client().await;

    let graph = api.work_tasks("work-1".into()).await.unwrap();
    assert_eq!(graph.tasks.len(), 2);
    assert_eq!(graph.tasks[0].state, ZTaskState::Done);
    assert_eq!(graph.tasks[1].state, ZTaskState::Creating);
    // The planner's own shape, passed through rather than typed by the protocol.
    assert_eq!(graph.tasks[0].micro_measurables[0]["text"], "tests pass");
    assert_eq!(graph.deps.len(), 1);
    assert_eq!(graph.deps[0].upstream_task_id, "task-1");
    assert_eq!(graph.deps[0].downstream_task_id, "task-2");

    // An unplanned work has a graph, it is just empty — not an error and not an absent field.
    assert_eq!(serde_json::from_str::<ZWorkTasks>("{}").unwrap(), ZWorkTasks::default());
}

#[tokio::test]
async fn work_read_and_lifecycle_tools_round_trip() {
    let api = client().await;

    assert_eq!(api.list_works(ZWorkFilter::default()).await.unwrap().len(), 2);
    let capped =
        api.list_works(ZWorkFilter { limit: Some(1), ..ZWorkFilter::default() }).await.unwrap();
    assert_eq!(capped.len(), 1);

    assert_eq!(api.get_work("work-1".into()).await.unwrap().id, "work-1");

    let renamed = api
        .update_work("work-1".into(), ZWorkUpdate { title: Some("Renamed".into()), ..ZWorkUpdate::default() })
        .await
        .unwrap();
    assert_eq!(renamed.title, "Renamed");

    api.stop_work("work-1".into()).await.unwrap();
    assert_eq!(api.continue_work("work-1".into()).await.unwrap().state, ZWorkState::Executing);
    api.delete_work("work-1".into()).await.unwrap();
}

/// The two scopes added alongside the work tools. `ZScope::ALL` is what a node asks for when it
/// wants everything, so a scope missing from it is a scope no node ever requests.
#[tokio::test]
async fn works_scopes_are_spelled_the_way_the_wire_spells_them() {
    assert_eq!(ZScope::WorksRead.as_str(), "works:read");
    assert_eq!(ZScope::WorksWrite.as_str(), "works:write");
    assert_eq!(ZScope::from_str("works:write"), Some(ZScope::WorksWrite));
    assert!(ZScope::ALL.contains(&ZScope::WorksRead));
    assert!(ZScope::ALL.contains(&ZScope::WorksWrite));
}

/// A sibling node: registered under the caller's device, listed without its one-time token. The
/// token is the only thing that must never survive a round trip into a listing.
#[tokio::test]
async fn sibling_nodes_round_trip() {
    let api = client().await;

    let created = api
        .register_node(ZNewNode {
            name: "coder-a".into(),
            platform: Some("linux".into()),
            scopes: vec!["sessions:write".into()],
        })
        .await
        .unwrap();
    assert_eq!(created.node_id, "sibling-1");
    assert_eq!(created.platform, "linux");
    assert_eq!(created.scopes, vec!["sessions:write".to_string()]);
    assert!(created.token.is_some(), "the register response is the one chance to see the token");

    let listed = api.list_nodes().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].node_id, "sibling-1");
    assert!(listed[0].token.is_none(), "the one-time token must never come back in a listing");
}

/// The scope added alongside the sibling-node tools. `ZScope::ALL` is what a node asks for when it
/// wants everything, so a scope missing from it is a scope no node ever requests.
#[tokio::test]
async fn sibling_node_scope_is_spelled_the_way_the_wire_spells_it() {
    assert_eq!(ZScope::NodesWrite.as_str(), "nodes:write");
    assert_eq!(ZScope::from_str("nodes:write"), Some(ZScope::NodesWrite));
    assert!(ZScope::ALL.contains(&ZScope::NodesWrite));
}

/// The scope guarding the three rendezvous tools.
#[test]
fn peers_write_scope_exists() {
    let scope: ZScope = serde_json::from_str("\"peers:write\"").unwrap();
    assert_eq!(scope, ZScope::PeersWrite);
    assert_eq!(scope.as_str(), "peers:write");
    assert_eq!(ZScope::from_str("peers:write"), Some(ZScope::PeersWrite));
    assert!(ZScope::ALL.contains(&ZScope::PeersWrite));
}

/// The lookup key is the slug, not `node_id`: this is what a node calling `peer_lookup("laptop")`
/// gets back, and it must be the same name it asked with rather than one the server minted, since
/// that name is what TOFU pinning in `zyris-p2p` keys on.
#[tokio::test]
async fn peer_rendezvous_tools_round_trip() {
    let api = client().await;

    api.peer_publish("ed25519:my-endpoint".into(), vec!["198.51.100.9:4433".into()]).await.unwrap();

    let addr = api.peer_lookup("laptop".into()).await.unwrap();
    assert_eq!(addr.slug, "laptop", "peer_lookup must answer under the slug it was asked for");
    assert_eq!(addr.node_id, "sibling-1");
    assert!(!addr.addrs.is_empty());
    assert!(addr.online);

    let peers = api.peer_list().await.unwrap();
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0].slug, "laptop");
    assert_eq!(peers[0].endpoint_id, "ed25519:sibling-endpoint");
}
