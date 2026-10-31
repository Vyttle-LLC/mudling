//! Embedded git: libgit2 with everything vendored, so sync works with nothing
//! installed and no system `git` is ever run (PRD §10).
//!
//! [`Repo`] offers what sync needs and no more. It holds the repo for its
//! whole life, so one sync thread should own it. Every step is safe to repeat:
//! a failure is a typed [`GitError`] and loses nothing, and a crash between
//! steps leaves the repo valid.
//!
//! Network steps run on a thread of their own, with a handle of their own, so
//! a [`Limit`] can end the wait for anything: a stalled server, a helper, or an
//! agent waiting on a finger. A step given up on may still finish later, which
//! is harmless: a fetch moves a tracking ref, and a push is one atomic step.
//!
//! Call [`prepare`] once at startup.

mod credentials;
mod error;
mod locks;
#[cfg(unix)]
mod ssh_agent;

#[cfg(test)]
mod tests;

pub use error::GitError;
pub use git2::Oid;

use crate::merge;
use git2::{
    Commit, FetchOptions, Index, IndexAddOption, IndexEntry, IndexTime, ObjectType, PushOptions,
    Repository, RepositoryInitOptions, Signature, Tree,
};
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

pub type Result<T> = std::result::Result<T, GitError>;

/// The files of a commit, by path relative to the repo's root.
pub type Files = BTreeMap<PathBuf, Vec<u8>>;

const DEFAULT_BRANCH: &str = "main";
const REGULAR_FILE: u32 = 0o100_644;
/// Commits made on a machine with no git identity set up.
const FALLBACK_NAME: &str = "MUDling";
const FALLBACK_EMAIL: &str = "mudling@localhost";
/// How long [`Repo::fetch`] and [`Repo::push`] wait with no limit of their own.
const NETWORK_LIMIT: Duration = Duration::from_secs(60);
/// libgit2's own socket limits, so a thread given up on doesn't stay for good.
const CONNECT_TIMEOUT_MS: i32 = 15_000;
const SOCKET_TIMEOUT_MS: i32 = 30_000;
/// How often a wait for a network step looks at its limit.
const LIMIT_POLL: Duration = Duration::from_millis(20);
/// How a server words a push that lost a race: another push moved the branch
/// after this one read it, or holds the branch's lock right now, so the
/// server's update of the ref failed.
const LOST_RACE: [&str; 5] = [
    "non-fast-forward",
    "fetch first",
    "cannot lock ref",
    "failed to update ref",
    "failed to lock",
];

/// Sets what must be set while no other thread can see it: libgit2's socket
/// timeouts, and which ssh-agent signs in. An app calls this once, first thing
/// in `main`. It may wait a moment on an agent that's slow to answer.
#[allow(unsafe_code)]
pub fn prepare() {
    // SAFETY: the caller runs this before any thread exists, so nothing is
    // using libgit2's options yet.
    unsafe {
        let _ = git2::opts::set_server_connect_timeout_in_milliseconds(CONNECT_TIMEOUT_MS);
        let _ = git2::opts::set_server_timeout_in_milliseconds(SOCKET_TIMEOUT_MS);
    }
    #[cfg(unix)]
    ssh_agent::select();
}

/// When to stop waiting for a network step: at a deadline, or as soon as
/// someone raises the flag.
#[derive(Clone, Debug)]
pub struct Limit {
    deadline: Instant,
    stop: Arc<AtomicBool>,
}

impl Limit {
    pub fn after(wait: Duration) -> Limit {
        Limit::stopped_by(wait, Arc::default())
    }

    pub fn stopped_by(wait: Duration, stop: Arc<AtomicBool>) -> Limit {
        Limit {
            deadline: Instant::now() + wait,
            stop,
        }
    }

    pub fn is_up(&self) -> bool {
        self.stop.load(Ordering::Relaxed) || Instant::now() >= self.deadline
    }
}

pub struct Repo {
    repo: Repository,
    _owner: File,
}

impl Repo {
    /// Opens the repo at `folder`, or starts one if the folder is empty or
    /// missing. Clears the locks a killed MUDling left. Fails with
    /// [`GitError::Locked`] if another MUDling has it open.
    pub fn open_or_init(folder: &Path) -> Result<Self> {
        let repo = match Repository::open(folder) {
            Ok(repo) => repo,
            Err(error) if error.code() == git2::ErrorCode::NotFound => init(folder)?,
            Err(error) => return Err(error.into()),
        };
        let owner = locks::take_ownership(repo.commondir())?;
        locks::clear_after_crash(repo.commondir());
        Ok(Self {
            repo,
            _owner: owner,
        })
    }

    /// The branch HEAD is on, which may have no commits yet.
    pub fn current_branch(&self) -> Result<String> {
        let head = self.repo.find_reference("HEAD")?;
        head.symbolic_target()?
            .and_then(|target| target.strip_prefix("refs/heads/"))
            .map(str::to_owned)
            .ok_or_else(|| GitError::Failed("HEAD isn't on a branch".into()))
    }

    /// Where `branch` points, or `None` if it has no commits.
    pub fn branch_tip(&self, branch: &str) -> Result<Option<Oid>> {
        self.tip(&local_ref(branch))
    }

    /// Stages every change in the folder and commits it to the current branch.
    /// Returns `None` if nothing changed.
    pub fn commit_all(&self, message: &str) -> Result<Option<Oid>> {
        let _writing = self.mark_write()?;
        let mut index = self.repo.index()?;
        index.read(true)?;
        index.add_all(["*"], IndexAddOption::DEFAULT, None)?;
        index.update_all(["*"], None)?;
        index.write()?;
        let tree_id = index.write_tree()?;

        let parent = self.head_commit()?;
        let unchanged = match &parent {
            Some(parent) => parent.tree_id() == tree_id,
            None => index.is_empty(),
        };
        if unchanged {
            return Ok(None);
        }
        let tree = self.repo.find_tree(tree_id)?;
        let parents: Vec<&Commit> = parent.iter().collect();
        let signature = self.signature()?;
        let id = self.repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &parents,
        )?;
        Ok(Some(id))
    }

    /// [`Repo::fetch_within`] with the usual limit.
    pub fn fetch(&self, remote: &str, branch: &str) -> Result<Option<Oid>> {
        self.fetch_within(remote, branch, &Limit::after(NETWORK_LIMIT))
    }

    /// Fetches `branch` from `remote`, returning its tip there, or `None` if
    /// the remote doesn't have it yet. Local branches don't move. Fails with
    /// [`GitError::TimedOut`] if `limit` comes first.
    pub fn fetch_within(&self, remote: &str, branch: &str, limit: &Limit) -> Result<Option<Oid>> {
        let (remote_name, branch_name) = (remote.to_owned(), branch.to_owned());
        let helpers = limit.clone();
        self.off_thread(limit, move |repo| {
            let _writing = locks::WriteMarker::set(repo.commondir())?;
            let tracking = tracking_ref(&remote_name, &branch_name);
            let refspec = format!("+{}:{tracking}", local_ref(&branch_name));
            let mut options = FetchOptions::new();
            options.remote_callbacks(credentials::machine_callbacks(repo.config()?, helpers));
            repo.find_remote(&remote_name)?
                .fetch(&[refspec], Some(&mut options), None)?;
            Ok(())
        })?;
        self.tip(&tracking_ref(remote, branch))
    }

    /// [`Repo::push_within`] with the usual limit.
    pub fn push(&self, remote: &str, branch: &str) -> Result<()> {
        self.push_within(remote, branch, &Limit::after(NETWORK_LIMIT))
    }

    /// Pushes `branch` to `remote`, which takes it in one atomic step or not at
    /// all. Fails with [`GitError::NonFastForward`] if the remote is ahead,
    /// even if it only got there during this push, and with
    /// [`GitError::PasswordInFile`], reaching no network, if any commit the
    /// remote lacks holds a password outside `secrets.vault`.
    pub fn push_within(&self, remote: &str, branch: &str, limit: &Limit) -> Result<()> {
        self.refuse_passwords(remote, branch)?;
        let remote = remote.to_owned();
        let name = local_ref(branch);
        let helpers = limit.clone();
        self.off_thread(limit, move |repo| {
            let _writing = locks::WriteMarker::set(repo.commondir())?;
            let mut rejection = None;
            let mut callbacks = credentials::machine_callbacks(repo.config()?, helpers);
            callbacks.push_update_reference(|_, status| {
                rejection = status.map(str::to_owned);
                Ok(())
            });
            let mut options = PushOptions::new();
            options.remote_callbacks(callbacks);
            repo.find_remote(&remote)?
                .push(&[format!("{name}:{name}")], Some(&mut options))?;
            drop(options);
            rejection.map_or(Ok(()), |reason| Err(push_rejected(reason)))
        })
    }

    /// Runs `work` on a thread with a handle of its own, and waits for it
    /// until `limit` is up. The thread is then left to end by itself.
    fn off_thread<T: Send + 'static>(
        &self,
        limit: &Limit,
        work: impl FnOnce(&Repository) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let folder = self.repo.path().to_owned();
        let (done, finished) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("mudling-git".to_owned())
            .spawn(move || {
                let _ = done.send(
                    Repository::open(folder)
                        .map_err(GitError::from)
                        .and_then(|repo| work(&repo)),
                );
            })?;
        loop {
            match finished.recv_timeout(LIMIT_POLL) {
                // A helper killed at the limit reads as a refusal, and isn't one.
                Ok(Err(_)) if limit.is_up() => return Err(timed_out()),
                Ok(result) => return result,
                Err(RecvTimeoutError::Timeout) if limit.is_up() => return Err(timed_out()),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(GitError::Failed("the git thread stopped".to_owned()));
                }
            }
        }
    }

    /// Whether the repo has a remote of this name.
    pub fn has_remote(&self, remote: &str) -> bool {
        self.repo.find_remote(remote).is_ok()
    }

    /// Where the last fetch from `remote` saw `branch`.
    pub fn tracked_tip(&self, remote: &str, branch: &str) -> Result<Option<Oid>> {
        self.tip(&tracking_ref(remote, branch))
    }

    /// The commit to squash a session's commits onto, which `tip` builds on:
    /// the newest finished session or what `remote` holds, or `None` if `tip`
    /// has nothing to squash onto. A merge folds in the sessions before it,
    /// because the squash must keep building on what `remote` holds.
    pub fn session_start(
        &self,
        remote: &str,
        branch: &str,
        session_message: &str,
    ) -> Result<Option<Oid>> {
        let Some(tip) = self.branch_tip(branch)? else {
            return Ok(None);
        };
        let tracked = self.tracked_tip(remote, branch)?;
        let mut commit = self.repo.find_commit(tip)?;
        let finished = loop {
            let in_session = commit.message().unwrap_or_default().trim_end() == session_message
                || commit.parent_count() > 1;
            if !in_session || Some(commit.id()) == tracked {
                break Some(commit.id());
            }
            match commit.parent(0) {
                Ok(parent) => commit = parent,
                Err(_) => break None,
            }
        };
        match (finished, tracked) {
            (Some(finished), Some(tracked)) if !self.is_ancestor(tracked, finished)? => {
                Ok(Some(tracked))
            }
            (None, tracked) => Ok(tracked),
            (finished, _) => Ok(finished),
        }
    }

    /// The `.lock` files in the repo's git folder, for telling the player why
    /// it's stuck.
    pub fn locks(&self) -> Vec<PathBuf> {
        locks::locks_in(self.repo.commondir())
    }

    /// Replaces the commits on `branch` after `onto` with one commit holding
    /// the same files. The new commit is written first and the branch moves to
    /// it in one step, so a crash between the two leaves the branch where it
    /// was, and the squash can simply run again.
    pub fn squash(&self, branch: &str, onto: Oid, message: &str) -> Result<Oid> {
        let tip = self.existing_tip(branch)?;
        if tip == onto {
            return Ok(tip);
        }
        let squashed = self.write_squash(tip, onto, message)?;
        self.move_branch(branch, Some(tip), squashed)?;
        Ok(squashed)
    }

    fn write_squash(&self, tip: Oid, onto: Oid, message: &str) -> Result<Oid> {
        if !self.repo.graph_descendant_of(tip, onto)? {
            return Err(GitError::Failed(format!("{tip} doesn't build on {onto}")));
        }
        let tree = self.repo.find_commit(tip)?.tree()?;
        let parent = self.repo.find_commit(onto)?;
        let signature = self.signature()?;
        let id = self
            .repo
            .commit(None, &signature, &signature, message, &tree, &[&parent])?;
        Ok(id)
    }

    /// Moves `branch` from `from` to `to` in one atomic step, or creates it if
    /// `from` is `None`. Fails, moving nothing, if it isn't at `from` any more.
    /// Only the ref moves: the folder's files stay as they are.
    pub fn move_branch(&self, branch: &str, from: Option<Oid>, to: Oid) -> Result<()> {
        let _writing = self.mark_write()?;
        let name = local_ref(branch);
        let log_message = "mudling: move branch";
        match from {
            Some(from) => self
                .repo
                .reference_matching(&name, to, true, from, log_message)?,
            None => self.repo.reference(&name, to, false, log_message)?,
        };
        Ok(())
    }

    /// Every file in `commit`, by path relative to the repo's root.
    pub fn files_at(&self, commit: Oid) -> Result<Files> {
        let mut files = Files::new();
        let tree = self.repo.find_commit(commit)?.tree()?;
        self.for_each_file(
            &tree,
            Path::new(""),
            &mut HashSet::new(),
            &mut |path, bytes| {
                files.insert(path, bytes.to_vec());
                Ok(())
            },
        )?;
        Ok(files)
    }

    /// The newest commit both sides build on, or `None` if they share no
    /// history, as two devices that each started a repo don't.
    pub fn merge_base(&self, a: Oid, b: Oid) -> Result<Option<Oid>> {
        match self.repo.merge_base(a, b) {
            Ok(base) => Ok(Some(base)),
            Err(error) if error.code() == git2::ErrorCode::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Whether `commit` is `of` or something `of` builds on.
    pub fn is_ancestor(&self, commit: Oid, of: Oid) -> Result<bool> {
        Ok(commit == of || self.repo.graph_descendant_of(of, commit)?)
    }

    /// When `commit` was made, in seconds since the Unix epoch.
    pub fn commit_time(&self, commit: Oid) -> Result<i64> {
        Ok(self.repo.find_commit(commit)?.time().seconds())
    }

    /// Writes a commit holding exactly `files`, on top of `parents`. No
    /// branch moves and the folder stays as it is.
    pub fn write_commit(&self, files: &Files, parents: &[Oid], message: &str) -> Result<Oid> {
        let mut index = Index::new()?;
        for (path, bytes) in files {
            index.add(&IndexEntry {
                ctime: IndexTime::new(0, 0),
                mtime: IndexTime::new(0, 0),
                dev: 0,
                ino: 0,
                mode: REGULAR_FILE,
                uid: 0,
                gid: 0,
                file_size: bytes.len() as u32,
                id: self.repo.blob(bytes)?,
                flags: 0,
                flags_extended: 0,
                path: git_path(path)?.into_bytes(),
            })?;
        }
        let tree = self.repo.find_tree(index.write_tree_to(&self.repo)?)?;
        let parents = parents
            .iter()
            .map(|parent| self.repo.find_commit(*parent))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let parents: Vec<&Commit> = parents.iter().collect();
        let signature = self.signature()?;
        Ok(self
            .repo
            .commit(None, &signature, &signature, message, &tree, &parents)?)
    }

    /// The messages of `branch`'s commits that `remote` lacks, newest first,
    /// leaving out merges. Empty when there's no such remote, because then
    /// there's nothing to sync to.
    pub fn unpushed_messages(&self, remote: &str, branch: &str) -> Result<Vec<String>> {
        if self.repo.find_remote(remote).is_err() {
            return Ok(Vec::new());
        }
        let mut messages = Vec::new();
        for id in self.unpushed(remote, branch)? {
            let commit = self.repo.find_commit(id)?;
            if commit.parent_count() < 2 {
                messages.push(commit.message().unwrap_or_default().to_owned());
            }
        }
        Ok(messages)
    }

    /// Every commit on `branch` the last fetch from `remote` didn't show.
    fn unpushed(&self, remote: &str, branch: &str) -> Result<Vec<Oid>> {
        let Some(tip) = self.branch_tip(branch)? else {
            return Ok(Vec::new());
        };
        let mut walk = self.repo.revwalk()?;
        walk.push(tip)?;
        if let Some(tracked) = self.tip(&tracking_ref(remote, branch))? {
            walk.hide(tracked)?;
        }
        Ok(walk.collect::<std::result::Result<_, _>>()?)
    }

    fn refuse_passwords(&self, remote: &str, branch: &str) -> Result<()> {
        let mut seen = HashSet::new();
        for id in self.unpushed(remote, branch)? {
            self.refuse_passwords_in(id, &mut seen)?;
        }
        Ok(())
    }

    fn refuse_passwords_in(&self, commit: Oid, seen: &mut HashSet<(PathBuf, Oid)>) -> Result<()> {
        let tree = self.repo.find_commit(commit)?.tree()?;
        self.for_each_file(
            &tree,
            Path::new(""),
            seen,
            &mut |path, bytes| match merge::find_password(&path, bytes) {
                Some(field) => Err(GitError::PasswordInFile {
                    path: path.display().to_string(),
                    field,
                }),
                None => Ok(()),
            },
        )
    }

    /// Takes a password the player has removed out of what `remote` would
    /// receive: if `branch` holds none now, every commit the remote lacks
    /// becomes one, so no old commit still holds it. The sessions among them
    /// fold together, which only this costs. Returns whether it did. The old
    /// commits stay on this device until git cleans them up, and are never
    /// pushed.
    pub fn fold_out_password(&self, remote: &str, branch: &str, message: &str) -> Result<bool> {
        let tip = self.existing_tip(branch)?;
        match self.refuse_passwords_in(tip, &mut HashSet::new()) {
            Err(GitError::PasswordInFile { .. }) => return Ok(false),
            result => result?,
        }
        let tree = self.repo.find_commit(tip)?.tree()?;
        let parent = self
            .tracked_tip(remote, branch)?
            .map(|id| self.repo.find_commit(id))
            .transpose()?;
        let parents: Vec<&Commit> = parent.iter().collect();
        let signature = self.signature()?;
        let folded = self
            .repo
            .commit(None, &signature, &signature, message, &tree, &parents)?;
        self.move_branch(branch, Some(tip), folded)?;
        Ok(true)
    }

    /// Calls `visit` for every file under `tree`, skipping any object already
    /// in `seen` at the same path, so a walk over many commits reads each
    /// version once. Two paths can hold one object, and each still counts.
    fn for_each_file(
        &self,
        tree: &Tree<'_>,
        folder: &Path,
        seen: &mut HashSet<(PathBuf, Oid)>,
        visit: &mut dyn FnMut(PathBuf, &[u8]) -> Result<()>,
    ) -> Result<()> {
        for entry in tree {
            let path = folder.join(entry.name()?);
            if !seen.insert((path.clone(), entry.id())) {
                continue;
            }
            match entry.kind() {
                Some(ObjectType::Tree) => {
                    let tree = self.repo.find_tree(entry.id())?;
                    self.for_each_file(&tree, &path, seen, visit)?;
                }
                Some(ObjectType::Blob) => {
                    visit(path, self.repo.find_blob(entry.id())?.content())?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn mark_write(&self) -> Result<locks::WriteMarker> {
        locks::WriteMarker::set(self.repo.commondir())
    }

    fn head_commit(&self) -> Result<Option<Commit<'_>>> {
        let Some(id) = self.tip("HEAD")? else {
            return Ok(None);
        };
        Ok(Some(self.repo.find_commit(id)?))
    }

    fn existing_tip(&self, branch: &str) -> Result<Oid> {
        self.branch_tip(branch)?
            .ok_or_else(|| GitError::Failed(format!("branch {branch} has no commits")))
    }

    /// Where a ref points, following symbolic refs.
    fn tip(&self, name: &str) -> Result<Option<Oid>> {
        match self.repo.refname_to_id(name) {
            Ok(id) => Ok(Some(id)),
            Err(error)
                if matches!(
                    error.code(),
                    git2::ErrorCode::NotFound | git2::ErrorCode::UnbornBranch
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn signature(&self) -> Result<Signature<'static>> {
        Ok(self
            .repo
            .signature()
            .or_else(|_| Signature::now(FALLBACK_NAME, FALLBACK_EMAIL))?)
    }
}

fn init(folder: &Path) -> Result<Repository> {
    fs::create_dir_all(folder)?;
    // Finder drops .DS_Store into any folder it shows, so that doesn't count.
    let holds_files = fs::read_dir(folder)?
        .flatten()
        .any(|entry| entry.file_name() != ".DS_Store");
    if holds_files {
        return Err(GitError::FolderNotEmpty);
    }
    let mut options = RepositoryInitOptions::new();
    options.initial_head(DEFAULT_BRANCH);
    Ok(Repository::init_opts(folder, &options)?)
}

/// `path` as git writes it, with `/` on every OS.
fn git_path(path: &Path) -> Result<String> {
    let parts: Option<Vec<&str>> = path.iter().map(|part| part.to_str()).collect();
    parts
        .map(|parts| parts.join("/"))
        .ok_or_else(|| GitError::Failed("a file name isn't UTF-8".into()))
}

fn local_ref(branch: &str) -> String {
    format!("refs/heads/{branch}")
}

fn tracking_ref(remote: &str, branch: &str) -> String {
    format!("refs/remotes/{remote}/{branch}")
}

fn timed_out() -> GitError {
    GitError::TimedOut("the remote or its sign-in didn't answer in time".to_owned())
}

/// A server's reason for rejecting a pushed branch.
fn push_rejected(reason: String) -> GitError {
    if LOST_RACE.iter().any(|words| reason.contains(words)) {
        GitError::NonFastForward(reason)
    } else {
        GitError::RemoteRefused(reason)
    }
}
