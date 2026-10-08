//! Asking a running node to stop, and answering that request.
//!
//! **A file rather than a signal**, and that is a decision with a cost on both sides:
//!
//! - A signal would have to be `SIGTERM` on one platform and `TerminateProcess` on another, and
//!   both are addressed by a **pid**, which is a number that can be reused. The only pid a console
//!   has to go on is the one a state file recorded, so a node that had stopped and a machine that
//!   had since started something else under that number would be a `zyris down` that killed an
//!   unrelated process. `--headless` is exactly the mode somebody runs on a machine where other
//!   things also run.
//! - The file costs a wedge: a node whose poll loop is not running never sees the request, and
//!   `zyris down` reports that rather than pretending. On Windows a hard kill also leaves the
//!   tray icon behind until something hovers over it, which is the second thing this avoids.
//!
//! What it buys besides the shape: a stop that goes the same way `Ctrl-C` and the tray's Quit do,
//! so the MCP servers are stopped and the push-to-talk key is handed back — the same two things
//! `gui.rs` and `headless.rs` already do on their own exits.
//!
//! The request is consumed by whoever acts on it, so a request nobody answered cannot stop the
//! next node: `main` clears any leftover on the way in.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::watch;

/// The request file, inside the instance's data directory.
///
/// Per instance like everything else this run owns, which is what makes `zyris --server URL down`
/// the way to stop a development run and `zyris down` the way to stop the real one.
pub const STOP_FILE: &str = "stop-request";

/// How often the node looks. A second is below the threshold at which somebody who typed a
/// command wonders whether it did anything, and a `stat` a second costs nothing next to the
/// socket the same process is holding open.
const POLL: std::time::Duration = std::time::Duration::from_secs(1);

/// A request waiting for the node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The process that asked, when it could be read out of the file. For the log line and nothing
    /// else: nothing here decides anything by a pid.
    pub by: Option<u32>,
}

/// Where the request file is for a run whose state is in `data`.
pub fn path(data: &Path) -> PathBuf {
    data.join(STOP_FILE)
}

/// Ask the node whose state is in `data` to stop.
///
/// Written through a temporary file and renamed, because the *existence* of this path is the
/// signal: a half-written file that already exists is a request, and a request is what a node acts
/// on. There is nothing to get right about the content beyond that.
pub fn request(data: &Path) -> io::Result<()> {
    std::fs::create_dir_all(data)?;
    let ask = serde_json::json!({
        "pid": std::process::id(),
        "atUnixMs": now_ms(),
    });
    let path = path(data);
    let part = path.with_extension(format!("part-{}", std::process::id()));
    std::fs::write(&part, ask.to_string())?;
    std::fs::rename(&part, &path).inspect_err(|_| {
        let _ = std::fs::remove_file(&part);
    })
}

/// The request waiting for the node, if there is one.
///
/// **Existence is the answer, and the content is only a note.** A person who creates this file by
/// hand with nothing in it has asked for the same thing as somebody who typed `zyris down`, and a
/// file that cannot be read is not a reason to ignore them.
pub fn requested(data: &Path) -> Option<Request> {
    if !path(data).exists() {
        return None;
    }
    let by = std::fs::read_to_string(path(data))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|ask| ask.get("pid").and_then(|pid| pid.as_u64()))
        .map(|pid| pid as u32);
    Some(Request { by })
}

/// Take the request away, so that it is acted on exactly once.
///
/// Called by whoever acts on it. A request that is not there is not an error: two callers can race
/// here — the node that is stopping, and a `zyris down` that decided to tidy up — and the loser of
/// that race has nothing to report.
pub fn clear(data: &Path) {
    match std::fs::remove_file(path(data)) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(%error, "could not remove the stop request"),
    }
}

/// Watch for a request, for as long as the task runs, and say so through the receiver.
///
/// **The receiver resolves `true` exactly once, when somebody asked.** It also resolves when this
/// task ends — a dropped sender turns `changed()` into an error at once — which is why the value
/// and not the result is what callers read: `*asked.borrow()` is `true` for a real request and
/// `false` for a watcher that has gone, and those are two different things to do something about.
///
/// The request is removed here, before the answer goes out, so that the node stopping is the same
/// thing as the request being consumed.
pub fn ask_when_requested(data: &Path, runtime: &tokio::runtime::Handle) -> watch::Receiver<bool> {
    let (asked, told) = watch::channel(false);
    let data = data.to_path_buf();
    runtime.spawn(async move {
        let mut ticker = tokio::time::interval(POLL);
        loop {
            // The first tick completes immediately, so a request that arrived while this process
            // was starting is acted on at once rather than a second later.
            ticker.tick().await;
            let Some(request) = requested(&data) else { continue };
            clear(&data);
            match request.by {
                Some(by) => tracing::info!(by, "a stop request arrived"),
                None => tracing::info!("a stop request arrived"),
            }
            let _ = asked.send(true);
            return;
        }
    });
    told
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_there_until_somebody_takes_it() {
        // The half that makes the whole thing safe: the request is consumed by the node that acts
        // on it, so a request no node ever answered cannot stop the next one to start.
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(requested(dir.path()), None);

        request(dir.path()).unwrap();
        assert_eq!(requested(dir.path()), Some(Request { by: Some(std::process::id()) }));

        clear(dir.path());
        assert_eq!(requested(dir.path()), None);
        // And clearing twice is not an error, because a node and a `zyris down` can both be
        // tidying up at the same moment.
        clear(dir.path());
    }

    #[test]
    fn a_request_with_nothing_in_it_is_still_a_request() {
        // Somebody who creates the file by hand has asked for the same thing as somebody who
        // typed the command, and the node acts on the file's *existence*.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(path(dir.path()), b"").unwrap();

        assert_eq!(requested(dir.path()), Some(Request { by: None }));
    }

    #[tokio::test]
    async fn the_watcher_says_so_once_and_consumes_the_request() {
        let dir = tempfile::tempdir().unwrap();
        let mut asked =
            ask_when_requested(dir.path(), &tokio::runtime::Handle::current());

        assert!(!*asked.borrow(), "nothing has been asked yet");
        request(dir.path()).unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(5), asked.changed())
            .await
            .expect("the watcher never saw the request")
            .expect("the watcher is gone");

        assert!(*asked.borrow());
        assert_eq!(
            requested(dir.path()),
            None,
            "the request has to be consumed, or the next node to start would stop on it"
        );
    }

    #[tokio::test]
    async fn a_watcher_that_ends_says_false_rather_than_asking_for_a_stop() {
        // *The distinction the caller's comment turns on.* A dropped sender resolves the receiver
        // too, and a caller that read only "the receiver resolved" would stop a node because a
        // task inside it had died. The watcher is run on a runtime of its own and that runtime is
        // taken away underneath it.
        let dir = tempfile::tempdir().unwrap();
        let other = tokio::runtime::Runtime::new().unwrap();
        let mut asked = ask_when_requested(dir.path(), other.handle());
        other.shutdown_background();

        tokio::time::timeout(std::time::Duration::from_secs(5), asked.changed())
            .await
            .expect("a watcher that ended has to resolve the receiver, or a node waits forever")
            .expect_err("nothing asked this node to stop, so this cannot resolve as a request");

        assert!(
            !*asked.borrow(),
            "a watcher that ended is not somebody asking this node to stop"
        );
    }
}
