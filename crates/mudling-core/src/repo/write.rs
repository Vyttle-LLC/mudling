//! Writes the repo's files: only those that changed, each in one step.

use super::Error;
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// A half-written file. A crash can leave one behind; the next export
/// sweeps it away.
pub(super) const TEMP_SUFFIX: &str = ".mudling-tmp";

/// Writes each file whose bytes differ from the disk, returning those paths.
pub(super) fn write_changed(
    root: &Path,
    files: &BTreeMap<PathBuf, Vec<u8>>,
) -> Result<Vec<PathBuf>, Error> {
    let mut written = Vec::new();
    for (relative, bytes) in files {
        let path = root.join(relative);
        if fs::read(&path).is_ok_and(|current| current == *bytes) {
            continue;
        }
        write_atomically(&path, bytes).map_err(|error| Error::io(&path, error))?;
        written.push(relative.clone());
    }
    Ok(written)
}

/// The file on disk is always the old one or the whole new one: the bytes
/// reach the disk under a temporary name, then a rename swaps them in.
fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut name = path.as_os_str().to_owned();
    name.push(TEMP_SUFFIX);
    let temporary = PathBuf::from(name);
    let mut file = fs::File::create(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)
}

/// Removes `.json` files in `folder` that no longer stand for anything: a
/// zone with no rooms left, a renamed character. The folder is MUDling's, so
/// a `.json` file someone else put there goes too; other kinds of file stay.
/// Returns the paths removed, relative to `root`.
pub(super) fn remove_stale(
    root: &Path,
    folder: &Path,
    files: &BTreeMap<PathBuf, Vec<u8>>,
) -> Result<Vec<PathBuf>, Error> {
    let mut removed = Vec::new();
    for path in list_files(&root.join(folder))? {
        let Some(name) = path.file_name() else {
            continue;
        };
        let relative = folder.join(name);
        if is_json(&path) && !files.contains_key(&relative) {
            fs::remove_file(&path).map_err(|error| Error::io(&path, error))?;
            removed.push(relative);
        }
    }
    Ok(removed)
}

/// Removes what a crash left mid-write in `folder`.
pub(super) fn sweep_temporaries(folder: &Path) -> Result<(), Error> {
    for path in list_files(folder)? {
        let is_temporary = path
            .to_str()
            .is_some_and(|path| path.ends_with(TEMP_SUFFIX));
        if is_temporary {
            fs::remove_file(&path).map_err(|error| Error::io(&path, error))?;
        }
    }
    Ok(())
}

/// The files in `folder`, sorted; none if it doesn't exist.
pub(super) fn list_files(folder: &Path) -> Result<Vec<PathBuf>, Error> {
    let entries = match fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(Error::io(folder, error)),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| Error::io(folder, error))?;
        let path = entry.path();
        if path.is_file() {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

pub(super) fn is_json(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "json")
}
