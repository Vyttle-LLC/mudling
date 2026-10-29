//! Persistence: one SQLite database per server, in WAL mode.
//!
//! Callers describe changed rows in a [`Batch`] and hand it to the
//! [`Writer`], whose thread commits each batch in one transaction; nothing
//! ever re-encodes a whole world. Startup loads and lists go through a
//! [`Reader`], on a connection of its own.
//!
//! ```no_run
//! use mudling_core::persist::{Batch, Database, Room, Timestamp};
//!
//! let folder = Database::default_folder().expect("the OS has a data folder");
//! let database = Database::open(&folder, "mud.example.org")?;
//! let writer = database.writer()?;
//! let mut batch = Batch::new();
//! batch.upsert(Room {
//!     id: 1,
//!     name: "A quiet road".into(),
//!     area: "The Meadow".into(),
//!     environment: "road".into(),
//!     description: String::new(),
//!     x: 0,
//!     y: 0,
//!     z: 0,
//!     building: None,
//!     tangled: false,
//!     maze: false,
//!     updated: Timestamp::now(),
//! });
//! writer.submit(batch)?;
//! let map = database.reader()?.map()?;
//! # Ok::<(), mudling_core::persist::Error>(())
//! ```

mod batch;
mod reader;
mod records;
mod schema;
mod writer;

pub use batch::{Batch, Key, Record};
pub use reader::{CharacterRows, GearRows, MapRows, Reader};
pub use records::*;
pub use schema::LATEST_VERSION;
pub use writer::{Failures, Writer};

use rusqlite::{Connection, ErrorCode, OpenFlags};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Statements each connection keeps compiled: every upsert, delete and
/// select fits.
const STATEMENT_CACHE: usize = 96;
/// How long a connection waits on another's lock (a checkpoint, a second
/// app instance) before giving up.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// A server's database file, created and migrated on open.
#[derive(Clone, Debug)]
pub struct Database {
    path: PathBuf,
}

impl Database {
    /// The OS data folder MUDling keeps its databases in. Tests and
    /// benchmarks pass a folder of their own to [`Database::open`] instead.
    pub fn default_folder() -> Option<PathBuf> {
        directories::ProjectDirs::from("com", "Vyttle", "MUDling")
            .map(|dirs| dirs.data_dir().join("servers"))
    }

    /// Opens (or creates) the database for `server` in `folder`, bringing
    /// its schema up to date. `server` names the file, so it's limited to
    /// letters, digits, `.`, `-` and `_`, as in a host name. This touches
    /// the disk: call it from a core thread.
    pub fn open(folder: &Path, server: &str) -> Result<Database, Error> {
        let valid = !server.is_empty()
            && !server.starts_with('.')
            && server
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
        if !valid {
            return Err(Error::BadServerName(server.to_owned()));
        }
        std::fs::create_dir_all(folder).map_err(Error::Io)?;
        let database = Database {
            path: folder.join(format!("{server}.sqlite")),
        };
        let mut connection = database.connect(OpenFlags::default())?;
        use_wal(&connection)?;
        schema::migrate(&mut connection)?;
        Ok(database)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Starts a writer thread with a connection of its own.
    pub fn writer(&self) -> Result<Writer, Error> {
        let connection = self.connect(OpenFlags::default())?;
        // Each commit reaches the disk before the next starts, so a crash
        // or power loss keeps every committed batch. It costs the writer
        // thread, never the caller.
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.pragma_update(None, "foreign_keys", true)?;
        Writer::spawn(connection)
    }

    /// A read-only connection for startup loads and lists.
    pub fn reader(&self) -> Result<Reader, Error> {
        let connection =
            self.connect(OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        Ok(Reader::new(connection))
    }

    /// The schema version the file is at.
    pub fn schema_version(&self) -> Result<u32, Error> {
        let connection = self.connect(OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        Ok(connection.pragma_query_value(None, "user_version", |row| row.get(0))?)
    }

    fn connect(&self, flags: OpenFlags) -> Result<Connection, Error> {
        let connection = Connection::open_with_flags(&self.path, flags)?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        connection.set_prepared_statement_cache_capacity(STATEMENT_CACHE);
        Ok(connection)
    }
}

#[derive(Debug)]
pub enum Error {
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
    /// The server name can't be used as a file name.
    BadServerName(String),
    /// The writer thread is gone (it panicked), so nothing more is written.
    WriterStopped,
    /// Batches that failed since failures were last collected.
    BatchesFailed(Failures),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Sqlite(error) => write!(f, "database error: {error}"),
            Error::Io(error) => write!(f, "couldn't reach the data folder: {error}"),
            Error::BadServerName(name) => write!(f, "{name:?} can't name a database file"),
            Error::WriterStopped => write!(f, "the database writer has stopped"),
            Error::BatchesFailed(failures) => {
                write!(
                    f,
                    "{} batch(es) failed and were rolled back; the latest: {}",
                    failures.count(),
                    failures.errors.back().expect("a failure was kept")
                )
            }
        }
    }
}

impl std::error::Error for Error {}

/// Puts the file in WAL mode, which every later connection then uses.
/// Switching needs the file to itself, and SQLite won't wait for that (it
/// could deadlock), so an open racing another retries until the timeout.
fn use_wal(connection: &Connection) -> rusqlite::Result<()> {
    let deadline = Instant::now() + BUSY_TIMEOUT;
    loop {
        match connection.pragma_update(None, "journal_mode", "WAL") {
            Err(error)
                if error.sqlite_error_code() == Some(ErrorCode::DatabaseBusy)
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(1));
            }
            result => return result,
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Error {
        Error::Sqlite(error)
    }
}

#[cfg(test)]
mod tests;
