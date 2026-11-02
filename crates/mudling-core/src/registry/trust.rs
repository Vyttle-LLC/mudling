//! Which copies of a plugin this device trusts to load, kept on this device
//! only, so nothing that can write to the repo can add to it.
//!
//! A copy is trusted by its content hash: one this device installed itself,
//! one an index this device verified listed, or one the player chose to
//! trust. A plugin that arrives by sync in any other shape stays unloaded
//! until the player reviews it.

use super::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const TRUST_FILE: &str = "plugin-trust.json";

#[derive(Clone, Debug)]
pub struct Trusted {
    path: PathBuf,
}

impl Trusted {
    pub fn in_data_folder(data_folder: &Path) -> Trusted {
        Trusted {
            path: data_folder.join(TRUST_FILE),
        }
    }

    /// Each plugin's id, with the hashes trusted for it.
    fn all(&self) -> BTreeMap<String, BTreeSet<String>> {
        fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn trusts(&self, id: &str, hash: &str) -> bool {
        self.all()
            .get(id)
            .is_some_and(|hashes| hashes.contains(hash))
    }

    pub fn trust(&self, id: &str, hash: &str) -> io::Result<()> {
        self.trust_all([(id, hash)])
    }

    /// Trusts every copy a verified index lists.
    pub fn trust_index(&self, entries: &[Entry]) -> io::Result<()> {
        self.trust_all(
            entries
                .iter()
                .map(|entry| (entry.id.as_str(), entry.hash.as_str())),
        )
    }

    fn trust_all<'a>(
        &self,
        copies: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> io::Result<()> {
        let mut all = self.all();
        let mut added = false;
        for (id, hash) in copies {
            added |= all
                .entry(id.to_owned())
                .or_default()
                .insert(hash.to_owned());
        }
        if !added {
            return Ok(());
        }
        let bytes = serde_json::to_vec_pretty(&all).expect("trusted hashes serialize");
        super::save(&self.path, &bytes)
    }
}
