//! The plugin directory (PRD §11): the community registry's signed index, the
//! plugins installed from it or from a link, the player's own plugin folders,
//! and the player's approvals.
//!
//! The registry's own docs say what's read here: `docs/index-format.md` and
//! `docs/signing.md` in `mudling-plugins`. Both the index and the plugins come
//! through the embedded git, from a bare [`Mirror`] in the data folder:
//!
//! - **The index** is `index.json` on the registry's `index` branch, one
//!   commit deep. Its Ed25519 signature is checked over the exact bytes of
//!   `signed` before anything in it is read, and an index older than the
//!   newest this device has seen is refused, so an old one can't be replayed
//!   to hide an update.
//! - **A plugin** is read from git's objects at the index's commit, not from a
//!   tarball or a checkout, because both apply the plugin's own
//!   `.gitattributes` (`export-ignore`, `export-subst`, line endings). Its
//!   content hash must equal the index's before a byte reaches the disk.
//! - **A plugin from a git link** comes the same way, checked as registry CI
//!   checks one rather than against an index: see [`fetch_link`].
//! - **An installed plugin** loads only as a copy this device trusts: see
//!   [`Trusted`]. Plugins sync through the repo, so one can arrive changed.
//! - **A folder the player added** loads from where it is, and only on this
//!   device: [`Origins`] keeps the list.
//!
//! Every call here waits on the disk or the network: run them off the UI
//! thread.

mod approvals;
mod folders;
mod installed;
mod link;
mod trust;

#[cfg(test)]
mod tests;

pub use approvals::{Approvals, ApprovedCopy, needs_approval};
pub use folders::AddedFolder;
pub use installed::{
    FolderCopy, InstalledPlugins, Loaded, PLUGINS_FOLDER, Record, Refused, folder_hash,
};
pub(crate) use installed::{is_working, plugin_of};
pub use link::{fetch_link, forget_link};
pub use trust::{Origins, Trusted};

use crate::git::{FolderFiles, GitError, Limit, Mirror, Oid};
use crate::plugin::check::Failure;
use crate::plugin::{Plugin, PluginError};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The registry's repo, which every device fetches from.
const REGISTRY_URL: &str = "https://github.com/Vyttle-LLC/mudling-plugins.git";

/// The public key the registry's CI signs the index with: 32 bytes in
/// base64, as `docs/signing.md` prints it. Without one, every index is
/// refused and the directory lists nothing.
pub const REGISTRY_PUBLIC_KEY: Option<&str> = Some("v00r5VUhmPRYcnxyD/Yir9WgNr2och43fasd1+1frF4=");

const INDEX_BRANCH: &str = "index";
const INDEX_FILE: &str = "index.json";
const INDEX_FORMAT: u64 = 1;
/// Under the data folder: the mirror, and the last index that checked out.
const REGISTRY_FOLDER: &str = "registry";
const MIRROR_FOLDER: &str = "mirror.git";

/// The index, once its signature has checked out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Index {
    /// The registry's commit every plugin is fetched at.
    pub commit: Oid,
    /// UTC ISO 8601, as `2026-11-02T17:04:31Z`.
    pub published_at: String,
    pub plugins: Vec<Entry>,
}

/// One plugin the index lists.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub version: String,
    pub license: String,
    pub games: Games,
    /// Its folder in the registry at the index's commit.
    pub source: String,
    /// `sha256:` and the folder's content hash: see [`content_hash`].
    pub hash: String,
    /// What its hooks need, if the index says. An index that doesn't still
    /// has the plugin's manifest asked before it installs.
    #[serde(default)]
    pub permissions: Vec<String>,
}

/// Which games a plugin is for. Both empty means any game.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Games {
    #[serde(default)]
    pub hosts: Vec<String>,
    #[serde(default)]
    pub mssp_names: Vec<String>,
}

impl Entry {
    pub fn sends_commands(&self) -> bool {
        self.permissions.iter().any(|p| p == "send_commands")
    }

    pub fn sends_gmcp(&self) -> bool {
        self.permissions.iter().any(|p| p == "send_gmcp")
    }

    /// The game it's for, as the player knows it, or `None` for any game.
    pub fn game(&self) -> Option<&str> {
        let games = &self.games;
        (games.mssp_names.first().or(games.hosts.first())).map(String::as_str)
    }
}

/// Why the directory has nothing to list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IndexError {
    /// This build has no registry key, so no index can be trusted.
    NoKey,
    Unreachable(GitError),
    /// The signature isn't the registry's.
    Forged,
    /// It's older than one this device has already seen.
    Stale {
        seen: String,
        offered: String,
    },
    UnknownFormat(u64),
    Malformed(String),
}

impl fmt::Display for IndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoKey => f.write_str(
                "This build of MUDling has no key to check the directory with, so it lists nothing.",
            ),
            Self::Unreachable(error) => write!(f, "The directory couldn't be reached: {error}."),
            Self::Forged => f.write_str(
                "The directory's signature doesn't check out, so nothing from it is shown.",
            ),
            Self::Stale { seen, offered } => write!(
                f,
                "The directory sent a list from {offered}, older than the one from {seen} \
                 already seen here, so it may be hiding an update.",
            ),
            Self::UnknownFormat(format) => write!(
                f,
                "The directory is in format {format}, which this MUDling can't read. Update MUDling to browse it.",
            ),
            Self::Malformed(why) => write!(f, "The directory's list can't be read: {why}."),
        }
    }
}

/// Why a plugin wasn't installed. Nothing reached the disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallError {
    Unreachable(GitError),
    /// What was downloaded isn't what the index signed for.
    HashMismatch {
        expected: String,
        found: String,
    },
    /// The files are the index's, but they aren't a plugin with its id.
    NotAPlugin(String),
    /// A plugin from a link that fails a check registry CI runs.
    Failed(Failure),
    Disk(String),
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable(error) => write!(f, "couldn't download it: {error}"),
            Self::HashMismatch { .. } => f.write_str(
                "what was downloaded doesn't match what the directory signed for, so it wasn't installed",
            ),
            Self::NotAPlugin(why) => write!(f, "it isn't a plugin MUDling can load: {why}"),
            Self::Failed(failure) => write!(f, "it failed a check: {failure}"),
            Self::Disk(why) => write!(f, "couldn't save it: {why}"),
        }
    }
}

/// A folder too large to be a plugin isn't one; anything else kept it from
/// being downloaded.
impl From<GitError> for InstallError {
    fn from(error: GitError) -> Self {
        match error {
            GitError::TooLarge(why) => Self::NotAPlugin(format!("it's too large: {why}")),
            error => Self::Unreachable(error),
        }
    }
}

impl From<PluginError> for InstallError {
    fn from(error: PluginError) -> Self {
        Self::NotAPlugin(error.to_string())
    }
}

/// A plugin downloaded and checked, not yet on the disk.
#[derive(Clone, Debug)]
pub struct Fetched {
    pub source: Source,
    pub commit: Oid,
    /// Its files' content hash: the index's, or the one computed for a link.
    pub hash: String,
    pub files: FolderFiles,
    pub plugin: Plugin,
}

/// Where a fetched plugin came from.
#[derive(Clone, Debug)]
pub enum Source {
    /// Listed in the signed index, and checked against its hash.
    Directory(Entry),
    /// A git link the player gave, and checked as registry CI would.
    Link(String),
}

/// The registry as this device reads it.
#[derive(Clone, Debug)]
pub struct Registry {
    folder: PathBuf,
    url: String,
    key: Option<VerifyingKey>,
    trusted: Trusted,
}

impl Registry {
    /// The community registry, kept under `data_folder`.
    pub fn new(data_folder: &Path) -> Registry {
        let key = REGISTRY_PUBLIC_KEY.and_then(|key| {
            let bytes = data_encoding::BASE64.decode(key.as_bytes()).ok()?;
            VerifyingKey::from_bytes(&bytes.try_into().ok()?).ok()
        });
        Registry::at(data_folder, REGISTRY_URL, key)
    }

    /// A registry at `url`, trusted with `key`: for tests and a local copy.
    fn at(data_folder: &Path, url: &str, key: Option<VerifyingKey>) -> Registry {
        Registry {
            folder: data_folder.join(REGISTRY_FOLDER),
            url: url.to_owned(),
            key,
            trusted: Trusted::in_data_folder(data_folder),
        }
    }

    /// Fetches the index and checks it. One that checks out becomes the
    /// newest seen; one that doesn't changes nothing here.
    pub fn refresh(&self, limit: &Limit) -> Result<Index, IndexError> {
        let key = self.key.ok_or(IndexError::NoKey)?;
        let mirror = self.mirror().map_err(IndexError::Unreachable)?;
        let tip = mirror
            .fetch_branch(INDEX_BRANCH, limit)
            .map_err(IndexError::Unreachable)?;
        let bytes = mirror
            .file(tip, INDEX_FILE)
            .map_err(IndexError::Unreachable)?
            .ok_or_else(|| IndexError::Malformed(format!("the branch has no {INDEX_FILE}")))?;
        let seen = self.newest_seen(&key);
        let index = verify(&bytes, &key, seen.as_deref())?;
        let keep = |error: io::Error| IndexError::Malformed(format!("couldn't keep it: {error}"));
        crate::repo::write_atomically(&self.folder.join(INDEX_FILE), &bytes).map_err(keep)?;
        // A copy the registry signed for is one this device can run.
        self.trusted.trust_index(&index.plugins).map_err(keep)?;
        Ok(index)
    }

    /// When the newest index seen was published, if one has checked out.
    fn newest_seen(&self, key: &VerifyingKey) -> Option<String> {
        let bytes = fs::read(self.folder.join(INDEX_FILE)).ok()?;
        verify(&bytes, key, None)
            .ok()
            .map(|index| index.published_at)
    }

    /// Downloads `entry` at `index`'s commit and checks it against its hash.
    pub fn fetch(
        &self,
        index: &Index,
        entry: &Entry,
        limit: &Limit,
    ) -> Result<Fetched, InstallError> {
        let mirror = self.mirror()?;
        mirror.fetch_commit(index.commit, limit)?;
        let files = mirror.folder_files(index.commit, &entry.source)?;
        let found = content_hash(&files);
        if found != entry.hash {
            return Err(InstallError::HashMismatch {
                expected: entry.hash.clone(),
                found,
            });
        }
        let plugin = load_files(&files)?;
        if plugin.id().as_str() != entry.id {
            return Err(InstallError::NotAPlugin(format!(
                "its id is {}, not {}",
                plugin.id(),
                entry.id
            )));
        }
        Ok(Fetched {
            source: Source::Directory(entry.clone()),
            commit: index.commit,
            hash: found,
            files,
            plugin,
        })
    }

    fn mirror(&self) -> Result<Mirror, GitError> {
        Mirror::open(&self.folder.join(MIRROR_FOLDER), &self.url)
    }
}

/// Reads an index file in the order `docs/index-format.md` sets: the
/// signature over `signed`'s exact bytes first, and only then what it says.
fn verify(file: &[u8], key: &VerifyingKey, seen: Option<&str>) -> Result<Index, IndexError> {
    #[derive(Deserialize)]
    struct Outer {
        signed: String,
        signature: String,
    }
    #[derive(Deserialize)]
    struct Signed {
        commit: String,
        published_at: String,
        plugins: Vec<Entry>,
    }
    let outer: Outer =
        serde_json::from_slice(file).map_err(|error| IndexError::Malformed(error.to_string()))?;
    let signature = data_encoding::BASE64
        .decode(outer.signature.as_bytes())
        .ok()
        .and_then(|bytes| Signature::from_slice(&bytes).ok())
        .ok_or(IndexError::Forged)?;
    key.verify_strict(outer.signed.as_bytes(), &signature)
        .map_err(|_| IndexError::Forged)?;

    #[derive(Deserialize)]
    struct Format {
        format: u64,
    }
    let format: Format = serde_json::from_str(&outer.signed)
        .map_err(|error| IndexError::Malformed(error.to_string()))?;
    if format.format != INDEX_FORMAT {
        return Err(IndexError::UnknownFormat(format.format));
    }
    let signed: Signed = serde_json::from_str(&outer.signed)
        .map_err(|error| IndexError::Malformed(error.to_string()))?;
    if !is_utc_time(&signed.published_at) {
        return Err(IndexError::Malformed(format!(
            "published_at {} isn't a UTC time",
            signed.published_at
        )));
    }
    // The fixed format sorts as text in the order of time.
    if let Some(seen) = seen.filter(|seen| signed.published_at.as_str() < *seen) {
        return Err(IndexError::Stale {
            seen: seen.to_owned(),
            offered: signed.published_at,
        });
    }
    let commit = Oid::from_str(&signed.commit)
        .ok()
        .filter(|_| signed.commit.len() == 40)
        .ok_or_else(|| IndexError::Malformed(format!("{} isn't a commit", signed.commit)))?;
    Ok(Index {
        commit,
        published_at: signed.published_at,
        plugins: signed.plugins,
    })
}

/// `YYYY-MM-DDTHH:MM:SSZ`, which is all CI writes.
fn is_utc_time(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 20
        && bytes.iter().enumerate().all(|(at, &byte)| match at {
            4 | 7 => byte == b'-',
            10 => byte == b'T',
            13 | 16 => byte == b':',
            19 => byte == b'Z',
            _ => byte.is_ascii_digit(),
        })
}

/// A folder's content hash, as `docs/index-format.md` defines it: a line per
/// file, `sha256sum`'s way, sorted by path, and the SHA-256 of those lines.
pub fn content_hash(files: &FolderFiles) -> String {
    let mut listing = Sha256::new();
    for (path, bytes) in files {
        listing.update(format!("{}  {path}\n", hex(&Sha256::digest(bytes))));
    }
    format!("sha256:{}", hex(&listing.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    data_encoding::HEXLOWER.encode(bytes)
}

/// Loads a plugin from files held in memory.
fn load_files(files: &FolderFiles) -> Result<Plugin, PluginError> {
    Plugin::from_files(|name| {
        let bytes = files
            .get(name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such file"))?;
        String::from_utf8(bytes.clone())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not UTF-8"))
    })
}

/// What the JSON file at `path` holds, or the default if it's missing or
/// can't be read.
fn read_json<T: DeserializeOwned + Default>(path: &Path) -> T {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Writes `value` to `path` as JSON, whole or not at all.
fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    crate::repo::write_atomically(path, &bytes)
}
