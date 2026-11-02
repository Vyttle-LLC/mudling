//! Devices over a bare repo in a temp folder, or a git server on a local
//! socket: nothing here can reach a real remote.
//!
//! The kill tests run a device in a child process that exits at a named step,
//! with no destructor running, then carry on in this one.

mod git_server;

use super::*;
use crate::map::{CURRENT_ROOM, JoinDone, LastJoin, MapRoom, VERSION_JOIN};
use crate::merge::{self, CHECKPOINT};
use crate::persist::{Batch, Character, Database, Room, RoomNote, Server, Skill, Timestamp};
use crate::session::{MapEdit, Session, SessionConfig};
use crate::store::{MapState, SettingsState, Versioned};
use crate::telnet::Bytes;
use crate::vault::{AutoLock, EntryKey, FILE_NAME, MemoryKeyStore, Vault};
use git_server::GitServer;
use git2::Repository;
use std::fs;
use std::path::Path;
use std::process::Command as Process;
use std::sync::Arc;
use std::thread;
use std::time::Instant;
use tempfile::TempDir;

const SERVER: &str = "mud.example.org";
const DATA: &str = "MUDLING_TEST_DATA";
const REPO: &str = "MUDLING_TEST_REPO";
/// Long enough that a passing test never sees it.
const PATIENCE: Duration = Duration::from_secs(60);

fn server(settings: &str) -> Server {
    Server {
        id: SERVER.to_owned(),
        name: "A MUD".to_owned(),
        host: SERVER.to_owned(),
        port: 4000,
        settings: settings.to_owned(),
        updated: Timestamp(1),
    }
}

fn room(id: i64, name: &str, area: &str) -> Room {
    Room {
        id,
        name: name.to_owned(),
        area: area.to_owned(),
        environment: "field".to_owned(),
        description: String::new(),
        x: id,
        y: 0,
        z: 0,
        building: None,
        tangled: false,
        maze: false,
        updated: Timestamp::now(),
    }
}

fn character(name: &str) -> Character {
    Character {
        id: name.to_lowercase(),
        server_id: SERVER.to_owned(),
        name: name.to_owned(),
        updated: Timestamp::now(),
    }
}

struct Device {
    data: PathBuf,
    repo: PathBuf,
}

impl Device {
    fn new(root: &Path, name: &str, remote: &str) -> Device {
        let device = Device {
            data: root.join(format!("{name}-data")),
            repo: root.join(name),
        };
        // On main, as a repo MUDling starts is.
        drop(git::Repo::open_or_init(&device.repo).unwrap());
        let repository = Repository::open(&device.repo).unwrap();
        repository.remote("origin", remote).unwrap();
        device
    }

    fn folder(&self) -> PathBuf {
        self.data.join("servers")
    }

    fn database(&self) -> Database {
        Database::open(&self.folder(), SERVER).unwrap()
    }

    fn apply(&self, fill: impl FnOnce(&mut Batch)) {
        let writer = self.database().writer().unwrap();
        let mut batch = Batch::new();
        fill(&mut batch);
        writer.submit(batch).unwrap();
        writer.flush().unwrap();
    }

    fn rooms(&self) -> Vec<String> {
        let reader = self.database().reader().unwrap();
        let mut names: Vec<String> = reader
            .map()
            .unwrap()
            .rooms
            .into_iter()
            .map(|r| r.name)
            .collect();
        names.sort();
        names
    }

    fn config(&self) -> SyncConfig {
        SyncConfig {
            repo: Some(RepoSettings::new(&self.repo)),
            quit_wait: PATIENCE,
            ..SyncConfig::new(&self.data)
        }
    }

    /// Runs a whole launch-to-quit of the app's sync.
    fn sync(&self) -> SyncState {
        let sync = Syncer::start(self.config(), || {}).unwrap();
        sync.quit().wait();
        state_of(&sync)
    }

    fn set_remote(&self, url: &str) {
        let repository = Repository::open(&self.repo).unwrap();
        repository.remote_set_url("origin", url).unwrap();
    }

    fn holds_file(&self, name: &str) -> bool {
        holds_file(&self.repo, name)
    }

    fn tip(&self) -> git::Oid {
        let repository = Repository::open(&self.repo).unwrap();
        repository.refname_to_id("refs/heads/main").unwrap()
    }
}

use crate::git;

fn holds_file(folder: &Path, name: &str) -> bool {
    fs::read_dir(folder).unwrap().flatten().any(|entry| {
        let path = entry.path();
        entry.file_name() == name
            || (path.is_dir() && entry.file_name() != ".git" && holds_file(&path, name))
    })
}

fn state_of(sync: &Syncer) -> SyncState {
    sync.stores().snapshot::<SyncState>().value().clone()
}

/// Waits for the store to show what `ready` wants.
fn wait_for(sync: &Syncer, ready: impl Fn(&SyncState) -> bool) -> SyncState {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let state = state_of(sync);
        if ready(&state) {
            return state;
        }
        assert!(Instant::now() < deadline, "stuck at {state:?}");
        thread::sleep(Duration::from_millis(10));
    }
}

/// What the bare repo's `main` holds, or nothing.
fn remote_files(remote: &Path) -> Vec<String> {
    let repository = Repository::open_bare(remote).unwrap();
    let Ok(tip) = repository.refname_to_id("refs/heads/main") else {
        return Vec::new();
    };
    let tree = repository.find_commit(tip).unwrap().tree().unwrap();
    let mut files = Vec::new();
    tree.walk(git2::TreeWalkMode::PreOrder, |_, entry| {
        files.extend(entry.name().map(str::to_owned));
        git2::TreeWalkResult::Ok
    })
    .unwrap();
    files
}

fn remote_commits(remote: &Path) -> Vec<String> {
    let repository = Repository::open_bare(remote).unwrap();
    let mut walk = repository.revwalk().unwrap();
    walk.push_ref("refs/heads/main").unwrap();
    walk.map(|id| {
        let commit = repository.find_commit(id.unwrap()).unwrap();
        commit.message().unwrap().to_owned()
    })
    .collect()
}

struct World {
    folder: TempDir,
    remote: PathBuf,
    mac: Device,
    ipad: Device,
}

/// Two devices that have synced one room in one zone.
fn two_devices() -> World {
    let folder = TempDir::new().unwrap();
    let remote = folder.path().join("remote.git");
    Repository::init_bare(&remote).unwrap();
    let url = remote.to_str().unwrap();
    let mac = Device::new(folder.path(), "mac", url);
    let ipad = Device::new(folder.path(), "ipad", url);
    mac.apply(|batch| {
        batch.upsert(server("{}"));
        batch.upsert(room(1, "A quiet road", "The Meadow"));
    });
    assert_eq!(mac.sync().problem, None);
    assert_eq!(ipad.sync().problem, None);
    assert_eq!(ipad.rooms(), ["A quiet road"]);
    assert_eq!(mac.sync().problem, None);
    World {
        folder,
        remote,
        mac,
        ipad,
    }
}

/// The child of a kill test: one launch, which exits at the step named.
#[test]
#[ignore = "a kill test runs this in a child process"]
fn killed_device() {
    let Ok(data) = std::env::var(DATA) else {
        return;
    };
    let config = SyncConfig {
        repo: Some(RepoSettings::new(std::env::var(REPO).unwrap())),
        quit_wait: PATIENCE,
        ..SyncConfig::new(data)
    };
    let sync = Syncer::start(config, || {}).unwrap();
    sync.quit().wait();
}

fn kill_at(device: &Device, step: &str) {
    let status = Process::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "sync::tests::killed_device"])
        .env(kill::STEP, step)
        .env(DATA, &device.data)
        .env(REPO, &device.repo)
        .status()
        .unwrap();
    assert_eq!(
        status.code(),
        Some(kill::EXIT_CODE),
        "{step} wasn't reached"
    );
}

/// The Mac finds a zone and a character the iPad has no record of, and the iPad
/// finds a room of its own before its sync dies.
fn devices_with_a_pull_waiting() -> World {
    let world = two_devices();
    world.mac.apply(|batch| {
        batch.upsert(room(2, "A far peak", "The Hills"));
        batch.upsert(character("Bren"));
        batch.upsert(Skill {
            character_id: "bren".to_owned(),
            name: "kick".to_owned(),
            level: Some(1),
            learned: Some(78),
            cost: None,
            updated: Timestamp::now(),
        });
    });
    assert_eq!(world.mac.sync().problem, None);
    world.ipad.apply(|batch| {
        batch.upsert(room(3, "A still lake", "The Meadow"));
    });
    world
}

const ALL_ROOMS: [&str; 3] = ["A far peak", "A quiet road", "A still lake"];

#[test]
fn a_kill_at_any_step_of_a_sync_loses_nothing() {
    for step in [
        "pass: exported",
        "pass: committed",
        "pass: marked seen",
        "pass: fetched",
        "land: databases forgot what they saw",
        "land: halfway through the files",
        "land: files written",
        "land: imported",
        "land: branch moved",
        "pass: squashed",
        "pass: pushed",
    ] {
        let world = devices_with_a_pull_waiting();

        kill_at(&world.ipad, step);

        assert_eq!(world.ipad.sync().problem, None, "{step}");
        assert_eq!(world.mac.sync().problem, None, "{step}");
        assert_eq!(world.ipad.sync().problem, None, "{step}");
        assert_eq!(world.mac.rooms(), ALL_ROOMS, "{step}");
        assert_eq!(world.ipad.rooms(), ALL_ROOMS, "{step}");
        assert!(world.ipad.holds_file("Bren.json"), "{step}");
        assert!(world.ipad.holds_file("The Hills.json"), "{step}");
        assert!(remote_files(&world.remote).contains(&"The Hills.json".to_owned()));
    }
}

/// With the remote gone there's no merge to run again, so only the import
/// before export keeps what was pulled.
#[test]
fn a_kill_between_pull_and_import_does_not_let_the_next_export_delete_pulled_files() {
    for step in ["land: files written", "land: halfway through the files"] {
        let world = devices_with_a_pull_waiting();
        kill_at(&world.ipad, step);
        let url = world.remote.to_str().unwrap().to_owned();
        world.ipad.set_remote(&format!("{url}-gone"));

        let state = world.ipad.sync();

        assert!(state.problem.is_some(), "{step}");
        assert!(
            world.ipad.rooms().contains(&"A still lake".to_owned()),
            "{step}"
        );
        if step == "land: files written" {
            assert_eq!(world.ipad.rooms(), ALL_ROOMS);
            assert!(world.ipad.holds_file("Bren.json"));
            assert!(world.ipad.holds_file("The Hills.json"));
        }

        world.ipad.set_remote(&url);
        assert_eq!(world.ipad.sync().problem, None, "{step}");
        assert_eq!(world.ipad.rooms(), ALL_ROOMS, "{step}");
    }
}

#[test]
fn a_kill_during_a_first_join_loses_nothing_and_the_join_runs_again() {
    let world = two_devices();
    let newcomer = Device::new(world.folder.path(), "phone", world.remote.to_str().unwrap());
    assert_eq!(newcomer.sync().problem, None);
    fs::remove_dir_all(&newcomer.data).unwrap();
    newcomer.apply(|batch| {
        batch.upsert(server("{}"));
        batch.upsert(room(9, "A phone booth", "The Meadow"));
    });

    kill_at(&newcomer, "pass: joined");

    assert_eq!(newcomer.sync().problem, None);
    assert_eq!(newcomer.rooms(), ["A phone booth", "A quiet road"]);
}

#[test]
fn the_first_export_on_a_device_that_holds_a_repo_deletes_nothing() {
    let world = two_devices();
    world.ipad.sync();
    fs::remove_dir_all(&world.ipad.data).unwrap();
    // A session made the database with only its server row, as a first
    // connection does before sync gets there.
    world.ipad.apply(|batch| {
        batch.upsert(server("{}"));
    });

    assert_eq!(world.ipad.sync().problem, None);

    assert_eq!(world.ipad.rooms(), ["A quiet road"]);
    assert!(world.ipad.holds_file("The Meadow.json"));
    assert!(remote_files(&world.remote).contains(&"The Meadow.json".to_owned()));
}

#[test]
fn a_branch_moved_by_another_git_is_taken_in_before_any_export() {
    let world = two_devices();
    world.mac.apply(|batch| {
        batch.upsert(room(2, "A far peak", "The Hills"));
    });
    world.mac.sync();
    let bare = Repository::open_bare(&world.remote).unwrap();
    let tip = bare.refname_to_id("refs/heads/main").unwrap();
    let ipad = Repository::open(&world.ipad.repo).unwrap();
    ipad.remote_anonymous(world.remote.to_str().unwrap())
        .unwrap()
        .fetch(&["refs/heads/main:refs/heads/main-by-hand"], None, None)
        .unwrap();
    ipad.set_head("refs/heads/main-by-hand").unwrap();
    ipad.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
        .unwrap();
    ipad.reference("refs/heads/main", tip, true, "by hand")
        .unwrap();
    ipad.set_head("refs/heads/main").unwrap();
    drop(ipad);

    assert_eq!(world.ipad.sync().problem, None);

    assert_eq!(world.ipad.rooms(), ["A far peak", "A quiet road"]);
    assert!(remote_files(&world.remote).contains(&"The Hills.json".to_owned()));
}

#[test]
fn a_server_only_the_other_device_has_gets_a_database_here() {
    let world = two_devices();
    let other = Database::open(&world.mac.folder(), "other.example.org").unwrap();
    let writer = other.writer().unwrap();
    let mut batch = Batch::new();
    batch.upsert(Server {
        id: "other.example.org".to_owned(),
        host: "other.example.org".to_owned(),
        ..server("{}")
    });
    batch.upsert(room(5, "A distant shore", "The Coast"));
    writer.submit(batch).unwrap();
    writer.flush().unwrap();
    world.mac.sync();

    assert_eq!(world.ipad.sync().problem, None);

    let arrived = Database::open(&world.ipad.folder(), "other.example.org").unwrap();
    let rooms = arrived.reader().unwrap().map().unwrap().rooms;
    assert_eq!(rooms.len(), 1);
}

#[test]
fn a_failed_batch_stops_the_sync_with_the_branch_unmoved_and_says_so() {
    let world = devices_with_a_pull_waiting();
    world.ipad.apply(|_| {});
    let path = world.ipad.database().path().to_owned();
    let poison = rusqlite::Connection::open(&path).unwrap();
    poison
        .execute_batch(
            "CREATE TRIGGER refuse BEFORE INSERT ON rooms WHEN NEW.name = 'A far peak'
             BEGIN SELECT RAISE(ABORT, 'refused'); END;",
        )
        .unwrap();
    let remote_tip_before = remote_commits(&world.remote).len();

    let state = world.ipad.sync();

    let problem = state.problem.expect("the player is told");
    assert!(problem.contains("branch didn't move"), "{problem}");
    assert!(!world.ipad.rooms().contains(&"A far peak".to_owned()));
    assert_eq!(remote_commits(&world.remote).len(), remote_tip_before);
    let repository = Repository::open(&world.ipad.repo).unwrap();
    let theirs = repository
        .refname_to_id("refs/remotes/origin/main")
        .unwrap();
    let merged = repository.graph_descendant_of(world.ipad.tip(), theirs);
    assert!(!merged.unwrap(), "the branch holds no part of the merge");

    poison.execute_batch("DROP TRIGGER refuse").unwrap();
    assert_eq!(world.ipad.sync().problem, None);
    assert_eq!(world.ipad.rooms(), ALL_ROOMS);
}

#[test]
fn a_refused_push_names_the_field_and_never_the_value() {
    let world = two_devices();
    let before = remote_commits(&world.remote);
    world.mac.apply(|batch| {
        batch.upsert(Server {
            updated: Timestamp::now(),
            ..server(r#"{"login":{"password":"hunter2"}}"#)
        });
    });

    let problem = world.mac.sync().problem.expect("the push is refused");

    assert!(problem.contains("login.password"), "{problem}");
    assert!(!problem.contains("hunter2"), "{problem}");
    assert_eq!(remote_commits(&world.remote), before);
}

/// Whether any commit on the bare repo's `main` holds `text` in any file.
fn remote_ever_held(remote: &Path, text: &str) -> bool {
    let repository = Repository::open_bare(remote).unwrap();
    let mut walk = repository.revwalk().unwrap();
    walk.push_ref("refs/heads/main").unwrap();
    walk.any(|id| {
        let tree = repository.find_commit(id.unwrap()).unwrap().tree().unwrap();
        let mut found = false;
        tree.walk(git2::TreeWalkMode::PreOrder, |_, entry| {
            if let Ok(blob) = repository.find_blob(entry.id()) {
                found |= blob
                    .content()
                    .windows(text.len())
                    .any(|w| w == text.as_bytes());
            }
            git2::TreeWalkResult::Ok
        })
        .unwrap();
        found
    })
}

#[test]
fn removing_a_refused_password_lets_the_next_sync_push_without_it() {
    let world = two_devices();
    world.mac.apply(|batch| {
        batch.upsert(Server {
            updated: Timestamp::now(),
            ..server(r#"{"login":{"password":"hunter2"}}"#)
        });
    });
    assert!(world.mac.sync().problem.is_some());

    world.mac.apply(|batch| {
        batch.upsert(Server {
            updated: Timestamp::now(),
            ..server("{}")
        });
        batch.upsert(room(2, "A hill", "The Meadow"));
    });
    let state = world.mac.sync();

    assert_eq!(state.problem, None);
    assert_eq!(state.unsynced, 0);
    assert!(!remote_ever_held(&world.remote, "hunter2"));
    assert!(remote_ever_held(&world.remote, "A hill"));
    assert_eq!(world.ipad.sync().problem, None);
    assert_eq!(world.ipad.rooms(), ["A hill", "A quiet road"]);
}

#[test]
fn a_password_in_the_very_first_push_can_be_removed_too() {
    let folder = TempDir::new().unwrap();
    let remote = folder.path().join("remote.git");
    Repository::init_bare(&remote).unwrap();
    let device = Device::new(folder.path(), "mac", remote.to_str().unwrap());
    device.apply(|batch| {
        batch.upsert(server(r#"{"login":{"password":"hunter2"}}"#));
    });
    assert!(device.sync().problem.is_some());

    device.apply(|batch| {
        batch.upsert(Server {
            updated: Timestamp::now(),
            ..server("{}")
        });
    });

    assert_eq!(device.sync().problem, None);
    assert!(!remote_ever_held(&remote, "hunter2"));
}

#[test]
fn a_password_still_there_stays_refused() {
    let world = two_devices();
    world.mac.apply(|batch| {
        batch.upsert(Server {
            updated: Timestamp::now(),
            ..server(r#"{"login":{"password":"hunter2"}}"#)
        });
    });

    assert!(world.mac.sync().problem.is_some());
    assert!(world.mac.sync().problem.is_some());
    assert!(!remote_ever_held(&world.remote, "hunter2"));
}

#[test]
fn a_shorter_pass_working_leaves_a_failed_push_waiting_and_on_show() {
    let world = two_devices();
    let url = world.remote.to_str().unwrap().to_owned();
    world.mac.set_remote(&format!("{url}-gone"));
    world.mac.apply(|batch| {
        batch.upsert(room(2, "A hill", "The Meadow"));
    });
    let sync = Syncer::start(
        SyncConfig {
            first_retry: Duration::from_millis(400),
            commit_every: Duration::from_millis(20),
            ..world.mac.config()
        },
        || {},
    )
    .unwrap();
    wait_for(&sync, |state| state.problem.is_some());
    sync.connected();

    // Timed commits work throughout, and none of them may clear this.
    let git = Repository::open(&world.mac.repo).unwrap();
    let tip = git.refname_to_id("refs/heads/main").unwrap();
    world.mac.apply(|batch| {
        batch.upsert(room(3, "A dale", "The Meadow"));
    });
    wait_until(|| git.refname_to_id("refs/heads/main").unwrap() != tip);
    assert!(state_of(&sync).problem.is_some());

    world.mac.set_remote(&url);
    wait_for(&sync, |state| state.problem.is_none());
    sync.quit().wait();
    assert!(remote_ever_held(&world.remote, "A dale"));
}

#[test]
fn a_pull_that_reaches_the_remote_sends_the_waiting_push_at_once() {
    let world = two_devices();
    let url = world.remote.to_str().unwrap().to_owned();
    world.mac.set_remote(&format!("{url}-gone"));
    world.mac.apply(|batch| {
        batch.upsert(room(2, "A hill", "The Meadow"));
    });
    let sync = Syncer::start(
        SyncConfig {
            first_retry: PATIENCE * 10,
            ..world.mac.config()
        },
        || {},
    )
    .unwrap();
    wait_for(&sync, |state| state.problem.is_some());

    world.mac.set_remote(&url);
    sync.foreground();

    wait_for(&sync, |state| {
        state.problem.is_none() && state.unsynced == 0
    });
    sync.quit();
    assert!(remote_ever_held(&world.remote, "A hill"));
}

#[test]
fn a_session_of_checkpoints_reaches_the_remote_as_one_commit() {
    let world = two_devices();
    let before = remote_commits(&world.remote).len();
    let sync = Syncer::start(
        SyncConfig {
            commit_every: Duration::from_millis(20),
            ..world.mac.config()
        },
        || {},
    )
    .unwrap();
    wait_for(&sync, |state| state.last_sync.is_some() && !state.syncing);
    sync.connected();
    let git = Repository::open(&world.mac.repo).unwrap();
    let commits = || {
        let mut walk = git.revwalk().unwrap();
        walk.push_head().unwrap();
        walk.count()
    };
    let start = commits();
    for (id, name) in [(2, "A hill"), (3, "A dale")] {
        let seen = commits();
        world.mac.apply(|batch| {
            batch.upsert(room(id, name, "The Meadow"));
        });
        let deadline = Instant::now() + PATIENCE;
        while commits() == seen {
            assert!(Instant::now() < deadline, "no checkpoint");
            thread::sleep(Duration::from_millis(5));
        }
    }
    assert_eq!(commits(), start + 2);
    wait_for(&sync, |state| state.unsynced == 1);

    let (done, last_rows_in) = mpsc::sync_channel(1);
    done.send(()).unwrap();
    sync.disconnected(last_rows_in);

    wait_for(&sync, |state| state.changed.is_some() && !state.syncing);
    sync.quit().wait();
    let commits = remote_commits(&world.remote);
    assert_eq!(commits.len(), before + 1);
    assert!(commits[0].contains("2 rooms"), "{}", commits[0]);
    assert!(
        !commits
            .iter()
            .any(|message| message.trim_end() == CHECKPOINT)
    );
}

#[test]
fn a_repo_with_no_remote_still_gets_commits() {
    let folder = TempDir::new().unwrap();
    let device = Device {
        data: folder.path().join("data"),
        repo: folder.path().join("repo"),
    };
    device.apply(|batch| {
        batch.upsert(server("{}"));
        batch.upsert(room(1, "A quiet road", "The Meadow"));
    });

    let state = device.sync();

    assert_eq!(state.problem, None);
    assert_eq!(state.unsynced, 0);
    assert!(device.holds_file("The Meadow.json"));
    assert!(state.last_sync.is_some());
}

#[test]
fn no_repo_keeps_everything_local_and_does_nothing() {
    let folder = TempDir::new().unwrap();
    let sync = Syncer::start(SyncConfig::new(folder.path()), || {}).unwrap();
    sync.sync_now();
    sync.quit().wait();

    let state = state_of(&sync);
    assert_eq!(state.repo, None);
    assert_eq!(state.problem, None);
    assert_eq!(state.vault_file, folder.path().join(FILE_NAME));
}

#[test]
fn newer_settings_from_another_device_are_kept_and_not_written_back() {
    let world = two_devices();
    let newer = crate::repo::AppSettings {
        theme: "dusk".to_owned(),
        updated: Timestamp::now(),
        ..Default::default()
    };
    let sync = Syncer::start(world.mac.config(), || {}).unwrap();
    sync.settings_changed(newer.clone());
    sync.sync_now();
    sync.quit().wait();

    let state = world.ipad.sync();

    assert_eq!(state.settings, newer);
    world.ipad.sync();
    assert_eq!(
        crate::repo::Repo::new(&world.ipad.repo).settings().unwrap(),
        newer
    );
}

#[test]
fn a_server_failing_with_503_is_retried_slowly_and_told_then_recovers() {
    let folder = TempDir::new().unwrap();
    let remote = folder.path().join("remote.git");
    Repository::init_bare(&remote).unwrap();
    let git_server = GitServer::start(&remote);
    let device = Device::new(folder.path(), "mac", &git_server.url());
    device.apply(|batch| {
        batch.upsert(server("{}"));
        batch.upsert(room(1, "A quiet road", "The Meadow"));
    });
    git_server.fail_with_503(true);

    let sync = Syncer::start(
        SyncConfig {
            first_retry: Duration::from_millis(100),
            ..device.config()
        },
        || {},
    )
    .unwrap();

    let told = wait_for(&sync, |state| state.problem.is_some());
    let problem = told.problem.unwrap();
    assert!(problem.contains("having trouble"), "{problem}");
    assert!(problem.contains("Trying again"), "{problem}");
    assert_eq!(remote_commits_or_none(&remote), 0);

    git_server.fail_with_503(false);

    wait_for(&sync, |state| {
        state.problem.is_none() && state.last_sync.is_some()
    });
    sync.quit().wait();
    assert!(remote_files(&remote).contains(&"The Meadow.json".to_owned()));
}

fn remote_commits_or_none(remote: &Path) -> usize {
    let repository = Repository::open_bare(remote).unwrap();
    usize::from(repository.refname_to_id("refs/heads/main").is_ok())
}

#[test]
fn a_push_that_loses_a_race_pulls_and_pushes_again() {
    let folder = TempDir::new().unwrap();
    let remote = folder.path().join("remote.git");
    Repository::init_bare(&remote).unwrap();
    let git_server = GitServer::start(&remote);
    let device = Device::new(folder.path(), "mac", &git_server.url());
    device.apply(|batch| {
        batch.upsert(server("{}"));
        batch.upsert(room(1, "A quiet road", "The Meadow"));
    });
    assert_eq!(device.sync().problem, None);
    device.apply(|batch| {
        batch.upsert(room(2, "A far peak", "The Hills"));
    });
    let bare = remote.clone();
    git_server.before_next_push(move || commit_file(&bare, "raced.txt"));

    let state = device.sync();

    assert_eq!(state.problem, None);
    assert!(git_server.lost_a_race());
    let files = remote_files(&remote);
    assert!(files.contains(&"raced.txt".to_owned()), "{files:?}");
    assert!(files.contains(&"The Hills.json".to_owned()), "{files:?}");
    assert!(device.holds_file("raced.txt"));
}

/// Another device's push, landing straight on the bare repo.
fn commit_file(bare: &Path, name: &str) {
    let repository = Repository::open_bare(bare).unwrap();
    let tip = repository
        .find_commit(repository.refname_to_id("refs/heads/main").unwrap())
        .unwrap();
    let blob = repository.blob(b"from another device\n").unwrap();
    let mut tree = repository.treebuilder(Some(&tip.tree().unwrap())).unwrap();
    tree.insert(name, blob, 0o100_644).unwrap();
    let tree = repository.find_tree(tree.write().unwrap()).unwrap();
    let signature = git2::Signature::now("Another", "other@example.org").unwrap();
    repository
        .commit(
            Some("refs/heads/main"),
            &signature,
            &signature,
            "Another device",
            &tree,
            &[&tip],
        )
        .unwrap();
}

#[cfg(unix)]
fn hanging_device(folder: &Path, git_server: &GitServer, pid_file: &Path) -> Device {
    let device = Device::new(folder, "mac", &git_server.url());
    device.apply(|batch| {
        batch.upsert(server("{}"));
        batch.upsert(room(1, "A quiet road", "The Meadow"));
    });
    let helper = format!("!echo $$ > '{}'; exec sleep 600; ", pid_file.display());
    Repository::open(&device.repo)
        .unwrap()
        .config()
        .unwrap()
        .set_str("credential.helper", &helper)
        .unwrap();
    device
}

#[cfg(unix)]
#[test]
fn a_hanging_credential_helper_is_given_up_on_and_says_so() {
    let folder = TempDir::new().unwrap();
    let remote = folder.path().join("remote.git");
    Repository::init_bare(&remote).unwrap();
    let git_server = GitServer::start(&remote);
    git_server.ask_for_sign_in();
    let pid_file = folder.path().join("helper-pid");
    let device = hanging_device(folder.path(), &git_server, &pid_file);

    let began = Instant::now();
    let sync = Syncer::start(
        SyncConfig {
            network_limit: Duration::from_millis(300),
            ..device.config()
        },
        || {},
    )
    .unwrap();

    let told = wait_for(&sync, |state| state.problem.is_some());
    assert!(began.elapsed() < Duration::from_secs(20));
    let problem = told.problem.unwrap();
    assert!(problem.contains("didn't answer"), "{problem}");
    assert!(device.holds_file("The Meadow.json"));
    sync.quit();
}

#[cfg(unix)]
#[test]
fn quit_lets_go_of_a_hanging_helper_after_about_two_seconds() {
    let folder = TempDir::new().unwrap();
    let remote = folder.path().join("remote.git");
    Repository::init_bare(&remote).unwrap();
    let git_server = GitServer::start(&remote);
    git_server.ask_for_sign_in();
    let pid_file = folder.path().join("helper-pid");
    let device = hanging_device(folder.path(), &git_server, &pid_file);
    let sync = Syncer::start(
        SyncConfig {
            quit_wait: QUIT_WAIT,
            ..device.config()
        },
        || {},
    )
    .unwrap();
    wait_for(&sync, |_| pid_file.exists());

    let began = Instant::now();
    sync.quit().wait();

    assert!(began.elapsed() < QUIT_WAIT + Duration::from_secs(1));
    assert!(device.holds_file("The Meadow.json"), "committed first");
}

#[test]
fn a_lock_left_by_another_git_is_retried_and_stated_plainly() {
    let world = two_devices();
    let lock = world.mac.repo.join(".git").join("index.lock");
    fs::write(&lock, "").unwrap();
    world.mac.apply(|batch| {
        batch.upsert(room(2, "A hill", "The Meadow"));
    });

    let sync = Syncer::start(
        SyncConfig {
            first_retry: Duration::from_millis(100),
            ..world.mac.config()
        },
        || {},
    )
    .unwrap();

    let told = wait_for(&sync, |state| {
        state
            .problem
            .as_ref()
            .is_some_and(|p| p.contains("index.lock"))
    });
    let problem = told.problem.unwrap();
    assert!(problem.contains("Another git program"), "{problem}");

    fs::remove_file(lock).unwrap();
    wait_for(&sync, |state| {
        state.problem.is_none() && state.last_sync.is_some()
    });
    sync.quit().wait();
    assert!(remote_files(&world.remote).contains(&"The Meadow.json".to_owned()));
}

fn block_on<T>(work: impl Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(work)
}

/// Saves a real vault at `path` with one password, and hands back its handle.
fn vault_with(path: &Path, server: &str) -> Vault {
    let vault = Vault::open(
        path.to_owned(),
        Arc::new(MemoryKeyStore::default()),
        AutoLock::NEVER,
    );
    block_on(async {
        drop(
            vault
                .create("a long and unusual passphrase".into())
                .await
                .unwrap(),
        );
        vault
            .set_password(EntryKey::new(server, "Alda"), "hunter2".into())
            .await
            .unwrap();
    });
    vault
}

#[test]
fn setting_a_repo_moves_the_local_vault_into_it_and_clearing_brings_it_home() {
    let world = two_devices();
    fs::create_dir_all(&world.mac.data).unwrap();
    let local = world.mac.data.join(FILE_NAME);
    let bytes = {
        let _vault = vault_with(&local, SERVER);
        fs::read(&local).unwrap()
    };

    let state = world.mac.sync();

    assert_eq!(state.problem, None);
    assert!(!local.exists(), "the local copy goes once it's committed");
    assert_eq!(fs::read(world.mac.repo.join(FILE_NAME)).unwrap(), bytes);
    assert_eq!(state.vault_file, world.mac.repo.join(FILE_NAME));
    assert!(remote_files(&world.remote).contains(&FILE_NAME.to_owned()));

    let sync = Syncer::start(world.mac.config(), || {}).unwrap();
    sync.clear_repo();
    wait_for(&sync, |state| state.repo.is_none());
    sync.quit().wait();

    assert_eq!(fs::read(&local).unwrap(), bytes);
    assert_eq!(fs::read(world.mac.repo.join(FILE_NAME)).unwrap(), bytes);
    assert_eq!(state_of(&sync).vault_file, local);
}

/// A sync set to `repo`, over data in `data`, which holds a vault: it says why
/// it won't run, and the vault is where it was with nothing committed.
fn refused_repo_leaves_the_vault_alone(data: &Path, repo: &Path) {
    fs::create_dir_all(data).unwrap();
    let local = data.join(FILE_NAME);
    let bytes = {
        let _vault = vault_with(&local, SERVER);
        fs::read(&local).unwrap()
    };
    let sync = Syncer::start(
        SyncConfig {
            repo: Some(RepoSettings::new(repo)),
            ..SyncConfig::new(data)
        },
        || {},
    )
    .unwrap();

    let state = wait_for(&sync, |state| state.problem.is_some());
    sync.sync_now();
    thread::sleep(Duration::from_millis(300));
    sync.quit().wait();

    assert!(
        state.problem.unwrap().contains("holds MUDling's own data"),
        "said in plain words"
    );
    assert_eq!(fs::read(&local).unwrap(), bytes, "the vault stays");
    assert!(!repo.join(FILE_NAME).exists() || repo == data);
    assert!(!repo.join(".git").exists(), "nothing was made or committed");
    assert!(state_of(&sync).repo.is_none());
}

#[test]
fn a_repo_that_is_the_data_folder_is_refused_and_the_vault_stays() {
    let folder = TempDir::new().unwrap();
    let data = folder.path().join("data");
    refused_repo_leaves_the_vault_alone(&data, &data);
}

#[test]
fn a_repo_that_holds_the_data_folder_is_refused_and_the_vault_stays() {
    let folder = TempDir::new().unwrap();
    let data = folder.path().join("everything").join("data");
    refused_repo_leaves_the_vault_alone(&data, folder.path());
}

#[test]
fn a_folder_beside_the_data_folder_is_no_overlap_and_a_link_to_it_is() {
    let folder = TempDir::new().unwrap();
    let data = folder.path().join("data");
    let beside = folder.path().join("repo");
    fs::create_dir_all(&data).unwrap();
    assert!(!repo_holds_data_folder(&beside, &data));
    assert!(!repo_holds_data_folder(&data.join("repo"), &data));
    #[cfg(unix)]
    {
        let link = folder.path().join("link");
        std::os::unix::fs::symlink(&data, &link).unwrap();
        assert!(repo_holds_data_folder(&link, &data));
        assert!(repo_holds_data_folder(
            &link.join("not-made-yet"),
            &link.join("not-made-yet")
        ));
    }
}

#[test]
fn choosing_another_repo_takes_the_vault_along() {
    let world = two_devices();
    fs::create_dir_all(&world.mac.data).unwrap();
    let local = world.mac.data.join(FILE_NAME);
    let bytes = {
        let _vault = vault_with(&local, SERVER);
        fs::read(&local).unwrap()
    };
    assert_eq!(world.mac.sync().problem, None);
    assert!(!local.exists());
    let second = world.folder.path().join("second");

    let sync = Syncer::start(world.mac.config(), || {}).unwrap();
    sync.set_repo(RepoSettings::new(&second));
    let state = wait_for(&sync, |state| state.repo.as_ref() == Some(&second));
    sync.quit().wait();

    assert_eq!(state.vault_file, second.join(FILE_NAME));
    assert_eq!(fs::read(second.join(FILE_NAME)).unwrap(), bytes);
    assert_eq!(fs::read(world.mac.repo.join(FILE_NAME)).unwrap(), bytes);
    assert!(!local.exists());
}

#[test]
fn clearing_a_repo_that_would_not_open_still_brings_the_vault_home() {
    let folder = TempDir::new().unwrap();
    let data = folder.path().join("data");
    let repo = folder.path().join("not-a-repo");
    fs::create_dir_all(&repo).unwrap();
    let held = repo.join(FILE_NAME);
    let bytes = {
        let _vault = vault_with(&held, SERVER);
        fs::read(&held).unwrap()
    };
    let sync = Syncer::start(
        SyncConfig {
            repo: Some(RepoSettings::new(&repo)),
            ..SyncConfig::new(&data)
        },
        || {},
    )
    .unwrap();
    wait_for(&sync, |state| state.problem.is_some());

    sync.clear_repo();

    let state = wait_for(&sync, |state| state.problem.is_none());
    sync.quit();
    assert_eq!(state.vault_file, data.join(FILE_NAME));
    assert_eq!(fs::read(data.join(FILE_NAME)).unwrap(), bytes);
}

#[test]
fn a_kill_while_moving_the_vault_leaves_it_in_both_places_and_the_next_sync_finishes() {
    let world = two_devices();
    fs::create_dir_all(&world.mac.data).unwrap();
    let local = world.mac.data.join(FILE_NAME);
    drop(vault_with(&local, SERVER));

    kill_at(&world.mac, "pass: committed");

    assert_eq!(
        fs::read(&local).unwrap(),
        fs::read(world.mac.repo.join(FILE_NAME)).unwrap()
    );
    assert_eq!(world.mac.sync().problem, None);
    assert!(!local.exists());
    assert!(world.mac.repo.join(FILE_NAME).exists());
}

#[test]
fn a_local_vault_merges_with_the_one_a_repo_already_holds() {
    let world = two_devices();
    let in_repo = world.mac.repo.join(FILE_NAME);
    let vault = vault_with(&in_repo, SERVER);
    assert_eq!(world.mac.sync().problem, None);
    world.ipad.sync();
    block_on(vault.set_password(EntryKey::new("two.example.org", "Bren"), "x".into())).unwrap();
    drop(vault);
    fs::create_dir_all(&world.ipad.data).unwrap();
    let local = world.ipad.data.join(FILE_NAME);
    fs::copy(&in_repo, &local).unwrap();

    assert_eq!(world.ipad.sync().problem, None);

    assert!(!local.exists());
    assert_eq!(
        fs::read(world.ipad.repo.join(FILE_NAME)).unwrap(),
        fs::read(in_repo).unwrap()
    );
}

fn hearth_recording() -> Vec<Bytes> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("session")
        .join("hearth.telnet");
    vec![Bytes::from(fs::read(path).unwrap())]
}

fn map_rooms(session: &Session) -> usize {
    session
        .stores()
        .read(|map: &Versioned<MapState>| map.value().rooms().len())
}

fn wait_until(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !ready() {
        assert!(Instant::now() < deadline, "it never happened");
        thread::sleep(Duration::from_millis(10));
    }
}

/// A real session: the core thread plays a recording, sync commits on the
/// disconnect, and a delete from another device reaches the session's map.
#[test]
fn a_session_syncs_at_its_end_and_its_map_follows_a_merge() {
    let world = two_devices();
    let session_data = world.mac.data.clone();
    let sync = Syncer::start(world.mac.config(), || {}).unwrap();
    wait_for(&sync, |state| state.last_sync.is_some() && !state.syncing);
    let session = Session::open(
        SessionConfig {
            sync: Some(sync.clone()),
            ..SessionConfig::new(&session_data, SERVER, 4000)
        },
        || {},
    )
    .unwrap();
    session.replay(hearth_recording());
    // The room the session opened with already shows on the map, so it's the
    // database the replay's rooms wait on.
    wait_until(|| world.mac.rooms().len() > 1);
    let played = world.mac.rooms();
    wait_until(|| {
        let pushed = |room: &String| remote_ever_held(&world.remote, room);
        played.iter().all(pushed)
    });

    // The other device deletes the room both hold.
    assert_eq!(world.ipad.sync().problem, None);
    let road = 1;
    world.ipad.apply(|batch| {
        batch.delete(crate::persist::Key::Room(road));
    });
    assert_eq!(world.ipad.sync().problem, None);

    sync.foreground();
    wait_until(|| {
        session
            .stores()
            .read(|map: &Versioned<MapState>| map.value().room(road).is_none())
    });
    assert!(!world.mac.rooms().contains(&"A quiet road".to_owned()));
    session.quit().wait().unwrap();
    sync.quit().wait();
}

/// Two devices sharing two copies of a square, on a game with no room numbers.
/// Returns the copies, lower id first.
fn two_devices_with_duplicates() -> (TempDir, Device, Device, i64, i64) {
    let folder = TempDir::new().unwrap();
    let remote = folder.path().join("remote.git");
    Repository::init_bare(&remote).unwrap();
    let url = remote.to_str().unwrap();
    let mac = Device::new(folder.path(), "mac", url);
    let ipad = Device::new(folder.path(), "ipad", url);
    let (kept, retired) = ((1 << 40) + 1, (1 << 40) + 2);
    mac.apply(|batch| {
        batch.upsert(server("{}"));
        batch.upsert(room(kept, "Temple Square", "The Meadow"));
        batch.upsert(room(retired, "Temple Square", "The Meadow"));
    });
    assert_eq!(mac.sync().problem, None);
    assert_eq!(ipad.sync().problem, None);
    assert_eq!(mac.sync().problem, None);
    (folder, mac, ipad, kept, retired)
}

fn last_join(session: &Session) -> Option<LastJoin> {
    session
        .stores()
        .read(|map: &Versioned<MapState>| map.value().last_join().cloned())
}

fn session_on(device: &Device) -> Session {
    Session::open(SessionConfig::new(&device.data, SERVER, 4000), || {}).unwrap()
}

fn wait_for_answer(session: &Session, attempt: u64) -> LastJoin {
    wait_until(|| last_join(session).is_some_and(|last| last.attempt == attempt));
    last_join(session).unwrap()
}

/// A device syncs until nothing new comes, and a pass after that makes no
/// commit: a merge that keeps rewriting itself fails here.
fn settle(mac: &Device, ipad: &Device) {
    for _ in 0..3 {
        assert_eq!(mac.sync().problem, None);
        assert_eq!(ipad.sync().problem, None);
    }
    let tips = (mac.tip(), ipad.tip());
    assert_eq!(tips.0, tips.1);
    assert_eq!(mac.sync().problem, None);
    assert_eq!(ipad.sync().problem, None);
    assert_eq!((mac.tip(), ipad.tip()), tips, "a pass made a commit");
}

fn map_of(device: &Device) -> crate::persist::MapRows {
    device.database().reader().unwrap().map().unwrap()
}

/// Real sessions on real devices: the player joins, the join reaches the other
/// device with no commit left to make, and Undo brings both rooms back there.
#[test]
fn a_join_through_a_session_syncs_and_its_undo_follows() {
    let (_folder, mac, ipad, kept, retired) = two_devices_with_duplicates();
    mac.apply(|batch| {
        batch.upsert(RoomNote {
            room_id: retired,
            text: "Beggars at night".to_owned(),
            updated: Timestamp::now(),
        });
    });

    let session = session_on(&mac);
    wait_until(|| map_rooms(&session) == 2);
    session.edit_map(MapEdit::Join {
        first: retired,
        second: kept,
    });
    let answer = wait_for_answer(&session, 1);
    let Ok(JoinDone::Joined { created, label, .. }) = answer.outcome else {
        panic!("the join failed: {:?}", answer.outcome);
    };
    assert_eq!(label, "Joined two copies of Temple Square");
    wait_until(|| !map_holds(&session, retired));
    let history = session.map_history();
    let versions = history.versions().unwrap();
    let listed = versions.iter().find(|v| v.kind == VERSION_JOIN).unwrap();
    assert_eq!((listed.label.as_str(), listed.room_count), (&*label, 2));

    settle(&mac, &ipad);
    assert_eq!(map_of(&ipad).rooms.len(), 1);
    assert_eq!(map_of(&ipad).notes[0].room_id, kept);

    assert!(history.undo_join(created));
    assert_eq!(wait_for_answer(&session, 2).outcome, Ok(JoinDone::Undone));
    wait_until(|| map_holds(&session, retired));
    let left = history.versions().unwrap();
    assert!(left.iter().all(|version| version.kind != VERSION_JOIN));
    session.quit().wait().unwrap();

    settle(&mac, &ipad);
    assert_eq!(map_of(&ipad).rooms.len(), 2);
    let notes = map_of(&ipad).notes;
    assert_eq!((notes.len(), notes[0].room_id), (1, retired));
    assert_eq!(map_of(&mac).rooms, map_of(&ipad).rooms);
}

fn rooms_shown(session: &Session) -> Vec<Arc<MapRoom>> {
    session
        .stores()
        .read(|map: &Versioned<MapState>| map.value().rooms().cloned().collect())
}

/// The version's copies read differently, and only the retired room changes
/// with the restore, so its stamp beats the kept room's. What the player sees
/// must be what a reload finds.
#[test]
fn a_restore_across_a_join_shows_what_a_reload_finds() {
    let (_folder, mac, _ipad, kept, retired) = two_devices_with_duplicates();
    mac.apply(|batch| {
        batch.upsert(Room {
            updated: Timestamp(1),
            x: 40,
            ..room(retired, "The old square", "The Meadow")
        });
        batch.upsert(crate::persist::Sighting {
            room_id: retired,
            mob: "a beggar".to_owned(),
            updated: Timestamp(1),
        });
        batch.save_map_version("manual", "Before the join", Timestamp(1));
    });
    let reader = mac.database().reader().unwrap();
    let version = reader.map_versions().unwrap()[0].id;
    let session = session_on(&mac);
    wait_until(|| map_rooms(&session) == 2);
    session.edit_map(MapEdit::Join {
        first: kept,
        second: retired,
    });
    assert!(wait_for_answer(&session, 1).outcome.is_ok());
    wait_until(|| !map_holds(&session, retired));

    assert!(session.map_history().restore(version));
    wait_until(|| {
        session
            .stores()
            .read(|settings: &Versioned<SettingsState>| settings.value().map_restore.is_some())
    });
    let restored = session
        .stores()
        .read(|settings: &Versioned<SettingsState>| settings.value().map_restore.clone());
    assert_eq!(restored.unwrap().outcome, Ok(version));
    let shown = rooms_shown(&session);
    session.quit().wait().unwrap();

    let reloaded = session_on(&mac);
    wait_until(|| map_rooms(&reloaded) > 0);
    assert_eq!(rooms_shown(&reloaded), shown);
    assert_eq!(shown.len(), 1, "the rooms stay joined");
    assert!(shown[0].sightings.contains_key("a beggar"));
    reloaded.quit().wait().unwrap();
}

/// The other device was playing on the retired room when the join came in.
#[test]
fn a_session_playing_on_a_retired_room_lands_on_the_kept_room() {
    let (_folder, mac, ipad, kept, retired) = two_devices_with_duplicates();
    let meta = |key: String, value: String| crate::persist::Meta {
        key,
        value,
        updated: Timestamp::now(),
    };
    ipad.apply(|batch| {
        batch.upsert(meta(CURRENT_ROOM.to_owned(), retired.to_string()));
    });
    let sync = Syncer::start(ipad.config(), || {}).unwrap();
    wait_for(&sync, |state| state.last_sync.is_some() && !state.syncing);
    let session = Session::open(
        SessionConfig {
            sync: Some(sync.clone()),
            ..SessionConfig::new(&ipad.data, SERVER, 4000)
        },
        || {},
    )
    .unwrap();
    wait_until(|| map_rooms(&session) == 2);

    let joining = session_on(&mac);
    wait_until(|| map_rooms(&joining) == 2);
    joining.edit_map(MapEdit::Join {
        first: kept,
        second: retired,
    });
    assert!(wait_for_answer(&joining, 1).outcome.is_ok());
    joining.quit().wait().unwrap();
    assert_eq!(mac.sync().problem, None);

    sync.foreground();
    wait_until(|| !map_holds(&session, retired));
    let whereabouts = session
        .stores()
        .read(|map: &Versioned<MapState>| map.value().whereabouts());
    assert_eq!(whereabouts.room, Some(kept));
    session.quit().wait().unwrap();
    sync.quit().wait();
}

#[test]
fn a_join_refuses_a_room_that_is_not_on_the_map() {
    let (_folder, mac, _ipad, kept, _retired) = two_devices_with_duplicates();
    let session = session_on(&mac);
    wait_until(|| map_rooms(&session) == 2);
    session.edit_map(MapEdit::Join {
        first: kept,
        second: kept + 1_000,
    });
    assert!(wait_for_answer(&session, 1).outcome.is_err());
    assert_eq!(map_rooms(&session), 2);
    session.quit().wait().unwrap();
}

/// The game saying the player is in room `number`, as GMCP.
fn in_room(number: i64) -> Bytes {
    let info = format!(
        r#"Room.Info {{"num": {number}, "name": "Room {number}", "zone": "Fields",
            "environment": "field", "exits": {{"n": {}}}, "details": []}}"#,
        number + 1
    );
    let mut bytes = vec![255, 251, 201, 255, 250, 201];
    bytes.extend_from_slice(info.as_bytes());
    bytes.extend_from_slice(&[255, 240]);
    Bytes::from(bytes)
}

fn map_holds(session: &Session, room: i64) -> bool {
    session
        .stores()
        .read(|map: &Versioned<MapState>| map.value().room(room).is_some())
}

/// Opens a session at the worst moment: after a merge read the session list
/// and before its import commits, so the map it loads still has a room the
/// merge deletes. Its first rows are about that room, and they mustn't fail
/// the batch that holds its new room.
#[test]
fn a_session_opened_while_a_merge_lands_follows_it_and_keeps_its_rows() {
    let world = two_devices();
    world.ipad.apply(|batch| {
        batch.delete(crate::persist::Key::Room(1));
        batch.upsert(room(5_000, "A new road", "The Meadow"));
    });
    assert_eq!(world.ipad.sync().problem, None);

    let mac = &world.mac;
    let sync = Syncer::start(SyncConfig::new(&mac.data), || {}).unwrap();
    let git = git::Repo::open_or_init(&mac.repo).unwrap();
    let files = crate::repo::Repo::new(&mac.repo);
    let database = mac.database();
    let writer = database.writer().unwrap();
    let stores = [merge::Store {
        server_id: SERVER,
        database: &database,
        writer: &writer,
    }];
    let theirs = git.fetch("origin", "main").unwrap().unwrap();
    let merging = merge::prepare(&git, &files, theirs).unwrap().unwrap();

    let held = pass::Held::hold(&sync.sessions, &[SERVER]).unwrap();
    let session = Session::open(
        SessionConfig {
            sync: Some(sync.clone()),
            ..SessionConfig::new(&mac.data, SERVER, 4000)
        },
        || {},
    )
    .unwrap();
    wait_until(|| map_holds(&session, 1));
    session.replay(vec![in_room(1), in_room(7_000)]);
    wait_until(|| map_holds(&session, 7_000));

    merge::land(&git, &files, &merging, &stores).unwrap();
    assert!(!mac.rooms().contains(&"Room 7000".to_owned()), "rows wait");
    held.release(merging.retired());

    wait_until(|| !map_holds(&session, 1) && map_holds(&session, 5_000));
    assert_eq!(
        mac.rooms(),
        ["A new road", "Room 7000"],
        "the deleted room stays deleted, and the rows around it landed"
    );

    session.replay(vec![in_room(7_001)]);
    wait_until(|| map_holds(&session, 7_001));
    wait_until(|| mac.rooms().contains(&"Room 7001".to_owned()));
    let notices = session
        .stores()
        .read(|settings: &Versioned<SettingsState>| settings.value().notices.clone());
    assert_eq!(notices, Vec::<String>::new());
    session.quit().wait().unwrap();
    sync.quit().wait();
}

#[test]
fn a_session_that_attaches_after_a_merge_is_not_held() {
    let world = two_devices();
    let sync = Syncer::start(SyncConfig::new(&world.mac.data), || {}).unwrap();
    pass::Held::hold(&sync.sessions, &[SERVER])
        .unwrap()
        .release(&merge::Retired::new());

    let session = Session::open(
        SessionConfig {
            sync: Some(sync.clone()),
            ..SessionConfig::new(&world.mac.data, SERVER, 4000)
        },
        || {},
    )
    .unwrap();
    session.replay(vec![in_room(7_000)]);

    wait_until(|| world.mac.rooms().contains(&"Room 7000".to_owned()));
    session.quit().wait().unwrap();
    sync.quit().wait();
}

/// The merge is big enough to run while the session opens. A session that
/// opens after it, or before, sees the same result, so this can't fail by
/// chance; it fails when the merge misses a session that opened during it.
#[test]
fn a_session_opened_while_a_sync_runs_follows_that_syncs_merge() {
    let world = two_devices();
    world.mac.apply(|batch| {
        for id in 10..3_000 {
            batch.upsert(room(id, "A road", "The Long Road"));
        }
    });
    assert_eq!(world.mac.sync().problem, None);
    assert_eq!(world.ipad.sync().problem, None);
    world.ipad.apply(|batch| {
        batch.delete(crate::persist::Key::Room(1));
        batch.upsert(room(5_000, "A new road", "The Long Road"));
    });
    assert_eq!(world.ipad.sync().problem, None);

    let sync = Syncer::start(world.mac.config(), || {}).unwrap();
    wait_for(&sync, |state| state.syncing);
    let session = Session::open(
        SessionConfig {
            sync: Some(sync.clone()),
            ..SessionConfig::new(&world.mac.data, SERVER, 4000)
        },
        || {},
    )
    .unwrap();
    wait_for(&sync, |state| state.last_sync.is_some() && !state.syncing);

    wait_until(|| {
        session.stores().read(|map: &Versioned<MapState>| {
            map.value().room(1).is_none() && map.value().room(5_000).is_some()
        })
    });
    session.quit().wait().unwrap();
    sync.quit().wait();
}

impl Device {
    /// Puts a plugin's files in the data folder, as an install does.
    fn install_plugin(&self, id: &str, files: &[(&str, &str)]) {
        let folder = self.data.join("plugins").join(id);
        let _ = fs::remove_dir_all(&folder);
        for (name, text) in files {
            fs::create_dir_all(&folder).unwrap();
            fs::write(folder.join(name), text).unwrap();
        }
        let record = self.data.join("plugins").join(format!("{id}.json"));
        fs::write(
            record,
            format!("{{\"from\": \"registry\", \"version\": \"{id}\"}}"),
        )
        .unwrap();
    }

    /// Each file of `id` in the data folder, with what it holds.
    fn plugin(&self, id: &str) -> Vec<(String, String)> {
        let Ok(entries) = fs::read_dir(self.data.join("plugins").join(id)) else {
            return Vec::new();
        };
        let mut files: Vec<(String, String)> = entries
            .flatten()
            .map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                (name, fs::read_to_string(entry.path()).unwrap())
            })
            .collect();
        files.sort();
        files
    }
}

#[test]
fn a_plugin_installed_on_one_device_reaches_the_other_and_so_does_its_removal() {
    let world = two_devices();
    world
        .mac
        .install_plugin("tells", &[("plugin.toml", "tells")]);
    assert_eq!(world.mac.sync().problem, None);

    assert_eq!(world.ipad.sync().problem, None);
    assert_eq!(
        world.ipad.plugin("tells"),
        [("plugin.toml".into(), "tells".into())]
    );
    assert!(world.ipad.data.join("plugins/tells.json").exists());

    fs::remove_dir_all(world.mac.data.join("plugins/tells")).unwrap();
    fs::remove_file(world.mac.data.join("plugins/tells.json")).unwrap();
    assert_eq!(world.mac.sync().problem, None);
    assert_eq!(world.ipad.sync().problem, None);

    assert!(world.ipad.plugin("tells").is_empty());
    assert!(!world.ipad.data.join("plugins/tells.json").exists());
}

#[test]
fn two_devices_updating_one_plugin_keep_one_whole_version_of_it() {
    let world = two_devices();
    world
        .mac
        .install_plugin("tells", &[("a.toml", "1"), ("b.toml", "1")]);
    assert_eq!(world.mac.sync().problem, None);
    assert_eq!(world.ipad.sync().problem, None);

    world
        .mac
        .install_plugin("tells", &[("a.toml", "2"), ("b.toml", "2")]);
    world
        .ipad
        .install_plugin("tells", &[("a.toml", "3"), ("c.toml", "3")]);
    assert_eq!(world.mac.sync().problem, None);
    assert_eq!(world.ipad.sync().problem, None);
    assert_eq!(world.mac.sync().problem, None);

    let kept = world.mac.plugin("tells");
    let whole = |version: &str, names: [&str; 2]| -> Vec<(String, String)> {
        names
            .map(|name| (name.to_owned(), version.to_owned()))
            .into()
    };
    assert!(
        kept == whole("2", ["a.toml", "b.toml"]) || kept == whole("3", ["a.toml", "c.toml"]),
        "{kept:?}"
    );
    assert_eq!(world.ipad.plugin("tells"), kept);
}

#[test]
fn a_plugin_from_another_device_waits_until_this_one_trusts_it() {
    let world = two_devices();
    let manifest = "id = \"tells\"\nname = \"Tells\"\nversion = \"1.0.0\"\napi = 1\n\
                    license = \"MIT\"\nlayer = \"default\"\n";
    world
        .mac
        .install_plugin("tells", &[("plugin.toml", manifest)]);
    let on_mac = crate::registry::InstalledPlugins::in_data_folder(&world.mac.data);
    on_mac.trust("tells").unwrap();
    assert_eq!(on_mac.load().plugins.len(), 1);
    assert_eq!(world.mac.sync().problem, None);

    assert_eq!(world.ipad.sync().problem, None);

    let on_ipad = crate::registry::InstalledPlugins::in_data_folder(&world.ipad.data);
    let loaded = on_ipad.load();
    assert!(loaded.plugins.is_empty(), "the Mac's trust didn't travel");
    assert_eq!(loaded.arrived.len(), 1);
}

#[test]
fn a_folder_with_no_record_is_the_player_s_own_only_where_it_was_made() {
    let world = two_devices();
    let made_here = world.mac.data.join("plugins/mine");
    fs::create_dir_all(&made_here).unwrap();
    let manifest = "id = \"mine\"\nname = \"Mine\"\nversion = \"1.0.0\"\napi = 1\n\
                    license = \"MIT\"\nlayer = \"default\"\n";
    fs::write(made_here.join("plugin.toml"), manifest).unwrap();
    let on_mac = crate::registry::InstalledPlugins::in_data_folder(&world.mac.data);
    assert_eq!(
        on_mac.load().plugins.len(),
        1,
        "made on the Mac, so the Mac's own"
    );
    assert_eq!(world.mac.sync().problem, None);

    assert_eq!(world.ipad.sync().problem, None);

    let on_ipad = crate::registry::InstalledPlugins::in_data_folder(&world.ipad.data);
    let loaded = on_ipad.load();
    assert!(
        loaded.plugins.is_empty(),
        "the repo brought it, with no record"
    );
    assert_eq!(loaded.arrived.len(), 1);
    assert_eq!(
        on_mac.load().plugins.len(),
        1,
        "the round trip left it the Mac's"
    );
}

#[test]
fn a_folder_that_was_the_player_s_own_and_changed_by_sync_waits_again() {
    let world = two_devices();
    let manifest = |version: &str| {
        format!(
            "id = \"mine\"\nname = \"Mine\"\nversion = \"{version}\"\napi = 1\n\
             license = \"MIT\"\nlayer = \"default\"\n"
        )
    };
    let on_ipad = world.ipad.data.join("plugins/mine");
    fs::create_dir_all(&on_ipad).unwrap();
    fs::write(on_ipad.join("plugin.toml"), manifest("1.0.0")).unwrap();
    let ipad = crate::registry::InstalledPlugins::in_data_folder(&world.ipad.data);
    assert_eq!(ipad.load().plugins.len(), 1);
    assert_eq!(world.ipad.sync().problem, None);
    assert_eq!(world.mac.sync().problem, None);

    // Someone with the repo changes it under the same name.
    fs::write(
        world.mac.data.join("plugins/mine/plugin.toml"),
        manifest("6.6.6"),
    )
    .unwrap();
    assert_eq!(world.mac.sync().problem, None);
    assert_eq!(world.ipad.sync().problem, None);

    assert!(ipad.load().plugins.is_empty());
}

#[test]
fn approvals_never_leave_the_device() {
    let world = two_devices();
    world
        .mac
        .install_plugin("extras", &[("plugin.toml", "extras")]);
    let extras = crate::plugin::Plugin::from_files(|_| {
        Ok(
            "id = \"extras\"\nname = \"Extras\"\nversion = \"1.0.0\"\napi = 1\n\
            license = \"MIT\"\nlayer = \"default\"\n"
                .to_owned(),
        )
    })
    .unwrap();
    crate::registry::Approvals::in_data_folder(&world.mac.data)
        .approve(&extras)
        .unwrap();

    assert_eq!(world.mac.sync().problem, None);

    assert!(remote_files(&world.remote).contains(&"extras.json".to_owned()));
    let fingerprint = crate::registry::fingerprint(&extras);
    assert!(!remote_ever_held(&world.remote, &fingerprint));
}
