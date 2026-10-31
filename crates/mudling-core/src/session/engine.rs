//! The session's work, with no threads or sockets of its own: bytes in;
//! stores, database writes and bytes to send out. [`crate::session::Session`]
//! runs it on a core thread. A replay or a benchmark can drive it directly.

use super::inventory::{self, Inventory};
use super::login::Login;
use super::map_rules::PluginRules;
use super::values::{self, HubView};
use super::{SessionConfig, SessionError, Typing};
use crate::completion::{HISTORY_FILE_NAME, HistoryWriter, load_history, words_of};
use crate::defaults::commands::{WalkSyntax, chain_lines, parse_speedwalk, split_chain};
use crate::defaults::detect::{ServerSignals, suggest};
use crate::defaults::hud::{self, FieldMap};
use crate::defaults::{self, Defaults, PendingAction};
use crate::line::{Encoding, Line, LineEnd, LinePipeline};
use crate::map::{Direction, LiveMap, MapChange, MapRoom, Mark};
use crate::merge::{RoomMoves, repoint_batch};
use crate::net::DisconnectReason;
use crate::persist::{self, Batch, Character, Database, Server, Timestamp, Writer};
use crate::plugin::luau::{HookOutput, HookSink, LuauHooks, StoreWrite};
use crate::plugin::{
    Layer, MapSignal, Plugin, PluginEvent, PluginHost, PluginId, PluginSink, ServerInfo,
};
use crate::store::{
    CharactersState, ConnectionStatus, GearState, MapState, Memo, MobsState, RoomState,
    SettingsState, StoreHub, TerminalChange, TerminalState, Versioned, VitalsState,
};
use crate::telnet::{self, Bytes, CompressionError, Event, gmcp};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::io;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// Under the data folder, one database per server.
pub(crate) const SERVERS_FOLDER: &str = "servers";
/// Who the session plays when no credentials name a character.
const UNNAMED_CHARACTER: &str = "player";
/// GMCP modules every server is asked for.
const CORE_GMCP: [&str; 3] = ["Core 1", "Char 1", "Room 1"];
/// The word list map words are kept under.
const MAP_WORDS: &str = "map";
/// Notices kept for a shell to show.
const MOST_NOTICES: usize = 32;

/// What one connection knows: made fresh for each, so a reconnect learns the
/// server's prompt again from nothing.
struct Link {
    parser: telnet::Parser,
    pipeline: LinePipeline,
    defaults: Defaults,
    inventory: Inventory,
    inventory_asked: bool,
    login: Option<Login>,
    hooks: LuauHooks,
    hook_outputs: UnboundedReceiver<HookOutput>,
    /// Plugins already reported for hook events dropped on a full queue.
    drops_noticed: HashSet<PluginId>,
    mssp_name: Option<String>,
    gmcp_enabled: bool,
}

/// Hook outputs, on their way from the hook worker to the core thread.
struct HookOutputs(UnboundedSender<HookOutput>);

impl HookSink for HookOutputs {
    fn output(&mut self, output: HookOutput) {
        let _ = self.0.send(output);
    }
}

/// One server's session, for a core thread. Lines and moves keep to memory
/// and queue their disk writes on the writer threads; opening, finishing, a
/// merge landing, and an override coming on with the server's name (each of
/// those two reloads the map) wait on the disk.
///
/// Feed it what happens, call [`Engine::settle`] after each batch of input to
/// publish what changed, then send [`Engine::take_output`] to the server.
pub struct Engine {
    config: SessionConfig,
    hub: Arc<StoreHub>,
    typing: Arc<Mutex<Typing>>,
    history_writer: HistoryWriter,
    database: Database,
    writer: Writer,
    plugins: PluginHost,
    map: LiveMap,
    walk: WalkSyntax,
    link: Option<Link>,
    character_id: String,
    settings: SettingsState,
    /// Which fields hold the vitals, by plugin set and whether any were
    /// learned yet.
    hud_fields: Memo<(u64, bool), FieldMap>,
    /// Lines for the terminal store, at the next settle.
    terminal: Vec<Line>,
    /// Plugin writes for the database, at the next settle.
    batch: Batch,
    /// Rows waiting out a merge, while one lands.
    held: Option<Batch>,
    output: Vec<u8>,
    lines: Vec<Line>,
    events: Vec<PluginEvent>,
}

impl Engine {
    /// Opens the server's database and loads what's saved into the stores.
    /// This touches the disk: call it from a core thread.
    pub fn open(
        config: SessionConfig,
        hub: Arc<StoreHub>,
        typing: Arc<Mutex<Typing>>,
    ) -> Result<Engine, SessionError> {
        let now = Timestamp::now();
        let database = Database::open(&config.data_folder.join(SERVERS_FOLDER), &config.host)?;
        let writer = database.writer()?;
        let history_path = config.data_folder.join(HISTORY_FILE_NAME);
        let mut settings = SettingsState {
            server: Some(config.host.clone()),
            ..SettingsState::default()
        };
        match load_history(&history_path) {
            Ok(history) => lock(&typing).history = history,
            Err(error) => settings
                .notices
                .push(format!("couldn't read the command history: {error}")),
        }
        let character_id = config.credentials.as_ref().map_or_else(
            || UNNAMED_CHARACTER.to_owned(),
            |credentials| credentials.name.to_lowercase(),
        );
        let mut batch = Batch::new();
        batch.upsert(Server {
            id: config.host.clone(),
            name: config.host.clone(),
            host: config.host.clone(),
            port: config.port,
            settings: "{}".to_owned(),
            updated: now,
        });
        batch.upsert(Character {
            id: character_id.clone(),
            server_id: config.host.clone(),
            name: character_id.clone(),
            updated: now,
        });
        writer.submit(batch)?;

        let plugins = PluginHost::new([defaults::plugin()])?;
        let walk = WalkSyntax::of(&plugins);
        let mut engine = Engine {
            map: LiveMap::new(Box::new(PluginRules::of(&plugins))),
            config,
            hub,
            typing,
            history_writer: HistoryWriter::new(history_path),
            database,
            writer,
            plugins,
            walk,
            link: None,
            character_id,
            settings,
            hud_fields: Memo::default(),
            terminal: Vec::new(),
            batch: Batch::new(),
            held: None,
            output: Vec::new(),
            lines: Vec::new(),
            events: Vec::new(),
        };
        engine.suggest_game();
        engine.enable_plugins_for_server();
        engine.load_saved()?;
        engine.walk = WalkSyntax::of(&engine.plugins);
        engine.publish_settings();
        Ok(engine)
    }

    /// Starts a connection: a fresh parser, pipeline and defaults, and fresh
    /// hook VMs. Bytes can be fed from here on, from a socket or a recording.
    pub fn start_connection(&mut self) -> io::Result<()> {
        let (outputs, hook_outputs) = unbounded_channel();
        let state = Arc::new(HubView(Arc::clone(&self.hub)));
        // A hook file's top level runs when it loads, so a game plugin that's
        // off gets no VM. An override can still come on with the server's name.
        let may_run = self.config.plugins.iter().filter(|plugin| {
            plugin.layer() == Layer::Override || self.config.enabled.contains(plugin.id())
        });
        let mut hooks = LuauHooks::start(
            may_run,
            self.config.hook_limits,
            state,
            HookOutputs(outputs),
        )?;
        for (plugin, error) in hooks.failed_to_load() {
            self.notice(format!("{plugin}'s hooks didn't load: {error}"));
        }
        for plugin in &self.config.approved {
            hooks.set_approved(plugin, true);
        }
        self.link = Some(Link {
            parser: telnet::Parser::new(),
            pipeline: LinePipeline::new(self.encoding()),
            defaults: Defaults::default(),
            inventory: Inventory::default(),
            inventory_asked: false,
            login: self.new_login(),
            hooks,
            hook_outputs,
            drops_noticed: HashSet::new(),
            mssp_name: None,
            gmcp_enabled: false,
        });
        self.settings.connection = ConnectionStatus::Connecting;
        self.settings.echo_off = false;
        self.publish_settings();
        Ok(())
    }

    /// The socket opened.
    pub fn connected(&mut self) {
        self.settings.connection = ConnectionStatus::Connected;
        self.publish_settings();
        if let Some(sync) = &self.config.sync {
            sync.connected();
        }
    }

    /// The server's next bytes, in any chunking. Corrupt compressed data
    /// fails the connection: nothing after it can be read.
    pub fn receive(&mut self, bytes: Bytes, now: Timestamp) -> Result<(), CompressionError> {
        let Some(link) = &mut self.link else {
            return Ok(());
        };
        let mut parsed = Vec::new();
        let fed = link.parser.feed(bytes, &mut parsed);
        self.output.extend(link.parser.take_output());
        for event in parsed {
            self.server_said(event, now);
        }
        fed
    }

    /// The server has gone quiet. Text it left unended won't be followed
    /// soon: a prompt on a server with no GA, which ends here, so the player
    /// sees it and the learner can use it.
    pub fn quiet(&mut self, now: Timestamp) {
        let Some(link) = &mut self.link else { return };
        if link
            .pipeline
            .pending()
            .is_some_and(|pending| !pending.plain.trim().is_empty())
        {
            self.lines.extend(link.pipeline.prompt_end());
            self.read_lines(now);
        }
    }

    /// What the player typed. With echo off it's a password: it goes to the
    /// server as typed, and nowhere else.
    pub fn send(&mut self, typed: &str, now: Timestamp) {
        if self.link.is_none() {
            return;
        }
        if self.settings.echo_off {
            write_line(typed, self.encoding(), &mut self.output);
            return;
        }
        let mut typing = lock(&self.typing);
        if typing.history.record(typed, false) {
            self.history_writer.save(&typing.history);
        }
        drop(typing);
        self.terminal.push(Line::command(typed));
        self.issue(typed, now);
    }

    /// Publishes what changed since the last settle: the stores, and the
    /// rows for the database. A move's map work finishes here.
    pub fn settle(&mut self, now: Timestamp) {
        self.take_hook_outputs(now);
        let mut notices = Vec::new();
        if let Some(link) = &mut self.link {
            for (plugin, turned_off) in link.hooks.turn_off_overrunning(&mut self.plugins) {
                if let Err(error) = turned_off {
                    notices.push(format!("{plugin} couldn't be turned off: {error}"));
                }
            }
            for plugin in link.hooks.dropped().keys() {
                if link.drops_noticed.insert(plugin.clone()) {
                    notices.push(format!(
                        "{plugin} is missing lines: its hooks fell behind the server"
                    ));
                }
            }
        }
        for notice in notices {
            self.notice(notice);
        }
        if let Some(commit) = self.map.commit(now) {
            self.publish_map(commit.change);
            self.submit(commit.batch);
        }
        if !self.terminal.is_empty() {
            let lines = std::mem::take(&mut self.terminal);
            self.hub
                .write::<TerminalState>(TerminalChange::Append(lines));
        }
        let batch = std::mem::take(&mut self.batch);
        self.submit(batch);
    }

    /// Bytes for the server, since the last call.
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.output)
    }

    /// The next thing a hook did, once one has.
    pub(super) async fn next_hook_output(&mut self) -> HookOutput {
        match &mut self.link {
            Some(Link { hook_outputs, .. }) => match hook_outputs.recv().await {
                Some(output) => output,
                None => std::future::pending().await,
            },
            None => std::future::pending().await,
        }
    }

    /// Acts on what a hook did.
    pub(super) fn hook_output(&mut self, output: HookOutput, now: Timestamp) {
        match output {
            HookOutput::StoreWrite(write) => self.plugin_store_write(write, now),
            HookOutput::SendCommand(send) => {
                // What's being typed now is a password; a hook's command
                // would land in the password box.
                if self.link.is_some() && !self.settings.echo_off {
                    self.terminal.push(Line::command(&send.command));
                    self.issue(&send.command, now);
                }
            }
            HookOutput::RefreshInventory => {
                if self.link.as_ref().is_some_and(|link| link.gmcp_enabled) {
                    gmcp::encode(inventory::ASK, "", &mut self.output);
                }
            }
            HookOutput::HookFailed(failed) => self.notice(format!(
                "{}'s hook {} failed: {}",
                failed.plugin,
                failed.hook.as_str(),
                failed.message
            )),
            HookOutput::PluginDisabled(disabled) => self.notice(format!(
                "{} was turned off: {:?}",
                disabled.plugin, disabled.reason
            )),
        }
    }

    /// The connection ended. Runs what its hooks still hold, so nothing they
    /// read is lost; that can wait on a slow hook, so call it on a core
    /// thread.
    pub fn close_connection(&mut self, reason: DisconnectReason, now: Timestamp) {
        // First, so a half row flushed next can't pass for the table's end;
        // again, as that half line may start a table.
        self.plugins.cut_tables(&mut self.events);
        self.route_events(now);
        self.quiet(now);
        self.plugins.cut_tables(&mut self.events);
        self.route_events(now);
        if let Some(link) = &mut self.link {
            link.hooks.wait_until_idle();
        }
        self.settle(now);
        let was_connected = self.settings.connection == ConnectionStatus::Connected;
        self.link = None;
        self.map.connection_lost();
        self.settings.connection = ConnectionStatus::Closed(reason);
        self.settings.echo_off = false;
        self.publish_settings();
        if let Some(sync) = self.config.sync.as_ref().filter(|_| was_connected) {
            // Sync exports only once the session's last rows are in.
            let (drained, last_rows_in) = sync_channel(1);
            if self.writer.notify_when_drained(drained).is_ok() {
                sync.disconnected(last_rows_in);
            }
        }
    }

    /// Sends what's queued for the database on its way, and keeps later rows
    /// here until [`Engine::release_writes`]. `drained` answers once the queue
    /// is done, with no waiting on this thread.
    pub(super) fn hold_writes(&mut self, drained: SyncSender<()>) {
        self.held.get_or_insert_default();
        if let Err(error) = self.writer.notify_when_drained(drained) {
            self.notice(error.to_string());
        }
    }

    pub(super) fn is_holding_writes(&self) -> bool {
        self.held.is_some()
    }

    /// A merge has landed: writes what was held, moved off any retired room,
    /// and rebuilds the map from the database. Rows of the merge's window reach
    /// the disk first so the new map holds them. This waits on the disk, which
    /// only a merge during play costs.
    pub(super) fn release_writes(&mut self, moves: &RoomMoves) {
        self.settle(Timestamp::now());
        let Some(mut held) = self.held.take() else {
            return;
        };
        repoint_batch(&mut held, moves);
        self.submit(held);
        if let Err(error) = self
            .writer
            .flush()
            .map_err(SessionError::from)
            .and_then(|()| self.reload_map())
        {
            self.notice(error.to_string());
        }
    }

    /// Replaces the map with what the database holds, for a merge it can't
    /// have seen. Views lose the rooms that are gone.
    fn reload_map(&mut self) -> Result<(), SessionError> {
        let revision = self.map.revision();
        self.load_saved()?;
        self.map.follows_revision(revision);
        let removed = self.hub.read(|map: &Versioned<MapState>| {
            map.value()
                .rooms()
                .map(|room| room.id)
                .filter(|id| !self.map.holds(*id))
                .collect()
        });
        self.hub.write::<MapState>(MapChange {
            removed,
            whereabouts: Some(self.map.whereabouts()),
            revision: self.map.revision(),
            ..MapChange::default()
        });
        Ok(())
    }

    /// The connection can't go on: closes it, saying why.
    pub(super) fn fail_connection(&mut self, error: impl fmt::Display, now: Timestamp) {
        self.close_connection(DisconnectReason::Failed(error.to_string()), now);
    }

    /// Ends the session, waiting for every write to reach the disk.
    pub fn finish(mut self) -> Result<(), SessionError> {
        let now = Timestamp::now();
        if self.link.is_some() {
            self.close_connection(DisconnectReason::ClosedByClient, now);
        }
        self.settle(now);
        if let Some(held) = self.held.take() {
            self.submit(held);
        }
        let flushed = self.writer.flush();
        let saved = self.history_writer.finish();
        flushed?;
        Ok(saved?)
    }

    fn server_said(&mut self, event: Event, now: Timestamp) {
        match event {
            Event::Data(text) => {
                let Some(link) = &mut self.link else { return };
                link.pipeline.push(&text, &mut self.lines);
                self.read_lines(now);
                self.read_pending(now);
            }
            Event::PromptEnd => {
                let Some(link) = &mut self.link else { return };
                let line = link.pipeline.prompt_end();
                self.lines.extend(line.clone());
                self.read_lines(now);
                if let Some(line) = line {
                    self.plugins.end_tables_at_signal(&line, &mut self.events);
                    self.route_events(now);
                }
            }
            Event::Gmcp { package, data } => self.gmcp(&package, &data, now),
            Event::GmcpEnabled => {
                if let Some(link) = &mut self.link {
                    link.gmcp_enabled = true;
                }
                self.subscribe_gmcp();
            }
            Event::Echo(echo) => {
                self.settings.echo_off = !echo;
                self.publish_settings();
            }
            Event::Mssp(name) => {
                if let Some(link) = &mut self.link {
                    link.mssp_name = Some(name);
                }
                self.game_signal_arrived(now);
            }
            Event::SubnegotiationDropped { option } => {
                self.notice(format!("dropped an oversized message for option {option}"));
            }
        }
    }

    /// Reads each finished line once: defaults, plugins, login, the map, the
    /// plugins' events, then the terminal. A prompt it opens with ends the
    /// open tables, and the plugins read what's glued after it.
    fn read_lines(&mut self, now: Timestamp) {
        let encoding = self.encoding();
        let mut lines = std::mem::take(&mut self.lines);
        for line in lines.drain(..) {
            let Some(link) = &mut self.link else { return };
            let text = line.plain();
            let at_signal = line.end() == LineEnd::PromptEnd;
            let prompt = if at_signal && self.plugins.settings().is_pager(text) {
                link.defaults.read_pager_line();
                None
            } else {
                link.defaults.read_line(&line)
            };
            if prompt.is_some() {
                self.plugins.end_tables(&line, &mut self.events);
            }
            let output_at = prompt.filter(|&at| at < text.len()).unwrap_or(0);
            let coloring = self
                .plugins
                .read_line_from(&line, output_at, &mut self.events);
            answer_login(&mut link.login, text, encoding, &mut self.output);
            let body = &text[prompt.unwrap_or(0)..];
            if prompt.is_none() || !body.is_empty() {
                self.map.line(body, now);
            }
            self.route_events(now);
            let Some(link) = &self.link else { return };
            let hide = link.defaults.hide_prompts && link.defaults.hud.has_learned();
            let shown = if coloring.is_empty() {
                line.clone()
            } else {
                line.colored(&coloring)
            };
            match prompt.filter(|_| hide) {
                Some(at) if at == text.len() => {}
                Some(at) => self.terminal.push(shown.after(at)),
                None => self.terminal.push(shown),
            }
        }
        // The buffer is kept: it's reused for every read.
        self.lines = lines;
    }

    /// Sends what the plugins matched to the map and to the hooks.
    fn route_events(&mut self, now: Timestamp) {
        let Some(link) = &mut self.link else { return };
        for event in self.events.drain(..) {
            match event.map_signal {
                Some(MapSignal::Exits) => self.map.exits_line(),
                Some(MapSignal::FailedMove) => self.map.move_failed(now),
                Some(MapSignal::Darkness) => self.map.darkness(),
                Some(MapSignal::Flee) => {
                    if let Some(direction) = event.capture("dir").and_then(Direction::parse) {
                        self.map.fled(direction, now);
                    }
                }
                Some(MapSignal::Aggressor) => {
                    if let Some(name) = event.capture("name") {
                        self.map.aggressor(name);
                    }
                }
                None => {}
            }
            link.hooks.event(event);
        }
    }

    /// Looks at the text the server hasn't ended. Call it after every read.
    fn read_pending(&mut self, now: Timestamp) {
        let encoding = self.encoding();
        let Some(link) = &mut self.link else { return };
        let Some(pending) = link.pipeline.pending() else {
            return;
        };
        let kind = self.plugins.read_pending(&pending);
        answer_login(&mut link.login, pending.plain, encoding, &mut self.output);
        if self.plugins.table_open() {
            link.defaults.pager.begin_table_read();
        } else {
            link.defaults.pager.end_table_read();
        }
        let action = link.defaults.read_pending(&pending, kind);
        match action {
            PendingAction::Wait => {}
            PendingAction::Send(key) => {
                write_line(&key, encoding, &mut self.output);
                self.plugins.pager_answered();
            }
            PendingAction::EndPrompt => {
                self.lines.extend(link.pipeline.prompt_end());
                self.read_lines(now);
            }
        }
    }

    fn gmcp(&mut self, package: &str, json: &str, now: Timestamp) {
        let Some(link) = &mut self.link else { return };
        link.defaults.gmcp_arrived(package, json);
        if let Some(entries) = link.inventory.read(package, json) {
            self.hub.write::<CharactersState>(CharactersState(entries));
        }
        // The first room means the character is in the game.
        if !link.inventory_asked && package.eq_ignore_ascii_case("Room.Info") {
            link.inventory_asked = true;
            gmcp::encode(inventory::ASK, "", &mut self.output);
        }
        let learned = link.defaults.hud.has_learned();
        let hud = &link.defaults.hud;
        let plugins = &self.plugins;
        let fields = self
            .hud_fields
            .get((plugins.generation(), learned), || hud.fields(plugins));
        if fields
            .values()
            .any(|field| field.package.eq_ignore_ascii_case(package))
            && let Ok(message) = serde_json::from_str::<Map<String, Value>>(json)
        {
            let vitals = hud::values(fields, package, &message).into_iter().collect();
            self.hub.write::<VitalsState>(VitalsState(vitals));
        }
        self.map.gmcp(package, json, now);
        if package.eq_ignore_ascii_case("Core.Hello") {
            self.game_signal_arrived(now);
        }
        self.read_pending(now);
    }

    /// The server named its game, perhaps. That can suggest a game plugin,
    /// and bring on an override for that name: then the map reloads under
    /// its rules, and the connection takes up its settings.
    fn game_signal_arrived(&mut self, now: Timestamp) {
        self.suggest_game();
        if self.enable_plugins_for_server() {
            self.settle(now);
            if let Err(error) = self.writer.flush().map_err(SessionError::from) {
                self.notice(format!("couldn't save the map: {error}"));
            }
            if let Err(error) = self.load_saved() {
                self.notice(format!("couldn't load the map: {error}"));
            }
            self.plugins_changed();
        }
    }

    /// Publishes the game plugin the server's signals suggest. It turns
    /// nothing on: that's the player's click, which reaches the session as
    /// [`SessionConfig::enabled`]. The core only compares what each plugin's
    /// manifest declares: it knows no game itself.
    fn suggest_game(&mut self) {
        if self.settings.suggestion.is_some() {
            return;
        }
        let link = self.link.as_ref();
        let signals = ServerSignals {
            host: &self.config.host,
            mssp_name: link.and_then(|link| link.mssp_name.as_deref()),
            gmcp_hello: link.and_then(|link| link.defaults.gmcp_hello()),
        };
        if let Some(suggestion) = suggest(&self.config.plugins, signals) {
            self.settings.suggestion = Some(suggestion);
            self.publish_settings();
        }
    }

    /// Adds the game plugins the config has on and the overrides for this
    /// server, those not added yet. Returns whether any went on.
    fn enable_plugins_for_server(&mut self) -> bool {
        let mssp_name = self
            .link
            .as_ref()
            .and_then(|link| link.mssp_name.as_deref());
        let server = ServerInfo {
            host: &self.config.host,
            mssp_name,
        };
        let wanted: Vec<Plugin> = self
            .config
            .plugins
            .iter()
            .filter(|plugin| match plugin.layer() {
                Layer::Override => plugin.manifest.matches_server(server),
                _ => self.config.enabled.contains(plugin.id()),
            })
            .filter(|plugin| self.plugins.is_enabled(plugin.id()).is_none())
            .cloned()
            .collect();
        let enabled = !wanted.is_empty();
        for plugin in wanted {
            let id = plugin.id().clone();
            if let Err(error) = self.plugins.add(plugin) {
                self.notice(format!("{id} couldn't be turned on: {error}"));
            }
        }
        enabled
    }

    /// Brings a live connection up to date with the enabled plugins.
    fn plugins_changed(&mut self) {
        self.walk = WalkSyntax::of(&self.plugins);
        let encoding = self.encoding();
        let login = self.new_login();
        let Some(link) = &mut self.link else { return };
        link.pipeline.set_encoding(encoding);
        link.login = login;
        self.subscribe_gmcp();
    }

    /// Loads the map, the mobs and the plugin stores from the database,
    /// under the rules of the plugins on now.
    fn load_saved(&mut self) -> Result<(), SessionError> {
        let reader = self.database.reader()?;
        let (rows, marks, meta) = (reader.map()?, reader.mob_marks()?, reader.meta()?);
        let stats = reader.characters()?.stats;
        let rules = Box::new(PluginRules::of(&self.plugins));
        self.map = LiveMap::load(rules, &rows, &marks, &meta);

        let change = self.map.snapshot();
        self.add_map_words(&change.rooms);
        self.hub.write::<MapState>(change);
        self.publish_room();
        let mut mobs = MobsState::default();
        for mark in Mark::ALL {
            let names = self.map.mobs().marks(mark).iter();
            mobs.marks.extend(names.map(|name| (mark, name.clone())));
        }
        self.hub.write::<MobsState>(mobs);
        let character = stats
            .iter()
            .filter(|stat| stat.character_id == self.character_id)
            .filter_map(|stat| Some((stat.key.clone(), values::from_row(&stat.value)?)))
            .collect();
        self.hub
            .write::<CharactersState>(CharactersState(character));
        let gear = meta
            .iter()
            .filter_map(|entry| {
                let key = entry.key.strip_prefix(values::GEAR_META_PREFIX)?;
                Some((key.to_owned(), values::from_row(&entry.value)?))
            })
            .collect();
        self.hub.write::<GearState>(GearState(gear));
        Ok(())
    }

    /// What the player or a hook sent, as the lines that go out. Each step
    /// tells the map what to expect, one per move, however they're packed.
    fn issue(&mut self, typed: &str, now: Timestamp) {
        let encoding = self.encoding();
        if typed.trim().is_empty() {
            write_line("", encoding, &mut self.output);
            return;
        }
        let mut commands = Vec::new();
        for command in split_chain(typed) {
            match self.walk.expand(command) {
                Some(expanded) => {
                    let steps = parse_speedwalk(command).expect("it expanded");
                    for step in steps {
                        for _ in 0..step.count {
                            self.map.sent(step.direction.word(), now);
                        }
                    }
                    commands.extend(expanded);
                }
                None => {
                    self.map.sent(command, now);
                    commands.push(command.to_owned());
                }
            }
        }
        let separator = self.plugins.settings().separator.as_deref();
        for line in chain_lines(&commands, separator) {
            write_line(&line, encoding, &mut self.output);
        }
    }

    fn plugin_store_write(&mut self, write: StoreWrite, now: Timestamp) {
        let StoreWrite {
            store, key, value, ..
        } = write;
        let key = format!("{store}.{key}");
        let row = values::to_row(&value);
        let change = BTreeMap::from([(key.clone(), value)]);
        if values::is_gear_store(&store) {
            self.hub.write::<GearState>(GearState(change));
            self.batch.upsert(persist::Meta {
                key: format!("{}{key}", values::GEAR_META_PREFIX),
                value: row,
                updated: now,
            });
        } else {
            self.hub.write::<CharactersState>(CharactersState(change));
            self.batch.upsert(persist::CharacterStat {
                character_id: self.character_id.clone(),
                key,
                value: row,
                updated: now,
            });
        }
    }

    fn take_hook_outputs(&mut self, now: Timestamp) {
        while let Some(output) = self
            .link
            .as_mut()
            .and_then(|link| link.hook_outputs.try_recv().ok())
        {
            self.hook_output(output, now);
        }
    }

    fn publish_map(&mut self, change: MapChange) {
        self.add_map_words(&change.rooms);
        let room_changed = change.whereabouts.is_some()
            || self
                .map
                .whereabouts()
                .room
                .is_some_and(|id| change.rooms.iter().any(|room| room.id == id));
        if !change.marks.is_empty() {
            let marks = self
                .hub
                .read(|mobs: &Versioned<MobsState>| mobs.value().with_changes(&change.marks));
            self.hub.write::<MobsState>(marks);
        }
        self.hub.write::<MapState>(change);
        if room_changed {
            self.publish_room();
        }
    }

    fn publish_room(&self) {
        self.hub.write::<RoomState>(RoomState {
            room: self.map.shared_current_room(),
            whereabouts: self.map.whereabouts(),
        });
    }

    /// Room and mob names, for Tab. Only names go in: never what's typed.
    fn add_map_words(&self, rooms: &[Arc<MapRoom>]) {
        if rooms.is_empty() {
            return;
        }
        let mut typing = lock(&self.typing);
        for room in rooms {
            typing.known.add_words(MAP_WORDS, words_of(&room.name));
            for title in room.sightings.keys() {
                typing
                    .known
                    .add_words(MAP_WORDS, words_of(&self.map.mobs().name(title)));
            }
        }
    }

    fn submit(&mut self, batch: Batch) {
        if let Some(held) = &mut self.held {
            held.extend(batch);
            return;
        }
        let queued = !batch.is_empty();
        if let Err(error) = self.writer.submit(batch) {
            self.notice(error.to_string());
        }
        if queued {
            let failures = self.writer.take_failures();
            if !failures.is_empty() {
                self.notice(persist::Error::BatchesFailed(failures).to_string());
            }
        }
    }

    fn subscribe_gmcp(&mut self) {
        if !self.link.as_ref().is_some_and(|link| link.gmcp_enabled) {
            return;
        }
        let mut modules: Vec<String> = CORE_GMCP.map(str::to_owned).into();
        for (_, connection) in self.plugins.declared(|points| points.connection.as_ref()) {
            modules.extend(connection.gmcp.iter().cloned());
        }
        modules.sort();
        modules.dedup();
        gmcp::encode_hello(
            &self.config.client_name,
            &self.config.client_version,
            &mut self.output,
        );
        gmcp::encode_supports_set(&modules, &mut self.output);
    }

    fn new_login(&self) -> Option<Login> {
        Login::new(&self.plugins, self.config.credentials.clone()?)
    }

    fn encoding(&self) -> Encoding {
        self.plugins.settings().encoding.unwrap_or_default()
    }

    fn notice(&mut self, notice: String) {
        let notices = &mut self.settings.notices;
        if notices.len() == MOST_NOTICES {
            notices.remove(0);
        }
        notices.push(notice);
        self.publish_settings();
    }

    fn publish_settings(&self) {
        self.hub.write::<SettingsState>(self.settings.clone());
    }

    pub(super) fn host(&self) -> &str {
        &self.config.host
    }

    pub(super) fn port(&self) -> u16 {
        self.config.port
    }

    pub(super) fn connect_timeout(&self) -> Duration {
        self.config.connect_timeout
    }
}

/// Answers `text` if it's a login prompt still to come.
fn answer_login(login: &mut Option<Login>, text: &str, encoding: Encoding, output: &mut Vec<u8>) {
    let Some(prompts) = login else { return };
    if let Some(reply) = prompts.answer(text) {
        write_line(reply, encoding, output);
    }
    if prompts.is_done() {
        *login = None;
    }
}

/// Puts a line of text on the wire, in the server's encoding.
fn write_line(text: &str, encoding: Encoding, output: &mut Vec<u8>) {
    telnet::encode_line(&encoding.encode(text), output);
}

fn lock(typing: &Mutex<Typing>) -> MutexGuard<'_, Typing> {
    typing.lock().unwrap_or_else(PoisonError::into_inner)
}
