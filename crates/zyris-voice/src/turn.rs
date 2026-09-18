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
    /// a cancel and an ordinary finish are the same thing. That is why barge-in posts a message
    /// saying where speech was cut off rather than the transcript recording it: see
    /// [`crate::session::Interruption`]. An upstream issue asks for a delivery point.
    async fn cancel_turn(&self, session_id: String) -> zyris::Result<()>;
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
    session_id: String,
    events: broadcast::Sender<TurnEvent>,
    state: Mutex<State>,
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

impl Feed {
    /// A feed for one session, with nothing attached to it yet.
    pub fn new(session_id: impl Into<String>) -> Arc<Feed> {
        Arc::new(Feed {
            session_id: session_id.into(),
            events: broadcast::channel(EVENT_CAPACITY).0,
            state: Mutex::new(State { api: None, generation: 0, live: false, cursor: None }),
        })
    }

    /// The session this feed is for.
    pub fn session_id(&self) -> &str {
        &self.session_id
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

        let Some(items) = self.open(api.as_ref(), generation).await else { return };
        let feed = self.clone();
        tokio::spawn(async move { feed.run(api, generation, items).await });
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
        api.send_message(self.session_id.clone(), message.into()).await
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
        api.cancel_turn(self.session_id.clone()).await
    }

    /// Open one subscription, and hand back its items. `None` means give up on this generation.
    async fn open(
        &self,
        api: &dyn TurnApi,
        generation: u64,
    ) -> Option<zyris::ItemStream<ZTurnFrame>> {
        let after = self.state.lock().expect("the feed state is not poisoned").cursor;
        let streaming = match api.turn_events(self.session_id.clone(), after).await {
            Ok(streaming) => streaming,
            Err(error) => {
                tracing::warn!(%error, "could not subscribe to this session's turns");
                self.publish(
                    TurnEvent::Lost { reason: error.to_string(), resubscribing: false },
                    generation,
                );
                return None;
            }
        };

        {
            let mut state = self.state.lock().expect("the feed state is not poisoned");
            if state.generation != generation {
                // A newer connection came up while this subscription was being opened. Its
                // stream is the one anything reads; this one is dropped here.
                return None;
            }
            if state.cursor.is_none() {
                // See the module documentation: only ever on the first subscription.
                state.cursor = streaming.head.last_cursor;
            }
            state.live = true;
        }

        self.publish(TurnEvent::Running(streaming.head.running), generation);
        Some(streaming.items)
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
                Next::Resubscribe => match self.open(api.as_ref(), generation).await {
                    Some(next) => items = next,
                    None => return,
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

        fn subscriptions(&self) -> usize {
            self.script.lock().unwrap().senders.len()
        }

        fn head(self: &Arc<Self>, head: ZTurnStatus) -> Arc<Self> {
            self.script.lock().unwrap().heads.push_back(head);
            self.clone()
        }

        fn refuse_next_subscribe(self: &Arc<Self>) -> Arc<Self> {
            self.script.lock().unwrap().refuse_subscribe = true;
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
        async fn turn_events(
            &self,
            _session_id: String,
            after: Option<i64>,
        ) -> zyris::Result<zyris::Streaming<ZTurnStatus, ZTurnFrame>> {
            let (tx, rx) = mpsc::unbounded_channel();
            let head = {
                let mut script = self.script.lock().unwrap();
                script.calls.push(Call::Subscribe { after });
                if std::mem::take(&mut script.refuse_subscribe) {
                    return Err(WireError::new(ErrorCode::Internal, "refused"));
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
}
