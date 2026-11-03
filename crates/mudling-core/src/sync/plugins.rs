//! Installed plugins in the repo (PRD §11): its `plugins/` folder holds what
//! the data folder's does, so a plugin installed on one device reaches the
//! others. Approvals and trust aren't in there, so they stay put, and a
//! plugin this writes into the data folder is marked as arrived: it loads
//! only once this device trusts its copy.
//!
//! The data folder's copy leads while it has seen the branch: export makes the
//! repo's match it, and a merge's import makes it match the repo's. Until then
//! (a new repo, a kill between a merge and its import, a branch moved
//! outside MUDling) a join comes first: the repo's plugins win, and those only
//! this device has stay, so an export deletes nothing it hasn't read.

use crate::git::Oid;
use crate::registry::{Origins, PLUGINS_FOLDER, is_working, plugin_of};
use crate::repo::{TEMP_SUFFIX, write_atomically};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The commit the data folder's plugins last matched. Local to the device.
const SEEN_FILE: &str = "plugins.seen";

type Files = BTreeMap<PathBuf, Vec<u8>>;

pub(super) fn has_seen(data_folder: &Path, tip: Option<Oid>) -> bool {
    let seen = fs::read_to_string(data_folder.join(SEEN_FILE)).ok();
    seen.as_deref().map(str::trim) == tip.map(|tip| tip.to_string()).as_deref()
}

pub(super) fn mark_seen(data_folder: &Path, tip: Oid) -> io::Result<()> {
    fs::create_dir_all(data_folder)?;
    fs::write(data_folder.join(SEEN_FILE), tip.to_string())
}

/// Makes the repo's plugins match the data folder's.
pub(super) fn export(data_folder: &Path, repo: &Path) -> io::Result<()> {
    let ours = files_in(&data_folder.join(PLUGINS_FOLDER))?;
    replace(&repo.join(PLUGINS_FOLDER), &ours)
}

/// Makes the data folder's plugins match the repo's.
pub(super) fn import(data_folder: &Path, repo: &Path) -> io::Result<()> {
    let theirs = files_in(&repo.join(PLUGINS_FOLDER))?;
    take_in(data_folder, &theirs)
}

/// Takes in the repo's plugins, keeping those only this device has.
pub(super) fn join(data_folder: &Path, repo: &Path) -> io::Result<()> {
    let theirs = files_in(&repo.join(PLUGINS_FOLDER))?;
    let mut joined = files_in(&data_folder.join(PLUGINS_FOLDER))?;
    let replaced: BTreeSet<String> = theirs.keys().filter_map(|path| plugin_of(path)).collect();
    joined.retain(|path, _| plugin_of(path).is_none_or(|plugin| !replaced.contains(&plugin)));
    joined.extend(theirs);
    take_in(data_folder, &joined)
}

/// Leaves the data folder's plugins holding `files`. Each plugin this
/// writes is marked as arrived first, so a folder that came from the repo
/// is never taken for one the player made here.
fn take_in(data_folder: &Path, files: &Files) -> io::Result<()> {
    let folder = data_folder.join(PLUGINS_FOLDER);
    let current = files_in(&folder)?;
    let arriving: BTreeSet<String> = (files.iter())
        .filter(|(path, bytes)| current.get(*path) != Some(*bytes))
        .filter_map(|(path, _)| plugin_of(path))
        .collect();
    Origins::in_data_folder(data_folder).mark_arrived(arriving.iter().map(String::as_str))?;
    replace(&folder, files)
}

/// Every file under `folder`, relative to it, but for a plugin being written.
fn files_in(folder: &Path) -> io::Result<Files> {
    let mut files = Files::new();
    let entries = match fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(files),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        if is_working(&entry.path()) {
            continue;
        }
        let name = PathBuf::from(entry.file_name());
        collect(&entry.path(), &name, &mut files)?;
    }
    Ok(files)
}

fn collect(path: &Path, relative: &Path, files: &mut Files) -> io::Result<()> {
    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            collect(&entry.path(), &relative.join(entry.file_name()), files)?;
        }
    } else if !relative.to_string_lossy().ends_with(TEMP_SUFFIX) {
        files.insert(relative.to_owned(), fs::read(path)?);
    }
    Ok(())
}

/// Leaves `folder` holding exactly `files`, writing only what differs, each
/// file whole. A plugin being written, which starts with `.`, is left alone.
fn replace(folder: &Path, files: &Files) -> io::Result<()> {
    let current = files_in(folder)?;
    for (relative, bytes) in files {
        if current.get(relative) == Some(bytes) {
            continue;
        }
        write_atomically(&folder.join(relative), bytes)?;
    }
    for relative in current.keys().filter(|path| !files.contains_key(*path)) {
        fs::remove_file(folder.join(relative))?;
        remove_empty_parents(folder, relative)?;
    }
    Ok(())
}

fn remove_empty_parents(folder: &Path, relative: &Path) -> io::Result<()> {
    for parent in relative.ancestors().skip(1) {
        if parent.as_os_str().is_empty() {
            break;
        }
        let path = folder.join(parent);
        if fs::read_dir(&path)?.next().is_some() {
            break;
        }
        fs::remove_dir(&path)?;
    }
    Ok(())
}
