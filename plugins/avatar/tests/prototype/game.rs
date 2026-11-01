//! Avatar through the engine: server text and GMCP in, as a socket would
//! deliver them, and the player's commands; the stores out.

use mudling_core::Line;
use mudling_core::map::{Derived, LiveMap, MapRoom, PlainRules, Whereabouts};
use mudling_core::net::DisconnectReason;
use mudling_core::persist::{Database, Timestamp};
use mudling_core::session::{Engine, SessionConfig, Typing};
use mudling_core::store::{MapState, StoreHub, TerminalState, Versioned};
use mudling_core::telnet::Bytes;
use mudling_core::telnet::command::{IAC, SB, SE, WILL};
use mudling_core::telnet::option::GMCP;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

const HOST: &str = "avatar.outland.org";
/// Between two things the game says in a session log.
const LOG_STEP_MS: i64 = 500;

/// `Package.Name {json}`, on the wire.
fn gmcp_frame(message: &str) -> Vec<u8> {
    let mut frame = vec![IAC, SB, GMCP];
    frame.extend_from_slice(message.as_bytes());
    frame.extend_from_slice(&[IAC, SE]);
    frame
}

pub struct Game {
    pub engine: Engine,
    pub hub: Arc<StoreHub>,
    folder: TempDir,
    now: Timestamp,
}

impl Game {
    pub fn new() -> Game {
        let folder = tempfile::tempdir().unwrap();
        let plugin = mudling_plugin_avatar::load();
        let config = SessionConfig {
            enabled: vec![plugin.id().clone()],
            plugins: vec![plugin],
            ..SessionConfig::new(folder.path(), HOST, 4000)
        };
        let hub = Arc::new(StoreHub::new(|| {}));
        let typing = Arc::new(Mutex::new(Typing::default()));
        let mut engine = Engine::open(config, Arc::clone(&hub), typing).unwrap();
        engine.start_connection().unwrap();
        let mut game = Game {
            engine,
            hub,
            folder,
            now: Timestamp(0),
        };
        game.say(&[IAC, WILL, GMCP]);
        game.engine.take_output();
        game
    }

    pub fn say(&mut self, bytes: &[u8]) {
        self.engine
            .receive(Bytes::copy_from_slice(bytes), self.now)
            .unwrap();
        self.engine.settle(self.now);
    }

    /// A line the server ended.
    pub fn line(&mut self, text: &str) {
        self.say(format!("{text}\r\n").as_bytes());
    }

    pub fn gmcp(&mut self, message: &str) {
        self.say(&gmcp_frame(message));
    }

    pub fn send(&mut self, typed: &str) {
        self.engine.send(typed, self.now);
        self.engine.settle(self.now);
    }

    /// What went to the server since the last call.
    pub fn sent(&mut self) -> String {
        String::from_utf8_lossy(&self.engine.take_output()).into_owned()
    }

    /// Plays a walk written the way the prototype's logs are: the server's
    /// text, `[GMCP]` lines, and `[MAP]` notes for the player's moves.
    pub fn play(&mut self, log: &str) {
        for line in log.lines() {
            self.now.0 += LOG_STEP_MS;
            if let Some(message) = line.strip_prefix("[GMCP] ") {
                self.gmcp(message);
            } else if let Some(direction) = line.strip_prefix("[MAP] move ") {
                self.send(direction);
            } else if !line.starts_with("[MAP] ") {
                self.line(line.trim_end_matches('\r'));
            }
        }
    }

    /// What a chat hiding the prompts the HUD repeats shows.
    pub fn terminal(&self) -> Vec<String> {
        self.hub.read(|terminal: &Versioned<TerminalState>| {
            let lines = terminal.value().lines().iter();
            lines
                .filter_map(Line::without_prompt)
                .map(|line| line.plain().to_owned())
                .collect()
        })
    }

    /// What a chat showing the prompts the HUD repeats shows.
    pub fn terminal_with_prompts(&self) -> Vec<String> {
        self.hub.read(|terminal: &Versioned<TerminalState>| {
            let lines = terminal.value().lines().iter();
            lines.map(|line| line.plain().to_owned()).collect()
        })
    }

    pub fn rooms(&self) -> Vec<Arc<MapRoom>> {
        self.hub
            .read(|map: &Versioned<MapState>| map.value().rooms().cloned().collect())
    }

    pub fn whereabouts(&self) -> Whereabouts {
        self.hub
            .read(|map: &Versioned<MapState>| map.value().whereabouts())
    }

    pub fn current(&self) -> Arc<MapRoom> {
        let id = self.whereabouts().room.expect("the player is on the map");
        self.hub
            .read(|map: &Versioned<MapState>| map.value().room(id).cloned())
            .expect("the room")
    }

    /// The first room called `name`.
    pub fn room(&self, name: &str) -> Arc<MapRoom> {
        let rooms = self.rooms();
        let room = rooms.iter().find(|room| room.name == name);
        room.unwrap_or_else(|| panic!("no room called {name}"))
            .clone()
    }

    /// Ends the connection, so the hooks are done, and keeps the stores.
    pub fn close(&mut self) {
        self.engine
            .close_connection(DisconnectReason::ClosedByServer, self.now);
    }

    /// The map's areas and buildings, from what reached the database.
    pub fn derived(self) -> Derived {
        self.engine.finish().unwrap();
        let folder = self.folder.path().join("servers");
        let reader = Database::open(&folder, HOST).unwrap().reader().unwrap();
        let (rows, marks, meta) = (
            reader.map().unwrap(),
            reader.mob_marks().unwrap(),
            reader.meta().unwrap(),
        );
        LiveMap::load(Box::new(PlainRules), &rows, &marks, &meta)
            .derived()
            .clone()
    }
}
