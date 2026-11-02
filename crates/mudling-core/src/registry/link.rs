//! A plugin added from a git link: the repo's root at its default branch's
//! tip, read from git's objects like a directory plugin, and checked as
//! registry CI checks one before anything reaches the disk. Installed, it's
//! one of the data folder's plugins like any other, so it syncs, and another
//! device holds it back until the player there trusts it.

use super::{Fetched, InstallError, REGISTRY_FOLDER, Source, content_hash, hex, load_files};
use crate::git::{GitError, Limit, Mirror};
use crate::plugin::check;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Under the registry's folder: a bare mirror for each link.
const LINKS_FOLDER: &str = "links";
/// Enough of a link's hash to name its mirror.
const NAME_LENGTH: usize = 16;
/// The most a link's fetch may download. A plugin is a few small text
/// files; its folder is held to [`crate::git::FOLDER_LIMITS`] too.
const MOST_DOWNLOAD: u64 = 16 * 1024 * 1024;

/// Fetches the plugin `url` points to and checks it: its manifest and
/// patterns load, its own tests replay to what they expect, its hooks don't
/// fail, and it keeps within its budget a line. Its tests' hooks run in the
/// same sandbox as `mudling plugin test`'s, sending nothing anywhere. Takes
/// seconds: run it off the UI thread.
pub fn fetch_link(data_folder: &Path, url: &str, limit: &Limit) -> Result<Fetched, InstallError> {
    let url = url.trim();
    if url.is_empty() {
        return Err(InstallError::Unreachable(GitError::Failed(
            "there's no link".to_owned(),
        )));
    }
    let mirror = Mirror::open(&mirror_folder(data_folder, url), url)?.capped(MOST_DOWNLOAD);
    let commit = mirror.fetch_default_branch(limit)?;
    let files = mirror.folder_files(commit, "")?;
    let plugin = load_files(&files)?;
    check::own_tests(&plugin, &files).map_err(InstallError::Failed)?;
    Ok(Fetched {
        source: Source::Link(url.to_owned()),
        commit,
        hash: content_hash(&files),
        files,
        plugin,
    })
}

fn mirror_folder(data_folder: &Path, url: &str) -> PathBuf {
    let name = hex(&Sha256::digest(url.as_bytes()));
    let folder = data_folder.join(REGISTRY_FOLDER).join(LINKS_FOLDER);
    folder.join(format!("{}.git", &name[..NAME_LENGTH]))
}
