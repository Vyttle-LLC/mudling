//! Two devices over a bare repo in a temp folder: no network, and nothing
//! here can reach a real remote.

use super::*;
use crate::persist::{
    Batch, Exit, GearCopy, Key, Meta, NOTEPAD, Room, Server, Sighting, Timestamp, Tombstone,
};
use crate::repo::format::tombstone_cutoff;
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
