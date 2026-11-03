//! Which plugins the player let send commands, on this device only.
//!
//! An approval never syncs. It's consent given here, so a plugin that reaches
//! this device by sync asks again, and nobody who can write to the repo can
//! grant one. Each is for one copy of the plugin, by its folder's content
//! hash: its patterns decide when its hooks run and what text they get, so
//! a change to any file of it asks again, unless it's an update the player
//! chose here. A folder the player added from this device is theirs, and
//! changes with every save, so it's approved by where it is instead.

use crate::plugin::Plugin;
use crate::plugin::manifest::Permission;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const APPROVALS_FILE: &str = "plugin-approvals.json";

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

    /// Each approved plugin's id, with the content hash it was approved at,
    /// or [`super::AddedFolder::approval_key`] for a folder the player added.
    fn all(&self) -> BTreeMap<String, String> {
        fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Whether the copy of `id` with content hash `hash` is approved.
    pub fn is_approved(&self, id: &str, hash: &str) -> bool {
        self.all().get(id).is_some_and(|approved| approved == hash)
    }

    pub fn approve(&self, id: &str, hash: &str) -> io::Result<()> {
        let mut all = self.all();
        all.insert(id.to_owned(), hash.to_owned());
        self.save(&all)
    }

    pub fn take_back(&self, id: &str) -> io::Result<()> {
        let mut all = self.all();
        if all.remove(id).is_none() {
            return Ok(());
        }
        self.save(&all)
    }

    fn save(&self, all: &BTreeMap<String, String>) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(all).expect("approvals serialize");
        super::save(&self.path, &bytes)
    }
}

/// Whether `plugin`'s hooks may send commands once approved.
pub fn sends_commands(plugin: &Plugin) -> bool {
    (plugin.manifest.permissions).contains(&Permission::SendCommands)
}
