//! The session's work, with no threads or sockets of its own: bytes in;
//! stores, database writes and bytes to send out. [`crate::session::Session`]
//! runs it on a core thread. A replay or a benchmark can drive it directly.

use super::game::GameFeed;
use super::gear::{Filing, GearKeeper, GearWatch, Saving};
use super::inventory::{self, Inventory};
use super::latency::Latency;
use super::login::Login;
use super::map_rules::PluginRules;
use super::quiet::{Moment, QuietReads, Seen};
use super::sheet::Sheet;
use super::values::{self, HubView};
use super::{GearEdit, MapEdit, RuleEdit, SessionConfig, SessionError, Typing};
use crate::completion::{HISTORY_FILE_NAME, HistoryWriter, load_history, words_of};
use crate::defaults::aliases::{AliasRule, AliasSet};
use crate::defaults::commands::{
    CommandVariety, WalkSyntax, chain_lines, parse_speedwalk, split_chain,
};
use crate::defaults::detect::{ServerSignals, suggest};
use crate::defaults::hud::{self, Field, FieldMap, GameFields};
use crate::defaults::triggers::{Scope, TriggerResponse, TriggerRule};
use crate::defaults::{self, Defaults, PendingAction, hotbar};
use crate::line::{Encoding, Line, LineEnd, LineKind, LinePipeline};
use crate::logs::SessionLogs;
use crate::map::{
    Direction, JoinDone, LastJoin, LiveMap, MapChange, MapRoom, Mark, RoomId, VERSION_AUTOSAVE,
    VERSION_JOIN, VERSION_SAFETY, room_pair, undo_pair,
};
use crate::merge::{self, RoomMoves, repoint_batch};
use crate::net::DisconnectReason;
use crate::persist::{
    self, Batch, Character, Database, Key, Meta, NOTEPAD, REMOVED, SERVERS_FOLDER, Server,
    Timestamp, UNNAMED_CHARACTER, Writer,
};
use crate::plugin::inspect::{Claiming, InspectedLine};
use crate::plugin::luau::{
    HookOutput, HookSink, LuauHooks, PluginDisabled, StoreWrite, Value as HookValue,
};
use crate::plugin::{
    Layer, MapSignal, PendingKind, Plugin, PluginEvent, PluginHost, PluginId, PluginSink,
    ServerInfo,
};
use crate::repo::format::ServerRows;
use crate::store::{
    CHARACTER_LEVEL, CHARACTER_NAME, CHARACTER_POSITION, CharactersState, ConnectionStatus,
    CuesState, GearState, InspectorChange, InspectorState, MapRestore, MapState, Memo, MobsState,
    RoomState, RulesState, SettingsState, SheetState, StoreHub, TargetState, TerminalChange,
    TerminalState, Versioned, VitalsState,
};
use crate::telnet::{self, Bytes, CompressionError, Event, gmcp};
use serde_json::{Map, Value};
use std::collections::hash_map::RandomState;
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::hash::{BuildHasher, Hasher};
use std::io;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// GMCP modules every server is asked for.
const CORE_GMCP: [&str; 3] = ["Core 1", "Char 1", "Room 1"];
/// The word list map words are kept under.
const MAP_WORDS: &str = "map";
/// How many room names and mob titles are remembered as added to the map's
/// words, each, before the memory starts over.
const NAMES_REMEMBERED: usize = 4096;
/// Notices kept for a shell to show.
const MOST_NOTICES: usize = 32;
/// How often plugin costs are published: each one sorts its samples.
const COSTS_EVERY_MS: i64 = 2_000;
/// A changed map is saved as a version this often during play.
const AUTOSAVE_EVERY_MS: i64 = 5 * 60 * 1000;
const AUTOSAVED: &str = "Autosave";
const SESSION_START: &str = "Session start";

/// What one connection knows: made fresh for each, so a reconnect learns the
/// server's prompt again from nothing.
struct Link {
    parser: telnet::Parser,
    pipeline: LinePipeline,
    defaults: Defaults,
    inventory: Inventory,
    inventory_asked: bool,
    gear: GearWatch,
    game: GameFeed,
    quiet: QuietReads,
    latency: Latency,
    login: Option<Login>,
    variety: CommandVariety,
    /// How much of the pending line the chat already shows.
    pending_shown: usize,
    hooks: LuauHooks,
    hook_outputs: UnboundedReceiver<HookOutput>,
    /// Plugins already reported for hook events dropped on a full queue.
    drops_noticed: HashSet<PluginId>,
    /// The notices of overruns, this connection, which the plugin being
    /// turned off then takes the place of.
    overrun_notices: Vec<(PluginId, String)>,
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

/// The room names and mob titles whose words the map's words already hold.
/// A mob's name depends on the plugins, so a change of them starts it over.
#[derive(Default)]
struct AddedNames {
    generation: u64,
    rooms: HashSet<String>,
    mobs: HashSet<String>,
}

impl AddedNames {
    fn keep_to(&mut self, generation: u64) {
        let full = self.rooms.len() > NAMES_REMEMBERED || self.mobs.len() > NAMES_REMEMBERED;
        if self.generation != generation || full {
            *self = AddedNames {
                generation,
                ..AddedNames::default()
            };
        }
    }
}

/// One server's session, for a core thread. Lines and moves keep to memory
/// and queue their disk writes on the writer threads; opening, finishing, a
/// merge landing, and an override coming on with the server's name (each of
/// those two reloads the map) wait on the disk, as does a restore of the map,
/// which nothing may build on until it's safe, and a join or its undo, which
/// rebuild the map from what they wrote.
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
    logs: SessionLogs,
    plugins: PluginHost,
    map: LiveMap,
    walk: WalkSyntax,
    link: Option<Link>,
    /// The sheet of the character [`Engine::playing`] names.
    sheet: Sheet,
    /// The command field holds text the player hasn't sent.
    typing_text: bool,
    settings: SettingsState,
    rules: RulesState,
    /// The triggers in the matcher: those on, for this character.
    running_triggers: Vec<TriggerRule>,
    /// Sounds for the cues store, at the next settle.
    sounds: Vec<String>,
    /// How many rules this session has given an id.
    rules_made: u32,
    /// Which fields hold the vitals, by plugin set and whether any were
    /// learned yet.
    hud_fields: Memo<(u64, bool), FieldMap>,
    /// Where affects are listed, by plugin set.
    affects_field: Memo<u64, Option<Field>>,
    /// Where the character sheet's values and the opponent's are, by plugin
    /// set.
    game_fields: Memo<u64, GameFields>,
    /// The room's contents changed since the last settle.
    contents_changed: bool,
    /// The game or a plugin named the character, at the next settle.
    character_renamed: bool,
    /// Who plays on this connection, by id: what the login, the game or a
    /// plugin has said since it opened, and nothing from an earlier one.
    playing: String,
    /// When plugin costs were last published.
    costs_published: Option<Timestamp>,
    /// The plugin set the shared link rules came from.
    link_rules_of: u64,
    /// What the map's words already hold, to add each only once.
    added_names: AddedNames,
    /// Lines for the terminal store, at the next settle.
    terminal: Vec<Line>,
    /// Whether the inspector is open, which is what pays for its record.
    inspecting: bool,
    /// Lines for the inspector store, at the next settle.
    inspected: Vec<InspectedLine>,
    /// The last line shown was a blank one from the server, or none has been.
    blank_shown: bool,
    /// What's known about items, and what the character has on them.
    gear: GearKeeper,
    /// Plugin writes for the database, at the next settle.
    batch: Batch,
    /// Rows waiting out a merge, while one lands.
    held: Option<Batch>,
    /// Rooms changed since the last version of the map was saved. A new
    /// session counts as such a change: it opens with a version.
    map_changed: bool,
    /// When the last version of the map was saved.
    version_saved: Timestamp,
    /// Hears when versions on their way to the disk have landed.
    versions_landing: Option<Receiver<()>>,
    restores: u64,
    joins_answered: u64,
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
            id: UNNAMED_CHARACTER.to_owned(),
            server_id: config.host.clone(),
            name: UNNAMED_CHARACTER.to_owned(),
            updated: now,
        });
        // Playing on a server the player removed puts it back on their list.
        batch.delete(Key::Meta(REMOVED.to_owned()));
        writer.submit(batch)?;

        let plugins = PluginHost::new([defaults::plugin()])?;
        let walk = WalkSyntax::of(&plugins);
        let mut engine = Engine {
            map: LiveMap::new(Box::new(PluginRules::of(&plugins))),
            logs: SessionLogs::new(&config.data_folder, &config.host),
            config,
            hub,
            typing,
            history_writer: HistoryWriter::new(history_path),
            database,
            writer,
            plugins,
            walk,
            link: None,
            sheet: Sheet::default(),
            typing_text: false,
            settings,
            rules: RulesState::default(),
            running_triggers: Vec::new(),
            sounds: Vec::new(),
            rules_made: 0,
            hud_fields: Memo::default(),
            affects_field: Memo::default(),
            game_fields: Memo::default(),
            contents_changed: false,
            character_renamed: false,
            playing: String::new(),
            costs_published: None,
            link_rules_of: 0,
            added_names: AddedNames::default(),
            terminal: Vec::new(),
            inspecting: false,
            inspected: Vec::new(),
            blank_shown: true,
            gear: GearKeeper::default(),
            batch: Batch::new(),
            held: None,
            map_changed: true,
            version_saved: now,
            versions_landing: None,
            restores: 0,
            joins_answered: 0,
            output: Vec::new(),
            lines: Vec::new(),
            events: Vec::new(),
        };
        engine.suggest_game();
        engine.enable_plugins_for_server();
        engine.load_saved()?;
        engine.walk = WalkSyntax::of(&engine.plugins);
        engine.load_rules()?;
        engine.settings.map_versions = 1;
        engine.publish_settings();
        Ok(engine)
    }

    /// Starts a connection: a fresh parser, pipeline and defaults, and fresh
    /// hook VMs. Bytes can be fed from here on, from a socket or a recording.
    pub fn start_connection(&mut self) -> io::Result<()> {
        if self.map_changed && self.map.room_count() > 0 {
            self.save_map_version(VERSION_SAFETY, SESSION_START, Timestamp::now());
        }
        let (outputs, hook_outputs) = unbounded_channel();
        let state = Arc::new(HubView(Arc::clone(&self.hub)));
        // A hook file's top level runs when it loads, so a game plugin that's
        // off gets no VM. An override can still come on with the server's name.
        let may_run = self
            .config
            .plugins
            .iter()
            .filter(|plugin| match plugin.layer() {
                Layer::Override => !self.config.overrides_off.contains(plugin.id()),
                _ => self.config.enabled.contains(plugin.id()),
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
        let mut quiet = QuietReads::default();
        quiet.declare(&self.plugins);
        self.link = Some(Link {
            parser: telnet::Parser::new(),
            pipeline: LinePipeline::new(self.encoding()),
            defaults: Defaults::default(),
            inventory: Inventory::default(),
            inventory_asked: false,
            gear: GearWatch::default(),
            game: GameFeed::default(),
            quiet,
            latency: Latency::default(),
            login: self.new_login(),
            variety: CommandVariety::default(),
            pending_shown: 0,
            hooks,
            hook_outputs,
            drops_noticed: HashSet::new(),
            overrun_notices: Vec::new(),
            mssp_name: None,
            gmcp_enabled: false,
        });
        self.logs.connection_started();
        self.settings.connection = ConnectionStatus::Connecting;
        self.settings.echo_off = false;
        self.publish_settings();
        // Each connection starts unnamed. Until the game says who plays, it's
        // who's logging in, if anyone is.
        let logging_in = (self.config.credentials.as_ref())
            .map(|credentials| credentials.name.to_lowercase())
            .unwrap_or_default();
        self.character_named(logging_in, Timestamp::now());
        if let Some(credentials) = self
            .config
            .credentials
            .as_ref()
            .filter(|_| self.character_name().is_none())
        {
            let name = HookValue::Text(credentials.name.clone());
            let change = [(CHARACTER_NAME.to_owned(), name)];
            self.hub
                .write::<CharactersState>(CharactersState(change.into()));
        }
        self.rules_follow_the_character();
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
        let answered = link.latency.received(now);
        let mut parsed = Vec::new();
        let fed = link.parser.feed(bytes, &mut parsed);
        self.output.extend(link.parser.take_output());
        if answered.is_some() {
            self.settings.latency = answered;
            self.publish_settings();
        }
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
        let Some(link) = &mut self.link else { return };
        link.quiet.player_acted(typed, now);
        if self.settings.echo_off {
            write_line(typed, self.encoding(), &mut self.output);
            return;
        }
        let mut typing = lock(&self.typing);
        if typing.history.record(typed, false) {
            self.history_writer.save(&typing.history);
        }
        drop(typing);
        self.show_pending();
        self.show(Line::command(typed));
        self.issue_expanded(typed, now);
    }

    /// A command the player pressed. With echo off it would go as a password,
    /// so it does nothing. It isn't typing: history doesn't keep it.
    pub fn press(&mut self, command: &str, now: Timestamp) {
        if let Some(link) = self.link.as_mut().filter(|_| !self.settings.echo_off) {
            link.quiet.player_acted(command, now);
            self.show_pending();
            self.show(Line::command(command));
            self.issue_expanded(command, now);
        }
    }

    /// Applies a change the player made and saves it.
    pub fn edit(&mut self, edit: RuleEdit, now: Timestamp) {
        match edit {
            RuleEdit::SaveTrigger(rule) => self.save_trigger(rule, now),
            RuleEdit::RemoveTrigger(id) => {
                let triggers = &mut self.rules.triggers;
                if let Some(at) = triggers.iter().position(|rule| rule.id == id) {
                    let rule = triggers.remove(at);
                    self.remove_row(&rule.home, persist::Key::Trigger(rule.id));
                    self.run_triggers();
                }
            }
            RuleEdit::SaveAlias(rule) => self.save_alias(rule, now),
            RuleEdit::RemoveAlias(id) => {
                let aliases = &mut self.rules.aliases;
                if let Some(at) = aliases.iter().position(|rule| rule.id == id) {
                    let rule = aliases.remove(at);
                    self.remove_row(&rule.home, persist::Key::Alias(rule.id));
                    self.run_aliases();
                }
            }
            RuleEdit::SetHotbar(chips) => {
                self.batch.upsert(hotbar::to_meta(&chips, now));
                self.rules.hotbar = chips;
            }
            RuleEdit::SetNotepad(text) => {
                self.batch.upsert(Meta {
                    key: NOTEPAD.to_owned(),
                    value: text.clone(),
                    updated: now,
                });
                self.rules.notepad = text;
            }
        }
        self.publish_rules();
    }

    /// Applies a change the player made to the map. The next settle saves it
    /// and publishes it, as it does a change the game made. A join or its
    /// undo lands here and now.
    pub fn edit_map(&mut self, edit: MapEdit, now: Timestamp) {
        match edit {
            MapEdit::Note { room, text } => self.map.set_notes(room, &text),
            MapEdit::Feature {
                room,
                feature,
                present,
            } => self.map.set_feature(room, &feature, present),
            MapEdit::Mark { title, mark, set } => self.map.mark_mob(&title, mark, set, now),
            MapEdit::NotSame { first, second } => self.map.mark_not_same(first, second),
            MapEdit::Join { first, second } => {
                self.answer_join(now, |engine| engine.join_rooms(first, second, now));
            }
            MapEdit::UndoJoin { created } => {
                self.answer_join(now, |engine| engine.undo_join(created, now));
            }
        }
    }

    /// Runs a join or its undo, each of which rebuilds the map from what it
    /// wrote, and tells views how it went. A merge landing counts as a
    /// failure, as it does for a restore.
    fn answer_join(
        &mut self,
        now: Timestamp,
        run: impl FnOnce(&mut Engine) -> Result<JoinDone, String>,
    ) {
        self.settle(now);
        let outcome = if self.is_holding_writes() {
            Err("a sync is landing, so try again in a moment".to_owned())
        } else {
            run(self)
        };
        if let Err(error) = &outcome {
            self.notice(format!("the map didn't change: {error}"));
        }
        self.joins_answered += 1;
        self.hub.write::<MapState>(MapChange {
            revision: self.map.revision(),
            last_join: Some(LastJoin {
                attempt: self.joins_answered,
                outcome,
            }),
            ..MapChange::default()
        });
    }

    fn join_rooms(
        &mut self,
        first: RoomId,
        second: RoomId,
        now: Timestamp,
    ) -> Result<JoinDone, String> {
        if !self.map.joinable(first, second) {
            return Err("only two different rooms with no game number can join".to_owned());
        }
        let pair = room_pair(first, second);
        let (rows, reader) = self.saved_rows()?;
        let (batch, label) = merge::join_rooms(&rows, pair, &reader, now)?;
        self.write_map_edit(batch)?;
        Ok(JoinDone::Joined {
            label,
            created: now,
        })
    }

    fn undo_join(&mut self, created: Timestamp, now: Timestamp) -> Result<JoinDone, String> {
        let (rows, reader) = self.saved_rows()?;
        let gone = || "that join can't be undone any more".to_owned();
        let pair = undo_pair(&rows.meta, created).ok_or_else(gone)?;
        let versions = reader.map_versions().map_err(say)?;
        let version = versions
            .iter()
            .find(|version| version.kind == VERSION_JOIN && version.created == created)
            .ok_or_else(gone)?;
        let before = reader.map_version(version.id).map_err(say)?;
        let batch = merge::undo_join(&rows, &before, pair, (version.id, created), now)?;
        self.write_map_edit(batch)?;
        Ok(JoinDone::Undone)
    }

    /// What the database holds, once everything queued has reached it.
    fn saved_rows(&mut self) -> Result<(ServerRows, persist::Reader), String> {
        self.writer.flush().map_err(say)?;
        let reader = self.database.reader().map_err(say)?;
        let rows = ServerRows::load(&reader).map_err(say)?;
        Ok((rows, reader))
    }

    /// Writes rows worked out from the database, then rebuilds the map from it.
    fn write_map_edit(&mut self, batch: Batch) -> Result<(), String> {
        self.writer.commit(batch).map_err(say)?;
        self.reload_map().map_err(say)?;
        self.settings.map_versions += 1;
        self.publish_settings();
        Ok(())
    }

    /// Applies a change the player made to their gear, and saves it. A set
    /// belongs to who it was made for: if they've gone, so has the change.
    pub fn edit_gear(&mut self, edit: GearEdit, now: Timestamp) {
        let plays = |character: &str| !character.is_empty() && character == self.playing;
        match edit {
            GearEdit::SaveSet(mut set) if plays(&set.character_id) => {
                if set.id.is_empty() {
                    self.rules_made += 1;
                    set.id = rule_id(self.rules_made);
                }
                set.updated = now;
                self.gear.save_set(set, &mut self.batch);
            }
            GearEdit::RemoveSet { character, id } if plays(&character) => {
                self.gear.remove_set(&id, &mut self.batch);
            }
            GearEdit::SetKeywords { item, words } => {
                let mut saving = Saving {
                    now,
                    batch: &mut self.batch,
                };
                self.gear.set_keywords(&words, &item, &mut saving);
            }
            GearEdit::SaveSet(_) | GearEdit::RemoveSet { .. } => {}
        }
    }

    /// Puts on `character`'s set `id`, from what's known now. With echo off it
    /// would go as a password, so it does nothing, and it isn't the player's
    /// typing: aliases and history stay out of it.
    pub fn wear_set(&mut self, character: &str, id: &str, now: Timestamp) {
        if self.settings.echo_off || character.is_empty() || character != self.playing {
            return;
        }
        let Some(link) = &mut self.link else { return };
        let Some((name, plan)) = self.gear.wear(id, &mut link.gear, now) else {
            return;
        };
        for item in &plan.missing {
            self.notice(format!(
                "{name}: couldn't find {item}. It isn't worn, carried or in a bag."
            ));
        }
        for item in &plan.stand_ins {
            self.notice(format!(
                "{name}: the copy chosen of {item} isn't seen, so another stays on."
            ));
        }
        if plan.commands.is_empty() && plan.missing.is_empty() && plan.stand_ins.is_empty() {
            self.notice(format!("{name} is already on."));
        }
        for command in &plan.commands {
            self.send_gear_command(command, now);
        }
    }

    /// A gear command MUDling worked out. It's never a password, so with echo
    /// off it goes nowhere.
    fn send_gear_command(&mut self, command: &str, now: Timestamp) {
        if self.settings.echo_off {
            return;
        }
        if let Some(link) = &mut self.link {
            link.quiet.player_acted(command, now);
        }
        self.show_pending();
        self.show(Line::command(command));
        self.issue(command, now);
    }

    /// A command a shell's button sent. It's never a password, so with echo
    /// off it goes nowhere: a click made a frame before the server asked for
    /// one mustn't land in the password box.
    pub fn send_action(&mut self, command: &str, now: Timestamp) {
        if self.settings.echo_off {
            return;
        }
        if let Some(link) = &mut self.link {
            self.gear.button_sent(command, &mut link.gear, now);
        }
        self.send(command, now);
    }

    /// The player has text in the command field, or no longer has.
    pub fn set_typing(&mut self, typing: bool) {
        self.typing_text = typing;
    }

    /// Reads the character's tables again as soon as the game can spare it.
    pub fn refresh_character(&mut self) {
        if let Some(link) = &mut self.link {
            link.quiet.refresh();
        }
    }

    /// Time passed: sends the next quiet read, if it's due and the game can
    /// spare it. Nothing else sends one, so a replay never does. A quiet read
    /// is no command of the player's, so it skips what theirs go through:
    /// the chat, the map's reckoning, the latency clock and history.
    pub fn tick(&mut self, now: Timestamp) {
        let encoding = self.encoding();
        let Some(link) = &mut self.link else { return };
        let moment = Moment {
            typing: self.typing_text,
            echo_off: self.settings.echo_off,
            in_fight: link.game.in_fight(),
            position: link.game.position(),
        };
        if let Some(command) = link.quiet.next(moment, now) {
            write_line(&command, encoding, &mut self.output);
        }
    }

    /// Publishes what changed since the last settle: the stores, and the
    /// rows for the database. A move's map work finishes here.
    pub fn settle(&mut self, now: Timestamp) {
        self.take_hook_outputs(now);
        self.publish_sheet();
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
        notices.extend(self.logs.take_notices());
        for notice in notices {
            self.notice(notice);
        }
        if let Some(commit) = self.map.commit(now) {
            self.map_changed |=
                !commit.change.rooms.is_empty() || !commit.change.removed.is_empty();
            self.publish_map(commit.change);
            self.submit(commit.batch);
        }
        if std::mem::take(&mut self.contents_changed) {
            self.publish_room();
        }
        if std::mem::take(&mut self.character_renamed) {
            self.rules_follow_the_character();
        }
        self.publish_plugin_costs(now);
        self.share_link_rules();
        if !self.sounds.is_empty() {
            let sounds = std::mem::take(&mut self.sounds);
            self.hub.write::<CuesState>(sounds);
        }
        self.gear.publish(&self.plugins, &self.hub);
        if !self.terminal.is_empty() {
            let lines = std::mem::take(&mut self.terminal);
            self.hub
                .write::<TerminalState>(TerminalChange::Append(lines));
        }
        if !self.inspected.is_empty() {
            let lines = std::mem::take(&mut self.inspected);
            self.hub.write::<InspectorState>(InspectorChange {
                clear: false,
                lines,
            });
        }
        let batch = std::mem::take(&mut self.batch);
        self.submit(batch);
        self.autosave_map(now);
        self.tell_of_landed_versions();
    }

    fn publish_sheet(&mut self) {
        let quiet = self.link.as_ref().map(|link| &link.quiet);
        self.sheet
            .set_reading(quiet.is_some_and(QuietReads::is_busy));
        let change = (self.sheet).take_change(|reader| quiet?.command_of(reader));
        if let Some(sheet) = change {
            self.hub.write::<SheetState>(sheet);
        }
    }

    fn autosave_map(&mut self, now: Timestamp) {
        let due = now.0 - self.version_saved.0 >= AUTOSAVE_EVERY_MS;
        if self.link.is_some() && self.map_changed && due {
            self.save_map_version(VERSION_AUTOSAVE, AUTOSAVED, now);
        }
    }

    /// Queues a version of the map as the writer will have it: call this
    /// with every commit submitted. A shell hears once it's on the disk.
    fn save_map_version(&mut self, kind: &str, label: &str, now: Timestamp) {
        self.submit(LiveMap::save_version(kind, label, now));
        self.map_changed = false;
        self.version_saved = now;
        self.versions_landing = self.writer.caught_up().ok();
    }

    fn tell_of_landed_versions(&mut self) {
        let landed = self
            .versions_landing
            .as_ref()
            .is_some_and(|landing| landing.try_recv().is_ok());
        if landed {
            self.versions_landing = None;
            self.settings.map_versions += 1;
            self.publish_settings();
        }
    }

    /// Puts a saved version of the map back, after saving the map as it is
    /// now. On failure the map stays as it was, on the disk and in memory. A
    /// merge landing counts as a failure: it would rebuild the map under the
    /// restore, and the restore writes past the rows held for it.
    pub fn restore_map(&mut self, version_id: i64, now: Timestamp) {
        self.settle(now);
        let outcome = if self.is_holding_writes() {
            Err("a sync is landing, so try again in a moment".to_owned())
        } else {
            self.apply_map_version(version_id, now).map(|()| version_id)
        };
        if let Err(error) = &outcome {
            self.notice(format!("couldn't restore the map: {error}"));
        }
        self.restores += 1;
        self.settings.map_restore = Some(MapRestore {
            attempt: self.restores,
            outcome,
        });
        self.settings.map_versions += 1;
        self.publish_settings();
    }

    /// A version from before a join comes back with the join still made, so
    /// a restore never brings a retired room back as a duplicate: see
    /// `merge::join`.
    fn apply_map_version(&mut self, version_id: i64, now: Timestamp) -> Result<(), String> {
        let reader = self.database.reader().map_err(say)?;
        let meta = reader.meta().map_err(say)?;
        let version = reader.map_version(version_id).map_err(say)?;
        let mut batch = LiveMap::restore_batch(version_id, now);
        for key in merge::joins_cancelled_by(&version, &meta) {
            batch.delete(key);
        }
        self.writer.commit(batch).map_err(say)?;
        let restored_rows = ServerRows::load(&reader).map_err(say)?;
        let rejoin = merge::rejoin_restored(restored_rows, now);
        self.writer.commit(rejoin).map_err(say)?;
        let restored = self.map.prepare_restore(&reader.map().map_err(say)?);
        self.writer.commit(restored.place(now)).map_err(say)?;
        let change = self.map.finish_restore(restored);
        self.map_changed = false;
        self.version_saved = now;
        self.publish_map(change);
        self.publish_room();
        Ok(())
    }

    /// Waits for every write so far: for tests and benchmarks.
    pub fn flush(&self) {
        let _ = self.writer.caught_up().map(|done| done.recv());
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
                    self.show(Line::command(&send.command));
                    self.issue(&send.command, now);
                }
            }
            HookOutput::RefreshInventory => {
                if self.link.as_ref().is_some_and(|link| link.gmcp_enabled) {
                    gmcp::encode(inventory::ASK, "", &mut self.output);
                }
            }
            HookOutput::HookFailed(failed) => {
                let notice = format!(
                    "{}'s hook {} failed: {}",
                    failed.plugin,
                    failed.hook.as_str(),
                    failed.message
                );
                if let Some(link) = self.link.as_mut().filter(|_| failed.overran) {
                    link.overrun_notices.push((failed.plugin, notice.clone()));
                }
                self.notice(notice);
            }
            HookOutput::PluginDisabled(disabled) => self.plugin_turned_off(disabled),
        }
    }

    /// Lists the plugin as turned off, in place of the overruns that led there.
    fn plugin_turned_off(&mut self, disabled: PluginDisabled) {
        if let Some(link) = &mut self.link {
            let overruns = std::mem::take(&mut link.overrun_notices);
            let (folded, others): (Vec<_>, Vec<_>) = overruns
                .into_iter()
                .partition(|(plugin, _)| *plugin == disabled.plugin);
            link.overrun_notices = others;
            let notices = &mut self.settings.notices;
            notices.retain(|notice| !folded.iter().any(|(_, overrun)| overrun == notice));
        }
        let off = &mut self.settings.turned_off;
        off.retain(|off| off.plugin != disabled.plugin);
        off.push(disabled);
        self.publish_settings();
    }

    /// Brings back a plugin the core turned off. A plugin that's on, or
    /// isn't installed, stays as it is.
    pub fn turn_plugin_on(&mut self, plugin: &PluginId) {
        if let Err(error) = self.plugins.set_enabled(plugin, true) {
            self.notice(format!("{plugin} couldn't be turned on: {error}"));
            return;
        }
        if let Some(link) = &mut self.link {
            link.hooks.turn_back_on(plugin);
        }
        self.settings.turned_off.retain(|off| off.plugin != *plugin);
        self.publish_settings();
    }

    /// Approves `plugin` to send commands, or takes that back, for this
    /// connection and any after it in the session.
    pub fn set_approved(&mut self, plugin: PluginId, approved: bool) {
        if let Some(link) = &mut self.link {
            link.hooks.set_approved(&plugin, approved);
        }
        let list = &mut self.config.approved;
        list.retain(|other| *other != plugin);
        if approved {
            list.push(plugin);
        }
    }

    /// Opens or closes the inspector. Closing it forgets what it recorded.
    pub fn inspect(&mut self, open: bool) {
        self.inspecting = open;
        if !open {
            self.inspected.clear();
            self.hub.write::<InspectorState>(InspectorChange {
                clear: true,
                lines: Vec::new(),
            });
        }
    }

    /// The player saved new log settings.
    pub fn logging_changed(&mut self) {
        self.logs.follow_settings();
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
        if let Some(link) = &mut self.link {
            let mut saving = Saving {
                now,
                batch: &mut self.batch,
            };
            self.gear.finish_change(&mut link.gear, &mut saving);
        }
        self.logs.connection_ended(Some(&reason.to_string()));
        let was_connected = self.settings.connection == ConnectionStatus::Connected;
        self.link = None;
        self.map.connection_lost();
        self.settings.connection = ConnectionStatus::Closed(reason);
        self.settings.echo_off = false;
        self.settings.latency = None;
        self.publish_settings();
        self.publish_room();
        self.hub.write::<TargetState>(TargetState(None));
        let (had_position, had_affects) =
            self.hub.read(|characters: &Versioned<CharactersState>| {
                let characters = characters.value();
                (
                    characters.0.contains_key(CHARACTER_POSITION),
                    !characters.affects().is_empty(),
                )
            });
        if had_position {
            let position = HookValue::Text(String::new());
            let change = [(CHARACTER_POSITION.to_owned(), position)];
            self.hub
                .write::<CharactersState>(CharactersState(change.into()));
        }
        if had_affects {
            self.hub
                .write::<CharactersState>(CharactersState::no_affects());
        }
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
    /// and rebuilds the map, the player's rules (triggers, aliases, hotbar
    /// and notepad) and whoever plays (sheet, sets and worn gear) from the
    /// database, since a merge may have brought newer ones. Rows of the merge's window reach the disk first so the new map
    /// holds them. This waits on the disk, which only a merge during play
    /// costs.
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
            .and_then(|()| self.load_rules())
            .map(|()| self.reload_character())
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
        self.logs.finish();
        flushed?;
        saved.map_err(SessionError::History)
    }

    fn server_said(&mut self, event: Event, now: Timestamp) {
        match event {
            Event::Data(text) => {
                self.logs.server_text(&text);
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
                    if let Some(link) = &mut self.link {
                        let ended = Seen {
                            events: &self.events,
                            prompt: true,
                            glued: false,
                            blank: false,
                            pager: false,
                        };
                        link.quiet.read_line(ended, now);
                    }
                    self.route_events(now);
                }
            }
            Event::Gmcp { package, data } => {
                self.logs.gmcp(&package, &data);
                self.gmcp(&package, &data, now);
            }
            Event::GmcpEnabled => {
                if let Some(link) = &mut self.link {
                    link.gmcp_enabled = true;
                }
                self.subscribe_gmcp();
            }
            Event::Echo(echo) => {
                let line_open =
                    (self.link.as_ref()).is_some_and(|link| link.pipeline.pending().is_some());
                self.logs.echo_changed(!echo, line_open);
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
    /// open tables, and the plugins read what's glued after it. A quiet
    /// read's answer stops at the plugins: no trigger, map or chat sees it.
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
                link.gear.prompt_seen();
                self.plugins.end_tables(&line, &mut self.events);
            }
            let output_at = prompt.filter(|&at| at < text.len()).unwrap_or(0);
            let started = self.inspecting.then(Instant::now);
            let (coloring, claims) = if self.inspecting {
                let mut claiming = Claiming::new(&mut self.events);
                let coloring = self.plugins.read_line_from(&line, output_at, &mut claiming);
                (coloring, claiming.into_claims())
            } else {
                let coloring = self
                    .plugins
                    .read_line_from(&line, output_at, &mut self.events);
                (coloring, Vec::new())
            };
            let took = started.map(|started| started.elapsed());
            let seen = Seen {
                events: &self.events,
                prompt: prompt.is_some(),
                glued: output_at > 0,
                blank: text.trim().is_empty(),
                pager: link.quiet.is_hiding() && self.plugins.settings().is_pager(text),
            };
            let quiet = link.quiet.read_line(seen, now);
            let response = if quiet {
                TriggerResponse::default()
            } else {
                self.trigger_response()
            };
            let Some(link) = &mut self.link else { return };
            answer_login(&mut link.login, text, encoding, &mut self.output);
            let body = &text[prompt.unwrap_or(0)..];
            if !quiet && (prompt.is_none() || !body.is_empty()) {
                self.map.line(body, now);
            }
            self.route_events(now);
            let Some(link) = &mut self.link else { return };
            let hud_repeats_it = link.defaults.hide_prompts && link.defaults.hud.has_learned();
            let shown = if coloring.is_empty() {
                line.clone()
            } else {
                line.colored(&coloring)
            };
            let shown = match response.color {
                Some(color) => shown.highlighted(output_at, color.style()),
                None => shown,
            };
            // What a command sent before this line ended already showed of it
            // isn't shown again, hidden or not.
            let already_shown = std::mem::take(&mut link.pending_shown);
            self.sounds.extend(response.sounds);
            if quiet || response.hides {
                continue;
            }
            let prompt_len = prompt.filter(|_| hud_repeats_it).unwrap_or(0);
            let shown_before = self.terminal.len();
            if already_shown == 0 {
                self.show(match prompt_len {
                    0 => shown,
                    len => shown.with_prompt(len),
                });
            } else if already_shown < text.len() {
                // What a command cut off is out already; the rest of the prompt
                // is still the prompt, so it stays marked.
                let rest = shown.after(already_shown);
                self.show(match prompt_len.saturating_sub(already_shown) {
                    0 => rest,
                    len => rest.with_prompt(len),
                });
            }
            if let (Some(took), Some(line)) = (took, self.terminal.get(shown_before)) {
                self.inspected.push(InspectedLine {
                    line: line.clone(),
                    claims,
                    took,
                });
            }
        }
        // The buffer is kept: it's reused for every read.
        self.lines = lines;
    }

    /// What the triggers ask of the line just read. While the server asks
    /// for a password none of them acts: nothing in that exchange is game text.
    fn trigger_response(&self) -> TriggerResponse {
        match self.plugins.triggered() {
            [] => TriggerResponse::default(),
            _ if self.settings.echo_off => TriggerResponse::default(),
            matched => TriggerResponse::to(matched, &self.running_triggers),
        }
    }

    /// Sends what the plugins matched to the map and to the hooks.
    fn route_events(&mut self, now: Timestamp) {
        let Some(link) = &mut self.link else { return };
        let (mut gear_commands, mut gear_notices) = (Vec::new(), Vec::new());
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
            let mut saving = Saving {
                now,
                batch: &mut self.batch,
            };
            let (inventory, watch) = (&mut link.inventory, &mut link.gear);
            let put_away = (self.gear).event(
                &event,
                &self.plugins,
                inventory,
                watch,
                &self.hub,
                &mut saving,
            );
            gear_commands.extend(put_away);
            gear_notices.extend((self.gear).put_answered(&event, &self.plugins, watch));
            link.hooks.event(event);
        }
        for notice in gear_notices {
            self.notice(notice);
        }
        for command in gear_commands {
            self.send_gear_command(&command, now);
        }
    }

    /// Looks at the text the server hasn't ended. Call it after every read.
    fn read_pending(&mut self, now: Timestamp) {
        let encoding = self.encoding();
        let Some(link) = &mut self.link else { return };
        let Some(pending) = link.pipeline.pending() else {
            link.quiet.pager_waiting(false);
            return;
        };
        let kind = self.plugins.read_pending(&pending);
        link.quiet
            .pager_waiting(matches!(kind, Some(PendingKind::Pager { .. })));
        answer_login(&mut link.login, pending.plain, encoding, &mut self.output);
        if self.plugins.table_open() {
            link.defaults.pager.begin_table_read();
        } else {
            link.defaults.pager.end_table_read();
        }
        let action = link.defaults.read_pending(&pending, kind);
        match action {
            PendingAction::Wait => {}
            // A key sent now would land in the password box.
            PendingAction::Send(_) if self.settings.echo_off => {}
            PendingAction::Send(key) => {
                write_line(&key, encoding, &mut self.output);
                self.plugins.pager_answered();
                link.quiet.pager_waiting(false);
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
            self.gear.carried_changed(&link.inventory, &mut link.gear);
            if link.gmcp_enabled {
                for bag in link.inventory.bags_to_ask() {
                    gmcp::encode(inventory::ASK_BAG, &bag, &mut self.output);
                }
            }
        } else if link.inventory.read_bag(package, json) {
            self.gear.bags_changed(&link.inventory);
        }
        let plugins = &self.plugins;
        let fields = (self.game_fields).get(plugins.generation(), || GameFields::of(plugins));
        let heard = link.game.read(package, json, fields);
        // The first room means the character is in the game. Asking for the
        // modules again has a server send their values, which it may not
        // have done when the character came in.
        let entered_game = !link.inventory_asked && package.eq_ignore_ascii_case("Room.Info");
        if entered_game {
            link.inventory_asked = true;
            link.quiet.entered_game();
            gmcp::encode(inventory::ASK, "", &mut self.output);
            self.ask_for_modules();
        }
        self.contents_changed |= heard.contents;
        if let Some(HookValue::Text(name)) = heard.character.get(CHARACTER_NAME) {
            self.character_named(name.to_lowercase(), now);
        }
        if !heard.character.is_empty() {
            let lasting = heard
                .character
                .iter()
                .filter(|(key, _)| *key != CHARACTER_POSITION);
            for (key, value) in lasting {
                self.keep_character_value(key, value, now);
            }
            self.hub
                .write::<CharactersState>(CharactersState(heard.character.clone()));
        }
        if let Some(target) = heard.target {
            self.hub.write::<TargetState>(target);
        }
        let Some(link) = &self.link else { return };
        let learned = link.defaults.hud.has_learned();
        let hud = &link.defaults.hud;
        let plugins = &self.plugins;
        let fields = self
            .hud_fields
            .get((plugins.generation(), learned), || hud.fields(plugins));
        let mut vitals_changed = false;
        if fields
            .values()
            .any(|field| field.package.eq_ignore_ascii_case(package))
            && let Ok(message) = serde_json::from_str::<Map<String, Value>>(json)
        {
            let vitals = hud::values(fields, package, &message).into_iter().collect();
            self.hub.write::<VitalsState>(VitalsState(vitals));
            vitals_changed = true;
        }
        let affects_field = self
            .affects_field
            .get(plugins.generation(), || hud::affects_field(plugins));
        if let Some(field) = affects_field
            && field.package.eq_ignore_ascii_case(package)
            && let Ok(message) = serde_json::from_str::<Map<String, Value>>(json)
            && let Some(affects) = hud::affects(field, &message)
        {
            self.hub
                .write::<CharactersState>(CharactersState::listing(&affects));
        }
        self.map.gmcp(package, json, now);
        if let Some(link) = &mut self.link {
            let mut saving = Saving {
                now,
                batch: &mut self.batch,
            };
            let (gear, hub) = (&mut self.gear, &self.hub);
            gear.gmcp(
                vitals_changed,
                &heard.character,
                &mut link.gear,
                hub,
                &mut saving,
            );
        }
        if !heard.mobs.is_empty() {
            self.map.note_sightings(&heard.mobs, now);
        }
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
                Layer::Override => {
                    plugin.manifest.matches_server(server)
                        && !self.config.overrides_off.contains(plugin.id())
                }
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
        link.quiet.declare(&self.plugins);
        self.subscribe_gmcp();
        self.run_aliases();
        self.rules.separator = self.plugins.settings().separator.clone();
        self.publish_rules();
    }

    /// Loads what the player set up for this server, and the `all`-scoped
    /// rules the other saved servers keep.
    fn load_rules(&mut self) -> Result<(), SessionError> {
        let reader = self.database.reader()?;
        let host = self.config.host.clone();
        let mut triggers: Vec<TriggerRule> = reader
            .triggers()?
            .into_iter()
            .map(|row| TriggerRule::from_row(row, &host))
            .collect();
        let mut aliases: Vec<AliasRule> = reader
            .aliases()?
            .into_iter()
            .map(|row| AliasRule::from_row(row, &host))
            .collect();
        let meta = reader.meta()?;
        let hotbar = hotbar::from_meta(&meta);
        let notepad = meta.into_iter().find(|meta| meta.key == NOTEPAD);
        let others = persist::other_servers_rows(&self.config.data_folder, &host);
        for (home, row) in others.triggers {
            let rule = TriggerRule::from_row(row, &home);
            if rule.scope == Scope::All {
                triggers.push(rule);
            }
        }
        for (home, row) in others.aliases {
            let rule = AliasRule::from_row(row, &home);
            if rule.scope == Scope::All {
                aliases.push(rule);
            }
        }
        for problem in others.problems {
            self.notice(format!("couldn't read the rules of {problem}"));
        }
        self.rules = RulesState {
            server: host,
            character: self.playing_as(),
            separator: self.plugins.settings().separator.clone(),
            triggers,
            aliases,
            hotbar,
            notepad: notepad.map_or_else(String::new, |notepad| notepad.value),
        };
        self.run_triggers();
        self.run_aliases();
        self.publish_rules();
        Ok(())
    }

    /// Hands the matcher the triggers that apply to this character.
    fn run_triggers(&mut self) {
        let character = self.playing_as();
        let mut running = Vec::new();
        let mut patterns = Vec::new();
        for rule in &self.rules.triggers {
            if rule.enabled
                && rule.scope.covers(&character)
                && let Some(pattern) = rule.compiled()
            {
                running.push(rule.clone());
                patterns.push(pattern);
            }
        }
        match self.plugins.set_triggers(patterns) {
            Ok(()) => self.running_triggers = running,
            Err(error) => self.notice(format!("couldn't use the triggers: {error}")),
        }
    }

    /// Hands the command path the aliases that apply to this character: the
    /// player's, then the plugins', the highest layer first.
    fn run_aliases(&self) {
        let character = self.playing_as();
        let players = (self.rules.aliases.iter())
            .filter(|rule| rule.enabled && rule.problem().is_none())
            .filter(|rule| rule.scope.covers(&character))
            .map(|rule| (rule.name.as_str(), rule.expansion.as_str()));
        let layers: Vec<_> = self
            .plugins
            .declared(|points| points.commands.as_ref())
            .collect();
        let plugins = (layers.iter().rev())
            .flat_map(|(_, commands)| &commands.aliases)
            .map(|alias| (alias.name.as_str(), alias.expansion.as_str()));
        lock(&self.typing).aliases = AliasSet::new(players.chain(plugins));
    }

    fn publish_rules(&self) {
        self.hub.write::<RulesState>(self.rules.clone());
    }

    fn save_trigger(&mut self, mut rule: TriggerRule, now: Timestamp) {
        let at = (self.rules.triggers.iter()).position(|saved| saved.id == rule.id);
        self.find_home(&mut rule.id, &mut rule.home, &rule.scope, |id| {
            persist::Key::Trigger(id)
        });
        let mut batch = Batch::new();
        batch.upsert(rule.to_row(now));
        self.write_rows(&rule.home.clone(), batch);
        match at {
            Some(at) => self.rules.triggers[at] = rule,
            None => self.rules.triggers.push(rule),
        }
        self.run_triggers();
    }

    fn save_alias(&mut self, mut rule: AliasRule, now: Timestamp) {
        let at = (self.rules.aliases.iter()).position(|saved| saved.id == rule.id);
        self.find_home(&mut rule.id, &mut rule.home, &rule.scope, |id| {
            persist::Key::Alias(id)
        });
        let mut batch = Batch::new();
        batch.upsert(rule.to_row(now));
        self.write_rows(&rule.home.clone(), batch);
        match at {
            Some(at) => self.rules.aliases[at] = rule,
            None => self.rules.aliases.push(rule),
        }
        self.run_aliases();
    }

    /// Gives a new rule an id and this server for a home. A rule that stops
    /// being for every server leaves the other server's database for this one.
    fn find_home(
        &mut self,
        id: &mut String,
        home: &mut String,
        scope: &Scope,
        key: impl Fn(String) -> persist::Key,
    ) {
        if id.is_empty() {
            self.rules_made += 1;
            *id = rule_id(self.rules_made);
        }
        if home.is_empty() {
            home.clone_from(&self.config.host);
        } else if *home != self.config.host && *scope != Scope::All {
            self.remove_row(home, key(id.clone()));
            home.clone_from(&self.config.host);
        }
    }

    fn remove_row(&mut self, home: &str, key: persist::Key) {
        let mut batch = Batch::new();
        batch.delete(key);
        self.write_rows(home, batch);
    }

    /// Saves `batch` to the database of `home`: this server's writer for its
    /// own, and a short wait on the disk for a rule another server keeps.
    fn write_rows(&mut self, home: &str, batch: Batch) {
        if home == self.config.host {
            self.submit(batch);
        } else if let Err(error) = persist::write_to_server(&self.config.data_folder, home, batch) {
            self.notice(format!("couldn't save to {home}: {error}"));
        }
    }

    /// Loads the map, the mobs and the gear stores from the database, under
    /// the rules of the plugins on now. A character's rows wait for the
    /// connection to name them: see [`Engine::character_named`].
    fn load_saved(&mut self) -> Result<(), SessionError> {
        let reader = self.database.reader()?;
        let (rows, marks, meta) = (reader.map()?, reader.mob_marks()?, reader.meta()?);
        let rules = Box::new(PluginRules::of(&self.plugins));
        self.map = LiveMap::load(rules, &rows, &marks, &meta);

        let change = self.map.snapshot();
        self.add_map_words(&change.rooms);
        self.hub.write::<MapState>(change);
        self.publish_room();
        let mut mobs = MobsState {
            namer: self.map.mobs().namer().clone(),
            ..MobsState::default()
        };
        for mark in Mark::ALL {
            let names = self.map.mobs().marks(mark).iter();
            mobs.marks.extend(names.map(|name| (mark, name.clone())));
        }
        self.hub.write::<MobsState>(mobs);
        let owner = self.gear_owner();
        let mut saving = Saving {
            now: Timestamp::now(),
            batch: &mut self.batch,
        };
        let worn = reader.characters()?.worn;
        (self.gear).load(reader.gear()?, worn, &owner, &meta, &self.hub, &mut saving);
        Ok(())
    }

    /// [`Engine::issue`], with the player's aliases expanded. Only the
    /// player's own commands come this way: a hook's go as the hook wrote them.
    fn issue_expanded(&mut self, typed: &str, now: Timestamp) {
        let expanded = lock(&self.typing).aliases.expand_chain(typed);
        match expanded {
            Some(commands) => self.issue_commands(&commands, now),
            None => self.issue(typed, now),
        }
    }

    /// What the player or a hook sent, as the lines that go out.
    fn issue(&mut self, typed: &str, now: Timestamp) {
        if typed.trim().is_empty() {
            self.note_sent(now);
            write_line("", self.encoding(), &mut self.output);
            return;
        }
        self.issue_commands(&split_chain(typed), now);
    }

    /// Starts the clock on the round trip a command begins.
    fn note_sent(&mut self, now: Timestamp) {
        if let Some(link) = &mut self.link {
            link.latency.sent(now);
        }
    }

    /// Each step tells the map what to expect, one per move, however they're
    /// packed.
    fn issue_commands(&mut self, chain: &[impl AsRef<str>], now: Timestamp) {
        self.note_sent(now);
        let encoding = self.encoding();
        let mut commands = Vec::new();
        for command in chain.iter().map(AsRef::as_ref) {
            match self.walk.expand(command) {
                Some(expanded) => {
                    let steps = parse_speedwalk(command).expect("it expanded");
                    for step in steps {
                        for _ in 0..step.count {
                            self.map.sent(step.direction.name(), now);
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
        if let Some(link) = &mut self.link {
            for command in &mut commands {
                *command = link.variety.next(command);
            }
        }
        let separator = self.plugins.settings().separator.as_deref();
        for line in chain_lines(&commands, separator) {
            write_line(&line, encoding, &mut self.output);
        }
    }

    /// Text the server hasn't ended came before the command being sent, so
    /// the chat shows it now. Only the chat: the line stays pending, and
    /// what follows it joins it for the plugins, the map and the learner.
    fn show_pending(&mut self) {
        let Some(link) = &mut self.link else { return };
        let Some(pending) = link.pipeline.pending().filter(|_| !link.quiet.is_hiding()) else {
            return;
        };
        if pending.plain[link.pending_shown..].trim().is_empty() {
            return;
        }
        let line = pending.shown_from(link.pending_shown);
        link.pending_shown = pending.plain.len();
        self.show(line);
    }

    /// Queues a line for the terminal. Runs of blank lines show as one, and a
    /// prompt the HUD repeats, which a chat may hide, counts as no line at
    /// all, so the blanks around it join one run.
    fn show(&mut self, line: Line) {
        if line.is_only_prompt() {
            self.terminal.push(line);
            return;
        }
        let blank = line.kind() == LineKind::Server && line.plain().trim().is_empty();
        if !(blank && self.blank_shown) {
            self.logs.shown(&line);
            self.terminal.push(line);
        }
        self.blank_shown = blank;
    }

    fn plugin_store_write(&mut self, write: StoreWrite, now: Timestamp) {
        let StoreWrite {
            store, key, value, ..
        } = write;
        let owner = self.gear_owner();
        let watch = self.link.as_ref().map(|link| &link.gear);
        let mut saving = Saving {
            now,
            batch: &mut self.batch,
        };
        let filing = (self.gear).hook_wrote(&store, &key, &value, &owner, watch, &mut saving);
        if filing == Filing::Draft {
            return;
        }
        let key = format!("{store}.{key}");
        self.contents_changed |= key == CHARACTER_NAME;
        if key == CHARACTER_NAME
            && let HookValue::Text(name) = &value
        {
            self.character_named(name.to_lowercase(), now);
        }
        if values::is_gear_store(&store) {
            let row = values::to_row(&value);
            let change = BTreeMap::from([(key.clone(), value)]);
            self.hub.write::<GearState>(GearState(change));
            if filing == Filing::Plain {
                self.batch.upsert(persist::Meta {
                    key: format!("{}{key}", values::GEAR_META_PREFIX),
                    value: row,
                    updated: now,
                });
            }
        } else {
            self.keep_character_value(&key, &value, now);
            let change = BTreeMap::from([(key, value)]);
            self.hub.write::<CharactersState>(CharactersState(change));
        }
    }

    /// Saves a value about the character: in its typed table if it has one,
    /// or as a stat. Until the connection names them, it's nobody's.
    fn keep_character_value(&mut self, key: &str, value: &HookValue, now: Timestamp) {
        let split = key.split_once('.').unwrap_or((key, ""));
        let character = match self.playing.as_str() {
            "" => UNNAMED_CHARACTER,
            playing => playing,
        };
        let typed =
            !self.playing.is_empty() && (self.sheet).keep(split, value, now, &mut self.batch);
        if !typed {
            self.batch.upsert(persist::CharacterStat {
                character_id: character.to_owned(),
                key: key.to_owned(),
                value: values::to_row(value),
                updated: now,
            });
        }
    }

    /// This connection said who plays. Their saved rows take over the sheet,
    /// and nothing of whoever played before stays on it. Reads the disk,
    /// once a character.
    fn character_named(&mut self, id: String, now: Timestamp) {
        self.character_renamed |= self.playing != id;
        self.playing = id;
        if self.sheet.character() == self.playing {
            return;
        }
        if self.playing.is_empty() {
            self.sheet = Sheet::default();
            self.gear.sets_of("", &[]);
            self.publish_sheet();
            return;
        }
        self.batch.upsert(Character {
            id: self.playing.clone(),
            server_id: self.config.host.clone(),
            name: self.playing.clone(),
            updated: now,
        });
        self.load_character(true);
    }

    /// After a merge: whoever plays has what the merge brought, and nobody
    /// has anything another's rows could have left. They are still playing,
    /// so what the game last said of their level and affects stays.
    fn reload_character(&mut self) {
        if !self.playing.is_empty() {
            self.load_character(false);
        }
    }

    /// Takes what the database holds of whoever plays: their sheet, worn gear
    /// and sets, and the values hooks read back. Another character's rows,
    /// which the database holds too, don't come into it. `replacing` is whether
    /// this is a new character or game, whose level and affects aren't known
    /// yet, and not the same one reloaded, whose live ones stay.
    fn load_character(&mut self, replacing: bool) {
        let saved = (self.database.reader())
            .and_then(|reader| reader.characters())
            .unwrap_or_else(|error| {
                self.settings
                    .notices
                    .push(format!("couldn't load the character: {error}"));
                persist::CharacterRows::default()
            });
        self.sheet = Sheet::of(&self.playing, &saved);
        self.gear.wear_of(&self.playing, saved.worn.clone());
        self.gear.sets_of(&self.playing, &saved.gear_sets);
        // What hooks read back, and the HUD's level: whoever played before
        // has theirs replaced, or blanked.
        let mut values: BTreeMap<String, HookValue> = BTreeMap::new();
        if replacing {
            values.insert(CHARACTER_LEVEL.to_owned(), HookValue::Text(String::new()));
        }
        let mine = saved
            .stats
            .iter()
            .filter(|stat| stat.character_id == self.playing);
        values.extend(
            mine.filter_map(|stat| Some((stat.key.clone(), values::from_row(&stat.value)?))),
        );
        self.hub.write::<CharactersState>(CharactersState(values));
        if replacing {
            self.hub
                .write::<CharactersState>(CharactersState::no_affects());
        }
        self.publish_sheet();
    }

    /// Who gear is filed under: the character playing, or nobody yet.
    fn gear_owner(&self) -> String {
        match self.playing.as_str() {
            "" => UNNAMED_CHARACTER.to_owned(),
            playing => playing.to_owned(),
        }
    }

    /// Who is playing, by id: what a rule for one character names. Empty
    /// until this connection has been told. The name saved from an earlier
    /// one doesn't count: on the desktop every character shares a session id.
    fn playing_as(&self) -> String {
        self.playing.clone()
    }

    /// The character's name changed, so which rules apply to them did too,
    /// and whose gear it is.
    fn rules_follow_the_character(&mut self) {
        let character = self.playing_as();
        if self.rules.character != character {
            self.gear.follow(&character);
            self.rules.character = character;
            self.run_triggers();
            self.run_aliases();
            self.publish_rules();
        }
    }

    /// Who the character is, as far as anything has said.
    fn character_name(&self) -> Option<String> {
        self.hub.read(|characters: &Versioned<CharactersState>| {
            match characters.value().0.get(CHARACTER_NAME) {
                Some(HookValue::Text(name)) => Some(name.clone()),
                _ => None,
            }
        })
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
        let character = self.character_name();
        self.hub.write::<RoomState>(RoomState {
            room: self.map.shared_current_room(),
            whereabouts: self.map.whereabouts(),
            contents: self.link.as_ref().map_or_else(Default::default, |link| {
                link.game.contents(character.as_deref())
            }),
        });
    }

    fn publish_plugin_costs(&mut self, now: Timestamp) {
        let due = self
            .costs_published
            .is_none_or(|last| now.0 - last.0 >= COSTS_EVERY_MS);
        if self.link.is_some() && due {
            self.costs_published = Some(now);
            self.settings.plugin_costs = self.plugins.costs();
            self.settings.plugin_line_cost = self.plugins.line_cost();
            self.publish_settings();
        }
    }

    /// Hands a shell the rules [`Session::links`] runs, when they changed.
    ///
    /// [`Session::links`]: super::Session::links
    fn share_link_rules(&mut self) {
        let generation = self.plugins.generation();
        if self.link_rules_of != generation {
            self.link_rules_of = generation;
            lock(&self.typing).link_rules = self.plugins.link_rules().cloned().collect();
        }
    }

    /// Room and mob names, for Tab. Only names go in: never what's typed.
    /// A room changes with every sighting, and its names are already in.
    fn add_map_words(&mut self, rooms: &[Arc<MapRoom>]) {
        if rooms.is_empty() {
            return;
        }
        self.added_names.keep_to(self.plugins.generation());
        let mut typing = lock(&self.typing);
        for room in rooms {
            if self.added_names.rooms.insert(room.name.clone()) {
                typing.known.add_words(MAP_WORDS, words_of(&room.name));
            }
            for title in room.sightings.keys() {
                if self.added_names.mobs.insert(title.clone()) {
                    let name = self.map.mobs().name(title);
                    typing.known.add_words(MAP_WORDS, words_of(&name));
                }
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
        gmcp::encode_hello(
            &self.config.client_name,
            &self.config.client_version,
            &mut self.output,
        );
        self.ask_for_modules();
    }

    fn ask_for_modules(&mut self) {
        let mut modules: Vec<String> = CORE_GMCP.map(str::to_owned).into();
        for (_, connection) in self.plugins.declared(|points| points.connection.as_ref()) {
            modules.extend(connection.gmcp.iter().cloned());
        }
        modules.sort();
        modules.dedup();
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

/// An error as a view shows it.
fn say(error: impl fmt::Display) -> String {
    error.to_string()
}

/// An id for a new rule. Rows load in id order and the first rule to match
/// wins, so ids sort by when they were made; the random end keeps two devices'
/// apart.
fn rule_id(made: u32) -> String {
    let random = RandomState::new().build_hasher().finish() as u32;
    format!("{:012x}{made:04x}{random:08x}", Timestamp::now().0)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_names_added_start_over_with_new_plugins_and_when_full() {
        let mut added = AddedNames::default();
        added.keep_to(1);
        assert!(added.mobs.insert("a grey wolf".to_owned()));
        added.keep_to(1);
        assert!(!added.mobs.insert("a grey wolf".to_owned()), "remembered");

        added.keep_to(2);
        assert!(added.mobs.insert("a grey wolf".to_owned()), "new plugins");

        for number in 0..=NAMES_REMEMBERED {
            added.rooms.insert(format!("Room {number}"));
        }
        added.keep_to(2);
        assert!(added.rooms.is_empty(), "full");
        assert!(added.mobs.is_empty());
    }
}
