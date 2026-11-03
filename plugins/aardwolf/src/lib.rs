//! Runs the Aardwolf plugin the way the client will, through the public
//! plugin API only. Its text goes through the line pipeline and the plugin
//! host. Its GMCP, which carries most of what MUDling knows of the game,
//! goes through a session engine with the plugin turned on, as a connection
//! would bring it; nothing here opens a socket.

use mudling_core::line::LinePipeline;
use mudling_core::persist::Timestamp;
use mudling_core::plugin::{PendingKind, Plugin, PluginEvent, PluginHost};
use mudling_core::replay::{self, Event};
use mudling_core::session::{Engine, SessionConfig, Typing};
use mudling_core::store::{Store, StoreHub, Versioned};
use mudling_core::telnet::{Bytes, gmcp};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// The plugin folder: this crate's own.
pub fn plugin_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub fn load() -> Plugin {
    Plugin::load(plugin_dir()).unwrap_or_else(|error| panic!("{error}"))
}

/// `text` with Windows line ends made Unix ones, as a checkout on Windows
/// may hand a fixture over.
pub fn unix(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// The events a session log's server lines raise in the plugin host. A line
/// the plugin's prompt covers before its line end is ended there, as the
/// client ends it.
pub fn events(log: &str) -> Vec<PluginEvent> {
    let mut host = PluginHost::new([load()]).unwrap_or_else(|error| panic!("{error}"));
    let mut pipeline = LinePipeline::default();
    let mut events = Vec::new();
    for text in replay::server_lines(&unix(log)) {
        let mut lines = Vec::new();
        pipeline.push(text.as_bytes(), &mut lines);
        let pending = pipeline
            .pending()
            .and_then(|pending| host.read_pending(&pending));
        if pending == Some(PendingKind::Prompt) {
            lines.extend(pipeline.prompt_end());
        } else {
            pipeline.push(b"\n", &mut lines);
        }
        for line in &lines {
            host.read_line(line, &mut events);
        }
    }
    events
}

/// A session with the plugin on, fed a session log: its text as the server's
/// bytes, and its `[GMCP]` notes as GMCP messages.
pub struct Played {
    hub: Arc<StoreHub>,
    _engine: Engine,
    _folder: tempfile::TempDir,
}

impl Played {
    pub fn log(log: &str) -> Played {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let hub = Arc::new(StoreHub::new(|| {}));
        let plugin = load();
        let config = SessionConfig {
            enabled: vec![plugin.id().clone()],
            plugins: vec![plugin],
            ..SessionConfig::new(folder.path(), "aardwolf.test", 23)
        };
        let typing = Arc::new(Mutex::new(Typing::default()));
        let mut engine = Engine::open(config, Arc::clone(&hub), typing)
            .unwrap_or_else(|error| panic!("{error}"));
        engine.start_connection().expect("the plugin's hooks load");
        for event in replay::events(&unix(log)) {
            let bytes = match event {
                Event::Text(text) => text.into_bytes(),
                Event::Gmcp { package, json } => {
                    let mut bytes = Vec::new();
                    gmcp::encode(&package, &json, &mut bytes);
                    bytes
                }
                Event::Connected | Event::Command(_) => continue,
            };
            let now = Timestamp(0);
            engine.receive(Bytes::from(bytes), now).expect("no MCCP");
            engine.settle(now);
        }
        Played {
            hub,
            _engine: engine,
            _folder: folder,
        }
    }

    /// What the store `S` holds now.
    pub fn read<S: Store, R>(&self, read: impl FnOnce(&S) -> R) -> R {
        self.hub.read(|store: &Versioned<S>| read(store.value()))
    }
}
