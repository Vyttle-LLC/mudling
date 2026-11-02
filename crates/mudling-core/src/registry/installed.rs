//! The plugins installed in the data folder: `plugins/<id>/`, each with a
//! `plugins/<id>.json` beside it saying where it came from. The folder is a
//! plugin's files exactly as fetched, so its content hash still holds.

use super::{Fetched, InstallError, Origins, Trusted, content_hash};
use crate::git::FolderFiles;
use crate::plugin::Plugin;
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

/// An installed plugin that wouldn't load, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused {
    pub id: String,
    pub why: String,
}

/// A plugin in the data folder, as read once: everything about it is the
/// folder's, which needn't be named for the plugin's id.
#[derive(Clone, Debug)]
pub struct FolderCopy {
    pub plugin: Plugin,
    /// Its folder's name under `plugins/`.
    pub folder: String,
    /// The folder's content hash as this device computed it.
    pub hash: String,
    /// A record beside it says it came from the directory.
    pub from_directory: bool,
}

/// What the data folder's plugins came to.
#[derive(Clone, Debug, Default)]
pub struct Loaded {
    pub plugins: Vec<FolderCopy>,
    /// Copies this device doesn't trust: read to show them, never run.
    pub arrived: Vec<FolderCopy>,
    pub refused: Vec<Refused>,
}

/// Why a copy wasn't trusted.
const CHANGED_SINCE_SHOWN: &str = "it changed since you reviewed it, so it wasn't trusted";

/// The plugins folder of a data folder.
#[derive(Clone, Debug)]
pub struct InstalledPlugins {
    folder: PathBuf,
    trusted: Trusted,
    origins: Origins,
}

impl InstalledPlugins {
    pub fn in_data_folder(data_folder: &Path) -> InstalledPlugins {
        InstalledPlugins {
            folder: data_folder.join(PLUGINS_FOLDER),
            trusted: Trusted::in_data_folder(data_folder),
            origins: Origins::in_data_folder(data_folder),
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

    /// Puts `fetched` in place of any plugin with its id, and trusts that
    /// copy here. The files are written beside it first, so a crash leaves
    /// the old one or the new one.
    pub fn install(&self, fetched: &Fetched) -> Result<(), InstallError> {
        let id = &fetched.entry.id;
        if !is_safe_name(id) {
            return Err(InstallError::NotAPlugin(format!(
                "{id:?} isn't a folder name"
            )));
        }
        let disk = |error: io::Error| InstallError::Disk(error.to_string());
        self.trusted.trust(id, &fetched.entry.hash).map_err(disk)?;
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

    /// Loads every installed plugin this device trusts: a copy whose content
    /// hash it trusts, or a folder with no record that began here. The
    /// record beside a plugin travels in the same repo as its files, so it
    /// vouches for nothing. Anything else is read to be shown, and waits for
    /// the player.
    pub fn load(&self) -> Loaded {
        let mut loaded = Loaded::default();
        for path in self.plugin_folders() {
            let folder = path
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
            let read = folder_hash(&path)
                .map_err(|error| error.to_string())
                .and_then(|hash| {
                    let plugin = Plugin::load(&path).map_err(|error| error.to_string())?;
                    Ok((plugin, hash))
                });
            let (plugin, hash) = match read {
                Ok(read) => read,
                Err(why) => {
                    loaded.refused.push(Refused { id: folder, why });
                    continue;
                }
            };
            let from_directory = self.record_path(&folder).exists();
            let copy = FolderCopy {
                plugin,
                folder,
                hash,
                from_directory,
            };
            if self.is_trusted(&copy) {
                loaded.plugins.push(copy);
            } else {
                loaded.arrived.push(copy);
            }
        }
        loaded
    }

    /// Whether this device trusts `copy`. A folder whose origin can't be
    /// read or saved isn't the player's own.
    fn is_trusted(&self, copy: &FolderCopy) -> bool {
        self.trusted.trusts(&copy.folder, &copy.hash)
            || (!copy.from_directory && self.origins.is_local(&copy.folder).unwrap_or(false))
    }

    /// Trusts the copy in `folder`, as long as it still hashes to `hash`, the
    /// copy the player was shown. One with no record becomes the player's own.
    pub fn trust(&self, folder: &str, hash: &str) -> Result<(), String> {
        let path = self.folder.join(folder);
        if !is_safe_name(folder) || folder_hash(&path).ok().as_deref() != Some(hash) {
            return Err(CHANGED_SINCE_SHOWN.to_owned());
        }
        let could_not = |error: io::Error| error.to_string();
        self.trusted.trust(folder, hash).map_err(could_not)?;
        if !self.record_path(folder).exists() {
            self.origins.mark_local(folder).map_err(could_not)?;
        }
        Ok(())
    }

    /// Takes the plugin in `folder` and its record out.
    pub fn remove(&self, folder: &str) -> io::Result<()> {
        if !is_safe_name(folder) {
            return Ok(());
        }
        remove_folder(&self.folder.join(folder))?;
        match fs::remove_file(self.record_path(folder)) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        self.origins.forget(folder)
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
