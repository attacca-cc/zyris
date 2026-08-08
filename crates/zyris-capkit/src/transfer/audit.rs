//! TODO(Task 1.7): 전송마다 한 줄.
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
    pub direct: bool,
}

pub struct Audit {
    path: PathBuf,
}

impl Audit {
    pub fn new(path: impl Into<PathBuf>) -> Audit {
        Audit { path: path.into() }
    }
    pub async fn record(&self, _line: AuditLine) {
        unimplemented!("Task 1.7")
    }
}
