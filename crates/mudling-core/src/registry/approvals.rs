//! Which plugins the player let send commands, on this device only.
//!
//! An approval never syncs. It's consent given here, so a plugin that reaches
//! this device by sync asks again, and nobody who can write to the repo can
//! grant one. Each is for the plugin's [`fingerprint`]: its permissions and
//! hooks, the only parts that can send. So a plugin whose hooks change by any
//! means but an update the player chose here asks again too.

use crate::plugin::Plugin;
use crate::plugin::manifest::Permission;
use sha2::{Digest, Sha256};
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

    /// Each approved plugin's id, with the fingerprint it was approved at.
    fn all(&self) -> BTreeMap<String, String> {
        fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// The plugins of `plugins` approved as they are now.
    pub fn approved<'a>(&self, plugins: impl IntoIterator<Item = &'a Plugin>) -> Vec<&'a Plugin> {
        let all = self.all();
        let holds = |plugin: &&Plugin| all.get(plugin.id().as_str()) == Some(&fingerprint(plugin));
        plugins.into_iter().filter(holds).collect()
    }

    pub fn is_approved(&self, plugin: &Plugin) -> bool {
        !self.approved([plugin]).is_empty()
    }

    pub fn approve(&self, plugin: &Plugin) -> io::Result<()> {
        let mut all = self.all();
        all.insert(plugin.id().to_string(), fingerprint(plugin));
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

/// What an approval is for: the plugin's permissions and its hook files.
pub fn fingerprint(plugin: &Plugin) -> String {
    let mut hash = Sha256::new();
    hash.update(format!("{:?}\n", plugin.manifest.permissions));
    for script in &plugin.scripts {
        hash.update(format!("{}\0{}\0", script.file, script.source));
    }
    format!("sha256:{}", super::hex(&hash.finalize()))
}
