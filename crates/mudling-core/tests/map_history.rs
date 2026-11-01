//! Map history through a session: versions saved as the player plays, and a
//! restore, which the live map follows, or fails and leaves alone.

use mudling_core::map::VERSION_SAFETY;
use mudling_core::persist::{Batch, Database, MapVersion, Room, SERVERS_FOLDER, Timestamp};
use mudling_core::plugin::Plugin;
use mudling_core::session::{Engine, SessionConfig, Typing};
use mudling_core::store::{MapState, RoomState, SettingsState, StoreHub, Versioned};
use mudling_core::telnet::{Bytes, gmcp};
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

const HOST: &str = "mud.hearth.example";
const ROAD: i64 = 1;
const GATE: i64 = 2;
const HALL: i64 = 3;
const TEN_MINUTES: i64 = 10 * 60 * 1000;

struct Played {
    engine: Engine,
    hub: Arc<StoreHub>,
    folder: TempDir,
}

impl Played {
    fn new() -> Played {
        let folder = tempfile::tempdir().unwrap();
        Played::in_folder(folder)
    }

    fn in_folder(folder: TempDir) -> Played {
        let hearth =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugins/hearth");
        let hearth = Plugin::load(hearth).unwrap_or_else(|e| panic!("{e}"));
        let mut config = SessionConfig::new(folder.path(), HOST, 4000);
        config.enabled = vec![hearth.id().clone()];
        config.plugins = vec![hearth];
        let hub = Arc::new(StoreHub::new(|| {}));
        let typing = Arc::new(Mutex::new(Typing::default()));
        let mut engine = Engine::open(config, Arc::clone(&hub), typing).unwrap();
        engine.start_connection().unwrap();
        Played {
            engine,
            hub,
            folder,
        }
    }

    fn room(&mut self, number: i64, name: &str, now: i64) {
        let info =
            format!(r#"{{"num": {number}, "name": "{name}", "zone": "Vale", "exits": {{}}}}"#);
        let mut bytes = Vec::new();
        gmcp::encode("Room.Info", &info, &mut bytes);
        self.engine
            .receive(Bytes::from(bytes), Timestamp(now))
            .unwrap();
        self.engine.settle(Timestamp(now));
    }

    /// Walks in, then saves the map as it stands.
    fn saved_after(&mut self, rooms: &[(i64, &str)]) -> i64 {
        for (number, name) in rooms {
            self.room(*number, name, 0);
        }
        self.save_version()
    }

    fn save_version(&mut self) -> i64 {
        self.engine.flush();
        let database = self.database();
        let writer = database.writer().unwrap();
        writer
            .commit(mudling_core::map::LiveMap::save_version(
                VERSION_SAFETY,
                "A test's",
                Timestamp(1),
            ))
            .unwrap();
        self.versions()[0].id
    }

    fn database(&self) -> Database {
        Database::at(&self.folder.path().join(SERVERS_FOLDER), HOST)
    }

    /// Reads the file: what's saved, not what the session holds.
    fn versions(&self) -> Vec<MapVersion> {
        self.engine.flush();
        self.database().reader().unwrap().map_versions().unwrap()
    }

    fn saved_rooms(&self) -> Vec<i64> {
        self.engine.flush();
        let rows = self.database().reader().unwrap().map().unwrap();
        rows.rooms.iter().map(|room| room.id).collect()
    }

    fn shown_rooms(&self) -> Vec<i64> {
        self.hub
            .read(|map: &Versioned<MapState>| map.value().rooms().map(|room| room.id).collect())
    }

    fn settings(&self) -> SettingsState {
        self.hub
            .read(|settings: &Versioned<SettingsState>| settings.value().clone())
    }

    fn room_state(&self) -> RoomState {
        self.hub
            .read(|room: &Versioned<RoomState>| room.value().clone())
    }
}

#[test]
fn a_restore_takes_the_live_map_back_and_keeps_what_it_replaced() {
    let mut played = Played::new();
    let version = played.saved_after(&[(ROAD, "A road"), (GATE, "A gate")]);
    played.room(HALL, "A hall", 0);

    played.engine.restore_map(version, Timestamp(5));

    assert_eq!(played.shown_rooms(), [ROAD, GATE]);
    assert_eq!(played.saved_rooms(), [ROAD, GATE]);
    let restored = played.settings().map_restore.expect("it reported");
    assert_eq!(restored.outcome, Ok(version));

    let before = &played.versions()[0];
    assert_eq!(before.kind, VERSION_SAFETY);
    assert_eq!(before.room_count, 3);

    let undo = before.id;
    played.engine.restore_map(undo, Timestamp(6));
    assert_eq!(played.shown_rooms(), [ROAD, GATE, HALL]);
    assert_eq!(played.saved_rooms(), [ROAD, GATE, HALL]);
    assert_eq!(played.settings().map_restore.unwrap().attempt, 2);
}

#[test]
fn a_restore_leaves_the_player_where_they_are_if_the_version_has_the_room() {
    let mut played = Played::new();
    let version = played.saved_after(&[(ROAD, "A road"), (GATE, "A gate")]);
    played.room(ROAD, "A road", 0);

    played.engine.restore_map(version, Timestamp(5));

    let room = played.room_state();
    assert_eq!(room.whereabouts.room, Some(ROAD));
    assert!(!room.whereabouts.unsure);
}

#[test]
fn a_restore_without_the_players_room_puts_them_on_the_nearest_for_now() {
    let mut played = Played::new();
    let version = played.saved_after(&[(ROAD, "A road"), (GATE, "A gate")]);
    played.room(HALL, "A hall", 0);

    played.engine.restore_map(version, Timestamp(5));

    let room = played.room_state();
    assert!(matches!(room.whereabouts.room, Some(ROAD | GATE)));
    assert!(room.whereabouts.unsure, "the room says it's a guess");

    // The next room shown says where they are, and nothing links to the guess.
    played.room(HALL, "A hall", 10);
    let room = played.room_state();
    assert_eq!(room.whereabouts.room, Some(HALL));
    assert!(!room.whereabouts.unsure);
    let links_to_hall = played.hub.read(|map: &Versioned<MapState>| {
        map.value()
            .rooms()
            .filter(|room| room.id != HALL)
            .any(|room| room.links.iter().any(|(_, to)| *to == HALL))
    });
    assert!(!links_to_hall);
}

#[test]
fn a_failed_restore_says_so_and_leaves_the_map_as_it_was() {
    let mut played = Played::new();
    played.saved_after(&[(ROAD, "A road")]);
    played.room(GATE, "A gate", 0);
    let versions = played.versions().len();

    played.engine.restore_map(404, Timestamp(5));

    assert_eq!(played.shown_rooms(), [ROAD, GATE]);
    assert_eq!(played.saved_rooms(), [ROAD, GATE]);
    assert_eq!(played.room_state().whereabouts.room, Some(GATE));
    assert_eq!(played.versions().len(), versions, "nothing half saved");
    let settings = played.settings();
    assert!(settings.map_restore.unwrap().outcome.is_err());
    assert!(settings.notices.iter().any(|n| n.contains("restore")));
}

/// A version the database itself refuses, part way through writing it.
#[test]
fn a_restore_that_fails_mid_write_loses_no_rows() {
    let mut played = Played::new();
    let version = played.saved_after(&[(ROAD, "A road"), (GATE, "A gate")]);
    played.room(HALL, "A hall", 0);
    played.engine.flush();
    let file = played.database().path().to_owned();
    let connection = Connection::open(file).unwrap();
    connection
        .execute(
            "INSERT INTO version_exits VALUES (?1, 999, 'north', 'open', 0)",
            [version],
        )
        .unwrap();

    played.engine.restore_map(version, Timestamp(5));

    assert_eq!(played.shown_rooms(), [ROAD, GATE, HALL]);
    assert_eq!(played.saved_rooms(), [ROAD, GATE, HALL]);
    assert!(played.settings().map_restore.unwrap().outcome.is_err());
}

#[test]
fn a_reader_mid_restore_sees_one_map_or_the_other() {
    let mut played = Played::new();
    let mut batch = Batch::new();
    for id in 0..5_000 {
        batch.upsert(Room {
            id,
            name: "A plain".into(),
            area: "Vale".into(),
            environment: String::new(),
            description: String::new(),
            x: id,
            y: 0,
            z: 0,
            building: None,
            tangled: false,
            maze: false,
            updated: Timestamp(0),
        });
    }
    let database = played.database();
    let writer = database.writer().unwrap();
    writer.commit(batch).unwrap();
    let version = played.save_version();
    let mut clear = Batch::new();
    clear.delete(mudling_core::persist::Key::Room(0));
    writer.commit(clear).unwrap();

    let reader = database.reader().unwrap();
    let watching = std::thread::spawn(move || {
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..400 {
            seen.insert(reader.map().unwrap().rooms.len());
        }
        seen
    });
    let mut restore = Batch::new();
    restore.restore_map_version(version, Timestamp(5));
    writer.commit(restore).unwrap();

    let seen = watching.join().unwrap();
    assert!(
        seen.iter().all(|rooms| [4_999, 5_000].contains(rooms)),
        "{seen:?}"
    );
    assert_eq!(played.saved_rooms().len(), 5_000);
}

#[test]
fn versions_save_at_session_start_and_as_a_changed_map_is_played() {
    let mut played = Played::new();
    assert!(played.versions().is_empty(), "no map, no version");
    played.room(ROAD, "A road", 0);

    // Five minutes after the session opened, a changed map is saved.
    let later = played.engine_time() + TEN_MINUTES;
    played.engine.settle(Timestamp(later));
    let versions = played.versions();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].room_count, 1);

    // Nothing changed since, so nothing more is saved.
    played.engine.settle(Timestamp(later + TEN_MINUTES));
    assert_eq!(played.versions().len(), 1);

    let folder = played.finish();
    let reopened = Played::in_folder(folder);
    let versions = reopened.versions();
    assert_eq!(versions.len(), 2);
    assert!(
        versions
            .iter()
            .any(|version| version.kind == VERSION_SAFETY)
    );
}

#[test]
fn a_view_hears_of_a_version_only_once_it_can_be_read() {
    let mut played = Played::new();
    played.room(ROAD, "A road", 0);
    let heard = played.settings().map_versions;
    let later = played.engine_time() + TEN_MINUTES;
    played.engine.settle(Timestamp(later));
    played.versions();

    played.engine.settle(Timestamp(later));

    assert!(played.settings().map_versions > heard);
}

#[test]
fn the_oldest_version_kept_can_still_be_restored() {
    let mut played = Played::new();
    let oldest = played.saved_after(&[(ROAD, "A road")]);
    played.room(GATE, "A gate", 0);
    for _ in 0..39 {
        played.save_version();
    }
    assert_eq!(played.versions().len(), 40);
    assert_eq!(played.versions()[39].id, oldest);

    played.engine.restore_map(oldest, Timestamp(5));

    assert_eq!(played.settings().map_restore.unwrap().outcome, Ok(oldest));
    assert_eq!(played.shown_rooms(), [ROAD]);
    assert_eq!(played.saved_rooms(), [ROAD]);
    assert_eq!(played.versions().len(), 40);
}

#[test]
fn old_automatic_versions_go_and_their_rooms_with_them() {
    let mut played = Played::new();
    played.room(ROAD, "A road", 0);
    for _ in 0..45 {
        played.save_version();
    }
    assert_eq!(played.versions().len(), 40);
    let connection = Connection::open(played.database().path()).unwrap();
    let copies: i64 = connection
        .query_row("SELECT count(*) FROM version_rooms", [], |row| row.get(0))
        .unwrap();
    assert_eq!(copies, 40);
}

impl Played {
    fn engine_time(&self) -> i64 {
        Timestamp::now().0
    }

    fn finish(self) -> TempDir {
        self.engine.finish().unwrap();
        self.folder
    }
}
