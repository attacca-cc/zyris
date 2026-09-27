//! A conversation with no microphone and no speaker: the phone build's.
//!
//! The same turn feed the voice engine drives ([`crate::turn::Feed`]), and the same traces the
//! Conversation screen reads, so the window is the one it is on a desktop. What it leaves out is
//! everything that needs whisper, Supertonic or an audio device — none of which builds for
//! Android or iOS yet — which is why this is its own feature and not a mode of [`crate::run`].

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

use crate::turn::{Feed, TurnEvent, VOICE_AGENT};
use crate::view::{HistoryView, SessionsView};
use crate::Trace;

/// The file, in the instance's data directory, that holds the session this device talks to.
pub const SESSION_FILE: &str = "conversation-session";

/// How many traces a window may fall behind before it loses the oldest; the voice engine's.
const TRACE_CAPACITY: usize = 512;

pub struct Chat {
    feed: Arc<Feed>,
    traces: broadcast::Sender<Trace>,
    session_path: Option<PathBuf>,
    /// Whether the task turning the feed into traces is running. Started on the first
    /// connection, since that is where a tokio runtime is certain to be.
    pumping: Mutex<bool>,
}

impl Chat {
    pub fn new(dir: Option<&std::path::Path>) -> Arc<Chat> {
        let session_path = dir.map(|dir| dir.join(SESSION_FILE));
        let saved = session_path
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty());
        let agent = Some(VOICE_AGENT.to_string());
        let feed = match saved {
            Some(session) => Feed::continuing(session, agent),
            None => Feed::making_one(agent),
        };
        Arc::new(Chat {
            feed,
            traces: broadcast::channel(TRACE_CAPACITY).0,
            session_path,
            pumping: Mutex::new(false),
        })
    }

    pub fn traces(&self) -> broadcast::Receiver<Trace> {
        self.traces.subscribe()
    }

    pub async fn on_connect(self: &Arc<Self>, connection: zyris::Connection) {
        self.pump();
        self.feed.on_connect(connection).await;
        self.remember();
    }

    pub async fn sessions(&self) -> Result<SessionsView, String> {
        self.feed.sessions().await.map_err(|error| error.message)
    }

    pub async fn history(&self) -> Result<HistoryView, String> {
        self.feed.history().await.map_err(|error| error.message)
    }

    pub async fn choose_session(self: &Arc<Self>, session: String) -> Result<(), String> {
        let switched = self.feed.switch_to(session).await.map_err(|error| error.message);
        self.remember();
        switched
    }

    pub async fn new_session(self: &Arc<Self>, project: Option<String>, agent: Option<String>) -> Result<(), String> {
        let made = self.feed.start_new(project, agent).await.map(|_| ()).map_err(|error| error.message);
        self.remember();
        made
    }

    /// Send a typed message, and say on the trace whether it went — the same two steps a
    /// spoken turn ends with, so the thread shows it the same way.
    pub async fn send_text(&self, text: String) -> Result<(), String> {
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err("there is nothing to send".to_string());
        }
        match self.feed.say(text.clone()).await {
            Ok(()) => {
                let _ = self.traces.send(Trace::Sent { text });
                Ok(())
            }
            Err(error) => {
                let _ = self.traces.send(Trace::SendFailed { reason: error.message.clone() });
                Err(format!("the message did not reach Attacca: {}", error.message))
            }
        }
    }

    fn remember(&self) {
        let (Some(path), Some(session)) = (&self.session_path, self.feed.session_id()) else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(error) = std::fs::write(path, &session) {
            tracing::warn!(%error, path = %path.display(), "could not remember the conversation's session");
        }
    }

    fn pump(&self) {
        let mut pumping = self.pumping.lock().expect("the pump flag is not poisoned");
        if *pumping {
            return;
        }
        *pumping = true;
        let mut events = self.feed.events();
        let traces = self.traces.clone();
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) => {
                        if let Some(step) = trace_of(event) {
                            let _ = traces.send(step);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        let _ = traces.send(Trace::Failed {
                            reason: format!("{missed} pieces of the answer came too fast to show"),
                        });
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });
    }
}

/// What the window shows of one thing the feed says. The voice engine's `Answers` does this
/// and also feeds a speaker; here there is nothing to read aloud, so every answer is `aloud:
/// false` and the fragments cut for speech are dropped.
fn trace_of(event: TurnEvent) -> Option<Trace> {
    match event {
        TurnEvent::Shown { kind, text } => Some(Trace::Delta { kind: format!("{kind:?}"), text }),
        TurnEvent::Running(true) => Some(Trace::Answering { aloud: false }),
        TurnEvent::Running(false) => Some(Trace::Answered),
        TurnEvent::Event { event, .. } if event.kind == "error" => Some(Trace::Failed {
            reason: match event.payload.get("message").and_then(|m| m.as_str()) {
                Some(message) => format!("The agent did not answer: {message}"),
                None => "The agent did not answer.".to_string(),
            },
        }),
        TurnEvent::Event { event, .. } => progress(&event),
        // The feed resubscribes by itself; the voice engine says nothing of this either.
        TurnEvent::Lost { .. } => None,
        TurnEvent::Say(_) => None,
    }
}

/// A durable event as the progress it shows: a note or a reasoning title, or a tool call. A
/// `report_result` call is the answer itself, not progress, and is left to the feed; a wrapper
/// that runs other calls is not a call of its own.
pub(crate) fn progress(event: &zyris_attacca::ZSessionEvent) -> Option<Trace> {
    let text = |key: &str| {
        event.payload.get(key).and_then(|v| v.as_str()).map(str::trim).filter(|t| !t.is_empty()).map(str::to_string)
    };
    match event.kind.as_str() {
        "work_summary" => text("content").map(|title| Trace::Working { title }),
        "thinking" => text("title").map(|title| Trace::Working { title }),
        "tool_call" => text("name")
            .filter(|name| !matches!(name.as_str(), "report_result" | "sequential_tool_calls" | "parallel_tool_calls"))
            .map(|name| Trace::Tool { name }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::speak::Kind;

    #[test]
    fn a_turn_reads_as_the_traces_the_window_knows() {
        assert_eq!(trace_of(TurnEvent::Running(true)), Some(Trace::Answering { aloud: false }));
        assert_eq!(
            trace_of(TurnEvent::Shown { kind: Kind::Assistant, text: "Hi.".into() }),
            Some(Trace::Delta { kind: "Assistant".into(), text: "Hi.".into() })
        );
        assert_eq!(trace_of(TurnEvent::Running(false)), Some(Trace::Answered));
    }
}
