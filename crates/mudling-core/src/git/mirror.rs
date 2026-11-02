//! A bare copy of a public repo, such as the plugin registry: fetched without
//! signing in, and read straight from git's objects, so no checkout and no
//! `.gitattributes` ever changes a byte that's read.

use super::{GitError, Limit, Oid, Result, on_own_thread};
use git2::{FetchOptions, FileMode, ObjectType, Repository, Tree};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where a fetched branch is kept, beside nothing else.
const FETCHED: &str = "refs/mirror";

/// A folder's files, by path relative to it with `/` between names.
pub type FolderFiles = BTreeMap<String, Vec<u8>>;

pub struct Mirror {
    folder: PathBuf,
    url: String,
}

impl Mirror {
    /// The mirror of `url` kept in `folder`, which is made if it's missing.
    pub fn open(folder: &Path, url: &str) -> Result<Mirror> {
        if Repository::open_bare(folder).is_err() {
            Repository::init_bare(folder)?;
        }
        Ok(Mirror {
            folder: folder.to_owned(),
            url: url.to_owned(),
        })
    }

    /// Fetches `branch`'s tip alone, with no history, and returns it.
    pub fn fetch_branch(&self, branch: &str, limit: &Limit) -> Result<Oid> {
        let tracking = format!("{FETCHED}/{branch}");
        self.fetch(format!("+refs/heads/{branch}:{tracking}"), limit)?;
        let repo = Repository::open_bare(&self.folder)?;
        Ok(repo.refname_to_id(&tracking)?)
    }

    /// Fetches the tip of the branch the remote's `HEAD` names, its default,
    /// with no history, and returns it.
    pub fn fetch_default_branch(&self, limit: &Limit) -> Result<Oid> {
        let tracking = format!("{FETCHED}/HEAD");
        self.fetch(format!("+HEAD:{tracking}"), limit)?;
        let repo = Repository::open_bare(&self.folder)?;
        Ok(repo.refname_to_id(&tracking)?)
    }

    /// Fetches `commit` alone, by its id, unless the mirror has it already.
    pub fn fetch_commit(&self, commit: Oid, limit: &Limit) -> Result<()> {
        let repo = Repository::open_bare(&self.folder)?;
        if repo.find_commit(commit).is_ok() {
            return Ok(());
        }
        self.fetch(format!("+{commit}:{FETCHED}/commit"), limit)
    }

    fn fetch(&self, refspec: String, limit: &Limit) -> Result<()> {
        let url = self.url.clone();
        let shallow = !url.starts_with("file:") && !Path::new(&url).exists();
        on_own_thread(self.folder.clone(), limit, move |repo| {
            let mut options = FetchOptions::new();
            // libgit2 fetches only what the remote's protocol can trim, so
            // a folder on this device is fetched whole.
            if shallow {
                options.depth(1);
            }
            repo.remote_anonymous(&url)?
                .fetch(&[refspec], Some(&mut options), None)?;
            Ok(())
        })
    }

    /// The bytes of the file at `path` in `commit`, if it's there.
    pub fn file(&self, commit: Oid, path: &str) -> Result<Option<Vec<u8>>> {
        let repo = Repository::open_bare(&self.folder)?;
        let tree = repo.find_commit(commit)?.tree()?;
        let Ok(entry) = tree.get_path(Path::new(path)) else {
            return Ok(None);
        };
        Ok(Some(repo.find_blob(entry.id())?.content().to_vec()))
    }

    /// Every file under `folder` in `commit`, as git stores it, or under the
    /// repo's root for an empty `folder`. A symbolic link or a submodule is
    /// refused, since neither is a file to hash.
    pub fn folder_files(&self, commit: Oid, folder: &str) -> Result<FolderFiles> {
        let repo = Repository::open_bare(&self.folder)?;
        let root = repo.find_commit(commit)?.tree()?;
        let tree = if folder.is_empty() {
            root
        } else {
            let entry = root
                .get_path(Path::new(folder))
                .map_err(|_| GitError::Failed(format!("the repo has no folder {folder}")))?;
            repo.find_tree(entry.id())
                .map_err(|_| GitError::Failed(format!("{folder} isn't a folder")))?
        };
        let mut files = FolderFiles::new();
        collect(&repo, &tree, "", &mut files)?;
        Ok(files)
    }
}

fn collect(
    repo: &Repository,
    tree: &Tree<'_>,
    prefix: &str,
    files: &mut FolderFiles,
) -> Result<()> {
    for entry in tree {
        let name = entry
            .name()
            .map_err(|_| GitError::Failed("a file name isn't UTF-8".into()))?;
        let path = format!("{prefix}{name}");
        match (entry.kind(), entry.filemode()) {
            (Some(ObjectType::Tree), _) => {
                collect(
                    repo,
                    &repo.find_tree(entry.id())?,
                    &format!("{path}/"),
                    files,
                )?;
            }
            (Some(ObjectType::Blob), mode) if mode == i32::from(FileMode::Link) => {
                return Err(GitError::Failed(format!("{path} is a symbolic link")));
            }
            (Some(ObjectType::Blob), _) => {
                files.insert(path, repo.find_blob(entry.id())?.content().to_vec());
            }
            _ => return Err(GitError::Failed(format!("{path} isn't a file"))),
        }
    }
    Ok(())
}
