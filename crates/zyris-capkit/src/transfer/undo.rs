//! TODO(Task 1.3): 덮기 전 원본을 옮겨 둔다.
use std::path::{Path, PathBuf};

pub struct UndoStore {
    root: PathBuf,
}

impl UndoStore {
    pub fn new(root: impl Into<PathBuf>) -> UndoStore {
        UndoStore { root: root.into() }
    }
    pub async fn stash(&self, _victim: &Path, _now_ms: u64) -> Option<PathBuf> {
        unimplemented!("Task 1.3")
    }
}
