//! Where received files land. **It is a jail.**
//!
//! `zyris-capkit`'s `path::resolve_under` is not one — its own comment says "the root is a
//! default, not a jail", absolute paths sail straight through, and symlinks are not considered at
//! all. What happens here is writing what someone else sent onto their machine, so that rule
//! cannot be reused.
//!
//! There are two defenses. Name washing (`super::name`) guards the ordinary path, and this file's
//! real-path checks catch whatever slipped past the washing. **Either one alone is not enough.**

use std::path::{Path, PathBuf};

use super::name::safe_name;

#[derive(Debug)]
pub enum InboxError {
    /// The final path is outside the root.
    Escaped,
    /// One of the path's components is a symbolic link.
    SymlinkInPath,
    Io(String),
}

impl std::fmt::Display for InboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InboxError::Escaped => write!(f, "destination is outside the inbox"),
            InboxError::SymlinkInPath => write!(f, "path contains a symbolic link"),
            InboxError::Io(e) => write!(f, "{e}"),
        }
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

    /// Settles the final path and creates its parent directory.
    ///
    /// `peer_slug` gets washed too — the peer names itself, so that is untrusted input as much as
    /// the file name is.
    pub async fn resolve(&self, peer_slug: &str, proposed: &str) -> Result<PathBuf, InboxError> {
        let parent = self.root.join(safe_name(peer_slug));
        tokio::fs::create_dir_all(&parent).await.map_err(|e| InboxError::Io(e.to_string()))?;

        let root = tokio::fs::canonicalize(&self.root)
            .await
            .map_err(|e| InboxError::Io(e.to_string()))?;

        // `canonicalize` follows links, so whatever is past a link can look "inside the root"
        // whether it actually is or not — each component has to be checked directly, before the
        // escape check that follows. In particular, if a link points at another spot that is
        // itself "inside" the root (another peer's directory, say), the escape check alone can
        // never catch it.
        //
        // The walk is anchored on **`self.root` unnormalized**. Anchoring on `root` (the
        // canonicalized one) instead would break as soon as any ancestor of `self.root` is a
        // symlink (macOS's `/var` → `/private/var`, a symlinked home directory, and so on):
        // `parent.strip_prefix(root)` would fail, `unwrap_or` would hand back the whole absolute
        // path, and the walk would restart from the filesystem root — flagging every ancestor
        // outside the inbox as a symlink and rejecting every ordinary request. `parent` is
        // `self.root.join(..)`, so `strip_prefix` against `self.root` always succeeds, and the
        // walk only ever looks at the components newly appended inside the inbox. The inbox
        // itself (or one of its ancestors) being a symlink is the receiving side's own setup and
        // is allowed; only a component **inside** the inbox being a symlink is rejected.
        ensure_no_symlink(&self.root, &parent).await?;

        // The parent is guaranteed to exist by this point, so `canonicalize` can confirm it. The
        // destination itself may not exist yet, so it is checked separately.
        let real_parent =
            tokio::fs::canonicalize(&parent).await.map_err(|e| InboxError::Io(e.to_string()))?;
        if !real_parent.starts_with(&root) {
            return Err(InboxError::Escaped);
        }

        let path = real_parent.join(safe_name(proposed));
        // By this point `real_parent` has already been confirmed to be inside `root`, and
        // `safe_name` returns exactly one separator-free component, so there is no real way for
        // this check to ever trip. It is only a second line of defense — do not later mistake it
        // for the primary one, which is `ensure_no_symlink` above.
        if !path.starts_with(&root) {
            return Err(InboxError::Escaped);
        }
        // If the destination is already a link, writing to it writes wherever the link points.
        if let Ok(info) = tokio::fs::symlink_metadata(&path).await {
            if info.file_type().is_symlink() {
                return Err(InboxError::SymlinkInPath);
            }
        }
        Ok(path)
    }
}

/// Walks down from `root` to `path`, checking each component for a symlink.
async fn ensure_no_symlink(root: &Path, path: &Path) -> Result<(), InboxError> {
    let rest = path.strip_prefix(root).unwrap_or(path);
    let mut current = root.to_path_buf();
    for component in rest.components() {
        current.push(component);
        let info = tokio::fs::symlink_metadata(&current)
            .await
            .map_err(|e| InboxError::Io(e.to_string()))?;
        if info.file_type().is_symlink() {
            return Err(InboxError::SymlinkInPath);
        }
    }
    Ok(())
}
