use super::*;
use std::collections::BTreeMap;
use std::sync::Barrier;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const T1: Timestamp = Timestamp(1_790_000_000_000);
const T2: Timestamp = Timestamp(1_790_000_060_000);
const MEADOW: &str = "{ 1 10} Builder  The Meadow";

fn open() -> (TempDir, Database) {
    let folder = TempDir::new().unwrap();
    let database = Database::open(folder.path(), "mud.example.org").unwrap();
    (folder, database)
}

fn commit(database: &Database, batch: Batch) {
    let writer = database.writer().unwrap();
    writer.submit(batch).unwrap();
    writer.flush().unwrap();
}

fn room(id: i64, name: &str) -> Room {
    Room {
        id,
        name: name.to_owned(),
        area: MEADOW.to_owned(),
        environment: "field".to_owned(),
        description: "Tall grass sways here.".to_owned(),
        x: id,
        y: -id,
        z: 0,
        building: (id % 2 == 0).then_some(1),
        tangled: id % 3 == 0,
        maze: false,
        updated: T1,
    }
}

fn exit(room_id: i64, direction: &str, state: &str) -> Exit {
    Exit {
        room_id,
        direction: direction.to_owned(),
        state: state.to_owned(),
        updated: T1,
    }
}

fn link(room_id: i64, direction: &str, to_room: i64) -> Link {
    Link {
        room_id,
        direction: direction.to_owned(),
        to_room,
        updated: T1,
    }
}

fn doorway(room_id: i64, direction: &str) -> Doorway {
    Doorway {
        room_id,
        direction: direction.to_owned(),
        updated: T1,
    }
}

fn stats(pairs: &[(&str, i64)]) -> StatMap {
    pairs
        .iter()
        .map(|(key, value)| (key.to_string(), *value))
        .collect()
}

/// Two rooms linked both ways, with a note, a feature and a sighting. Room
/// 2 no longer lists west or up, but remembers a link west and a doorway up.
/// Rows are in the order the reader returns them.
fn small_map() -> MapRows {
    MapRows {
        rooms: vec![room(1, "A quiet road"), room(2, "A crossroads")],
        exits: vec![
            exit(1, "north", "open"),
            exit(2, "east", "closed"),
            exit(2, "south", "open"),
        ],
        links: vec![link(1, "north", 2), link(2, "south", 1), link(2, "west", 1)],
        doorways: vec![doorway(2, "east"), doorway(2, "up")],
        notes: vec![RoomNote {
            room_id: 1,
            text: "Ask the guard.".to_owned(),
            updated: T1,
        }],
        features: vec![RoomFeature {
            room_id: 2,
            feature: "water".to_owned(),
            updated: T1,
        }],
        sightings: vec![Sighting {
            room_id: 2,
            mob: "a grey wolf".to_owned(),
            updated: T1,
        }],
    }
}

fn small_map_batch() -> Batch {
    let map = small_map();
    let records = (map.rooms.into_iter().map(Record::from))
        .chain(map.exits.into_iter().map(Record::from))
        .chain(map.links.into_iter().map(Record::from))
        .chain(map.doorways.into_iter().map(Record::from))
        .chain(map.notes.into_iter().map(Record::from))
        .chain(map.features.into_iter().map(Record::from))
        .chain(map.sightings.into_iter().map(Record::from));
    let mut batch = Batch::new();
    for record in records {
        batch.upsert(record);
    }
    batch
}

#[test]
fn opening_an_empty_folder_migrates_to_the_latest_schema() {
    let (folder, database) = open();
    assert_eq!(database.schema_version().unwrap(), LATEST_VERSION);
    assert_eq!(
        database.path(),
        folder.path().join("mud.example.org.sqlite")
    );
    let journal: String = Connection::open(database.path())
        .unwrap()
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    assert_eq!(journal, "wal");

    // Opening again finds the schema current and changes nothing.
    commit(&database, small_map_batch());
    let again = Database::open(folder.path(), "mud.example.org").unwrap();
    assert_eq!(again.schema_version().unwrap(), LATEST_VERSION);
    assert_eq!(again.reader().unwrap().map().unwrap(), small_map());
}

#[test]
fn migrating_starts_from_the_version_found() {
    let folder = TempDir::new().unwrap();
    let mut connection = Connection::open(folder.path().join("db.sqlite")).unwrap();
    assert_eq!(schema::migrate(&mut connection).unwrap(), 0);
    assert_eq!(schema::migrate(&mut connection).unwrap(), LATEST_VERSION);
}

#[test]
fn connections_opening_a_new_file_at_once_migrate_it_once() {
    const OPENERS: usize = 8;
    for _ in 0..10 {
        let folder = TempDir::new().unwrap();
        let start = Barrier::new(OPENERS);
        std::thread::scope(|scope| {
            for _ in 0..OPENERS {
                scope.spawn(|| {
                    start.wait();
                    Database::open(folder.path(), "mud.example.org").unwrap()
                });
            }
        });
        let database = Database::open(folder.path(), "mud.example.org").unwrap();
        assert_eq!(database.schema_version().unwrap(), LATEST_VERSION);
    }
}

#[test]
fn server_names_must_be_file_names() {
    let folder = TempDir::new().unwrap();
    for bad in ["", "../escape", ".hidden", "a/b", "a\\b", "host:4000"] {
        assert!(
            matches!(
                Database::open(folder.path(), bad),
                Err(Error::BadServerName(_))
            ),
            "{bad:?}"
        );
    }
    assert!(Database::open(folder.path(), "mud.example.org").is_ok());
}

#[test]
fn every_record_type_round_trips() {
    let (_folder, database) = open();
    let server = Server {
        id: "srv-1".to_owned(),
        name: "Example".to_owned(),
        host: "mud.example.org".to_owned(),
        port: 4000,
        settings: r#"{"encoding":"latin1"}"#.to_owned(),
        updated: T1,
    };
    let character = Character {
        id: "chr-1".to_owned(),
        server_id: server.id.clone(),
        name: "Ada".to_owned(),
        updated: T1,
    };
    let meta = Meta {
        key: "current_room".to_owned(),
        value: "2".to_owned(),
        updated: T1,
    };
    let mark = MobMark {
        name: "wolf".to_owned(),
        mark: "aggressive".to_owned(),
        updated: T1,
    };
    let stat = CharacterStat {
        character_id: character.id.clone(),
        key: "level".to_owned(),
        value: "12".to_owned(),
        updated: T1,
    };
    let skill = Skill {
        character_id: character.id.clone(),
        name: "magic missile".to_owned(),
        level: Some(1),
        learned: Some(75),
        cost: Some(15),
        updated: T1,
    };
    let affect = Affect {
        character_id: character.id.clone(),
        name: "bless".to_owned(),
        detail: None,
        updated: T1,
    };
    let worn = Worn {
        character_id: character.id.clone(),
        position: 0,
        slot: "finger".to_owned(),
        item: "a pink ice ring".to_owned(),
        copy_id: Some("30989400863".to_owned()),
        updated: T1,
    };
    let gear_set = GearSet {
        id: "set-1".to_owned(),
        character_id: character.id.clone(),
        name: "Tank".to_owned(),
        container: Some("a backpack".to_owned()),
        pieces: vec![
            GearPiece {
                item: "a pink ice ring".to_owned(),
                slot: Some("finger".to_owned()),
                copy_id: None,
            },
            GearPiece {
                item: "a tower shield".to_owned(),
                slot: None,
                copy_id: Some("42".to_owned()),
            },
        ],
        updated: T1,
    };
    let copy = GearCopy {
        id: "30989400863".to_owned(),
        name: "a pink ice ring".to_owned(),
        measured: Some(stats(&[("dex", 1)])),
        updated: T1,
    };
    let facts = ItemFacts {
        name: "pink ice ring".to_owned(),
        keywords: Some("ring pink ice".to_owned()),
        slot: Some("finger".to_owned()),
        measured: None,
        updated: T1,
    };
    let identification = Identification {
        id: "ident-1".to_owned(),
        item_name: "pink ice ring".to_owned(),
        copy_id: Some(copy.id.clone()),
        keywords: "ring pink ice".to_owned(),
        item_type: "armor".to_owned(),
        flags: vec!["magic".to_owned(), "glow".to_owned()],
        level: Some(10),
        quality: Some("20 / 20".to_owned()),
        base: stats(&[("dex", 1), ("ac", 2)]),
        added: BTreeMap::from([("enchant armor".to_owned(), stats(&[("ac", -4)]))]),
        details: vec!["Weight 1, value 100.".to_owned()],
        updated: T1,
    };

    let mut batch = small_map_batch();
    batch
        .upsert(server.clone())
        .upsert(character.clone())
        .upsert(meta.clone())
        .upsert(mark.clone())
        .upsert(stat.clone())
        .upsert(skill.clone())
        .upsert(affect.clone())
        .upsert(worn.clone())
        .upsert(gear_set.clone())
        .upsert(copy.clone())
        .upsert(facts.clone())
        .upsert(identification.clone());
    commit(&database, batch);

    let reader = database.reader().unwrap();
    assert_eq!(reader.servers().unwrap(), vec![server]);
    assert_eq!(reader.meta().unwrap(), vec![meta]);
    assert_eq!(reader.mob_marks().unwrap(), vec![mark]);
    assert_eq!(reader.map().unwrap(), small_map());
    assert_eq!(
        reader.characters().unwrap(),
        CharacterRows {
            characters: vec![character],
            stats: vec![stat],
            skills: vec![skill],
            affects: vec![affect],
            worn: vec![worn],
            gear_sets: vec![gear_set],
        }
    );
    assert_eq!(
        reader.gear().unwrap(),
        GearRows {
            copies: vec![copy],
            facts: vec![facts],
            identifications: vec![identification]
        }
    );
}

#[test]
fn links_and_doorways_outlive_the_exits_listed() {
    let (_folder, database) = open();
    let writer = database.writer().unwrap();
    writer.submit(small_map_batch()).unwrap();

    // Room 2 stops listing east: its door and its links stay known.
    let mut batch = Batch::new();
    batch.delete(Key::Exit {
        room_id: 2,
        direction: "east".to_owned(),
    });
    writer.submit(batch).unwrap();
    writer.flush().unwrap();

    let map = database.reader().unwrap().map().unwrap();
    assert_eq!(
        map.exits,
        vec![exit(1, "north", "open"), exit(2, "south", "open")]
    );
    assert_eq!(map.links, small_map().links);
    assert_eq!(map.doorways, small_map().doorways);
}

#[test]
fn upserts_update_in_place() {
    let (_folder, database) = open();
    let writer = database.writer().unwrap();
    writer.submit(small_map_batch()).unwrap();

    let renamed = Room {
        name: "A muddy road".to_owned(),
        updated: T2,
        ..room(1, "")
    };
    let mut batch = Batch::new();
    batch.upsert(renamed.clone());
    writer.submit(batch).unwrap();
    writer.flush().unwrap();

    // Updating a room in place keeps its exits, links and note.
    let mut expected = small_map();
    expected.rooms[0] = renamed;
    assert_eq!(database.reader().unwrap().map().unwrap(), expected);
}

#[test]
fn deleting_a_room_takes_its_details_and_the_links_to_it() {
    let (_folder, database) = open();
    let mut batch = small_map_batch();
    batch.delete(Key::Room(2));
    commit(&database, batch);

    let map = database.reader().unwrap().map().unwrap();
    assert_eq!(map.rooms, vec![room(1, "A quiet road")]);
    assert_eq!(map.exits, vec![exit(1, "north", "open")]);
    assert!(map.links.is_empty() && map.doorways.is_empty());
    assert!(map.features.is_empty() && map.sightings.is_empty());
    assert_eq!(map.notes.len(), 1);
}

#[test]
fn a_failing_batch_writes_nothing_and_later_batches_still_commit() {
    let (_folder, database) = open();
    let writer = database.writer().unwrap();

    // The exit's room doesn't exist, which only shows at commit.
    let mut dangling = Batch::new();
    dangling
        .upsert(room(1, "A quiet road"))
        .upsert(exit(9, "north", "open"));
    writer.submit(dangling).unwrap();
    // Restoring a version that doesn't exist fails mid-batch.
    let mut missing = Batch::new();
    missing
        .upsert(room(2, "A crossroads"))
        .restore_map_version(404, T2);
    writer.submit(missing).unwrap();
    let mut good = Batch::new();
    good.upsert(room(3, "A bridge"));
    writer.submit(good).unwrap();

    match writer.flush() {
        Err(Error::BatchesFailed(failures)) => assert_eq!(failures.count(), 2),
        other => panic!("expected two failed batches, got {other:?}"),
    }
    let map = database.reader().unwrap().map().unwrap();
    assert_eq!(map.rooms, vec![room(3, "A bridge")]);
    assert!(map.exits.is_empty());
    // Failures are reported once.
    writer.flush().unwrap();
}

#[test]
fn failures_are_collected_without_waiting_and_capped() {
    let (_folder, database) = open();
    let writer = database.writer().unwrap();
    let failing = writer::KEPT_FAILURES + 3;
    for _ in 0..failing {
        let mut batch = Batch::new();
        batch.upsert(exit(9, "north", "open"));
        writer.submit(batch).unwrap();
    }
    let mut marker = Batch::new();
    marker.upsert(room(1, "A quiet road"));
    writer.submit(marker).unwrap();

    // Once the last batch is visible, every failure before it is recorded.
    let reader = database.reader().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while reader.map().unwrap().rooms.is_empty() {
        assert!(Instant::now() < deadline, "the writer never caught up");
        std::thread::sleep(Duration::from_millis(5));
    }
    let failures = writer.take_failures();
    assert_eq!(failures.errors.len(), writer::KEPT_FAILURES);
    assert_eq!(failures.dropped, 3);
    assert!(writer.take_failures().is_empty());
    writer.flush().unwrap();
}

#[test]
fn a_dropped_writer_commits_what_was_queued() {
    let (_folder, database) = open();
    let writer = database.writer().unwrap();
    for id in 1..=100 {
        let mut batch = Batch::new();
        batch.upsert(room(id, "A field"));
        writer.submit(batch).unwrap();
    }
    drop(writer);
    assert_eq!(database.reader().unwrap().map().unwrap().rooms.len(), 100);
}

#[test]
fn the_reader_sees_commits_while_the_writer_is_open() {
    let (_folder, database) = open();
    let writer = database.writer().unwrap();
    let reader = database.reader().unwrap();
    assert!(reader.map().unwrap().rooms.is_empty());
    writer.submit(small_map_batch()).unwrap();
    writer.flush().unwrap();
    assert_eq!(reader.map().unwrap().rooms.len(), 2);
}

#[test]
fn map_versions_save_list_and_restore() {
    let (_folder, database) = open();
    let writer = database.writer().unwrap();
    let mut batch = small_map_batch();
    batch.save_map_version("autosave", "Session start", T1);
    writer.submit(batch).unwrap();
    writer.flush().unwrap();

    // Change the map, then save it again.
    let mut batch = Batch::new();
    batch
        .delete(Key::Room(1))
        .upsert(Room {
            name: "A ruined crossroads".to_owned(),
            updated: T2,
            ..room(2, "")
        })
        .upsert(room(3, "A bridge"))
        .upsert(Room {
            area: "The Ford".to_owned(),
            ..room(4, "A ford")
        })
        .save_map_version("named", "Before the flood", T2);
    writer.submit(batch).unwrap();
    writer.flush().unwrap();

    let reader = database.reader().unwrap();
    let versions = reader.map_versions().unwrap();
    let listed: Vec<_> = versions
        .iter()
        .map(|v| (v.kind.as_str(), v.label.as_str(), v.room_count, v.created))
        .collect();
    assert_eq!(
        listed,
        vec![
            ("named", "Before the flood", 3, T2),
            ("autosave", "Session start", 2, T1)
        ]
    );
    assert_eq!(versions[0].areas, stats(&[(MEADOW, 2), ("The Ford", 1)]));
    assert_eq!(versions[1].areas, stats(&[(MEADOW, 2)]));
    let first = versions[1].id;
    assert_eq!(reader.map_version(first).unwrap(), small_map());

    let restored_at = Timestamp(T2.0 + 1);
    let mut batch = Batch::new();
    batch.restore_map_version(first, restored_at);
    writer.submit(batch).unwrap();
    writer.flush().unwrap();

    // The live map is the first version again, stamped with the restore's
    // time; sightings keep when the mob was seen.
    let mut expected = small_map();
    expected
        .rooms
        .iter_mut()
        .for_each(|r| r.updated = restored_at);
    expected
        .exits
        .iter_mut()
        .for_each(|e| e.updated = restored_at);
    expected
        .links
        .iter_mut()
        .for_each(|l| l.updated = restored_at);
    expected
        .doorways
        .iter_mut()
        .for_each(|d| d.updated = restored_at);
    expected
        .notes
        .iter_mut()
        .for_each(|n| n.updated = restored_at);
    expected
        .features
        .iter_mut()
        .for_each(|f| f.updated = restored_at);
    assert_eq!(reader.map().unwrap(), expected);

    // Deleting a version deletes its rows; the other stays.
    let mut batch = Batch::new();
    batch.delete(Key::MapVersion(first));
    writer.submit(batch).unwrap();
    writer.flush().unwrap();
    assert_eq!(reader.map_versions().unwrap().len(), 1);
    assert_eq!(reader.map_version(first).unwrap(), MapRows::default());
}

#[test]
fn an_empty_map_saves_a_version_with_no_areas() {
    let (_folder, database) = open();
    let mut batch = Batch::new();
    batch.save_map_version("safety", "Before reset", T1);
    commit(&database, batch);
    let versions = database.reader().unwrap().map_versions().unwrap();
    assert_eq!(versions[0].room_count, 0);
    assert!(versions[0].areas.is_empty());
}
