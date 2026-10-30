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
//! Each extension point's matches reach the engine as [`PluginEvent`]s, read
//! in one place, so when the host runs more points than Text their events
//! arrive the same way.
//!
//! [`PluginEvent`]: crate::plugin::PluginEvent

mod engine;
mod inventory;
mod login;
mod map_rules;
mod values;

pub use engine::Engine;
pub use login::Credentials;

use crate::completion::{Completer, Completion, History, KnownWords};
use crate::net::{self, ConnectOptions, Connection, DEFAULT_CONNECT_TIMEOUT, DisconnectReason};
use crate::persist::{self, Timestamp};
use crate::plugin::luau::HookLimits;
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
    Quit,
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
                        let reason = DisconnectReason::Failed(error.to_string());
                        engine.close_connection(reason, Timestamp::now());
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
                Some(Input::Quit) | None => break,
            },
            Some(event) = socket_event(&mut socket) => {
                let now = Timestamp::now();
                match event {
                    net::Event::Connected => engine.connected(),
                    net::Event::Received(bytes) => {
                        if let Err(error) = engine.receive(bytes, now) {
                            let reason = DisconnectReason::Failed(error.to_string());
                            engine.close_connection(reason, now);
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
        let reason = DisconnectReason::Failed(error.to_string());
        engine.close_connection(reason, Timestamp::now());
        return;
    }
    engine.connected();
    for chunk in chunks {
        let now = Timestamp::now();
        if let Err(error) = engine.receive(chunk, now) {
            engine.close_connection(DisconnectReason::Failed(error.to_string()), now);
            return;
        }
        engine.settle(now);
        drop(engine.take_output());
    }
    engine.close_connection(DisconnectReason::ClosedByServer, Timestamp::now());
}
