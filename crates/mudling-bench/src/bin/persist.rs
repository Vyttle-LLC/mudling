//! `persist`: measures persistence against the PRD §6 budgets, headless,
//! on synthetic maps in a temporary folder.
//!
//! ```text
//! cargo run --release -p mudling-bench --bin persist -- [--check] [--json <report.json>]
//! ```
//!
//! - A move's writes (a room, its exits, the mobs seen there): what
//!   queueing them costs the caller, at a speedwalk's pace (one move every
//!   100 ms) and in a burst, and whether the writer thread keeps up.
//! - Listing 500 map-history versions of a prototype-sized map.
//! - Reading a 10,000-room map at startup (informational).
//!
//! `--check` exits non-zero when a budget is missed.

use mudling_core::persist::{Batch, Database, Exit, Room, Sighting, Timestamp, Writer};
use mudling_core::stats::Summary;
use serde::Serialize;
use std::path::Path;
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

const ENQUEUE_BUDGET_US: f64 = 50.0;
const LIST_BUDGET_US: f64 = 20_000.0;
/// One move every 100 ms: a fast speedwalk.
const SPEEDWALK_INTERVAL: Duration = Duration::from_millis(100);
const PACED_MOVES: usize = 50;
const BURST_MOVES: usize = 5_000;
/// The prototype's real map had 1,381 rooms.
const HISTORY_MAP_ROOMS: i64 = 1_400;
const HISTORY_VERSIONS: usize = 500;
const LIST_ROUNDS: usize = 100;
const STARTUP_MAP_ROOMS: i64 = 10_000;
const STARTUP_ROUNDS: usize = 10;
const EXITS_PER_MOVE: usize = 4;
const SIGHTINGS_PER_MOVE: usize = 3;

#[derive(Serialize)]
struct Report {
    /// Caller's cost to build and queue one move's batch, paced and burst.
    enqueue: Summary,
    /// From queueing a paced move to its commit.
    paced_commit: Summary,
    burst_moves: usize,
    burst_drain_ms: f64,
    burst_moves_per_second: f64,
    history_versions: usize,
    history_map_rooms: i64,
    history_build_s: f64,
    /// A new reader's first list, connection opening included.
    list_cold_us: f64,
    list: Summary,
    startup_map_rooms: i64,
    startup_map_exits: usize,
    /// Opening the database and reading the whole map.
    startup_read: Summary,
    enqueue_budget_us: f64,
    list_budget_us: f64,
    enqueue_within_budget: bool,
    writer_keeps_up: bool,
    list_within_budget: bool,
}

fn main() -> ExitCode {
    let mut check = false;
    let mut json = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--check" => check = true,
            "--json" => json = args.next(),
            _ => {
                eprintln!("usage: persist [--check] [--json <report.json>]");
                return ExitCode::from(2);
            }
        }
    }

    let folder = tempfile::tempdir().expect("a temporary folder");
    let report = run(folder.path());
    print_report(&report);
    if let Some(path) = json {
        let text = serde_json::to_string_pretty(&report).expect("the report serializes");
        if let Err(error) = std::fs::write(&path, text) {
            eprintln!("couldn't write {path}: {error}");
            return ExitCode::from(2);
        }
    }
    if check
        && !(report.enqueue_within_budget && report.writer_keeps_up && report.list_within_budget)
    {
        eprintln!("a budget was missed");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn run(folder: &Path) -> Report {
    let (enqueue, paced_commit, burst_drain) = measure_moves(folder);
    let (history_build, list_cold, list) = measure_history(folder);
    let (startup_map_exits, startup_read) = measure_startup(folder);

    let enqueue = Summary::of(&enqueue).expect("moves were timed");
    let paced_commit = Summary::of(&paced_commit).expect("commits were timed");
    let list = Summary::of(&list).expect("lists were timed");
    let burst_moves_per_second = BURST_MOVES as f64 / burst_drain.as_secs_f64();
    Report {
        enqueue_within_budget: enqueue.p99_us <= ENQUEUE_BUDGET_US,
        writer_keeps_up: paced_commit.max_us < SPEEDWALK_INTERVAL.as_secs_f64() * 1e6
            && burst_moves_per_second > 1.0 / SPEEDWALK_INTERVAL.as_secs_f64(),
        list_within_budget: list.p99_us <= LIST_BUDGET_US
            && list_cold.as_secs_f64() * 1e6 <= LIST_BUDGET_US,
        enqueue,
        paced_commit,
        burst_moves: BURST_MOVES,
        burst_drain_ms: millis(burst_drain),
        burst_moves_per_second,
        history_versions: HISTORY_VERSIONS,
        history_map_rooms: HISTORY_MAP_ROOMS,
        history_build_s: history_build.as_secs_f64(),
        list_cold_us: list_cold.as_secs_f64() * 1e6,
        list,
        startup_map_rooms: STARTUP_MAP_ROOMS,
        startup_map_exits,
        startup_read: Summary::of(&startup_read).expect("startups were timed"),
        enqueue_budget_us: ENQUEUE_BUDGET_US,
        list_budget_us: LIST_BUDGET_US,
    }
}

/// Walks the map one room per move. Returns the caller's enqueue times, the
/// paced moves' commit times, and how long the writer took to drain a burst.
fn measure_moves(folder: &Path) -> (Vec<Duration>, Vec<Duration>, Duration) {
    let database = Database::open(folder, "moves").expect("the database opens");
    let writer = database.writer().expect("the writer starts");
    write_map(&writer, HISTORY_MAP_ROOMS);

    let mut enqueue = Vec::with_capacity(PACED_MOVES + BURST_MOVES);
    let mut paced_commit = Vec::with_capacity(PACED_MOVES);
    for step in 0..PACED_MOVES {
        let started = Instant::now();
        writer.submit(move_batch(step)).expect("the writer runs");
        enqueue.push(started.elapsed());
        writer.flush().expect("the move commits");
        let committed = started.elapsed();
        paced_commit.push(committed);
        thread::sleep(SPEEDWALK_INTERVAL.saturating_sub(committed));
    }

    let burst_started = Instant::now();
    for step in 0..BURST_MOVES {
        let started = Instant::now();
        writer
            .submit(move_batch(PACED_MOVES + step))
            .expect("the writer runs");
        enqueue.push(started.elapsed());
    }
    writer.flush().expect("the burst commits");
    (enqueue, paced_commit, burst_started.elapsed())
}

/// What one move writes: the room entered, its exits, and the mobs there.
fn move_batch(step: usize) -> Batch {
    let room_id = (step as i64 % HISTORY_MAP_ROOMS) + 1;
    let now = Timestamp::now();
    let mut batch = Batch::new();
    batch.upsert(room(room_id, now));
    for direction in ["north", "east", "south", "west"]
        .into_iter()
        .take(EXITS_PER_MOVE)
    {
        batch.upsert(Exit {
            room_id,
            direction: direction.to_owned(),
            door: None,
            doorway: false,
            to_room: None,
            updated: now,
        });
    }
    for mob in 0..SIGHTINGS_PER_MOVE {
        batch.upsert(Sighting {
            room_id,
            mob: format!("a wandering creature number {mob}"),
            updated: now,
        });
    }
    batch
}

/// Saves the versions, then times listing them.
fn measure_history(folder: &Path) -> (Duration, Duration, Vec<Duration>) {
    let database = Database::open(folder, "history").expect("the database opens");
    let build_started = Instant::now();
    {
        let writer = database.writer().expect("the writer starts");
        write_map(&writer, HISTORY_MAP_ROOMS);
        for version in 0..HISTORY_VERSIONS {
            let mut batch = Batch::new();
            batch.save_map_version(
                "autosave",
                &format!("Autosave {version}"),
                Timestamp(version as i64 * 60_000),
            );
            writer.submit(batch).expect("the writer runs");
        }
        writer.flush().expect("the versions save");
    }
    let build = build_started.elapsed();

    let cold_started = Instant::now();
    let reader = database.reader().expect("the reader opens");
    let listed = reader.map_versions().expect("the versions list");
    let cold = cold_started.elapsed();
    assert_eq!(listed.len(), HISTORY_VERSIONS);

    let lists = (0..LIST_ROUNDS)
        .map(|_| {
            let started = Instant::now();
            std::hint::black_box(reader.map_versions().expect("the versions list"));
            started.elapsed()
        })
        .collect();
    (build, cold, lists)
}

/// Times opening a database and reading its whole map, as at startup.
fn measure_startup(folder: &Path) -> (usize, Vec<Duration>) {
    let database = Database::open(folder, "startup").expect("the database opens");
    write_map(
        &database.writer().expect("the writer starts"),
        STARTUP_MAP_ROOMS,
    );
    let mut exits = 0;
    let reads = (0..STARTUP_ROUNDS)
        .map(|_| {
            let started = Instant::now();
            let database = Database::open(folder, "startup").expect("the database opens");
            let map = database
                .reader()
                .expect("the reader opens")
                .map()
                .expect("the map reads");
            let elapsed = started.elapsed();
            exits = map.exits.len();
            elapsed
        })
        .collect();
    (exits, reads)
}

/// A square grid of rooms, each linked to its east and north neighbours
/// both ways, with a few notes, features and sightings.
fn write_map(writer: &Writer, rooms: i64) {
    let side = (rooms as f64).sqrt().ceil() as i64;
    let now = Timestamp::now();
    let mut batch = Batch::new();
    for id in 1..=rooms {
        batch.upsert(room(id, now));
        let index = id - 1;
        let mut link = |direction: &str, back: &str, neighbour: i64| {
            batch.upsert(Exit {
                room_id: id,
                direction: direction.to_owned(),
                door: None,
                doorway: false,
                to_room: Some(neighbour),
                updated: now,
            });
            batch.upsert(Exit {
                room_id: neighbour,
                direction: back.to_owned(),
                door: None,
                doorway: false,
                to_room: Some(id),
                updated: now,
            });
        };
        if index % side + 1 < side && id < rooms {
            link("east", "west", id + 1);
        }
        if id + side <= rooms {
            link("north", "south", id + side);
        }
        if id % 50 == 0 {
            batch.upsert(Sighting {
                room_id: id,
                mob: "a patrolling guard".to_owned(),
                updated: now,
            });
        }
    }
    writer.submit(batch).expect("the writer runs");
    writer.flush().expect("the map saves");
}

fn room(id: i64, updated: Timestamp) -> Room {
    let side = 100;
    Room {
        id,
        name: format!("A winding path, part {id}"),
        area: format!("Area {}", id / 250),
        environment: "field".to_owned(),
        description: "Tall grass sways on either side of the path.".to_owned(),
        x: id % side,
        y: id / side,
        z: 0,
        building: None,
        tangled: false,
        maze: false,
        updated,
    }
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e3
}

fn print_report(report: &Report) {
    let verdict = |ok: bool| if ok { "ok" } else { "MISSED" };
    let line = |what: &str, s: &Summary, unit: f64, suffix: &str| {
        println!(
            "{what}: p50 {:.2} {suffix}, p99 {:.2} {suffix}, max {:.2} {suffix} ({} samples)",
            s.p50_us / unit,
            s.p99_us / unit,
            s.max_us / unit,
            s.samples
        );
    };
    line(
        "enqueue a move (room + 4 exits + 3 sightings)",
        &report.enqueue,
        1.0,
        "µs",
    );
    println!(
        "  budget p99 ≤ {} µs: {}",
        report.enqueue_budget_us,
        verdict(report.enqueue_within_budget)
    );
    line(
        "paced move, queued to committed (1 move / 100 ms)",
        &report.paced_commit,
        1e3,
        "ms",
    );
    println!(
        "burst of {} moves drained in {:.0} ms ({:.0} moves/s); writer keeps up with 10 moves/s: {}",
        report.burst_moves,
        report.burst_drain_ms,
        report.burst_moves_per_second,
        verdict(report.writer_keeps_up)
    );
    println!(
        "map history: {} versions of a {}-room map, saved in {:.1} s",
        report.history_versions, report.history_map_rooms, report.history_build_s
    );
    println!(
        "  first list on a new reader: {:.2} ms",
        report.list_cold_us / 1e3
    );
    line("  list", &report.list, 1e3, "ms");
    println!(
        "  budget ≤ {} ms: {}",
        report.list_budget_us / 1e3,
        verdict(report.list_within_budget)
    );
    line(
        &format!(
            "startup read, {} rooms + {} exits (open + read)",
            report.startup_map_rooms, report.startup_map_exits
        ),
        &report.startup_read,
        1e3,
        "ms",
    );
}
