use super::*;
use crate::map::ISLANDS;
use crate::persist::*;
use crate::repo::Error;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;
use tempfile::TempDir;

const T1: Timestamp = Timestamp(1_790_000_000_000);
const T2: Timestamp = Timestamp(1_790_000_060_000);
const SERVER: &str = "mud.example.org";
const MEADOW: &str = "The Meadow";
const NON_ASCII_ZONE: &str = "Grüner Wald 龍";
const HERO: &str = "Alda";

fn commit(database: &Database, batch: Batch) {
    let writer = database.writer().unwrap();
    writer.submit(batch).unwrap();
    writer.flush().unwrap();
}

fn open(folder: &TempDir) -> Database {
    Database::open(folder.path(), "world").unwrap()
}

fn server(id: &str) -> Server {
    Server {
        id: id.to_owned(),
        name: "Müd Wörld".to_owned(),
        host: "mud.example.org".to_owned(),
        port: 4000,
        settings: r#"{"encoding":"utf-8"}"#.to_owned(),
        updated: T1,
    }
}

fn character(id: &str, name: &str) -> Character {
    Character {
        id: id.to_owned(),
        server_id: SERVER.to_owned(),
        name: name.to_owned(),
        updated: T1,
    }
}

fn room(id: i64, name: &str, area: &str) -> Room {
    Room {
        id,
        name: name.to_owned(),
        area: area.to_owned(),
        environment: "field".to_owned(),
        description: "Tall grass sways here.".to_owned(),
        x: id,
        y: -id,
        z: 0,
        building: None,
        tangled: false,
        maze: false,
        updated: T1,
    }
}

/// One of every record type the repo keeps, with non-ASCII text throughout.
fn every_record() -> Batch {
    let mut batch = Batch::new();
    batch.upsert(server(SERVER));
    batch.upsert(character("c1", HERO));
    batch.upsert(character("c2", "Ñandú"));
    batch.upsert(Meta {
        key: ISLANDS.to_owned(),
        value: "3".to_owned(),
        updated: T1,
    });
    batch.upsert(Meta {
        key: NOTEPAD.to_owned(),
        value: "Buy rope — 綱\nAsk the smith.\n".to_owned(),
        updated: T1,
    });
    batch.upsert(room(1, "A quiet road", MEADOW));
    batch.upsert(Room {
        building: Some(1),
        tangled: true,
        maze: true,
        ..room(2, "Čajovna 茶室", NON_ASCII_ZONE)
    });
    batch.upsert(Exit {
        room_id: 1,
        direction: "north".to_owned(),
        state: "open".to_owned(),
        updated: T1,
    });
    batch.upsert(Link {
        room_id: 1,
        direction: "north".to_owned(),
        to_room: 2,
        updated: T1,
    });
    batch.upsert(Doorway {
        room_id: 2,
        direction: "süd".to_owned(),
        updated: T1,
    });
    batch.upsert(RoomNote {
        room_id: 2,
        text: "Der Wächter schläft.".to_owned(),
        updated: T1,
    });
    batch.upsert(RoomFeature {
        room_id: 1,
        feature: "water".to_owned(),
        updated: T1,
    });
    batch.upsert(Sighting {
        room_id: 1,
        mob: "ein Dachs".to_owned(),
        updated: T1,
    });
    batch.upsert(MobMark {
        name: "ein Dachs".to_owned(),
        mark: "aggressive".to_owned(),
        updated: T1,
    });
    batch.upsert(CharacterStat {
        character_id: "c1".to_owned(),
        key: "level".to_owned(),
        value: "12".to_owned(),
        updated: T1,
    });
    batch.upsert(Skill {
        character_id: "c1".to_owned(),
        name: "schwimmen".to_owned(),
        level: Some(3),
        learned: Some(78),
        cost: None,
        updated: T1,
    });
    batch.upsert(Affect {
        character_id: "c2".to_owned(),
        name: "bénédiction".to_owned(),
        detail: Some("12 hours".to_owned()),
        updated: T1,
    });
    batch.upsert(Worn {
        character_id: "c1".to_owned(),
        position: 0,
        slot: "head".to_owned(),
        item: "a straw hat".to_owned(),
        copy_id: Some("g1".to_owned()),
        updated: T1,
    });
    batch.upsert(GearSet {
        id: "s1".to_owned(),
        character_id: "c1".to_owned(),
        name: "Rüstung".to_owned(),
        container: Some("a sack".to_owned()),
        pieces: vec![GearPiece {
            item: "a straw hat".to_owned(),
            slot: Some("head".to_owned()),
            copy_id: Some("g1".to_owned()),
        }],
        updated: T1,
    });
    batch.upsert(GearCopy {
        id: "g1".to_owned(),
        name: "a straw hat".to_owned(),
        measured: Some(StatMap::from([("dex".to_owned(), 1)])),
        updated: T1,
    });
    batch.upsert(ItemFacts {
        name: "a straw hat".to_owned(),
        keywords: Some("straw hat".to_owned()),
        slot: Some("head".to_owned()),
        measured: None,
        updated: T1,
    });
    batch.upsert(Identification {
        id: "i1".to_owned(),
        item_name: "a straw hat".to_owned(),
        copy_id: Some("g1".to_owned()),
        keywords: "straw hat".to_owned(),
        item_type: "armor".to_owned(),
        flags: vec!["glowing".to_owned()],
        level: Some(4),
        quality: None,
        base: StatMap::from([("ac".to_owned(), -2)]),
        added: BTreeMap::from([("enchant".to_owned(), StatMap::from([("dex".to_owned(), 1)]))]),
        details: vec!["Weight: 1".to_owned()],
        updated: T1,
    });
    batch.upsert(Trigger {
        id: "t1".to_owned(),
        scope: "server".to_owned(),
        matches: "^Der Dachs".to_owned(),
        action: "flee".to_owned(),
        enabled: true,
        updated: T1,
    });
    batch.upsert(Alias {
        id: "a1".to_owned(),
        scope: "character:c1".to_owned(),
        name: "gö".to_owned(),
        expansion: "go north;look".to_owned(),
        updated: T1,
    });
    batch
}

fn settings() -> AppSettings {
    AppSettings {
        theme: "dark".to_owned(),
        font: "JetBrains Mono".to_owned(),
        shortcuts: BTreeMap::from([("map.toggle".to_owned(), "cmd+m".to_owned())]),
        panels: BTreeMap::from([("narrow".to_owned(), "over".to_owned())]),
    }
}

fn seeded(folder: &TempDir) -> Database {
    let database = open(folder);
    commit(&database, every_record());
    database
}

fn export(repo: &Repo, database: &Database) -> Exported {
    repo.export(&settings(), &[database.reader().unwrap()])
        .unwrap()
}

/// Every file under `root`, by path with `/` separators on every OS.
fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, folder: &Path, files: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(folder).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, files);
            } else {
                let relative: Vec<String> = path
                    .strip_prefix(root)
                    .unwrap()
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy().into_owned())
                    .collect();
                files.insert(relative.join("/"), fs::read(&path).unwrap());
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(root, root, &mut files);
    files
}

/// Imports the whole repo into new databases, and returns them.
fn import_into_fresh(repo: &Repo, folder: &TempDir) -> Vec<Database> {
    let mut databases = Vec::new();
    for (index, server) in repo.servers().unwrap().iter().enumerate() {
        let database = Database::open(folder.path(), &format!("world{index}")).unwrap();
        let imported = repo
            .import(&server.id, &database.reader().unwrap())
            .unwrap();
        commit(&database, imported.batch);
        databases.push(database);
    }
    databases
}

#[test]
fn export_writes_the_documented_layout() {
    let folder = TempDir::new().unwrap();
    let database = seeded(&folder);
    let root = TempDir::new().unwrap();

    export(&Repo::new(root.path()), &database);

    let paths: BTreeSet<String> = tree(root.path()).into_keys().collect();
    let hashed_zone = paths
        .iter()
        .find(|path| path.contains("Gr_ner Wald"))
        .expect("the non-ASCII zone has a file")
        .clone();
    let expected: BTreeSet<String> = [
        "mudling.json",
        "servers.json",
        "settings.json",
        "mud.example.org/map/The Meadow.json",
        hashed_zone.as_str(),
        "mud.example.org/mobs.json",
        "mud.example.org/characters/Alda.json",
        paths
            .iter()
            .find(|path| path.contains("characters/") && !path.ends_with("Alda.json"))
            .expect("the non-ASCII character has a file"),
        "mud.example.org/gear/items.json",
        "mud.example.org/gear/copies.json",
        "mud.example.org/triggers.json",
        "mud.example.org/aliases.json",
        "mud.example.org/notes.txt",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(paths, expected);

    let manifest = &tree(root.path())["mudling.json"];
    let manifest: serde_json::Value = serde_json::from_slice(manifest).unwrap();
    assert_eq!(manifest["format"], FORMAT_VERSION);
}

#[test]
fn a_second_export_writes_nothing_and_matches_byte_for_byte() {
    let folder = TempDir::new().unwrap();
    let database = seeded(&folder);
    let root = TempDir::new().unwrap();
    let repo = Repo::new(root.path());

    let first = export(&repo, &database);
    let before = tree(root.path());
    let second = export(&repo, &database);

    assert!(!first.written.is_empty());
    assert!(second.written.is_empty(), "{:?}", second.written);
    assert_eq!(second.unchanged, before.len());
    assert_eq!(tree(root.path()), before);

    let other_root = TempDir::new().unwrap();
    export(&Repo::new(other_root.path()), &database);
    assert_eq!(tree(other_root.path()), before);
}

#[test]
fn one_room_changed_is_one_file_and_a_few_lines() {
    let folder = TempDir::new().unwrap();
    let database = seeded(&folder);
    let root = TempDir::new().unwrap();
    let repo = Repo::new(root.path());
    export(&repo, &database);
    let before = tree(root.path());

    let mut batch = Batch::new();
    batch.upsert(Room {
        updated: T2,
        ..room(1, "A loud road", MEADOW)
    });
    commit(&database, batch);
    let exported = export(&repo, &database);

    let path = Path::new(SERVER).join("map").join("The Meadow.json");
    assert_eq!(exported.written, vec![path]);
    let after = tree(root.path());
    let key = "mud.example.org/map/The Meadow.json";
    let lines = |files: &BTreeMap<String, Vec<u8>>| -> Vec<String> {
        let text = String::from_utf8(files[key].clone()).unwrap();
        text.lines().map(str::to_owned).collect()
    };
    let (old, new) = (lines(&before), lines(&after));
    assert_eq!(old.len(), new.len());
    let changed = old.iter().zip(&new).filter(|(a, b)| a != b).count();
    assert_eq!(changed, 2, "the name and the time");
}

#[test]
fn a_clean_export_rewrites_no_file() {
    let folder = TempDir::new().unwrap();
    let database = seeded(&folder);
    let root = TempDir::new().unwrap();
    let repo = Repo::new(root.path());
    export(&repo, &database);

    let path = root.path().join("settings.json");
    let long_ago = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    let file = fs::File::options().write(true).open(&path).unwrap();
    file.set_modified(long_ago).unwrap();

    export(&repo, &database);

    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), long_ago);
}

#[test]
fn round_trip_gives_identical_files() {
    let folder = TempDir::new().unwrap();
    let database = seeded(&folder);
    let root = TempDir::new().unwrap();
    let repo = Repo::new(root.path());
    export(&repo, &database);

    let fresh_folder = TempDir::new().unwrap();
    let fresh = import_into_fresh(&repo, &fresh_folder);
    let readers: Vec<Reader> = fresh.iter().map(|db| db.reader().unwrap()).collect();
    let again = TempDir::new().unwrap();
    Repo::new(again.path())
        .export(&repo.settings().unwrap(), &readers)
        .unwrap();

    assert_eq!(tree(again.path()), tree(root.path()));

    // The rows themselves came back too, not only the files.
    let (old, new) = (database.reader().unwrap(), fresh[0].reader().unwrap());
    assert_eq!(new.servers().unwrap(), old.servers().unwrap());
    assert_eq!(new.map().unwrap(), old.map().unwrap());
    assert_eq!(new.characters().unwrap(), old.characters().unwrap());
    assert_eq!(new.gear().unwrap(), old.gear().unwrap());
    assert_eq!(new.mob_marks().unwrap(), old.mob_marks().unwrap());
    assert_eq!(new.triggers().unwrap(), old.triggers().unwrap());
    assert_eq!(new.aliases().unwrap(), old.aliases().unwrap());
    assert_eq!(repo.settings().unwrap(), settings());
}

#[test]
fn import_reads_only_what_differs_from_the_database() {
    let folder = TempDir::new().unwrap();
    let database = seeded(&folder);
    let root = TempDir::new().unwrap();
    let repo = Repo::new(root.path());
    export(&repo, &database);

    let nothing = repo.import(SERVER, &database.reader().unwrap()).unwrap();
    assert!(nothing.batch.is_empty());
    assert!(nothing.read.is_empty());

    // Another device changes one zone.
    let other_folder = TempDir::new().unwrap();
    let other = seeded(&other_folder);
    let mut batch = Batch::new();
    batch.upsert(Room {
        updated: T2,
        ..room(1, "A loud road", MEADOW)
    });
    commit(&other, batch);
    export(&repo, &other);

    let changed = repo.import(SERVER, &database.reader().unwrap()).unwrap();
    let path = Path::new(SERVER).join("map").join("The Meadow.json");
    assert_eq!(changed.read, vec![path]);
    commit(&database, changed.batch);

    let rooms = database.reader().unwrap().map().unwrap().rooms;
    assert_eq!(rooms[0].name, "A loud road");
    let settled = repo.import(SERVER, &database.reader().unwrap()).unwrap();
    assert!(settled.read.is_empty());
}

#[test]
fn import_ignores_files_the_format_does_not_name() {
    let folder = TempDir::new().unwrap();
    let database = seeded(&folder);
    let root = TempDir::new().unwrap();
    let repo = Repo::new(root.path());
    export(&repo, &database);
    fs::write(root.path().join(SERVER).join("map").join(".DS_Store"), [0]).unwrap();

    let imported = repo.import(SERVER, &database.reader().unwrap()).unwrap();

    assert!(imported.read.is_empty());
}

#[test]
fn any_name_round_trips_with_no_two_files_alike() {
    let hostile = [
        "CON",
        "con.txt",
        "a/b",
        "a\\b",
        "../../outside",
        "what?",
        "trailing.",
        " ",
        "",
        "Brightwater",
        "brightwater",
        "BRIGHTWATER",
        "Søren",
        "søren",
        "龍の谷",
        "😀",
    ];
    let folder = TempDir::new().unwrap();
    let database = open(&folder);
    let server_id = "../CON:mud?";
    let mut batch = Batch::new();
    batch.upsert(server(server_id));
    for (index, name) in hostile.iter().enumerate() {
        let id = index as i64 + 1;
        batch.upsert(room(id, name, name));
        batch.upsert(Character {
            server_id: server_id.to_owned(),
            ..character(&format!("c{id}"), name)
        });
    }
    // Two characters may share a name.
    batch.upsert(Character {
        server_id: server_id.to_owned(),
        ..character("twin", "Brightwater")
    });
    commit(&database, batch);
    let root = TempDir::new().unwrap();
    let repo = Repo::new(root.path());

    export(&repo, &database);

    let files = tree(root.path());
    let folded: BTreeSet<String> = files.keys().map(|path| path.to_lowercase()).collect();
    assert_eq!(folded.len(), files.len(), "{:?}", files.keys());
    let zones = files.keys().filter(|path| path.contains("/map/")).count();
    let characters = files
        .keys()
        .filter(|path| path.contains("/characters/"))
        .count();
    assert_eq!(zones, hostile.len());
    assert_eq!(characters, hostile.len() + 1);
    assert!(
        files
            .keys()
            .all(|path| path.is_ascii() && !path.contains("..") && !path.contains(':'))
    );

    let fresh_folder = TempDir::new().unwrap();
    let fresh = import_into_fresh(&repo, &fresh_folder);
    let again = TempDir::new().unwrap();
    export(&Repo::new(again.path()), &fresh[0]);
    assert_eq!(tree(again.path()), files);
}

#[test]
fn a_server_cannot_take_a_name_the_root_keeps() {
    let folder = TempDir::new().unwrap();
    let database = open(&folder);
    let mut batch = Batch::new();
    batch.upsert(server("plugins"));
    commit(&database, batch);
    let root = TempDir::new().unwrap();
    let plugin = root.path().join("plugins").join("mine.toml");
    fs::create_dir_all(plugin.parent().unwrap()).unwrap();
    fs::write(&plugin, "kept").unwrap();

    export(&Repo::new(root.path()), &database);

    let plugins = fs::read_dir(root.path().join("plugins")).unwrap().count();
    assert_eq!(plugins, 1);
}

#[test]
fn only_the_documented_files_reach_the_repo() {
    let folder = TempDir::new().unwrap();
    let database = seeded(&folder);
    let mut batch = Batch::new();
    batch.upsert(Room {
        name: "SENTINEL-OLD-MAP".to_owned(),
        ..room(9, "", MEADOW)
    });
    batch.save_map_version("manual", "SENTINEL-VERSION", T1);
    batch.delete(Key::Room(9));
    batch.upsert(Meta {
        key: "map.current-room".to_owned(),
        value: "SENTINEL-WHERE".to_owned(),
        updated: T1,
    });
    batch.upsert(Meta {
        key: "window.position".to_owned(),
        value: "SENTINEL-WINDOW".to_owned(),
        updated: T1,
    });
    batch.upsert(Meta {
        key: "commands.typed".to_owned(),
        value: "SENTINEL-TYPED".to_owned(),
        updated: T1,
    });
    commit(&database, batch);
    let root = TempDir::new().unwrap();

    export(&Repo::new(root.path()), &database);

    for (path, bytes) in tree(root.path()) {
        let name = path.rsplit('/').next().unwrap();
        let allowed = name.ends_with(".json") || name == "notes.txt";
        assert!(allowed, "{path}");
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains("SENTINEL"), "{path}");
    }
}

#[test]
fn exporting_one_server_keeps_the_others_listed() {
    let root = TempDir::new().unwrap();
    let repo = Repo::new(root.path());
    let (first_folder, second_folder) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (first, second) = (seeded(&first_folder), open(&second_folder));
    let mut batch = Batch::new();
    batch.upsert(server("other.example.org"));
    batch.upsert(room(1, "A far road", "The Far Hills"));
    commit(&second, batch);
    let readers = [first.reader().unwrap(), second.reader().unwrap()];
    repo.export(&settings(), &readers).unwrap();
    let before = tree(root.path());

    let exported = export(&repo, &first);

    assert!(exported.written.is_empty(), "{:?}", exported.written);
    assert_eq!(tree(root.path()), before);
    let fresh_folder = TempDir::new().unwrap();
    let fresh = Database::open(fresh_folder.path(), "fresh").unwrap();
    let imported = repo
        .import("other.example.org", &fresh.reader().unwrap())
        .unwrap();
    commit(&fresh, imported.batch);
    assert_eq!(fresh.reader().unwrap().map().unwrap().rooms.len(), 1);
}

#[test]
fn a_stray_json_file_in_a_folder_mudling_owns_is_removed() {
    let folder = TempDir::new().unwrap();
    let database = seeded(&folder);
    let root = TempDir::new().unwrap();
    let repo = Repo::new(root.path());
    export(&repo, &database);
    let stray = root.path().join(SERVER).join("map").join("custom.json");
    fs::write(&stray, "{}").unwrap();

    let exported = export(&repo, &database);

    assert_eq!(
        exported.removed,
        vec![Path::new(SERVER).join("map").join("custom.json")]
    );
    assert!(!stray.exists());
}

#[test]
fn secrets_plugins_and_foreign_files_are_left_alone() {
    let folder = TempDir::new().unwrap();
    let database = seeded(&folder);
    let root = TempDir::new().unwrap();
    let repo = Repo::new(root.path());
    export(&repo, &database);
    let vault = root.path().join("secrets.vault");
    let plugin = root.path().join("plugins").join("plugin.toml");
    let foreign = root.path().join(SERVER).join("map").join("README.md");
    fs::create_dir_all(plugin.parent().unwrap()).unwrap();
    for path in [&vault, &plugin, &foreign] {
        fs::write(path, b"mine").unwrap();
    }

    let exported = export(&repo, &database);

    assert!(exported.removed.is_empty());
    for path in [&vault, &plugin, &foreign] {
        assert_eq!(fs::read(path).unwrap(), b"mine");
    }
}

#[test]
fn a_zone_with_no_rooms_left_loses_its_file() {
    let folder = TempDir::new().unwrap();
    let database = seeded(&folder);
    let root = TempDir::new().unwrap();
    let repo = Repo::new(root.path());
    export(&repo, &database);

    let mut batch = Batch::new();
    batch.delete(Key::Room(1));
    commit(&database, batch);
    let exported = export(&repo, &database);

    let path = Path::new(SERVER).join("map").join("The Meadow.json");
    assert_eq!(exported.removed, vec![path.clone()]);
    assert!(!root.path().join(path).exists());
}

#[test]
fn what_a_crash_left_mid_write_is_swept_away() {
    let folder = TempDir::new().unwrap();
    let database = seeded(&folder);
    let root = TempDir::new().unwrap();
    let repo = Repo::new(root.path());
    export(&repo, &database);
    let gear = root.path().join(SERVER).join("gear");
    fs::write(gear.join("items.json.mudling-tmp"), b"half").unwrap();
    fs::write(root.path().join("settings.json.mudling-tmp"), b"half").unwrap();
    let before = tree(root.path()).len();

    export(&repo, &database);

    assert_eq!(tree(root.path()).len(), before - 2);
}

#[test]
fn every_file_has_lf_endings() {
    let folder = TempDir::new().unwrap();
    let database = seeded(&folder);
    let mut batch = Batch::new();
    batch.upsert(Meta {
        key: NOTEPAD.to_owned(),
        value: "one\r\ntwo".to_owned(),
        updated: T1,
    });
    batch.upsert(RoomNote {
        room_id: 1,
        text: "three\r\nfour".to_owned(),
        updated: T1,
    });
    commit(&database, batch);
    let root = TempDir::new().unwrap();

    export(&Repo::new(root.path()), &database);

    for (path, bytes) in tree(root.path()) {
        assert!(!bytes.contains(&b'\r'), "{path}");
        assert_eq!(bytes.last(), Some(&b'\n'), "{path}");
    }
}

#[test]
fn a_newer_format_is_refused() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("mudling.json"), r#"{"format": 999}"#).unwrap();

    let error = Repo::new(root.path()).servers().unwrap_err();

    assert!(matches!(error, Error::NewerFormat(999)));
}

#[test]
fn a_newer_format_is_not_exported_over() {
    let folder = TempDir::new().unwrap();
    let database = seeded(&folder);
    let root = TempDir::new().unwrap();
    let repo = Repo::new(root.path());
    fs::write(root.path().join("mudling.json"), r#"{"format": 999}"#).unwrap();
    let before = tree(root.path());

    let error = repo
        .export(&settings(), &[database.reader().unwrap()])
        .unwrap_err();

    assert!(matches!(error, Error::NewerFormat(999)));
    assert_eq!(tree(root.path()), before);
}

#[test]
fn a_server_named_like_a_temporary_file_does_not_block_the_write() {
    let folder = TempDir::new().unwrap();
    let database = open(&folder);
    let mut batch = Batch::new();
    batch.upsert(server("settings.json.mudling-tmp"));
    commit(&database, batch);
    let root = TempDir::new().unwrap();
    let repo = Repo::new(root.path());
    export(&repo, &database);

    let changed = AppSettings {
        theme: "light".to_owned(),
        ..settings()
    };
    let exported = repo
        .export(&changed, &[database.reader().unwrap()])
        .unwrap();

    assert_eq!(exported.written, vec![PathBuf::from("settings.json")]);
}

#[test]
fn a_broken_file_fails_the_import_and_names_itself() {
    let folder = TempDir::new().unwrap();
    let database = seeded(&folder);
    let root = TempDir::new().unwrap();
    let repo = Repo::new(root.path());
    export(&repo, &database);
    let path = root.path().join(SERVER).join("mobs.json");
    fs::write(&path, "{ not json").unwrap();

    let error = repo.import(SERVER, &database.reader().unwrap());

    assert!(matches!(error, Err(Error::Format { path, .. }) if path.ends_with("mobs.json")));
}

#[test]
fn a_database_without_its_server_row_is_refused() {
    let folder = TempDir::new().unwrap();
    let database = open(&folder);
    let root = TempDir::new().unwrap();

    let error = Repo::new(root.path())
        .export(&settings(), &[database.reader().unwrap()])
        .unwrap_err();

    assert!(matches!(error, Error::NotOneServer(0)));
}

#[test]
fn the_game_keeps_writing_while_the_sync_thread_exports() {
    let folder = TempDir::new().unwrap();
    let database = seeded(&folder);
    let writer = database.writer().unwrap();
    let first_id = 100;

    let last_id = std::thread::scope(|scope| {
        let sync = scope.spawn(|| {
            let root = TempDir::new().unwrap();
            let repo = Repo::new(root.path());
            for _ in 0..20 {
                export(&repo, &database);
            }
        });
        let mut id = first_id;
        while !sync.is_finished() {
            let mut batch = Batch::new();
            batch.upsert(room(id, "A new room", MEADOW));
            writer.submit(batch).unwrap();
            writer.flush().unwrap();
            id += 1;
        }
        sync.join().unwrap();
        id
    });

    assert!(last_id > first_id, "no write finished during the exports");
}

/// Run with
/// `cargo test --release -p mudling-core repo::tests::cost -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, not a check"]
fn cost_of_exporting_ten_thousand_rooms() {
    const ROOMS: i64 = 10_000;
    const ZONES: i64 = 40;
    let folder = TempDir::new().unwrap();
    let database = open(&folder);
    let mut batch = Batch::new();
    batch.upsert(server(SERVER));
    for id in 1..=ROOMS {
        let zone = format!("Zone {}", id % ZONES);
        batch.upsert(room(id, "A room with an ordinary name", &zone));
    }
    for id in 1..=ROOMS {
        for (direction, to_room) in [("north", id % ROOMS + 1), ("south", (id + 1) % ROOMS + 1)] {
            batch.upsert(Exit {
                room_id: id,
                direction: direction.to_owned(),
                state: "open".to_owned(),
                updated: T1,
            });
            batch.upsert(Link {
                room_id: id,
                direction: direction.to_owned(),
                to_room,
                updated: T1,
            });
        }
    }
    commit(&database, batch);

    let root = TempDir::new().unwrap();
    let repo = Repo::new(root.path());
    let start = Instant::now();
    let full = export(&repo, &database);
    let full_time = start.elapsed();
    let size: usize = tree(root.path()).values().map(Vec::len).sum();

    let mut times = Vec::new();
    for round in 0..20 {
        let mut batch = Batch::new();
        batch.upsert(Room {
            updated: Timestamp(T2.0 + round),
            ..room(
                round + 1,
                "A renamed room",
                &format!("Zone {}", (round + 1) % ZONES),
            )
        });
        commit(&database, batch);
        let start = Instant::now();
        let incremental = export(&repo, &database);
        times.push(start.elapsed());
        assert_eq!(incremental.written.len(), 1);
    }
    times.sort();

    let start = Instant::now();
    let clean = export(&repo, &database);
    let clean_time = start.elapsed();
    assert!(clean.written.is_empty());

    let fresh_folder = TempDir::new().unwrap();
    let fresh = open(&fresh_folder);
    let start = Instant::now();
    let imported = repo.import(SERVER, &fresh.reader().unwrap()).unwrap();
    let parse_time = start.elapsed();
    commit(&fresh, imported.batch);
    let import_time = start.elapsed();

    println!(
        "{ROOMS} rooms, {ZONES} zones, {} files, {:.1} MB",
        full.written.len(),
        size as f64 / 1e6
    );
    println!("full export: {full_time:?}");
    println!(
        "incremental, one room: median {:?}, worst {:?}",
        times[times.len() / 2],
        times[times.len() - 1]
    );
    println!("export with nothing changed: {clean_time:?}");
    println!("full import: {import_time:?} ({parse_time:?} to read and parse)");
}
