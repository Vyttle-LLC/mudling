//! Who may touch a repo, and clearing the locks a killed app leaves behind.
//!
//! git's `.lock` files are empty, so they can't say who holds them. Two checks
//! stand in for that. A running MUDling holds an OS lock on [`OWNER_FILE`],
//! which the OS drops the moment the process dies, so a free owner file means
//! no MUDling is mid-write. Any other git process (say, the player's terminal)
//! holds its locks for moments, so a lock younger than [`STALE_AFTER`] is left
//! alone. Clearing runs at open and again before each write, so a lock that was
//! too young at a quick relaunch goes once it has aged.

use super::{GitError, Result};
use std::fs::{self, File, TryLockError};
use std::path::Path;
use std::time::{Duration, SystemTime};

const OWNER_FILE: &str = "mudling-owner";
const STALE_AFTER: Duration = Duration::from_secs(60);

/// Takes the repo for this process until the returned file is dropped.
pub(super) fn take_ownership(git_dir: &Path) -> Result<File> {
    // Not truncated: Windows refuses that on a file another process has locked,
    // and the lock is what should answer.
    let owner = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(git_dir.join(OWNER_FILE))?;
    match owner.try_lock() {
        Ok(()) => Ok(owner),
        Err(TryLockError::WouldBlock) => Err(GitError::Locked(
            "another MUDling has this repo open".into(),
        )),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

/// Removes `.lock` files old enough to be stale. One that can't be removed is
/// still held, which on Windows is how a live holder shows; the operation that
/// needs it then fails as [`GitError::Locked`].
pub(super) fn clear_stale(git_dir: &Path) {
    let Ok(entries) = fs::read_dir(git_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Object files are immutable and take no locks, and there are many.
            if path.file_name().is_none_or(|name| name != "objects") {
                clear_stale(&path);
            }
        } else if path.extension().is_some_and(|ext| ext == "lock") && is_stale(&path) {
            let _ = fs::remove_file(path);
        }
    }
}

fn is_stale(lock: &Path) -> bool {
    let age = fs::metadata(lock)
        .and_then(|metadata| metadata.modified())
        .map(|modified| SystemTime::now().duration_since(modified));
    matches!(age, Ok(Ok(age)) if age >= STALE_AFTER)
}
