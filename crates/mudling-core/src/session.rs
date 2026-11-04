//! One running session with one server: everything the core does, joined.
//!
//! ```text
//! socket or recording ─ bytes ─▶ telnet · GMCP · MCCP2 ─▶ line pipeline
//!   ─▶ plugin host ─▶ defaults, Luau hooks, map, completion
//!   ─▶ stores (a shell subscribes) and the SQLite writer (a thread of its own)
//! ```
//!
//! [`Session`] is what a shell holds. It only queues work: everything runs on
//! one core thread, so nothing a shell calls waits on the disk or the
//! network. A shell reads the stores through [`Session::stores`], the usual
//! subscribe and batched-diff path, and sends with [`Session::send`].
//!
//! [`Engine`] is the session's work with no thread of its own. The core
//! thread feeds it what the socket read, and [`Session::replay`] feeds it a
//! recording, so both take the same path after the read. A replay tool or
//! benchmark can drive an `Engine` itself.
//!
//! A session is for one server, and outlives its connections. What a
//! connection learned (the prompt, the HUD fields, hook state) goes with it;
//! the map and what plugins wrote are saved in the data folder.
//!
//! Every extension point's matches reach the engine as [`PluginEvent`]s, read
//! in one place.
//!
//! [`PluginEvent`]: crate::plugin::PluginEvent

mod channels;
mod engine;
mod game;
mod gear;
mod inventory;
mod latency;
mod login;
mod map_rules;
mod quiet;
mod requests;
mod sheet;
mod values;

pub use engine::Engine;
pub use login::Credentials;

use crate::completion::{Completer, Completion, History, KnownWords};
use crate::defaults::aliases::{AliasRule, AliasSet};
use crate::defaults::triggers::TriggerRule;
use crate::line::{Line, Link};
use crate::map::{Mark, RoomId};
use crate::merge::RoomMoves;
use crate::net::{self, ConnectOptions, Connection, DEFAULT_CONNECT_TIMEOUT, DisconnectReason};
use crate::persist::{self, Database, GearSet, MapVersion, SERVERS_FOLDER, Timestamp};
use crate::plugin::luau::HookLimits;
use crate::plugin::points::LinkRule;
use crate::plugin::{Plugin, PluginError, PluginId};
use crate::store::{ConnectionStatus, SettingsState, StoreHub};
use crate::sync::Syncer;
use crate::telnet::Bytes;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender, WeakUnboundedSender};

/// How long a clean quit waits for the socket to close.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);
/// How long a quit waits for a merge to land.
const RELEASE_TIMEOUT: Duration = Duration::from_secs(2);
/// How often a connection looks for a quiet read to send.
const QUIET_READ_CHECK: Duration = Duration::from_secs(1);

/// What a session is for. Plain data, so it can cross into a session from
/// any shell.
#[derive(Clone, Debug)]
pub struct SessionConfig {
    /// Where the session keeps its files; the caller picks it.
    pub data_folder: PathBuf,
    pub host: String,
    pub port: u16,
    pub connect_timeout: Duration,
    /// The game plugins and overrides installed. Overrides for this server
    /// run. A game plugin runs only if it's in `enabled`; game detection
    /// puts a suggestion in the settings store and turns nothing on.
    pub plugins: Vec<Plugin>,
    /// The game plugins the player turned on for this server.
    pub enabled: Vec<PluginId>,
    /// The overrides the player turned off for this server.
    pub overrides_off: Vec<PluginId>,
    /// Plugins the player allowed to send commands and GMCP requests.
    pub approved: Vec<PluginId>,
    /// For auto-login, when a plugin names the login prompts.
    pub credentials: Option<Credentials>,
    /// Told when the connection opens and closes, and allowed to reach the
    /// session around a merge.
    pub sync: Option<Syncer>,
    pub client_name: String,
    pub client_version: String,
    pub hook_limits: HookLimits,
}

impl SessionConfig {
    pub fn new(data_folder: impl Into<PathBuf>, host: impl Into<String>, port: u16) -> Self {
        SessionConfig {
            data_folder: data_folder.into(),
            host: host.into(),
            port,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            plugins: Vec::new(),
            enabled: Vec::new(),
            overrides_off: Vec::new(),
            approved: Vec::new(),
            credentials: None,
            sync: None,
            client_name: "MUDling".to_owned(),
            client_version: crate::build_version().to_owned(),
            hook_limits: HookLimits::default(),
        }
    }
}

#[derive(Debug)]
pub enum SessionError {
    Persist(persist::Error),
    Plugin(PluginError),
    /// The command history couldn't be saved.
    History(io::Error),
    /// The core thread's async runtime couldn't start.
    Runtime(io::Error),
    /// The core thread panicked.
    Stopped,
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionError::Persist(error) => error.fmt(f),
            SessionError::Plugin(error) => error.fmt(f),
            SessionError::History(error) => {
                write!(f, "couldn't save the command history: {error}")
            }
            SessionError::Runtime(error) => write!(f, "couldn't start the session: {error}"),
            SessionError::Stopped => f.write_str("the session stopped unexpectedly"),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<persist::Error> for SessionError {
    fn from(error: persist::Error) -> SessionError {
        SessionError::Persist(error)
    }
}

impl From<PluginError> for SessionError {
    fn from(error: PluginError) -> SessionError {
        SessionError::Plugin(error)
    }
}

/// What a shell reads on its own thread: memory only, under a lock held just
/// long enough for one key press, one line's links or one room's words.
#[derive(Debug, Default)]
pub struct Typing {
    history: History,
    known: KnownWords,
    completer: Completer,
    link_rules: Vec<LinkRule>,
    aliases: AliasSet,
}

impl Typing {
    /// What `typed` would send, for the alias preview, if it holds an alias.
    pub fn alias_preview(&self, typed: &str) -> Option<String> {
        self.aliases.preview(typed)
    }
}

/// A change the player made to what they set up. A rule with no id is new.
#[derive(Clone, Debug, PartialEq)]
pub enum RuleEdit {
    SaveTrigger(TriggerRule),
    RemoveTrigger(String),
    SaveAlias(AliasRule),
    RemoveAlias(String),
    /// The hotbar's whole new list.
    SetHotbar(Vec<String>),
    /// The server's notepad, whole.
    SetNotepad(String),
}

/// A change the player made to their gear. None of it ever goes to the server.
#[derive(Clone, Debug, PartialEq)]
pub enum GearEdit {
    /// A set, for its `character_id`, who must still be playing. A set with no
    /// id is new.
    SaveSet(GearSet),
    RemoveSet {
        character: String,
        id: String,
    },
    /// The words the game knows an item by; none leaves commands to guess.
    SetKeywords {
        item: String,
        words: String,
    },
}

/// A change the player made to the map. None of it ever goes to the server.
#[derive(Clone, Debug, PartialEq)]
pub enum MapEdit {
    /// A room's note, whole; empty clears it.
    Note { room: RoomId, text: String },
    Feature {
        room: RoomId,
        feature: String,
        present: bool,
    },
    /// A mark on a mob, by the line the game shows for it.
    Mark {
        title: String,
        mark: Mark,
        set: bool,
    },
    /// Two rooms are one. `MapState::last_join` says how it went.
    Join { first: RoomId, second: RoomId },
    /// Two rooms that look alike aren't one: stop suggesting them.
    NotSame { first: RoomId, second: RoomId },
    /// Undoes the join whose version was saved at `created`, on the device
    /// that made it.
    /// `MapState::last_join` says how it went.
    UndoJoin { created: Timestamp },
    /// Removes a room and every link into it, so walking in maps it afresh.
    Forget { room: RoomId },
}

enum Input {
    Connect,
    Send(String),
    SendAction(String),
    Press(String),
    Typing(bool),
    RefreshCharacter,
    Edit(RuleEdit),
    EditMap(MapEdit),
    EditGear(GearEdit),
    WearSet { character: String, id: String },
    Cancel,
    Replay(Vec<Bytes>),
    HoldWrites(SyncSender<()>),
    ReleaseWrites(RoomMoves),
    TurnPluginOn(PluginId),
    Approve(PluginId, bool),
    Inspect(bool),
    LoggingChanged,
    RestoreMap(i64),
    Quit,
}

/// How the sync thread reaches a running session, around a merge. A merge can
/// retire a room the map holds in memory, and a row written for a retired room
/// comes back as a duplicate or fails its whole batch. So the session holds
/// its rows while the merge lands, moves them onto the rooms that stayed, and
/// reloads. Play carries on throughout; only rows wait.
#[derive(Clone)]
pub(crate) struct SessionLink {
    server: String,
    inbox: WeakUnboundedSender<Input>,
}

impl SessionLink {
    pub(crate) fn server(&self) -> &str {
        &self.server
    }

    pub(crate) fn is_alive(&self) -> bool {
        self.inbox.upgrade().is_some()
    }

    /// Asks the session to hold its rows, unless it has ended. `drained`
    /// answers once what it already queued has reached the database, and the
    /// session stays reachable until [`HeldSession::release`], even if it's
    /// quit by then.
    pub(crate) fn hold(&self, drained: SyncSender<()>) -> Option<HeldSession> {
        let inbox = self.inbox.upgrade()?;
        inbox.send(Input::HoldWrites(drained)).ok()?;
        Some(HeldSession {
            server: self.server.clone(),
            inbox,
        })
    }
}

/// A session holding its rows.
pub(crate) struct HeldSession {
    server: String,
    inbox: UnboundedSender<Input>,
}

impl HeldSession {
    pub(crate) fn server(&self) -> &str {
        &self.server
    }

    /// Lets held rows go, moved by `moves`, and rebuilds the map from the
    /// database.
    pub(crate) fn release(self, moves: RoomMoves) {
        let _ = self.inbox.send(Input::ReleaseWrites(moves));
    }
}

/// A running session. Every method returns at once.
pub struct Session {
    hub: Arc<StoreHub>,
    typing: Arc<Mutex<Typing>>,
    inbox: UnboundedSender<Input>,
    core: JoinHandle<Result<(), SessionError>>,
    map_history: MapHistory,
}

/// A session's map history, for a shell to hold. Listing goes straight to
/// the disk on a connection of its own, so it never waits behind the core
/// thread, and never holds it up.
#[derive(Clone)]
pub struct MapHistory {
    database: Database,
    inbox: UnboundedSender<Input>,
}

impl MapHistory {
    /// Saved versions, newest first. This blocks on the disk: call it off
    /// the UI thread.
    pub fn versions(&self) -> Result<Vec<MapVersion>, persist::Error> {
        self.database.reader()?.map_versions()
    }

    /// Puts a saved version back, and saves the map as it is now first.
    /// `SettingsState::map_restore` says how it went. Returns false if the
    /// session has stopped, so nothing will.
    pub fn restore(&self, version_id: i64) -> bool {
        self.inbox.send(Input::RestoreMap(version_id)).is_ok()
    }

    /// Undoes the join saved as the version from `created`.
    /// `MapState::last_join` says how it went. Returns false if the session
    /// has stopped, so nothing will.
    pub fn undo_join(&self, created: Timestamp) -> bool {
        let edit = MapEdit::UndoJoin { created };
        self.inbox.send(Input::EditMap(edit)).is_ok()
    }
}

/// A session on its way out; see [`Session::quit`].
pub struct Quitting(JoinHandle<Result<(), SessionError>>);

impl Quitting {
    /// Waits until every write has reached the disk. This blocks on it: call
    /// it on the way out of the app, off the UI thread.
    pub fn wait(self) -> Result<(), SessionError> {
        self.0.join().unwrap_or(Err(SessionError::Stopped))
    }
}

impl Session {
    /// Starts a session. The database opens on the core thread; if that
    /// fails, the settings store says why and the session does nothing.
    ///
    /// `waker` runs on a core thread when store changes are waiting, at most
    /// once per drained batch, so a shell schedules one frame for them.
    pub fn open(
        config: SessionConfig,
        waker: impl Fn() + Send + Sync + 'static,
    ) -> io::Result<Session> {
        let hub = Arc::new(StoreHub::new(waker));
        let typing = Arc::new(Mutex::new(Typing::default()));
        let (inbox, inputs) = mpsc::unbounded_channel();
        if let Some(sync) = &config.sync {
            sync.attach(SessionLink {
                server: config.host.clone(),
                inbox: inbox.downgrade(),
            });
        }
        let map_history = MapHistory {
            database: Database::at(&config.data_folder.join(SERVERS_FOLDER), &config.host),
            inbox: inbox.clone(),
        };
        let core = {
            let (hub, typing) = (Arc::clone(&hub), Arc::clone(&typing));
            thread::Builder::new()
                .name("mudling-session".to_owned())
                .spawn(move || run_core(config, hub, typing, inputs))?
        };
        Ok(Session {
            hub,
            typing,
            inbox,
            core,
            map_history,
        })
    }

    /// The stores: subscribe, then drain once per frame.
    pub fn stores(&self) -> &StoreHub {
        &self.hub
    }

    pub fn map_history(&self) -> MapHistory {
        self.map_history.clone()
    }

    /// Opens a connection to the server. The settings store shows how it
    /// goes.
    pub fn connect(&self) {
        self.queue(Input::Connect);
    }

    /// Sends what the player typed.
    pub fn send(&self, typed: impl Into<String>) {
        self.queue(Input::Send(typed.into()));
    }

    /// Sends a command from a button, not the command box: see
    /// [`Engine::send_action`].
    pub fn send_action(&self, command: impl Into<String>) {
        self.queue(Input::SendAction(command.into()));
    }

    /// Runs a command the player pressed, as on the hotbar. While the server
    /// asks for a password it does nothing: it would go as the password.
    pub fn press(&self, command: impl Into<String>) {
        self.queue(Input::Press(command.into()));
    }

    /// Says whether the command field holds text the player hasn't sent:
    /// quiet reads wait while it does.
    pub fn set_typing(&self, typing: bool) {
        self.queue(Input::Typing(typing));
    }

    /// Reads the character's tables again, quietly, once the game can spare
    /// it. [`crate::store::SheetState::reading`] shows it's under way.
    pub fn refresh_character(&self) {
        self.queue(Input::RefreshCharacter);
    }

    /// Saves a change the player made; the rules store shows it.
    pub fn edit(&self, edit: RuleEdit) {
        self.queue(Input::Edit(edit));
    }

    /// Saves a change the player made to the map; the map store shows it.
    pub fn edit_map(&self, edit: MapEdit) {
        self.queue(Input::EditMap(edit));
    }

    /// Applies a change to the player's gear and saves it.
    pub fn edit_gear(&self, edit: GearEdit) {
        self.queue(Input::EditGear(edit));
    }

    /// Puts on `character`'s gear set `id`. While the server asks for a
    /// password it does nothing, and nor does it if they aren't playing now.
    pub fn wear_set(&self, character: impl Into<String>, id: impl Into<String>) {
        self.queue(Input::WearSet {
            character: character.into(),
            id: id.into(),
        });
    }

    /// See [`Typing::alias_preview`].
    pub fn alias_preview(&self, typed: &str) -> Option<String> {
        self.typing().alias_preview(typed)
    }

    /// Closes the connection, or gives up on one still opening.
    pub fn cancel(&self) {
        self.queue(Input::Cancel);
    }

    /// Feeds a recorded byte stream through the session as one connection,
    /// each chunk as a read from the socket. What it would send goes nowhere.
    pub fn replay(&self, chunks: Vec<Bytes>) {
        self.queue(Input::Replay(chunks));
    }

    /// Turns a plugin back on after the core turned it off;
    /// `SettingsState::turned_off` lists those. It runs again from the next
    /// line, on a clean count of overruns.
    pub fn turn_plugin_on(&self, plugin: PluginId) {
        self.queue(Input::TurnPluginOn(plugin));
    }

    /// Lets a plugin's hooks send commands, or takes that back, from the next
    /// event on and for the rest of the session.
    pub fn set_approved(&self, plugin: PluginId, approved: bool) {
        self.queue(Input::Approve(plugin, approved));
    }

    /// Opens or closes the plugin inspector: lines fill `InspectorState`
    /// only while it's open, and closing it empties that.
    pub fn inspect(&self, open: bool) {
        self.queue(Input::Inspect(open));
    }

    /// The player saved new [`LogSettings`]: a running connection follows.
    ///
    /// [`LogSettings`]: crate::logs::LogSettings
    pub fn logging_changed(&self) {
        self.queue(Input::LoggingChanged);
    }

    /// The links the enabled plugins find in `line`, for a shell to offer.
    /// Ask for lines shown, not lines read.
    pub fn links(&self, line: &Line) -> Vec<Link> {
        let mut links = Vec::new();
        for rule in &self.typing().link_rules {
            rule.find(line.plain(), &mut links);
        }
        links
    }

    /// Up arrow in the command box: see [`History::older`].
    pub fn older(&self, line: &str, prefix: &str) -> Option<String> {
        self.typing().history.older(line, prefix)
    }

    /// Down arrow in the command box: see [`History::newer`].
    pub fn newer(&self, line: &str) -> Option<String> {
        self.typing().history.newer(line)
    }

    /// Tab in the command box: see [`Completer::complete`].
    pub fn complete(&self, line: &str, cursor: usize) -> Option<Completion> {
        let typing = &mut *self.typing();
        typing
            .completer
            .complete(line, cursor, &typing.history, &typing.known)
    }

    /// Whether the core thread ended: the database wouldn't open, or it
    /// panicked. Nothing more happens here; only a new session can connect.
    pub fn has_stopped(&self) -> bool {
        self.core.is_finished()
    }

    /// Closes the connection and flushes every writer. Returns at once;
    /// [`Quitting::wait`] waits for the disk.
    pub fn quit(self) -> Quitting {
        self.queue(Input::Quit);
        Quitting(self.core)
    }

    fn queue(&self, input: Input) {
        // A core thread that's gone has nothing left to do with it.
        let _ = self.inbox.send(input);
    }

    fn typing(&self) -> MutexGuard<'_, Typing> {
        self.typing.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn run_core(
    config: SessionConfig,
    hub: Arc<StoreHub>,
    typing: Arc<Mutex<Typing>>,
    inputs: UnboundedReceiver<Input>,
) -> Result<(), SessionError> {
    let host = config.host.clone();
    let started = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(SessionError::Runtime)
        .and_then(|runtime| Ok((runtime, Engine::open(config, Arc::clone(&hub), typing)?)));
    match started {
        Ok((runtime, engine)) => {
            let result = runtime.block_on(run(engine, inputs));
            // A hostname lookup still running mustn't hold the session open.
            runtime.shutdown_background();
            result
        }
        Err(error) => {
            hub.write::<SettingsState>(SettingsState {
                server: Some(host),
                connection: ConnectionStatus::Closed(DisconnectReason::Failed(error.to_string())),
                ..SettingsState::default()
            });
            Err(error)
        }
    }
}

/// An open socket, with what it reports.
struct Socket {
    connection: Connection,
    events: mpsc::Receiver<net::Event>,
}

/// The next thing a socket reports. With none, it waits forever.
async fn socket_event(socket: &mut Option<Socket>) -> Option<net::Event> {
    match socket {
        Some(socket) => socket.events.recv().await,
        None => std::future::pending().await,
    }
}

/// The core thread: waits for the next input, gives it to the engine, and
/// publishes what changed.
async fn run(mut engine: Engine, mut inputs: UnboundedReceiver<Input>) -> Result<(), SessionError> {
    let mut socket = None;
    let host = engine.host().to_owned();
    let port = engine.port();
    let timeout = engine.connect_timeout();
    let mut quiet_reads = tokio::time::interval(QUIET_READ_CHECK);
    quiet_reads.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            input = inputs.recv() => match input {
                Some(Input::Connect) if socket.is_none() => {
                    if let Err(error) = engine.start_connection() {
                        engine.fail_connection(error, Timestamp::now());
                        continue;
                    }
                    let options = ConnectOptions {
                        host: host.clone(),
                        port,
                        connect_timeout: timeout,
                    };
                    let (connection, events) =
                        net::connect(&tokio::runtime::Handle::current(), options);
                    socket = Some(Socket { connection, events });
                }
                Some(Input::Connect) => {}
                Some(Input::Send(typed)) => engine.send(&typed, Timestamp::now()),
                Some(Input::SendAction(command)) => {
                    engine.send_action(&command, Timestamp::now());
                }
                Some(Input::Press(command)) => engine.press(&command, Timestamp::now()),
                Some(Input::Typing(typing)) => engine.set_typing(typing),
                Some(Input::RefreshCharacter) => engine.refresh_character(),
                Some(Input::Edit(edit)) => engine.edit(edit, Timestamp::now()),
                Some(Input::EditMap(edit)) => engine.edit_map(edit, Timestamp::now()),
                Some(Input::EditGear(edit)) => engine.edit_gear(edit, Timestamp::now()),
                Some(Input::WearSet { character, id }) => {
                    engine.wear_set(&character, &id, Timestamp::now());
                }
                Some(Input::Cancel) => {
                    if let Some(socket) = &socket {
                        socket.connection.close();
                    }
                }
                Some(Input::Replay(chunks)) if socket.is_none() => replay(&mut engine, chunks),
                Some(Input::Replay(_)) => {}
                Some(Input::HoldWrites(done)) => engine.hold_writes(done),
                Some(Input::ReleaseWrites(moves)) => engine.release_writes(&moves),
                Some(Input::TurnPluginOn(plugin)) => engine.turn_plugin_on(&plugin),
                Some(Input::Approve(plugin, approved)) => engine.set_approved(plugin, approved),
                Some(Input::Inspect(open)) => engine.inspect(open),
                Some(Input::LoggingChanged) => engine.logging_changed(),
                Some(Input::RestoreMap(version_id)) => {
                    engine.restore_map(version_id, Timestamp::now());
                }
                Some(Input::Quit) | None => break,
            },
            Some(event) = socket_event(&mut socket) => {
                let now = Timestamp::now();
                match event {
                    net::Event::Connected => engine.connected(),
                    net::Event::Received(bytes) => {
                        if let Err(error) = engine.receive(bytes, now) {
                            engine.fail_connection(error, now);
                            socket = None;
                        }
                    }
                    net::Event::Quiet => engine.quiet(now),
                    net::Event::Closed(reason) => {
                        engine.close_connection(reason, now);
                        socket = None;
                    }
                }
            }
            output = engine.next_hook_output() => engine.hook_output(output, Timestamp::now()),
            _ = quiet_reads.tick(), if socket.is_some() => engine.tick(Timestamp::now()),
        }
        engine.settle(Timestamp::now());
        let output = engine.take_output();
        if let Some(socket) = socket.as_ref().filter(|_| !output.is_empty()) {
            socket.connection.write(output);
        }
    }
    if let Some(socket) = socket {
        close(socket, &mut engine).await;
    }
    wait_for_release(&mut engine, &mut inputs).await;
    engine.finish()
}

/// A session that ends during a merge lets it land first, so its last rows go
/// where the merge says. A sync that never answers doesn't hold up a quit.
async fn wait_for_release(engine: &mut Engine, inputs: &mut UnboundedReceiver<Input>) {
    while engine.is_holding_writes() {
        match tokio::time::timeout(RELEASE_TIMEOUT, inputs.recv()).await {
            Ok(Some(Input::ReleaseWrites(moves))) => engine.release_writes(&moves),
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return,
        }
    }
}

/// Closes a socket and waits for it, so the server sees a clean close.
async fn close(mut socket: Socket, engine: &mut Engine) {
    let output = engine.take_output();
    if !output.is_empty() {
        socket.connection.write(output);
    }
    socket.connection.close();
    let closed = async {
        while let Some(event) = socket.events.recv().await {
            if matches!(event, net::Event::Closed(_)) {
                return;
            }
        }
    };
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, closed).await;
}

/// A recording, as one connection: the same calls a socket's events make.
fn replay(engine: &mut Engine, chunks: Vec<Bytes>) {
    if let Err(error) = engine.start_connection() {
        engine.fail_connection(error, Timestamp::now());
        return;
    }
    engine.connected();
    for chunk in chunks {
        let now = Timestamp::now();
        if let Err(error) = engine.receive(chunk, now) {
            engine.fail_connection(error, now);
            return;
        }
        engine.settle(now);
        drop(engine.take_output());
    }
    engine.close_connection(DisconnectReason::ClosedByServer, Timestamp::now());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defaults::hotbar::HOTBAR_META;
    use crate::map::{LiveMap, Whereabouts};
    use crate::persist::SERVERS_FOLDER;
    use crate::persist::{
        Batch, Character, CharacterStat, Database, GearSet, Key, Meta, NOTEPAD, Room,
    };
    use crate::plugin::luau::{HookOutput, StoreWrite, Value};
    use crate::store::{
        Affect, CHARACTER_LEVEL, CharactersState, ItemsState, MapState, RulesState, SheetState,
        Versioned,
    };
    use std::sync::mpsc::sync_channel;
    use std::time::Instant;

    const HOST: &str = "mud.example.org";
    const IAC: u8 = 255;

    /// The game saying the player is in room `number`, as GMCP.
    fn in_room(number: i64) -> Bytes {
        let info = format!(
            r#"Room.Info {{"num": {number}, "name": "Room {number}", "zone": "Fields",
                "environment": "field", "exits": {{"n": {}}}, "details": []}}"#,
            number + 2
        );
        let mut bytes = vec![IAC, 251, 201, IAC, 250, 201];
        bytes.extend_from_slice(info.as_bytes());
        bytes.extend_from_slice(&[IAC, 240]);
        Bytes::from(bytes)
    }

    fn engine(folder: &std::path::Path, hub: &Arc<StoreHub>) -> Engine {
        let config = SessionConfig::new(folder, HOST, 4000);
        let mut engine = Engine::open(config, Arc::clone(hub), Arc::default()).unwrap();
        engine.start_connection().unwrap();
        engine.connected();
        engine
    }

    fn walk_to(engine: &mut Engine, number: i64) {
        let now = Timestamp::now();
        engine.send("n", now);
        engine.receive(in_room(number), now).unwrap();
        engine.settle(now);
    }

    fn room_ids(folder: &std::path::Path) -> Vec<i64> {
        let database = Database::open(&folder.join(SERVERS_FOLDER), HOST).unwrap();
        let rooms = database.reader().unwrap().map().unwrap().rooms;
        rooms.into_iter().map(|room| room.id).collect()
    }

    /// What a merge does to the database: room 2 goes, and room 3 takes its
    /// place.
    fn retire_room_2(folder: &std::path::Path) {
        let database = Database::open(&folder.join(SERVERS_FOLDER), HOST).unwrap();
        let writer = database.writer().unwrap();
        let mut batch = Batch::new();
        batch.upsert(Room {
            id: 3,
            name: "Room 2".to_owned(),
            area: "Fields".to_owned(),
            environment: "field".to_owned(),
            description: String::new(),
            x: 0,
            y: 2,
            z: 0,
            building: None,
            tangled: false,
            maze: false,
            updated: Timestamp::now(),
        });
        batch.delete(Key::Room(2));
        writer.submit(batch).unwrap();
        writer.flush().unwrap();
    }

    fn whereabouts(hub: &StoreHub) -> Whereabouts {
        hub.read(|map: &Versioned<MapState>| map.value().whereabouts())
    }

    /// A row for a child of the retired room would fail its whole batch if it
    /// reached the database unmoved, so the new room would be lost with it.
    #[test]
    fn rows_written_during_a_merge_survive_it_and_the_map_follows() {
        let folder = tempfile::tempdir().unwrap();
        let hub = Arc::new(StoreHub::new(|| {}));
        let mut engine = engine(folder.path(), &hub);
        walk_to(&mut engine, 1);
        walk_to(&mut engine, 2);

        let (drained, queue_empty) = sync_channel(1);
        engine.hold_writes(drained);
        queue_empty.recv().unwrap();
        assert_eq!(room_ids(folder.path()), [1, 2]);

        retire_room_2(folder.path());
        walk_to(&mut engine, 4);
        assert_eq!(room_ids(folder.path()), [1, 3], "rows wait for the merge");

        engine.release_writes(&RoomMoves::from([(2, Some(3))]));

        assert_eq!(room_ids(folder.path()), [1, 3, 4]);
        hub.read(|map: &Versioned<MapState>| {
            assert!(map.value().room(2).is_none());
            assert!(map.value().room(3).is_some());
        });
        assert_eq!(whereabouts(&hub).room, Some(4));
        let notices =
            hub.read(|settings: &Versioned<SettingsState>| settings.value().notices.clone());
        assert_eq!(notices, Vec::<String>::new());
        engine.finish().unwrap();
    }

    /// What a merge does to the rules: a newer notepad and hotbar arrive in
    /// the database.
    fn merge_brings_notes_and_hotbar(folder: &std::path::Path) {
        let database = Database::open(&folder.join(SERVERS_FOLDER), HOST).unwrap();
        let writer = database.writer().unwrap();
        let mut batch = Batch::new();
        for (key, value) in [
            (NOTEPAD, "from the other device"),
            (HOTBAR_META, r#"["look"]"#),
        ] {
            batch.upsert(Meta {
                key: key.to_owned(),
                value: value.to_owned(),
                updated: Timestamp::now(),
            });
        }
        writer.submit(batch).unwrap();
        writer.flush().unwrap();
    }

    #[test]
    fn the_notepad_and_hotbar_follow_a_merge() {
        let folder = tempfile::tempdir().unwrap();
        let hub = Arc::new(StoreHub::new(|| {}));
        let mut engine = engine(folder.path(), &hub);
        engine.edit(RuleEdit::SetNotepad("mine".to_owned()), Timestamp::now());
        engine.settle(Timestamp::now());
        let (drained, queue_empty) = sync_channel(1);
        engine.hold_writes(drained);
        queue_empty.recv().unwrap();

        merge_brings_notes_and_hotbar(folder.path());
        engine.release_writes(&RoomMoves::new());

        hub.read(|rules: &Versioned<RulesState>| {
            assert_eq!(rules.value().notepad, "from the other device");
            assert_eq!(rules.value().hotbar, ["look"]);
        });
        engine.finish().unwrap();
    }

    /// What another device brings: sets and a stat for each of two characters,
    /// the one playing and another who never played here.
    fn merge_brings_two_characters(folder: &std::path::Path) {
        let database = Database::open(&folder.join(SERVERS_FOLDER), HOST).unwrap();
        let writer = database.writer().unwrap();
        let mut batch = Batch::new();
        for (owner, strength) in [("alda", "14"), ("bryn", "9")] {
            batch.upsert(Character {
                id: owner.to_owned(),
                server_id: HOST.to_owned(),
                name: owner.to_owned(),
                updated: Timestamp::now(),
            });
            batch.upsert(CharacterStat {
                character_id: owner.to_owned(),
                key: "character.str".to_owned(),
                value: strength.to_owned(),
                updated: Timestamp::now(),
            });
            batch.upsert(GearSet {
                id: format!("{owner}-set"),
                character_id: owner.to_owned(),
                name: format!("{owner}'s set"),
                container: None,
                pieces: Vec::new(),
                updated: Timestamp::now(),
            });
        }
        writer.submit(batch).unwrap();
        writer.flush().unwrap();
    }

    #[test]
    fn whoever_plays_has_what_a_merge_brought_and_none_of_another_characters() {
        let folder = tempfile::tempdir().unwrap();
        let hub = Arc::new(StoreHub::new(|| {}));
        let mut engine = engine(folder.path(), &hub);
        let named = StoreWrite {
            plugin: PluginId::try_from("sample-game".to_owned()).unwrap(),
            store: "character".to_owned(),
            key: "name".to_owned(),
            value: Value::Text("Alda".to_owned()),
        };
        engine.hook_output(HookOutput::StoreWrite(named), Timestamp::now());
        engine.settle(Timestamp::now());
        let (drained, queue_empty) = sync_channel(1);
        engine.hold_writes(drained);
        queue_empty.recv().unwrap();

        merge_brings_two_characters(folder.path());
        engine.release_writes(&RoomMoves::new());
        engine.settle(Timestamp::now());

        hub.read(|sheet: &Versioned<SheetState>| {
            assert_eq!(sheet.value().character, "alda");
            assert_eq!(sheet.value().stats.get("str"), Some(&Value::Number(14.)));
        });
        hub.read(|items: &Versioned<ItemsState>| {
            let sets: Vec<_> = items
                .value()
                .sets
                .iter()
                .map(|set| set.id.as_str())
                .collect();
            assert_eq!(sets, ["alda-set"]);
        });
        engine.finish().unwrap();
    }

    #[test]
    fn a_merge_landing_leaves_the_affects_and_level_of_whoever_still_plays() {
        let folder = tempfile::tempdir().unwrap();
        let hub = Arc::new(StoreHub::new(|| {}));
        let mut engine = engine(folder.path(), &hub);
        let named = StoreWrite {
            plugin: PluginId::try_from("sample-game".to_owned()).unwrap(),
            store: "character".to_owned(),
            key: "name".to_owned(),
            value: Value::Text("Alda".to_owned()),
        };
        engine.hook_output(HookOutput::StoreWrite(named), Timestamp::now());
        engine.settle(Timestamp::now());
        let affect = Affect {
            name: "bless".to_owned(),
            detail: "12 hours".to_owned(),
        };
        hub.write::<CharactersState>(CharactersState::listing(std::slice::from_ref(&affect)));
        hub.write::<CharactersState>(CharactersState(
            [(CHARACTER_LEVEL.to_owned(), Value::Number(12.))].into(),
        ));
        let (drained, queue_empty) = sync_channel(1);
        engine.hold_writes(drained);
        queue_empty.recv().unwrap();

        merge_brings_two_characters(folder.path());
        engine.release_writes(&RoomMoves::new());

        hub.read(|characters: &Versioned<CharactersState>| {
            assert_eq!(characters.value().affects(), [affect]);
            assert_eq!(
                characters.value().0.get(CHARACTER_LEVEL),
                Some(&Value::Number(12.))
            );
        });
        engine.finish().unwrap();
    }

    #[test]
    fn a_restore_waits_out_a_merge_landing() {
        let folder = tempfile::tempdir().unwrap();
        let hub = Arc::new(StoreHub::new(|| {}));
        let mut engine = engine(folder.path(), &hub);
        walk_to(&mut engine, 1);
        // The version is taken from the disk, which the engine writes to on
        // its writer's thread.
        engine.flush();
        let database = Database::open(&folder.path().join(SERVERS_FOLDER), HOST).unwrap();
        let writer = database.writer().unwrap();
        writer
            .submit(LiveMap::save_version(
                "manual",
                "One room",
                Timestamp::now(),
            ))
            .unwrap();
        writer.flush().unwrap();
        walk_to(&mut engine, 2);
        let version = database.reader().unwrap().map_versions().unwrap()[0].id;
        let (drained, queue_empty) = sync_channel(1);
        engine.hold_writes(drained);
        queue_empty.recv().unwrap();

        engine.restore_map(version, Timestamp::now());

        let restore = hub.read(|settings: &Versioned<SettingsState>| {
            settings.value().map_restore.clone().unwrap()
        });
        assert!(restore.outcome.is_err(), "{restore:?}");
        assert_eq!(room_ids(folder.path()), [1, 2], "the map was left alone");
        engine.release_writes(&RoomMoves::new());
        engine.restore_map(version, Timestamp::now());
        assert_eq!(
            room_ids(folder.path()),
            [1],
            "restoring works once it's over"
        );
        engine.finish().unwrap();
    }

    #[test]
    fn a_session_quit_during_a_merge_waits_for_it_to_land() {
        quit_during_a_merge();
    }

    #[test]
    #[ignore = "timing: runs in the profile"]
    fn timing_a_session_quit_during_a_merge_waits_for_it_to_land() {
        assert!(quit_during_a_merge() < RELEASE_TIMEOUT);
    }

    /// Quits a session while a merge holds its writes, then lands the merge.
    /// Returns how long the quit took after that.
    fn quit_during_a_merge() -> Duration {
        let folder = tempfile::tempdir().unwrap();
        let session = Session::open(SessionConfig::new(folder.path(), HOST, 4000), || {}).unwrap();
        let link = SessionLink {
            server: HOST.to_owned(),
            inbox: session.inbox.downgrade(),
        };
        let (drained, queue_empty) = sync_channel(1);
        let held = link.hold(drained).unwrap();
        queue_empty.recv().unwrap();

        let quitting = session.quit();
        let began = Instant::now();
        held.release(RoomMoves::new());

        quitting.wait().unwrap();
        began.elapsed()
    }
}
