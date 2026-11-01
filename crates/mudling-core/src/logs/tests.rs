use super::*;
use crate::git;
use crate::persist::{Database, SERVERS_FOLDER};
use crate::replay::{Event, events};
use crate::repo::{AppSettings, Repo};
use crate::session::{Engine, SessionConfig};
use crate::store::StoreHub;
use crate::telnet::Bytes;
use std::sync::Arc;

const HOST: &str = "mud.example.org";
const IAC: u8 = 255;
const WILL: u8 = 251;
const WONT: u8 = 252;
const ECHO: u8 = 1;
const PASSWORD: &str = "hunter2";

fn engine(folder: &Path) -> Engine {
    let config = SessionConfig::new(folder, HOST, 4000);
    let hub = Arc::new(StoreHub::new(|| {}));
    Engine::open(config, hub, Arc::default()).unwrap()
}

fn turn_on(folder: &Path) {
    let mut settings = LogSettings {
        debug: true,
        ..LogSettings::default()
    };
    settings.set_chat(HOST, true);
    Logs::in_data_folder(folder)
        .save_settings(&settings)
        .unwrap();
}

fn receive(engine: &mut Engine, bytes: &[u8]) {
    let now = Timestamp::now();
    engine.receive(Bytes::copy_from_slice(bytes), now).unwrap();
    engine.settle(now);
}

fn gmcp(package_and_json: &str) -> Vec<u8> {
    let mut bytes = vec![IAC, WILL, 201, IAC, 250, 201];
    bytes.extend_from_slice(package_and_json.as_bytes());
    bytes.extend_from_slice(&[IAC, 240]);
    bytes
}

/// A login with a password, a server that echoes it back, then some play.
fn play_a_session(folder: &Path) {
    let mut engine = engine(folder);
    engine.start_connection().unwrap();
    engine.connected();
    receive(&mut engine, b"Hello there.\r\n");
    receive(&mut engine, b"Password: ");
    receive(&mut engine, &[IAC, WILL, ECHO]);
    engine.send(PASSWORD, Timestamp::now());
    receive(&mut engine, PASSWORD.as_bytes());
    receive(&mut engine, &gmcp("Secret.Value {\"said\": \"hunter2\"}"));
    receive(&mut engine, &[IAC, WONT, ECHO]);
    receive(&mut engine, b"\r\nWelcome back.\r\n");
    engine.send("look", Timestamp::now());
    receive(&mut engine, b"\x1b[32mA quiet glade.\x1b[0m\r\n");
    receive(&mut engine, &gmcp("Room.Info {\n\"num\": 1\n}"));
    engine.finish().unwrap();
}

fn log_of(folder: &Path, kind: Kind) -> String {
    let logs = Logs::in_data_folder(folder).list().unwrap();
    let log = logs.iter().find(|log| log.kind == kind).expect("a log");
    fs::read_to_string(&log.path).unwrap()
}

fn sample(logs: &Logs, kind: Kind, started: i64, bytes: usize) -> LogFile {
    let path = logs.new_path(kind, HOST, Timestamp(started));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, vec![b'x'; bytes]).unwrap();
    logs.of_kind(kind)
        .unwrap()
        .into_iter()
        .find(|log| log.path == path)
        .unwrap()
}

fn kept(logs: &Logs, kind: Kind) -> Vec<i64> {
    let kept = logs.of_kind(kind).unwrap();
    kept.iter().map(|log| log.started.0).collect()
}

#[test]
fn nothing_is_logged_until_the_player_says() {
    let folder = tempfile::tempdir().unwrap();
    play_a_session(folder.path());
    assert!(!folder.path().join(LOGS_FOLDER).exists());
}

#[test]
fn a_chat_log_is_for_the_servers_chosen_and_a_debug_log_for_all() {
    let folder = tempfile::tempdir().unwrap();
    let mut settings = LogSettings::default();
    settings.set_chat("other.example", true);
    let logs = Logs::in_data_folder(folder.path());
    logs.save_settings(&settings).unwrap();
    play_a_session(folder.path());
    assert!(logs.list().unwrap().is_empty());

    settings.debug = true;
    logs.save_settings(&settings).unwrap();
    play_a_session(folder.path());
    let kinds: Vec<Kind> = logs.list().unwrap().iter().map(|log| log.kind).collect();
    assert_eq!(kinds, [Kind::Debug]);
}

#[test]
fn neither_log_holds_input_or_anything_from_while_echo_is_off() {
    let folder = tempfile::tempdir().unwrap();
    turn_on(folder.path());
    play_a_session(folder.path());

    let debug = log_of(folder.path(), Kind::Debug);
    assert!(debug.contains("Welcome back."), "{debug}");
    assert!(!debug.contains(PASSWORD), "{debug}");
    assert!(!debug.contains("look"), "{debug}");

    let chat = log_of(folder.path(), Kind::Chat);
    assert!(chat.contains("Welcome back."), "{chat}");
    assert!(chat.contains("[sent] look"), "the player saw it: {chat}");
    assert!(!chat.contains(PASSWORD), "{chat}");
    assert!(
        !chat.contains("\x1b["),
        "what they saw has no codes: {chat}"
    );
}

#[test]
fn a_log_replays_what_the_server_sent() {
    let folder = tempfile::tempdir().unwrap();
    turn_on(folder.path());
    play_a_session(folder.path());

    let debug = log_of(folder.path(), Kind::Debug);
    assert_eq!(
        events(&debug),
        [
            Event::Connected,
            Event::Text(
                "Hello there.\r\nPassword: \r\nWelcome back.\r\n\x1b[32mA quiet glade.\x1b[0m\r\n"
                    .into()
            ),
            Event::Gmcp {
                package: "Room.Info".into(),
                json: "{ \"num\": 1 }".into()
            },
        ]
    );
    let chat = log_of(folder.path(), Kind::Chat);
    assert_eq!(
        events(&chat),
        [
            Event::Connected,
            Event::Text("Hello there.\nWelcome back.\n".into()),
            Event::Text("A quiet glade.\n".into()),
        ]
    );
}

#[test]
fn turning_a_log_off_ends_it_at_once_and_on_starts_it_mid_game() {
    let folder = tempfile::tempdir().unwrap();
    let logs = Logs::in_data_folder(folder.path());
    let mut engine = engine(folder.path());
    engine.start_connection().unwrap();
    engine.connected();
    receive(&mut engine, b"Before.\r\n");

    turn_on(folder.path());
    engine.logging_changed();
    receive(&mut engine, b"During.\r\n");

    logs.save_settings(&LogSettings::default()).unwrap();
    engine.logging_changed();
    receive(&mut engine, b"After.\r\n");
    engine.finish().unwrap();

    let debug = log_of(folder.path(), Kind::Debug);
    assert!(debug.contains("During."), "{debug}");
    assert!(
        !debug.contains("Before.") && !debug.contains("After."),
        "{debug}"
    );
}

#[test]
fn only_the_last_ten_stay_and_a_pin_keeps_one() {
    let folder = tempfile::tempdir().unwrap();
    let logs = Logs::in_data_folder(folder.path());
    let oldest = sample(&logs, Kind::Chat, 1, 1);
    logs.set_pinned(&oldest, true).unwrap();
    for started in 2..=13 {
        sample(&logs, Kind::Chat, started, 1);
    }
    logs.prune(Kind::Chat).unwrap();
    assert_eq!(
        kept(&logs, Kind::Chat),
        [13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 1]
    );

    logs.set_pinned(&oldest, false).unwrap();
    logs.prune(Kind::Chat).unwrap();
    assert_eq!(kept(&logs, Kind::Chat).last(), Some(&4));
}

#[test]
fn the_oldest_go_past_the_size_cap() {
    let folder = tempfile::tempdir().unwrap();
    let logs = Logs::in_data_folder(folder.path());
    let third = MOST_BYTES as usize / 3 + 1;
    for started in 1..=3 {
        sample(&logs, Kind::Debug, started, third);
    }
    sample(&logs, Kind::Chat, 1, 1);
    logs.prune(Kind::Debug).unwrap();
    assert_eq!(kept(&logs, Kind::Debug), [3, 2]);
    assert_eq!(kept(&logs, Kind::Chat), [1], "each kind has its own cap");
}

#[test]
fn clear_takes_a_kinds_logs_with_their_pins_but_not_the_one_being_written() {
    let folder = tempfile::tempdir().unwrap();
    let logs = Logs::in_data_folder(folder.path());
    let pinned = sample(&logs, Kind::Debug, 1, 1);
    logs.set_pinned(&pinned, true).unwrap();
    let writing = sample(&logs, Kind::Debug, 2, 1);
    live().insert(writing.path.clone());
    sample(&logs, Kind::Chat, 3, 1);

    logs.clear(Kind::Debug).unwrap();
    live().remove(&writing.path);

    assert_eq!(kept(&logs, Kind::Debug), [2]);
    assert!(!pin_of(&pinned.path).exists());
    assert_eq!(kept(&logs, Kind::Chat), [3]);
}

#[test]
fn an_export_goes_where_the_player_picked() {
    let folder = tempfile::tempdir().unwrap();
    let picked = tempfile::tempdir().unwrap();
    let logs = Logs::in_data_folder(folder.path());
    let log = sample(&logs, Kind::Debug, 1, 5);
    let to = picked.path().join("for-the-bug-report.log");
    logs.export(&log, &to).unwrap();
    assert_eq!(fs::read(to).unwrap(), b"xxxxx");
}

#[test]
fn settings_that_cant_be_read_mean_off() {
    let folder = tempfile::tempdir().unwrap();
    let logs = Logs::in_data_folder(folder.path());
    assert_eq!(logs.settings(), LogSettings::default());
    fs::create_dir_all(folder.path().join(LOGS_FOLDER)).unwrap();
    fs::write(folder.path().join(LOGS_FOLDER).join(SETTINGS_FILE), "{oops").unwrap();
    assert_eq!(logs.settings(), LogSettings::default());
}

#[test]
fn a_log_reads_its_start_in_utc() {
    let log = LogFile {
        kind: Kind::Debug,
        path: PathBuf::new(),
        server: HOST.to_owned(),
        // 2026-03-01 00:04:00 UTC, past a February.
        started: Timestamp(1_772_323_440_000),
        bytes: 0,
        pinned: false,
        live: false,
    };
    assert_eq!(log.started_text(), "2026-03-01 00:04");
}

/// The worst case: the player chose the data folder itself as their repo.
#[test]
fn no_log_reaches_the_repo_even_in_a_repo_over_the_data_folder() {
    let folder = tempfile::tempdir().unwrap();
    let git = git::Repo::open_or_init(folder.path()).unwrap();
    turn_on(folder.path());
    play_a_session(folder.path());
    let logs = Logs::in_data_folder(folder.path());
    let saved = logs.list().unwrap();
    assert_eq!(saved.len(), 2, "both logs are there to leak");
    logs.set_pinned(&saved[0], true).unwrap();

    let database = Database::open(&folder.path().join(SERVERS_FOLDER), HOST).unwrap();
    let repo = Repo::new(folder.path());
    let exported = repo
        .export(&AppSettings::default(), &[database.reader().unwrap()])
        .unwrap();
    let in_logs = |path: &&PathBuf| path.starts_with(LOGS_FOLDER);
    assert_eq!(exported.written.iter().filter(in_logs).count(), 0);

    let commit = git.commit_all("export").unwrap().expect("a commit");
    let committed = git.files_at(commit).unwrap();
    assert!(committed.contains_key(Path::new("servers.json")));
    let leaked: Vec<_> = committed
        .keys()
        .filter(|path| path.starts_with(LOGS_FOLDER))
        .collect();
    assert!(leaked.is_empty(), "{leaked:?}");
}
