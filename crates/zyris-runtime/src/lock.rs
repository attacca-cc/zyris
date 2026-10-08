//! One Zyris per machine, whichever mode it is running in.
//!
//! Step 1 had single-instance protection in the GUI only, through a Tauri plugin, so `zyris` and
//! `zyris --headless` could run side by side. That was harmless while nothing was held
//! exclusively. It is not harmless now: there is one credential, and two processes enrolling and
//! storing one would leave the account with a credential nobody is using.

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

    /// Whether some other process holds this instance's lock right now.
    ///
    /// **Asked by taking it and giving it straight back**, which is the only way a file lock can
    /// be asked: `try_lock` never waits, so this cannot disturb the node that holds it, and a lock
    /// this call did win is released as the guard goes out of scope before it returns.
    ///
    /// It is the answer `zyris status`, `zyris up` and `zyris down` are built on, because it is
    /// the one signal that cannot be stale: the lock is held for exactly as long as the node is
    /// running and is released when the process ends for **any** reason, including being killed.
    /// A pid in a file is not, which is why the console reads one only to name a process to a
    /// person and never to decide anything with.
    ///
    /// `false` when the lock directory cannot even be reached: a machine where nothing can be
    /// locked is a machine where no node is running, and reporting an I/O error here would make
    /// `zyris status` fail instead of saying that. Note that this creates the (empty) lock file
    /// if it was not there, which is what the running side does on the way past too.
    pub fn is_held(name: &str) -> bool {
        InstanceLock::is_held_in(default_dir(), name).unwrap_or(false)
    }

    /// [`InstanceLock::is_held`], in a directory of the caller's choosing — so a test can use one
    /// of its own rather than the machine's real runtime directory.
    pub fn is_held_in(dir: PathBuf, name: &str) -> Result<bool, io::Error> {
        Ok(InstanceLock::acquire_in(dir, name)?.is_none())
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

        assert!(second.is_none(), "two instances would race over one credential");
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

    #[test]
    fn a_lock_nobody_holds_is_reported_as_not_held() {
        // What `zyris status` prints on a machine with nothing running, and what makes
        // `zyris up` willing to start something.
        let dir = tempfile::tempdir().unwrap();

        assert!(!InstanceLock::is_held_in(dir.path().to_path_buf(), "zyris").unwrap());
    }

    #[test]
    fn a_held_lock_is_reported_as_held_and_asking_does_not_take_it() {
        // **The asking must not steal the lock.** `is_held` wins the lock when it can and drops
        // it again in the same call, so a node that holds it is undisturbed — and a node that
        // does not must still be able to take it afterwards, which is the second assertion.
        let dir = tempfile::tempdir().unwrap();
        let held = InstanceLock::acquire_in(dir.path().to_path_buf(), "zyris").unwrap().unwrap();

        assert!(InstanceLock::is_held_in(dir.path().to_path_buf(), "zyris").unwrap());

        // Still the same lock, and still ours: dropping it is what lets the next caller in.
        assert!(InstanceLock::is_held_in(dir.path().to_path_buf(), "zyris").unwrap());
        drop(held);
        assert!(!InstanceLock::is_held_in(dir.path().to_path_buf(), "zyris").unwrap());
        assert!(
            InstanceLock::acquire_in(dir.path().to_path_buf(), "zyris").unwrap().is_some(),
            "asking about the lock left it taken, so a node started afterwards would be refused"
        );
    }
}
