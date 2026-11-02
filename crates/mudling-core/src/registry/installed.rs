//! The plugins installed in the data folder: `plugins/<id>/`, each with a
//! `plugins/<id>.json` beside it saying where it came from. The folder is a
//! plugin's files exactly as fetched, so its content hash still holds.

use super::{Fetched, InstallError, content_hash};
use crate::git::FolderFiles;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Under the data folder, and the repo's root.
pub const PLUGINS_FOLDER: &str = "plugins";
const RECORD_EXTENSION: &str = "json";
/// A folder being written, or one on its way out. A crash can leave either.
const WORKING_PREFIX: &str = ".";

/// Where an installed plugin came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "from", rename_all = "snake_case")]
pub enum Record {
    Registry {
        version: String,
        commit: String,
        hash: String,
    },
}

/// The plugins folder of a data folder.
#[derive(Clone, Debug)]
pub struct InstalledPlugins {
    folder: PathBuf,
}

impl InstalledPlugins {
    pub fn in_data_folder(data_folder: &Path) -> InstalledPlugins {
        InstalledPlugins {
            folder: data_folder.join(PLUGINS_FOLDER),
        }
    }

    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// The folder of each installed plugin. One a crash left half-written isn't
    /// one.
    pub fn plugin_folders(&self) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(&self.folder) else {
            return Vec::new();
        };
        let mut folders: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir() && !is_working(path))
            .collect();
        folders.sort();
        folders
    }

    /// Puts `fetched` in place of any plugin with its id. The files are
    /// written beside it first, so a crash leaves the old one or the new one.
    pub fn install(&self, fetched: &Fetched) -> Result<(), InstallError> {
        let id = &fetched.entry.id;
        if !is_safe_name(id) {
            return Err(InstallError::NotAPlugin(format!(
                "{id:?} isn't a folder name"
            )));
        }
        let disk = |error: io::Error| InstallError::Disk(error.to_string());
        let incoming = self.folder.join(format!("{WORKING_PREFIX}{id}.new"));
        let outgoing = self.folder.join(format!("{WORKING_PREFIX}{id}.old"));
        remove_folder(&incoming).map_err(disk)?;
        write_files(&incoming, &fetched.files)?;
        let record = Record::Registry {
            version: fetched.entry.version.clone(),
            commit: fetched.commit.to_string(),
            hash: fetched.entry.hash.clone(),
        };
        let target = self.folder.join(id);
        remove_folder(&outgoing).map_err(disk)?;
        if target.exists() {
            fs::rename(&target, &outgoing).map_err(disk)?;
        }
        fs::rename(&incoming, &target).map_err(disk)?;
        remove_folder(&outgoing).map_err(disk)?;
        let record = serde_json::to_vec_pretty(&record).expect("a record serializes");
        super::save(&self.record_path(id), &record).map_err(disk)
    }

    /// Takes a plugin and its record out.
    pub fn remove(&self, id: &str) -> io::Result<()> {
        if !is_safe_name(id) {
            return Ok(());
        }
        remove_folder(&self.folder.join(id))?;
        match fs::remove_file(self.record_path(id)) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// Where each plugin with a record came from, by id.
    pub fn records(&self) -> BTreeMap<String, Record> {
        let Ok(entries) = fs::read_dir(&self.folder) else {
            return BTreeMap::new();
        };
        entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let id = path.file_stem()?.to_str()?.to_owned();
                let is_record = path.extension()? == RECORD_EXTENSION && !is_working(&path);
                let record = serde_json::from_slice(&fs::read(&path).ok()?).ok()?;
                is_record.then_some((id, record))
            })
            .collect()
    }

    fn record_path(&self, id: &str) -> PathBuf {
        self.folder.join(format!("{id}.{RECORD_EXTENSION}"))
    }
}

/// A folder's content hash, as it is on the disk now.
pub fn folder_hash(folder: &Path) -> io::Result<String> {
    let mut files = FolderFiles::new();
    read_files(folder, "", &mut files)?;
    Ok(content_hash(&files))
}

fn read_files(folder: &Path, prefix: &str, files: &mut FolderFiles) -> io::Result<()> {
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = format!("{prefix}{name}");
        if entry.file_type()?.is_dir() {
            read_files(&entry.path(), &format!("{path}/"), files)?;
        } else {
            files.insert(path, fs::read(entry.path())?);
        }
    }
    Ok(())
}

/// Writes `files` under `folder`, refusing any name that could reach outside
/// it.
fn write_files(folder: &Path, files: &FolderFiles) -> Result<(), InstallError> {
    for (path, bytes) in files {
        if !path.split('/').all(is_safe_name) {
            return Err(InstallError::NotAPlugin(format!(
                "{path:?} isn't a safe file name"
            )));
        }
        let target = folder.join(path);
        let written = target
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| fs::write(&target, bytes));
        written.map_err(|error| InstallError::Disk(error.to_string()))?;
    }
    Ok(())
}

/// The names the registry allows: ASCII letters, digits, `.`, `_` and `-`.
fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// The installed plugin a path in a plugins folder belongs to: `<id>/…` is
/// its files, and `<id>.json` its record.
pub(crate) fn plugin_of(relative: &Path) -> Option<String> {
    let mut parts = relative.components();
    let first = parts.next()?.as_os_str().to_str()?;
    if first.starts_with(WORKING_PREFIX) {
        return None;
    }
    if parts.next().is_some() {
        return Some(first.to_owned());
    }
    first
        .strip_suffix(&format!(".{RECORD_EXTENSION}"))
        .map(str::to_owned)
}

fn is_working(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(WORKING_PREFIX))
}

fn remove_folder(folder: &Path) -> io::Result<()> {
    match fs::remove_dir_all(folder) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}
