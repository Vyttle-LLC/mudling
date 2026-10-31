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
mod inventory;
mod login;
mod map_rules;
mod values;

pub use engine::Engine;
pub(crate) use engine::SERVERS_FOLDER;
pub use login::Credentials;

use crate::completion::{Completer, Completion, History, KnownWords};
use crate::merge::RoomMoves;
use crate::net::{self, ConnectOptions, Connection, DEFAULT_CONNECT_TIMEOUT, DisconnectReason};
use crate::persist::{self, Timestamp};
use crate::plugin::luau::HookLimits;
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
            approved: Vec::new(),
            credentials: None,
            sync: None,
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

impl From<io::Error> for SessionError {
    fn from(error: io::Error) -> SessionError {
        SessionError::History(error)
    }
}

/// What the command box reads: memory only, under a lock held just long
/// enough for one key press or one room's words.
#[derive(Debug, Default)]
pub struct Typing {
    history: History,
    known: KnownWords,
    completer: Completer,
}

enum Input {
    Connect,
    Send(String),
    Cancel,
    Replay(Vec<Bytes>),
    HoldWrites(SyncSender<()>),
    ReleaseWrites(RoomMoves),
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
        })
    }

    /// The stores: subscribe, then drain once per frame.
    pub fn stores(&self) -> &StoreHub {
        &self.hub
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

    /// Closes the connection, or gives up on one still opening.
    pub fn cancel(&self) {
        self.queue(Input::Cancel);
    }

    /// Feeds a recorded byte stream through the session as one connection,
    /// each chunk as a read from the socket. What it would send goes nowhere.
    pub fn replay(&self, chunks: Vec<Bytes>) {
        self.queue(Input::Replay(chunks));
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
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let host = config.host.clone();
    let result = match Engine::open(config, Arc::clone(&hub), typing) {
        Ok(engine) => runtime.block_on(run(engine, inputs)),
        Err(error) => {
            hub.write::<SettingsState>(SettingsState {
                server: Some(host),
                connection: ConnectionStatus::Closed(DisconnectReason::Failed(error.to_string())),
                ..SettingsState::default()
            });
            Err(error)
        }
    };
    // A hostname lookup still running mustn't hold the session open.
    runtime.shutdown_background();
    result
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
                Some(Input::Cancel) => {
                    if let Some(socket) = &socket {
                        socket.connection.close();
                    }
                }
                Some(Input::Replay(chunks)) if socket.is_none() => replay(&mut engine, chunks),
                Some(Input::Replay(_)) => {}
                Some(Input::HoldWrites(done)) => engine.hold_writes(done),
                Some(Input::ReleaseWrites(moves)) => engine.release_writes(&moves),
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
    use crate::map::Whereabouts;
    use crate::persist::{Batch, Database, Key, Room};
    use crate::session::engine::SERVERS_FOLDER;
    use crate::store::{MapState, Versioned};
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

    #[test]
    fn a_session_quit_during_a_merge_waits_for_it_to_land() {
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
        assert!(began.elapsed() < RELEASE_TIMEOUT);
    }
}
