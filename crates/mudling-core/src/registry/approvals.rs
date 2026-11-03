//! Which plugins the player let send commands, on this device only.
//!
//! An approval never syncs. It's consent given here, so a plugin that reaches
//! this device by sync asks again, and nobody who can write to the repo can
//! grant one. Each is for one copy of the plugin, by where it runs from: the
//! data folder's copy by its content hash, since its patterns decide when its
//! hooks run and what text they get, so a change to any file of it asks
//! again, unless it's an update the player chose here. A folder the player
//! added from this device is theirs, and changes with every save, so it's
//! approved by where it is instead. A folder and an installed copy with one
//! id are two copies, each with its own approval.

use crate::plugin::Plugin;
use crate::plugin::manifest::Permission;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

const APPROVALS_FILE: &str = "plugin-approvals.json";

/// The copy of a plugin an approval is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApprovedCopy {
    /// The copy in the data folder with this content hash.
    Installed(String),
    /// The folder at this path, which the player added.
    Folder(PathBuf),
}

/// What's approved of one plugin.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Approved {
    /// The content hash of the data folder's copy, if that's approved.
    installed: Option<String>,
    /// The approved folders the player added.
    folders: BTreeSet<PathBuf>,
}

impl Approved {
    fn holds(&self, copy: &ApprovedCopy) -> bool {
        match copy {
            ApprovedCopy::Installed(hash) => self.installed.as_ref() == Some(hash),
            ApprovedCopy::Folder(path) => self.folders.contains(path),
        }
    }

    fn is_empty(&self) -> bool {
        self.installed.is_none() && self.folders.is_empty()
    }
}

#[derive(Clone, Debug)]
pub struct Approvals {
    path: PathBuf,
}

impl Approvals {
    pub fn in_data_folder(data_folder: &Path) -> Approvals {
        Approvals {
            path: data_folder.join(APPROVALS_FILE),
        }
    }

    /// What's approved, by plugin id.
    fn all(&self) -> BTreeMap<String, Approved> {
        super::read_json(&self.path)
    }

    pub fn is_approved(&self, id: &str, copy: &ApprovedCopy) -> bool {
        self.all()
            .get(id)
            .is_some_and(|approved| approved.holds(copy))
    }

    /// Approves `copy`, in place of any other copy of `id` in the data folder.
    pub fn approve(&self, id: &str, copy: &ApprovedCopy) -> io::Result<()> {
        let mut all = self.all();
        let approved = all.entry(id.to_owned()).or_default();
        match copy {
            ApprovedCopy::Installed(hash) => approved.installed = Some(hash.clone()),
            ApprovedCopy::Folder(path) => {
                approved.folders.insert(path.clone());
            }
        }
        super::write_json(&self.path, &all)
    }

    /// Takes back the approval of `copy`, and of no other copy of `id`.
    pub fn take_back(&self, id: &str, copy: &ApprovedCopy) -> io::Result<()> {
        let mut all = self.all();
        let Some(approved) = all.get_mut(id).filter(|approved| approved.holds(copy)) else {
            return Ok(());
        };
        match copy {
            ApprovedCopy::Installed(_) => approved.installed = None,
            ApprovedCopy::Folder(path) => {
                approved.folders.remove(path);
            }
        }
        if approved.is_empty() {
            all.remove(id);
        }
        super::write_json(&self.path, &all)
    }
}

/// Whether `plugin`'s hooks may send commands once approved.
pub fn sends_commands(plugin: &Plugin) -> bool {
    (plugin.manifest.permissions).contains(&Permission::SendCommands)
}
