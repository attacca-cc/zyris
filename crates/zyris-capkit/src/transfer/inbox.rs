//! TODO(Task 1.2): inbox 감옥.
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum InboxError {
    Escaped,
    SymlinkInPath,
    Io(String),
}

impl std::fmt::Display for InboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for InboxError {}

pub struct Inbox {
    root: PathBuf,
}

impl Inbox {
    pub fn new(root: impl Into<PathBuf>) -> Inbox {
        Inbox { root: root.into() }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub async fn resolve(&self, _peer_slug: &str, _proposed: &str) -> Result<PathBuf, InboxError> {
        unimplemented!("Task 1.2")
    }
}
