//! A bare copy of a public repo, such as the plugin registry: fetched without
//! signing in, and read straight from git's objects, so no checkout and no
//! `.gitattributes` ever changes a byte that's read.
//!
//! What's fetched comes from a server nobody here vouches for, so a fetch can
//! be capped, it stops itself once its [`Limit`] is up, and a folder is read
//! only while it stays within [`FOLDER_LIMITS`].

use super::{GitError, Limit, Oid, Result, on_own_thread};
use git2::{FetchOptions, FileMode, ObjectType, RemoteCallbacks, Repository};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Where a fetched branch is kept, beside nothing else.
const FETCHED: &str = "refs/mirror";
const MB: u64 = 1024 * 1024;

/// A folder's files, by path relative to it with `/` between names.
pub type FolderFiles = BTreeMap<String, Vec<u8>>;

/// How big a folder may be read: generous for a plugin, which is a few
/// small text files, and small enough that no repo can fill the memory or
/// the stack.
pub struct FolderLimits {
    /// Folders inside folders.
    pub depth: usize,
    pub files: usize,
    pub bytes: u64,
    pub largest_file: u64,
}

pub const FOLDER_LIMITS: FolderLimits = FolderLimits {
    depth: 16,
    files: 1_000,
    bytes: 8 * MB,
    largest_file: 2 * MB,
};

pub struct Mirror {
    folder: PathBuf,
    url: String,
    /// The most a fetch may download, if there's a cap.
    most_bytes: Option<u64>,
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
            most_bytes: None,
        })
    }

    /// Stops any fetch that downloads more than `bytes`.
    pub fn capped(self, bytes: u64) -> Mirror {
        Mirror {
            most_bytes: Some(bytes),
            ..self
        }
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

    /// Fetches `refspec`. Each step of the download checks the cap and the
    /// limit, so the git thread stops too, not just the wait for it.
    fn fetch(&self, refspec: String, limit: &Limit) -> Result<()> {
        let url = self.url.clone();
        let shallow = !url.starts_with("file:") && !Path::new(&url).exists();
        let (most_bytes, stop_at) = (self.most_bytes, limit.clone());
        on_own_thread(self.folder.clone(), limit, move |repo| {
            let over = Arc::new(AtomicBool::new(false));
            let mut callbacks = RemoteCallbacks::new();
            let over_cap = Arc::clone(&over);
            callbacks.transfer_progress(move |progress| {
                let downloaded = progress.received_bytes() as u64;
                if most_bytes.is_some_and(|most| downloaded > most) {
                    over_cap.store(true, Ordering::Relaxed);
                    return false;
                }
                !stop_at.is_up()
            });
            let mut options = FetchOptions::new();
            options.remote_callbacks(callbacks);
            // libgit2 fetches only what the remote's protocol can trim, so
            // a folder on this device is fetched whole.
            if shallow {
                options.depth(1);
            }
            let fetched = repo
                .remote_anonymous(&url)?
                .fetch(&[refspec], Some(&mut options), None);
            if over.load(Ordering::Relaxed) {
                let most = most_bytes.unwrap_or_default() / MB;
                return Err(GitError::TooLarge(format!(
                    "it's more than {most} MB to download"
                )));
            }
            Ok(fetched?)
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
    /// refused, since neither is a file to hash, and so is a folder past
    /// [`FOLDER_LIMITS`], before it's read whole.
    pub fn folder_files(&self, commit: Oid, folder: &str) -> Result<FolderFiles> {
        let repo = Repository::open_bare(&self.folder)?;
        let root = repo.find_commit(commit)?.tree()?;
        let tree = if folder.is_empty() {
            root.id()
        } else {
            let entry = root
                .get_path(Path::new(folder))
                .map_err(|_| GitError::Failed(format!("the repo has no folder {folder}")))?;
            repo.find_tree(entry.id())
                .map_err(|_| GitError::Failed(format!("{folder} isn't a folder")))?
                .id()
        };
        collect(&repo, tree, &FOLDER_LIMITS)
    }
}

/// The files under the tree `root`, read one folder at a time from a list
/// rather than by recursion, so no depth of folders can use up the stack.
fn collect(repo: &Repository, root: Oid, limits: &FolderLimits) -> Result<FolderFiles> {
    let too_large = |why: String| GitError::TooLarge(why);
    let odb = repo.odb()?;
    let mut files = FolderFiles::new();
    let mut bytes = 0;
    let mut folders = vec![(root, String::new(), 0)];
    while let Some((tree, prefix, depth)) = folders.pop() {
        if depth > limits.depth {
            return Err(too_large(format!(
                "its folders go more than {} deep",
                limits.depth
            )));
        }
        for entry in repo.find_tree(tree)?.iter() {
            let name = entry
                .name()
                .map_err(|_| GitError::Failed("a file name isn't UTF-8".into()))?;
            let path = format!("{prefix}{name}");
            match (entry.kind(), entry.filemode()) {
                (Some(ObjectType::Tree), _) => {
                    folders.push((entry.id(), format!("{path}/"), depth + 1));
                }
                (Some(ObjectType::Blob), mode) if mode == i32::from(FileMode::Link) => {
                    return Err(GitError::Failed(format!("{path} is a symbolic link")));
                }
                (Some(ObjectType::Blob), _) => {
                    // The size is read from the object's header, so a huge
                    // file is refused without being read.
                    let (size, _) = odb.read_header(entry.id())?;
                    let size = size as u64;
                    bytes += size;
                    if size > limits.largest_file {
                        return Err(too_large(format!(
                            "{path} is more than {} MB",
                            limits.largest_file / MB
                        )));
                    }
                    if bytes > limits.bytes {
                        return Err(too_large(format!(
                            "its files come to more than {} MB",
                            limits.bytes / MB
                        )));
                    }
                    if files.len() == limits.files {
                        return Err(too_large(format!(
                            "it has more than {} files",
                            limits.files
                        )));
                    }
                    files.insert(path, repo.find_blob(entry.id())?.content().to_vec());
                }
                _ => return Err(GitError::Failed(format!("{path} isn't a file"))),
            }
        }
    }
    Ok(files)
}
