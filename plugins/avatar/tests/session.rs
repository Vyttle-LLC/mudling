//! Avatar through the whole session: picked at runtime by its host, with its
//! hooks' writes reaching the stores and the database.

use mudling_core::defaults::detect::Source;
use mudling_core::net::DisconnectReason;
use mudling_core::plugin::luau::Value;
use mudling_core::session::{Session, SessionConfig};
use mudling_core::store::{CharactersState, ConnectionStatus, SettingsState, Versioned};
use mudling_core::telnet::Bytes;
use std::time::{Duration, Instant};

const HOST: &str = "avatar.outland.org";
const PROMPT: &[u8] = b"(120h-130m-104v-1210)(-/-)";

#[test]
fn the_session_picks_avatar_by_its_host_and_fills_its_stores() {
    let folder = tempfile::tempdir().unwrap();
    let session = Session::open(
        SessionConfig {
            plugins: vec![mudling_plugin_avatar::load()],
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

    let game = settings(&session).game.expect("the host names the game");
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
