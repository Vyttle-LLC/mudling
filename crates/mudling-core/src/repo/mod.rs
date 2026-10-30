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
//! then, after a pull has merged the repo's files, import. Import replaces a
//! local record with the file's copy, so a change not yet exported would be
//! lost; export deletes zone and character files with no records behind
//! them, so a file pulled but not yet imported would be too. Import only
//! adds and updates: deleting records and choosing between two copies of one
//! belong to the merge before it.
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

mod format;
mod names;
mod write;

pub use format::AppSettings;

use crate::persist::{self, Batch, Reader};
use format::{
    CHARACTERS_FOLDER, FORMAT_VERSION, GEAR_FOLDER, MANIFEST, MAP_FOLDER, Manifest, NOTES, SERVERS,
    SETTINGS, ServerEntry, ServerList, ServerRows, json, parsed,
};
use names::Names;
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
    /// database, and leaves everything else in the folder alone. A repo in a
    /// newer format is refused untouched.
    pub fn export(&self, settings: &AppSettings, readers: &[Reader]) -> Result<Exported, Error> {
        self.check_format()?;
        let mut servers = Vec::with_capacity(readers.len());
        for reader in readers {
            let rows = ServerRows::load(reader)?;
            let [server] = rows.servers.as_slice() else {
                return Err(Error::NotOneServer(rows.servers.len()));
            };
            servers.push((rows.entry(server), rows));
        }
        servers.sort_by(|(a, _), (b, _)| a.server.id.cmp(&b.server.id));

        let mut files = BTreeMap::new();
        let manifest = Manifest {
            format: FORMAT_VERSION,
        };
        files.insert(PathBuf::from(MANIFEST), json(&manifest)?);
        files.insert(PathBuf::from(SETTINGS), json(settings)?);
        let list = ServerList {
            servers: servers.iter().map(|(entry, _)| entry.clone()).collect(),
        };
        files.insert(PathBuf::from(SERVERS), json(&list)?);

        let mut folders = Names::default();
        let mut server_folders = Vec::with_capacity(servers.len());
        for (entry, rows) in &servers {
            let folder = PathBuf::from(folders.folder(&entry.server.id));
            for (path, bytes) in rows.files()? {
                files.insert(folder.join(path), bytes);
            }
            server_folders.push(folder);
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
        match self.read_if_present(Path::new(SETTINGS))? {
            Some(bytes) => parsed(Path::new(SETTINGS), &bytes),
            None => Ok(AppSettings::default()),
        }
    }

    /// Reads back what differs from the database behind `reader`: only
    /// files that don't match what it would export are parsed. Submit the
    /// batch to the database's writer. A fresh database gets everything.
    pub fn import(&self, server_id: &str, reader: &Reader) -> Result<Imported, Error> {
        let list = self.server_list()?;
        let mut folders = Names::default();
        let (listed, folder) = list
            .servers
            .iter()
            .map(|entry| (entry, PathBuf::from(folders.folder(&entry.server.id))))
            .find(|(entry, _)| entry.server.id == server_id)
            .ok_or_else(|| Error::UnknownServer(server_id.to_owned()))?;

        let rows = ServerRows::load(reader)?;
        let local = rows.files()?;
        let mut batch = Batch::new();
        let mut read = Vec::new();
        if local_entry(&rows, server_id).as_ref() != Some(listed) {
            add_entry(listed, &mut batch);
            read.push(PathBuf::from(SERVERS));
        }
        for relative in self.server_files(&folder)? {
            let path = self.root.join(&folder).join(&relative);
            let bytes = fs::read(&path).map_err(|error| Error::io(&path, error))?;
            if local.get(&relative) != Some(&bytes) {
                format::read_file(&relative, &bytes, &mut batch)?;
                read.push(folder.join(relative));
            }
        }
        Ok(Imported { batch, read })
    }

    /// Refuses a repo a newer MUDling wrote. A repo with no manifest yet has
    /// nothing to protect.
    fn check_format(&self) -> Result<(), Error> {
        let Some(bytes) = self.read_if_present(Path::new(MANIFEST))? else {
            return Ok(());
        };
        let Manifest { format } = parsed(Path::new(MANIFEST), &bytes)?;
        if format > FORMAT_VERSION {
            return Err(Error::NewerFormat(format));
        }
        Ok(())
    }

    fn server_list(&self) -> Result<ServerList, Error> {
        self.check_format()?;
        let Some(bytes) = self.read_if_present(Path::new(SERVERS))? else {
            return Ok(ServerList {
                servers: Vec::new(),
            });
        };
        let mut list: ServerList = parsed(Path::new(SERVERS), &bytes)?;
        // Folder names are claimed in this order, as export claims them.
        list.servers.sort_by(|a, b| a.server.id.cmp(&b.server.id));
        Ok(list)
    }

    /// Every file of a server, relative to its folder.
    fn server_files(&self, folder: &Path) -> Result<Vec<PathBuf>, Error> {
        let folder = self.root.join(folder);
        let mut files = Vec::new();
        for inner in [Path::new(""), Path::new(GEAR_FOLDER)]
            .into_iter()
            .chain([Path::new(MAP_FOLDER), Path::new(CHARACTERS_FOLDER)])
        {
            for path in write::listed(&folder.join(inner))? {
                let Some(name) = path.file_name() else {
                    continue;
                };
                if write::is_json(&path) || name == NOTES {
                    files.push(inner.join(name));
                }
            }
        }
        Ok(files)
    }

    fn read_if_present(&self, relative: &Path) -> Result<Option<Vec<u8>>, Error> {
        let path = self.root.join(relative);
        match fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(Error::io(&path, error)),
        }
    }
}

fn local_entry(rows: &ServerRows, server_id: &str) -> Option<ServerEntry> {
    let server = rows.servers.iter().find(|server| server.id == server_id)?;
    Some(rows.entry(server))
}

fn add_entry(entry: &ServerEntry, batch: &mut Batch) {
    batch.upsert(entry.server.clone());
    for meta in &entry.meta {
        batch.upsert(meta.clone());
    }
    for character in &entry.characters {
        batch.upsert(character.clone());
    }
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
