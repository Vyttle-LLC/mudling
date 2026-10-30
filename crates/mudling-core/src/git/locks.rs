//! Who may touch a repo, and clearing the locks a killed app leaves behind.
//!
//! git's `.lock` files don't say who holds them, so two markers of MUDling's
//! own answer that. A running MUDling holds an OS lock on [`OWNER_FILE`], which
//! the OS drops the moment the process dies. [`WRITE_MARKER`] exists only while
//! MUDling writes. So at open, a free owner file with a marker still there means
//! a MUDling died mid-write, and the locks made since the marker are taken as
//! its own. Every other lock belongs to some other git, however old, and stays:
//! the operation that needs it fails as [`GitError::Locked`] until that git is
//! done.
//!
//! One gap remains, because a marker has no end time and git's locks name no
//! holder. After a kill, a lock taken by a live git before the next launch
//! counts as MUDling's and is cleared, which fails that git.

use super::{GitError, Result};
use std::fs::{self, File, TryLockError};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const OWNER_FILE: &str = "mudling-owner";
const WRITE_MARKER: &str = "mudling-writing";

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

/// Says MUDling is writing to the repo until dropped. Hold it around anything
/// that takes git locks. Only the repo's owner may set it.
pub(super) struct WriteMarker(PathBuf);

impl WriteMarker {
    pub(super) fn set(git_dir: &Path) -> Result<Self> {
        let path = git_dir.join(WRITE_MARKER);
        File::create(&path)?;
        Ok(Self(path))
    }
}

impl Drop for WriteMarker {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Clears what a MUDling killed mid-write left. Only the repo's owner may call
/// it, since a marker is live for as long as some MUDling owns the repo.
pub(super) fn clear_after_crash(git_dir: &Path) {
    let marker = git_dir.join(WRITE_MARKER);
    let Some(write_began) = modified(&marker) else {
        return;
    };
    remove_locks_since(git_dir, write_began);
    let _ = fs::remove_file(marker);
}

/// Removes `.lock` files made at or after `since`. One that can't be removed
/// is still held, which on Windows is how a live holder shows.
fn remove_locks_since(dir: &Path, since: SystemTime) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Object files are immutable and take no locks, and there are many.
            if path.file_name().is_none_or(|name| name != "objects") {
                remove_locks_since(&path, since);
            }
        } else if path.extension().is_some_and(|ext| ext == "lock")
            && modified(&path).is_some_and(|made| made >= since)
        {
            let _ = fs::remove_file(path);
        }
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|file| file.modified()).ok()
}
