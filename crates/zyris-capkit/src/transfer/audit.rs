//! One line per transfer. **In a flow with no human confirmation this is the only way to find out
//! afterwards what happened.**
//!
//! Failing to write it does not stop the transfer — a log that cannot be written is no reason for
//! a file not to arrive.

use std::path::PathBuf;

#[derive(Debug, Clone, serde::Serialize)]
pub struct AuditLine {
    pub at_ms: u64,
    pub peer_slug: String,
    pub peer_endpoint: String,
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
    pub written: String,
    pub replaced: bool,
    /// Where the replaced original was stashed, if it was.
    ///
    /// **`replaced: true` together with `undo: null` means the original is gone** — stashing was
    /// attempted and failed, and the overwrite went ahead anyway (see [`super::undo`] for why it
    /// goes ahead). Nothing else in the line distinguishes a reversible overwrite from a
    /// permanent loss, which is exactly the moment an audit log exists for.
    pub undo: Option<String>,
    /// A direct connection, or through a relay. The seed for measuring the relay ratio.
    pub direct: bool,
}

pub struct Audit {
    path: PathBuf,
}

impl Audit {
    pub fn new(path: impl Into<PathBuf>) -> Audit {
        Audit { path: path.into() }
    }
    pub async fn record(&self, line: AuditLine) {
        if let Err(e) = self.write(line).await {
            tracing::warn!(error = %e, path = %self.path.display(), "failed to write audit log");
        }
    }

    async fn write(&self, line: AuditLine) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt;
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut text = serde_json::to_string(&line).unwrap_or_default();
        text.push('\n');
        let mut f =
            tokio::fs::OpenOptions::new().create(true).append(true).open(&self.path).await?;
        f.write_all(text.as_bytes()).await
    }
}

#[cfg(test)]
mod tests {
    use super::{Audit, AuditLine};

    fn one_line() -> AuditLine {
        AuditLine {
            at_ms: 1_754_700_000_000,
            peer_slug: "arch-zyris-code".into(),
            peer_endpoint: "abc123".into(),
            name: "report.pdf".into(),
            bytes: 4096,
            sha256: "de.ad".into(),
            written: "/home/x/inbox/a/report.pdf".into(),
            replaced: true,
            undo: Some("/home/x/undo/1754700000000-0/report.pdf".into()),
            direct: false,
        }
    }

    #[tokio::test]
    async fn one_transfer_appends_one_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transfers.log");
        let audit = Audit::new(&path);
        audit.record(one_line()).await;
        audit.record(one_line()).await;

        let text = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(text.lines().count(), 2, "must append");
        let first_line: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(first_line["peer_slug"], "arch-zyris-code");
        assert_eq!(first_line["replaced"], true);
    }

    #[tokio::test]
    async fn overwrite_without_backup_shows_up_in_the_log() {
        // A reversible overwrite and a permanently lost original are told apart only by `undo`.
        // Without this field they read as identical, letter for letter, in the audit log.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transfers.log");
        let audit = Audit::new(&path);
        audit.record(AuditLine { undo: None, ..one_line() }).await;

        let text = tokio::fs::read_to_string(&path).await.unwrap();
        let line: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(line["replaced"], true);
        assert!(line["undo"].is_null(), "an overwrite without a backup must show up in the log");
    }

    #[tokio::test]
    async fn failing_to_write_does_not_block_the_transfer() {
        let audit = Audit::new("/proc/unwritable-dir/x.log");
        audit.record(one_line()).await; // passes as long as this does not panic
    }
}
