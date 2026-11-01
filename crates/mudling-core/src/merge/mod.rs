//! Two devices' data becoming one, by record and never by line of text
//! (PRD §10).
//!
//! Every pull merges the same way, whether or not git would call it a
//! conflict. Each server's files on both sides are read back into records; the
//! newer `updated` wins each record, ties go to the same side on every device,
//! and rooms, sightings and copies only one side has are kept. [`Tombstone`]s
//! carry deletes: one outranks anything last changed before it, so a deleted
//! record stays deleted unless the other side changed it afterwards. They live
//! in each server's `tombstones.json` for 90 days. A device offline for longer
//! brings back what was deleted while it was away.
//!
//! Rooms with no game number are named by the device that found them, so one
//! room can reach a merge under two ids. `rooms.rs` says when they're united,
//! and `join.rs` how the player joins them by hand.
//!
//! Files with no records are merged file by file against the commit both sides
//! share. `secrets.vault` has a merge of its own, which needs no keys.
//!
//! Each sync on the sync thread runs in this order:
//!
//! ```no_run
//! use mudling_core::merge::{self, Store};
//! use mudling_core::persist::Database;
//! use mudling_core::repo::{AppSettings, Repo};
//! use mudling_core::git;
//! # use std::path::Path;
//! # fn demo(folder: &Path) -> Result<(), merge::Error> {
//! # let database = Database::open(folder, "mud.example.org").unwrap();
//! # let writer = database.writer().unwrap();
//! let repo = Repo::new(folder);
//! let git = git::Repo::open_or_init(folder)?;
//! let stores = [Store {
//!     server_id: "mud.example.org",
//!     database: &database,
//!     writer: &writer,
//! }];
//! repo.export(&AppSettings::default(), &[database.reader().unwrap()])?;
//! git.commit_all(merge::CHECKPOINT)?;
//! if let Some(theirs) = git.fetch("origin", "main")? {
//!     merge::pull(&git, &repo, theirs, &stores)?;
//! }
//! git.push("origin", "main")?;
//! # Ok(())
//! # }
//! ```
//!
//! [`crate::sync`] runs this on its thread, splitting [`pull`] into [`prepare`]
//! and [`land`] so a live session can wait out the part that touches its rows.
//!
//! **A kill at any moment loses nothing.** The merged commit is written first,
//! touching no branch. Each database then forgets which commit it last saw, and
//! the folder's files come next, then the import into SQLite, and the branch
//! moves last. So a database that was killed partway says so: [`has_seen`]
//! answers no, and [`join`] takes everything in the folder into it before any
//! export can delete files it hasn't read. The merge then runs again, and the
//! merge of a merged side changes nothing. The same check protects the first
//! export on a device that already holds a repo, a server's database made after
//! the repo was set, and a branch moved by a git outside MUDling.
//!
//! [`Tombstone`]: crate::persist::Tombstone

mod files;
mod join;
mod records;
mod rooms;
mod secrets;

pub(crate) use files::{servers, three_way};
pub(crate) use join::{join_rooms, joins_cancelled_by, rejoin_restored, undo_join};
pub use rooms::RoomMoves;
pub(crate) use rooms::repoint_batch;
pub use secrets::find_password;

#[cfg(test)]
mod tests;

use crate::git::{self, GitError, Oid};
use crate::map::CURRENT_ROOM;
use crate::persist::{self, Batch, Database, Meta, Reader, Timestamp, Writer};
use crate::repo::format::{ServerRows, tombstone_cutoff};
use crate::repo::{self, AppSettings, Repo};
use crate::sync::kill;
use crate::vault;
use records::merge_server;
use rooms::retired_rooms;
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// The message of a commit made during a session, before the session's squash
/// replaces them with one. All of them count as one session not yet synced.
pub const CHECKPOINT: &str = "Checkpoint";

const MERGE_MESSAGE: &str = "Merge another device's changes";
const JOIN_MESSAGE: &str = "Add this device's data";
/// The commit a database has taken in everything from. Local to the device,
/// since it says what this database holds.
const SEEN: &str = "sync.seen";

/// The rooms a merge retires, by server id.
pub type Retired = BTreeMap<String, RoomMoves>;

/// One server's database on this device.
pub struct Store<'a> {
    pub server_id: &'a str,
    pub database: &'a Database,
    pub writer: &'a Writer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pulled {
    /// This branch already holds everything `theirs` does.
    UpToDate,
    /// The branch moved to this commit, and the folder and `stores` match it.
    Merged(Oid),
}

/// What setting a repo came to.
#[derive(Clone, Debug, PartialEq)]
pub struct Joined {
    /// The commit holding this device's data, if it added any.
    pub commit: Option<Oid>,
    /// The newer of the device's settings and the repo's, for the app to use.
    pub settings: AppSettings,
    /// The rooms this retired, for sessions playing on them.
    pub retired: Retired,
}

/// Merges `theirs`, a fetched commit, into the current branch, the folder and
/// `stores`. Export and commit first, so the branch holds every local change.
/// Servers the repo lists with no store here stay in the folder for
/// [`Repo::import`], and `repo.settings()` then holds the merged settings.
pub fn pull(
    git: &git::Repo,
    repo: &Repo,
    theirs: Oid,
    stores: &[Store<'_>],
) -> Result<Pulled, Error> {
    forget_old_tombstones(stores)?;
    let Some(merge) = prepare(git, repo, theirs)? else {
        return Ok(Pulled::UpToDate);
    };
    land(git, repo, &merge, stores)?;
    Ok(Pulled::Merged(merge.commit))
}

/// Puts a prepared merge into the folder, `stores` and the branch, in the order
/// the kill rule needs. Anything playing on `stores` should wait for this.
pub fn land(
    git: &git::Repo,
    repo: &Repo,
    merge: &Prepared,
    stores: &[Store<'_>],
) -> Result<(), Error> {
    refuse_unsaved_vault(repo, merge.vault.as_ref())?;
    forget_seen(stores)?;
    kill::point("land: databases forgot what they saw");
    kill::arm_after_one_file("land: halfway through the files");
    repo.replace_files(&merge.files, &merge.gone)?;
    kill::point("land: files written");
    import(repo, stores, &merge.retired)?;
    kill::point("land: imported");
    git.move_branch(&merge.branch, merge.ours, merge.commit)?;
    kill::point("land: branch moved");
    Ok(())
}

/// Whether `store`'s database has taken in everything up to `tip`, the branch's
/// commit. Until it has, an export would delete what it hasn't read.
pub fn has_seen(store: &Store<'_>, tip: Option<Oid>) -> Result<bool, Error> {
    let Some(tip) = tip else {
        return Ok(false);
    };
    let meta = store.database.reader()?.meta()?;
    let seen = meta.iter().find(|entry| entry.key == SEEN);
    Ok(seen.is_some_and(|entry| entry.value == tip.to_string()))
}

/// Records that `stores` hold everything up to `tip`.
pub fn mark_seen(stores: &[Store<'_>], tip: Oid) -> Result<(), Error> {
    for store in stores {
        if !has_seen(store, Some(tip))? {
            write_seen(store, tip.to_string())?;
        }
    }
    Ok(())
}

fn forget_seen(stores: &[Store<'_>]) -> Result<(), Error> {
    for store in stores {
        write_seen(store, String::new())?;
    }
    Ok(())
}

fn write_seen(store: &Store<'_>, value: String) -> Result<(), Error> {
    let mut batch = Batch::new();
    batch.upsert(Meta {
        key: SEEN.to_owned(),
        value,
        updated: Timestamp::now(),
    });
    store.writer.submit(batch)?;
    store.writer.flush()?;
    Ok(())
}

/// Brings this device's data into the repo set, which may already hold another
/// device's: SQLite and the repo's records merge as two sides do, into SQLite
/// first, and the export that follows writes the result. So no file is lost
/// to an export that hasn't seen it, and the only records that go are the ones
/// a tombstone says were deleted on purpose. A database that [`has_seen`] the
/// branch has nothing to take in. `stores` holds a database for every server on
/// this device, and an empty one for each server only the repo has. Safe to run
/// again after a kill.
pub fn join(
    git: &git::Repo,
    repo: &Repo,
    settings: &AppSettings,
    stores: &[Store<'_>],
) -> Result<Joined, Error> {
    let tip = git.branch_tip(&git.current_branch()?)?;
    let mut retired = Retired::new();
    for store in stores {
        if has_seen(store, tip)? {
            continue;
        }
        if let Some(held) = repo.server_rows(store.server_id)? {
            let local = ServerRows::load(&store.database.reader()?)?;
            let merged = merge_server(local.clone(), held, &[]);
            let moves = retired_rooms(&local, &merged);
            let mut batch = merged.into_batch();
            follow_current_room(&store.database.reader()?, &moves, &mut batch)?;
            store.writer.submit(batch)?;
            store.writer.flush()?;
            if !moves.is_empty() {
                retired.insert(store.server_id.to_owned(), moves);
            }
        }
    }
    let held = repo.settings()?;
    let settings = if held.updated > settings.updated {
        held
    } else {
        settings.clone()
    };
    let mut readers = Vec::with_capacity(stores.len());
    for store in stores {
        let reader = store.database.reader()?;
        if !reader.servers()?.is_empty() {
            readers.push(reader);
        }
    }
    repo.export(&settings, &readers)?;
    let commit = git.commit_all(JOIN_MESSAGE)?;
    Ok(Joined {
        commit,
        settings,
        retired,
    })
}

/// Keeps the player's place when the room they're in is retired: the map's
/// current room isn't synced, so nothing else would move it.
fn follow_current_room(
    reader: &Reader,
    moves: &RoomMoves,
    batch: &mut Batch,
) -> Result<(), persist::Error> {
    let meta = reader.meta()?;
    let current = meta.iter().find(|entry| entry.key == CURRENT_ROOM);
    let id = current.and_then(|entry| entry.value.parse().ok());
    if let Some(Some(kept)) = id.and_then(|id| moves.get(&id)) {
        batch.upsert(Meta {
            key: CURRENT_ROOM.to_owned(),
            value: kept.to_string(),
            updated: Timestamp::now(),
        });
    }
    Ok(())
}

/// The part of `moves` that happened: the rooms `store` no longer holds. A
/// merge that failed partway may have landed on one database and not another.
pub fn moves_landed(store: &Store<'_>, moves: &RoomMoves) -> Result<RoomMoves, Error> {
    let rooms = store.database.reader()?.map()?.rooms;
    let mut landed = moves.clone();
    landed.retain(|id, _| rooms.iter().all(|room| room.id != *id));
    Ok(landed)
}

/// How many sessions on this device `remote` hasn't got yet, for the sprocket
/// badge: each squashed commit is one, and [`CHECKPOINT`] commits together are
/// the one still going. Counts from the last fetch, and is 0 with no remote.
pub fn unsynced_sessions(git: &git::Repo, remote: &str, branch: &str) -> Result<usize, GitError> {
    let messages = git.unpushed_messages(remote, branch)?;
    let is_checkpoint = |message: &String| message.trim_end() == CHECKPOINT;
    let squashed = messages.iter().filter(|m| !is_checkpoint(m)).count();
    Ok(squashed + usize::from(messages.iter().any(is_checkpoint)))
}

/// Everything a merge has worked out before it touches the folder.
pub struct Prepared {
    branch: String,
    ours: Option<Oid>,
    commit: Oid,
    files: git::Files,
    /// Files on the branch now that the merged commit doesn't hold.
    gone: Vec<PathBuf>,
    retired: Retired,
    /// The vault as the branch holds it, which the folder matched when the
    /// merge was worked out.
    vault: Option<Vec<u8>>,
}

impl Prepared {
    /// The rooms on this device that the merge retires.
    pub fn retired(&self) -> &Retired {
        &self.retired
    }
}

/// Works out the merge, and writes its commit without moving any branch.
/// `None` if `theirs` brings nothing new.
pub fn prepare(git: &git::Repo, repo: &Repo, theirs: Oid) -> Result<Option<Prepared>, Error> {
    let branch = git.current_branch()?;
    let ours = git.branch_tip(&branch)?;
    if let Some(ours) = ours
        && git.is_ancestor(theirs, ours)?
    {
        return Ok(None);
    }
    let their_files = git.files_at(theirs)?;
    files::check_format(&their_files)?;
    let our_files = match ours {
        Some(ours) => git.files_at(ours)?,
        None => git::Files::new(),
    };
    let vault = our_files.get(Path::new(vault::FILE_NAME)).cloned();
    refuse_unsaved_vault(repo, vault.as_ref())?;
    let (files, commit) = match ours {
        Some(ours) if !git.is_ancestor(ours, theirs)? => {
            let base = match git.merge_base(ours, theirs)? {
                Some(base) => git.files_at(base)?,
                None => git::Files::new(),
            };
            let theirs_newer = (git.commit_time(theirs)?, theirs) > (git.commit_time(ours)?, ours);
            let files = files::merge_files(&base, &our_files, &their_files, theirs_newer)?;
            let commit = git.write_commit(&files, &[ours, theirs], MERGE_MESSAGE)?;
            (files, commit)
        }
        _ => (their_files, theirs),
    };
    let gone = our_files
        .keys()
        .filter(|path| !files.contains_key(*path))
        .cloned()
        .collect();
    let retired = files::retired(&our_files, &files)?;
    Ok(Some(Prepared {
        branch,
        ours,
        commit,
        files,
        gone,
        retired,
        vault,
    }))
}

/// The vault saves straight to the folder, so its file can be ahead of the
/// branch. Writing the merge over that would lose a password. This runs when
/// the merge is worked out and again as it lands, since a session's rows are
/// waited for in between, and a password can be saved meanwhile. A save in the
/// instant between that second look and the write can still be lost.
fn refuse_unsaved_vault(repo: &Repo, committed: Option<&Vec<u8>>) -> Result<(), Error> {
    let path = Path::new(vault::FILE_NAME);
    if repo.read_if_present(path)?.as_ref() == committed {
        Ok(())
    } else {
        Err(Error::VaultNotCommitted)
    }
}

/// Brings the repo's files into each database that has a server there, and
/// waits for them to land: the caller moves on only once they have.
fn import(repo: &Repo, stores: &[Store<'_>], retired: &Retired) -> Result<(), Error> {
    let listed = repo.servers()?;
    for store in stores {
        if listed.iter().any(|server| server.id == store.server_id) {
            let reader = store.database.reader()?;
            let mut batch = repo.import(store.server_id, &reader)?.batch;
            if let Some(moves) = retired.get(store.server_id) {
                follow_current_room(&reader, moves, &mut batch)?;
            }
            store.writer.submit(batch)?;
            store.writer.flush()?;
        }
    }
    Ok(())
}

/// Clears tombstones past their life, on every sync.
pub fn forget_old_tombstones(stores: &[Store<'_>]) -> Result<(), Error> {
    let before = tombstone_cutoff();
    for store in stores {
        let mut batch = Batch::new();
        batch.forget_tombstones(before);
        store.writer.submit(batch)?;
    }
    Ok(())
}

#[derive(Debug)]
pub enum Error {
    Git(GitError),
    Repo(repo::Error),
    Persist(persist::Error),
    Vault(vault::Error),
    /// `secrets.vault` changed since the last commit. Nothing was touched:
    /// commit, and the merge can run.
    VaultNotCommitted,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Git(error) => write!(f, "{error}"),
            Error::Repo(error) => write!(f, "{error}"),
            Error::Persist(error) => write!(f, "{error}"),
            Error::Vault(error) => write!(f, "{error}"),
            Error::VaultNotCommitted => {
                write!(f, "{} has changes not committed yet", vault::FILE_NAME)
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<GitError> for Error {
    fn from(error: GitError) -> Error {
        Error::Git(error)
    }
}

impl From<repo::Error> for Error {
    fn from(error: repo::Error) -> Error {
        Error::Repo(error)
    }
}

impl From<persist::Error> for Error {
    fn from(error: persist::Error) -> Error {
        Error::Persist(error)
    }
}

impl From<vault::Error> for Error {
    fn from(error: vault::Error) -> Error {
        Error::Vault(error)
    }
}
