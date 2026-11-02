//! One sync, start to finish. Everything here runs on the sync thread.

use super::{RepoSettings, Sessions, kill, plugins, summary, vault};
use crate::git::{self, GitError, Limit, Oid};
use crate::merge::{self, Retired, Store};
use crate::persist::SERVERS_FOLDER;
use crate::persist::{self, Database, Writer};
use crate::repo::{self, AppSettings};
use crate::session::{HeldSession, SessionLink};
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

/// How long to wait for a session to stop writing. A session that doesn't
/// answer puts the merge off to the next sync.
const HOLD_WAIT: Duration = Duration::from_secs(5);
/// The commit that replaces those a removed password was in.
const PASSWORD_REMOVED: &str = "This device's sessions, with a password taken out";

/// How far a sync goes. Each goes as far as the one before it, and further.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Pass {
    /// Export and commit.
    Commit,
    /// Then fetch and merge.
    Pull,
    /// Then squash the session and push.
    Full,
}

#[derive(Debug)]
pub(super) enum Failure {
    Git(GitError),
    Merge(merge::Error),
}

impl From<GitError> for Failure {
    fn from(error: GitError) -> Failure {
        Failure::Git(error)
    }
}

impl From<merge::Error> for Failure {
    fn from(error: merge::Error) -> Failure {
        match error {
            merge::Error::Git(error) => Failure::Git(error),
            error => Failure::Merge(error),
        }
    }
}

impl From<repo::Error> for Failure {
    fn from(error: repo::Error) -> Failure {
        merge::Error::Repo(error).into()
    }
}

impl From<persist::Error> for Failure {
    fn from(error: persist::Error) -> Failure {
        merge::Error::Persist(error).into()
    }
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Failure {
        GitError::from(error).into()
    }
}

/// What a sync that worked did.
#[derive(Debug, Default)]
pub(super) struct Done {
    /// It reached the remote, or a repo with no remote committed.
    pub reached_remote: bool,
    pub changed: Option<String>,
}

/// One server's database, open for this sync. Its own writer means a failure
/// here is never confused with one of the game's.
struct OpenStore {
    server_id: String,
    database: Database,
    writer: Writer,
}

/// The repo a sync thread owns.
pub(super) struct Repository {
    settings: RepoSettings,
    data_folder: PathBuf,
    git: git::Repo,
    files: repo::Repo,
    /// The repo holds this copy of a vault that was in the data folder, and
    /// it can go once that's committed.
    vault_taken: Option<Vec<u8>>,
}

/// Whether a repo in `repo` would hold the data folder: it's the same folder,
/// or one above it. A repo there commits the databases and logs, and takes the
/// vault in from where it already is. Paths are compared as the disk has them,
/// through links, and for a folder not made yet by what exists of it.
pub fn repo_holds_data_folder(repo: &Path, data_folder: &Path) -> bool {
    resolved(data_folder).starts_with(resolved(repo))
}

fn resolved(path: &Path) -> PathBuf {
    let mut missing = Vec::new();
    let mut here = path;
    loop {
        if let Ok(found) = here.canonicalize() {
            return missing
                .iter()
                .rev()
                .fold(found, |full, part| full.join(part));
        }
        match (here.file_name(), here.parent()) {
            (Some(name), Some(parent)) => {
                missing.push(name);
                here = parent;
            }
            _ => return path.to_owned(),
        }
    }
}

fn refuse_data_folder(repo: &Path, data_folder: &Path) -> Result<(), GitError> {
    if repo_holds_data_folder(repo, data_folder) {
        Err(GitError::HoldsTheDataFolder)
    } else {
        Ok(())
    }
}

impl Repository {
    /// Opens the repo for good, clearing what a killed MUDling left. A folder
    /// that holds the data folder is refused before anything is made in it.
    pub(super) fn open(settings: RepoSettings, data_folder: &Path) -> Result<Repository, GitError> {
        refuse_data_folder(&settings.folder, data_folder)?;
        Ok(Repository {
            git: git::Repo::open_or_init(&settings.folder)?,
            files: repo::Repo::new(&settings.folder),
            data_folder: data_folder.to_owned(),
            vault_taken: None,
            settings,
        })
    }

    pub(super) fn folder(&self) -> &Path {
        &self.settings.folder
    }

    pub(super) fn locks(&self) -> Vec<PathBuf> {
        self.git.locks()
    }

    /// Sessions not on the remote yet, for the badge, counted from the last
    /// fetch. A repo with no remote has nowhere to be behind.
    pub(super) fn unsynced(&self) -> usize {
        let Ok(branch) = self.git.current_branch() else {
            return 0;
        };
        merge::unsynced_sessions(&self.git, &self.settings.remote, &branch).unwrap_or(0)
    }

    /// Runs `pass`. `settings` comes out as the newest there is.
    pub(super) fn run(
        &mut self,
        pass: Pass,
        settings: &mut AppSettings,
        sessions: &Sessions,
        session_began: Option<Instant>,
        limit: &Limit,
    ) -> Result<Done, Failure> {
        // Before anything touches the vault or a database: a repo chosen
        // before this check, or a folder moved since, can still hold the data.
        refuse_data_folder(&self.settings.folder, &self.data_folder)?;
        let mut stores = self.open_stores()?;
        let branch = self.git.current_branch()?;

        self.take_in_vault()?;
        let held = self.files.settings()?;
        if held.updated > settings.updated {
            *settings = held;
        }
        self.join_unseen(&views(&stores), settings, sessions, &branch)?;
        if !plugins::has_seen(&self.data_folder, self.git.branch_tip(&branch)?) {
            plugins::join(&self.data_folder, self.folder())?;
        }

        self.export(settings, &views(&stores))?;
        kill::point("pass: exported");
        self.git.commit_all(merge::CHECKPOINT)?;
        kill::point("pass: committed");
        self.drop_taken_vault()?;
        self.mark_seen(&views(&stores), &branch)?;
        kill::point("pass: marked seen");
        if pass == Pass::Commit {
            return Ok(Done::default());
        }

        let remote = self.settings.remote.clone();
        let mut done = Done::default();
        if self.git.has_remote(&remote) {
            merge::forget_old_tombstones(&views(&stores))?;
            if let Some(theirs) = self.git.fetch_within(&remote, &branch, limit)? {
                kill::point("pass: fetched");
                if self.merge_in(theirs, &views(&stores), settings, sessions)? {
                    done.changed = Some("Brought in another device's changes".to_owned());
                }
                self.mark_seen(&views(&stores), &branch)?;
                if self.lists_new_server(&stores)? {
                    stores = self.open_stores()?;
                    self.join_unseen(&views(&stores), settings, sessions, &branch)?;
                    self.mark_seen(&views(&stores), &branch)?;
                }
            }
        }
        if pass == Pass::Pull {
            done.reached_remote = true;
            return Ok(done);
        }

        if let Some(line) = self.squash(&branch, session_began)? {
            done.changed = Some(line);
        }
        kill::point("pass: squashed");
        self.mark_seen(&views(&stores), &branch)?;
        if self.git.has_remote(&remote) {
            match self.git.push_within(&remote, &branch, limit) {
                Err(GitError::PasswordInFile { .. })
                    if self
                        .git
                        .fold_out_password(&remote, &branch, PASSWORD_REMOVED)? =>
                {
                    self.mark_seen(&views(&stores), &branch)?;
                    self.git.push_within(&remote, &branch, limit)?;
                }
                pushed => pushed?,
            }
            kill::point("pass: pushed");
            // Only a fetch shows the push here. Missing it costs a stale badge
            // until the next sync.
            let _ = self.git.fetch_within(&remote, &branch, limit);
        }
        done.reached_remote = true;
        Ok(done)
    }

    /// Every server this device or the repo knows, with a database for each. A
    /// server only the repo has gets an empty one, for a join to fill.
    fn open_stores(&self) -> Result<Vec<OpenStore>, Failure> {
        let folder = self.data_folder.join(SERVERS_FOLDER);
        let mut ids: BTreeSet<String> = self
            .files
            .servers()?
            .into_iter()
            .map(|server| server.id)
            .collect();
        match fs::read_dir(&folder) {
            Ok(entries) => {
                for entry in entries {
                    let path = entry?.path();
                    if path.extension().is_some_and(|ext| ext == "sqlite")
                        && let Some(id) = path.file_stem().and_then(|stem| stem.to_str())
                    {
                        ids.insert(id.to_owned());
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let mut stores = Vec::with_capacity(ids.len());
        for server_id in ids {
            let database = Database::open(&folder, &server_id)?;
            let writer = database.writer()?;
            stores.push(OpenStore {
                server_id,
                database,
                writer,
            });
        }
        Ok(stores)
    }

    fn take_in_vault(&mut self) -> Result<(), Failure> {
        if let Some(taken) = vault::take_in(&self.data_folder, &self.files)? {
            self.vault_taken = Some(taken);
        }
        Ok(())
    }

    fn drop_taken_vault(&mut self) -> Result<(), Failure> {
        if let Some(taken) = self.vault_taken.take() {
            vault::drop_local(&self.data_folder, &taken)?;
        }
        Ok(())
    }

    /// Lets every database that hasn't seen the branch take it in, which any
    /// export first needs. A live session on one waits it out.
    fn join_unseen(
        &self,
        stores: &[Store<'_>],
        settings: &mut AppSettings,
        sessions: &Sessions,
        branch: &str,
    ) -> Result<(), Failure> {
        let tip = self.git.branch_tip(branch)?;
        let mut unseen = Vec::new();
        for store in stores {
            if !merge::has_seen(store, tip)? {
                unseen.push(store.server_id);
            }
        }
        if unseen.is_empty() {
            return Ok(());
        }
        let held = Held::hold(sessions, &unseen)?;
        match merge::join(&self.git, &self.files, settings, stores) {
            Ok(joined) => {
                *settings = joined.settings;
                held.release(&joined.retired);
                kill::point("pass: joined");
                Ok(())
            }
            Err(error) => {
                held.release(&Retired::new());
                Err(error.into())
            }
        }
    }

    fn export(&self, settings: &AppSettings, stores: &[Store<'_>]) -> Result<(), Failure> {
        let mut readers = Vec::with_capacity(stores.len());
        for store in stores {
            let reader = store.database.reader()?;
            if !reader.servers()?.is_empty() {
                readers.push(reader);
            }
        }
        self.files.export(settings, &readers)?;
        plugins::export(&self.data_folder, self.folder())?;
        Ok(())
    }

    /// Merges `theirs`, holding any session's rows while it lands. Returns
    /// whether anything came in.
    fn merge_in(
        &self,
        theirs: Oid,
        stores: &[Store<'_>],
        settings: &mut AppSettings,
        sessions: &Sessions,
    ) -> Result<bool, Failure> {
        let Some(merging) = merge::prepare(&self.git, &self.files, theirs)? else {
            return Ok(false);
        };
        let servers: Vec<&str> = stores.iter().map(|store| store.server_id).collect();
        let held = Held::hold(sessions, &servers)?;
        let landed = merge::land(&self.git, &self.files, &merging, stores);
        if landed.is_ok() {
            held.release(merging.retired());
        } else {
            held.release(&moves_landed(stores, merging.retired()));
        }
        landed?;
        plugins::import(&self.data_folder, self.folder())?;
        // The merge chose between both sides' settings, and the next export
        // would write the old ones back.
        *settings = self.files.settings()?;
        Ok(true)
    }

    /// Whether a merge brought a server that has no database here yet.
    fn lists_new_server(&self, stores: &[OpenStore]) -> Result<bool, Failure> {
        let listed = self.files.servers()?;
        Ok(listed
            .iter()
            .any(|server| stores.iter().all(|store| store.server_id != server.id)))
    }

    /// Folds the session's commits into one, unless it holds just one that
    /// says so already. Returns its line, if there was a session.
    fn squash(
        &self,
        branch: &str,
        session_began: Option<Instant>,
    ) -> Result<Option<String>, Failure> {
        let remote = &self.settings.remote;
        let Some(tip) = self.git.branch_tip(branch)? else {
            return Ok(None);
        };
        let Some(onto) = self.git.session_start(remote, branch, merge::CHECKPOINT)? else {
            return Ok(None);
        };
        if onto == tip {
            return Ok(None);
        }
        let played = session_began.map(|began| began.elapsed());
        let line = summary::session_line(&self.git, onto, tip, played)?;
        self.git.squash(branch, onto, &line)?;
        Ok(Some(line))
    }

    /// Records that `stores` hold the branch's commit, once the folder and
    /// they agree on it.
    fn mark_seen(&self, stores: &[Store<'_>], branch: &str) -> Result<(), Failure> {
        if let Some(tip) = self.git.branch_tip(branch)? {
            merge::mark_seen(stores, tip)?;
            plugins::mark_seen(&self.data_folder, tip)?;
        }
        Ok(())
    }
}

fn views(stores: &[OpenStore]) -> Vec<Store<'_>> {
    stores
        .iter()
        .map(|store| Store {
            server_id: &store.server_id,
            database: &store.database,
            writer: &store.writer,
        })
        .collect()
}

/// The moves that landed, for a merge that failed partway.
fn moves_landed(stores: &[Store<'_>], retired: &Retired) -> Retired {
    let mut landed = Retired::new();
    for store in stores {
        if let Some(moves) = retired.get(store.server_id)
            && let Ok(moves) = merge::moves_landed(store, moves)
        {
            landed.insert(store.server_id.to_owned(), moves);
        }
    }
    landed
}

/// Sessions holding their rows until [`Held::release`].
pub(super) struct Held {
    sessions: Vec<HeldSession>,
    list: Sessions,
}

impl Held {
    /// Asks the sessions on `servers` to hold, and waits until what they'd
    /// already queued is on the disk, so none of it can land on a merge. A
    /// session too busy to answer puts the merge off to the next try. Until
    /// [`Held::release`], a session that attaches holds too, whatever its
    /// server: it may load its map before the merge lands. One that attaches
    /// after that loads what the merge left, so none is missed.
    pub(super) fn hold(sessions: &Sessions, servers: &[&str]) -> Result<Held, Failure> {
        let mut list = sessions.lock().unwrap_or_else(PoisonError::into_inner);
        list.sessions.retain(SessionLink::is_alive);
        let (done, drained) = mpsc::sync_channel(list.sessions.len().max(1));
        let held: Vec<HeldSession> = list
            .sessions
            .iter()
            .filter(|session| servers.contains(&session.server()))
            .filter_map(|session| session.hold(done.clone()))
            .collect();
        list.attached_during_merge = Some(Vec::new());
        drop(list);
        drop(done);
        let held = Held {
            sessions: held,
            list: Arc::clone(sessions),
        };
        for _ in &held.sessions {
            match drained.recv_timeout(HOLD_WAIT) {
                // A session that ended took its sender with it.
                Ok(()) | Err(RecvTimeoutError::Disconnected) => {}
                Err(RecvTimeoutError::Timeout) => {
                    held.release(&Retired::new());
                    return Err(GitError::Locked("a session is busy".to_owned()).into());
                }
            }
        }
        Ok(held)
    }

    /// Lets them go. Each writes what it held, moved off retired rooms, and
    /// rebuilds its map.
    pub(super) fn release(self, retired: &Retired) {
        let mut list = self.list.lock().unwrap_or_else(PoisonError::into_inner);
        let late = list.attached_during_merge.take().unwrap_or_default();
        drop(list);
        for session in self.sessions.into_iter().chain(late) {
            let moves = retired.get(session.server()).cloned().unwrap_or_default();
            session.release(moves);
        }
    }
}
