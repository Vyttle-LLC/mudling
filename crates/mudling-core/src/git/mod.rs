//! Embedded git: libgit2 with everything vendored, so sync works with nothing
//! installed and no system `git` is ever run (PRD §10).
//!
//! [`Repo`] offers what sync needs and no more. It holds the repo for its
//! whole life, so one sync thread should own it. Every step is safe to repeat:
//! a failure is a typed [`GitError`] and loses nothing, and a crash between
//! steps leaves the repo valid.

mod credentials;
mod error;
mod locks;
#[cfg(unix)]
mod ssh_agent;

#[cfg(test)]
mod tests;

pub use error::GitError;
pub use git2::Oid;

use git2::{
    Commit, FetchOptions, IndexAddOption, PushOptions, Repository, RepositoryInitOptions, Signature,
};
use std::fs::{self, File};
use std::path::Path;

pub type Result<T> = std::result::Result<T, GitError>;

const DEFAULT_BRANCH: &str = "main";
/// Commits made on a machine with no git identity set up.
const FALLBACK_NAME: &str = "MUDling";
const FALLBACK_EMAIL: &str = "mudling@localhost";

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

    /// Fetches `branch` from `remote`, returning its tip there, or `None` if
    /// the remote doesn't have it yet. Local branches don't move.
    pub fn fetch(&self, remote: &str, branch: &str) -> Result<Option<Oid>> {
        let _writing = self.mark_write()?;
        let tracking = tracking_ref(remote, branch);
        let refspec = format!("+{}:{tracking}", local_ref(branch));
        let mut options = FetchOptions::new();
        options.remote_callbacks(credentials::machine_callbacks(self.repo.config()?));
        self.repo
            .find_remote(remote)?
            .fetch(&[refspec], Some(&mut options), None)?;
        self.tip(&tracking)
    }

    /// Pushes `branch` to `remote`, which takes it in one atomic step or not at
    /// all. Fails with [`GitError::NonFastForward`] if the remote is ahead.
    pub fn push(&self, remote: &str, branch: &str) -> Result<()> {
        let _writing = self.mark_write()?;
        let name = local_ref(branch);
        let mut rejection = None;
        let mut callbacks = credentials::machine_callbacks(self.repo.config()?);
        callbacks.push_update_reference(|_, status| {
            rejection = status.map(str::to_owned);
            Ok(())
        });
        let mut options = PushOptions::new();
        options.remote_callbacks(callbacks);
        self.repo
            .find_remote(remote)?
            .push(&[format!("{name}:{name}")], Some(&mut options))?;
        drop(options);
        rejection.map_or(Ok(()), |reason| Err(push_rejected(reason)))
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

fn local_ref(branch: &str) -> String {
    format!("refs/heads/{branch}")
}

fn tracking_ref(remote: &str, branch: &str) -> String {
    format!("refs/remotes/{remote}/{branch}")
}

/// A server's reason for rejecting a pushed branch.
fn push_rejected(reason: String) -> GitError {
    if reason.contains("non-fast-forward") || reason.contains("fetch first") {
        GitError::NonFastForward(reason)
    } else {
        GitError::RemoteRefused(reason)
    }
}
