//! 전송마다 한 줄. **사람 확인이 없는 흐름에서 사후에 무슨 일이 있었는지 아는 유일한 길이다.**
//!
//! 못 쓰더라도 전송을 막지 않는다 — 로그가 없다고 파일이 안 가야 할 이유가 없다.

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
    /// 직접 연결이었나, 릴레이를 지났나. 릴레이 비율을 재는 씨앗이다.
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
    async fn 못_써도_전송을_막지_않는다() {
        let audit = Audit::new("/proc/못쓰는자리/x.log");
        audit.record(한_줄()).await; // 패닉하지 않으면 통과
    }
}
