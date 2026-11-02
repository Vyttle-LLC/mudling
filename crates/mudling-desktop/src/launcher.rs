//! Where a game comes from: the saved servers, the plugins, and the session
//! opened for a server. Everything here that isn't a plain getter blocks on
//! the disk, so the shell calls it off the UI thread.

use crate::room_panel::Verbs;
use crate::settings::{PluginRow, ServerPlugins, SuggestedPlugin};
use crate::shell::Game;
use futures::executor::block_on;
use futures::future::{Either, select};
use mudling_core::defaults::detect::{ServerSignals, Suggestion, suggest};
use mudling_core::logs::Logs;
use mudling_core::persist;
use mudling_core::plugin::{Layer, Plugin, PluginId, ServerInfo};
use mudling_core::session::{Credentials, Session, SessionConfig};
use mudling_core::sync::Syncer;
use mudling_core::vault::{Error as VaultError, Secret, Vault, VaultState};
use std::fmt;
use std::path::{Path, PathBuf};
use std::pin::pin;
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
    /// Told of every session, if there's sync to tell.
    sync: Option<Syncer>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

/// What the vault says about logging in to a server.
#[derive(Debug)]
pub enum Login {
    /// Log in with these, or connect without if there are none to use.
    Ready(Option<Credentials>),
    /// A character's password may be in there, but only the player can open
    /// it.
    Locked,
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
            sync: None,
            wake: Arc::new(wake),
        }
    }

    pub fn data_folder(&self) -> &Path {
        &self.data_folder
    }

    pub fn logs(&self) -> Logs {
        Logs::in_data_folder(&self.data_folder)
    }

    /// Hands `sync` to every session opened from now on.
    pub fn with_sync(self, sync: Option<Syncer>) -> Launcher {
        Launcher { sync, ..self }
    }

    /// The saved login for the character last played on `host`. A locked
    /// vault is tried from this device's cache first; this waits on the
    /// vault's thread, so call it off the UI thread.
    pub fn login(&self, vault: &Vault, host: &str) -> Login {
        match self.last_played(host) {
            Some(character) => login_as(vault, host, character),
            None => Login::Ready(None),
        }
    }

    fn last_played(&self, host: &str) -> Option<String> {
        let saved = self.saved();
        let server = saved.servers.iter().find(|server| server.host == host)?;
        let newest = server
            .characters
            .iter()
            .filter(|character| character.name.is_some())
            .max_by_key(|character| character.last_played)?;
        newest.name.clone()
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
    pub fn open(
        &self,
        target: &Target,
        turn_on: Option<&PluginId>,
        credentials: Option<Credentials>,
    ) -> Result<Game, String> {
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
        let overrides_off = self
            .overrides_off(&target.host)
            .map_err(|error| format!("couldn't open {target}: {error}"))?;
        let config = SessionConfig {
            plugins: self.plugins.clone(),
            enabled: enabled.clone(),
            overrides_off: overrides_off.clone(),
            approved: self.command_line.clone(),
            credentials,
            sync: self.sync.clone(),
            ..SessionConfig::new(&self.data_folder, &target.host, target.port)
        };
        let wake = Arc::clone(&self.wake);
        let session = Session::open(config, move || wake())
            .map_err(|error| format!("couldn't start the session: {error}"))?;
        Ok(Game {
            session,
            replay_folder: None,
            verbs: Verbs::of(&self.plugins, &enabled),
            plugins: PluginRow::for_session(&self.plugins, &enabled, &overrides_off, &target.host),
            enabled,
            target: target.clone(),
        })
    }

    fn overrides_off(&self, host: &str) -> Result<Vec<PluginId>, persist::Error> {
        let off = persist::overrides_off(&self.data_folder, host)?;
        let turned_off = |plugin: &&PluginId| off.iter().any(|id| id == plugin.as_str());
        let ids = self.plugins.iter().map(Plugin::id).filter(turned_off);
        Ok(ids.cloned().collect())
    }

    /// What Settings shows of `host`'s plugins. `suggested` and `running` are
    /// what a session there learned; without one, its host is all there is to
    /// go on, so an override that names only the game's MSSP name isn't listed
    /// until the server has said it, or the player has turned it off.
    pub fn server_plugins(
        &self,
        host: &str,
        suggested: Option<Suggestion>,
        running: &[PluginId],
    ) -> Result<ServerPlugins, String> {
        let could_not = |error: persist::Error| format!("couldn't read {host}: {error}");
        let saved = persist::enabled_plugins(&self.data_folder, host).map_err(could_not)?;
        let enabled: Vec<PluginId> = (self.plugins.iter().map(Plugin::id))
            .filter(|plugin| saved.iter().any(|id| id == plugin.as_str()))
            .cloned()
            .collect();
        let overrides_off = self.overrides_off(host).map_err(could_not)?;
        let (triggers, aliases) =
            persist::rule_counts(&self.data_folder, host).map_err(could_not)?;
        let server = ServerInfo {
            host,
            mssp_name: None,
        };
        let matches_host = |row: &PluginRow| {
            let plugin = self.plugins.iter().find(|plugin| *plugin.id() == row.id);
            plugin.is_some_and(|plugin| plugin.manifest.matches_server(server))
        };
        let mut plugins = PluginRow::for_session(&self.plugins, &enabled, &overrides_off, host);
        for row in plugins
            .iter_mut()
            .filter(|row| row.layer == Layer::Override)
        {
            row.on |= running.contains(&row.id) && !overrides_off.contains(&row.id);
        }
        plugins.retain(|row| {
            row.layer != Layer::Override
                || matches_host(row)
                || running.contains(&row.id)
                || overrides_off.contains(&row.id)
        });
        let signals = ServerSignals {
            host,
            ..ServerSignals::default()
        };
        let suggested = suggested.or_else(|| suggest(&self.plugins, signals));
        Ok(ServerPlugins {
            host: host.to_owned().into(),
            plugins,
            suggested: suggested.map(|suggestion| SuggestedPlugin {
                name: self
                    .plugin_name(&suggestion.plugin)
                    .unwrap_or(suggestion.plugin.as_str())
                    .to_owned()
                    .into(),
                plugin: suggestion.plugin,
                source: suggestion.source,
            }),
            triggers,
            aliases,
        })
    }

    /// Saves the player's switch for `plugin` on `host`, for the next session
    /// there: game plugins are off until turned on, overrides on until off.
    pub fn set_plugin(&self, host: &str, plugin: &PluginId, on: bool) -> Result<(), String> {
        let could_not = |error: persist::Error| format!("couldn't save {host}: {error}");
        let folder = &self.data_folder;
        let is_override = (self.plugins.iter())
            .any(|other| other.id() == plugin && other.layer() == Layer::Override);
        let listed = |mut list: Vec<String>, wanted: bool| {
            list.retain(|id| id != plugin.as_str());
            if wanted {
                list.push(plugin.as_str().to_owned());
            }
            list
        };
        if is_override {
            let off = listed(
                persist::overrides_off(folder, host).map_err(could_not)?,
                !on,
            );
            persist::save_overrides_off(folder, host, &off).map_err(could_not)
        } else {
            let enabled = listed(
                persist::enabled_plugins(folder, host).map_err(could_not)?,
                on,
            );
            persist::save_enabled_plugins(folder, host, &enabled).map_err(could_not)
        }
    }

    /// The name the plugin gives itself.
    pub fn plugin_name(&self, id: &PluginId) -> Option<&str> {
        let plugin = self.plugins.iter().find(|plugin| plugin.id() == id)?;
        Some(&plugin.manifest.name)
    }
}

fn login_as(vault: &Vault, host: &str, character: String) -> Login {
    if vault.state() != VaultState::Unlocked {
        match block_on(vault.unlock_from_device()) {
            Ok(()) => {}
            Err(VaultError::NotCached) => return Login::Locked,
            Err(_) => return Login::Ready(None),
        }
    }
    match block_on(saved_password(vault, host, &character)) {
        Ok(password) => Login::Ready(password.map(|password| Credentials {
            name: character,
            password: password.expose().to_owned(),
        })),
        Err(VaultError::Locked) => Login::Locked,
        Err(_) => Login::Ready(None),
    }
}

/// The password saved for `character`, however the player capitalized it
/// there. Gives [`VaultError::Locked`] rather than wait out a vault that locks
/// mid-call, since a job running here holds up the window.
async fn saved_password(
    vault: &Vault,
    host: &str,
    character: &str,
) -> Result<Option<Secret>, VaultError> {
    let mut state = vault.subscribe();
    let read = async {
        let entries = vault.entries().await?;
        let entry = entries
            .iter()
            .find(|entry| entry.server == host && entry.character.eq_ignore_ascii_case(character));
        match entry {
            Some(entry) => vault.password(entry).await,
            None => Ok(None),
        }
    };
    let locked = state.wait_for(|state| *state != VaultState::Unlocked);
    match select(pin!(read), pin!(locked)).await {
        Either::Left((read, _)) => read,
        Either::Right(_) => Err(VaultError::Locked),
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
    use mudling_core::vault::{AutoLock, FILE_NAME, MemoryKeyStore};
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

        let game = launcher.open(&target, None, None).unwrap();
        assert!(game.enabled.is_empty(), "detection turns nothing on");
        game.session.quit().wait().unwrap();

        let game = launcher.open(&target, Some(&id), None).unwrap();
        assert_eq!(game.enabled, std::slice::from_ref(&id));
        game.session.quit().wait().unwrap();

        let game = launcher.open(&target, None, None).unwrap();
        assert_eq!(game.enabled, [id]);
        game.session.quit().wait().unwrap();
    }

    #[test]
    fn switches_are_kept_for_one_server_and_the_next_session_follows() {
        let folder = TempDir::new().unwrap();
        let plugins = bundled_plugins();
        let game = plugins[0].id().clone();
        let launcher = Launcher::new(folder.path().to_owned(), plugins, Vec::new(), || {});
        let target = Target {
            host: "mud.example".to_owned(),
            port: 4000,
        };

        launcher.set_plugin("mud.example", &game, true).unwrap();
        let on = |launcher: &Launcher, host| {
            let page = launcher.server_plugins(host, None, &[]).unwrap();
            page.plugins.iter().find(|row| row.id == game).unwrap().on
        };
        assert!(on(&launcher, "mud.example"));
        assert!(!on(&launcher, "other.example"), "this server only");
        let opened = launcher.open(&target, None, None).unwrap();
        assert_eq!(opened.enabled, std::slice::from_ref(&game));
        opened.session.quit().wait().unwrap();

        launcher.set_plugin("mud.example", &game, false).unwrap();
        assert!(!on(&launcher, "mud.example"));
        let opened = launcher.open(&target, None, None).unwrap();
        assert!(opened.enabled.is_empty());
        opened.session.quit().wait().unwrap();
    }

    #[test]
    fn an_override_stays_off_for_the_server_it_was_turned_off_on() {
        let folder = TempDir::new().unwrap();
        let manifest = "id = \"mine\"\nname = \"Mine\"\nversion = \"1.0.0\"\napi = 1\n\
                        license = \"MIT\"\nlayer = \"override\"\n[servers]\n\
                        hosts = [\"mud.example\"]\n";
        let mine = Plugin::from_files(|file| match file {
            "plugin.toml" => Ok(manifest.to_owned()),
            _ => Err(std::io::Error::other(file.to_owned())),
        })
        .unwrap();
        let id = mine.id().clone();
        let launcher = Launcher::new(folder.path().to_owned(), vec![mine], Vec::new(), || {});
        let row = |host, running: &[PluginId]| {
            let page = launcher.server_plugins(host, None, running).unwrap();
            page.plugins.into_iter().find(|row| row.id == id)
        };
        assert!(row("mud.example", &[]).unwrap().on);
        assert!(row("other.example", &[]).is_none(), "it's not for there");

        launcher.set_plugin("mud.example", &id, false).unwrap();
        assert!(!row("mud.example", &[]).unwrap().on);
        launcher.set_plugin("mud.example", &id, true).unwrap();
        assert!(row("mud.example", &[]).unwrap().on);

        let by_its_name = row("other.example", std::slice::from_ref(&id));
        assert!(by_its_name.unwrap().on, "the game's name brought it on");
        launcher.set_plugin("other.example", &id, false).unwrap();
        let off = row("other.example", &[]).expect("still there to turn back on");
        assert!(!off.on);
    }

    const HOST: &str = "mud.example.org";

    fn fixture_vault(folder: &Path) -> Vault {
        let path = folder.join(FILE_NAME);
        std::fs::write(
            &path,
            include_str!("../../mudling-core/src/vault/testdata/format-1.vault"),
        )
        .unwrap();
        Vault::open(path, Arc::new(MemoryKeyStore::default()), AutoLock::NEVER)
    }

    #[test]
    fn a_locked_vault_is_left_to_the_player_and_an_open_one_logs_in() {
        let folder = TempDir::new().unwrap();
        let vault = fixture_vault(folder.path());
        let login = login_as(&vault, HOST, "Fikus".to_owned());
        assert!(matches!(login, Login::Locked), "{login:?}");

        let master = "violet anchor mosaic drifting under quiet lanterns";
        let _ = block_on(vault.unlock_with_password(master.into())).unwrap();
        let Login::Ready(Some(credentials)) = login_as(&vault, HOST, "fikus".to_owned()) else {
            panic!("a saved login, however it's capitalized");
        };
        assert_eq!(credentials.password, "swordfish-42");

        let nobody = login_as(&vault, HOST, "Nobody".to_owned());
        assert!(matches!(nobody, Login::Ready(None)), "{nobody:?}");
    }

    #[test]
    fn with_no_vault_a_server_just_connects() {
        let folder = TempDir::new().unwrap();
        let path = folder.path().join(FILE_NAME);
        let vault = Vault::open(path, Arc::new(MemoryKeyStore::default()), AutoLock::NEVER);
        let login = login_as(&vault, HOST, "Fikus".to_owned());
        assert!(matches!(login, Login::Ready(None)), "{login:?}");
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
