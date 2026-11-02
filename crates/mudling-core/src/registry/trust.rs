//! Which copies of a plugin this device trusts to load, kept on this device
//! only, so nothing that can write to the repo can add to it.
//!
//! A copy is trusted by its content hash: one this device installed itself,
//! one an index this device verified listed, or one the player chose to
//! trust. A plugin that arrives by sync in any other shape stays unloaded
//! until the player reviews it.
//!
//! A folder with no record is the player's own only if it began on this
//! device: [`Origins`] remembers which did, and which sync wrote.

use super::Entry;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const TRUST_FILE: &str = "plugin-trust.json";
const ORIGINS_FILE: &str = "plugin-origins.json";

/// Where this device's plugin folders with no record began, by id.
#[derive(Clone, Debug)]
pub struct Origins {
    path: PathBuf,
}

#[derive(Default, Serialize, Deserialize)]
struct Known {
    /// Made on this device: the player's own.
    local: BTreeSet<String>,
    /// Written by sync, so from another device or the repo.
    arrived: BTreeSet<String>,
}

impl Origins {
    pub fn in_data_folder(data_folder: &Path) -> Origins {
        Origins {
            path: data_folder.join(ORIGINS_FILE),
        }
    }

    fn known(&self) -> Known {
        fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self, known: &Known) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(known).expect("origins serialize");
        super::save(&self.path, &bytes)
    }

    /// Whether the folder `id` is the player's own. One never seen before,
    /// and not written by sync, began here, and is remembered as such.
    pub fn is_local(&self, id: &str) -> io::Result<bool> {
        let mut known = self.known();
        if known.arrived.contains(id) {
            return Ok(false);
        }
        if known.local.insert(id.to_owned()) {
            self.save(&known)?;
        }
        Ok(true)
    }

    /// Sync is about to write these plugins' files. Call it before writing,
    /// so a crash between the two can't pass them off as the player's own.
    pub fn mark_arrived<'a>(&self, ids: impl IntoIterator<Item = &'a str>) -> io::Result<()> {
        let mut known = self.known();
        let mut changed = false;
        for id in ids {
            changed |= known.local.remove(id);
            changed |= known.arrived.insert(id.to_owned());
        }
        if changed { self.save(&known) } else { Ok(()) }
    }

    /// The player chose this folder as their own.
    pub fn mark_local(&self, id: &str) -> io::Result<()> {
        let mut known = self.known();
        known.arrived.remove(id);
        known.local.insert(id.to_owned());
        self.save(&known)
    }

    pub fn forget(&self, id: &str) -> io::Result<()> {
        let mut known = self.known();
        if known.arrived.remove(id) | known.local.remove(id) {
            self.save(&known)?;
        }
        Ok(())
    }
}

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
