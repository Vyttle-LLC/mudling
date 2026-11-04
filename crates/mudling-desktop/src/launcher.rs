//! Where a game comes from: the saved servers, the plugins, and the session
//! opened for a server. Everything here that isn't a plain getter blocks on
//! the disk, so the shell calls it off the UI thread.

use crate::room_panel::Verbs;
use crate::settings::{
    Added, AddedPlugin, Downloaded, PluginRow, Sending, ServerPlugins, SuggestedPlugin, shown_path,
};
use crate::shell::Game;
use futures::executor::block_on;
use futures::future::{Either, select};
use mudling_core::defaults::detect::{ServerSignals, Suggestion, suggest};
use mudling_core::git::Limit;
use mudling_core::logs::Logs;
use mudling_core::persist;
use mudling_core::plugin::{Layer, Plugin, PluginId, ServerInfo};
use mudling_core::registry::{
    self, AddedFolder, Approvals, ApprovedCopy, Fetched, FolderCopy, InstalledPlugins, Origins,
    Record, Refused, Registry, Source,
};
use mudling_core::session::{Credentials, Session, SessionConfig};
use mudling_core::sync::{Syncer, is_within};
use mudling_core::vault::{Error as VaultError, Secret, Vault, VaultState};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::Arc;

/// A server to connect to, and who logs in there: no one, for Quick connect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub host: String,
    pub port: u16,
    pub character: Option<String>,
}

impl Target {
    pub fn server(host: impl Into<String>, port: u16) -> Target {
        Target {
            host: host.into(),
            port,
            character: None,
        }
    }

    pub fn with_character(self, character: impl Into<String>) -> Target {
        Target {
            character: Some(character.into()),
            ..self
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.host, self.port)
    }
}

/// Each plugin that runs, by id, with the hash of its copy if that came from
/// the directory: what the directory offers updates by.
pub type InstalledVersions = BTreeMap<String, Option<String>>;

#[derive(Clone)]
pub struct Launcher {
    data_folder: PathBuf,
    /// The app's own plugins.
    own: Vec<Plugin>,
    /// What the data folder had when last read, each with its folder and
    /// the hash this device computed for it.
    installed: Vec<FolderCopy>,
    /// What it had in a copy this device doesn't trust, so didn't load.
    arrived: Vec<FolderCopy>,
    /// What it had that wouldn't load, and why.
    refused: Vec<Refused>,
    /// The folders the player added from this device, each with the last
    /// copy that loaded.
    added: Vec<AddedFolder>,
    /// The command line's `--plugin` folders.
    named: Vec<Plugin>,
    /// The app's own, the installed, the added and the named, each replacing
    /// the last.
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
            own: plugins.clone(),
            installed: Vec::new(),
            arrived: Vec::new(),
            refused: Vec::new(),
            added: Vec::new(),
            named: Vec::new(),
            plugins,
            command_line,
            sync: None,
            wake: Arc::new(wake),
        }
    }

    /// With the command line's plugins, which replace any with their ids.
    pub fn with_named(self, named: Vec<Plugin>) -> Launcher {
        Launcher { named, ..self }.layered()
    }

    /// With the plugins the data folder holds now, each loaded only in a
    /// copy this device trusts. Reads the disk.
    pub fn with_installed(self) -> Launcher {
        let loaded = InstalledPlugins::in_data_folder(&self.data_folder).load();
        for refused in &loaded.refused {
            eprintln!("skipping the plugin {}: {}", refused.id, refused.why);
        }
        Launcher {
            installed: loaded.plugins,
            arrived: loaded.arrived,
            refused: loaded.refused,
            ..self
        }
        .layered()
    }

    /// With the folders the player added from this device, each read now.
    /// Reads the disk.
    pub fn with_added_folders(self) -> Launcher {
        let folders = Origins::in_data_folder(&self.data_folder).added_folders();
        let folders = folders.unwrap_or_else(|error| {
            eprintln!("couldn't read the plugin folders added here: {error}");
            Vec::new()
        });
        let added = folders.iter().map(|path| AddedFolder::read(path)).collect();
        Launcher { added, ..self }.layered()
    }

    pub fn added_folders(&self) -> &[AddedFolder] {
        &self.added
    }

    /// Adds the plugin in `folder`, which must load, and returns the
    /// launcher that has it. A folder sync writes to, in the data folder or
    /// in `sync_repo`, is refused: sync could put code in it that would load
    /// with no trust check and keep the folder's approval.
    pub fn add_folder(&self, folder: &Path, sync_repo: Option<&Path>) -> Result<Launcher, String> {
        let shown = folder.display();
        if self.added.iter().any(|added| added.path == folder) {
            return Err(format!("{shown} is added already."));
        }
        if is_within(folder, &self.data_folder) {
            return Err(format!(
                "{shown} is in MUDling's data folder. {SYNCED_FOLDER}"
            ));
        }
        if sync_repo.is_some_and(|repo| is_within(folder, repo)) {
            return Err(format!(
                "{shown} is in the folder you sync with. {SYNCED_FOLDER}"
            ));
        }
        let added = AddedFolder::read(folder);
        if let Some(problem) = &added.problem {
            return Err(format!("{shown} wasn't added: {problem}."));
        }
        Origins::in_data_folder(&self.data_folder)
            .add_folder(folder)
            .map_err(|error| format!("{shown} wasn't added: {error}."))?;
        let mut launcher = self.clone();
        launcher.added.push(added);
        Ok(launcher.layered())
    }

    /// Reads `folder` again. One that no longer loads keeps its last working
    /// copy, and says why.
    pub fn reload_folder(&self, folder: &Path) -> Launcher {
        let mut launcher = self.clone();
        for added in (launcher.added.iter_mut()).filter(|added| added.path == folder) {
            *added = added.reloaded();
        }
        launcher.layered()
    }

    /// Stops loading the plugin in `folder`, with its approval. The folder
    /// and its files stay where they are.
    pub fn forget_folder(&self, folder: &Path) -> Result<Launcher, String> {
        let could_not = |error| format!("{} wasn't removed: {error}", folder.display());
        Origins::in_data_folder(&self.data_folder)
            .forget_folder(folder)
            .map_err(could_not)?;
        let forgotten = self.added.iter().find(|added| added.path == folder);
        if let Some((added, plugin)) =
            forgotten.and_then(|added| Some((added, added.plugin.as_ref()?)))
        {
            Approvals::in_data_folder(&self.data_folder)
                .take_back(plugin.id().as_str(), &added.approval_key())
                .map_err(could_not)?;
        }
        let mut launcher = self.clone();
        launcher.added.retain(|added| added.path != folder);
        Ok(launcher.layered())
    }

    /// Installed plugins that wouldn't load, and why.
    pub fn refused(&self) -> &[Refused] {
        &self.refused
    }

    /// Trusts the copy in `folder`, as long as it still hashes to `hash`, the
    /// copy the player was shown, and returns the launcher that loads it.
    pub fn trust(&self, folder: &str, hash: &str) -> Result<Launcher, String> {
        InstalledPlugins::in_data_folder(&self.data_folder)
            .trust(folder, hash)
            .map_err(|error| format!("couldn't trust the plugin in {folder}: {error}"))?;
        Ok(self.clone().with_installed())
    }

    /// Whether the copy in `folder` with `hash` is still held back.
    pub fn is_held_back(&self, folder: &str, hash: &str) -> bool {
        (self.arrived.iter()).any(|copy| copy.folder == folder && copy.hash == hash)
    }

    fn layered(self) -> Launcher {
        let installed = self.installed.iter().map(|copy| copy.plugin.clone());
        let plugins = replaced(self.own.clone(), installed.collect());
        let added = self.added.iter().filter_map(|added| added.plugin.clone());
        let plugins = replaced(plugins, added.collect());
        let plugins = replaced(plugins, self.named.clone());
        Launcher { plugins, ..self }
    }

    #[cfg(test)]
    pub fn plugins(&self) -> &[Plugin] {
        &self.plugins
    }

    /// Where the copy of `plugin` that runs comes from. The last with its id
    /// replaces the others, as `replaced` does.
    fn runs_from(&self, plugin: &PluginId) -> Runs<'_> {
        if self.named.iter().any(|named| named.id() == plugin) {
            return Runs::CommandLine;
        }
        let added = (self.added.iter().rev())
            .find(|added| added.plugin.as_ref().is_some_and(|it| it.id() == plugin));
        if let Some(added) = added {
            return Runs::Added(added);
        }
        let copy = (self.installed.iter().rev()).find(|copy| copy.plugin.id() == plugin);
        copy.map_or(Runs::InApp, Runs::Installed)
    }

    /// What the player's approval of the copy of `plugin` that runs is kept
    /// by, if they can give one here: the command line's and the app's own
    /// send commands only with `--enable`.
    fn approval_key(&self, plugin: &PluginId) -> Option<ApprovedCopy> {
        match self.runs_from(plugin) {
            Runs::Added(added) => Some(added.approval_key()),
            Runs::Installed(copy) => Some(ApprovedCopy::Installed(copy.hash.clone())),
            Runs::CommandLine | Runs::InApp => None,
        }
    }

    /// Whether the copy of `plugin` that runs is the one in the data
    /// folder's `folder`.
    pub fn runs_from_folder(&self, plugin: &PluginId, folder: &str) -> bool {
        matches!(self.runs_from(plugin), Runs::Installed(copy) if copy.folder == folder)
    }

    /// Whether the copy of `plugin` that runs is the one added from `path`.
    pub fn runs_from_added(&self, plugin: &PluginId, path: &Path) -> bool {
        matches!(self.runs_from(plugin), Runs::Added(added) if added.path == path)
    }

    /// The plugins let to send commands: by the command line, or by the
    /// player on this device for the copy that runs now.
    pub fn approved(&self) -> Vec<PluginId> {
        let approvals = Approvals::in_data_folder(&self.data_folder);
        let by_player = (self.plugins.iter().map(Plugin::id)).filter(|id| {
            !self.command_line.contains(id)
                && (self.approval_key(id))
                    .is_some_and(|key| approvals.is_approved(id.as_str(), &key))
        });
        let mut approved = self.command_line.clone();
        approved.extend(by_player.cloned());
        approved
    }

    /// Lets the copy of `plugin` that runs send commands, or takes that back.
    /// A copy in the data folder is approved by its content hash, a folder
    /// the player added by where it is.
    pub fn set_approved(&self, plugin: &PluginId, approved: bool) -> Result<(), String> {
        let approvals = Approvals::in_data_folder(&self.data_folder);
        let could_not = |error: std::io::Error| format!("couldn't save your choice: {error}");
        let Some(key) = self.approval_key(plugin) else {
            return match approved {
                true => Err(format!("{plugin} can send commands only with --enable")),
                false => Ok(()),
            };
        };
        match approved {
            true => approvals.approve(plugin.as_str(), &key),
            false => approvals.take_back(plugin.as_str(), &key),
        }
        .map_err(could_not)
    }

    pub fn registry(&self) -> Registry {
        Registry::new(&self.data_folder)
    }

    /// Fetches the plugin a git link points to and checks it. Takes seconds.
    pub fn fetch_link(&self, url: &str, limit: &Limit) -> Result<Fetched, String> {
        registry::fetch_link(&self.data_folder, url, limit)
            .map_err(|error| format!("The plugin at {} wasn't added: {error}.", url.trim()))
    }

    /// What's installed, by id, for the directory to offer updates by: the
    /// hash this device computed for each copy from the directory. A record
    /// travels with the repo, so its hash isn't one to go by.
    pub fn installed_versions(&self) -> InstalledVersions {
        (self.plugins.iter().map(Plugin::id))
            .map(|id| {
                let hash = match self.runs_from(id) {
                    Runs::Installed(copy) if copy.from_directory() => Some(copy.hash.clone()),
                    _ => None,
                };
                (id.to_string(), hash)
            })
            .collect()
    }

    /// What the player added from a link or a folder, for the page that
    /// adds them.
    pub fn added_plugins(&self) -> Vec<AddedPlugin> {
        let links = self.installed.iter().filter_map(|copy| match &copy.record {
            Some(Record::Link { url, .. }) => Some(AddedPlugin {
                plugin: Some(copy.plugin.id().clone()),
                name: copy.plugin.manifest.name.clone().into(),
                from: Added::Link {
                    url: url.clone().into(),
                    folder: copy.folder.clone().into(),
                },
            }),
            _ => None,
        });
        let folders = self.added.iter().map(|added| AddedPlugin {
            plugin: added.plugin.as_ref().map(|plugin| plugin.id().clone()),
            name: (added.plugin.as_ref())
                .map(|plugin| plugin.manifest.name.clone())
                .or_else(|| folder_name(&added.path))
                .unwrap_or_default()
                .into(),
            from: Added::Folder {
                path: added.path.clone(),
                loaded_at: added.loaded_at,
                problem: added.problem.clone().map(Into::into),
            },
        });
        links.chain(folders).collect()
    }

    /// Whether `fetched` updates the installed copy that runs from where that
    /// came from: the directory again, or the same link.
    fn updates_in_place(&self, fetched: &Fetched) -> bool {
        let Runs::Installed(copy) = self.runs_from(fetched.plugin.id()) else {
            return false;
        };
        match (&copy.record, &fetched.source) {
            (Some(Record::Registry { .. }), Source::Directory(_)) => true,
            (Some(Record::Link { url, .. }), Source::Link(new)) => url == new,
            _ => false,
        }
    }

    /// Whether `fetched` may send commands as soon as it's installed: only
    /// an update of an approved copy from where that came from. A plugin
    /// from anywhere else with the same id is other code, and asks.
    pub fn keeps_approval(&self, fetched: &Fetched) -> bool {
        self.updates_in_place(fetched) && self.approved().contains(fetched.plugin.id())
    }

    /// What `fetched` would replace, in words for its approval, if that's an
    /// installed plugin from somewhere else.
    pub fn replaces(&self, fetched: &Fetched) -> Option<String> {
        let Runs::Installed(copy) = self.runs_from(fetched.plugin.id()) else {
            return None;
        };
        if self.updates_in_place(fetched) {
            return None;
        }
        let name = &copy.plugin.manifest.name;
        Some(match &copy.record {
            Some(Record::Registry { .. }) => {
                format!("It replaces the {name} you installed from the directory.")
            }
            Some(Record::Link { url, .. }) => {
                format!("It replaces the {name} you installed from {url}.")
            }
            None => format!("It replaces the {name} in your data folder."),
        })
    }

    /// `fetched`, with whether it keeps an approval and what it replaces,
    /// for the sheet to ask by.
    pub fn downloaded(&self, fetched: Fetched) -> Downloaded {
        Downloaded {
            keeps_approval: self.keeps_approval(&fetched),
            replaces: self.replaces(&fetched),
            fetched: Arc::new(fetched),
        }
    }

    /// Puts `fetched` in the data folder and returns the launcher that has
    /// it. An update from where the old copy came from keeps its approval:
    /// the player chose it here.
    pub fn install(&self, fetched: &Fetched, approve: bool) -> Result<Launcher, String> {
        let id = fetched.plugin.id();
        let was_approved = self.keeps_approval(fetched);
        InstalledPlugins::in_data_folder(&self.data_folder)
            .install(fetched)
            .map_err(|error| {
                let name = &fetched.plugin.manifest.name;
                format!("{name} wasn't installed: {error}")
            })?;
        let launcher = self.clone().with_installed();
        if approve || was_approved {
            launcher.set_approved(id, true)?;
        }
        Ok(launcher)
    }

    /// Takes `plugin` out of the data folder from `folder`, the one its row
    /// came from, and returns the launcher without it. That copy's approval
    /// goes with it: one held back never had one, so a copy running beside
    /// it keeps its own. So does the mirror of the link it came from, if
    /// it did, as far as it can: the plugin is gone either way.
    pub fn uninstall(&self, plugin: &PluginId, folder: &str) -> Result<Launcher, String> {
        let could_not = |error: std::io::Error| format!("{plugin} wasn't removed: {error}");
        let copy = (self.installed.iter().chain(&self.arrived)).find(|copy| copy.folder == folder);
        let link = copy.and_then(|copy| match &copy.record {
            Some(Record::Link { url, .. }) => Some(url.clone()),
            _ => None,
        });
        InstalledPlugins::in_data_folder(&self.data_folder)
            .remove(folder)
            .map_err(could_not)?;
        if let Some(copy) = copy {
            Approvals::in_data_folder(&self.data_folder)
                .take_back(plugin.as_str(), &ApprovedCopy::Installed(copy.hash.clone()))
                .map_err(could_not)?;
        }
        if let Some(url) = link
            && let Err(error) = registry::forget_link(&self.data_folder, &url)
        {
            eprintln!("couldn't delete the copy of {url} kept for updates: {error}");
        }
        Ok(self.clone().with_installed())
    }

    /// Marks what may send commands, where each plugin that runs came from
    /// and how the player can remove it, and adds a row for each copy held
    /// back.
    fn finished_rows(&self, rows: &mut Vec<PluginRow>) {
        PluginRow::approve(rows, &self.approved());
        for row in rows.iter_mut() {
            match self.runs_from(&row.id) {
                Runs::Installed(copy) => {
                    row.folder = Some(copy.folder.clone().into());
                    row.source = match &copy.record {
                        Some(Record::Registry { .. }) => Some(FROM_DIRECTORY.into()),
                        Some(Record::Link { url, .. }) => Some(format!("From {url}").into()),
                        None => None,
                    };
                }
                Runs::Added(added) => {
                    row.added_from = Some(added.path.clone());
                    row.source = Some(format!("From {}", shown_path(&added.path)).into());
                    row.problem = added.problem.clone().map(Into::into);
                }
                Runs::CommandLine => row.source = Some(FROM_COMMAND_LINE.into()),
                Runs::InApp => {}
            }
            if row.sending == Sending::Waiting && self.approval_key(&row.id).is_none() {
                row.sending = Sending::OnlyWithEnable;
            }
        }
        let held_back = self.arrived.iter().map(|copy| PluginRow {
            folder: Some(copy.folder.clone().into()),
            held_back: Some(copy.hash.clone().into()),
            ..PluginRow::of(&copy.plugin, false)
        });
        rows.extend(held_back);
    }

    /// What Settings › Plugins shows for a session on `host`, as saved now.
    pub fn plugin_rows(&self, host: &str) -> Result<Vec<PluginRow>, String> {
        let could_not = |error: persist::Error| format!("couldn't read {host}: {error}");
        let saved = persist::enabled_plugins(&self.data_folder, host).map_err(could_not)?;
        let enabled = self.ids_in(&saved);
        let overrides_off = self.overrides_off(host).map_err(could_not)?;
        let mut rows = PluginRow::for_session(&self.plugins, &enabled, &overrides_off, host);
        self.finished_rows(&mut rows);
        Ok(rows)
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

    pub fn saved(&self) -> persist::SavedServers {
        persist::saved_servers(&self.data_folder)
    }

    pub fn save(&self, target: &Target) -> Result<(), String> {
        persist::save_server(&self.data_folder, &target.host, target.port)
            .map_err(|error| format!("couldn't save {target}: {error}"))
    }

    /// Saves `target`'s character, and its server first if that isn't on the
    /// list.
    pub fn add_character(&self, target: &Target) -> Result<(), String> {
        let Some(name) = &target.character else {
            return self.save(target);
        };
        let saved = self.saved();
        if !saved
            .servers
            .iter()
            .any(|server| server.host == target.host)
        {
            self.save(target)?;
        }
        persist::save_character(&self.data_folder, &target.host, name)
            .map_err(|error| format!("couldn't save {name} on {}: {error}", target.host))
    }

    /// Takes the character `id` off `host`'s list. See
    /// [`persist::remove_character`] for what stays.
    pub fn remove_character(&self, host: &str, id: &str) -> Result<(), String> {
        persist::remove_character(&self.data_folder, host, id)
            .map_err(|error| format!("couldn't remove {id} from {host}: {error}"))
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
        saved.extend(self.command_line.iter().map(|id| id.as_str().to_owned()));
        let enabled = self.ids_in(&saved);
        let overrides_off = self.overrides_off(&target.host).map_err(could_not)?;
        let approved = self.approved();
        let mut plugins =
            PluginRow::for_session(&self.plugins, &enabled, &overrides_off, &target.host);
        self.finished_rows(&mut plugins);
        let config = SessionConfig {
            plugins: self.plugins.clone(),
            enabled: enabled.clone(),
            overrides_off,
            approved,
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
            plugins,
            enabled,
            target: target.clone(),
        })
    }

    fn overrides_off(&self, host: &str) -> Result<Vec<PluginId>, persist::Error> {
        Ok(self.ids_in(&persist::overrides_off(&self.data_folder, host)?))
    }

    /// The ids of the plugins here that `listed` names: a saved list can
    /// name one that's gone.
    fn ids_in(&self, listed: &[String]) -> Vec<PluginId> {
        let named = |plugin: &&PluginId| listed.iter().any(|id| id == plugin.as_str());
        (self.plugins.iter().map(Plugin::id))
            .filter(named)
            .cloned()
            .collect()
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
        let enabled = self.ids_in(&saved);
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
        self.finished_rows(&mut plugins);
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

/// How `target` logs in: as its character, with the password the vault holds
/// for it or else with its name alone, and as no one for Quick connect. With
/// no `vault` to ask, only the name. A locked vault is tried from this
/// device's cache first; this waits on the vault's thread, so call it off the
/// UI thread.
pub fn login_for(vault: Option<&Vault>, target: &Target) -> Login {
    let Some(character) = target.character.clone() else {
        return Login::Ready(None);
    };
    match vault {
        Some(vault) => login_as(vault, &target.host, character),
        None => Login::Ready(Some(name_only(character))),
    }
}

/// Answers the game's name prompt and leaves the password to the player.
fn name_only(name: String) -> Credentials {
    Credentials {
        name,
        password: String::new(),
    }
}

fn login_as(vault: &Vault, host: &str, character: String) -> Login {
    if vault.state() == VaultState::NotCreated {
        return Login::Ready(Some(name_only(character)));
    }
    if vault.state() != VaultState::Unlocked {
        match block_on(vault.unlock_from_device()) {
            Ok(()) => {}
            Err(VaultError::NotCached | VaultError::Cancelled) => return Login::Locked,
            Err(_) => return Login::Ready(Some(name_only(character))),
        }
    }
    match block_on(saved_password(vault, host, &character)) {
        Ok(Some(password)) => Login::Ready(Some(Credentials {
            name: character,
            password: password.expose().to_owned(),
        })),
        Ok(None) => Login::Ready(Some(name_only(character))),
        Err(VaultError::Locked) => Login::Locked,
        Err(_) => Login::Ready(Some(name_only(character))),
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
fn replaced(mut plugins: Vec<Plugin>, replacing: Vec<Plugin>) -> Vec<Plugin> {
    plugins.retain(|plugin| !replacing.iter().any(|other| other.id() == plugin.id()));
    plugins.extend(replacing);
    plugins
}

/// Where the copy of a plugin that runs comes from.
enum Runs<'a> {
    /// A `--plugin` folder.
    CommandLine,
    /// A folder the player added from this device.
    Added(&'a AddedFolder),
    /// The data folder.
    Installed(&'a FolderCopy),
    /// The app itself.
    InApp,
}

const FROM_DIRECTORY: &str = "From the directory";
const SYNCED_FOLDER: &str = "Sync writes there, so it can't be a plugin folder. Choose one \
     of your own outside it.";
const FROM_COMMAND_LINE: &str = "From the command line";

fn folder_name(path: &Path) -> Option<String> {
    Some(path.file_name()?.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Sending;
    use mudling_core::registry::{PLUGINS_FOLDER, Record};
    use mudling_core::vault::{AutoLock, FILE_NAME, MemoryKeyStore};
    use tempfile::TempDir;

    #[test]
    fn a_plugin_the_player_turns_on_stays_on_for_that_server() {
        let folder = TempDir::new().unwrap();
        let plugins = bundled_plugins();
        let id = plugins[0].id().clone();
        let launcher = Launcher::new(folder.path().to_owned(), plugins, Vec::new(), || {});
        let target = Target::server("mud.example", 4000);

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
        let target = Target::server("mud.example", 4000);

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

    /// A plugin with hooks that send commands, fetched as if from the directory.
    fn fetched_sender(hook: &str) -> Fetched {
        let manifest = "id = \"extras\"\nname = \"Extras\"\nversion = \"1.0.0\"\napi = 1\n\
                        license = \"MIT\"\nlayer = \"default\"\npermissions = [\"send_commands\"]\n\
                        [points]\ntext = \"text.toml\"\n[hooks]\nfiles = [\"hooks.luau\"]\n";
        let patterns = "[[pattern]]\nname = \"go\"\npattern = '''^go$'''\nhook = \"on_go\"\n";
        let files: mudling_core::git::FolderFiles = [
            ("plugin.toml", manifest),
            ("text.toml", patterns),
            ("hooks.luau", hook),
        ]
        .map(|(name, text)| (name.to_owned(), text.as_bytes().to_vec()))
        .into();
        let plugin = Plugin::from_files(|name| {
            let bytes = files
                .get(name)
                .ok_or_else(|| std::io::Error::other(name.to_owned()))?;
            Ok(String::from_utf8(bytes.clone()).unwrap())
        })
        .unwrap();
        let entry = mudling_core::registry::Entry {
            id: "extras".to_owned(),
            name: "Extras".to_owned(),
            version: "1.0.0".to_owned(),
            license: "MIT".to_owned(),
            games: Default::default(),
            source: "plugins/extras".to_owned(),
            hash: mudling_core::registry::content_hash(&files),
            permissions: vec!["send_commands".to_owned()],
        };
        Fetched {
            hash: entry.hash.clone(),
            source: mudling_core::registry::Source::Directory(entry),
            commit: mudling_core::git::Oid::ZERO_SHA1,
            files,
            plugin,
        }
    }

    #[test]
    fn an_installed_plugin_waits_for_approval_until_given_and_again_once_taken_back() {
        let folder = TempDir::new().unwrap();
        let launcher = Launcher::new(folder.path().to_owned(), Vec::new(), Vec::new(), || {});
        let extras = fetched_sender("function on_go() mud.send('wave') end");
        let id = extras.plugin.id().clone();

        let launcher = launcher.install(&extras, false).unwrap();
        assert!(launcher.plugins().iter().any(|plugin| *plugin.id() == id));
        assert!(
            launcher.approved().is_empty(),
            "installing alone approves nothing"
        );
        let row = |launcher: &Launcher| {
            let rows = launcher.plugin_rows("mud.example").unwrap();
            rows.into_iter().find(|row| row.id == id).unwrap().sending
        };
        assert_eq!(row(&launcher), Sending::Waiting);

        launcher.set_approved(&id, true).unwrap();
        assert_eq!(launcher.approved(), std::slice::from_ref(&id));
        assert_eq!(row(&launcher), Sending::Approved);

        launcher.set_approved(&id, false).unwrap();
        assert!(launcher.approved().is_empty());
        assert_eq!(row(&launcher), Sending::Waiting);
    }

    #[test]
    fn an_update_from_the_directory_keeps_its_approval_but_a_changed_copy_asks_again() {
        let folder = TempDir::new().unwrap();
        let launcher = Launcher::new(folder.path().to_owned(), Vec::new(), Vec::new(), || {});
        let first = fetched_sender("function on_go() mud.send('wave') end");
        let id = first.plugin.id().clone();
        let launcher = launcher.install(&first, true).unwrap();
        assert_eq!(launcher.approved(), std::slice::from_ref(&id));

        let update = fetched_sender("function on_go() mud.send('bow') end");
        let launcher = launcher.install(&update, false).unwrap();
        assert_eq!(
            launcher.approved(),
            std::slice::from_ref(&id),
            "the player chose it"
        );

        // The same hooks changed by anything else, such as a sync.
        let hooks = folder.path().join(PLUGINS_FOLDER).join("extras/hooks.luau");
        std::fs::write(hooks, "function on_go() mud.send('quit') end").unwrap();
        let launcher = launcher.with_installed();
        assert!(launcher.approved().is_empty());
    }

    #[test]
    fn the_command_line_approves_what_it_turns_on() {
        let folder = TempDir::new().unwrap();
        let plugin = fetched_sender("function on_go() end").plugin;
        let id = plugin.id().clone();
        let launcher = Launcher::new(
            folder.path().to_owned(),
            Vec::new(),
            vec![id.clone()],
            || {},
        )
        .with_named(vec![plugin]);

        assert_eq!(launcher.approved(), [id]);
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
        assert_name_only(nobody, "Nobody");
    }

    /// Logs in with `name` and leaves the password to the player.
    fn assert_name_only(login: Login, name: &str) {
        let Login::Ready(Some(credentials)) = login else {
            panic!("{name} logs in: {login:?}");
        };
        assert_eq!(credentials.name, name);
        assert_eq!(credentials.password, "", "the player types it");
    }

    #[test]
    fn two_characters_on_one_server_each_log_in_as_themselves() {
        let folder = TempDir::new().unwrap();
        let vault = fixture_vault(folder.path());
        let master = "violet anchor mosaic drifting under quiet lanterns";
        let _ = block_on(vault.unlock_with_password(master.into())).unwrap();
        let mira = mudling_core::vault::EntryKey::new(HOST, "Mira");
        block_on(vault.set_password(mira, "moonstone".into())).unwrap();
        let server = Target::server(HOST, 4000);

        for (character, password) in [("Fikus", "swordfish-42"), ("Mira", "moonstone")] {
            let target = server.clone().with_character(character);
            let Login::Ready(Some(credentials)) = login_for(Some(&vault), &target) else {
                panic!("{character} has a saved login");
            };
            assert_eq!(credentials.name, character);
            assert_eq!(credentials.password, password);
        }
        assert!(matches!(
            login_for(Some(&vault), &server),
            Login::Ready(None)
        ));
    }

    #[test]
    fn with_no_vault_a_character_sends_only_its_name() {
        let folder = TempDir::new().unwrap();
        let path = folder.path().join(FILE_NAME);
        let vault = Vault::open(path, Arc::new(MemoryKeyStore::default()), AutoLock::NEVER);
        let target = Target::server(HOST, 4000).with_character("Fikus");
        assert_name_only(login_for(Some(&vault), &target), "Fikus");
        assert_name_only(login_for(None, &target), "Fikus");
    }

    #[test]
    fn an_added_character_is_saved_with_its_new_server() {
        let folder = TempDir::new().unwrap();
        let launcher = Launcher::new(folder.path().to_owned(), Vec::new(), Vec::new(), || {});
        let target = Target::server("new.example", 23).with_character("Wren");

        launcher.add_character(&target).unwrap();

        let saved = launcher.saved();
        assert_eq!(
            (&*saved.servers[0].host, saved.servers[0].port),
            ("new.example", 23)
        );
        assert_eq!(saved.servers[0].characters[0].id, "wren");
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
        let launcher = Launcher::new(folder.path().to_owned(), bundled, Vec::new(), || {});
        let named = |launcher: &Launcher| -> Vec<String> {
            (launcher.plugins().iter())
                .filter(|p| p.id().as_str() == game)
                .map(|p| p.manifest.name.clone())
                .collect()
        };

        // Made on this device, so the player's own.
        assert_eq!(named(&launcher.with_installed()), ["Mine"]);
    }

    #[test]
    fn a_plugin_folder_that_will_not_load_leaves_the_others_installed() {
        let folder = TempDir::new().unwrap();
        let installed = || {
            let launcher = Launcher::new(folder.path().to_owned(), Vec::new(), Vec::new(), || {});
            launcher.with_installed()
        };
        assert!(installed().plugins().is_empty());

        let plugins = folder.path().join(PLUGINS_FOLDER);
        let good = plugins.join("good");
        std::fs::create_dir_all(&good).unwrap();
        std::fs::create_dir_all(plugins.join("broken")).unwrap();
        let manifest = "id = \"good\"\nname = \"Good\"\nversion = \"1.0.0\"\napi = 1\n\
                        license = \"MIT\"\nlayer = \"game\"\n[servers]\nhosts = [\"mud.example\"]\n";
        std::fs::write(good.join("plugin.toml"), manifest).unwrap();

        let launcher = installed();
        assert_eq!(launcher.plugins().len(), 1);
        assert_eq!(launcher.plugins()[0].id().as_str(), "good");
        assert_eq!(
            launcher.refused()[0].id,
            "broken",
            "and says why it isn't loaded"
        );
    }

    #[test]
    fn a_tampered_copy_with_a_matching_record_is_held_back_until_trusted() {
        let folder = TempDir::new().unwrap();
        let launcher = Launcher::new(folder.path().to_owned(), Vec::new(), Vec::new(), || {});
        let extras = fetched_sender("function on_go() end");
        let id = extras.plugin.id().clone();
        let launcher = launcher.install(&extras, false).unwrap();

        // As a tampered repo's sync would leave it: the same name, other
        // code, and a record rewritten to match.
        let plugins = folder.path().join(PLUGINS_FOLDER);
        let swapped = "[[pattern]]\nname = \"go\"\npattern = '''.*'''\nhook = \"on_go\"\n";
        std::fs::write(plugins.join("extras/text.toml"), swapped).unwrap();
        let hash = mudling_core::registry::folder_hash(&plugins.join("extras")).unwrap();
        let record = Record::Registry {
            version: "1.0.0".to_owned(),
            commit: "0".repeat(40),
            hash,
        };
        std::fs::write(
            plugins.join("extras.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        let launcher = launcher.with_installed();

        assert!(launcher.plugins().is_empty(), "nothing of it runs");
        let rows = launcher.plugin_rows("mud.example").unwrap();
        let row = rows.iter().find(|row| row.id == id).unwrap();
        assert!(!row.on);
        assert_eq!(row.folder.as_deref(), Some("extras"));
        let shown = row.held_back.clone().expect("it's held back");

        let launcher = launcher.trust("extras", &shown).unwrap();
        assert_eq!(launcher.plugins().len(), 1, "the player trusted this copy");
    }

    #[test]
    fn a_trusted_copy_whose_patterns_changed_no_longer_has_its_approval() {
        let folder = TempDir::new().unwrap();
        let launcher = Launcher::new(folder.path().to_owned(), Vec::new(), Vec::new(), || {});
        let extras = fetched_sender("function on_go(event) mud.send(event.captures.x) end");
        let id = extras.plugin.id().clone();
        let launcher = launcher.install(&extras, true).unwrap();
        assert_eq!(launcher.approved(), std::slice::from_ref(&id));

        // Only the patterns change, to feed the hook any text from the server.
        let patterns = folder.path().join(PLUGINS_FOLDER).join("extras/text.toml");
        let wide = "[[pattern]]\nname = \"go\"\npattern = '''(?P<x>.*)'''\nhook = \"on_go\"\n";
        std::fs::write(patterns, wide).unwrap();
        let held = launcher.with_installed();
        let rows = held.plugin_rows("mud.example").unwrap();
        let shown = (rows.iter().find_map(|row| row.held_back.clone())).expect("held back");
        let launcher = held.trust("extras", &shown).unwrap();

        assert_eq!(launcher.plugins().len(), 1, "trusted, so it runs");
        assert!(launcher.approved().is_empty(), "but sending asks again");
    }

    #[test]
    fn an_update_is_offered_by_the_copy_this_device_holds_not_its_record() {
        let folder = TempDir::new().unwrap();
        let launcher = Launcher::new(folder.path().to_owned(), Vec::new(), Vec::new(), || {});
        let extras = fetched_sender("function on_go() end");
        let installed_hash = extras.hash.clone();
        let launcher = launcher.install(&extras, false).unwrap();

        // The repo's record claims a newer copy than the one on the disk.
        let record = Record::Registry {
            version: "1.1.0".to_owned(),
            commit: "0".repeat(40),
            hash: "sha256:what-the-index-lists-now".to_owned(),
        };
        let path = folder.path().join(PLUGINS_FOLDER).join("extras.json");
        std::fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
        let launcher = launcher.with_installed();

        let versions = launcher.installed_versions();
        assert_eq!(versions.get("extras"), Some(&Some(installed_hash)));
    }

    #[test]
    fn trust_and_remove_take_the_folder_shown_not_the_one_named_for_the_id() {
        let folder = TempDir::new().unwrap();
        let data = folder.path();
        mudling_core::registry::Origins::in_data_folder(data)
            .mark_arrived(["zz", "extras"])
            .unwrap();
        // The repo brings `zz/`, holding `extras`, and `extras/`, holding `other`.
        let plugins = data.join(PLUGINS_FOLDER);
        let manifest = |id: &str| {
            format!(
                "id = \"{id}\"\nname = \"{id}\"\nversion = \"1.0.0\"\napi = 1\n\
                 license = \"MIT\"\nlayer = \"default\"\n"
            )
        };
        for (name, id) in [("zz", "extras"), ("extras", "other")] {
            std::fs::create_dir_all(plugins.join(name)).unwrap();
            std::fs::write(plugins.join(name).join("plugin.toml"), manifest(id)).unwrap();
        }
        let launcher = Launcher::new(data.to_owned(), Vec::new(), Vec::new(), || {});
        let launcher = launcher.with_installed();
        let rows = launcher.plugin_rows("mud.example").unwrap();
        let row = rows.iter().find(|row| row.id.as_str() == "extras").unwrap();
        let (shown_folder, shown) = (row.folder.clone().unwrap(), row.held_back.clone().unwrap());
        assert_eq!(shown_folder, "zz");

        let launcher = launcher.trust(&shown_folder, &shown).unwrap();
        let ids: Vec<&str> = (launcher.plugins().iter())
            .map(|p| p.id().as_str())
            .collect();
        assert_eq!(ids, ["extras"], "`other`, never shown, stays out");

        let launcher = launcher.uninstall(&row.id, &shown_folder).unwrap();
        assert!(!plugins.join("zz").exists());
        assert!(
            plugins.join("extras").exists(),
            "the folder named for the id stays"
        );
        assert!(launcher.plugins().is_empty());
    }

    /// A folder of the player's own on this device, holding `fetched`'s files.
    fn players_folder(fetched: &Fetched) -> TempDir {
        let folder = TempDir::new().unwrap();
        for (name, bytes) in &fetched.files {
            std::fs::write(folder.path().join(name), bytes).unwrap();
        }
        folder
    }

    #[test]
    fn a_reload_that_breaks_a_folder_keeps_its_last_working_copy() {
        let data = TempDir::new().unwrap();
        let launcher = Launcher::new(data.path().to_owned(), Vec::new(), Vec::new(), || {});
        let mine = players_folder(&fetched_sender("function on_go() end"));
        let launcher = launcher.add_folder(mine.path(), None).unwrap();
        let id = launcher.plugins()[0].id().clone();

        let patterns = mine.path().join("text.toml");
        std::fs::write(&patterns, "[[pattern]]\nname = \"go\"\npattern = '''('''\n").unwrap();
        let launcher = launcher.reload_folder(mine.path());

        assert_eq!(launcher.plugins().len(), 1, "the last working copy runs");
        assert_eq!(launcher.plugins()[0].id(), &id);
        let rows = launcher.plugin_rows("mud.example").unwrap();
        let row = rows.iter().find(|row| row.id == id).unwrap();
        assert!(
            row.problem
                .as_deref()
                .is_some_and(|why| why.contains("text.toml"))
        );
        assert_eq!(row.added_from.as_deref(), Some(mine.path()));

        let fixed = "[[pattern]]\nname = \"go\"\npattern = '''^went$'''\nhook = \"on_go\"\n";
        std::fs::write(&patterns, fixed).unwrap();
        let launcher = launcher.reload_folder(mine.path());
        let rows = launcher.plugin_rows("mud.example").unwrap();
        assert_eq!(rows.iter().find(|row| row.id == id).unwrap().problem, None);
        let text = launcher.plugins()[0].points.text.as_ref().unwrap();
        assert!(
            text.patterns[0].pattern.is_match("went"),
            "the new copy runs"
        );
    }

    #[test]
    fn a_folder_that_does_not_load_is_not_added() {
        let data = TempDir::new().unwrap();
        let launcher = Launcher::new(data.path().to_owned(), Vec::new(), Vec::new(), || {});
        let empty = TempDir::new().unwrap();

        assert!(launcher.add_folder(empty.path(), None).is_err());
        assert!(launcher.with_added_folders().added_folders().is_empty());
    }

    #[test]
    fn a_folder_sync_writes_to_is_not_added() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let launcher = Launcher::new(data.path().to_owned(), Vec::new(), Vec::new(), || {});
        let sender = fetched_sender("function on_go() end");
        let in_plugins = data.path().join(PLUGINS_FOLDER).join("mine");
        let in_repo = repo.path().join("plugins/mine");
        for folder in [&in_plugins, &in_repo] {
            std::fs::create_dir_all(folder).unwrap();
            for (name, bytes) in &sender.files {
                std::fs::write(folder.join(name), bytes).unwrap();
            }
        }

        let repo = Some(repo.path());
        for folder in [data.path(), &in_plugins, &in_repo] {
            let refused = launcher.add_folder(folder, repo);
            assert!(
                refused
                    .as_ref()
                    .is_err_and(|why| why.contains("Sync writes there")),
                "{}",
                folder.display()
            );
        }
        assert!(launcher.with_added_folders().added_folders().is_empty());
    }

    #[test]
    fn a_link_with_an_approved_plugin_s_id_asks_and_says_what_it_replaces() {
        let data = TempDir::new().unwrap();
        let launcher = Launcher::new(data.path().to_owned(), Vec::new(), Vec::new(), || {});
        let launcher = launcher
            .install(&fetched_sender("function on_go() end"), true)
            .unwrap();
        let mut fork = fetched_sender("function on_go() mud.send('quit') end");
        fork.source = Source::Link("https://example.org/fork.git".to_owned());

        assert!(!launcher.keeps_approval(&fork));
        let replaces = launcher.replaces(&fork).expect("it says so");
        assert!(replaces.contains("from the directory"), "{replaces}");
        let launcher = launcher.install(&fork, false).unwrap();
        assert!(launcher.approved().is_empty(), "the fork never had it");

        // The same link again is an update, and keeps what the player gave.
        let launcher = launcher.install(&fork, true).unwrap();
        let mut update = fetched_sender("function on_go() mud.send('wave') end");
        update.source = fork.source.clone();
        assert!(launcher.keeps_approval(&update));
        assert_eq!(launcher.replaces(&update), None);
    }

    #[test]
    fn removing_a_folder_plugin_forgets_it_and_leaves_the_players_folder() {
        let data = TempDir::new().unwrap();
        let launcher = Launcher::new(data.path().to_owned(), Vec::new(), Vec::new(), || {});
        let mine = players_folder(&fetched_sender("function on_go() end"));
        let launcher = launcher.add_folder(mine.path(), None).unwrap();
        let id = launcher.plugins()[0].id().clone();
        launcher.set_approved(&id, true).unwrap();
        assert!(launcher.runs_from_added(&id, mine.path()));

        let launcher = launcher.forget_folder(mine.path()).unwrap();

        assert!(launcher.plugins().is_empty());
        assert!(mine.path().join("plugin.toml").exists());
        assert!(mine.path().join("hooks.luau").exists());
        let fresh = Launcher::new(data.path().to_owned(), Vec::new(), Vec::new(), || {});
        let fresh = fresh.with_added_folders();
        assert!(fresh.plugins().is_empty(), "forgotten for good");
        let fresh = fresh.add_folder(mine.path(), None).unwrap();
        assert!(fresh.approved().is_empty(), "adding it again asks again");
    }

    #[test]
    fn a_folder_plugin_s_approval_holds_across_the_player_s_saves() {
        let data = TempDir::new().unwrap();
        let launcher = Launcher::new(data.path().to_owned(), Vec::new(), Vec::new(), || {});
        let mine = players_folder(&fetched_sender("function on_go() end"));
        let launcher = launcher.add_folder(mine.path(), None).unwrap();
        let id = launcher.plugins()[0].id().clone();
        launcher.set_approved(&id, true).unwrap();

        let hooks = mine.path().join("hooks.luau");
        std::fs::write(hooks, "function on_go() mud.send('wave') end").unwrap();
        let launcher = launcher.reload_folder(mine.path());

        assert_eq!(launcher.approved(), [id]);
    }

    #[test]
    fn a_folder_with_an_installed_plugin_s_id_has_an_approval_of_its_own() {
        let data = TempDir::new().unwrap();
        let launcher = Launcher::new(data.path().to_owned(), Vec::new(), Vec::new(), || {});
        let extras = fetched_sender("function on_go() end");
        let id = extras.plugin.id().clone();
        let launcher = launcher.install(&extras, true).unwrap();
        let mine = players_folder(&fetched_sender("function on_go() mud.send('wave') end"));
        let launcher = launcher.add_folder(mine.path(), None).unwrap();
        assert!(launcher.runs_from_added(&id, mine.path()));
        assert!(
            launcher.approved().is_empty(),
            "the folder asks for its own"
        );

        launcher.set_approved(&id, true).unwrap();
        let launcher = launcher.forget_folder(mine.path()).unwrap();

        assert!(launcher.runs_from_folder(&id, "extras"));
        assert_eq!(
            launcher.approved(),
            [id],
            "the installed copy keeps its own"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_link_plugin_is_removed_even_when_its_mirror_cannot_be() {
        use std::os::unix::fs::PermissionsExt;
        let data = TempDir::new().unwrap();
        let launcher = Launcher::new(data.path().to_owned(), Vec::new(), Vec::new(), || {});
        let mut linked = fetched_sender("function on_go() end");
        linked.source = Source::Link("https://example.org/extras.git".to_owned());
        let id = linked.plugin.id().clone();
        let launcher = launcher.install(&linked, true).unwrap();
        let links = data.path().join("registry/links");
        std::fs::create_dir_all(&links).unwrap();
        // Nothing in here can be looked up, so the link's mirror can't go.
        let locked = |mode| std::fs::set_permissions(&links, PermissionsExt::from_mode(mode));
        locked(0o000).unwrap();

        let removed = launcher.uninstall(&id, "extras");
        locked(0o755).unwrap();

        let launcher = removed.unwrap();
        assert!(launcher.plugins().is_empty());
        let launcher = launcher.install(&linked, false).unwrap();
        assert!(launcher.approved().is_empty(), "its approval went with it");
    }

    #[test]
    fn a_command_line_plugin_that_sends_offers_no_approval_it_cannot_give() {
        let data = TempDir::new().unwrap();
        let plugin = fetched_sender("function on_go() end").plugin;
        let id = plugin.id().clone();
        let launcher = Launcher::new(data.path().to_owned(), Vec::new(), Vec::new(), || {})
            .with_named(vec![plugin]);

        let rows = launcher.plugin_rows("mud.example").unwrap();
        let row = rows.iter().find(|row| row.id == id).unwrap();
        assert_eq!(row.sending, Sending::OnlyWithEnable);
        assert!(launcher.set_approved(&id, true).is_err());
    }

    #[test]
    fn removing_a_held_back_copy_leaves_the_running_copy_and_its_approval() {
        let folder = TempDir::new().unwrap();
        let data = folder.path();
        let launcher = Launcher::new(data.to_owned(), Vec::new(), Vec::new(), || {});
        let extras = fetched_sender("function on_go() end");
        let id = extras.plugin.id().clone();
        let launcher = launcher.install(&extras, true).unwrap();
        // Sync brings `zz/`, another copy of `extras`.
        mudling_core::registry::Origins::in_data_folder(data)
            .mark_arrived(["zz"])
            .unwrap();
        let zz = data.join(PLUGINS_FOLDER).join("zz");
        std::fs::create_dir_all(&zz).unwrap();
        for (name, bytes) in &fetched_sender("function on_go() mud.send('quit') end").files {
            std::fs::write(zz.join(name), bytes).unwrap();
        }
        let launcher = launcher.with_installed();
        let rows = launcher.plugin_rows("mud.example").unwrap();
        let copies: Vec<&PluginRow> = rows.iter().filter(|row| row.id == id).collect();
        assert_eq!(copies.len(), 2);
        assert_ne!(copies[0].key(), copies[1].key(), "their elements differ");

        assert!(!launcher.runs_from_folder(&id, "zz"));
        let launcher = launcher.uninstall(&id, "zz").unwrap();

        assert_eq!(launcher.approved(), [id]);
        assert!(data.join(PLUGINS_FOLDER).join("extras").exists());
    }

    #[test]
    fn removing_a_plugin_takes_its_approval_with_it() {
        let folder = TempDir::new().unwrap();
        let launcher = Launcher::new(folder.path().to_owned(), Vec::new(), Vec::new(), || {});
        let extras = fetched_sender("function on_go() end");
        let id = extras.plugin.id().clone();
        let launcher = launcher.install(&extras, true).unwrap();
        let rows = launcher.plugin_rows("mud.example").unwrap();
        let row = rows.iter().find(|row| row.id == id).unwrap();
        assert_eq!(row.folder.as_deref(), Some("extras"));

        let launcher = launcher.uninstall(&id, "extras").unwrap();

        assert!(launcher.plugins().iter().all(|plugin| *plugin.id() != id));
        let launcher = launcher.install(&extras, false).unwrap();
        assert!(
            launcher.approved().is_empty(),
            "installing it again asks again"
        );
    }
}
