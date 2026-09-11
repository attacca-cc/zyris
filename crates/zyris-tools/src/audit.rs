//! What ran on this machine.
//!
//! One line per call, appended as JSON so the file stays readable when it is the only thing a
//! support report has. It records what was asked for — the command, the path — and never the
//! payload: a file's contents and a PTY's output are the things most likely to hold a secret,
//! and a log nobody dares hand over is not a log.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// How a call ended, from the machine's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Outcome {
    /// It ran. Whether the agent liked the result is not this log's business.
    Allowed,
    /// The gate stopped it.
    Refused,
    /// The dispatch itself errored — bad parameters, a transport fault, a spawn that failed.
    ///
    /// **Not** a command that exited non-zero: `exec` returns `Ok(ExecOutput { exit_code, .. })`
    /// for a command that exited 1, so that is an `Allowed` line here. A person reading this log
    /// will assume the opposite unless told.
    Failed,
}

impl Outcome {
    /// The wire spelling, identical to what `serde` writes for this value.
    ///
    /// `CoreEvent::ToolCall` carries the outcome as a plain `String` rather than this enum, so
    /// the window reads the live event and the stored entry through the same three words. The
    /// test below is what keeps the two spellings from drifting.
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Allowed => "allowed",
            Outcome::Refused => "refused",
            Outcome::Failed => "failed",
        }
    }
}

/// One call.
///
/// `detail` is a short, human-readable summary of what was asked for — a command line, a path.
/// It is deliberately not the payload: file contents and terminal output are where secrets live.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    /// RFC 3339, UTC.
    pub at: String,
    pub capability: String,
    pub tool: String,
    pub detail: String,
    pub outcome: Outcome,
}

/// How many lines `recent` will read back before giving up. The file is append-only and a busy
/// machine will grow it; the window only ever shows the tail.
const SCAN_LIMIT: usize = 2000;

#[derive(Clone)]
pub struct AuditLog {
    path: PathBuf,
    /// Serializes appends from concurrent tool calls so two lines never interleave.
    writing: Arc<Mutex<()>>,
}

impl AuditLog {
    pub fn new(path: PathBuf) -> AuditLog {
        AuditLog { path, writing: Arc::new(Mutex::new(())) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends one line. Never fails the caller: a tool call must not fail because the log
    /// could not be written, and the person needs to hear about it in the process log instead.
    pub fn record(&self, entry: Entry) {
        let line = match serde_json::to_string(&entry) {
            Ok(line) => line,
            Err(error) => {
                tracing::error!(%error, "could not encode an audit entry");
                return;
            }
        };
        let _guard = self.writing.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Err(error) = self.append(&line) {
            tracing::error!(%error, path = %self.path.display(), "could not write the audit log");
        }
    }

    fn append(&self, line: &str) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
        writeln!(file, "{line}")
    }

    /// The newest entries first.
    ///
    /// **`Err` is not an empty log.** A file that cannot be read — its permissions changed, a
    /// directory sits where it should be, the disk answered with an error — is a history nobody
    /// can see, and a reader handed `Vec::new()` for it would tell a person that nothing has ever
    /// run on their machine. That is the one wrong answer this function can give, so the error is
    /// handed back instead of swallowed. `NotFound` is the exception and really is `Ok(vec![])`:
    /// a log that was never written is genuinely empty.
    ///
    /// The file is read as bytes and decoded lossily rather than through `read_to_string`, which
    /// validates UTF-8 across the whole file: one bad byte from a half-written append would
    /// otherwise cost the entire history rather than the single line it landed in.
    ///
    /// A line that will not parse is skipped rather than allowed to hide every line after it —
    /// a truncated write must not cost the whole history.
    pub fn recent(&self, limit: usize) -> std::io::Result<Vec<Entry>> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        Ok(String::from_utf8_lossy(&bytes)
            .lines()
            .rev()
            .take(SCAN_LIMIT)
            .filter_map(|line| serde_json::from_str::<Entry>(line).ok())
            .take(limit)
            .collect())
    }
}

/// Now, as the log spells it.
pub fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(tool: &str, outcome: Outcome) -> Entry {
        Entry {
            at: "2026-09-11T00:00:00Z".to_string(),
            capability: "terminal".to_string(),
            tool: tool.to_string(),
            detail: "ls -la".to_string(),
            outcome,
        }
    }

    #[test]
    fn a_recorded_call_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let log = AuditLog::new(dir.path().join("audit.jsonl"));

        log.record(entry("exec", Outcome::Allowed));

        let recent = log.recent(10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].tool, "exec");
    }

    #[test]
    fn recent_returns_the_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let log = AuditLog::new(dir.path().join("audit.jsonl"));
        log.record(entry("open", Outcome::Allowed));
        log.record(entry("exec", Outcome::Allowed));

        let recent = log.recent(10).unwrap();

        assert_eq!(recent[0].tool, "exec", "the last thing that ran is the thing a person is looking for");
    }

    #[test]
    fn recent_honours_its_limit() {
        let dir = tempfile::tempdir().unwrap();
        let log = AuditLog::new(dir.path().join("audit.jsonl"));
        for _ in 0..5 {
            log.record(entry("exec", Outcome::Allowed));
        }

        assert_eq!(log.recent(2).unwrap().len(), 2);
    }

    #[test]
    fn a_refusal_is_recorded_as_one() {
        // A paused machine that logged nothing would leave a person wondering whether the agent
        // ever tried. The refusal is the interesting part.
        let dir = tempfile::tempdir().unwrap();
        let log = AuditLog::new(dir.path().join("audit.jsonl"));

        log.record(entry("exec", Outcome::Refused));

        assert_eq!(log.recent(1).unwrap()[0].outcome, Outcome::Refused);
    }

    #[test]
    fn the_file_survives_a_new_handle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        AuditLog::new(path.clone()).record(entry("exec", Outcome::Allowed));

        let reopened = AuditLog::new(path);

        assert_eq!(reopened.recent(10).unwrap().len(), 1);
    }

    #[test]
    fn a_write_that_cannot_land_does_not_panic() {
        // The log must never be the reason a tool call fails. A machine that cannot write it
        // should still work, loudly in its own logs and quietly to the caller.
        //
        // A file standing where the parent directory should be is what makes the write fail,
        // and it is the one way to arrange that which holds on every platform. An earlier
        // version of this test used `/proc/nonexistent/`, which Windows reads as `C:\proc\\
        // nonexistent\` and cheerfully creates — so the write succeeded, the failure under test
        // never happened, and the test both passed for the wrong reason on Linux and failed on
        // Windows.
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("not-a-directory");
        std::fs::write(&blocker, "").unwrap();
        let path = blocker.join("audit.jsonl");
        let log = AuditLog::new(path.clone());

        // Returning at all is the assertion: a panic here fails the test, and `record` has no
        // error to hand back by design.
        log.record(entry("exec", Outcome::Allowed));

        assert!(!path.exists(), "the entry landed somewhere it could not");
    }

    #[test]
    fn a_log_that_cannot_be_read_is_an_error_rather_than_an_empty_history() {
        // The one answer this must never give. A caller handed `Ok(vec![])` for a log it could
        // not open would tell a person that nothing has ever run on their machine, which is a
        // confident false negative about the record of what touched it. A directory standing
        // where the file should be is the cheapest way to make a read fail that is not
        // `NotFound`.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        std::fs::create_dir(&path).unwrap();

        let read = AuditLog::new(path).recent(10);

        assert!(read.is_err(), "an unreadable log came back as an empty one");
    }

    #[test]
    fn a_log_that_was_never_written_is_genuinely_empty() {
        // The exception, and the reason this is not simply "any failure is an error": a machine
        // where no agent has run anything yet has no file, and that is not a fault to report.
        let dir = tempfile::tempdir().unwrap();

        let read = AuditLog::new(dir.path().join("never-written.jsonl")).recent(10);

        assert_eq!(read.unwrap(), Vec::new());
    }

    #[test]
    fn one_bad_byte_costs_one_line_rather_than_the_whole_history() {
        // A half-written append under a full disk can cut a non-ASCII `detail` mid-codepoint.
        // Validating UTF-8 across the whole file would turn that into a permanently unreadable
        // history, which is the opposite of what this log is for.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let good = serde_json::to_string(&entry("exec", Outcome::Allowed)).unwrap();
        let mut bytes = b"{\"at\":\"x\",\xff\xfe garbage\n".to_vec();
        bytes.extend_from_slice(good.as_bytes());
        bytes.push(b'\n');
        std::fs::write(&path, bytes).unwrap();

        let recent = AuditLog::new(path).recent(10).unwrap();

        assert_eq!(recent.len(), 1, "the good line was lost with the bad one");
        assert_eq!(recent[0].tool, "exec");
    }

    #[test]
    fn an_unreadable_line_is_skipped_rather_than_losing_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let log = AuditLog::new(path.clone());
        log.record(entry("exec", Outcome::Allowed));
        std::fs::write(
            &path,
            format!("{{ this is not json\n{}\n", serde_json::to_string(&entry("open", Outcome::Allowed)).unwrap()),
        )
        .unwrap();

        let recent = AuditLog::new(path).recent(10).unwrap();

        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].tool, "open");
    }

    #[test]
    fn the_wire_shape_is_what_the_window_reads() {
        let json = serde_json::to_string(&entry("exec", Outcome::Refused)).unwrap();

        assert_eq!(
            json,
            r#"{"at":"2026-09-11T00:00:00Z","capability":"terminal","tool":"exec","detail":"ls -la","outcome":"refused"}"#
        );
    }

    #[test]
    fn every_outcome_spells_itself_the_same_way_twice() {
        // `as_str` and `serde` are two independent spellings of the same three words, and the
        // window reads a live `CoreEvent::ToolCall` through the first and a stored `Entry`
        // through the second. Nothing but this test stops them drifting apart.
        for outcome in [Outcome::Allowed, Outcome::Refused, Outcome::Failed] {
            assert_eq!(
                serde_json::to_string(&outcome).unwrap(),
                format!(r#""{}""#, outcome.as_str())
            );
        }
    }
}
