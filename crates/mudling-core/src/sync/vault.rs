//! `secrets.vault` lives in the data folder until a repo is set, and then in
//! the repo. Moving it never leaves it nowhere: the repo's copy is written, then
//! committed, and only then does the local copy go. A vault saved to the old
//! place a moment later is simply taken in by the next sync.

use crate::repo;
use crate::vault::{self, FILE_NAME};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub(super) fn file_in(folder: &Path) -> PathBuf {
    folder.join(FILE_NAME)
}

/// Brings a local vault into the repo, merging it with one already there.
/// Returns the local bytes to compare against before removing them, or `None`
/// if there's no local vault.
pub(super) fn take_in(
    data_folder: &Path,
    files: &repo::Repo,
) -> Result<Option<Vec<u8>>, crate::merge::Error> {
    let local_path = file_in(data_folder);
    let Some(local) = repo::Repo::new(data_folder).read_if_present(Path::new(FILE_NAME))? else {
        return Ok(None);
    };
    let held = files.read_if_present(Path::new(FILE_NAME))?;
    let merged = match held {
        Some(held) if held != local => {
            let local_newer = saved_at(&local_path) > saved_at(&file_in(files.root()));
            Some(vault::merge_files(None, &held, &local, local_newer)?)
        }
        Some(_) => None,
        None => Some(local.clone()),
    };
    if let Some(merged) = merged {
        files.replace_files(&BTreeMap::from([(FILE_NAME.into(), merged)]), &[])?;
    }
    Ok(Some(local))
}

/// Removes the local vault, now that the repo's commit holds `taken`. A vault
/// that changed since stays for the next sync.
pub(super) fn drop_local(data_folder: &Path, taken: &[u8]) -> io::Result<()> {
    let path = file_in(data_folder);
    if fs::read(&path).is_ok_and(|bytes| bytes == taken) {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn saved_at(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|file| file.modified()).ok()
}

/// Copies the repo's vault to the data folder when the repo is cleared, merging
/// with a local one if a vault has been saved there since. The repo keeps its
/// own copy.
pub(super) fn bring_home(
    repo_folder: &Path,
    data_folder: &Path,
) -> Result<(), crate::merge::Error> {
    let Some(held) = repo::Repo::new(repo_folder).read_if_present(Path::new(FILE_NAME))? else {
        return Ok(());
    };
    let home = repo::Repo::new(data_folder);
    let local = home.read_if_present(Path::new(FILE_NAME))?;
    let bytes = match local {
        Some(local) if local != held => vault::merge_files(None, &local, &held, true)?,
        _ => held,
    };
    home.replace_files(&BTreeMap::from([(FILE_NAME.into(), bytes)]), &[])?;
    Ok(())
}
