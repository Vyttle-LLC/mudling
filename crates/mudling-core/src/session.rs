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

mod engine;
mod game;
mod gear;
mod inventory;
mod latency;
mod login;
mod map_rules;
mod values;

pub use engine::Engine;
pub use login::Credentials;

use crate::completion::{Completer, Completion, History, KnownWords};
use crate::defaults::aliases::{AliasRule, AliasSet};
use crate::defaults::triggers::TriggerRule;
use crate::line::{Line, Link};
use crate::map::{Mark, RoomId};
use crate::net::{self, ConnectOptions, Connection, DEFAULT_CONNECT_TIMEOUT, DisconnectReason};
use crate::persist::{self, Database, MapVersion, SERVERS_FOLDER, Timestamp};
use crate::plugin::luau::HookLimits;
use crate::plugin::points::LinkRule;
use crate::plugin::{Plugin, PluginError, PluginId};
use crate::store::{ConnectionStatus, SettingsState, StoreHub};
use crate::telnet::Bytes;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

/// How long a clean quit waits for the socket to close.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

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
    /// Plugins the player allowed to send commands.
    pub approved: Vec<PluginId>,
    /// For auto-login, when a plugin names the login prompts.
    pub credentials: Option<Credentials>,
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
            approved: Vec::new(),
            credentials: None,
            client_name: "MUDling".to_owned(),
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
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
}

enum Input {
    Connect,
    Send(String),
    SendAction(String),
    Press(String),
    Edit(RuleEdit),
    EditMap(MapEdit),
    Cancel,
    Replay(Vec<Bytes>),
    TurnPluginOn(PluginId),
    RestoreMap(i64),
    Quit,
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

    /// Saves a change the player made; the rules store shows it.
    pub fn edit(&self, edit: RuleEdit) {
        self.queue(Input::Edit(edit));
    }

    /// Saves a change the player made to the map; the map store shows it.
    pub fn edit_map(&self, edit: MapEdit) {
        self.queue(Input::EditMap(edit));
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
                Some(Input::Edit(edit)) => engine.edit(edit, Timestamp::now()),
                Some(Input::EditMap(edit)) => engine.edit_map(edit, Timestamp::now()),
                Some(Input::Cancel) => {
                    if let Some(socket) = &socket {
                        socket.connection.close();
                    }
                }
                Some(Input::Replay(chunks)) if socket.is_none() => replay(&mut engine, chunks),
                Some(Input::Replay(_)) => {}
                Some(Input::TurnPluginOn(plugin)) => engine.turn_plugin_on(&plugin),
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
    engine.finish()
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
