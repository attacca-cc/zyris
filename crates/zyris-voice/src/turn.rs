//! The live turn feed: what Attacca is saying, while it is still being written.
//!
//! One session, subscribed once per connection, cut into fragments on the way past. What leaves
//! here is [`TurnEvent`]; what enters is [`ZTurnFrame`], and the two are deliberately not the
//! same shape.
//!
//! # The ordering, and why it is not a race this module has to win
//!
//! `turn_events(session, after)` with `after: None` is **live frames only** — no replay.
//! `session_history`'s own documentation says so by contrast: omitting `after` there means the
//! whole history, where here it means nothing that already happened. So a node that sends a
//! message and *then* subscribes loses every delta that arrived in between, and on a machine
//! where the answer starts inside a hundred milliseconds that is most of the first sentence.
//!
//! The fix is not to subscribe faster. **The subscription belongs to the connection, not to the
//! message**: [`Feed::attach`] runs from the connect hook, before anything can be said, and
//! [`Feed::say`] refuses outright while no subscription is live. There is then no window to
//! lose anything in, because sending is only reachable through a feed that is already
//! listening. `a_message_is_never_sent_before_the_feed_is_listening` is the test, and it asserts
//! on the *order of the calls that reached Attacca* rather than on a timing.
//!
//! # A subscription belongs to one connection
//!
//! The stream dies with the connection — `zyris-core` fails every open stream with
//! `connection_lost` — and `Connector`'s hook runs again on the connection that replaces it. So
//! every attach takes a **generation**, and a pump whose generation is no longer the current one
//! publishes nothing and advances nothing. Without it the dead connection's reader is still a
//! reader: it would go on emitting fragments from a stream nobody can answer, and its cursor
//! would overwrite the live one.
//!
//! # Two ways a stream fails, and they are not the same failure
//!
//! | code | what happened | what this does |
//! |---|---|---|
//! | `StreamLagged` | a chunk-sequence gap; the protocol requires the stream to fail rather than deliver past it, and `zyris-core` sends `SCancel` back | re-subscribe **on this connection**, from `last_cursor` |
//! | `ConnectionLost` | the socket went | nothing here; the connect hook re-attaches on the next connection |
//!
//! They are distinguishable — `WireError::code` — and treating them alike costs something in
//! both directions: re-dialling a live connection for a gap it could have resumed, or sitting
//! silent on a live connection because a gap looked like a disconnect.
//!
//! # What a cursor is, and when it may be taken from the head
//!
//! `ZTurnStatus::last_cursor` is where the server's durable timeline had got to when the
//! subscription opened. It is adopted **only when this feed has no cursor of its own**: with
//! `after: None` everything up to it was deliberately not delivered, so it is exactly the
//! watermark "already skipped". Adopting it on a later subscription would skip the replay that
//! was asked for — the events between what this node has seen and what the server has.

use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use tokio::sync::broadcast;
use zyris::{ErrorCode, WireError};
use zyris_attacca::{
    AttaccaApi, AttaccaApiClient, ZDeltaKind, ZSessionEvent, ZTurnFrame, ZTurnStatus,
};

use crate::speak::{Filter, Kind};
use crate::split::{Fragment, Splitter, IDLE_FLUSH};
use crate::view::{AgentEntry, ProjectEntry, SessionEntry, SessionsView};

/// How long a connection gets to announce `attacca_api` before this gives up on it.
///
/// The same thirty seconds `zyris_tools::transfer` waits, and for the same reason: the wait ends
/// early when the connection closes, and it runs beside the close report rather than in front of
/// it.
pub const API_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// How many events the feed will hold for a consumer that is not reading.
///
/// A `broadcast` drops the oldest when it overflows, which for a fragment means a sentence that
/// is never spoken. The consumer is a synthesis worker that takes a fragment at a time and is
/// slower than the stream, so the queue is generous.
const EVENT_CAPACITY: usize = 512;

/// What the feed needs from Attacca, and nothing else.
///
/// **A trait rather than the client, because there is no other way to test any of this.** No
/// example or test anywhere in the protocol stack consumes `turn_events`; there is no reference
/// consumer to copy and no fixture to run against, so the shape of a subscription — what is
/// asked for, in what order, and what a failure does — is decided here against a double. The one
/// implementation that is not a double is for [`AttaccaApiClient`], below, and it is three
/// delegating lines.
#[zyris::async_trait]
pub trait TurnApi: Send + Sync + 'static {
    /// The live feed. `after: None` is live-only; see this module's documentation.
    async fn turn_events(
        &self,
        session_id: String,
        after: Option<i64>,
    ) -> zyris::Result<zyris::Streaming<ZTurnStatus, ZTurnFrame>>;

    /// Post a message, starting a turn.
    async fn send_message(&self, session_id: String, message: String) -> zyris::Result<()>;

    /// Stop the turn that is running, if one is.
    ///
    /// **It takes a session and nothing else.** There is no way to tell the server *where* the
    /// answer stopped being useful, and no `Cancelled` frame comes back — from the stream alone
    /// a cancel and an ordinary finish are the same thing. That is why barge-in puts a note saying
    /// where speech was cut off in front of the person's next message: see
    /// [`crate::session::Interruption`]. An upstream issue asks for a delivery point.
    async fn cancel_turn(&self, session_id: String) -> zyris::Result<()>;

    /// The agents on this account, so a session can be created against one.
    ///
    /// **Its ordering is not documented upstream.** `list_projects` promises "the default
    /// first, then the rest oldest-first" and this one promises nothing, so a caller that took
    /// the first would be relying on an order nobody offered. See [`Feed::choose_agent`].
    async fn list_agents(&self) -> zyris::Result<Vec<(String, String)>>;

    /// Create a session against an agent, and answer its id.
    ///
    /// No title: Attacca names a session from its first message, and a title given at creation
    /// is permanent and suppresses that. `project: None` files it under the account's default
    /// project, which is made on demand.
    async fn create_session(
        &self,
        agent_id: String,
        project: Option<String>,
    ) -> zyris::Result<String>;

    /// The account's projects: the default first, then the rest oldest-first.
    async fn list_projects(&self) -> zyris::Result<Vec<ProjectEntry>>;

    /// The account's sessions, in every project.
    async fn list_sessions(&self) -> zyris::Result<Vec<SessionEntry>>;
}

#[zyris::async_trait]
impl TurnApi for AttaccaApiClient {
    async fn turn_events(
        &self,
        session_id: String,
        after: Option<i64>,
    ) -> zyris::Result<zyris::Streaming<ZTurnStatus, ZTurnFrame>> {
        AttaccaApi::turn_events(self, session_id, after).await
    }

    async fn send_message(&self, session_id: String, message: String) -> zyris::Result<()> {
        // No attachments: this node speaks, it does not upload. `Datum` is the protocol's shape
        // for a file riding along with a message and nothing in the voice path produces one.
        AttaccaApi::send_message(self, session_id, message, Vec::new()).await
    }

    async fn cancel_turn(&self, session_id: String) -> zyris::Result<()> {
        AttaccaApi::cancel_turn(self, session_id).await
    }

    async fn list_agents(&self) -> zyris::Result<Vec<(String, String)>> {
        // Reduced to (id, name) at the seam rather than carried whole: those are the two
        // things this crate has any use for — one to create with and one to name in a
        // sentence a person reads — and a double that had to build a `ZAgent` would be
        // agreeing with a shape nothing here depends on.
        Ok(AttaccaApi::list_agents(self)
            .await?
            .into_iter()
            .map(|agent| (agent.id, agent.name))
            .collect())
    }

    async fn create_session(
        &self,
        agent_id: String,
        project: Option<String>,
    ) -> zyris::Result<String> {
        let session = AttaccaApi::create_session_with(
            self,
            zyris_attacca::ZNewSession {
                agent_id,
                title: None,
                project_id: project,
                preamble: None,
            },
        )
        .await?;
        Ok(session.id)
    }

    async fn list_projects(&self) -> zyris::Result<Vec<ProjectEntry>> {
        Ok(AttaccaApi::list_projects(self)
            .await?
            .into_iter()
            .map(|project| ProjectEntry {
                id: project.id,
                name: project.name,
                is_default: project.is_default,
            })
            .collect())
    }

    async fn list_sessions(&self) -> zyris::Result<Vec<SessionEntry>> {
        Ok(AttaccaApi::list_sessions(self, zyris_attacca::ZSessionFilter::default())
            .await?
            .into_iter()
            .map(|session| SessionEntry {
                id: session.id,
                title: session.title,
                project: session.project_id,
                agent: session.agent_id,
                running: session.running,
            })
            .collect())
    }
}

/// What comes out of a turn.
///
/// Not `VoiceEvent`: these are the pieces a session assembles into one, and a window never sees
/// them. The crate still exposes exactly one thing outward — this is between two modules of it.
#[derive(Debug, Clone, PartialEq)]
pub enum TurnEvent {
    /// A delta as the screen should have it. **Every delta, reasoning included** — rule 1 is
    /// about the speaker, not about the transcript.
    Shown { kind: Kind, text: String },
    /// Something to say, filtered and cut. Ready for synthesis as it stands.
    Say(Fragment),
    /// A durable event, carried exactly as it arrived.
    ///
    /// `ZSessionEvent::kind` is an untyped string and `payload` an untyped value, and the
    /// vocabulary (`assistant_message`, `chat_user`, `chat_agent`) appears only in the
    /// protocol's own test fixtures. Inventing a schema for it here would be inventing one for
    /// a server that has not promised it; it is carried whole instead, `seq` and `created_at`
    /// included.
    Event { cursor: i64, event: ZSessionEvent },
    /// Whether a turn is running, as the server last said.
    Running(bool),
    /// The subscription ended, and whether this feed is opening another one itself.
    ///
    /// `resubscribing: true` is a chunk gap, which resumes on the same connection;
    /// `resubscribing: false` means the next connection is what restores the feed, exactly as
    /// `CoreEvent::Disconnected { retrying }` distinguishes the two waits a person is in.
    Lost { reason: String, resubscribing: bool },
}

/// One session's live feed: the subscription, the cursor it resumes from, and the filter and
/// splitter that turn deltas into something sayable.
///
/// **One session, created once and reused** — there is no screen for choosing one, so the id is
/// given at construction and never changes.
pub struct Feed {
    /// The session this feed is for. **`None` until one exists**, which is the ordinary state of
    /// a fresh install: the spec's loop begins *get a session*, and nothing had ever done that.
    session_id: Mutex<Option<String>>,
    /// Which agent to create against, when the account has more than one. Read from settings.
    agent: Option<String>,
    events: broadcast::Sender<TurnEvent>,
    state: Mutex<State>,
    /// The agents this account has, recorded when they are the reason no session was made.
    ///
    /// **Only then.** A screen showing a list of agents on a machine that simply has not
    /// connected yet would be answering a question nobody asked; this is set when the answer
    /// is *your account has none* or *your account has several and I will not choose*.
    agent_trouble: Mutex<Option<Vec<String>>>,
}

/// Everything about the feed that a reconnect replaces, under one lock so that a generation and
/// the client it belongs to cannot be read apart.
struct State {
    /// The client of the connection that is current, or `None` before the first one.
    api: Option<Arc<dyn TurnApi>>,
    /// How many connections this feed has been attached to. A pump carries the number it was
    /// started with and stops as soon as it is not the current one.
    generation: u64,
    /// Whether a subscription is open **now**. What [`Feed::say`] refuses on.
    live: bool,
    /// The last durable cursor this feed has actually processed. What a resumption asks for.
    cursor: Option<i64>,
}

/// How opening a subscription went.
enum Opened {
    Items(zyris::ItemStream<ZTurnFrame>),
    /// Give up on this generation; already logged and published.
    Failed,
    /// The session does not exist for this account any more. See [`session_is_gone`].
    Gone,
}

/// Whether `turn_events` refused because the session is not there — deleted in the web app, or
/// never this account's.
///
/// ponytail: matches Attacca's message text. Its gateway sends a missing or foreign session from
/// `turn_events` as `Internal` with the error's text, so there is no code to read; switch to
/// `ErrorCode::Other("not_found")` alone once it maps that lookup through `wire_repo`, as it
/// already does elsewhere.
fn session_is_gone(error: &WireError) -> bool {
    matches!(&error.code, ErrorCode::Other(code) if code == "not_found")
        || error.message.contains("session not found")
        || error.message.contains("owned by a different user")
}

impl Feed {
    /// A feed for one session, with nothing attached to it yet.
    pub fn new(session_id: impl Into<String>) -> Arc<Feed> {
        Feed::build(Some(session_id.into()), None)
    }

    /// A feed with no session yet: it makes one on the first connection that offers an agent.
    ///
    /// `agent` names which, for an account with more than one.
    ///
    /// **Whoever wants to write the id down asks for it**, rather than being told through a
    /// channel. A channel meant a task waiting on it, and the only place to start one was
    /// `Engine::new` — a synchronous constructor, called before there is a runtime. The id is
    /// state, [`Feed::session_id`] answers it, and `on_connect` is already async and already
    /// the moment it can have changed.
    pub fn making_one(agent: Option<String>) -> Arc<Feed> {
        Feed::build(None, agent)
    }

    /// A feed for a session written down earlier, which makes a new one against `agent` if that
    /// session turns out to be gone. See [`session_is_gone`].
    pub fn continuing(session_id: impl Into<String>, agent: Option<String>) -> Arc<Feed> {
        Feed::build(Some(session_id.into()), agent)
    }

    fn build(session_id: Option<String>, agent: Option<String>) -> Arc<Feed> {
        Arc::new(Feed {
            session_id: Mutex::new(session_id),
            agent,
            events: broadcast::channel(EVENT_CAPACITY).0,
            state: Mutex::new(State { api: None, generation: 0, live: false, cursor: None }),
            agent_trouble: Mutex::new(None),
        })
    }

    /// The session this feed is for, once there is one.
    pub fn session_id(&self) -> Option<String> {
        self.session_id.lock().expect("the session id is not poisoned").clone()
    }

    /// The agents on the account, when they are why there is no session. `None` means the
    /// question has not arisen — nothing has connected yet, or a session already exists.
    pub fn agent_trouble(&self) -> Option<Vec<String>> {
        self.agent_trouble.lock().expect("not poisoned").clone()
    }

    /// A new subscription to what the turn is producing. Each caller gets its own.
    pub fn events(&self) -> broadcast::Receiver<TurnEvent> {
        self.events.subscribe()
    }

    /// Take the subscription down without ending the connection, the way a chunk gap does.
    ///
    /// Test-only. The gap between a stream failing and the next one opening is a real window
    /// with real behaviour in it — a cancel is accepted there and a message is not — and there
    /// is no way to reach it from outside without a server that produces a `StreamLagged`.
    #[cfg(test)]
    pub(crate) fn take_the_subscription_down(&self) {
        self.state.lock().expect("the feed state is not poisoned").live = false;
    }

    /// Whether a turn subscription is open right now.
    ///
    /// Not "whether this node is connected": a connection that never announced `attacca_api`,
    /// or a `turn_events` the server refused, is a live connection with no feed on it.
    pub fn is_live(&self) -> bool {
        self.state.lock().expect("the feed state is not poisoned").live
    }

    /// The `zyris_runtime::ConnectHook` half: what to do with a connection that has just come up.
    ///
    /// **Every connection, including every redial.** What is kept from the last one is the
    /// cursor and nothing else — the client, the subscription and the filter state all belong to
    /// a connection that is gone.
    ///
    /// Nothing here returns an error. A connection that never announces `attacca_api` leaves
    /// this machine connected and serving every other capability, and the next connection tries
    /// again.
    pub async fn on_connect(self: &Arc<Self>, connection: zyris::Connection) {
        match connection.wait_capability::<AttaccaApiClient>(API_WAIT).await {
            Ok(api) => self.attach(Arc::new(api)).await,
            Err(error) => {
                tracing::warn!(
                    %error,
                    "this connection never announced attacca_api, so there is no turn to listen \
                     to; the voice waits for the next one"
                );
            }
        }
    }

    /// Take a client for the connection that is now current, and open a subscription on it.
    ///
    /// **Awaits the subscription rather than spawning it**, which is the whole ordering rule:
    /// the hook that calls this does not finish until the feed is either listening or has failed
    /// to, so nothing can send a message into a session nobody is watching.
    ///
    /// Separate from [`Feed::on_connect`] so that every test below can drive a feed without a
    /// `zyris::Connection`: what is worth deciding here is what a subscription asks for and what
    /// a failure does, and none of that needs a socket.
    pub async fn attach(self: &Arc<Self>, api: Arc<dyn TurnApi>) {
        let generation = {
            let mut state = self.state.lock().expect("the feed state is not poisoned");
            state.generation += 1;
            state.api = Some(api.clone());
            // Not live until the new subscription is open. A `say` in this window is refused
            // rather than sent into a session this node has stopped watching.
            state.live = false;
            state.generation
        };

        // **In front of the subscription, and only once.** The ordering rule this module is
        // built on is that sending is only reachable through a feed that is already listening;
        // a session that does not exist yet has to be made before there is anything to listen
        // to, so it goes here rather than beside the first message.
        if self.session_id().is_none() && !self.make_a_session(api.as_ref()).await {
            return;
        }
        let items = match self.open(api.as_ref(), generation).await {
            Opened::Items(items) => items,
            Opened::Failed => return,
            // **Once per connection.** The session this machine wrote down was deleted, or was
            // never this account's; retrying it on every reconnect would be a voice that never
            // comes back, with nothing on any screen to clear it. A new one is written down by
            // `Engine::remember_the_session` like the first.
            Opened::Gone => {
                tracing::warn!("the voice's session is gone from Attacca; making a new one");
                *self.session_id.lock().expect("the session id is not poisoned") = None;
                self.state.lock().expect("the feed state is not poisoned").cursor = None;
                if !self.make_a_session(api.as_ref()).await {
                    return;
                }
                match self.open(api.as_ref(), generation).await {
                    Opened::Items(items) => items,
                    Opened::Failed | Opened::Gone => return,
                }
            }
        };
        let feed = self.clone();
        tokio::spawn(async move { feed.run(api, generation, items).await });
    }

    /// The client of the connection that is current, or a sentence saying there is none.
    fn api(&self) -> zyris::Result<Arc<dyn TurnApi>> {
        self.state.lock().expect("the feed state is not poisoned").api.clone().ok_or_else(|| {
            WireError::new(
                ErrorCode::ConnectionLost,
                "this machine is not connected to Attacca yet, so there are no sessions to \
                 choose from",
            )
        })
    }

    /// Everything the Conversation screen needs to choose a session, read off the account.
    ///
    /// **Each of the three is read on its own**, and one that is refused leaves the other two
    /// standing with a sentence saying what is missing. A credential is granted scopes one by
    /// one: a machine enrolled without `projects:read` still has sessions and agents to choose
    /// from, and failing the whole screen over the one it cannot read was a picker that showed
    /// nothing but an error. `Err` only when there is no connection at all.
    pub async fn sessions(&self) -> zyris::Result<SessionsView> {
        let api = self.api()?;
        let (projects, sessions, agents) =
            tokio::join!(api.list_projects(), api.list_sessions(), api.list_agents());
        let mut problems = Vec::new();
        let mut keep = |what: &str, error: WireError| {
            tracing::warn!(%error, "could not read this account's {what}");
            problems.push(format!("{what}: {}", error.message));
        };
        let projects = projects.unwrap_or_else(|error| {
            keep("projects", error);
            Vec::new()
        });
        let sessions = sessions.unwrap_or_else(|error| {
            keep("sessions", error);
            Vec::new()
        });
        let agents = agents.unwrap_or_else(|error| {
            keep("agents", error);
            Vec::new()
        });
        Ok(SessionsView {
            projects,
            sessions,
            agents: agents.into_iter().map(|(id, name)| AgentEntry { id, name }).collect(),
            current: self.session_id(),
            problems,
        })
    }

    /// Talk to another session from now on, on the connection that is current.
    ///
    /// **The same path a new connection takes**, rather than a second way of subscribing:
    /// [`Feed::attach`] bumps the generation, so the old subscription's pump stops at its next
    /// frame and nothing it was in the middle of is read aloud as if it belonged to this one.
    /// The cursor is dropped with the session it belonged to — a cursor from one session's
    /// timeline means nothing in another's.
    ///
    /// `Err` when there is no connection, or when the subscription could not be opened; in the
    /// second case the feed is left pointing at the session asked for, and the next connection
    /// tries it again.
    pub async fn switch_to(self: &Arc<Self>, session: String) -> zyris::Result<()> {
        let api = self.api()?;
        if self.session_id().as_deref() == Some(session.as_str()) && self.is_live() {
            return Ok(());
        }
        *self.session_id.lock().expect("the session id is not poisoned") = Some(session);
        self.state.lock().expect("the feed state is not poisoned").cursor = None;
        self.attach(api).await;
        if self.is_live() {
            Ok(())
        } else {
            Err(WireError::new(
                ErrorCode::Internal,
                "Attacca would not let this machine listen to that session",
            ))
        }
    }

    /// Create a session in `project` against `agent`, and talk to it from now on.
    ///
    /// `agent: None` is the same rule a first session is made by — one agent is taken, several
    /// is a choice this does not make — so a screen that offers no agent picker on an account
    /// with one agent gets the obvious answer and one with several is told to pick.
    pub async fn start_new(
        self: &Arc<Self>,
        project: Option<String>,
        agent: Option<String>,
    ) -> zyris::Result<String> {
        let api = self.api()?;
        let agent = match agent {
            Some(agent) => agent,
            None => {
                let agents = api.list_agents().await?;
                Feed::choose_agent(&agents, self.agent.as_deref()).ok_or_else(|| {
                    WireError::new(
                        ErrorCode::InvalidParams,
                        if agents.is_empty() {
                            "this account has no agent to start a session with"
                        } else {
                            "this account has more than one agent; choose which one to talk to"
                        },
                    )
                })?
            }
        };
        let id = api.create_session(agent, project).await?;
        tracing::info!(session = %id, "created a session from the Conversation screen");
        self.switch_to(id.clone()).await?;
        Ok(id)
    }

    /// Say something, starting a turn.
    ///
    /// **Refuses while no subscription is open**, which is the ordering rule made unavoidable
    /// rather than remembered: a message sent now would be answered by deltas that arrive before
    /// anything is listening for them, and `after: None` does not replay them.
    pub async fn say(&self, message: impl Into<String>) -> zyris::Result<()> {
        let api = {
            let state = self.state.lock().expect("the feed state is not poisoned");
            if !state.live {
                return Err(WireError::new(
                    ErrorCode::ConnectionLost,
                    "nothing is listening to this session, so a message sent now would be \
                     answered into silence; the turn feed subscribes when the connection comes \
                     up",
                ));
            }
            state.api.clone()
        };
        let Some(api) = api else {
            // Unreachable: `live` is only ever set with a client in the same lock.
            return Err(WireError::new(ErrorCode::ConnectionLost, "no connection"));
        };
        let Some(session) = self.session_id() else {
            // Unreachable while `live`: nothing subscribes without a session. Refused rather
            // than unwrapped, because the cost of being wrong is a panic inside an audio
            // session and the cost of being right is one branch.
            return Err(WireError::new(ErrorCode::ConnectionLost, "there is no session"));
        };
        api.send_message(session, message.into()).await
    }

    /// Stop the turn that is running.
    ///
    /// **Not refused while the subscription is down**, unlike [`Feed::say`], and the asymmetry
    /// is deliberate: `say` is refused because a message sent into an unwatched session loses
    /// its answer, whereas a cancel has no answer to lose. What it needs is a client, which
    /// outlives the subscription — a stream failed by `StreamLagged` is re-opened on the same
    /// connection, and cancelling in that window is exactly the case where speech has been
    /// stopped and the agent is still generating.
    pub async fn cancel(&self) -> zyris::Result<()> {
        let api = self.state.lock().expect("the feed state is not poisoned").api.clone();
        let Some(api) = api else {
            return Err(WireError::new(
                ErrorCode::ConnectionLost,
                "this node is not connected, so there is no turn it can stop",
            ));
        };
        let Some(session) = self.session_id() else {
            return Err(WireError::new(
                ErrorCode::ConnectionLost,
                "there is no session, so there is no turn to stop",
            ));
        };
        api.cancel_turn(session).await
    }

    /// Make a session on this connection. `false` means there is still none.
    ///
    /// Whatever goes wrong here is logged and not published: a feed with no session publishes
    /// nothing either way, and the Voice screen is where a person is told — it reads the same
    /// settings and asks the same question, before anybody has spoken.
    async fn make_a_session(self: &Arc<Self>, api: &dyn TurnApi) -> bool {
        let agents = match api.list_agents().await {
            Ok(agents) => agents,
            Err(error) => {
                tracing::warn!(%error, "could not read this account's agents, so no session \
                     was created; the next connection tries again");
                return false;
            }
        };
        let Some(agent) = Feed::choose_agent(&agents, self.agent.as_deref()) else {
            *self.agent_trouble.lock().expect("not poisoned") =
                Some(agents.into_iter().map(|(_, name)| name).collect());
            return false;
        };
        *self.agent_trouble.lock().expect("not poisoned") = None;
        let id = match api.create_session(agent, None).await {
            Ok(id) => id,
            Err(error) => {
                tracing::warn!(%error, "could not create a session");
                return false;
            }
        };
        tracing::info!(session = %id, "created a session for the voice");
        *self.session_id.lock().expect("the session id is not poisoned") = Some(id);
        true
    }

    /// Which agent to create against.
    ///
    /// **One agent is not a choice, and several is.** `list_agents` does not document its
    /// order — `list_projects` promises "the default first, then the rest oldest-first" and
    /// this one promises nothing — so taking the first would be relying on an order nobody
    /// offered, and would quietly change which agent this machine talks to the day somebody
    /// adds one. The same reading `announce.rs` gives about controls that cannot work: an
    /// arbitrary answer is worse than none, because nobody can tell it from a considered one.
    fn choose_agent(agents: &[(String, String)], named: Option<&str>) -> Option<String> {
        if let Some(named) = named {
            let found = agents.iter().find(|(id, name)| id == named || name == named);
            if found.is_none() {
                tracing::warn!(
                    agent = named,
                    "the agent named in the voice settings is not on this account"
                );
            }
            return found.map(|(id, _)| id.clone());
        }
        match agents {
            [] => {
                tracing::warn!("this account has no agent, so no session can be created");
                None
            }
            [(id, _)] => Some(id.clone()),
            several => {
                tracing::warn!(
                    agents = several.len(),
                    names = several.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>().join(", "),
                    "this account has more than one agent, so Zyris will not choose; name one \
                     in the voice settings"
                );
                None
            }
        }
    }

    /// Open one subscription, and hand back its items.
    async fn open(&self, api: &dyn TurnApi, generation: u64) -> Opened {
        let after = self.state.lock().expect("the feed state is not poisoned").cursor;
        let Some(session) = self.session_id() else {
            // `attach` makes one before it gets here, so this is the case where it could not.
            return Opened::Failed;
        };
        let streaming = match api.turn_events(session, after).await {
            Ok(streaming) => streaming,
            Err(error) if session_is_gone(&error) => return Opened::Gone,
            Err(error) => {
                tracing::warn!(%error, "could not subscribe to this session's turns");
                self.publish(
                    TurnEvent::Lost { reason: error.to_string(), resubscribing: false },
                    generation,
                );
                return Opened::Failed;
            }
        };

        {
            let mut state = self.state.lock().expect("the feed state is not poisoned");
            if state.generation != generation {
                // A newer connection came up while this subscription was being opened. Its
                // stream is the one anything reads; this one is dropped here.
                return Opened::Failed;
            }
            if state.cursor.is_none() {
                // See the module documentation: only ever on the first subscription.
                state.cursor = streaming.head.last_cursor;
            }
            state.live = true;
        }

        self.publish(TurnEvent::Running(streaming.head.running), generation);
        Opened::Items(streaming.items)
    }

    /// Read one subscription after another for as long as this generation is the current one.
    ///
    /// A loop rather than `pump` calling `open` again, because an `async fn` that awaits itself
    /// through a spawn is a future whose type contains itself and does not compile.
    async fn run(
        self: Arc<Self>,
        api: Arc<dyn TurnApi>,
        generation: u64,
        mut items: zyris::ItemStream<ZTurnFrame>,
    ) {
        loop {
            match self.pump(&mut items, generation).await {
                Next::Stop => return,
                // A session gone in the middle of a connection is left for the next one's
                // `attach`, which is where one is made.
                Next::Resubscribe => match self.open(api.as_ref(), generation).await {
                    Opened::Items(next) => items = next,
                    Opened::Failed | Opened::Gone => return,
                },
            }
        }
    }

    /// One subscription, from its first frame to whatever ends it.
    ///
    /// The filter and the splitter live here rather than on the feed, and that is a decision:
    /// deltas are not durable and are **not** replayed, so whatever a lost connection was in the
    /// middle of — an open fence, half a word — belongs to a stream that is gone. Carrying it
    /// across would resume a sentence with the state of a different one.
    async fn pump(
        &self,
        items: &mut zyris::ItemStream<ZTurnFrame>,
        generation: u64,
    ) -> Next {
        let mut filter = Filter::new();
        let mut splitter = Splitter::new();

        loop {
            let idle = tokio::time::sleep(IDLE_FLUSH);
            tokio::pin!(idle);

            tokio::select! {
                item = items.next() => match item {
                    Some(Ok(frame)) => {
                        if !self.on_frame(frame, &mut filter, &mut splitter, generation) {
                            return Next::Stop;
                        }
                    }
                    Some(Err(error)) => return self.on_stream_error(error, generation),
                    None => {
                        // The server ended the stream without failing it. Nothing is coming, and
                        // nothing here can ask for more on this connection.
                        self.publish(
                            TurnEvent::Lost {
                                reason: "the turn stream ended".to_string(),
                                resubscribing: false,
                            },
                            generation,
                        );
                        return Next::Stop;
                    }
                },
                // A stream that stalls mid-sentence. Guarded on something actually being held —
                // by either of the two things that hold text, since the filter keeps the word a
                // delta ended on and the splitter keeps everything short of a boundary. An
                // unguarded arm would flush nothing, over and over, every 750 ms of an idle
                // connection.
                _ = &mut idle, if !splitter.held().trim().is_empty() || filter.holds_a_word() => {
                    let held = filter.pause();
                    for fragment in splitter.push(&held) {
                        if !self.publish(TurnEvent::Say(fragment), generation) {
                            return Next::Stop;
                        }
                    }
                    if let Some(fragment) = splitter.flush() {
                        if !self.publish(TurnEvent::Say(fragment), generation) {
                            return Next::Stop;
                        }
                    }
                }
            }
        }
    }

    /// One frame. `false` means this generation is over.
    fn on_frame(
        &self,
        frame: ZTurnFrame,
        filter: &mut Filter,
        splitter: &mut Splitter,
        generation: u64,
    ) -> bool {
        match frame {
            ZTurnFrame::Delta { kind, text } => {
                // **The one place the protocol's two kinds become the filter's two kinds**, and
                // it is a `match` rather than a cast so that a third arm upstream is a compile
                // error here rather than a delta silently read aloud. Rule 1 is then the
                // filter's, decided before a character is looked at.
                let kind = match kind {
                    ZDeltaKind::Assistant => Kind::Assistant,
                    ZDeltaKind::Reasoning => Kind::Reasoning,
                };
                let reading = filter.read(kind, &text);
                if !self.publish(
                    TurnEvent::Shown { kind, text: reading.shown.text().to_string() },
                    generation,
                ) {
                    return false;
                }
                for fragment in splitter.push(&reading.aloud) {
                    if !self.publish(TurnEvent::Say(fragment), generation) {
                        return false;
                    }
                }
                true
            }
            ZTurnFrame::Event { cursor, event } => {
                {
                    let mut state = self.state.lock().expect("the feed state is not poisoned");
                    if state.generation != generation {
                        return false;
                    }
                    state.cursor = Some(cursor);
                }
                self.publish(TurnEvent::Event { cursor, event }, generation)
            }
            ZTurnFrame::Status { running } => {
                if !running {
                    // **The end of a turn, and the only thing that releases the last word.**
                    // `Filter::finish` holds the final token until something tells it the turn
                    // is over — there is no trailing space after the last word of an answer —
                    // and `Splitter::flush` releases a last sentence short of `MIN_CHARS`. A
                    // feed that skipped either would lose the end of every answer, quietly.
                    let rest = filter.finish();
                    for fragment in splitter.push(&rest) {
                        if !self.publish(TurnEvent::Say(fragment), generation) {
                            return false;
                        }
                    }
                    if let Some(fragment) = splitter.flush() {
                        if !self.publish(TurnEvent::Say(fragment), generation) {
                            return false;
                        }
                    }
                }
                self.publish(TurnEvent::Running(running), generation)
            }
        }
    }

    /// A stream that failed. The two codes mean different things; see the module documentation.
    fn on_stream_error(&self, error: WireError, generation: u64) -> Next {
        let lagged = error.code == ErrorCode::StreamLagged;
        {
            let mut state = self.state.lock().expect("the feed state is not poisoned");
            if state.generation != generation {
                return Next::Stop;
            }
            state.live = false;
        }
        self.publish(
            TurnEvent::Lost { reason: error.to_string(), resubscribing: lagged },
            generation,
        );
        if lagged {
            Next::Resubscribe
        } else {
            Next::Stop
        }
    }

    /// Publish, unless this pump belongs to a connection that is no longer the current one.
    ///
    /// `false` is "stop reading": a subscription from a dead connection must not be the one
    /// anything reads, and the cheapest way to guarantee it is that its every output goes
    /// through here.
    fn publish(&self, event: TurnEvent, generation: u64) -> bool {
        if self.state.lock().expect("the feed state is not poisoned").generation != generation {
            return false;
        }
        // An error is "nobody is subscribed", which is ordinary: the feed comes up with the
        // connection and a consumer may not exist yet.
        let _ = self.events.send(event);
        true
    }
}

/// What a finished subscription leaves the loop to do.
#[derive(Debug, PartialEq, Eq)]
enum Next {
    /// This generation is over.
    Stop,
    /// The connection is still good; open another subscription on it from the cursor.
    Resubscribe,
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::time::Duration;

    use tokio::sync::mpsc;

    use super::*;

    /// What reached Attacca, in order. The ordering rule is decided on this and not on a
    /// timing: a test that waited and then looked would pass for an implementation that
    /// subscribed late but quickly.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) enum Call {
        Subscribe { after: Option<i64> },
        Send { message: String },
        Cancel,
        Agents,
        Create { agent: String, project: Option<String> },
    }

    #[derive(Default)]
    struct Script {
        calls: Vec<Call>,
        /// One head per subscription, in order; the default is used once they run out.
        heads: VecDeque<ZTurnStatus>,
        /// The far end of each subscription's stream, so a test can push frames into it.
        /// The far end of each subscription. `None` once the test has ended that stream --
        /// which has to be done here as well as at the call site, because both hold a clone and
        /// the channel closes only when the last one goes.
        senders: Vec<Option<mpsc::UnboundedSender<zyris::Result<ZTurnFrame>>>>,
        /// Refuse the next `turn_events` outright.
        refuse_subscribe: bool,
        /// Answer the next `turn_events` the way Attacca answers for a deleted session.
        session_gone: bool,
        /// What `list_agents` answers. Empty by default, which is an account with none.
        agents: Vec<(String, String)>,
        /// What `create_session` answers with.
        made: Option<String>,
        /// Which session each `turn_events` was for, in order.
        subscribed_to: Vec<String>,
        /// Refuse `list_projects` the way Attacca does for a credential without the scope.
        no_projects_scope: bool,
    }

    /// A stand-in for Attacca.
    ///
    /// **Where this is a guess rather than a protocol fact**, said plainly because there is no
    /// reference consumer to check against: that a subscription's items arrive as an ordinary
    /// stream the test can feed one frame at a time, and that a failed stream yields
    /// `Some(Err(..))` and then ends. Both are how `zyris-core` builds one — `StreamEvent::Failed`
    /// is delivered to the same receiver the data went to, and the entry is removed — but no test
    /// anywhere exercises it, so this double is the specification and would be wrong with it.
    pub(super) struct Fake {
        script: Arc<Mutex<Script>>,
    }

    impl Fake {
        pub(super) fn new() -> Arc<Fake> {
            Arc::new(Fake { script: Arc::new(Mutex::new(Script::default())) })
        }

        pub(super) fn calls(&self) -> Vec<Call> {
            self.script.lock().unwrap().calls.clone()
        }

        fn subscribed_to(&self) -> Vec<String> {
            self.script.lock().unwrap().subscribed_to.clone()
        }

        fn subscriptions(&self) -> usize {
            self.script.lock().unwrap().senders.len()
        }

        /// What this account's agents are.
        pub(super) fn with_agents(self: &Arc<Self>, agents: &[(&str, &str)]) -> Arc<Fake> {
            self.script.lock().unwrap().agents =
                agents.iter().map(|(id, name)| (id.to_string(), name.to_string())).collect();
            self.clone()
        }

        fn head(self: &Arc<Self>, head: ZTurnStatus) -> Arc<Self> {
            self.script.lock().unwrap().heads.push_back(head);
            self.clone()
        }

        pub(super) fn refuse_next_subscribe(self: &Arc<Self>) -> Arc<Self> {
            self.script.lock().unwrap().refuse_subscribe = true;
            self.clone()
        }

        pub(super) fn lose_the_session(self: &Arc<Self>) -> Arc<Self> {
            self.script.lock().unwrap().session_gone = true;
            self.clone()
        }

        /// End the `nth` subscription's stream, as a server closing it does. The caller drops
        /// its own clone too, or the channel stays open.
        fn end(&self, nth: usize) {
            self.script.lock().unwrap().senders[nth] = None;
        }

        /// The sender of the `nth` subscription, waiting for it to exist.
        async fn stream(&self, nth: usize) -> mpsc::UnboundedSender<zyris::Result<ZTurnFrame>> {
            for _ in 0..2000 {
                if let Some(Some(tx)) = self.script.lock().unwrap().senders.get(nth) {
                    return tx.clone();
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            panic!("subscription {nth} was never opened");
        }
    }

    #[zyris::async_trait]
    impl TurnApi for Fake {
        async fn list_agents(&self) -> zyris::Result<Vec<(String, String)>> {
            let mut script = self.script.lock().unwrap();
            script.calls.push(Call::Agents);
            Ok(script.agents.clone())
        }

        async fn create_session(
            &self,
            agent_id: String,
            project: Option<String>,
        ) -> zyris::Result<String> {
            let mut script = self.script.lock().unwrap();
            script.calls.push(Call::Create { agent: agent_id, project });
            Ok(script.made.clone().unwrap_or_else(|| "made-1".to_string()))
        }

        async fn list_projects(&self) -> zyris::Result<Vec<ProjectEntry>> {
            if self.script.lock().unwrap().no_projects_scope {
                return Err(WireError::new(
                    ErrorCode::ForbiddenScope,
                    "this credential was not granted the projects:read scope",
                ));
            }
            Ok(vec![ProjectEntry {
                id: "p-default".to_string(),
                name: "Default".to_string(),
                is_default: true,
            }])
        }

        async fn list_sessions(&self) -> zyris::Result<Vec<SessionEntry>> {
            Ok(vec![SessionEntry {
                id: "session-1".to_string(),
                title: Some("First".to_string()),
                project: Some("p-default".to_string()),
                agent: Some("a1".to_string()),
                running: false,
            }])
        }

        async fn turn_events(
            &self,
            session_id: String,
            after: Option<i64>,
        ) -> zyris::Result<zyris::Streaming<ZTurnStatus, ZTurnFrame>> {
            let (tx, rx) = mpsc::unbounded_channel();
            let head = {
                let mut script = self.script.lock().unwrap();
                script.calls.push(Call::Subscribe { after });
                script.subscribed_to.push(session_id);
                if std::mem::take(&mut script.refuse_subscribe) {
                    return Err(WireError::new(ErrorCode::Internal, "refused"));
                }
                if std::mem::take(&mut script.session_gone) {
                    // Attacca's own words: `wire()` on a `RepoError::NotFound`.
                    return Err(WireError::new(
                        ErrorCode::Internal,
                        "session not found: 0b7e6a52-7c5e-4d8e-9d2b-2f0f7d1b3a11",
                    ));
                }
                script.senders.push(Some(tx));
                script.heads.pop_front().unwrap_or(ZTurnStatus {
                    session_id: "s".to_string(),
                    running: false,
                    last_cursor: None,
                })
            };
            let items = futures_util::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|item| (item, rx))
            });
            Ok(zyris::Streaming::new(head, items))
        }

        async fn send_message(&self, _session_id: String, message: String) -> zyris::Result<()> {
            self.script.lock().unwrap().calls.push(Call::Send { message });
            Ok(())
        }

        async fn cancel_turn(&self, _session_id: String) -> zyris::Result<()> {
            self.script.lock().unwrap().calls.push(Call::Cancel);
            Ok(())
        }
    }

    fn delta(kind: ZDeltaKind, text: &str) -> zyris::Result<ZTurnFrame> {
        Ok(ZTurnFrame::Delta { kind, text: text.to_string() })
    }

    fn assistant(text: &str) -> zyris::Result<ZTurnFrame> {
        delta(ZDeltaKind::Assistant, text)
    }

    fn durable(cursor: i64) -> zyris::Result<ZTurnFrame> {
        Ok(ZTurnFrame::Event {
            cursor,
            event: ZSessionEvent {
                seq: cursor,
                cursor,
                kind: "assistant_message".to_string(),
                payload: serde_json::json!({ "text": "hello" }),
                created_at: None,
            },
        })
    }

    /// Every await in these tests can hang — a subscription that never opens, a fragment that
    /// never arrives — and `#[tokio::test]` has no deadline of its own. So every wait is inside
    /// one of these, and the timeout is the assertion.
    async fn within<F: std::future::Future>(what: &str, f: F) -> F::Output {
        match tokio::time::timeout(Duration::from_secs(10), f).await {
            Ok(value) => value,
            Err(_) => panic!("timed out waiting for {what}"),
        }
    }

    /// Drain what the feed has published so far, as far as it has got.
    async fn next_event(events: &mut broadcast::Receiver<TurnEvent>) -> TurnEvent {
        within("an event from the feed", events.recv()).await.expect("the feed is still open")
    }

    async fn next_say(events: &mut broadcast::Receiver<TurnEvent>) -> String {
        loop {
            if let TurnEvent::Say(fragment) = next_event(events).await {
                return fragment.text().to_string();
            }
        }
    }

    /// The ordering rule. **`after: None` is live-only**, so a message sent before anything is
    /// listening is answered into a stream that does not exist yet — and nothing in the protocol
    /// says so.
    #[tokio::test]
    async fn a_message_is_never_sent_before_the_feed_is_listening() {
        let api = Fake::new();
        let feed = Feed::new("session-1");

        feed.attach(api.clone()).await;
        within("the message to be posted", feed.say("hello")).await.expect("posted");

        assert_eq!(
            api.calls(),
            vec![
                Call::Subscribe { after: None },
                Call::Send { message: "hello".to_string() }
            ],
            "the subscription has to be open before the message that starts the turn"
        );
    }

    /// The other half of that rule, and the one a reordering would not catch: a feed with no
    /// subscription **refuses** rather than posting into silence.
    #[tokio::test]
    async fn a_message_with_nothing_listening_is_refused_rather_than_posted() {
        let api = Fake::new();
        let feed = Feed::new("session-1");

        let error = feed.say("hello").await.expect_err("nothing is listening yet");
        assert_eq!(error.code, ErrorCode::ConnectionLost);
        assert!(api.calls().is_empty(), "the message must not have reached Attacca: {:?}", api.calls());

        // And a connection whose subscription was refused is not a feed either: the client is
        // there, so only `live` can tell these apart.
        feed.attach(api.refuse_next_subscribe()).await;
        assert!(!feed.is_live());
        feed.say("hello").await.expect_err("the subscription was refused");
        assert_eq!(
            api.calls(),
            vec![Call::Subscribe { after: None }],
            "a refused subscription must not be followed by a message"
        );
    }

    /// The same rule on a *re*-connection, which is the half that is easy to lose: a feed that
    /// was live a moment ago is not live while the connection that replaced it is still opening
    /// a subscription, and a message posted in that window is answered into nothing.
    #[tokio::test]
    async fn a_reconnection_is_not_listening_until_its_own_subscription_is_open() {
        let api = Fake::new();
        let feed = Feed::new("session-1");
        feed.attach(api.clone()).await;
        assert!(feed.is_live());

        feed.attach(api.refuse_next_subscribe()).await;
        assert!(!feed.is_live(), "the connection that just replaced the live one has no feed yet");
        feed.say("hello").await.expect_err("nothing is listening on this connection");
        assert_eq!(
            api.calls(),
            vec![Call::Subscribe { after: None }, Call::Subscribe { after: None }],
            "the message must not have been posted on the strength of the old subscription"
        );
    }

    /// Rule 1, at the seam this task owns. The kind is a protocol-level distinction and the only
    /// place it could be dropped is the mapping in `on_frame`.
    #[tokio::test]
    async fn a_reasoning_delta_is_shown_and_never_spoken() {
        let api = Fake::new();
        let feed = Feed::new("session-1");
        let mut events = feed.events();
        feed.attach(api.clone()).await;
        let stream = within("the subscription", api.stream(0)).await;

        stream.send(delta(ZDeltaKind::Reasoning, "The user wants a greeting. ")).unwrap();
        stream.send(assistant("Good morning to you, and welcome back. ")).unwrap();

        let mut shown = Vec::new();
        let mut spoken = Vec::new();
        while spoken.is_empty() {
            match next_event(&mut events).await {
                TurnEvent::Shown { kind, text } => shown.push((kind, text)),
                TurnEvent::Say(fragment) => spoken.push(fragment.text().to_string()),
                _ => {}
            }
        }

        assert_eq!(
            shown,
            vec![
                (Kind::Reasoning, "The user wants a greeting. ".to_string()),
                (Kind::Assistant, "Good morning to you, and welcome back. ".to_string()),
            ],
            "the screen gets both halves of a turn"
        );
        // Cut at the comma, which is the first fragment's rule and nobody else's: `split` lets
        // only the opening fragment end at a clause, because the whole wait is paid at the
        // front. What matters here is that not one word of the reasoning is in it.
        assert_eq!(spoken, vec!["Good morning to you,".to_string()]);
    }

    /// `Filter::finish` and `Splitter::flush`, which are what a turn's end is for. Without them
    /// the last word of every answer is held forever — "a caller that forgets it loses the last
    /// word", and this is the caller.
    ///
    /// **The stream is closed straight after the status frame, and that is deliberate.** With it
    /// left open, `IDLE_FLUSH` says all of this 750 ms later and every mutation of this path
    /// survives — a turn end that released nothing would be indistinguishable from one that did,
    /// except by a clock, and no test on this machine may assert on a clock. A server that ends
    /// the stream is also the ordinary case, and then the idle flush is never reached at all.
    ///
    /// `finish` rather than `pause` is decided by the unclosed bracket: an aside that never
    /// closed is released at the end of a turn and held through a mere stall.
    #[tokio::test]
    async fn the_end_of_a_turn_releases_the_last_word_and_an_aside_that_never_closed() {
        let api = Fake::new();
        let feed = Feed::new("session-1");
        let mut events = feed.events();
        feed.attach(api.clone()).await;
        let stream = within("the subscription", api.stream(0)).await;

        // No trailing space, no terminal stop, and a bracket nothing closes: every one of these
        // is held by the filter or the splitter until something says the turn is over.
        stream.send(assistant("All done (nearly")).unwrap();
        stream.send(Ok(ZTurnFrame::Status { running: false })).unwrap();
        api.end(0);
        drop(stream);

        let said = next_say(&mut events).await;
        assert!(said.contains("All done"), "the last word of the answer: {said:?}");
        assert!(said.contains("nearly"), "and the aside that never closed: {said:?}");
    }

    /// A stream that stalls mid-sentence. `IDLE_FLUSH` is the only thing that speaks it, and
    /// nothing else in the crate has a clock to apply it with.
    #[tokio::test(start_paused = true)]
    async fn a_stalled_stream_is_spoken_after_the_idle_flush() {
        let api = Fake::new();
        let feed = Feed::new("session-1");
        let mut events = feed.events();
        feed.attach(api.clone()).await;
        let stream = within("the subscription", api.stream(0)).await;

        // Under `MIN_CHARS`, no terminal punctuation, and the turn never ends.
        stream.send(assistant("One moment")).unwrap();

        assert_eq!(next_say(&mut events).await, "One moment");
    }

    /// The whole reason a cursor is kept. A reconnect that asked `None` again would ask for live
    /// frames only and lose everything the server recorded while this node was away.
    #[tokio::test]
    async fn a_reconnect_resumes_from_the_last_cursor_this_node_actually_saw() {
        let api = Fake::new();
        let feed = Feed::new("session-1");
        let mut events = feed.events();
        feed.attach(api.clone()).await;
        let stream = within("the subscription", api.stream(0)).await;

        stream.send(durable(7)).unwrap();
        loop {
            if let TurnEvent::Event { cursor, event } = next_event(&mut events).await {
                assert_eq!(cursor, 7);
                assert_eq!(event.kind, "assistant_message", "the durable event is carried whole");
                assert_eq!(event.payload, serde_json::json!({ "text": "hello" }));
                break;
            }
        }

        feed.attach(api.clone()).await;
        assert_eq!(
            api.calls(),
            vec![Call::Subscribe { after: None }, Call::Subscribe { after: Some(7) }],
        );
    }

    /// The first subscription has no cursor of its own, and `after: None` means everything
    /// before it was deliberately not delivered — so the head's `last_cursor` is exactly the
    /// watermark for what has been skipped, and only then.
    #[tokio::test]
    async fn the_first_subscription_takes_its_cursor_from_the_head_and_a_later_one_does_not() {
        let api = Fake::new()
            .head(ZTurnStatus {
                session_id: "session-1".to_string(),
                running: false,
                last_cursor: Some(4),
            })
            .head(ZTurnStatus {
                session_id: "session-1".to_string(),
                running: false,
                // The server's tip has moved on. Adopting this would skip the replay between 4
                // and 9 that the second subscription was opened to collect.
                last_cursor: Some(9),
            });
        let feed = Feed::new("session-1");

        feed.attach(api.clone()).await;
        feed.attach(api.clone()).await;
        feed.attach(api.clone()).await;

        assert_eq!(
            api.calls(),
            vec![
                Call::Subscribe { after: None },
                Call::Subscribe { after: Some(4) },
                Call::Subscribe { after: Some(4) },
            ],
        );
    }

    /// A subscription belongs to one connection, and the previous one's is still a running task
    /// holding a live stream. Nothing it says may be heard, and nothing it saw may move the
    /// cursor the live subscription resumes from.
    #[tokio::test]
    async fn a_dead_connection_is_not_the_one_anything_reads() {
        let api = Fake::new();
        let feed = Feed::new("session-1");
        feed.attach(api.clone()).await;
        let dead = within("the first subscription", api.stream(0)).await;

        feed.attach(api.clone()).await;
        let _live = within("the second subscription", api.stream(1)).await;
        let mut events = feed.events();

        // Everything a live stream would produce, on the dead one.
        dead.send(assistant("This should never be said out loud. ")).unwrap();
        dead.send(durable(99)).unwrap();
        dead.send(Ok(ZTurnFrame::Status { running: false })).unwrap();

        // Nothing arrives. The wait is the assertion, and it is bounded.
        assert!(
            tokio::time::timeout(Duration::from_millis(300), events.recv()).await.is_err(),
            "a stream from a connection that is gone must not reach a consumer"
        );

        feed.attach(api.clone()).await;
        assert_eq!(
            api.calls().last(),
            Some(&Call::Subscribe { after: None }),
            "the dead connection's cursor must not become the live one's resume point"
        );
    }

    /// A chunk-sequence gap. The protocol requires the stream to fail rather than deliver past
    /// it, and `zyris-core` cancels the stream — but the **connection** is fine, and the cursor
    /// is exactly what recovers it.
    #[tokio::test]
    async fn a_chunk_gap_resubscribes_on_the_same_connection() {
        let api = Fake::new();
        let feed = Feed::new("session-1");
        let mut events = feed.events();
        feed.attach(api.clone()).await;
        let stream = within("the subscription", api.stream(0)).await;

        stream.send(durable(3)).unwrap();
        stream.send(Err(WireError::new(ErrorCode::StreamLagged, "chunk gap: expected 5 got 7"))).unwrap();

        let lost = loop {
            if let TurnEvent::Lost { reason, resubscribing } = next_event(&mut events).await {
                break (reason, resubscribing);
            }
        };
        assert!(lost.1, "a gap is recovered here, not by waiting for another connection");
        assert!(lost.0.contains("chunk gap"), "the reason says what happened: {}", lost.0);

        within("the second subscription", api.stream(1)).await;
        assert_eq!(
            api.calls(),
            vec![Call::Subscribe { after: None }, Call::Subscribe { after: Some(3) }],
            "the gap is resumed from the last cursor, on the connection that is still up"
        );
    }

    /// The other failure, which looks identical from a `Result` and is not. Nothing here can
    /// re-open a stream on a connection that is gone; the connect hook is what restores it.
    #[tokio::test]
    async fn a_lost_connection_waits_for_the_next_one_rather_than_resubscribing() {
        let api = Fake::new();
        let feed = Feed::new("session-1");
        let mut events = feed.events();
        feed.attach(api.clone()).await;
        let stream = within("the subscription", api.stream(0)).await;

        stream.send(Err(WireError::connection_lost())).unwrap();

        let resubscribing = loop {
            if let TurnEvent::Lost { resubscribing, .. } = next_event(&mut events).await {
                break resubscribing;
            }
        };
        assert!(!resubscribing);

        // Long enough that a retry would have happened, and bounded.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(api.subscriptions(), 1, "a dead connection cannot be re-subscribed on");
        assert!(!feed.is_live(), "and nothing may be sent into it either");
        feed.say("hello").await.expect_err("the connection is gone");
    }

    /// What a turn's running flag does, both at subscription time and mid-stream. The head
    /// carries one and it is how a node that attaches mid-answer knows there is one in flight.
    #[tokio::test]
    async fn the_head_says_whether_a_turn_is_already_running() {
        let api = Fake::new().head(ZTurnStatus {
            session_id: "session-1".to_string(),
            running: true,
            last_cursor: None,
        });
        let feed = Feed::new("session-1");
        let mut events = feed.events();
        feed.attach(api.clone()).await;

        assert_eq!(next_event(&mut events).await, TurnEvent::Running(true));
    }

    // -----------------------------------------------------------------------------------------
    // Choosing a session from the Conversation screen
    // -----------------------------------------------------------------------------------------

    /// **Switching is a new subscription, not a relabelled one.** The old stream is still a
    /// running task holding a live channel; anything it says after the switch belongs to the
    /// session somebody just left and must not be read aloud as an answer from the new one. And
    /// the cursor goes with the session: a cursor from one timeline asked of another would skip
    /// or replay events that have nothing to do with it.
    #[tokio::test]
    async fn switching_listens_to_the_new_session_and_nothing_from_the_old_one_is_said() {
        let api = Fake::new();
        let feed = Feed::new("session-1");
        feed.attach(api.clone()).await;
        let old = within("the first subscription", api.stream(0)).await;
        old.send(durable(7)).unwrap();
        within("the cursor to move", async {
            while feed.state.lock().unwrap().cursor != Some(7) {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await;

        within("the switch", feed.switch_to("session-2".to_string())).await.expect("switched");
        let new = within("the second subscription", api.stream(1)).await;
        let mut events = feed.events();

        assert_eq!(api.subscribed_to(), vec!["session-1", "session-2"]);
        assert_eq!(
            api.calls().last(),
            Some(&Call::Subscribe { after: None }),
            "the old session's cursor was carried into the new one"
        );
        assert_eq!(feed.session_id(), Some("session-2".to_string()));

        old.send(assistant("This is from the session that was left. ")).unwrap();
        old.send(Ok(ZTurnFrame::Status { running: false })).unwrap();
        new.send(assistant("This is the new one. ")).unwrap();
        new.send(Ok(ZTurnFrame::Status { running: false })).unwrap();

        assert_eq!(next_say(&mut events).await, "This is the new one.");
        within("the message to be posted", feed.say("hello")).await.expect("posted");
    }

    /// With no connection there is no client to switch on, and saying so is the whole answer —
    /// the feed must not quietly change which session it will use at the next connection.
    #[tokio::test]
    async fn switching_with_no_connection_is_refused_and_changes_nothing() {
        let feed = Feed::new("session-1");

        let error = feed.switch_to("session-2".to_string()).await.expect_err("not connected");
        assert_eq!(error.code, ErrorCode::ConnectionLost);
        assert_eq!(feed.session_id(), Some("session-1".to_string()));
        feed.sessions().await.expect_err("nothing to list without a connection");
    }

    /// A new session goes in the project somebody picked, and becomes the one this machine
    /// talks to — made first, subscribed second, the order the rest of this module keeps.
    #[tokio::test]
    async fn a_new_session_is_made_in_the_chosen_project_and_listened_to() {
        let api = Fake::new().with_agents(&[("agent-1", "Ada")]);
        let feed = Feed::new("session-1");
        feed.attach(api.clone()).await;

        let id = within("the new session", feed.start_new(Some("p-2".into()), None))
            .await
            .expect("made");

        assert_eq!(id, "made-1");
        assert_eq!(feed.session_id(), Some("made-1".to_string()));
        assert_eq!(
            api.calls()[1..],
            [
                Call::Agents,
                Call::Create { agent: "agent-1".into(), project: Some("p-2".into()) },
                Call::Subscribe { after: None },
            ]
        );
        assert_eq!(api.subscribed_to(), vec!["session-1", "made-1"]);
    }

    /// An agent named by the screen is used as given, without reading the list.
    #[tokio::test]
    async fn a_new_session_takes_the_agent_it_is_given() {
        let api = Fake::new().with_agents(&[("a", "Ada"), ("b", "Bea")]);
        let feed = Feed::new("session-1");
        feed.attach(api.clone()).await;

        feed.start_new(None, Some("b".into())).await.expect("made");

        assert_eq!(
            api.calls()[1..2],
            [Call::Create { agent: "b".into(), project: None }],
            "the agent was chosen for it rather than taken from the screen"
        );
    }

    /// Several agents and none named is a choice this does not make, from the screen any more
    /// than at startup: nothing is created, and the session in use is left alone.
    #[tokio::test]
    async fn a_new_session_with_several_agents_and_none_chosen_is_refused() {
        let api = Fake::new().with_agents(&[("a", "Ada"), ("b", "Bea")]);
        let feed = Feed::new("session-1");
        feed.attach(api.clone()).await;

        let error = feed.start_new(None, None).await.expect_err("there is a choice to make");
        assert_eq!(error.code, ErrorCode::InvalidParams);
        assert!(!api.calls().iter().any(|c| matches!(c, Call::Create { .. })));
        assert_eq!(feed.session_id(), Some("session-1".to_string()));
    }

    /// Measured on a real account: a machine enrolled without `projects:read` got nothing but
    /// that error where the picker should have been. The sessions and agents it *can* read are
    /// still the whole of what choosing needs.
    #[tokio::test]
    async fn a_scope_the_credential_lacks_costs_that_list_and_nothing_else() {
        let api = Fake::new().with_agents(&[("a1", "Ada")]);
        api.script.lock().unwrap().no_projects_scope = true;
        let feed = Feed::new("session-1");
        feed.attach(api.clone()).await;

        let view = feed.sessions().await.expect("the rest is still listed");
        assert!(view.projects.is_empty());
        assert_eq!(view.sessions.len(), 1);
        assert_eq!(view.agents.len(), 1);
        assert_eq!(view.problems.len(), 1);
        assert!(view.problems[0].contains("projects:read"), "{:?}", view.problems);
    }

    /// What the screen is handed: every project, every session, every agent, and which session
    /// is current.
    #[tokio::test]
    async fn the_sessions_view_carries_the_account_and_the_current_session() {
        let api = Fake::new().with_agents(&[("a1", "Ada")]);
        let feed = Feed::new("session-1");
        feed.attach(api.clone()).await;

        let view = feed.sessions().await.expect("listed");
        assert_eq!(view.current.as_deref(), Some("session-1"));
        assert_eq!(view.projects.len(), 1);
        assert_eq!(view.sessions[0].project.as_deref(), Some("p-default"));
        assert_eq!(view.agents, vec![AgentEntry { id: "a1".into(), name: "Ada".into() }]);
    }
}

#[cfg(test)]
mod stopping_a_turn {
    use super::*;
    use super::tests::{Call, Fake};

    /// **A cancel reaches Attacca**, which is the half of barge-in that leaves this machine.
    #[tokio::test]
    async fn cancelling_a_turn_reaches_the_session_it_is_for() {
        let api = Fake::new();
        let feed = Feed::new("s");
        feed.attach(api.clone()).await;

        feed.cancel().await.expect("a live connection takes a cancel");

        assert!(api.calls().contains(&Call::Cancel));
    }

    /// **A cancel is not refused while the subscription is down, and [`Feed::say`] is** — the
    /// asymmetry is the point.
    ///
    /// `say` is refused because a message sent into a session nobody is watching loses its
    /// answer: `after: None` replays nothing. A cancel has no answer to lose, and the window
    /// where it is most wanted is exactly the one where the subscription has just failed — a
    /// chunk gap between the stream dying and the next one opening, with speech already stopped
    /// and an agent still generating.
    #[tokio::test]
    async fn a_cancel_is_not_refused_in_the_window_where_a_message_would_be() {
        let api = Fake::new();
        let feed = Feed::new("s");
        feed.attach(api.clone()).await;
        feed.take_the_subscription_down();

        assert!(feed.say("hello").await.is_err(), "a message has an answer to lose");
        feed.cancel().await.expect("a cancel does not");
    }

    /// A machine that has never connected has no turn to stop, and says so rather than
    /// pretending it did.
    #[tokio::test]
    async fn a_node_that_never_connected_cannot_cancel_anything() {
        let feed = Feed::new("s");

        let refused = feed.cancel().await.expect_err("there is no connection");

        assert_eq!(refused.code, ErrorCode::ConnectionLost);
    }
    // -----------------------------------------------------------------------------------------
    // Making a session
    // -----------------------------------------------------------------------------------------

    /// **A fresh install has no session and nothing ever made one.** The spec's loop starts
    /// *get a session*; until this, the id had to be hand-written into `voice.json` and a
    /// person who installed Zyris had a speaking half that did nothing until they did.
    #[tokio::test]
    async fn a_feed_with_no_session_makes_one_before_it_subscribes() {
        let api = Fake::new().with_agents(&[("agent-1", "Ada")]);
        let feed = Feed::making_one(None);

        feed.attach(api.clone()).await;

        // The order is the assertion, not a timing: creating after subscribing would subscribe
        // to nothing, and creating after the first message would lose its answer.
        assert_eq!(
            api.calls(),
            vec![
                Call::Agents,
                Call::Create { agent: "agent-1".into(), project: None },
                Call::Subscribe { after: None },
            ]
        );
        assert_eq!(feed.session_id(), Some("made-1".to_string()));
        // The id is state on the feed rather than something announced; `Engine::on_connect`
        // reads it and writes it down.
    }

    /// **Once, not once per connection.** A node that made a session on every reconnect would
    /// fill the account and lose the conversation every time the network moved — the same
    /// failure as a machine that forgot its credential and enrolled again, one layer up.
    #[tokio::test]
    async fn a_reconnection_does_not_make_a_second_session() {
        let api = Fake::new().with_agents(&[("agent-1", "Ada")]);
        let feed = Feed::making_one(None);

        feed.attach(api.clone()).await;
        feed.attach(api.clone()).await;

        let made = api.calls().iter().filter(|c| matches!(c, Call::Create { .. })).count();
        assert_eq!(made, 1, "a reconnection made another session: {:?}", api.calls());
    }

    /// A session named in the settings is used as it stands. Nothing is created and the
    /// account's agents are not even read.
    #[tokio::test]
    async fn a_session_that_was_given_is_not_replaced() {
        let api = Fake::new().with_agents(&[("agent-1", "Ada")]);
        let feed = Feed::new("session-1");

        feed.attach(api.clone()).await;

        assert_eq!(api.calls(), vec![Call::Subscribe { after: None }]);
        assert_eq!(feed.session_id(), Some("session-1".to_string()));
    }

    /// **A session deleted in the web app is replaced, once.** Retrying the dead id on every
    /// reconnect was a voice that never came back, with nothing on any screen to clear it.
    #[tokio::test]
    async fn a_session_that_is_gone_is_replaced_with_a_new_one() {
        let api = Fake::new().with_agents(&[("agent-1", "Ada")]).lose_the_session();
        let feed = Feed::continuing("deleted", None);

        feed.attach(api.clone()).await;

        assert_eq!(
            api.calls(),
            vec![
                Call::Subscribe { after: None },
                Call::Agents,
                Call::Create { agent: "agent-1".to_string(), project: None },
                Call::Subscribe { after: None },
            ]
        );
        assert_eq!(feed.session_id(), Some("made-1".to_string()));
        assert!(feed.is_live());
    }

    /// Anything else that refuses a subscription is not a reason to throw the session away: a
    /// server having a bad minute must not cost somebody their conversation.
    #[tokio::test]
    async fn a_refused_subscription_keeps_the_session() {
        let api = Fake::new().with_agents(&[("agent-1", "Ada")]).refuse_next_subscribe();
        let feed = Feed::continuing("session-1", None);

        feed.attach(api.clone()).await;

        assert_eq!(api.calls(), vec![Call::Subscribe { after: None }]);
        assert_eq!(feed.session_id(), Some("session-1".to_string()));
    }

    /// **An account with no agent gets no session, and nothing is guessed.** There is nothing
    /// to create against; creating an agent would be this program making something on somebody
    /// else's account because it wanted a place to talk.
    #[tokio::test]
    async fn an_account_with_no_agent_gets_no_session() {
        let api = Fake::new();
        let feed = Feed::making_one(None);

        feed.attach(api.clone()).await;

        assert_eq!(api.calls(), vec![Call::Agents]);
        assert_eq!(feed.session_id(), None);
    }

    /// **Several agents is a choice and Zyris does not make it.** `list_agents` does not
    /// document its order, so taking the first would rely on an order nobody offered — and
    /// would quietly change which agent this machine talks to the day somebody adds one.
    #[tokio::test]
    async fn several_agents_and_none_named_is_refused_rather_than_guessed() {
        let api = Fake::new().with_agents(&[("a", "Ada"), ("b", "Grace")]);
        let feed = Feed::making_one(None);

        feed.attach(api.clone()).await;

        assert_eq!(api.calls(), vec![Call::Agents]);
        assert_eq!(feed.session_id(), None);
    }

    /// And naming one settles it. By id or by name: a person reads names and a settings file
    /// holds whatever they typed.
    #[tokio::test]
    async fn naming_an_agent_settles_which() {
        for named in ["b", "Grace"] {
            let api = Fake::new().with_agents(&[("a", "Ada"), ("b", "Grace")]);
            let feed = Feed::making_one(Some(named.to_string()));

            feed.attach(api.clone()).await;

            assert!(
                api.calls().contains(&Call::Create { agent: "b".into(), project: None }),
                "{named} did not settle it: {:?}",
                api.calls()
            );
        }
    }

    /// An agent named that is not there is refused rather than falling back to a different
    /// one. Somebody who named an agent meant that agent.
    #[tokio::test]
    async fn an_agent_that_is_not_there_is_not_replaced_by_another() {
        let api = Fake::new().with_agents(&[("a", "Ada")]);
        let feed = Feed::making_one(Some("Grace".to_string()));

        feed.attach(api.clone()).await;

        assert_eq!(api.calls(), vec![Call::Agents]);
        assert_eq!(feed.session_id(), None);
    }
}
