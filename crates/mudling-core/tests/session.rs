//! The session end to end: a recorded byte stream, and a fake server on a
//! real socket, through to the stores and the database.

use mudling_core::defaults::detect::Source;
use mudling_core::defaults::hud::Vital;
use mudling_core::map::Mark;
use mudling_core::net::DisconnectReason;
use mudling_core::persist::{Database, Timestamp};
use mudling_core::plugin::luau::Value;
use mudling_core::plugin::{Plugin, PluginId};
use mudling_core::session::{Credentials, Engine, Session, SessionConfig, Typing};
use mudling_core::store::{
    CharactersState, ConnectionStatus, GearState, MapState, MobsState, RoomState, SettingsState,
    StoreHub, TerminalState, Versioned, VitalsState,
};
use mudling_core::telnet::Bytes;
use mudling_core::telnet::command::{DO, GA, IAC, SB, SE, WILL};
use mudling_core::telnet::option::{ECHO, GMCP};
use mudling_core::{Line, LineKind};
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
        approved: vec![hearth_id()],
        ..SessionConfig::new(folder, HOST, port)
    }
}

fn open(config: SessionConfig) -> Session {
    Session::open(config, || {}).expect("the session starts")
}

fn terminal(session: &Session) -> Vec<String> {
    session
        .stores()
        .read(|terminal: &Versioned<TerminalState>| {
            terminal
                .value()
                .lines()
                .iter()
                .map(|line| line.plain().to_owned())
                .collect()
        })
}

/// The server's lines, without commands: hooks run off the line thread, so
/// where a hook's command lands among them isn't fixed.
fn server_lines(session: &Session) -> Vec<String> {
    session
        .stores()
        .read(|terminal: &Versioned<TerminalState>| {
            terminal
                .value()
                .lines()
                .iter()
                .filter(|line| line.kind() == LineKind::Server)
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
        assert!(mobs.value().marks.contains(&(Mark::Aggressive, "wolf".to_owned())));
    });
    let character = stores.read(|c: &Versioned<CharactersState>| c.value().0.clone());
    assert_eq!(character.get("skills.dodge"), Some(&Value::Number(45.0)));
    let gear = stores.read(|g: &Versioned<GearState>| g.value().0.clone());
    assert_eq!(gear.get("gear.worn.a pink ring"), Some(&Value::Bool(true)));
    let game = settings(&session).game.expect("the MSSP name named the game");
    assert_eq!((game.plugin, game.source), (hearth_id(), Source::Mssp));

    session.quit().wait().unwrap();
    let database = Database::open(&folder.path().join("servers"), HOST).unwrap();
    let reader = database.reader().unwrap();
    assert_eq!(reader.map().unwrap().rooms.len(), 2);
    let stats = reader.characters().unwrap().stats;
    assert!(stats.iter().any(|stat| stat.key == "skills.dodge"));
    assert!(reader.mob_marks().unwrap().iter().any(|m| m.name == "wolf"));
    let meta = reader.meta().unwrap();
    assert!(meta.iter().any(|m| m.key.ends_with("gear.worn.a pink ring")));
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
    let character = stores.read(|c: &Versioned<CharactersState>| c.value().0.clone());
    assert_eq!(character.get("skills.dodge"), Some(&Value::Number(45.0)));
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
        self.say(&[IAC, SB, GMCP]);
        self.say(message.as_bytes());
        self.say(&[IAC, SE]);
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

    // GMCP: the hello and the subscriptions, the game plugin's included,
    // once the game names itself.
    server.say(&[IAC, WILL, GMCP]);
    server.expect(&[IAC, DO, GMCP]);
    server.expect(b"Core.Hello");
    server.expect(b"Char 1");
    server.gmcp(r#"Core.Hello {"name": "Hearth"}"#);
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
        assert!(road.links.iter().any(|(_, to)| *to == 2), "linked by the move");
    });

    drop(server);
    wait_for_status(&session, closed_by_server());
    session.quit().wait().unwrap();
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

    assert!(terminal(&session).iter().all(|line| !line.contains("swordfish")));
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

/// No GA and no GMCP: the connection going quiet shows the prompt, and
/// the prompt repeated is learned, so the next one ends itself.
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
    server.say(b"\r\n<9hp 2mv> ");
    wait_for_terminal(&session, "<9hp 2mv> ");

    let learned = Instant::now();
    server.say(b"\r\n<8hp 2mv> ");
    wait_for_terminal(&session, "<8hp 2mv> ");
    assert!(
        learned.elapsed() < mudling_core::net::QUIET_AFTER,
        "a learned prompt doesn't wait for quiet"
    );
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

    let mut closed = [0; 1];
    let read = server.stream.read(&mut closed).unwrap_or(0);
    assert_eq!(read, 0, "the socket closed");

    let database = Database::open(&folder.path().join("servers"), HOST).unwrap();
    let reader = database.reader().unwrap();
    let stats = reader.characters().unwrap().stats;
    assert_eq!(stats.len(), SKILLS, "every hook's write");
    let meta = reader.meta().unwrap();
    assert!(meta.iter().any(|m| m.key.ends_with("gear.worn.the last thing")));
    let history = folder.path().join("command-history.json");
    assert!(std::fs::read_to_string(history).unwrap().contains("goodbye"));
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

    let lines: Vec<Line> = shell.terminal.value().lines().iter().cloned().collect();
    assert_eq!(lines.len(), terminal(&session).len());
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
        let folder = tempfile::tempdir().unwrap();
        let hub = Arc::new(StoreHub::new(|| {}));
        let config = SessionConfig {
            plugins,
            ..SessionConfig::new(folder.path(), host, 4000)
        };
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
        self.engine.receive(Bytes::copy_from_slice(bytes), now).unwrap();
        self.engine.settle(now);
    }

    fn gmcp(&mut self, message: &str) {
        let mut bytes = vec![IAC, SB, GMCP];
        bytes.extend_from_slice(message.as_bytes());
        bytes.extend_from_slice(&[IAC, SE]);
        self.say(&bytes);
    }

    fn terminal(&self) -> Vec<String> {
        self.hub.read(|terminal: &Versioned<TerminalState>| {
            let lines = terminal.value().lines().iter();
            lines.map(|line| line.plain().to_owned()).collect()
        })
    }
}

#[test]
fn game_detection_comes_from_the_host_at_open() {
    let driven = Driven::new(vec![hearth()], "mud.hearth.example");
    let game = driven
        .hub
        .read(|settings: &Versioned<SettingsState>| settings.value().game.clone())
        .expect("the host names the game");
    assert_eq!((game.plugin, game.source), (hearth_id(), Source::Host));
}

#[test]
fn a_server_no_plugin_claims_runs_on_the_defaults() {
    let mut driven = Driven::new(vec![hearth()], "mud.example.org");
    driven.engine.start_connection().unwrap();
    driven.say(b"Mira waves at you.\r\n");
    driven.engine.close_connection(DisconnectReason::ClosedByServer, Timestamp(0));
    let game = driven
        .hub
        .read(|settings: &Versioned<SettingsState>| settings.value().game.clone());
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
        driven.engine.close_connection(DisconnectReason::ClosedByServer, Timestamp(0));
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

#[test]
fn a_speedwalk_sends_its_moves_and_the_map_expects_them() {
    let mut driven = Driven::new(Vec::new(), "mud.example.org");
    driven.engine.start_connection().unwrap();
    driven.engine.take_output();
    driven.engine.send("2n, e", Timestamp(0));
    let sent = driven.engine.take_output();
    assert_eq!(sent, b"north\r\nnorth\r\neast\r\n");
    driven.gmcp(r#"Room.Info {"num": 1, "name": "A", "exits": {"n": 2}}"#);
    driven.gmcp(r#"Room.Info {"num": 2, "name": "B", "exits": {"s": 1}}"#);
    let linked = driven.hub.read(|map: &Versioned<MapState>| {
        map.value().room(1).unwrap().links.iter().count()
    });
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
