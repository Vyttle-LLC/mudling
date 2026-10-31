//! Avatar through the whole session: suggested at runtime by its host, and
//! once turned on, its hooks' writes reaching the stores.

use mudling_core::defaults::detect::Source;
use mudling_core::net::DisconnectReason;
use mudling_core::persist::Timestamp;
use mudling_core::plugin::luau::Value;
use mudling_core::session::{Credentials, Engine, Session, SessionConfig, Typing};
use mudling_core::store::{
    CharactersState, ConnectionStatus, RoomState, SettingsState, StoreHub, Versioned,
};
use mudling_core::telnet::Bytes;
use mudling_core::telnet::command::{IAC, SB, SE};
use mudling_core::telnet::option::GMCP;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const HOST: &str = "avatar.outland.org";
const PROMPT: &[u8] = b"(120h-130m-104v-1210)(-/-)";

/// An engine with Avatar on, its connection started, and its stores.
fn engine(folder: &Path) -> (Engine, Arc<StoreHub>) {
    let plugin = mudling_plugin_avatar::load();
    let config = SessionConfig {
        enabled: vec![plugin.id().clone()],
        plugins: vec![plugin],
        ..SessionConfig::new(folder, HOST, 4000)
    };
    let hub = Arc::new(StoreHub::new(|| {}));
    let typing = Arc::new(Mutex::new(Typing::default()));
    let mut engine = Engine::open(config, Arc::clone(&hub), typing).unwrap();
    engine.start_connection().unwrap();
    (engine, hub)
}

#[test]
fn the_login_prompts_are_each_answered_once() {
    let folder = tempfile::tempdir().unwrap();
    let plugin = mudling_plugin_avatar::load();
    let config = SessionConfig {
        enabled: vec![plugin.id().clone()],
        plugins: vec![plugin],
        credentials: Some(Credentials {
            name: "Fen".to_owned(),
            password: "pass-word".to_owned(),
        }),
        ..SessionConfig::new(folder.path(), HOST, 4000)
    };
    let hub = Arc::new(StoreHub::new(|| {}));
    let typing = Arc::new(Mutex::new(Typing::default()));
    let mut engine = Engine::open(config, hub, typing).unwrap();
    engine.start_connection().unwrap();
    let mut answer = |prompt: &'static [u8]| {
        let now = Timestamp(0);
        engine.receive(Bytes::from_static(prompt), now).unwrap();
        engine.quiet(now);
        String::from_utf8(engine.take_output()).unwrap()
    };

    let name = answer(b"What name shall you be known by, adventurer? ");
    assert_eq!(name.trim(), "Fen");
    let password = answer(b"Your Password: ");
    assert_eq!(password.trim(), "pass-word");
    let banner = answer(b"When you have read this, please press <RETURN> to continue ---->");
    assert_eq!(banner.trim(), "");
    assert!(!banner.is_empty(), "the banner wants a blank line");
    assert_eq!(answer(b"Your Password: "), "", "a second ask is a refusal");
}

#[test]
fn a_stall_at_the_pager_with_vitals_awaited_leaves_the_table_open() {
    let folder = tempfile::tempdir().unwrap();
    let (mut engine, hub) = engine(folder.path());
    let now = Timestamp(0);
    let mut vitals = vec![IAC, SB, GMCP];
    vitals.extend_from_slice(br#"Char.Vitals {"hp":"119"}"#);
    vitals.extend_from_slice(&[IAC, SE]);
    engine.receive(Bytes::from(vitals), now).unwrap();

    let first_page = b"(119h-130m-104v-1210)(-/-)You have 12 practice sessions left.\r\n\
        Name                 % Lvl Sub Mana\r\n\
        dodge               90   4   -    0\r\n\
        [B]ack,[H]elp,[R]efresh, or [C]ontinue: ";
    engine.receive(Bytes::from_static(first_page), now).unwrap();
    engine.quiet(now);
    let last_page = b"\r\nzeal                90   8   -    5\r\n(119h-130m-104v-1210)(-/-)";
    engine.receive(Bytes::from_static(last_page), now).unwrap();
    engine.close_connection(DisconnectReason::ClosedByServer, now);

    let skill = |key: &str| {
        hub.read(|character: &Versioned<CharactersState>| character.value().0.get(key).cloned())
    };
    assert_eq!(skill("skills.practice.read"), Some(Value::Number(2.0)));
    assert_eq!(skill("skills.zeal.learned"), Some(Value::Number(90.0)));
}

#[test]
fn a_room_keeps_the_description_after_its_exits_line() {
    let folder = tempfile::tempdir().unwrap();
    let (mut engine, hub) = engine(folder.path());

    // By lines, so a checkout with CRLF endings reads the same.
    let mut bytes = Vec::new();
    for line in include_str!("fixtures/room.txt").lines() {
        bytes.extend_from_slice(line.as_bytes());
        bytes.extend_from_slice(b"\r\n");
    }
    bytes.extend_from_slice(&[IAC, SB, GMCP]);
    bytes.extend_from_slice(
        br#"Room.Info {"name":"Eastern Meadow","zone":"{ 1   4} Crom    The Meadow","exits":{"n":"O","e":"O"}}"#,
    );
    bytes.extend_from_slice(&[IAC, SE]);
    engine.receive(Bytes::from(bytes), Timestamp(0)).unwrap();
    engine.settle(Timestamp(0));

    let room = hub.read(|room: &Versioned<RoomState>| room.value().room.clone());
    assert_eq!(
        room.expect("the room").description,
        "You stand hip deep in thick red wildflowers.  Small gnats and aphids spring"
    );
}

#[test]
fn the_session_suggests_avatar_by_its_host_and_fills_its_stores_when_on() {
    let folder = tempfile::tempdir().unwrap();
    let plugin = mudling_plugin_avatar::load();
    let session = Session::open(
        SessionConfig {
            enabled: vec![plugin.id().clone()],
            plugins: vec![plugin],
            ..SessionConfig::new(folder.path(), HOST, 4000)
        },
        || {},
    )
    .unwrap();

    let score = include_bytes!("fixtures/score.txt");
    session.replay(vec![
        Bytes::from_static(score),
        Bytes::from_static(b"\r\n"),
        Bytes::from_static(PROMPT),
    ]);
    let settings = |session: &Session| {
        session
            .stores()
            .read(|settings: &Versioned<SettingsState>| settings.value().clone())
    };
    let started = Instant::now();
    while settings(&session).connection
        != ConnectionStatus::Closed(DisconnectReason::ClosedByServer)
    {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the replay hung"
        );
        std::thread::sleep(Duration::from_millis(2));
    }

    let game = settings(&session)
        .suggestion
        .expect("the host names the game");
    assert_eq!(
        (game.plugin.as_str(), game.source),
        ("avatar", Source::Host)
    );
    let level = session
        .stores()
        .read(|character: &Versioned<CharactersState>| {
            character.value().0.get("character.level").cloned()
        });
    assert_eq!(level, Some(Value::Number(12.0)));
    session.quit().wait().unwrap();
}
