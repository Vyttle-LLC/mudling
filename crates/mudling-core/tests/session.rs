//! The session end to end: a recorded byte stream, and a fake server on a
//! real socket, through to the stores and the database.

use mudling_core::defaults::detect::Source;
use mudling_core::defaults::hud::Vital;
use mudling_core::map::{Mark, feature};
use mudling_core::net::DisconnectReason;
use mudling_core::persist::{Database, Timestamp, saved_servers};
use mudling_core::plugin::luau::{DisabledReason, Value};
use mudling_core::plugin::{Plugin, PluginId};
use mudling_core::session::{Credentials, Engine, Session, SessionConfig, Typing};
use mudling_core::store::{
    CHARACTER_LEVEL, CHARACTER_NAME, CHARACTER_POSITION, CharactersState, ConnectionStatus,
    GearState, InspectorState, Item, MapState, MobsState, Occupant, RoomState, SettingsState,
    Store, StoreHub, Target, TargetState, TerminalState, Versioned, VitalsState,
};
use mudling_core::telnet::command::{DO, GA, IAC, SB, SE, WILL};
use mudling_core::telnet::option::{ECHO, GMCP};
use mudling_core::telnet::{Bytes, gmcp};
use mudling_core::{Line, LineKind};
use std::fmt::Debug;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(5);
const HOST: &str = "127.0.0.1";

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

fn hearth() -> Plugin {
    Plugin::load(fixtures().join("plugins").join("hearth")).unwrap_or_else(|e| panic!("{e}"))
}

fn hearth_id() -> PluginId {
    hearth().id().clone()
}

fn recording() -> Vec<u8> {
    std::fs::read(fixtures().join("session").join("hearth.telnet")).expect("the fixture")
}

fn config(folder: &Path, port: u16) -> SessionConfig {
    SessionConfig {
        plugins: vec![hearth()],
        enabled: vec![hearth_id()],
        approved: vec![hearth_id()],
        ..SessionConfig::new(folder, HOST, port)
    }
}

fn open(config: SessionConfig) -> Session {
    Session::open(config, || {}).expect("the session starts")
}

/// What a chat hiding the prompts the HUD repeats shows.
fn terminal(session: &Session) -> Vec<String> {
    session
        .stores()
        .read(|terminal: &Versioned<TerminalState>| {
            terminal
                .value()
                .lines()
                .iter()
                .filter_map(Line::without_prompt)
                .map(|line| line.plain().to_owned())
                .collect()
        })
}

/// The server's lines as [`terminal`] shows them, without commands: hooks run
/// off the line thread, so where a hook's command lands among them isn't fixed.
fn server_lines(session: &Session) -> Vec<String> {
    session
        .stores()
        .read(|terminal: &Versioned<TerminalState>| {
            terminal
                .value()
                .lines()
                .iter()
                .filter(|line| line.kind() == LineKind::Server)
                .filter_map(Line::without_prompt)
                .map(|line| line.plain().to_owned())
                .collect()
        })
}

fn settings(session: &Session) -> SettingsState {
    session
        .stores()
        .read(|settings: &Versioned<SettingsState>| settings.value().clone())
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(started.elapsed() < WAIT, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn wait_for_terminal(session: &Session, text: &str) {
    wait_for(text, || terminal(session).iter().any(|line| line == text));
}

fn wait_for_status(session: &Session, status: ConnectionStatus) {
    wait_for(&format!("{status:?}"), || {
        settings(session).connection == status
    });
}

fn closed_by_server() -> ConnectionStatus {
    ConnectionStatus::Closed(DisconnectReason::ClosedByServer)
}

fn replay(session: &Session, chunk_size: usize) {
    let bytes = recording();
    let chunks = bytes.chunks(chunk_size).map(Bytes::copy_from_slice);
    session.replay(chunks.collect());
    wait_for_status(session, closed_by_server());
}

/// Every byte the database files hold, for proving something isn't there.
fn database_bytes(folder: &Path) -> Vec<u8> {
    let mut bytes = Vec::new();
    for entry in std::fs::read_dir(folder.join("servers")).unwrap().flatten() {
        bytes.extend(std::fs::read(entry.path()).unwrap());
    }
    bytes
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|at| at == needle)
}

/// `Package.Name {json}`, on the wire.
fn gmcp_frame(message: &str) -> Vec<u8> {
    let (package, data) = message.split_once(' ').unwrap_or((message, ""));
    let mut bytes = Vec::new();
    gmcp::encode(package, data, &mut bytes);
    bytes
}

/// Every store, as text, for proving something isn't in any of them.
fn every_store(session: &Session) -> String {
    fn text<S: Store + Debug>(session: &Session) -> String {
        session
            .stores()
            .read(|store: &Versioned<S>| format!("{:?}", store.value()))
    }
    [
        text::<TerminalState>(session),
        text::<VitalsState>(session),
        text::<RoomState>(session),
        text::<MapState>(session),
        text::<MobsState>(session),
        text::<CharactersState>(session),
        text::<TargetState>(session),
        text::<GearState>(session),
        text::<SettingsState>(session),
    ]
    .concat()
}

// ---- A recorded byte stream ----

#[test]
fn a_recording_reaches_the_stores_and_the_database() {
    let folder = tempfile::tempdir().unwrap();
    let session = open(config(folder.path(), 4000));
    replay(&session, 256);

    let stores = session.stores();
    assert_eq!(
        server_lines(&session),
        [
            "Welcome to Hearth.",
            "A quiet road",
            "Mira says 'welcome'",
            "Mira waves at you.",
            "You are now 45% in dodge.",
            "You wear a pink ring.",
            "A gate",
            "A grey wolf lunges at you!",
        ],
        "prompts are hidden once the HUD has numbers"
    );
    stores.read(|terminal: &Versioned<TerminalState>| {
        let lines = terminal.value().lines();
        let find = |text: &str| lines.iter().find(|line| line.plain().starts_with(text));
        let colored = find("Mira says").unwrap();
        assert_eq!(colored.styled().len(), 1, "the plugin's coloring");
        let echoed = find("wave Mira").unwrap();
        assert_eq!(echoed.kind(), LineKind::Command, "a hook's command shows");
    });
    stores.read(|vitals: &Versioned<VitalsState>| {
        let values = &vitals.value().0;
        assert_eq!(values.get(&Vital::Hp), Some(&11));
        assert_eq!(values.get(&Vital::MaxHp), Some(&30));
    });
    stores.read(|room: &Versioned<RoomState>| {
        assert_eq!(room.value().room.as_ref().unwrap().name, "A gate");
    });
    stores.read(|map: &Versioned<MapState>| {
        assert_eq!(map.value().rooms().len(), 2);
        assert_eq!(map.value().whereabouts().room, Some(2));
    });
    stores.read(|mobs: &Versioned<MobsState>| {
        assert!(
            mobs.value()
                .marks
                .contains(&(Mark::Aggressive, "wolf".to_owned()))
        );
    });
    let character = stores.read(|c: &Versioned<CharactersState>| c.value().0.clone());
    assert_eq!(character.get("skills.dodge"), Some(&Value::Number(45.0)));
    let gear = stores.read(|g: &Versioned<GearState>| g.value().0.clone());
    assert_eq!(gear.get("gear.worn.a pink ring"), Some(&Value::Bool(true)));
    let game = settings(&session)
        .suggestion
        .expect("the MSSP name named the game");
    assert_eq!((game.plugin, game.source), (hearth_id(), Source::Mssp));

    session.quit().wait().unwrap();
    let database = Database::open(&folder.path().join("servers"), HOST).unwrap();
    let reader = database.reader().unwrap();
    assert_eq!(reader.map().unwrap().rooms.len(), 2);
    let stats = reader.characters().unwrap().stats;
    assert!(stats.iter().any(|stat| stat.key == "skills.dodge"));
    assert!(reader.mob_marks().unwrap().iter().any(|m| m.name == "wolf"));
    let meta = reader.meta().unwrap();
    assert!(
        meta.iter()
            .any(|m| m.key.ends_with("gear.worn.a pink ring"))
    );
}

#[test]
fn chunking_changes_nothing() {
    let run = |chunk_size| {
        let folder = tempfile::tempdir().unwrap();
        let session = open(config(folder.path(), 4000));
        replay(&session, chunk_size);
        let lines = server_lines(&session);
        session.quit().wait().unwrap();
        lines
    };
    assert_eq!(run(1), run(100_000));
}

#[test]
fn what_was_saved_comes_back_in_the_next_session() {
    let folder = tempfile::tempdir().unwrap();
    let session = open(config(folder.path(), 4000));
    replay(&session, 4096);
    session.quit().wait().unwrap();

    let session = open(config(folder.path(), 4000));
    wait_for("the map to load", || {
        session
            .stores()
            .read(|map: &Versioned<MapState>| map.value().rooms().len() == 2)
    });
    let stores = session.stores();
    // The map is published before the gear is, so the map arriving isn't it.
    wait_for("the gear to load", || {
        stores.read(|gear: &Versioned<GearState>| !gear.value().0.is_empty())
    });
    let character = stores.read(|c: &Versioned<CharactersState>| c.value().0.clone());
    assert_eq!(
        character.get("skills.dodge"),
        None,
        "a character's values wait for this connection to name them"
    );
    let gear = stores.read(|g: &Versioned<GearState>| g.value().0.clone());
    assert_eq!(gear.get("gear.worn.a pink ring"), Some(&Value::Bool(true)));
    stores.read(|room: &Versioned<RoomState>| {
        assert_eq!(room.value().whereabouts.room, Some(2));
    });
    session.quit().wait().unwrap();
}

// ---- A real socket ----

/// A scripted server on a local socket.
struct FakeServer {
    stream: TcpStream,
    received: Vec<u8>,
}

impl FakeServer {
    fn listen() -> (TcpListener, u16) {
        let listener = TcpListener::bind((HOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        (listener, port)
    }

    fn accept(listener: &TcpListener) -> FakeServer {
        let (stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        FakeServer {
            stream,
            received: Vec::new(),
        }
    }

    fn say(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).unwrap();
    }

    fn gmcp(&mut self, message: &str) {
        self.say(&gmcp_frame(message));
    }

    /// Reads until `needle` has arrived, and everything before it is gone.
    fn expect(&mut self, needle: &[u8]) {
        let mut buffer = [0; 4096];
        while !contains(&self.received, needle) {
            let read = self.stream.read(&mut buffer).unwrap_or_else(|error| {
                panic!(
                    "waiting for {:?}, got {:?}: {error}",
                    String::from_utf8_lossy(needle),
                    String::from_utf8_lossy(&self.received)
                )
            });
            assert!(read > 0, "the client closed before sending {needle:?}");
            self.received.extend_from_slice(&buffer[..read]);
        }
        let at = self
            .received
            .windows(needle.len())
            .position(|window| window == needle)
            .unwrap();
        self.received.drain(..at + needle.len());
    }
}

fn logged_in_session(folder: &Path, port: u16) -> Session {
    let session = open(SessionConfig {
        credentials: Some(Credentials {
            name: "Tamsin".to_owned(),
            password: "hunter2;hunter3".to_owned(),
        }),
        ..config(folder, port)
    });
    session.connect();
    session
}

#[test]
fn a_whole_session_on_a_real_connection() {
    let folder = tempfile::tempdir().unwrap();
    let (listener, port) = FakeServer::listen();
    let session = logged_in_session(folder.path(), port);
    let mut server = FakeServer::accept(&listener);
    wait_for_status(&session, ConnectionStatus::Connected);

    // GMCP: the hello and the subscriptions, the game plugin's included.
    server.say(&[IAC, WILL, GMCP]);
    server.expect(&[IAC, DO, GMCP]);
    server.expect(b"Core.Hello");
    server.expect(b"Char 1");
    server.expect(b"Char.Skills 1");

    // Login, from a pending prompt and one ended by a newline.
    server.say(b"By what name are you known?\r\n");
    server.expect(b"Tamsin\r\n");
    server.say(&[IAC, WILL, ECHO]);
    server.say(b"Password: ");
    server.expect(b"hunter2;hunter3\r\n");
    server.say(&[IAC, 252, ECHO]);

    server.gmcp(r#"Room.Info {"num": 1, "name": "A road", "exits": {"n": 2}}"#);
    server.gmcp(r#"Char.Vitals {"hp": 20, "maxhp": 30}"#);
    server.say(b"<20hp 30mv> ");
    server.say(&[IAC, GA]);
    wait_for("the room", || {
        session
            .stores()
            .read(|room: &Versioned<RoomState>| room.value().room.is_some())
    });

    // A chain and a move: the map follows, and the separator packs it.
    session.send("north;look");
    server.expect(b"north|look\r\n");
    wait_for_terminal(&session, "north;look");

    server.gmcp(r#"Room.Info {"num": 2, "name": "A gate", "exits": {"s": 1}}"#);
    server.gmcp(r#"Char.Vitals {"hp": 19, "maxhp": 30}"#);
    server.say(b"<19hp 29mv> ");
    server.say(&[IAC, GA]);
    wait_for("the move", || {
        session
            .stores()
            .read(|map: &Versioned<MapState>| map.value().whereabouts().room == Some(2))
    });
    session.stores().read(|map: &Versioned<MapState>| {
        let road = map.value().room(1).unwrap();
        assert!(
            road.links.iter().any(|(_, to)| *to == 2),
            "linked by the move"
        );
    });

    drop(server);
    wait_for_status(&session, closed_by_server());
    assert!(!every_store(&session).contains("hunter2"));
    session.quit().wait().unwrap();
    assert!(!contains(&database_bytes(folder.path()), b"hunter2"));
}

#[test]
fn plugin_outputs_reach_the_right_place() {
    let folder = tempfile::tempdir().unwrap();
    let (listener, port) = FakeServer::listen();
    let session = logged_in_session(folder.path(), port);
    let mut server = FakeServer::accept(&listener);
    wait_for_status(&session, ConnectionStatus::Connected);
    server.say(&[IAC, WILL, GMCP]);
    server.gmcp(r#"Core.Hello {"name": "Hearth"}"#);
    server.expect(b"Char.Skills 1");

    server.say(b"Mira waves at you.\r\nYou are now 12% in kick.\r\n");
    server.expect(b"wave Mira\r\n");
    wait_for("the store write", || {
        session.stores().read(|c: &Versioned<CharactersState>| {
            c.value().0.get("skills.kick") == Some(&Value::Number(12.0))
        })
    });
    session.quit().wait().unwrap();

    let database = Database::open(&folder.path().join("servers"), HOST).unwrap();
    let stats = database.reader().unwrap().characters().unwrap().stats;
    let kick = stats.iter().find(|stat| stat.key == "skills.kick").unwrap();
    assert_eq!((&*kick.character_id, &*kick.value), ("tamsin", "12.0"));
}

/// Tab knows the room and mob names the game listed. A room changes with every
/// sighting, so each name is added once, and a name that comes later still is.
#[test]
fn tab_completes_the_room_and_mob_names_the_game_has_listed() {
    let folder = tempfile::tempdir().unwrap();
    let (listener, port) = FakeServer::listen();
    let session = open(config(folder.path(), port));
    session.connect();
    let mut server = FakeServer::accept(&listener);
    wait_for_status(&session, ConnectionStatus::Connected);
    let completes = |typed: &str, word: &str| {
        let done = session.complete(typed, typed.len());
        done.is_some_and(|done| done.line.eq_ignore_ascii_case(word))
    };

    server.say(&gmcp_frame(
        r#"Room.Info {"num": 1, "name": "The Orchard", "exits": {"n": 2}}"#,
    ));
    let wolf = r#"Room.AddPlayer {"1": {"name": "1", "fullname": "A grey wolf stands here."}}"#;
    server.say(&gmcp_frame(wolf));
    wait_for("the wolf", || completes("wol", "wolf"));
    assert!(completes("orc", "orchard"), "the room's name");

    server.say(&gmcp_frame(wolf));
    let bear = r#"Room.AddPlayer {"2": {"name": "2", "fullname": "A tall bear stands here."}}"#;
    server.say(&gmcp_frame(bear));
    wait_for("the bear", || completes("bea", "bear"));
    assert!(completes("wol", "wolf"), "the wolf, still");

    server.say(&gmcp_frame(
        r#"Room.Info {"num": 2, "name": "The Mill", "exits": {"s": 1}}"#,
    ));
    wait_for("the mill", || completes("mil", "mill"));
    assert!(completes("orc", "orchard"), "the first room, still");
    session.quit().wait().unwrap();
}

/// What's typed with echo off goes to the server, verbatim, and nowhere else.
#[test]
fn a_password_typed_with_echo_off_is_kept_nowhere() {
    let folder = tempfile::tempdir().unwrap();
    let (listener, port) = FakeServer::listen();
    let session = open(config(folder.path(), port));
    session.connect();
    let mut server = FakeServer::accept(&listener);
    wait_for_status(&session, ConnectionStatus::Connected);

    session.send("before");
    server.expect(b"before\r\n");
    server.say(&[IAC, WILL, ECHO]);
    server.say(b"Password: ");
    wait_for("echo off", || settings(&session).echo_off);

    const SECRET: &str = "swordfish;n;3e";
    session.send(SECRET);
    server.expect(format!("{SECRET}\r\n").as_bytes());

    server.say(&[IAC, 252, ECHO]);
    wait_for("echo on", || !settings(&session).echo_off);
    session.send("after");
    server.expect(b"after\r\n");
    wait_for_terminal(&session, "after");

    let stores = every_store(&session);
    assert!(stores.contains("before"), "the stores were read");
    assert!(!stores.contains("swordfish"), "{stores}");
    assert_eq!(session.complete("swo", 3), None, "no word from it");
    assert_eq!(session.older("", "").as_deref(), Some("after"));
    assert_eq!(session.older("after", "").as_deref(), Some("before"));
    assert_eq!(session.older("before", ""), None);
    session.quit().wait().unwrap();

    let history = folder.path().join("command-history.json");
    let saved = std::fs::read_to_string(history).unwrap();
    assert!(saved.contains("before") && saved.contains("after"));
    assert!(!saved.contains("swordfish"));
    assert!(!contains(&database_bytes(folder.path()), b"swordfish"));
}

#[test]
fn a_cancel_while_connecting_shows_as_a_cancel() {
    let folder = tempfile::tempdir().unwrap();
    // TEST-NET-1 (RFC 5737): a connect to it hangs, or fails at once where
    // the network has no route. Both inputs are queued before the core
    // thread starts the socket, so the cancel comes first either way.
    let session = open(SessionConfig::new(folder.path(), "192.0.2.1", 4000));
    session.connect();
    session.cancel();
    wait_for_status(
        &session,
        ConnectionStatus::Closed(DisconnectReason::ClosedByClient),
    );
    session.quit().wait().unwrap();
}

/// No GA and no GMCP: the connection going quiet shows the prompt.
#[test]
fn quiet_ends_a_prompt_on_a_server_with_no_ga() {
    let folder = tempfile::tempdir().unwrap();
    let (listener, port) = FakeServer::listen();
    let session = open(SessionConfig::new(folder.path(), HOST, port));
    session.connect();
    let mut server = FakeServer::accept(&listener);
    wait_for_status(&session, ConnectionStatus::Connected);

    server.say(b"Password: ");
    wait_for_terminal(&session, "Password: ");
    server.say(b"\r\n<10hp 2mv> ");
    wait_for_terminal(&session, "<10hp 2mv> ");
    session.quit().wait().unwrap();
}

#[test]
fn quit_closes_the_connection_and_loses_nothing() {
    let folder = tempfile::tempdir().unwrap();
    let (listener, port) = FakeServer::listen();
    let session = logged_in_session(folder.path(), port);
    let mut server = FakeServer::accept(&listener);
    wait_for_status(&session, ConnectionStatus::Connected);
    server.say(&[IAC, WILL, GMCP]);
    server.gmcp(r#"Core.Hello {"name": "Hearth"}"#);
    server.expect(b"Char.Skills 1");

    const SKILLS: usize = 200;
    for skill in 0..SKILLS {
        server.say(format!("You are now 1% in skill{skill}.\r\n").as_bytes());
    }
    server.say(b"You wear the last thing.\r\n");
    wait_for_terminal(&session, "You wear the last thing.");
    session.send("say goodbye");
    server.expect(b"say goodbye\r\n");
    session.quit().wait().unwrap();

    // The read timeout fails here: only a real close passes.
    let mut closed = [0; 1];
    match server.stream.read(&mut closed) {
        Ok(read) => assert_eq!(read, 0, "the socket closed"),
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset),
    }

    let database = Database::open(&folder.path().join("servers"), HOST).unwrap();
    let reader = database.reader().unwrap();
    let stats = reader.characters().unwrap().stats;
    assert_eq!(stats.len(), SKILLS, "every hook's write");
    let meta = reader.meta().unwrap();
    assert!(
        meta.iter()
            .any(|m| m.key.ends_with("gear.worn.the last thing"))
    );
    let history = folder.path().join("command-history.json");
    assert!(
        std::fs::read_to_string(history)
            .unwrap()
            .contains("goodbye")
    );
}

// ---- Shells ----

/// A shell drains once per frame: however many writes a frame holds, one
/// wake and one batch.
#[test]
fn a_shell_sees_one_batch_per_frame() {
    let folder = tempfile::tempdir().unwrap();
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&wakes);
    let session = Session::open(config(folder.path(), 4000), move || {
        counter.fetch_add(1, Ordering::Relaxed);
    })
    .unwrap();
    let mut shell = mudling_core::store::Stores {
        terminal: session.stores().subscribe(),
        vitals: session.stores().subscribe(),
        ..Default::default()
    };

    replay(&session, 1);
    let batch = session.stores().drain().expect("a frame of changes");
    shell.apply_batch(&batch);
    assert!(session.stores().drain().is_none());
    assert_eq!(wakes.load(Ordering::Relaxed), 1, "one wake for the batch");

    let lines = shell.terminal.value().lines().iter();
    let shown = lines.filter_map(Line::without_prompt);
    assert_eq!(shown.count(), terminal(&session).len());
    assert_eq!(shell.vitals.value().0.get(&Vital::Hp), Some(&11));
    session.quit().wait().unwrap();
}

// ---- The engine, driven directly ----

struct Driven {
    engine: Engine,
    hub: Arc<StoreHub>,
    _folder: tempfile::TempDir,
}

impl Driven {
    fn new(plugins: Vec<Plugin>, host: &str) -> Driven {
        Driven::configured(host, |config| config.plugins = plugins)
    }

    fn configured(host: &str, adjust: impl FnOnce(&mut SessionConfig)) -> Driven {
        let folder = tempfile::tempdir().unwrap();
        let hub = Arc::new(StoreHub::new(|| {}));
        let mut config = SessionConfig::new(folder.path(), host, 4000);
        adjust(&mut config);
        let typing = Arc::new(Mutex::new(Typing::default()));
        let engine = Engine::open(config, Arc::clone(&hub), typing).unwrap();
        Driven {
            engine,
            hub,
            _folder: folder,
        }
    }

    fn say(&mut self, bytes: &[u8]) {
        let now = Timestamp(0);
        self.engine
            .receive(Bytes::copy_from_slice(bytes), now)
            .unwrap();
        self.engine.settle(now);
    }

    fn gmcp(&mut self, message: &str) {
        self.say(&gmcp_frame(message));
    }

    /// What a chat hiding the prompts the HUD repeats shows.
    fn terminal(&self) -> Vec<String> {
        let lines = self.lines();
        let shown = lines.iter().filter_map(Line::without_prompt);
        shown.map(|line| line.plain().to_owned()).collect()
    }

    fn lines(&self) -> Vec<Line> {
        self.hub.read(|terminal: &Versioned<TerminalState>| {
            terminal.value().lines().iter().cloned().collect()
        })
    }
}

#[test]
fn game_detection_comes_from_the_host_at_open() {
    let driven = Driven::new(vec![hearth()], "mud.hearth.example");
    let game = driven
        .hub
        .read(|settings: &Versioned<SettingsState>| settings.value().suggestion.clone())
        .expect("the host names the game");
    assert_eq!((game.plugin, game.source), (hearth_id(), Source::Host));
}

/// Detection only suggests (PRD §11): with its plugin off, a game named by
/// the server gets none of the plugin, approved or not.
#[test]
fn a_detected_game_with_its_plugin_off_runs_no_plugin() {
    let folder = tempfile::tempdir().unwrap();
    let session = open(SessionConfig {
        enabled: Vec::new(),
        ..config(folder.path(), 4000)
    });
    replay(&session, 256);

    let game = settings(&session).suggestion.expect("it's still suggested");
    assert_eq!((game.plugin, game.source), (hearth_id(), Source::Mssp));
    assert!(
        terminal(&session).iter().all(|line| line != "wave Mira"),
        "no hook ran"
    );
    let stores = session.stores();
    assert!(stores.read(|c: &Versioned<CharactersState>| c.value().0.is_empty()));
    assert!(stores.read(|g: &Versioned<GearState>| g.value().0.is_empty()));
    assert!(stores.read(|mobs: &Versioned<MobsState>| mobs.value().marks.is_empty()));
    stores.read(|terminal: &Versioned<TerminalState>| {
        let lines = terminal.value().lines();
        let said = lines.iter().find(|l| l.plain().starts_with("Mira says"));
        assert!(said.unwrap().styled().is_empty(), "no plugin coloring");
    });
    session.quit().wait().unwrap();
}

/// Whether a hook of hearth's sent its command during a replay.
fn hook_sent(session: &Session) -> bool {
    terminal(session).iter().any(|line| line == "wave Mira")
}

/// Hearth's patterns color a line whether or not its hooks may send.
fn plugin_colored(session: &Session) -> bool {
    session
        .stores()
        .read(|terminal: &Versioned<TerminalState>| {
            let lines = terminal.value().lines();
            let said = lines.iter().find(|l| l.plain().starts_with("Mira says"));
            !said.unwrap().styled().is_empty()
        })
}

#[test]
fn a_plugin_waiting_for_approval_runs_its_patterns_but_not_its_hooks() {
    let folder = tempfile::tempdir().unwrap();
    let session = open(SessionConfig {
        approved: Vec::new(),
        ..config(folder.path(), 4000)
    });
    replay(&session, 256);

    assert!(!hook_sent(&session));
    assert!(plugin_colored(&session));
    session.quit().wait().unwrap();
}

#[test]
fn approval_given_or_taken_back_applies_from_the_next_line() {
    let folder = tempfile::tempdir().unwrap();
    let taken_back = open(config(folder.path(), 4000));
    taken_back.set_approved(hearth_id(), false);
    replay(&taken_back, 256);
    assert!(!hook_sent(&taken_back), "taken back before the game spoke");
    taken_back.quit().wait().unwrap();

    let folder = tempfile::tempdir().unwrap();
    let given = open(SessionConfig {
        approved: Vec::new(),
        ..config(folder.path(), 4000)
    });
    given.set_approved(hearth_id(), true);
    replay(&given, 256);
    wait_for("the hook's command", || hook_sent(&given));
    given.quit().wait().unwrap();
}

fn room_state(driven: &Driven) -> RoomState {
    driven
        .hub
        .read(|room: &Versioned<RoomState>| room.value().clone())
}

fn character(driven: &Driven, key: &str) -> Option<Value> {
    driven
        .hub
        .read(|characters: &Versioned<CharactersState>| characters.value().0.get(key).cloned())
}

fn target(driven: &Driven) -> Option<Target> {
    driven
        .hub
        .read(|target: &Versioned<TargetState>| target.value().0.clone())
}

fn driven_settings(driven: &Driven) -> SettingsState {
    driven
        .hub
        .read(|settings: &Versioned<SettingsState>| settings.value().clone())
}

/// A connected session, with the first room of `name` announced.
fn in_a_room(name: &str) -> Driven {
    let mut driven = Driven::new(Vec::new(), "mud.example");
    driven.engine.start_connection().unwrap();
    driven.gmcp(&format!(
        r#"Room.Info {{"num": 1, "name": "{name}", "exits": {{"n": 2}}}}"#
    ));
    driven
}

#[test]
fn the_room_fills_from_gmcp_and_empties_with_the_connection() {
    let mut driven = in_a_room("A road");
    driven.gmcp(r#"Char.Status {"character_name": "Tamsin", "level": "22"}"#);
    driven.gmcp(
        r#"Room.Players {"Tamsin": {"name": "Tamsin", "fullname": "Tamsin is here."},
            "Mira": {"name": "Mira", "fullname": "Mira the Quick is here."},
            "7": {"name": "7", "fullname": "(Glowing) A grey wolf sits here."}}"#,
    );
    driven.gmcp(
        r#"Char.Items.List {"location": "room", "items": [
            {"id": "1", "name": "a silver ring"}, {"id": "2", "name": "a silver ring"}]}"#,
    );
    let contents = room_state(&driven).contents;
    assert_eq!(
        contents.occupants,
        [
            Occupant {
                name: "(Glowing) A grey wolf sits here.".to_owned(),
                target: Some("wolf".to_owned())
            },
            Occupant {
                name: "Mira the Quick is here.".to_owned(),
                target: Some("mira".to_owned())
            }
        ]
    );
    assert_eq!(
        contents.items,
        [Item {
            name: "a silver ring".to_owned(),
            target: "ring".to_owned(),
            count: 2
        }]
    );

    driven.gmcp(r#"Room.RemovePlayer "Mira""#);
    assert_eq!(room_state(&driven).contents.occupants.len(), 1);

    driven
        .engine
        .close_connection(DisconnectReason::ClosedByServer, Timestamp(0));
    assert_eq!(room_state(&driven).contents, Default::default());
}

#[test]
fn the_character_and_the_fight_reach_their_stores() {
    let mut driven = in_a_room("A road");
    driven.gmcp(r#"Char.Status {"character_name": "Tamsin", "level": "22"}"#);
    driven.gmcp(r#"Char.Vitals {"hp": "30", "maxhp": "42", "position": "Fight"}"#);
    driven.gmcp(
        r#"Char.Status {"opponent_name": "A rat", "opponent_level": "3",
            "opponent_health": "36", "opponent_health_max": "100"}"#,
    );
    assert_eq!(
        character(&driven, CHARACTER_NAME),
        Some(Value::Text("Tamsin".to_owned()))
    );
    assert_eq!(
        character(&driven, CHARACTER_LEVEL),
        Some(Value::Number(22.0))
    );
    assert_eq!(
        character(&driven, CHARACTER_POSITION),
        Some(Value::Text("Fight".to_owned()))
    );
    assert_eq!(
        target(&driven),
        Some(Target {
            name: "A rat".to_owned(),
            level: Some(3),
            health: Some(36),
            max_health: Some(100)
        })
    );

    driven
        .engine
        .close_connection(DisconnectReason::ClosedByServer, Timestamp(0));
    assert_eq!(target(&driven), None);
    assert_eq!(
        character(&driven, CHARACTER_POSITION),
        Some(Value::Text(String::new()))
    );
}

#[test]
fn the_first_room_asks_for_the_modules_again() {
    let mut driven = Driven::new(Vec::new(), "mud.example");
    driven.engine.start_connection().unwrap();
    driven.say(&[IAC, WILL, GMCP]);
    drop(driven.engine.take_output());
    let room = r#"Room.Info {"num": 1, "name": "A road", "exits": {}}"#;
    driven.gmcp(room);
    assert!(contains(&driven.engine.take_output(), b"Core.Supports.Set"));
    driven.gmcp(room);
    assert!(driven.engine.take_output().is_empty(), "once a connection");
}

#[test]
fn room_players_note_mobs_on_the_map_by_their_lines() {
    let mut driven = in_a_room("A road");
    let wolf = "(Glowing) A grey wolf sits here.";
    let seen = |driven: &Driven| room_state(driven).room.unwrap();
    driven.gmcp(&format!(
        r#"Room.Players {{"7": {{"name": "7", "fullname": "{wolf}"}},
            "Mira": {{"name": "Mira", "fullname": "Mira is here."}}}}"#
    ));
    let room = seen(&driven);
    assert_eq!(room.sightings.keys().collect::<Vec<_>>(), [wolf]);
    assert!(room.features.is_empty(), "no one has marked wolves");

    driven.gmcp(r#"Room.AddPlayer {"8": {"name": "8", "fullname": "A brown bear stands here."}}"#);
    assert_eq!(seen(&driven).sightings.len(), 2);
}

#[test]
fn a_button_press_is_refused_at_a_password_prompt() {
    let mut driven = Driven::new(Vec::new(), "mud.example");
    driven.engine.start_connection().unwrap();
    driven.say(&[IAC, WILL, ECHO]);
    drop(driven.engine.take_output());
    driven.engine.send_action("kill wolf", Timestamp(0));
    assert!(driven.engine.take_output().is_empty());

    driven.engine.send("hunter2", Timestamp(0));
    assert!(contains(&driven.engine.take_output(), b"hunter2"));
}

#[test]
fn color_codes_in_a_room_name_never_reach_the_room() {
    let mut driven = Driven::new(Vec::new(), "mud.example");
    driven.engine.start_connection().unwrap();
    driven.gmcp(
        r#"Room.Info {"num": 1, "name": "A \u001b[1;31mred\u001b[0m road",
            "zone": "\u001b[32mThe Fields\u001b[0m", "exits": {}}"#,
    );
    let room = room_state(&driven).room.unwrap();
    assert_eq!(room.name, "A red road");
    assert_eq!(room.zone, "The Fields");
}

#[test]
fn the_name_in_the_character_store_keeps_the_character_out_of_the_room() {
    let name = "Bren";
    let mut driven = Driven::configured("mud.hearth.example", |config| {
        config.plugins = vec![hearth()];
        config.enabled = vec![hearth_id()];
    });
    driven.engine.start_connection().unwrap();
    driven.gmcp(r#"Room.Info {"num": 1, "name": "A road", "exits": {}}"#);
    driven.gmcp(r#"Room.Players {"Bren": {"name": "Bren", "fullname": "Bren is here."}}"#);
    assert_eq!(room_state(&driven).contents.occupants.len(), 1);

    let write = (CHARACTER_NAME.to_owned(), Value::Text(name.to_owned()));
    driven
        .hub
        .write::<CharactersState>(CharactersState([write].into()));
    driven.gmcp(r#"Room.Players {"Bren": {"name": "Bren", "fullname": "Bren is here."}}"#);
    assert!(room_state(&driven).contents.occupants.is_empty());
}

#[test]
fn the_login_names_the_character_until_the_game_does() {
    let mut driven = Driven::configured("mud.example", |config| {
        config.credentials = Some(Credentials {
            name: "bren".to_owned(),
            password: "unused".to_owned(),
        });
    });
    driven.engine.start_connection().unwrap();
    assert_eq!(
        character(&driven, CHARACTER_NAME),
        Some(Value::Text("bren".to_owned()))
    );
    driven.gmcp(r#"Char.Status {"character_name": "Bren", "level": "9"}"#);
    assert_eq!(
        character(&driven, CHARACTER_NAME),
        Some(Value::Text("Bren".to_owned()))
    );
}

#[test]
fn a_name_from_gmcp_is_saved_for_the_server_list() {
    let folder = tempfile::tempdir().unwrap();
    let hub = Arc::new(StoreHub::new(|| {}));
    let config = SessionConfig::new(folder.path(), "mud.example", 4000);
    let typing = Arc::new(Mutex::new(Typing::default()));
    let mut engine = Engine::open(config, hub, typing).unwrap();
    engine.start_connection().unwrap();
    let status = gmcp_frame(r#"Char.Status {"character_name": "Bren", "level": "9"}"#);
    engine.receive(status.into(), Timestamp(0)).unwrap();
    engine.settle(Timestamp(0));
    engine.finish().unwrap();

    let saved = saved_servers(folder.path());
    let character = &saved.servers[0].characters[0];
    assert_eq!(character.name.as_deref(), Some("Bren"));
    assert_eq!(character.level, Some(9));
}

#[test]
fn a_mob_of_a_marked_kind_marks_the_room() {
    let mut driven = Driven::configured("mud.hearth.example", |config| {
        config.plugins = vec![hearth()];
        config.enabled = vec![hearth_id()];
    });
    driven.engine.start_connection().unwrap();
    driven.gmcp(r#"Room.Info {"num": 1, "name": "A road", "exits": {"n": 2}}"#);
    driven.say(b"A grey wolf lunges at you!\r\n");
    driven.gmcp(r#"Room.Info {"num": 2, "name": "A gate", "exits": {"s": 1}}"#);
    driven.gmcp(r#"Room.Players {"7": {"name": "7", "fullname": "A lean wolf sits here."}}"#);
    let room = room_state(&driven).room.unwrap();
    assert_eq!(room.name, "A gate");
    assert!(room.features.contains(feature::AGGRESSIVE_MOB));
}

/// A game plugin whose `go` hook never ends, and which links the word `exit`.
fn slow_plugin() -> Plugin {
    let manifest = "id = \"slow\"\nname = \"slow\"\nversion = \"1.0.0\"\napi = 1\n\
        license = \"MIT\"\nlayer = \"game\"\n[servers]\nhosts = [\"mud.example\"]\n\
        [points]\ntext = \"text.toml\"\n[hooks]\nfiles = [\"hooks.luau\"]\n";
    let text = "[[pattern]]\nname = \"go\"\npattern = '''^go$'''\nhook = \"on_go\"\n\
        [[link]]\npattern = '''(?<word>exit)'''\naction = \"look {word}\"\n";
    Plugin::from_files(|file| match file {
        "plugin.toml" => Ok(manifest.to_owned()),
        "text.toml" => Ok(text.to_owned()),
        "hooks.luau" => Ok("function on_go(event) while true do end end".to_owned()),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no such file",
        )),
    })
    .unwrap_or_else(|error| panic!("{error}"))
}

/// One that takes too long three times, then the player's click.
#[test]
fn a_plugin_turned_off_says_why_and_can_come_back() {
    let id = slow_plugin().id().clone();
    let mut driven = Driven::configured("mud.example", |config| {
        config.plugins = vec![slow_plugin()];
        config.enabled = vec![id.clone()];
    });
    driven.engine.start_connection().unwrap();
    for _ in 0..3 {
        driven.say(b"go\r\n");
    }
    wait_for("the plugin to be turned off", || {
        driven.say(b"\r\n");
        !driven_settings(&driven).turned_off.is_empty()
    });

    let settings = driven_settings(&driven);
    assert_eq!(settings.turned_off[0].plugin, id);
    assert_eq!(settings.turned_off[0].reason, DisabledReason::Overran(3));
    let turned_off = settings.turned_off[0].to_string();
    assert_eq!(
        turned_off,
        "slow was turned off: its hooks ran too long 3 times"
    );
    assert_eq!(
        settings.notices,
        [] as [String; 0],
        "the overruns are folded"
    );
    assert!(settings.plugin_costs.iter().any(|cost| cost.plugin == id));

    driven.engine.turn_plugin_on(&id);
    assert!(driven_settings(&driven).turned_off.is_empty());

    // It runs again, on a clean count: two overruns leave it on, and a third
    // turns it off once more.
    driven.say(b"go\r\ngo\r\n");
    wait_for("two more overruns", || {
        driven.say(b"\r\n");
        driven_settings(&driven).notices.len() == 2
    });
    assert!(driven_settings(&driven).turned_off.is_empty());
    driven.say(b"go\r\n");
    wait_for("the plugin to be turned off again", || {
        driven.say(b"\r\n");
        !driven_settings(&driven).turned_off.is_empty()
    });
}

#[test]
fn an_override_the_player_turned_off_stays_out_of_the_session() {
    let manifest = "id = \"mine\"\nname = \"Mine\"\nversion = \"1.0.0\"\napi = 1\n\
        license = \"MIT\"\nlayer = \"override\"\n[servers]\nhosts = [\"mud.example\"]\n\
        [points]\ntext = \"text.toml\"\n";
    let mine = Plugin::from_files(|file| match file {
        "plugin.toml" => Ok(manifest.to_owned()),
        "text.toml" => Ok("[[pattern]]\nname = \"go\"\npattern = '''^go$'''\n".to_owned()),
        _ => Err(std::io::Error::other(file.to_owned())),
    })
    .unwrap_or_else(|error| panic!("{error}"));
    let claimed = |off: Vec<PluginId>| {
        let mine = mine.clone();
        let mut driven = Driven::configured("mud.example", move |config| {
            config.plugins = vec![mine];
            config.overrides_off = off;
        });
        driven.engine.start_connection().unwrap();
        driven.engine.inspect(true);
        driven.say(b"go\r\n");
        inspected(&driven)[0].1.clone()
    };
    assert_eq!(claimed(Vec::new()), ["mine"]);
    assert!(claimed(vec![mine.id().clone()]).is_empty());
}

fn inspected(driven: &Driven) -> Vec<(String, Vec<String>)> {
    driven.hub.read(|state: &Versioned<InspectorState>| {
        let lines = state.value().lines.iter();
        lines
            .map(|inspected| {
                let claimers = inspected.claims.iter().map(|c| c.plugin.to_string());
                (inspected.line.plain().to_owned(), claimers.collect())
            })
            .collect()
    })
}

#[test]
fn the_inspector_records_only_while_open_and_forgets_when_closed() {
    let id = slow_plugin().id().clone();
    let mut driven = Driven::configured("mud.example", |config| {
        config.plugins = vec![slow_plugin()];
        config.enabled = vec![id.clone()];
    });
    driven.engine.start_connection().unwrap();
    driven.say(b"before\r\n");
    assert_eq!(inspected(&driven), []);

    driven.engine.inspect(true);
    driven.say(b"go\r\nnobody's\r\n");
    assert_eq!(
        inspected(&driven),
        [
            ("go".to_owned(), vec!["slow".to_owned()]),
            ("nobody's".to_owned(), Vec::new()),
        ]
    );

    driven.engine.inspect(false);
    assert_eq!(inspected(&driven), []);
    driven.say(b"go\r\n");
    assert_eq!(inspected(&driven), [], "closed, so nothing is recorded");
}

#[test]
fn a_real_connection_times_a_command_from_traffic_that_flows() {
    let folder = tempfile::tempdir().unwrap();
    let (listener, port) = FakeServer::listen();
    let session = open(SessionConfig::new(folder.path(), HOST, port));
    session.connect();
    let mut server = FakeServer::accept(&listener);
    wait_for_status(&session, ConnectionStatus::Connected);
    assert_eq!(settings(&session).latency, None);

    session.send("look");
    server.expect(b"look\r\n");
    std::thread::sleep(Duration::from_millis(20));
    server.say(b"A road.\r\n");
    wait_for("a reading", || settings(&session).latency.is_some());
    assert!(settings(&session).latency.unwrap() >= Duration::from_millis(15));
    assert_eq!(server.received, b"", "nothing extra went out");

    drop(server);
    wait_for_status(&session, closed_by_server());
    assert_eq!(settings(&session).latency, None);
    session.quit().wait().unwrap();
}

#[test]
fn a_plugins_link_rule_reaches_a_shell() {
    let folder = tempfile::tempdir().unwrap();
    let session = open(SessionConfig {
        plugins: vec![slow_plugin()],
        enabled: vec![slow_plugin().id().clone()],
        ..SessionConfig::new(folder.path(), HOST, 4000)
    });
    session.replay(Vec::new());
    wait_for_status(&session, closed_by_server());

    let links = session.links(&Line::command("the exit"));
    assert_eq!(links.len(), 1);
    assert_eq!(&*links[0].action, "look exit");
    assert_eq!((links[0].start, links[0].end), (4, 8));
    session.quit().wait().unwrap();
}

/// A queue of none drops every event, as a flood would: the player is told
/// once, not once per line.
#[test]
fn hook_events_dropped_on_a_full_queue_are_noticed_once() {
    let mut driven = Driven::configured("mud.hearth.example", |config| {
        config.plugins = vec![hearth()];
        config.enabled = vec![hearth_id()];
        config.hook_limits.queue = 0;
    });
    driven.engine.start_connection().unwrap();
    driven.say(b"You are now 1% in kick.\r\n");
    driven.say(b"You are now 2% in kick.\r\n");
    let notices = driven
        .hub
        .read(|settings: &Versioned<SettingsState>| settings.value().notices.clone());
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].contains("hearth"), "{notices:?}");
}

/// A plugin that names a hook its files don't define fails to load on every
/// connection, and the notices it leaves stay within their cap.
#[test]
fn notices_stay_within_their_cap_across_reconnects() {
    let plugin = Plugin::from_files(|file| {
        Ok(match file {
            "plugin.toml" => {
                "id = \"broken\"\nname = \"Broken\"\nversion = \"1.0.0\"\napi = 1\n\
                 license = \"MIT\"\nlayer = \"game\"\n[servers]\nmssp_names = [\"Broken\"]\n\
                 [points]\ntext = \"text.toml\"\n[hooks]\nfiles = [\"broken.luau\"]\n"
            }
            "text.toml" => "[[pattern]]\nname = \"go\"\npattern = 'go'\nhook = \"missing\"\n",
            _ => "defined = true\n",
        }
        .to_owned())
    })
    .unwrap_or_else(|error| panic!("{error}"));
    let id = plugin.id().clone();
    let mut driven = Driven::configured("mud.example.org", |config| {
        config.plugins = vec![plugin];
        config.enabled = vec![id];
    });
    for _ in 0..40 {
        driven.engine.start_connection().unwrap();
        driven
            .engine
            .close_connection(DisconnectReason::ClosedByServer, Timestamp(0));
    }
    let notices = driven
        .hub
        .read(|settings: &Versioned<SettingsState>| settings.value().notices.len());
    assert_eq!(notices, 32);
}

#[test]
fn a_plugin_that_is_off_gets_no_login_subscription_or_separator() {
    let mut driven = Driven::configured("mud.hearth.example", |config| {
        config.plugins = vec![hearth()];
        config.credentials = Some(Credentials {
            name: "Tamsin".to_owned(),
            password: "hunter2".to_owned(),
        });
    });
    driven.engine.start_connection().unwrap();
    driven.say(&[IAC, WILL, GMCP]);
    let subscribed = driven.engine.take_output();
    assert!(contains(&subscribed, b"Char 1"));
    assert!(!contains(&subscribed, b"Char.Skills"));

    driven.say(b"By what name are you known?\r\n");
    assert_eq!(driven.engine.take_output(), b"", "no auto-login");
    driven.engine.send("north;look", Timestamp(0));
    assert_eq!(driven.engine.take_output(), b"north\r\nlook\r\n");
}

#[test]
fn a_server_no_plugin_claims_runs_on_the_defaults() {
    let mut driven = Driven::new(vec![hearth()], "mud.example.org");
    driven.engine.start_connection().unwrap();
    driven.say(b"Mira waves at you.\r\n");
    driven
        .engine
        .close_connection(DisconnectReason::ClosedByServer, Timestamp(0));
    let game = driven
        .hub
        .read(|settings: &Versioned<SettingsState>| settings.value().suggestion.clone());
    assert_eq!(game, None);
    assert_eq!(driven.terminal(), ["Mira waves at you."]);
}

/// Each connection learns the prompt again: a fresh `Defaults`.
#[test]
fn a_reconnect_starts_from_nothing_learned() {
    let mut driven = Driven::new(Vec::new(), "mud.example.org");
    for round in 0..2 {
        driven.engine.start_connection().unwrap();
        driven.gmcp(r#"Char.Vitals {"hp": 20}"#);
        driven.say(b"<20hp> ");
        driven.say(&[IAC, GA]);
        driven.say(b"<19hp> ");
        driven.say(&[IAC, GA]);
        driven
            .engine
            .close_connection(DisconnectReason::ClosedByServer, Timestamp(0));
        assert_eq!(driven.terminal(), [] as [&str; 0], "round {round}");
    }
    driven.engine.start_connection().unwrap();
    driven.say(b"<18hp> ");
    driven.engine.quiet(Timestamp(0));
    driven.engine.settle(Timestamp(0));
    assert_eq!(
        driven.terminal(),
        ["<18hp> "],
        "no HUD numbers yet on this connection, so the prompt shows"
    );
}

/// A move sent as the connection died never got its answer: the next
/// connection's first room isn't where it led.
#[test]
fn a_move_in_flight_doesnt_cross_a_reconnect() {
    let mut driven = Driven::new(Vec::new(), "mud.example.org");
    driven.engine.start_connection().unwrap();
    driven.gmcp(r#"Room.Info {"num": 1, "name": "A road", "exits": {"n": 2}}"#);
    driven.engine.send("north", Timestamp(0));
    driven
        .engine
        .close_connection(DisconnectReason::ClosedByServer, Timestamp(0));

    driven.engine.start_connection().unwrap();
    driven.gmcp(r#"Room.Info {"num": 50, "name": "The temple", "exits": {}}"#);
    let links = driven.hub.read(|map: &Versioned<MapState>| {
        let links = map.value().room(1).unwrap().links.iter();
        links.map(|(_, to)| *to).collect::<Vec<_>>()
    });
    assert_eq!(links, [] as [i64; 0]);
}

/// Three prompts of each shape, each with `Char.Vitals` and GA. `{}` is the
/// number that changes.
fn prompts(driven: &mut Driven, shape: &str) {
    for hp in 0..3 {
        driven.gmcp(&format!(r#"Char.Vitals {{"hp": {hp}}}"#));
        driven.say(shape.replace("{}", &hp.to_string()).as_bytes());
        driven.say(&[IAC, GA]);
    }
}

/// A prompt that grows, with its old shape seen as much: ended at a signal,
/// no part of it shows, whatever the part looks like.
#[test]
fn a_grown_prompt_is_hidden_whole() {
    let cases = [
        ("<{}hp> ", "<{}hp> <{}mv> "),
        ("<{}hp> ", "<{}hp> [Fighting] "),
        ("<{}hp> ", "<{}hp> [In Combat] "),
        ("<{}hp> ", "<{}hp> Tnl 500> "),
        ("<{}hp> ", "<{}hp> Tank healthy> "),
        ("HP {} MP 2 ", "HP {} MP 2 MV 3 "),
    ];
    for (bare, grown) in cases {
        let mut driven = Driven::new(Vec::new(), "mud.example.org");
        driven.engine.start_connection().unwrap();
        prompts(&mut driven, bare);
        prompts(&mut driven, grown);
        assert_eq!(driven.terminal(), [] as [&str; 0], "{grown:?}");
    }
}

/// On a game with `Char.Vitals`, only it teaches prompts: a numbered question
/// or pager line ended at a signal stays visible however often it repeats.
#[test]
fn repeated_numbered_lines_stay_visible_with_vitals() {
    let signaled = |driven: &mut Driven, line: &str| {
        driven.say(line.as_bytes());
        driven.say(&[IAC, GA]);
    };
    let question = "Choose a class (1-5): ";
    for between_vitals in [false, true] {
        let mut driven = Driven::new(Vec::new(), "mud.example.org");
        driven.engine.start_connection().unwrap();
        prompts(&mut driven, "<{}hp> ");
        for _ in 0..3 {
            signaled(&mut driven, question);
            if between_vitals {
                prompts(&mut driven, "<{}hp> ");
            }
        }
        signaled(&mut driven, "--More-- (20%) ");
        signaled(&mut driven, "--More-- (45%) ");
        assert_eq!(
            driven.terminal(),
            [
                question,
                question,
                question,
                "--More-- (20%) ",
                "--More-- (45%) "
            ],
            "Vitals between: {between_vitals}"
        );
    }
}

/// A menu repeated at login, before the first `Char.Vitals`, isn't a prompt
/// once the game turns out to have GMCP.
#[test]
fn shapes_repeated_before_the_first_vitals_are_forgotten() {
    let mut driven = Driven::new(Vec::new(), "mud.example.org");
    driven.engine.start_connection().unwrap();
    let question = "Choose a class (1-5): ";
    for _ in 0..2 {
        driven.say(question.as_bytes());
        driven.say(&[IAC, GA]);
    }
    prompts(&mut driven, "<{}hp> ");
    driven.say(question.as_bytes());
    driven.say(&[IAC, GA]);
    assert_eq!(driven.terminal(), [question, question, question]);
}

/// Ended by a newline, output after a prompt shows, even with a shape learned
/// from that same text at a signal.
#[test]
fn newline_ended_output_after_a_prompt_stays_visible() {
    for output in ["You rest.", "You are carrying:", "You have 5 gold."] {
        let mut driven = Driven::new(Vec::new(), "mud.example.org");
        driven.engine.start_connection().unwrap();
        prompts(&mut driven, "<{}hp> ");
        prompts(&mut driven, &format!("<{{}}hp> {output}"));
        driven.say(format!("<7hp> {output}\r\n").as_bytes());
        assert_eq!(driven.terminal(), [output], "{output:?}");
    }
}

#[test]
fn output_glued_after_a_prompt_shows_without_the_prompt() {
    let mut driven = Driven::new(Vec::new(), "mud.example.org");
    driven.engine.start_connection().unwrap();
    driven.gmcp(r#"Char.Vitals {"hp": 20}"#);
    driven.say(b"<20hp> ");
    driven.say(&[IAC, GA]);
    driven.gmcp(r#"Char.Vitals {"hp": 19}"#);
    driven.say(b"<19hp> You rest.\r\n");
    assert_eq!(driven.terminal(), ["You rest."]);
}

/// A shell's setting has to reach lines already in the scrollback, so prompts
/// stay there, marked. Before the HUD has numbers there's nothing to hide for.
#[test]
fn prompts_the_hud_repeats_stay_in_the_store_marked() {
    let mut driven = Driven::new(Vec::new(), "mud.example.org");
    driven.engine.start_connection().unwrap();
    driven.say(b"<20hp> ");
    driven.say(&[IAC, GA]);
    driven.gmcp(r#"Char.Vitals {"hp": 20}"#);
    driven.say(b"<20hp> ");
    driven.say(&[IAC, GA]);
    driven.gmcp(r#"Char.Vitals {"hp": 19}"#);
    driven.say(b"<19hp> You rest.\r\n");

    let lines = driven.lines();
    let marks: Vec<(&str, usize)> = lines
        .iter()
        .map(|line| (line.plain(), line.prompt_len()))
        .collect();
    assert_eq!(
        marks,
        [("<20hp> ", 0), ("<20hp> ", 7), ("<19hp> You rest.", 7)]
    );
    assert!(lines[1].is_only_prompt());
    assert_eq!(lines[1].without_prompt(), None);
}

#[test]
fn a_hud_point_lists_affects_and_an_empty_list_clears_them() {
    let plugin = Plugin::from_files(|file| {
        Ok(match file {
            "plugin.toml" => {
                "id = \"lister\"\nname = \"Lister\"\nversion = \"1.0.0\"\napi = 1\n\
                 license = \"MIT\"\nlayer = \"game\"\n[servers]\nmssp_names = [\"Lister\"]\n\
                 [points]\nhud = \"hud.toml\"\n"
            }
            _ => "affects = \"Char.Status.affects\"\n",
        }
        .to_owned())
    })
    .unwrap_or_else(|error| panic!("{error}"));
    let id = plugin.id().clone();
    let mut driven = Driven::configured("mud.example.org", |config| {
        config.plugins = vec![plugin];
        config.enabled = vec![id];
    });
    driven.engine.start_connection().unwrap();
    let affects = |driven: &Driven| {
        driven
            .hub
            .read(|characters: &Versioned<CharactersState>| characters.value().affects())
    };

    driven.gmcp(r#"Char.Status {"affects": {"Kind: bless": "for 6 hours", "haste": "a while"}}"#);
    let names: Vec<String> = affects(&driven).into_iter().map(|a| a.name).collect();
    assert_eq!(names, ["bless", "haste"]);
    assert_eq!(affects(&driven)[0].detail, "for 6 hours");

    driven.gmcp(r#"Char.Status {"health": 12}"#);
    assert_eq!(
        affects(&driven).len(),
        2,
        "a status without them leaves them"
    );
    driven.gmcp(r#"Char.Status {"affects": null}"#);
    assert_eq!(affects(&driven), []);

    driven.gmcp(r#"Char.Status {"affects": {"haste": "a while"}}"#);
    assert_eq!(affects(&driven).len(), 1);
    driven
        .engine
        .close_connection(DisconnectReason::ClosedByServer, Timestamp(0));
    assert_eq!(affects(&driven), [], "a dropped connection clears them");
}

#[test]
fn a_speedwalk_sends_its_moves_and_the_map_expects_them() {
    let mut driven = Driven::new(Vec::new(), "mud.example.org");
    driven.engine.start_connection().unwrap();
    driven.engine.take_output();
    driven.engine.send("2n, e", Timestamp(0));
    let sent = driven.engine.take_output();
    assert_eq!(sent, b"north\r\nn\r\neast\r\n");
    driven.gmcp(r#"Room.Info {"num": 1, "name": "A", "exits": {"n": 2}}"#);
    driven.gmcp(r#"Room.Info {"num": 2, "name": "B", "exits": {"s": 1}}"#);
    let linked = driven
        .hub
        .read(|map: &Versioned<MapState>| map.value().room(1).unwrap().links.iter().count());
    assert_eq!(linked, 1);
}

#[test]
fn corrupt_compressed_data_is_an_error_to_close_on() {
    let mut driven = Driven::new(Vec::new(), "mud.example.org");
    driven.engine.start_connection().unwrap();
    let mut bytes = vec![IAC, WILL, 86, IAC, SB, 86, IAC, SE];
    bytes.extend_from_slice(b"not zlib at all");
    let result = driven.engine.receive(Bytes::from(bytes), Timestamp(0));
    assert!(result.is_err());
}

/// A game with no prompt it declares, a table, and a hook that writes when
/// the table ends, and how.
fn scribe() -> Plugin {
    let manifest = "id = \"scribe\"\nname = \"Scribe\"\nversion = \"1.0.0\"\napi = 1\n\
         license = \"MIT\"\nlayer = \"game\"\npermissions = [\"read_state\", \"write_store:seen\"]\n\
         [servers]\nhosts = [\"mud.example.org\"]\n\
         [points]\ntext = \"text.toml\"\ncharacter = \"character.toml\"\nmap = \"map.toml\"\n\
         [hooks]\nfiles = [\"scribe.luau\"]\n";
    let text = "[pager]\nprompt = '''--more--$'''\nkey = \"c\"\n\
         [[pattern]]\nname = \"changed\"\npattern = '''^You change your gear\\.$'''\n\
         hook = \"on_changed\"\n";
    let character = "[[reader]]\nname = \"bag\"\nstart = '''^Your bag:$'''\n\
         row = '''^- (?<item>.+)$'''\nhook = \"on_bag\"\n";
    let map = "[[exits]]\nname = \"exits\"\npattern = '''^Exits:'''\n\
         [[flee]]\nname = \"fled\"\npattern = '''^You run (?<dir>north|south)'''\n\
         [[darkness]]\nname = \"dark\"\npattern = '''^Pitch dark$'''\n";
    let hooks = "local rows = 0\n\
         function on_bag(event)\n\
         local kind = string.match(event.name, '[^.]+$')\n\
         if kind == 'row' then rows += 1 end\n\
         if kind == 'end' or kind == 'cut' then\n\
         mud.store('seen', 'rows', rows)\n\
         mud.store('seen', kind, mud.state('inventory.count') or -1)\nend\nend\n\
         function on_changed(event) mud.refresh_inventory() end";
    Plugin::from_files(|file| {
        Ok(match file {
            "plugin.toml" => manifest,
            "text.toml" => text,
            "character.toml" => character,
            "map.toml" => map,
            "scribe.luau" => hooks,
            _ => return Err(std::io::Error::other(file.to_owned())),
        }
        .to_owned())
    })
    .unwrap_or_else(|error| panic!("{error}"))
}

impl Driven {
    fn scribe() -> Driven {
        let plugin = scribe();
        let mut driven = Driven::configured("mud.example.org", |config| {
            config.enabled = vec![plugin.id().clone()];
            config.plugins = vec![plugin];
        });
        driven.engine.start_connection().unwrap();
        driven
    }

    fn seen(&mut self, key: &str) -> Option<Value> {
        let key = format!("seen.{key}");
        wait_for(&key.clone(), || {
            self.engine.settle(Timestamp(0));
            self.hub
                .read(|stats: &Versioned<CharactersState>| stats.value().0.contains_key(&key))
        });
        self.hub
            .read(|stats: &Versioned<CharactersState>| stats.value().0.get(&key).cloned())
    }

    fn seen_rows(&mut self) -> usize {
        match self.seen("rows") {
            Some(Value::Number(rows)) => rows as usize,
            other => panic!("{other:?}"),
        }
    }

    fn learn_prompt(&mut self) {
        self.gmcp(r#"Char.Vitals {"hp": 20}"#);
        self.say(b"<20hp> ");
        self.say(&[IAC, GA]);
    }
}

#[test]
fn a_learned_prompt_ends_a_table_and_what_follows_it_is_read() {
    let mut driven = Driven::scribe();
    driven.learn_prompt();
    driven.say(b"Your bag:\r\n- rope\r\n");
    driven.gmcp(r#"Char.Vitals {"hp": 19}"#);
    driven.say(b"<19hp> Your bag:\r\n- lamp\r\n");
    assert_eq!(driven.seen("end"), Some(Value::Number(-1.0)));
}

#[test]
fn the_pager_is_answered_in_a_table_and_left_alone_outside_one() {
    let mut driven = Driven::scribe();
    driven.say(b"--more--");
    assert!(!contains(&driven.engine.take_output(), b"c\r\n"));
    driven.say(b"\r\nYour bag:\r\n- rope\r\n--more--");
    assert!(contains(&driven.engine.take_output(), b"c\r\n"));
    driven.say(b"\r\n\r\n");
    driven.say(b"<20hp> ");
    driven.say(&[IAC, GA]);
    assert_eq!(driven.seen("end"), Some(Value::Number(-1.0)));
}

#[test]
fn a_pager_line_at_a_signal_is_no_prompt_with_vitals_awaited() {
    let mut driven = Driven::scribe();
    driven.gmcp(r#"Char.Vitals {"hp": 20}"#);
    driven.say(b"Your bag:\r\n- rope\r\n--more--");
    driven.say(&[IAC, GA]);
    driven.say(b"\r\n- lamp\r\n");
    driven.gmcp(r#"Char.Vitals {"hp": 18}"#);
    driven.say(b"<18hp> ");
    driven.say(&[IAC, GA]);
    assert_eq!(driven.seen("end"), Some(Value::Number(-1.0)));
    assert_eq!(driven.seen_rows(), 2, "the rest of the table was read");
}

#[test]
fn a_pager_prompt_is_not_learned_and_later_pages_read_on() {
    let mut driven = Driven::scribe();
    driven.gmcp(r#"Char.Vitals {"hp": 20}"#);
    driven.say(b"Your bag:\r\n- rope\r\n--more--");
    driven.gmcp("Char.Status {}");
    driven.engine.quiet(Timestamp(0));
    driven.say(b"\r\n- lamp\r\n--more--");
    driven.say(&[IAC, GA]);
    driven.say(b"\r\n- coil\r\n");
    driven.gmcp(r#"Char.Vitals {"hp": 18}"#);
    driven.say(b"<18hp> ");
    driven.say(&[IAC, GA]);
    assert_eq!(driven.seen("end"), Some(Value::Number(-1.0)));
    assert_eq!(driven.seen_rows(), 3);
}

#[test]
fn a_connection_ending_in_a_table_cuts_it() {
    let mut driven = Driven::scribe();
    driven.say(b"Your bag:\r\n- rope\r\n");
    let now = Timestamp(0);
    driven
        .engine
        .close_connection(DisconnectReason::ClosedByServer, now);
    let cut = driven
        .hub
        .read(|stats: &Versioned<CharactersState>| stats.value().0.get("seen.cut").cloned());
    assert_eq!(cut, Some(Value::Number(-1.0)));
}

#[test]
fn a_connection_ending_on_a_half_row_cuts_the_table() {
    let mut driven = Driven::scribe();
    driven.say(b"Your bag:\r\n- rope\r\n- la");
    let now = Timestamp(0);
    driven
        .engine
        .close_connection(DisconnectReason::ClosedByServer, now);
    let seen = driven.hub.read(|stats: &Versioned<CharactersState>| {
        let seen = &stats.value().0;
        (seen.contains_key("seen.cut"), seen.contains_key("seen.end"))
    });
    assert_eq!(seen, (true, false));
}

#[test]
fn a_stall_on_a_half_row_leaves_the_table_open() {
    let mut driven = Driven::scribe();
    driven.say(b"Your bag:\r\n- rope\r\n- la");
    driven.engine.quiet(Timestamp(0));
    driven.say(b"mp\r\n- coil\r\n<20hp> ");
    driven.say(&[IAC, GA]);
    assert_eq!(driven.seen("end"), Some(Value::Number(-1.0)));
    assert_eq!(
        driven.seen_rows(),
        3,
        "both rows, the half one, and the last"
    );
}

#[test]
fn the_inventory_is_asked_for_once_on_entering_the_game() {
    let ask = b"Char.Items.Inv";
    let mut driven = Driven::scribe();
    driven.say(b"Hello\r\n");
    assert!(!contains(&driven.engine.take_output(), ask));
    driven.gmcp(r#"Room.Info {"num":1,"name":"Hall","zone":"z","exits":{}}"#);
    assert!(contains(&driven.engine.take_output(), ask));
    driven.gmcp(r#"Room.Info {"num":1,"name":"Hall","zone":"z","exits":{}}"#);
    assert!(!contains(&driven.engine.take_output(), ask));
}

#[test]
fn a_hook_can_ask_for_the_inventory_again() {
    let mut driven = Driven::scribe();
    driven.say(&[IAC, WILL, GMCP]);
    driven.say(b"You change your gear.\r\n");
    wait_for("the hook's ask", || {
        driven.engine.settle(Timestamp(0));
        contains(&driven.engine.take_output(), b"Char.Items.Inv")
    });
}

#[test]
fn a_hook_asks_for_nothing_on_a_server_without_gmcp() {
    let mut driven = Driven::scribe();
    driven.say(b"You change your gear.\r\nYour bag:\r\n- rope\r\n<20hp> ");
    driven.say(&[IAC, GA]);
    // The hook that writes this ran after the one that asked.
    driven.seen("end");
    driven.engine.settle(Timestamp(0));
    assert!(!contains(&driven.engine.take_output(), b"Char.Items.Inv"));
}

#[test]
fn a_table_started_by_the_flushed_half_line_is_cut_too() {
    let mut driven = Driven::scribe();
    driven.say(b"Your bag:");
    let now = Timestamp(0);
    driven
        .engine
        .close_connection(DisconnectReason::ClosedByServer, now);
    let cut = driven
        .hub
        .read(|stats: &Versioned<CharactersState>| stats.value().0.contains_key("seen.cut"));
    assert!(cut);
}

#[test]
fn hooks_read_the_inventory_from_gmcp() {
    let mut driven = Driven::scribe();
    driven.gmcp(
        r#"Char.Items.List {"location":"inv","items":[{"id":"1","name":"a rope"},{"id":"2","name":"a lamp"}]}"#,
    );
    driven.gmcp(r#"Char.Items.Add {"location":"room","item":{"id":"3","name":"a rock"}}"#);
    driven.say(b"Your bag:\r\n- rope\r\n");
    driven.learn_prompt();
    assert_eq!(driven.seen("end"), Some(Value::Number(2.0)));
}

#[test]
fn map_rules_reach_the_map_through_the_merged_matcher() {
    let mut driven = Driven::scribe();
    driven.gmcp(r#"Room.Info {"num":1,"name":"Hall","zone":"z","exits":{"n":2}}"#);
    driven.engine.send("north", Timestamp(0));
    driven.say(b"Yard\r\nExits: south\r\nA wide yard.\r\n");
    driven.gmcp(r#"Room.Info {"num":2,"name":"Yard","zone":"z","exits":{"s":1}}"#);
    let room = driven
        .hub
        .read(|room: &Versioned<RoomState>| room.value().room.clone());
    assert_eq!(room.unwrap().description, "A wide yard.");
}
