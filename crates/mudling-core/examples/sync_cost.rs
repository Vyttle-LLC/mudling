//! Prints what a sync costs at 10,000 rooms, over a bare repo in a temp folder.
//! Run it in release:
//!
//!   cargo run --release -p mudling-core --example sync_cost

use mudling_core::map::{LiveMap, PlainRules};
use mudling_core::persist::{Batch, Database, Exit, Room, Server, Timestamp};
use mudling_core::session::{Session, SessionConfig};
use mudling_core::store::Versioned;
use mudling_core::sync::{RepoSettings, SyncConfig, SyncState, Syncer};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

const HOST: &str = "mud.example.org";
const ROOMS: i64 = 10_000;
const ZONES: i64 = 100;
const DESCRIPTION: &str = "A plain stretch of road runs on between low stone walls, and \
    the fields beyond them lie quiet under a wide and empty sky.";

fn room(id: i64, name: &str) -> Room {
    Room {
        id,
        name: name.to_owned(),
        area: format!("Zone {}", id % ZONES),
        environment: "road".to_owned(),
        description: DESCRIPTION.to_owned(),
        x: id % 100,
        y: id / 100,
        z: 0,
        building: None,
        tangled: false,
        maze: false,
        updated: Timestamp::now(),
    }
}

fn seed(data: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let database = Database::open(&data.join("servers"), HOST)?;
    let writer = database.writer()?;
    let mut batch = Batch::new();
    batch.upsert(Server {
        id: HOST.to_owned(),
        name: "A MUD".to_owned(),
        host: HOST.to_owned(),
        port: 4000,
        settings: "{}".to_owned(),
        updated: Timestamp::now(),
    });
    for id in 1..=ROOMS {
        batch.upsert(room(id, &format!("Road {id}")));
        for direction in ["north", "south"] {
            batch.upsert(Exit {
                room_id: id,
                direction: direction.to_owned(),
                state: "open".to_owned(),
                updated: Timestamp::now(),
            });
        }
    }
    writer.submit(batch)?;
    writer.flush()?;
    Ok(())
}

fn change_one_room(data: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let database = Database::open(&data.join("servers"), HOST)?;
    let writer = database.writer()?;
    let mut batch = Batch::new();
    batch.upsert(room(7, "Road 7, repaved"));
    writer.submit(batch)?;
    writer.flush()?;
    Ok(())
}

fn state(sync: &Syncer) -> SyncState {
    sync.stores()
        .read(|state: &Versioned<SyncState>| state.value().clone())
}

/// Waits for the sync running since `before`'s state to finish.
fn wait(sync: &Syncer, before: Option<Timestamp>) -> Duration {
    let began = Instant::now();
    loop {
        let now = state(sync);
        assert_eq!(now.problem, None);
        if now.last_sync != before && !now.syncing {
            return began.elapsed();
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn start(data: &Path, repo: &Path) -> Result<Syncer, std::io::Error> {
    Syncer::start(
        SyncConfig {
            repo: Some(RepoSettings::new(repo)),
            ..SyncConfig::new(data)
        },
        || {},
    )
}

fn device(root: &Path, name: &str, remote: &Path) -> Result<(), Box<dyn std::error::Error>> {
    drop(mudling_core::git::Repo::open_or_init(&root.join(name))?);
    git2::Repository::open(root.join(name))?.remote("origin", remote.to_str().ok_or("path")?)?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let folder = tempfile::tempdir()?;
    let root = folder.path();
    let remote = root.join("remote.git");
    git2::Repository::init_bare(&remote)?;
    device(root, "mac", &remote)?;
    device(root, "ipad", &remote)?;
    let (mac_data, ipad_data) = (root.join("mac-data"), root.join("ipad-data"));
    seed(&mac_data)?;

    let mac = start(&mac_data, &root.join("mac"))?;
    println!("first sync, all files: {:?}", wait(&mac, None));

    let last = state(&mac).last_sync;
    mac.sync_now();
    println!("sync, nothing changed: {:?}", wait(&mac, last));

    let last = state(&mac).last_sync;
    change_one_room(&mac_data)?;
    mac.sync_now();
    println!("sync, one room changed: {:?}", wait(&mac, last));

    let ipad = start(&ipad_data, &root.join("ipad"))?;
    println!("new device, pulls everything: {:?}", wait(&ipad, None));

    let last = state(&mac).last_sync;
    change_one_room(&mac_data)?;
    mac.sync_now();
    wait(&mac, last);
    let last = state(&ipad).last_sync;
    change_one_room(&ipad_data)?;
    ipad.sync_now();
    println!("merge with the other device: {:?}", wait(&ipad, last));

    let reader = Database::open(&mac_data.join("servers"), HOST)?.reader()?;
    let began = Instant::now();
    let (map, marks, meta) = (reader.map()?, reader.mob_marks()?, reader.meta()?);
    let read = began.elapsed();
    let began = Instant::now();
    let live = LiveMap::load(Box::new(PlainRules), &map, &marks, &meta);
    println!("map reload: read {read:?}, rebuild {:?}", began.elapsed());
    drop(live);

    let began = Instant::now();
    let session = Session::open(SessionConfig::new(&mac_data, HOST, 4000), || {})?;
    session.quit().wait()?;
    println!("session open and quit, all of it: {:?}", began.elapsed());

    mac.quit().wait();
    ipad.quit().wait();
    Ok(())
}
