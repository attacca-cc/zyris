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
            tracing::warn!(error = %e, path = %self.path.display(), "감사 로그를 쓰지 못했습니다");
        }
    }

    async fn write(&self, line: AuditLine) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt;
        if let Some(부모) = self.path.parent() {
            tokio::fs::create_dir_all(부모).await?;
        }
        let mut 글 = serde_json::to_string(&line).unwrap_or_default();
        글.push('\n');
        let mut f =
            tokio::fs::OpenOptions::new().create(true).append(true).open(&self.path).await?;
        f.write_all(글.as_bytes()).await
    }
}

#[cfg(test)]
mod tests {
    use super::{Audit, AuditLine};

    fn 한_줄() -> AuditLine {
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
    async fn 한_전송이_한_줄로_쌓인다() {
        let 자리 = tempfile::tempdir().unwrap();
        let 길 = 자리.path().join("transfers.log");
        let audit = Audit::new(&길);
        audit.record(한_줄()).await;
        audit.record(한_줄()).await;

        let 글 = tokio::fs::read_to_string(&길).await.unwrap();
        assert_eq!(글.lines().count(), 2, "append여야 한다");
        let 첫_줄: serde_json::Value = serde_json::from_str(글.lines().next().unwrap()).unwrap();
        assert_eq!(첫_줄["peer_slug"], "arch-zyris-code");
        assert_eq!(첫_줄["replaced"], true);
    }

    #[tokio::test]
    async fn 백업을_못_하고_덮은_것이_로그에서_드러난다() {
        // 되돌릴 수 있게 덮은 것과 원본을 영영 잃은 것은 `undo`로만 갈린다. 이 필드가 없으면
        // 감사 로그에서 둘이 글자 하나 다르지 않다.
        let 자리 = tempfile::tempdir().unwrap();
        let 길 = 자리.path().join("transfers.log");
        let audit = Audit::new(&길);
        audit.record(AuditLine { undo: None, ..한_줄() }).await;

        let 글 = tokio::fs::read_to_string(&길).await.unwrap();
        let 줄: serde_json::Value = serde_json::from_str(글.lines().next().unwrap()).unwrap();
        assert_eq!(줄["replaced"], true);
        assert!(줄["undo"].is_null(), "백업 없이 덮은 것이 로그에 드러나야 한다");
    }

    #[tokio::test]
    async fn 못_써도_전송을_막지_않는다() {
        let audit = Audit::new("/proc/못쓰는자리/x.log");
        audit.record(한_줄()).await; // 패닉하지 않으면 통과
    }
}
