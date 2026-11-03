//! A plugin folder the player added from this device: it loads from where
//! it is, again whenever a file in it changes, and never reaches the repo.
//! A reload that breaks it leaves the last copy that loaded running, and
//! says why.

use crate::persist::Timestamp;
use crate::plugin::Plugin;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct AddedFolder {
    pub path: PathBuf,
    /// The last copy that loaded, which runs until another loads.
    pub plugin: Option<Plugin>,
    /// When that copy loaded.
    pub loaded_at: Option<Timestamp>,
    /// Why the folder's files as they are now don't load.
    pub problem: Option<String>,
}

impl AddedFolder {
    /// Reads the plugin in `path`, which needn't load.
    pub fn read(path: &Path) -> AddedFolder {
        let unread = AddedFolder {
            path: path.to_owned(),
            plugin: None,
            loaded_at: None,
            problem: None,
        };
        unread.reloaded()
    }

    /// Reads the folder again. If it doesn't load, the last copy that did
    /// stays.
    pub fn reloaded(&self) -> AddedFolder {
        match Plugin::load(&self.path) {
            Ok(plugin) => AddedFolder {
                path: self.path.clone(),
                plugin: Some(plugin),
                loaded_at: Some(Timestamp::now()),
                problem: None,
            },
            Err(error) => AddedFolder {
                problem: Some(crate::plugin::check::one_line(&error.to_string())),
                ..self.clone()
            },
        }
    }

    /// What an approval of it is kept by. It's the player's own folder on
    /// this device, so it's approved by where it is, not by its content,
    /// which changes with every save.
    pub fn approval_key(&self) -> String {
        format!("folder:{}", self.path.display())
    }
}
