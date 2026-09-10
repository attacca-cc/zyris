//! One Zyris per machine, whichever mode it is running in.
//!
//! Step 1 had single-instance protection in the GUI only, through a Tauri plugin, so `zyris` and
//! `zyris --headless` could run side by side. That was harmless while nothing was held
//! exclusively. It is not harmless now: there is one node token, and two processes minting and
//! storing one would leave the account with a node nobody is using.

use std::fs::File;
use std::io;
use std::path::PathBuf;

use fs4::{FileExt, TryLockError};

/// Held for as long as this process should be the only Zyris. Dropping it releases the lock, and
/// so does the process ending for any reason — including being killed, which is the reason this
/// is a file lock rather than a PID file.
pub struct InstanceLock {
    /// Never read. The lock lives as long as this handle does.
    _file: File,
}

impl InstanceLock {
    pub fn acquire(name: &str) -> Result<Option<InstanceLock>, io::Error> {
        InstanceLock::acquire_in(default_dir(), name)
    }

    /// `Ok(None)` is the ordinary "someone else is already running" answer, not an error — the
    /// caller's response to it is to exit quietly, and making that an `Err` would put a normal
    /// outcome on the failure path.
    pub fn acquire_in(dir: PathBuf, name: &str) -> Result<Option<InstanceLock>, io::Error> {
        std::fs::create_dir_all(&dir)?;
        let file = File::create(dir.join(format!("{name}.lock")))?;
        // `File` has an inherent `try_lock` (stable since Rust 1.89) that would shadow the
        // trait method of the same name, so this calls the `fs4::FileExt` one explicitly.
        match FileExt::try_lock(&file) {
            Ok(()) => Ok(Some(InstanceLock { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(error),
        }
    }
}

fn default_dir() -> PathBuf {
    directories::ProjectDirs::from("cc", "attacca", "zyris")
        .map(|dirs| dirs.runtime_dir().unwrap_or_else(|| dirs.cache_dir()).to_path_buf())
        .unwrap_or_else(std::env::temp_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_caller_gets_the_lock() {
        let dir = tempfile::tempdir().unwrap();

        let lock = InstanceLock::acquire_in(dir.path().to_path_buf(), "zyris").unwrap();

        assert!(lock.is_some());
    }

    #[test]
    fn a_second_caller_is_refused_while_the_first_holds_it() {
        let dir = tempfile::tempdir().unwrap();
        let _first = InstanceLock::acquire_in(dir.path().to_path_buf(), "zyris").unwrap().unwrap();

        let second = InstanceLock::acquire_in(dir.path().to_path_buf(), "zyris").unwrap();

        assert!(second.is_none(), "two instances would race over one node token");
    }

    #[test]
    fn dropping_the_lock_lets_the_next_caller_in() {
        let dir = tempfile::tempdir().unwrap();
        let first = InstanceLock::acquire_in(dir.path().to_path_buf(), "zyris").unwrap().unwrap();

        drop(first);

        assert!(InstanceLock::acquire_in(dir.path().to_path_buf(), "zyris").unwrap().is_some());
    }

    #[test]
    fn different_names_do_not_block_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let _a = InstanceLock::acquire_in(dir.path().to_path_buf(), "zyris").unwrap().unwrap();

        let b = InstanceLock::acquire_in(dir.path().to_path_buf(), "something-else").unwrap();

        assert!(b.is_some());
    }
}
