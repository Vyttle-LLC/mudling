//! Two devices over a bare repo in a temp folder: no network, and nothing
//! here can reach a real remote.

use super::*;
use crate::map::{LiveMap, MapRules, RoomIdentity};
use crate::persist::{
    Batch, Exit, GearCopy, Key, Link, Meta, NOTEPAD, Room, Server, Sighting, Timestamp, Tombstone,
};
use crate::repo::format::{ServerRows, tombstone_cutoff};
use git2::Repository;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

const SERVER: &str = "mud.example.org";
const REMOTE: &str = "origin";
const BRANCH: &str = "main";
const T1: Timestamp = Timestamp(1_790_000_000_000);
const T2: Timestamp = Timestamp(1_790_000_060_000);
const T3: Timestamp = Timestamp(1_790_000_120_000);

fn server(settings: &str) -> Server {
    Server {
        id: SERVER.to_owned(),
        name: "A MUD".to_owned(),
        host: SERVER.to_owned(),
        port: 4000,
        settings: settings.to_owned(),
        updated: T1,
    }
}

fn room(id: i64, name: &str, updated: Timestamp) -> Room {
    Room {
        id,
        name: name.to_owned(),
        area: "The Meadow".to_owned(),
        environment: "field".to_owned(),
        description: String::new(),
        x: id,
        y: 0,
        z: 0,
        building: None,
        tangled: false,
        maze: false,
        updated,
    }
}

fn exit(room_id: i64, updated: Timestamp) -> Exit {
    Exit {
        room_id,
        direction: "north".to_owned(),
        state: "open".to_owned(),
        updated,
    }
}

fn sighting(room_id: i64, mob: &str) -> Sighting {
    Sighting {
        room_id,
        mob: mob.to_owned(),
        updated: T2,
    }
}

fn copy(id: &str) -> GearCopy {
    GearCopy {
        id: id.to_owned(),
        name: "a straw hat".to_owned(),
        measured: None,
        updated: T2,
    }
}

struct Device {
    _data: TempDir,
    folder: PathBuf,
    database: Database,
    writer: Writer,
    git: git::Repo,
    repo: Repo,
}

impl Device {
    fn new(root: &Path, name: &str) -> Device {
        let data = TempDir::new().unwrap();
        let folder = root.join(name);
        let database = Database::open(data.path(), "world").unwrap();
        let writer = database.writer().unwrap();
        let git = git::Repo::open_or_init(&folder).unwrap();
        let remote = root.join("remote.git");
        Repository::open(&folder)
            .unwrap()
            .remote(REMOTE, remote.to_str().unwrap())
            .unwrap();
        Device {
            _data: data,
            repo: Repo::new(&folder),
            folder,
            database,
            writer,
            git,
        }
    }

    fn apply(&self, fill: impl FnOnce(&mut Batch)) {
        let mut batch = Batch::new();
        fill(&mut batch);
        self.writer.submit(batch).unwrap();
        self.writer.flush().unwrap();
    }

    fn stores(&self) -> [Store<'_>; 1] {
        [Store {
            server_id: SERVER,
            database: &self.database,
            writer: &self.writer,
        }]
    }

    fn commit_local(&self) {
        self.repo
            .export(&AppSettings::default(), &[self.database.reader().unwrap()])
            .unwrap();
        self.git.commit_all(CHECKPOINT).unwrap();
    }

    fn fetch(&self) -> Option<Oid> {
        self.git.fetch(REMOTE, BRANCH).unwrap()
    }

    /// One whole sync, as the sync thread will run it. A new device has
    /// nothing to export until its first pull.
    fn sync(&self) {
        if !self
            .database
            .reader()
            .unwrap()
            .servers()
            .unwrap()
            .is_empty()
        {
            self.commit_local();
        }
        if let Some(theirs) = self.fetch() {
            pull(&self.git, &self.repo, theirs, &self.stores()).unwrap();
        }
        self.git.push(REMOTE, BRANCH).unwrap();
    }

    fn rooms(&self) -> Vec<Room> {
        self.database.reader().unwrap().map().unwrap().rooms
    }

    fn room_names(&self) -> Vec<String> {
        self.rooms().into_iter().map(|room| room.name).collect()
    }
}

struct World {
    folder: TempDir,
    remote: PathBuf,
    mac: Device,
    ipad: Device,
}

/// Two devices that have synced one room and one exit.
fn two_devices() -> World {
    let folder = TempDir::new().unwrap();
    let remote = folder.path().join("remote.git");
    Repository::init_bare(&remote).unwrap();
    let mac = Device::new(folder.path(), "mac");
    let ipad = Device::new(folder.path(), "ipad");
    mac.apply(|batch| {
        batch.upsert(server("{}"));
        batch.upsert(room(1, "A quiet road", T1));
        batch.upsert(exit(1, T1));
    });
    mac.sync();
    ipad.sync();
    assert_eq!(ipad.room_names(), ["A quiet road"]);
    World {
        folder,
        remote,
        mac,
        ipad,
    }
}

/// Both devices sync until each has seen the other's changes.
fn settle(world: &World) {
    world.mac.sync();
    world.ipad.sync();
    world.mac.sync();
}

fn remote_tip(world: &World) -> Option<Oid> {
    Repository::open_bare(&world.remote)
        .unwrap()
        .refname_to_id("refs/heads/main")
        .ok()
}

/// A game that gives its rooms no numbers.
struct NoNumbers;

impl MapRules for NoNumbers {
    fn identity(&self) -> RoomIdentity {
        RoomIdentity::Text
    }
}

/// A step of a walk: how the player got there, then the room's name and exits.
type Step<'a> = (Option<&'a str>, &'a str, &'a [&'a str]);

impl Device {
    fn with_server(root: &Path, name: &str) -> Device {
        let device = Device::new(root, name);
        device.apply(|batch| {
            batch.upsert(server("{}"));
        });
        device
    }

    fn map(&self) -> LiveMap {
        let reader = self.database.reader().unwrap();
        LiveMap::load(
            Box::new(NoNumbers),
            &reader.map().unwrap(),
            &reader.mob_marks().unwrap(),
            &reader.meta().unwrap(),
        )
    }

    /// Plays a walk through the real map, as a session writes it.
    fn walk(&self, steps: &[Step<'_>]) {
        let mut map = self.map();
        for (direction, name, exits) in steps {
            let now = Timestamp::now();
            if let Some(direction) = direction {
                map.sent(direction, now);
            }
            let exits: Vec<String> = exits
                .iter()
                .map(|exit| format!("\"{exit}\": \"O\""))
                .collect();
            let info = format!(
                r#"{{"name": "{name}", "zone": "The Meadow", "environment": "field",
                    "exits": {{{}}}, "details": []}}"#,
                exits.join(", ")
            );
            map.room_info(&info, now).unwrap();
            if let Some(commit) = map.commit(now) {
                self.writer.submit(commit.batch).unwrap();
            }
        }
        self.writer.flush().unwrap();
    }

    fn sorted_rooms(&self) -> Vec<String> {
        let mut names = self.room_names();
        names.sort();
        names
    }

    /// Links by the names of the rooms they join.
    fn links(&self) -> Vec<(String, String)> {
        let map = self.database.reader().unwrap().map().unwrap();
        let name = |id: i64| {
            let room = map.rooms.iter().find(|room| room.id == id).unwrap();
            room.name.clone()
        };
        let mut links: Vec<(String, String)> = map
            .links
            .iter()
            .map(|link| (name(link.room_id), name(link.to_room)))
            .collect();
        links.sort();
        links
    }
}

fn two_explorers() -> (TempDir, Device, Device) {
    let folder = TempDir::new().unwrap();
    Repository::init_bare(folder.path().join("remote.git")).unwrap();
    let mac = Device::with_server(folder.path(), "mac");
    let ipad = Device::with_server(folder.path(), "ipad");
    (folder, mac, ipad)
}

/// Syncs until a round changes nothing, so a merge that keeps rewriting
/// itself fails here.
fn converge(mac: &Device, ipad: &Device) {
    for _ in 0..4 {
        mac.sync();
        ipad.sync();
    }
    let settled = (
        mac.git.branch_tip(BRANCH).unwrap(),
        ipad.git.branch_tip(BRANCH).unwrap(),
    );
    assert_eq!(settled.0, settled.1);
    mac.sync();
    ipad.sync();
    assert_eq!(mac.git.branch_tip(BRANCH).unwrap(), settled.0, "churn");
}

/// Two devices that share one room, "A hub", under one id.
fn two_explorers_at_a_hub(exits: &[&str]) -> (TempDir, Device, Device) {
    let (folder, mac, ipad) = two_explorers();
    mac.walk(&[(None, "A hub", exits)]);
    mac.sync();
    ipad.sync();
    assert_eq!(mac.rooms(), ipad.rooms());
    (folder, mac, ipad)
}

fn road_of(device: &Device) -> i64 {
    let rooms = device.rooms();
    let road = rooms.iter().find(|room| room.name == "A quiet road");
    road.unwrap().id
}

#[test]
fn devices_give_different_rooms_different_ids_and_keep_them_all() {
    let (_folder, mac, ipad) = two_explorers();
    mac.walk(&[
        (None, "The Mac's hill", &["e"]),
        (Some("e"), "A barn", &["w"]),
    ]);
    ipad.walk(&[
        (None, "The iPad's lake", &["e"]),
        (Some("e"), "A pier", &["w"]),
    ]);

    converge(&mac, &ipad);

    let names = ["A barn", "A pier", "The Mac's hill", "The iPad's lake"];
    assert_eq!(mac.sorted_rooms(), names);
    assert_eq!(ipad.sorted_rooms(), names);
    assert_eq!(mac.links(), ipad.links());
    assert_eq!(mac.links().len(), 4);
}

#[test]
fn a_mix_of_shared_and_new_rooms_keeps_every_room_and_way_between_them() {
    let hub: &[&str] = &["n", "e", "s"];
    let (_folder, mac, ipad) = two_explorers_at_a_hub(hub);
    let shared: [Step<'_>; 2] = [(None, "A hub", hub), (Some("s"), "A gate", &["n"])];
    mac.walk(&shared);
    ipad.walk(&shared);
    mac.walk(&[(None, "A hub", hub), (Some("n"), "The Mac's hill", &["s"])]);
    ipad.walk(&[(None, "A hub", hub), (Some("e"), "The iPad's lake", &["w"])]);

    converge(&mac, &ipad);

    let names = ["A gate", "A hub", "The Mac's hill", "The iPad's lake"];
    assert_eq!(mac.sorted_rooms(), names);
    assert_eq!(ipad.sorted_rooms(), names);
    assert_eq!(mac.links(), ipad.links());
    assert_eq!(mac.links().len(), 6);
}

#[test]
fn what_hung_off_the_retired_id_moves_to_the_room_that_stays() {
    let hub: &[&str] = &["n"];
    let (_folder, mac, ipad) = two_explorers_at_a_hub(hub);
    let walk: &[Step<'_>] = &[(None, "A hub", hub), (Some("n"), "A quiet road", &["s"])];
    mac.walk(walk);
    ipad.walk(walk);
    for (device, name) in [(&mac, "water"), (&ipad, "shop")] {
        let mut map = device.map();
        let id = road_of(device);
        map.set_feature(id, name, true);
        device.apply(|batch| {
            *batch = map.commit(Timestamp::now()).unwrap().batch;
            batch.upsert(sighting(id, name));
        });
    }

    converge(&mac, &ipad);

    for device in [&mac, &ipad] {
        let map = device.database.reader().unwrap().map().unwrap();
        assert_eq!(map.rooms.len(), 2);
        assert_eq!(map.exits.len(), 2);
        assert_eq!(map.features.len(), 2);
        assert_eq!(map.sightings.len(), 2);
        for sighting in &map.sightings {
            assert_eq!(sighting.room_id, road_of(device));
        }
    }
}

#[test]
fn a_device_that_played_on_keeps_what_it_added_to_a_room_since_retired() {
    let hub: &[&str] = &["n"];
    let (_folder, mac, ipad) = two_explorers_at_a_hub(hub);
    let walk: &[Step<'_>] = &[(None, "A hub", hub), (Some("n"), "A quiet road", &["s"])];
    mac.walk(walk);
    ipad.walk(walk);
    let (lower, higher) = if road_of(&mac) < road_of(&ipad) {
        (&mac, &ipad)
    } else {
        (&ipad, &mac)
    };
    higher.sync();
    lower.sync();
    let id = road_of(higher);
    higher.apply(|batch| {
        batch.upsert(sighting(id, "a heron"));
    });

    converge(&mac, &ipad);

    for device in [&mac, &ipad] {
        assert_eq!(device.rooms().len(), 2);
        let sightings = device.database.reader().unwrap().map().unwrap().sightings;
        assert_eq!(sightings.len(), 1, "the heron moved with its room");
    }
}

const CORRIDOR: &[&str] = &["n", "s"];

#[test]
fn two_corridors_that_read_the_same_stay_two_rooms_in_either_order() {
    for mac_syncs_first in [true, false] {
        let (_folder, mac, ipad) = two_explorers_at_a_hub(&["n", "s"]);
        mac.walk(&[
            (None, "A hub", &["n", "s"]),
            (Some("n"), "A corridor", CORRIDOR),
        ]);
        ipad.walk(&[
            (None, "A hub", &["n", "s"]),
            (Some("s"), "A corridor", CORRIDOR),
        ]);
        let hub_links = |device: &Device| device.links();

        if mac_syncs_first {
            converge(&mac, &ipad);
        } else {
            converge(&ipad, &mac);
        }

        assert_eq!(mac.rooms().len(), 3);
        assert_eq!(mac.rooms(), ipad.rooms());
        assert_eq!(hub_links(&mac).len(), 4, "each corridor, both ways");
        assert_eq!(hub_links(&mac), hub_links(&ipad));
    }
}

#[test]
fn reading_alike_with_links_that_agree_on_nothing_is_no_evidence() {
    // No rule forbids this union: the corridors' links go different ways, to
    // different rooms. Only the lack of evidence keeps them apart.
    let hall: &[&str] = &["e", "w"];
    for mac_syncs_first in [true, false] {
        let (_folder, mac, ipad) = two_explorers();
        mac.walk(&[
            (None, "A west room", &["e"]),
            (Some("e"), "A corridor", hall),
        ]);
        ipad.walk(&[
            (None, "An east room", &["w"]),
            (Some("w"), "A corridor", hall),
        ]);

        if mac_syncs_first {
            converge(&mac, &ipad);
        } else {
            converge(&ipad, &mac);
        }

        let names = ["A corridor", "A corridor", "A west room", "An east room"];
        assert_eq!(mac.sorted_rooms(), names);
        assert_eq!(mac.rooms(), ipad.rooms());
    }
}

#[test]
fn rooms_that_land_on_one_square_are_no_evidence_of_one_room() {
    // Each device counts squares from its own first room, so both corridors
    // here sit at (1, 0), and their links have nothing to disagree about.
    let hall: &[&str] = &["e", "w"];
    for mac_syncs_first in [true, false] {
        let (_folder, mac, ipad) = two_explorers();
        mac.walk(&[(None, "An inn", &["e"]), (Some("e"), "A corridor", hall)]);
        ipad.walk(&[
            (None, "A temple", &["n"]),
            (Some("n"), "A yard", &["s", "e"]),
            (Some("e"), "A lane", &["w", "e"]),
            (Some("e"), "A square", &["w", "s"]),
            (Some("s"), "A gate", &["n", "w"]),
            (Some("w"), "A corridor", hall),
        ]);

        if mac_syncs_first {
            converge(&mac, &ipad);
        } else {
            converge(&ipad, &mac);
        }

        assert_eq!(mac.rooms().len(), 8);
        assert_eq!(mac.rooms(), ipad.rooms());
    }
}

#[test]
fn devices_that_never_synced_keep_both_copies_of_a_room() {
    let walk: &[Step<'_>] = &[
        (None, "A quiet road", &["n"]),
        (Some("n"), "A hill", &["s"]),
    ];
    for mac_syncs_first in [true, false] {
        let (_folder, mac, ipad) = two_explorers();
        mac.walk(walk);
        ipad.walk(walk);

        if mac_syncs_first {
            converge(&mac, &ipad);
        } else {
            converge(&ipad, &mac);
        }

        let names = ["A hill", "A hill", "A quiet road", "A quiet road"];
        assert_eq!(mac.sorted_rooms(), names);
        assert_eq!(mac.rooms(), ipad.rooms());
        assert_eq!(mac.links().len(), 4);
    }
}

#[test]
fn a_new_room_off_a_shared_room_found_on_both_devices_unites() {
    let hub: &[&str] = &["n"];
    let (_folder, mac, ipad) = two_explorers_at_a_hub(hub);
    let walk: &[Step<'_>] = &[(None, "A hub", hub), (Some("n"), "A corridor", &["s"])];
    mac.walk(walk);
    ipad.walk(walk);

    converge(&mac, &ipad);

    assert_eq!(mac.sorted_rooms(), ["A corridor", "A hub"]);
    assert_eq!(mac.rooms(), ipad.rooms());
    assert_eq!(mac.links().len(), 2);
}

#[test]
fn a_chain_unites_from_one_anchor() {
    let hub: &[&str] = &["n"];
    let (_folder, mac, ipad) = two_explorers_at_a_hub(hub);
    let halls = ["The first hall", "The second hall", "The third hall"];
    let walk: &[Step<'_>] = &[
        (None, "A hub", hub),
        (Some("n"), halls[0], CORRIDOR),
        (Some("n"), halls[1], CORRIDOR),
        (Some("n"), halls[2], &["s"]),
    ];
    mac.walk(walk);
    ipad.walk(walk);

    converge(&mac, &ipad);

    assert_eq!(mac.rooms().len(), 4);
    assert_eq!(mac.rooms(), ipad.rooms());
    assert_eq!(mac.links().len(), 6);
}

#[test]
fn a_shared_link_with_links_that_disagree_stays_two_rooms() {
    let hub: &[&str] = &["n"];
    let (_folder, mac, ipad) = two_explorers_at_a_hub(hub);
    let corridor: Step<'_> = (Some("n"), "A corridor", CORRIDOR);
    mac.walk(&[
        (None, "A hub", hub),
        corridor,
        (Some("n"), "A hill", &["s"]),
    ]);
    ipad.walk(&[
        (None, "A hub", hub),
        corridor,
        (Some("n"), "A lake", &["s"]),
    ]);

    converge(&mac, &ipad);

    let names = ["A corridor", "A corridor", "A hill", "A hub", "A lake"];
    assert_eq!(mac.sorted_rooms(), names);
    assert_eq!(mac.rooms(), ipad.rooms());
}

fn rows_with(rooms: Vec<Room>, links: &[(i64, &str, i64)]) -> ServerRows {
    let mut rows = ServerRows::default();
    rows.map.rooms = rooms;
    rows.map.links = links
        .iter()
        .map(|(room_id, direction, to_room)| Link {
            room_id: *room_id,
            direction: (*direction).to_owned(),
            to_room: *to_room,
            updated: T1,
        })
        .collect();
    rows
}

const HUB: i64 = 1 << 41;
const OURS: i64 = (1 << 41) + 1;
const THEIRS: i64 = (1 << 41) + 2;

fn ids_after_merge(ours: ServerRows, theirs: ServerRows) -> Vec<i64> {
    let merged = records::merge_server(ours, theirs);
    merged.map.rooms.iter().map(|room| room.id).collect()
}

#[test]
fn a_union_never_makes_one_room_reach_another_two_ways() {
    // The way back south proves the rooms, but the hub leaves by two ways.
    let rooms = |id: i64| vec![room(HUB, "A hub", T1), room(id, "A maze", T1)];
    let ours = || rows_with(rooms(OURS), &[(HUB, "n", OURS), (OURS, "s", HUB)]);
    let theirs = || rows_with(rooms(THEIRS), &[(HUB, "e", THEIRS), (THEIRS, "s", HUB)]);

    assert_eq!(ids_after_merge(ours(), theirs()), [HUB, OURS, THEIRS]);
    assert_eq!(ids_after_merge(theirs(), ours()), [HUB, OURS, THEIRS]);
}

#[test]
fn a_pair_that_falls_takes_the_pair_that_leaned_on_it() {
    // The ends share the way south to the hub, but lead north to rooms that
    // stay apart. The halls have only the ends to prove them.
    let side = |hall: i64, far: &str| {
        let end = hall + 10;
        let far_id = hall + 20;
        rows_with(
            vec![
                room(HUB, "A hub", T1),
                room(hall, "A hall", T1),
                room(end, "An end", T1),
                room(far_id, far, T1),
            ],
            &[(end, "s", HUB), (end, "n", far_id), (hall, "e", end)],
        )
    };
    let ours = || side(OURS, "A hill");
    let theirs = || side(THEIRS, "A lake");

    assert_eq!(ids_after_merge(ours(), theirs()).len(), 7);
    assert_eq!(ids_after_merge(theirs(), ours()).len(), 7);
}

#[test]
fn look_alikes_all_stay_when_either_side_has_more_than_one() {
    let (_folder, mac, ipad) = two_explorers();
    let tunnel: &[&str] = &["e", "w"];
    mac.walk(&[
        (None, "A dark tunnel", tunnel),
        (Some("e"), "A dark tunnel", tunnel),
    ]);
    ipad.walk(&[(None, "A dark tunnel", tunnel)]);

    converge(&mac, &ipad);

    assert_eq!(mac.rooms().len(), 3);
    assert_eq!(ipad.rooms().len(), 3);
    assert_eq!(mac.rooms(), ipad.rooms());
}

#[test]
fn a_room_with_a_game_number_is_never_united() {
    let (_folder, mac, ipad) = two_explorers();
    mac.apply(|batch| {
        batch.upsert(room(1, "A quiet road", T2));
    });
    ipad.apply(|batch| {
        batch.upsert(room(2, "A quiet road", T2));
    });

    converge(&mac, &ipad);

    assert_eq!(mac.rooms().len(), 2);
}

#[test]
fn a_room_deleted_on_purpose_is_not_brought_back_by_a_union() {
    let (_folder, mac, ipad) = two_explorers();
    let walk: &[Step<'_>] = &[(None, "A quiet road", &["n"])];
    mac.walk(walk);
    ipad.walk(walk);
    mac.apply(|batch| {
        batch.delete(Key::Room(mac.rooms()[0].id));
    });

    converge(&mac, &ipad);

    assert_eq!(mac.sorted_rooms(), ipad.sorted_rooms());
    assert_eq!(mac.rooms().len(), 1, "the iPad's copy was never deleted");
}

#[test]
fn a_devices_id_prefix_stays_on_the_device_and_survives_a_merge() {
    let (_folder, mac, ipad) = two_explorers();
    mac.walk(&[(None, "The Mac's hill", &["e"])]);
    ipad.walk(&[(None, "The iPad's lake", &["e"])]);
    let prefix_of = |id: i64| id >> 28;
    let ours = prefix_of(mac.rooms()[0].id);
    assert_ne!(ours, prefix_of(ipad.rooms()[0].id));

    converge(&mac, &ipad);
    mac.walk(&[
        (None, "The Mac's hill", &["e"]),
        (Some("e"), "A barn", &["w"]),
    ]);

    let barn = mac.rooms().into_iter().find(|room| room.name == "A barn");
    assert_eq!(prefix_of(barn.unwrap().id), ours);
    assert!(mac.rooms().iter().all(|room| room.id < 1 << 53));
    let tip = mac.git.branch_tip(BRANCH).unwrap().unwrap();
    for bytes in mac.git.files_at(tip).unwrap().values() {
        assert!(!String::from_utf8_lossy(bytes).contains("map.id-prefix"));
    }
}

#[test]
fn undoing_a_restore_keeps_a_sighting_through_a_real_merge() {
    let world = two_devices();
    world.mac.apply(|batch| {
        batch.save_map_version("manual", "Before the heron", T1);
        batch.upsert(sighting(1, "a heron"));
        batch.save_map_version("manual", "With the heron", T2);
    });
    settle(&world);
    let versions = world.mac.database.reader().unwrap().map_versions().unwrap();
    let id_of = |label: &str| versions.iter().find(|v| v.label == label).unwrap().id;
    let (without, with) = (id_of("Before the heron"), id_of("With the heron"));

    world.mac.apply(|batch| {
        batch.restore_map_version(without, Timestamp::now());
    });
    world.mac.sync();
    world.ipad.sync();
    assert!(
        world
            .ipad
            .database
            .reader()
            .unwrap()
            .map()
            .unwrap()
            .sightings
            .is_empty()
    );
    world.mac.apply(|batch| {
        batch.restore_map_version(with, Timestamp::now());
    });
    world.ipad.apply(|batch| {
        batch.upsert(room(2, "iPad's lake", Timestamp::now()));
    });

    settle(&world);
    settle(&world);

    for device in [&world.mac, &world.ipad] {
        let map = device.database.reader().unwrap().map().unwrap();
        assert_eq!(map.sightings.len(), 1, "{:?}", map.rooms);
        assert_eq!(map.rooms.len(), 2);
    }
}

#[test]
fn undoing_a_restore_brings_back_a_room_with_its_sightings() {
    let world = two_devices();
    world.mac.apply(|batch| {
        batch.save_map_version("manual", "Before the hill", T1);
        batch.upsert(room(2, "A hill", T2));
        batch.upsert(sighting(2, "a heron"));
        batch.save_map_version("manual", "With the hill", T2);
    });
    world.mac.sync();
    let versions = world.mac.database.reader().unwrap().map_versions().unwrap();
    let id_of = |label: &str| versions.iter().find(|v| v.label == label).unwrap().id;
    let (without, with) = (id_of("Before the hill"), id_of("With the hill"));

    world.mac.apply(|batch| {
        batch.restore_map_version(without, Timestamp::now());
    });
    world.mac.apply(|batch| {
        batch.restore_map_version(with, Timestamp::now());
    });
    world.ipad.apply(|batch| {
        batch.upsert(room(3, "iPad's lake", T2));
    });

    settle(&world);

    for device in [&world.mac, &world.ipad] {
        let map = device.database.reader().unwrap().map().unwrap();
        assert_eq!(map.rooms.len(), 3);
        assert_eq!(map.sightings.len(), 1);
    }
}

#[test]
fn a_first_join_of_two_separate_maps_keeps_both_copies() {
    let folder = TempDir::new().unwrap();
    Repository::init_bare(folder.path().join("remote.git")).unwrap();
    let mac = Device::with_server(folder.path(), "mac");
    mac.walk(&[
        (None, "A quiet road", &["e"]),
        (Some("e"), "A hill", &["w"]),
    ]);
    mac.sync();

    let ipad = Device::with_server(folder.path(), "ipad");
    ipad.walk(&[(None, "A dark cave", &["n"])]);
    ipad.walk(&[(None, "A quiet road", &["e"])]);
    let theirs = ipad.fetch().unwrap();
    pull(&ipad.git, &ipad.repo, theirs, &[]).unwrap();
    let settings = AppSettings::default();
    let joined = join(&ipad.git, &ipad.repo, &settings, &ipad.stores()).unwrap();

    let names = ["A dark cave", "A hill", "A quiet road", "A quiet road"];
    assert_eq!(ipad.sorted_rooms(), names);
    assert!(joined.commit.is_some());

    ipad.git.push(REMOTE, BRANCH).unwrap();
    converge(&mac, &ipad);
    assert_eq!(mac.sorted_rooms(), names);
    assert_eq!(mac.rooms(), ipad.rooms());
}

#[test]
fn a_join_unites_a_room_found_off_a_shared_room() {
    let hub: &[&str] = &["n"];
    let (_folder, mac, ipad) = two_explorers_at_a_hub(hub);
    let walk: &[Step<'_>] = &[(None, "A hub", hub), (Some("n"), "A corridor", &["s"])];
    mac.walk(walk);
    mac.sync();
    ipad.walk(walk);

    let theirs = ipad.fetch().unwrap();
    pull(&ipad.git, &ipad.repo, theirs, &[]).unwrap();
    let settings = AppSettings::default();
    join(&ipad.git, &ipad.repo, &settings, &ipad.stores()).unwrap();

    assert_eq!(ipad.sorted_rooms(), ["A corridor", "A hub"]);
    ipad.git.push(REMOTE, BRANCH).unwrap();
    converge(&mac, &ipad);
    assert_eq!(mac.rooms(), ipad.rooms());
}

#[test]
fn both_edit_one_record_and_the_newer_wins_on_both_devices() {
    let world = two_devices();
    world.mac.apply(|batch| {
        batch.upsert(room(1, "Mac's road", T3));
    });
    world.ipad.apply(|batch| {
        batch.upsert(room(1, "iPad's road", T2));
    });

    settle(&world);

    assert_eq!(world.mac.room_names(), ["Mac's road"]);
    assert_eq!(world.ipad.room_names(), ["Mac's road"]);
    assert_eq!(
        world.mac.git.branch_tip(BRANCH).unwrap(),
        world.ipad.git.branch_tip(BRANCH).unwrap()
    );
}

#[test]
fn what_conflicts_as_text_merges_by_record() {
    // Both change one line of a zone's file and add a room next to it, which
    // git can't merge by line.
    let world = two_devices();
    world.mac.apply(|batch| {
        batch.upsert(room(1, "Mac's road", T2));
        batch.upsert(room(2, "Mac's hill", T2));
    });
    world.ipad.apply(|batch| {
        batch.upsert(room(1, "iPad's road", T3));
        batch.upsert(room(3, "iPad's lake", T2));
    });

    settle(&world);

    let names = ["iPad's road", "Mac's hill", "iPad's lake"];
    assert_eq!(world.mac.room_names(), names);
    assert_eq!(world.ipad.room_names(), names);
    for side in [&world.mac, &world.ipad] {
        let zone = fs::read_to_string(side.folder.join(SERVER).join("map").join("The Meadow.json"));
        assert!(!zone.unwrap().contains("<<<<"));
    }
}

#[test]
fn rooms_sightings_and_copies_from_both_sides_are_kept() {
    let world = two_devices();
    world.mac.apply(|batch| {
        batch.upsert(room(2, "Mac's hill", T2));
        batch.upsert(sighting(1, "a badger"));
        batch.upsert(copy("g1"));
    });
    world.ipad.apply(|batch| {
        batch.upsert(room(3, "iPad's lake", T2));
        batch.upsert(sighting(1, "a heron"));
        batch.upsert(copy("g2"));
    });

    settle(&world);

    for side in [&world.mac, &world.ipad] {
        let reader = side.database.reader().unwrap();
        let map = reader.map().unwrap();
        assert_eq!(map.rooms.len(), 3);
        let mobs: Vec<&str> = map.sightings.iter().map(|s| s.mob.as_str()).collect();
        assert_eq!(mobs, ["a badger", "a heron"]);
        assert_eq!(reader.gear().unwrap().copies.len(), 2);
    }
}

#[test]
fn a_delete_stays_deleted_against_an_older_edit() {
    let world = two_devices();
    world.ipad.apply(|batch| {
        batch.upsert(room(1, "iPad's road", T2));
    });
    world.mac.apply(|batch| {
        batch.delete(Key::Room(1));
    });

    settle(&world);

    assert!(world.mac.rooms().is_empty());
    assert!(world.ipad.rooms().is_empty());
    assert!(
        world
            .ipad
            .database
            .reader()
            .unwrap()
            .map()
            .unwrap()
            .exits
            .is_empty(),
        "the room's exit goes with it"
    );
}

#[test]
fn a_record_edited_after_its_delete_comes_back() {
    let world = two_devices();
    world.mac.apply(|batch| {
        batch.delete(Key::Room(1));
    });
    let after_the_delete = Timestamp(Timestamp::now().0 + 60_000);
    world.ipad.apply(|batch| {
        batch.upsert(room(1, "iPad's road", after_the_delete));
    });

    settle(&world);

    assert_eq!(world.mac.room_names(), ["iPad's road"]);
    assert_eq!(world.ipad.room_names(), ["iPad's road"]);
}

#[test]
fn a_room_edited_after_its_delete_settles_without_its_old_children() {
    let world = two_devices();
    world.mac.apply(|batch| {
        batch.delete(Key::Room(1));
    });
    let after_the_delete = Timestamp(Timestamp::now().0 + 60_000);
    world.ipad.apply(|batch| {
        batch.upsert(room(1, "iPad's road", after_the_delete));
    });

    settle(&world);
    let settled = remote_tip(&world);
    settle(&world);

    for side in [&world.mac, &world.ipad] {
        let map = side.database.reader().unwrap().map().unwrap();
        assert_eq!(map.rooms.len(), 1);
        assert!(map.exits.is_empty());
    }
    assert_eq!(remote_tip(&world), settled, "nothing left to sync");
}

#[test]
fn files_with_identical_bytes_are_all_kept() {
    let world = two_devices();
    let license = |plugin: &str| Path::new("plugins").join(plugin).join("LICENSE");
    for plugin in ["one", "two"] {
        let path = world.mac.folder.join(license(plugin));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "MIT").unwrap();
    }
    world.mac.sync();

    // A device with no history of its own, then a real two-sided merge.
    let fresh = Device::new(world.folder.path(), "fresh");
    let theirs = fresh.fetch().unwrap();
    pull(&fresh.git, &fresh.repo, theirs, &[]).unwrap();
    world.ipad.apply(|batch| {
        batch.upsert(room(2, "iPad's hill", T2));
    });
    world.mac.apply(|batch| {
        batch.upsert(room(3, "Mac's lake", T2));
    });
    settle(&world);

    for device in [&fresh, &world.ipad, &world.mac] {
        for plugin in ["one", "two"] {
            assert!(device.folder.join(license(plugin)).exists(), "{plugin}");
        }
    }
    let tip = world.ipad.git.branch_tip(BRANCH).unwrap().unwrap();
    let files = world.ipad.git.files_at(tip).unwrap();
    assert!(files.contains_key(&license("one")));
    assert!(files.contains_key(&license("two")));
}

#[test]
fn a_password_is_judged_at_every_path_that_holds_its_bytes() {
    let world = two_devices();
    let notes = "guild password: mellon\n";
    world.mac.apply(|batch| {
        batch.upsert(Meta {
            key: NOTEPAD.to_owned(),
            value: notes.to_owned(),
            updated: T2,
        });
    });
    world.mac.commit_local();
    // Only notes are read for a bare `password:`, and this sorts before them.
    fs::write(world.mac.folder.join("0-backup.txt"), notes).unwrap();
    world.mac.git.commit_all("a backup").unwrap();
    let pushed = remote_tip(&world);

    let error = world.mac.git.push(REMOTE, BRANCH).unwrap_err();

    assert!(
        matches!(error, GitError::PasswordInFile { .. }),
        "{error:?}"
    );
    assert_eq!(remote_tip(&world), pushed);
}

#[test]
fn a_room_added_back_does_not_revive_the_children_deleted_with_it() {
    let world = two_devices();
    world.mac.apply(|batch| {
        batch.delete(Key::Room(1));
    });
    let later = Timestamp(Timestamp::now().0 + 60_000);
    world.mac.apply(|batch| {
        batch.upsert(room(1, "A quiet road", later));
    });

    settle(&world);

    for side in [&world.mac, &world.ipad] {
        let map = side.database.reader().unwrap().map().unwrap();
        assert_eq!(map.rooms.len(), 1);
        assert!(map.exits.is_empty());
    }
}

#[test]
fn a_child_newer_than_its_rooms_delete_has_nothing_to_hang_off() {
    let world = two_devices();
    let later = Timestamp(Timestamp::now().0 + 60_000);
    world.ipad.apply(|batch| {
        batch.upsert(exit(1, later));
    });
    world.mac.apply(|batch| {
        batch.delete(Key::Room(1));
    });

    settle(&world);

    for side in [&world.mac, &world.ipad] {
        let map = side.database.reader().unwrap().map().unwrap();
        assert!(map.rooms.is_empty());
        assert!(map.exits.is_empty());
    }
}

#[test]
fn a_tombstone_reaches_the_repo_and_the_other_device() {
    let world = two_devices();
    world.mac.apply(|batch| {
        batch.delete(Key::Room(1));
    });
    settle(&world);

    let tombstones = world.ipad.database.reader().unwrap().tombstones().unwrap();

    assert_eq!(tombstones.len(), 1);
    assert_eq!(tombstones[0].key, Key::Room(1));
    assert!(
        world
            .mac
            .folder
            .join(SERVER)
            .join("tombstones.json")
            .exists()
    );
}

#[test]
fn a_tombstone_past_its_life_leaves_the_repo_and_then_the_database() {
    let world = two_devices();
    let expired = Timestamp(tombstone_cutoff().0 - 1);
    world.mac.apply(|batch| {
        batch.bury(Tombstone {
            key: Key::Room(9),
            deleted: expired,
        });
    });

    world.mac.sync();
    world.mac.writer.flush().unwrap();

    let file = world.mac.folder.join(SERVER).join("tombstones.json");
    assert!(!fs::read_to_string(file).unwrap().contains("\"room\""));
    assert!(
        world
            .mac
            .database
            .reader()
            .unwrap()
            .tombstones()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_kill_at_any_step_of_a_merge_leaves_both_sides_whole() {
    /// How far the merge got before the process died.
    #[derive(Clone, Copy)]
    enum Killed {
        AfterTheCommit,
        HalfwayThroughTheFiles,
        AfterTheFiles,
        AfterTheImport,
        AfterTheBranch,
    }

    for killed in [
        Killed::AfterTheCommit,
        Killed::HalfwayThroughTheFiles,
        Killed::AfterTheFiles,
        Killed::AfterTheImport,
        Killed::AfterTheBranch,
    ] {
        let world = two_devices();
        world.mac.apply(|batch| {
            batch.upsert(room(1, "Mac's road", T3));
            batch.upsert(room(2, "Mac's hill", T2));
        });
        world.mac.sync();
        let pushed = remote_tip(&world);
        world.ipad.apply(|batch| {
            batch.upsert(room(3, "iPad's lake", T2));
        });
        world.ipad.commit_local();
        let theirs = world.ipad.fetch().unwrap();

        let device = &world.ipad;
        let merge = prepare(&device.git, &device.repo, theirs).unwrap().unwrap();
        match killed {
            Killed::AfterTheCommit => {}
            Killed::HalfwayThroughTheFiles => {
                let half: git::Files = merge
                    .files
                    .iter()
                    .take(merge.files.len() / 2)
                    .map(|(path, bytes)| (path.clone(), bytes.clone()))
                    .collect();
                device.repo.replace_files(&half, &[]).unwrap();
            }
            _ => device
                .repo
                .replace_files(&merge.files, &merge.gone)
                .unwrap(),
        }
        if matches!(killed, Killed::AfterTheImport | Killed::AfterTheBranch) {
            import(&device.repo, &device.stores()).unwrap();
        }
        if matches!(killed, Killed::AfterTheBranch) {
            device
                .git
                .move_branch(&merge.branch, merge.ours, merge.commit)
                .unwrap();
        }

        // The remote never saw any of this, and the iPad kept its own room.
        assert_eq!(remote_tip(&world), pushed);
        assert!(device.room_names().contains(&"iPad's lake".to_owned()));

        settle(&world);

        let names = ["Mac's road", "Mac's hill", "iPad's lake"];
        assert_eq!(world.ipad.room_names(), names);
        assert_eq!(world.mac.room_names(), names);
    }
}

#[test]
fn the_merge_stops_before_it_writes_over_an_unsaved_vault() {
    let world = two_devices();
    world.mac.apply(|batch| {
        batch.upsert(room(2, "Mac's hill", T2));
    });
    world.mac.sync();
    let vault_path = world.ipad.folder.join(vault::FILE_NAME);
    fs::write(&vault_path, "saved just now").unwrap();
    let theirs = world.ipad.fetch().unwrap();

    let error = pull(
        &world.ipad.git,
        &world.ipad.repo,
        theirs,
        &world.ipad.stores(),
    )
    .unwrap_err();

    assert!(matches!(error, Error::VaultNotCommitted));
    assert_eq!(fs::read(vault_path).unwrap(), b"saved just now");
    assert_eq!(world.ipad.rooms().len(), 1);
}

#[test]
fn two_devices_that_each_started_a_repo_merge_by_record() {
    let folder = TempDir::new().unwrap();
    Repository::init_bare(folder.path().join("remote.git")).unwrap();
    let mac = Device::new(folder.path(), "mac");
    let ipad = Device::new(folder.path(), "ipad");
    mac.apply(|batch| {
        batch.upsert(server("{}"));
        batch.upsert(room(1, "Mac's road", T2));
    });
    ipad.apply(|batch| {
        batch.upsert(server("{}"));
        batch.upsert(room(1, "iPad's road", T3));
        batch.upsert(room(2, "iPad's lake", T2));
    });

    mac.sync();
    ipad.sync();
    mac.sync();

    assert_eq!(mac.room_names(), ["iPad's road", "iPad's lake"]);
    assert_eq!(ipad.room_names(), ["iPad's road", "iPad's lake"]);
}

#[test]
fn setting_a_repo_that_holds_data_merges_by_record_and_deletes_nothing() {
    let world = two_devices();
    world.mac.apply(|batch| {
        batch.upsert(room(1, "Mac's road", T2));
        batch.upsert(room(2, "Mac's hill", T2));
    });
    world.mac.sync();

    // The iPad holds its own data, and a clone of the repo is set as its repo.
    let ipad = Device::new(world.folder.path(), "tablet");
    let theirs = ipad.fetch().unwrap();
    pull(&ipad.git, &ipad.repo, theirs, &[]).unwrap();
    let files_before: Vec<PathBuf> = ipad.git.files_at(theirs).unwrap().into_keys().collect();
    ipad.apply(|batch| {
        batch.upsert(server("{}"));
        batch.upsert(room(1, "An old road", T1));
        batch.upsert(room(3, "iPad's lake", T2));
    });

    let joined = join(
        &ipad.git,
        &ipad.repo,
        &AppSettings::default(),
        &ipad.stores(),
    )
    .unwrap();

    assert_eq!(
        ipad.room_names(),
        ["Mac's road", "Mac's hill", "iPad's lake"]
    );
    let commit = joined.commit.unwrap();
    let files = ipad.git.files_at(commit).unwrap();
    for path in files_before {
        assert!(files.contains_key(&path), "{}", path.display());
    }
    let zone: PathBuf = [SERVER, "map", "The Meadow.json"].iter().collect();
    let zone = String::from_utf8(files[&zone].clone()).unwrap();
    for name in ["Mac's road", "Mac's hill", "iPad's lake"] {
        assert!(zone.contains(name), "{name}");
    }

    // Run again after a kill, it finds nothing left to do.
    let again = join(&ipad.git, &ipad.repo, &joined.settings, &ipad.stores()).unwrap();
    assert_eq!(again.commit, None);
}

#[test]
fn setting_an_empty_repo_makes_the_local_data_the_first_commit() {
    let folder = TempDir::new().unwrap();
    let device = Device::new(folder.path(), "mac");
    device.apply(|batch| {
        batch.upsert(server("{}"));
        batch.upsert(room(1, "A quiet road", T1));
    });

    let joined = join(
        &device.git,
        &device.repo,
        &AppSettings::default(),
        &device.stores(),
    )
    .unwrap();

    let files = device.git.files_at(joined.commit.unwrap()).unwrap();
    assert!(files.contains_key(Path::new("servers.json")));
}

#[test]
fn the_newer_settings_win_as_one_record() {
    let world = two_devices();
    let settings = |theme: &str, updated| AppSettings {
        theme: theme.to_owned(),
        updated,
        ..AppSettings::default()
    };
    let readers = |device: &Device| [device.database.reader().unwrap()];
    world
        .mac
        .repo
        .export(&settings("dark", T3), &readers(&world.mac))
        .unwrap();
    world.mac.git.commit_all("settings").unwrap();
    world.mac.git.push(REMOTE, BRANCH).unwrap();
    world
        .ipad
        .repo
        .export(&settings("light", T2), &readers(&world.ipad))
        .unwrap();
    world.ipad.git.commit_all("settings").unwrap();

    let theirs = world.ipad.fetch().unwrap();
    pull(
        &world.ipad.git,
        &world.ipad.repo,
        theirs,
        &world.ipad.stores(),
    )
    .unwrap();

    assert_eq!(world.ipad.repo.settings().unwrap().theme, "dark");
}

#[test]
fn the_newer_notes_win_with_the_time_they_were_written() {
    let world = two_devices();
    let note = |text: &str, updated| Meta {
        key: NOTEPAD.to_owned(),
        value: text.to_owned(),
        updated,
    };
    world.mac.apply(|batch| {
        batch.upsert(note("Mac's notes\n", T3));
    });
    world.ipad.apply(|batch| {
        batch.upsert(note("iPad's notes\n", T2));
    });

    settle(&world);

    for side in [&world.mac, &world.ipad] {
        let meta = side.database.reader().unwrap().meta().unwrap();
        let notes = meta.iter().find(|meta| meta.key == NOTEPAD).unwrap();
        assert_eq!((notes.value.as_str(), notes.updated), ("Mac's notes\n", T3));
    }
}

#[test]
fn a_file_with_no_records_merges_by_file() {
    let world = two_devices();
    let plugin = Path::new("plugins").join("mine.toml");
    let write = |device: &Device, name: &str, text: &str| {
        let path = device.folder.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    };
    write(&world.mac, plugin.to_str().unwrap(), "mac");
    world.mac.sync();
    world.ipad.sync();
    write(&world.ipad, "ipad-only.txt", "kept");
    write(&world.mac, plugin.to_str().unwrap(), "mac again");

    settle(&world);

    let read = |name: &str| fs::read_to_string(world.ipad.folder.join(name)).unwrap();
    assert_eq!(read(plugin.to_str().unwrap()), "mac again");
    assert_eq!(
        fs::read_to_string(world.mac.folder.join("ipad-only.txt")).unwrap(),
        "kept"
    );
}

#[test]
fn a_repo_in_a_newer_format_is_not_merged() {
    let world = two_devices();
    fs::write(world.mac.folder.join("mudling.json"), r#"{"format": 999}"#).unwrap();
    world.mac.git.commit_all("from the future").unwrap();
    world.mac.git.push(REMOTE, BRANCH).unwrap();
    world.ipad.commit_local();
    let theirs = world.ipad.fetch().unwrap();

    let error = pull(
        &world.ipad.git,
        &world.ipad.repo,
        theirs,
        &world.ipad.stores(),
    )
    .unwrap_err();

    assert!(matches!(error, Error::Repo(repo::Error::NewerFormat(999))));
}

#[test]
fn a_password_in_server_settings_never_leaves_the_device() {
    let folder = TempDir::new().unwrap();
    let remote = folder.path().join("remote.git");
    Repository::init_bare(&remote).unwrap();
    let device = Device::new(folder.path(), "mac");
    device.apply(|batch| {
        batch.upsert(server(r#"{"encoding":"utf-8","password":"hunter2"}"#));
    });
    device.commit_local();

    let error = device.git.push(REMOTE, BRANCH).unwrap_err();

    let GitError::PasswordInFile { path, field } = &error else {
        panic!("{error:?}");
    };
    assert_eq!(path, "servers.json");
    assert_eq!(field, "servers[0].settings.password");
    assert!(!error.to_string().contains("hunter2"));
    let bare = Repository::open_bare(&remote).unwrap();
    assert!(bare.refname_to_id("refs/heads/main").is_err());
}

#[test]
fn a_password_in_an_earlier_unpushed_commit_is_refused_too() {
    let world = two_devices();
    let notes = world.mac.folder.join("keep.txt");
    fs::write(&notes, "password = \"hunter2\"\n").unwrap();
    world.mac.git.commit_all("oops").unwrap();
    fs::remove_file(&notes).unwrap();
    world.mac.git.commit_all("taken out").unwrap();

    let error = world.mac.git.push(REMOTE, BRANCH).unwrap_err();

    assert!(matches!(error, GitError::PasswordInFile { .. }));
}

#[test]
fn the_ignore_file_keeps_the_vaults_half_written_copies_out() {
    let world = two_devices();
    let device = &world.mac;
    let ignore = fs::read_to_string(device.folder.join(".gitignore")).unwrap();
    assert!(ignore.lines().any(|line| line == ".secrets.vault.*"));
    let attributes = fs::read_to_string(device.folder.join(".gitattributes")).unwrap();
    assert!(attributes.lines().any(|line| line == "* -text"));

    fs::write(device.folder.join(".secrets.vault.a1b2c3"), "half").unwrap();
    fs::write(device.folder.join("session.log"), "typing").unwrap();
    fs::write(device.folder.join("kept.txt"), "kept").unwrap();
    let commit = device.git.commit_all("a commit").unwrap().unwrap();

    let files = device.git.files_at(commit).unwrap();
    assert!(files.contains_key(Path::new("kept.txt")));
    assert!(!files.contains_key(Path::new(".secrets.vault.a1b2c3")));
    assert!(!files.contains_key(Path::new("session.log")));
}

#[test]
fn the_badge_counts_sessions_not_commits() {
    let world = two_devices();
    let device = &world.mac;
    assert_eq!(unsynced_sessions(&device.git, REMOTE, BRANCH).unwrap(), 0);

    device.apply(|batch| {
        batch.upsert(room(2, "A hill", T2));
    });
    device.commit_local();
    device.apply(|batch| {
        batch.upsert(room(3, "A lake", T2));
    });
    device.commit_local();
    assert_eq!(unsynced_sessions(&device.git, REMOTE, BRANCH).unwrap(), 1);

    let onto = device.fetch().unwrap();
    device
        .git
        .squash(BRANCH, onto, "A MUD · Alda · 2h10: 2 rooms")
        .unwrap();
    device.apply(|batch| {
        batch.upsert(room(4, "A cave", T2));
    });
    device.commit_local();
    assert_eq!(unsynced_sessions(&device.git, REMOTE, BRANCH).unwrap(), 2);

    device.git.push(REMOTE, BRANCH).unwrap();
    device.fetch().unwrap();
    assert_eq!(unsynced_sessions(&device.git, REMOTE, BRANCH).unwrap(), 0);
}

#[test]
fn a_repo_with_no_remote_has_nothing_unsynced() {
    let folder = TempDir::new().unwrap();
    let git = git::Repo::open_or_init(folder.path()).unwrap();
    fs::write(folder.path().join("a.txt"), "a").unwrap();
    git.commit_all("a").unwrap();

    assert_eq!(unsynced_sessions(&git, REMOTE, BRANCH).unwrap(), 0);
}
