//! Moves the original aside before it gets overwritten.
//!
//! A **move**, not a copy, so disk usage does not double. And **a failed stash does not stop the
//! transfer** — zyris-code's `code_edit` follows the same rule. Blocking work because the safety
//! net is missing produces states nobody can repair.
//!
//! What must never happen is "the stash failed" turning into "an earlier backup was quietly
//! deleted". So slot names carry an in-process sequence number as well as the millisecond (two
//! backups of the same name in the same millisecond must not collide), a symlink is not
//! copy-fallbacked when rename does not work (that would leak the contents of whatever it points
//! at), and the case where the copy succeeded but only unlinking the original failed returns the
//! backup path rather than `None` (otherwise a backup that exists is lost to the caller).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub struct UndoStore {
    root: PathBuf,
}

/// A starting sequence number, appended so that a different victim landing under the same name
/// in the same millisecond does not collide with an existing slot. **What actually guarantees
/// uniqueness is not this counter but `stash`'s `create_dir` retry** — the counter is only a hint
/// that saves the waste of searching from 0 every time. Within one process it is safe even when a
/// colliding candidate gets picked, because the retry moves on to the next sequence number; it is
/// safe across several processes too, even though each starts its own counter at 0 and so the
/// values can coincide, because the filesystem's `mkdir` has the final say.
static NEXT_SEQ: AtomicU64 = AtomicU64::new(0);

/// The most times a single `stash` call retries to find a slot. Giving up beyond this means
/// something else is wrong (a stopped clock, say) — piling up this much collision within one
/// millisecond and one sequence-number range is not supposed to happen on its own.
const MAX_ATTEMPTS: u32 = 64;

impl UndoStore {
    pub fn new(root: impl Into<PathBuf>) -> UndoStore {
        UndoStore { root: root.into() }
    }

    /// Moves the original into a stash slot and returns where it went.
    ///
    /// `None` when there is nothing to move (no such file), and `None` when the slot could not be
    /// created or nothing could be moved into it. **One exception**: rename did not work, the
    /// copy fallback succeeded, and only unlinking the original failed — the backup is complete
    /// at that point, so its path is returned. `None` means "there is no backup at all", never
    /// "the original is safe".
    pub async fn stash(&self, victim: &Path, now_ms: u64) -> Option<PathBuf> {
        let meta = tokio::fs::symlink_metadata(victim).await.ok()?;
        let name = victim.file_name()?;

        let slot = self.claim_slot(now_ms).await?;
        let dest = slot.join(name);

        // On the same filesystem, rename is cheap, and it moves a symlink as-is, pointer and all.
        if tokio::fs::rename(victim, &dest).await.is_ok() {
            return Some(dest);
        }

        // rename did not work (a different filesystem, or a permissions problem). Moving a
        // symlink with copy does not copy the link — it copies the *contents* of whatever it
        // points at, exposing in the stash slot exactly what the original was hiding. For the same
        // reason inbox.rs rejects symlinks outright, this gives up here instead of taking the copy
        // fallback.
        if meta.file_type().is_symlink() {
            return None;
        }

        tokio::fs::copy(victim, &dest).await.ok()?;
        // The backup is already complete by this point. A failure to delete the original does not
        // hide that result — quietly returning `None` here would mean the caller never learns
        // about a backup that does exist.
        let _ = tokio::fs::remove_file(victim).await;
        Some(dest)
    }

    /// Creates one new `{now_ms}-{seq}` slot and returns its path.
    ///
    /// **Using `create_dir` instead of `create_dir_all` is the whole point of this function.**
    /// `create_dir_all` succeeds quietly even when the slot already exists — so the old
    /// implementation would just walk straight through a slot another process had already
    /// created, and the `rename` that followed would silently overwrite what was in it (since the
    /// counter is only unique within one process, another process's counter also starts at 0, and
    /// the same millisecond and sequence number commonly collide). `create_dir` fails with
    /// `AlreadyExists` when the slot is already there — that failure is the signal to move on to
    /// the next sequence number. Because the filesystem's `mkdir` is atomic, no matter how many
    /// processes are racing, exactly one of them ends up owning a given slot.
    async fn claim_slot(&self, now_ms: u64) -> Option<PathBuf> {
        // The root itself is shared by every slot, so creating it every time (succeeding if it
        // already exists) is safe — the only place contention happens is the `{now_ms}-{seq}`
        // leaf underneath it.
        tokio::fs::create_dir_all(&self.root).await.ok()?;

        for _ in 0..MAX_ATTEMPTS {
            let seq = NEXT_SEQ.fetch_add(1, Ordering::Relaxed);
            let candidate = self.root.join(format!("{now_ms}-{seq}"));
            match tokio::fs::create_dir(&candidate).await {
                Ok(()) => return Some(candidate),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return None,
            }
        }
        None
    }
}
