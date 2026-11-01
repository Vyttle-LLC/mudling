//! Where a game comes from: the saved servers, the plugins, and the session
//! opened for a server. Everything here that isn't a plain getter blocks on
//! the disk, so the shell calls it off the UI thread.

use crate::room_panel::Verbs;
use crate::settings::PluginRow;
use crate::shell::Game;
use mudling_core::logs::Logs;
use mudling_core::persist;
use mudling_core::plugin::{Plugin, PluginId};
use mudling_core::session::{Session, SessionConfig};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Under the data folder, one folder per installed plugin.
const PLUGINS_FOLDER: &str = "plugins";

/// A server to connect to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub host: String,
    pub port: u16,
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.host, self.port)
    }
}

#[derive(Clone)]
pub struct Launcher {
    data_folder: PathBuf,
    plugins: Vec<Plugin>,
    /// Plugins the command line turned on, which may also send commands.
    command_line: Vec<PluginId>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Launcher {
    pub fn new(
        data_folder: PathBuf,
        plugins: Vec<Plugin>,
        command_line: Vec<PluginId>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Launcher {
        Launcher {
            data_folder,
            plugins,
            command_line,
            wake: Arc::new(wake),
        }
    }

    pub fn logs(&self) -> Logs {
        Logs::in_data_folder(&self.data_folder)
    }

    pub fn saved(&self) -> persist::SavedServers {
        persist::saved_servers(&self.data_folder)
    }

    pub fn save(&self, target: &Target) -> Result<(), String> {
        persist::save_server(&self.data_folder, &target.host, target.port)
            .map_err(|error| format!("couldn't save {target}: {error}"))
    }

    /// Takes `host` off the saved list. See [`persist::remove_server`] for what
    /// stays.
    pub fn remove(&self, host: &str) -> Result<(), String> {
        persist::remove_server(&self.data_folder, host)
            .map_err(|error| format!("couldn't remove {host}: {error}"))
    }

    /// Opens a session on `target`, with the plugins saved for it. `turn_on`
    /// is saved among them first: the core reads a session's plugins when it
    /// opens, so the player's click takes effect through a new session.
    pub fn open(&self, target: &Target, turn_on: Option<&PluginId>) -> Result<Game, String> {
        let could_not = |error: persist::Error| format!("couldn't open {target}: {error}");
        let mut saved =
            persist::enabled_plugins(&self.data_folder, &target.host).map_err(could_not)?;
        if let Some(plugin) = turn_on.filter(|plugin| !saved.iter().any(|id| id == plugin.as_str()))
        {
            saved.push(plugin.as_str().to_owned());
            persist::save_enabled_plugins(&self.data_folder, &target.host, &saved)
                .map_err(could_not)?;
        }
        let enabled: Vec<PluginId> = self
            .plugins
            .iter()
            .map(Plugin::id)
            .filter(|plugin| {
                self.command_line.contains(plugin) || saved.iter().any(|id| id == plugin.as_str())
            })
            .cloned()
            .collect();
        let config = SessionConfig {
            plugins: self.plugins.clone(),
            enabled: enabled.clone(),
            approved: self.command_line.clone(),
            ..SessionConfig::new(&self.data_folder, &target.host, target.port)
        };
        let wake = Arc::clone(&self.wake);
        let session = Session::open(config, move || wake())
            .map_err(|error| format!("couldn't start the session: {error}"))?;
        Ok(Game {
            session,
            replay_folder: None,
            verbs: Verbs::of(&self.plugins, &enabled),
            plugins: PluginRow::for_session(&self.plugins, &enabled, &target.host),
            enabled,
            target: target.clone(),
        })
    }

    /// The name the plugin gives itself.
    pub fn plugin_name(&self, id: &PluginId) -> Option<&str> {
        let plugin = self.plugins.iter().find(|plugin| plugin.id() == id)?;
        Some(&plugin.manifest.name)
    }
}

/// The plugins that ship inside the app. Compiled in rather than copied to the
/// data folder on first run: nothing can go stale across an update or be left
/// half copied, and the player's folder stays theirs. They're installed, not
/// on: detection suggests one, and the player turns it on.
pub fn bundled_plugins() -> Vec<Plugin> {
    vec![mudling_plugin_avatar::bundled()]
}

/// `plugins`, with those in `replacing` taking the place of any with their id.
pub fn replaced(mut plugins: Vec<Plugin>, replacing: Vec<Plugin>) -> Vec<Plugin> {
    plugins.retain(|plugin| !replacing.iter().any(|other| other.id() == plugin.id()));
    plugins.extend(replacing);
    plugins
}

/// The plugins installed in the data folder, one folder each. One that won't
/// load is left out, with a line on stderr, so it can't stop the app.
pub fn installed_plugins(data_folder: &Path) -> Vec<Plugin> {
    let Ok(folders) = std::fs::read_dir(data_folder.join(PLUGINS_FOLDER)) else {
        return Vec::new();
    };
    folders
        .flatten()
        .filter(|folder| folder.path().is_dir())
        .filter_map(|folder| {
            Plugin::load(folder.path())
                .inspect_err(|error| eprintln!("skipping a plugin: {error}"))
                .ok()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn a_plugin_the_player_turns_on_stays_on_for_that_server() {
        let folder = TempDir::new().unwrap();
        let plugins = bundled_plugins();
        let id = plugins[0].id().clone();
        let launcher = Launcher::new(folder.path().to_owned(), plugins, Vec::new(), || {});
        let target = Target {
            host: "mud.example".to_owned(),
            port: 4000,
        };

        let game = launcher.open(&target, None).unwrap();
        assert!(game.enabled.is_empty(), "detection turns nothing on");
        game.session.quit().wait().unwrap();

        let game = launcher.open(&target, Some(&id)).unwrap();
        assert_eq!(game.enabled, std::slice::from_ref(&id));
        game.session.quit().wait().unwrap();

        let game = launcher.open(&target, None).unwrap();
        assert_eq!(game.enabled, [id]);
        game.session.quit().wait().unwrap();
    }

    #[test]
    fn a_bundled_plugin_is_installed_and_a_folder_with_its_id_replaces_it() {
        let bundled = bundled_plugins();
        let game = bundled[0].id().as_str().to_owned();
        assert!(!bundled.is_empty());

        let folder = TempDir::new().unwrap();
        let mine = folder.path().join(PLUGINS_FOLDER).join("mine");
        std::fs::create_dir_all(&mine).unwrap();
        let manifest = format!(
            "id = \"{game}\"\nname = \"Mine\"\nversion = \"1.0.0\"\napi = 1\n\
             license = \"MIT\"\nlayer = \"game\"\n[servers]\nhosts = [\"mud.example\"]\n"
        );
        std::fs::write(mine.join("plugin.toml"), manifest).unwrap();

        let plugins = replaced(bundled, installed_plugins(folder.path()));
        let named: Vec<_> = plugins.iter().filter(|p| p.id().as_str() == game).collect();
        assert_eq!(named.len(), 1);
        assert_eq!(named[0].manifest.name, "Mine");
    }

    #[test]
    fn a_plugin_folder_that_will_not_load_leaves_the_others_installed() {
        let folder = TempDir::new().unwrap();
        assert!(installed_plugins(folder.path()).is_empty());

        let plugins = folder.path().join(PLUGINS_FOLDER);
        let good = plugins.join("good");
        std::fs::create_dir_all(&good).unwrap();
        std::fs::create_dir_all(plugins.join("broken")).unwrap();
        let manifest = "id = \"good\"\nname = \"Good\"\nversion = \"1.0.0\"\napi = 1\n\
                        license = \"MIT\"\nlayer = \"game\"\n[servers]\nhosts = [\"mud.example\"]\n";
        std::fs::write(good.join("plugin.toml"), manifest).unwrap();

        let installed = installed_plugins(folder.path());
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].id().as_str(), "good");
    }
}
