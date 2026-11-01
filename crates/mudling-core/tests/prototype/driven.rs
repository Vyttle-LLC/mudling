//! An engine with no game, or a made-up one, fed as a socket would.

use mudling_core::Line;
use mudling_core::persist::Timestamp;
use mudling_core::plugin::Plugin;
use mudling_core::session::{Credentials, Engine, SessionConfig, Typing};
use mudling_core::store::{StoreHub, TerminalState, Versioned};
use mudling_core::telnet::Bytes;
use mudling_core::telnet::command::{IAC, SB, SE};
use mudling_core::telnet::option::GMCP;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

const HOST: &str = "mud.example.org";

pub struct Driven {
    pub engine: Engine,
    pub hub: Arc<StoreHub>,
    _folder: TempDir,
}

impl Driven {
    pub fn new() -> Driven {
        Driven::with(Vec::new(), None)
    }

    pub fn with(plugins: Vec<Plugin>, credentials: Option<Credentials>) -> Driven {
        let folder = tempfile::tempdir().unwrap();
        let config = SessionConfig {
            enabled: plugins.iter().map(|plugin| plugin.id().clone()).collect(),
            plugins,
            credentials,
            ..SessionConfig::new(folder.path(), HOST, 4000)
        };
        let hub = Arc::new(StoreHub::new(|| {}));
        let typing = Arc::new(Mutex::new(Typing::default()));
        let mut engine = Engine::open(config, Arc::clone(&hub), typing).unwrap();
        engine.start_connection().unwrap();
        engine.take_output();
        Driven {
            engine,
            hub,
            _folder: folder,
        }
    }

    pub fn say(&mut self, bytes: &[u8]) {
        let now = Timestamp(0);
        self.engine
            .receive(Bytes::copy_from_slice(bytes), now)
            .unwrap();
        self.engine.settle(now);
    }

    pub fn gmcp(&mut self, message: &str) {
        let mut frame = vec![IAC, SB, GMCP];
        frame.extend_from_slice(message.as_bytes());
        frame.extend_from_slice(&[IAC, SE]);
        self.say(&frame);
    }

    pub fn send(&mut self, typed: &str) {
        self.engine.send(typed, Timestamp(0));
        self.engine.settle(Timestamp(0));
    }

    /// The lines that went to the server since the last call.
    pub fn sent(&mut self) -> Vec<String> {
        let output = self.engine.take_output();
        let text = String::from_utf8_lossy(&output);
        text.lines().map(str::to_owned).collect()
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
}
