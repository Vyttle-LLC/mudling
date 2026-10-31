//! The repo's text format: a player's data as one file per thing (PRD §10).
//!
//! SQLite stays the working store, and the repo is the synced copy. Export
//! renders every file in memory and writes only those that differ from the
//! disk, so a change to one room rewrites one file. Import does the reverse:
//! it renders the database, and parses only the files that differ from that.
//! No state is kept between runs, so a killed app has nothing to repair.
//!
//! Calls block on the disk and the database, so they belong on the sync
//! thread. They read through a [`Reader`], which never waits on the game's
//! [`Writer`](crate::persist::Writer), and hand imports back as a [`Batch`]
//! for that writer to commit.
//!
//! The order matters. Export first, so the repo holds every local change;
//! then, after a pull has merged the repo's files, import. Export deletes
//! zone and character files with no records behind them, so a file pulled but
//! not yet imported would be lost. Import never overrules what's newer: the
//! writer checks each record against the row and the tombstones it finds when
//! it commits, because the game can write between export and import.
//! [`merge`](crate::merge) runs this order, and chooses between two sides'
//! files before import sees them.
//!
//! ```no_run
//! use mudling_core::persist::Database;
//! use mudling_core::repo::{AppSettings, Repo};
//! # use std::path::Path;
//! # fn demo(folder: &Path) -> Result<(), mudling_core::repo::Error> {
//! let repo = Repo::new(folder);
//! let database = Database::open(folder, "mud.example.org")?;
//! repo.export(&AppSettings::default(), &[database.reader()?])?;
//! // A pull merges the repo's files here.
//! let imported = repo.import("mud.example.org", &database.reader()?)?;
//! database.writer()?.submit(imported.batch)?;
//! # Ok(())
//! # }
//! ```

pub(crate) mod format;
pub(crate) mod names;
mod write;

pub use format::AppSettings;

use crate::persist::{self, Batch, Reader};
use format::{
    ATTRIBUTES, CHARACTERS_FOLDER, FORMAT_VERSION, GEAR_FOLDER, GITATTRIBUTES, GITIGNORE, IGNORED,
    MANIFEST, MAP_FOLDER, Manifest, SERVERS, SETTINGS, ServerList, ServerRows, check_manifest,
    is_record_file, json, parse, with_lines,
};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// A folder holding a player's data in the text format.
#[derive(Clone, Debug)]
pub struct Repo {
    root: PathBuf,
}

/// What an export did. Paths are relative to the repo's root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exported {
    pub written: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
    pub unchanged: usize,
}

/// What an import found: the records of the files that differ from the
/// database, ready for the writer. Paths are relative to the repo's root.
#[derive(Clone, Debug, PartialEq)]
pub struct Imported {
    pub batch: Batch,
    pub read: Vec<PathBuf>,
}

impl Repo {
    pub fn new(root: impl Into<PathBuf>) -> Repo {
        Repo { root: root.into() }
    }

    /// Brings the repo's files up to date with `readers`, one per server's
    /// database. Servers the repo lists but `readers` doesn't hold stay as
    /// they are. The `map` and `characters` folders of an exported server are
    /// MUDling's: any `.json` file in them with no records behind it is
    /// deleted, whoever wrote it. `.gitignore` and `.gitattributes` gain any
    /// line MUDling needs and keep the rest. Everything else in the repo is
    /// left alone. A repo in a newer format is refused untouched.
    pub fn export(&self, settings: &AppSettings, readers: &[Reader]) -> Result<Exported, Error> {
        let mut list = self.server_list()?;
        let mut exported = BTreeMap::new();
        for reader in readers {
            let rows = ServerRows::load(reader)?;
            let [server] = rows.servers.as_slice() else {
                return Err(Error::NotOneServer(rows.servers.len()));
            };
            let id = server.id.clone();
            let entry = rows.entry(server);
            list.servers.retain(|listed| listed.server.id != id);
            list.servers.push(entry);
            exported.insert(id, rows);
        }
        list.sort();

        let mut files = BTreeMap::new();
        let manifest = Manifest {
            format: FORMAT_VERSION,
        };
        files.insert(PathBuf::from(MANIFEST), json(&manifest));
        files.insert(PathBuf::from(SETTINGS), json(settings));
        files.insert(PathBuf::from(SERVERS), json(&list));
        for (name, lines) in [
            (GITIGNORE, IGNORED.as_slice()),
            (GITATTRIBUTES, ATTRIBUTES.as_slice()),
        ] {
            let current = self.read_if_present(Path::new(name))?;
            files.insert(PathBuf::from(name), with_lines(current.as_deref(), lines));
        }

        let mut server_folders = Vec::with_capacity(exported.len());
        for (entry, folder) in list.with_folders() {
            if let Some(rows) = exported.get(&entry.server.id) {
                for (path, bytes) in rows.files() {
                    files.insert(folder.join(path), bytes);
                }
                server_folders.push(folder);
            }
        }

        let written = write::write_changed(&self.root, &files)?;
        let mut removed = Vec::new();
        write::sweep_temporaries(&self.root)?;
        for folder in server_folders {
            write::sweep_temporaries(&self.root.join(&folder))?;
            write::sweep_temporaries(&self.root.join(&folder).join(GEAR_FOLDER))?;
            for inner in [MAP_FOLDER, CHARACTERS_FOLDER] {
                let inner = folder.join(inner);
                write::sweep_temporaries(&self.root.join(&inner))?;
                removed.extend(write::remove_stale(&self.root, &inner, &files)?);
            }
        }
        let unchanged = files.len() - written.len();
        Ok(Exported {
            written,
            removed,
            unchanged,
        })
    }

    /// The servers the repo lists, each to be imported with [`Repo::import`].
    pub fn servers(&self) -> Result<Vec<persist::Server>, Error> {
        Ok(self
            .server_list()?
            .servers
            .into_iter()
            .map(|entry| entry.server)
            .collect())
    }

    /// The settings the repo holds, or the defaults if it holds none yet.
    pub fn settings(&self) -> Result<AppSettings, Error> {
        self.check_format()?;
        match self.read_if_present(Path::new(SETTINGS))? {
            Some(bytes) => parse(Path::new(SETTINGS), &bytes),
            None => Ok(AppSettings::default()),
        }
    }

    /// Reads back what differs from the database behind `reader`: only
    /// files that don't match what it would export are parsed. Submit the
    /// batch to the database's writer, which keeps any row newer than the
    /// file's copy and applies the file's deletes. A fresh database gets
    /// everything.
    pub fn import(&self, server_id: &str, reader: &Reader) -> Result<Imported, Error> {
        let list = self.server_list()?;
        let (listed, folder) = list
            .with_folders()
            .find(|(entry, _)| entry.server.id == server_id)
            .ok_or_else(|| Error::UnknownServer(server_id.to_owned()))?;

        let rows = ServerRows::load(reader)?;
        let local = rows.files();
        let mut arrived = ServerRows::default();
        let mut read = Vec::new();
        if local_entry(&rows, server_id).as_ref() != Some(listed) {
            arrived.add_entry(listed);
            read.push(PathBuf::from(SERVERS));
        }
        for relative in self.server_files(&folder)? {
            let path = self.root.join(&folder).join(&relative);
            let bytes = fs::read(&path).map_err(|error| Error::io(&path, error))?;
            if local.get(&relative) != Some(&bytes) {
                arrived.read_file(&relative, &bytes, listed.notes_updated)?;
                read.push(folder.join(relative));
            }
        }
        Ok(Imported {
            batch: arrived.into_batch(),
            read,
        })
    }

    /// The records the folder holds for a server, or `None` if it isn't listed.
    pub(crate) fn server_rows(&self, server_id: &str) -> Result<Option<ServerRows>, Error> {
        let list = self.server_list()?;
        let Some((listed, folder)) = list
            .with_folders()
            .find(|(entry, _)| entry.server.id == server_id)
        else {
            return Ok(None);
        };
        let mut files = Vec::new();
        for relative in self.server_files(&folder)? {
            let path = self.root.join(&folder).join(&relative);
            let bytes = fs::read(&path).map_err(|error| Error::io(&path, error))?;
            files.push((relative, bytes));
        }
        let held = files
            .iter()
            .map(|(path, bytes)| (path.as_path(), &bytes[..]));
        ServerRows::parse(listed, held).map(Some)
    }

    /// Writes `files` to the folder and removes `gone`, for a merge. Paths
    /// are relative to the repo's root.
    pub(crate) fn replace_files(
        &self,
        files: &BTreeMap<PathBuf, Vec<u8>>,
        gone: &[PathBuf],
    ) -> Result<(), Error> {
        write::write_changed(&self.root, files)?;
        for relative in gone {
            let path = self.root.join(relative);
            match fs::remove_file(&path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => {
                    return Err(Error::io(&path, error));
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Refuses a repo a newer MUDling wrote. A repo with no manifest yet has
    /// nothing to protect.
    fn check_format(&self) -> Result<(), Error> {
        let manifest = self.read_if_present(Path::new(MANIFEST))?;
        check_manifest(manifest.as_deref())
    }

    fn server_list(&self) -> Result<ServerList, Error> {
        self.check_format()?;
        let Some(bytes) = self.read_if_present(Path::new(SERVERS))? else {
            return Ok(ServerList::default());
        };
        let mut list: ServerList = parse(Path::new(SERVERS), &bytes)?;
        list.sort();
        Ok(list)
    }

    /// Every file of a server, relative to its folder.
    fn server_files(&self, folder: &Path) -> Result<Vec<PathBuf>, Error> {
        let folder = self.root.join(folder);
        let mut files = Vec::new();
        for inner in ["", GEAR_FOLDER, MAP_FOLDER, CHARACTERS_FOLDER].map(Path::new) {
            for path in write::list_files(&folder.join(inner))? {
                let Some(name) = path.file_name() else {
                    continue;
                };
                let relative = inner.join(name);
                if is_record_file(&relative) {
                    files.push(relative);
                }
            }
        }
        Ok(files)
    }

    pub(crate) fn read_if_present(&self, relative: &Path) -> Result<Option<Vec<u8>>, Error> {
        let path = self.root.join(relative);
        match fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(Error::io(&path, error)),
        }
    }
}

fn local_entry(rows: &ServerRows, server_id: &str) -> Option<format::ServerEntry> {
    let server = rows.servers.iter().find(|server| server.id == server_id)?;
    Some(rows.entry(server))
}

#[derive(Debug)]
pub enum Error {
    Persist(persist::Error),
    Io {
        path: PathBuf,
        source: io::Error,
    },
    /// A file isn't what the format says it should be.
    Format {
        path: PathBuf,
        detail: String,
    },
    /// A newer MUDling wrote the repo, in a format this one can't read.
    NewerFormat(u32),
    /// A database must hold exactly one server's row, which names its folder.
    NotOneServer(usize),
    /// `servers.json` doesn't list the server.
    UnknownServer(String),
}

impl Error {
    fn io(path: &Path, source: io::Error) -> Error {
        Error::Io {
            path: path.to_owned(),
            source,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Persist(error) => write!(f, "{error}"),
            Error::Io { path, source } => {
                write!(f, "couldn't reach {}: {source}", path.display())
            }
            Error::Format { path, detail } => {
                write!(f, "{} can't be read: {detail}", path.display())
            }
            Error::NewerFormat(format) => {
                write!(f, "the repo is in format {format}, newer than this MUDling")
            }
            Error::NotOneServer(found) => {
                write!(f, "a database should hold one server, not {found}")
            }
            Error::UnknownServer(id) => write!(f, "the repo doesn't list {id:?}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<persist::Error> for Error {
    fn from(error: persist::Error) -> Error {
        Error::Persist(error)
    }
}

#[cfg(test)]
mod tests;
