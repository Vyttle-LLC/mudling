//! `map`: measures the automap against the PRD §6 move budget, headless.
//!
//! ```text
//! cargo run --release -p mudling-bench --bin map -- \
//!     [--logs <folder of session logs>] [--world <prototype world file>] \
//!     [--check] [--json <report.json>]
//! ```
//!
//! A move is a Room.Info read, the map updated, and the changed rows queued
//! on the persistence writer. It's timed on a synthetic 10,000-room map, and
//! on the prototype's session logs replayed in order when `--logs` is given.
//! Logs hold other players' chat, so they're read at runtime and never
//! committed.
//!
//! `--world` compares the replayed map with the prototype's saved world
//! (informational): the file is copied to a temporary folder first, and only
//! the copy is read.
//!
//! `--check` exits non-zero when the move budget is missed.

use mudling_core::AnsiParser;
use mudling_core::map::{AreaName, Direction, LiveMap, MapRules, RoomIdentity, RoomInfo};
use mudling_core::persist::{Database, Timestamp, Writer};
use mudling_core::stats::Summary;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

/// PRD §6: a move costs at most 1 ms at p99.
const MOVE_BUDGET_US: f64 = 1_000.0;
const GRID_SIDE: i64 = 100;
const GRID_WALK_MOVES: usize = 5_000;
/// The prototype's replay counts a second per room.
const ROOM_INFO_MS: i64 = 1_000;
/// Between logs: long enough for any pending move to expire.
const SESSION_GAP_MS: i64 = 3_600_000;
/// Rooms listed by name when the maps differ.
const EXAMPLES: usize = 15;

#[derive(Serialize)]
struct Report {
    grid: GridReport,
    replay: Option<ReplayReport>,
    comparison: Option<Comparison>,
    move_budget_us: f64,
    within_budget: bool,
}

#[derive(Serialize)]
struct GridReport {
    rooms: usize,
    links: usize,
    /// Walking every row and column once: half new rooms, half revisits.
    mapping: Summary,
    /// Wandering the finished map.
    walk: Summary,
    failed_batches: usize,
}

#[derive(Serialize)]
struct ReplayReport {
    logs: usize,
    room_infos: usize,
    moves: Summary,
    rooms: usize,
    links: usize,
    failed_batches: usize,
}

#[derive(Serialize)]
struct Comparison {
    prototype_rooms: usize,
    prototype_links: usize,
    replay_rooms: usize,
    replay_links: usize,
    /// Rooms (by area, name and description) the prototype has more of.
    only_in_prototype: usize,
    only_in_replay: usize,
    examples_only_in_prototype: Vec<String>,
    examples_only_in_replay: Vec<String>,
}

fn main() -> ExitCode {
    let mut check = false;
    let mut json = None;
    let mut logs = None;
    let mut world = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--check" => check = true,
            "--json" => json = args.next(),
            "--logs" => logs = args.next().map(PathBuf::from),
            "--world" => world = args.next().map(PathBuf::from),
            _ => {
                eprintln!(
                    "usage: map [--logs <folder>] [--world <file>] [--check] [--json <report.json>]"
                );
                return ExitCode::from(2);
            }
        }
    }

    let folder = tempfile::tempdir().expect("a temporary folder");
    let grid = measure_grid(folder.path());
    let replayed = match logs.as_deref().map(|logs| replay(logs, folder.path())) {
        Some(Ok(replayed)) => Some(replayed),
        Some(Err(error)) => {
            eprintln!("couldn't read the logs: {error}");
            return ExitCode::from(2);
        }
        None => None,
    };
    let comparison = match (&replayed, world.as_deref()) {
        (Some((_, map)), Some(world)) => match compare(map, world, folder.path()) {
            Ok(comparison) => Some(comparison),
            Err(error) => {
                eprintln!("couldn't read the world file: {error}");
                return ExitCode::from(2);
            }
        },
        _ => None,
    };
    let replay = replayed.map(|(report, _)| report);
    let within_budget = grid.mapping.p99_us <= MOVE_BUDGET_US
        && grid.walk.p99_us <= MOVE_BUDGET_US
        && replay
            .as_ref()
            .is_none_or(|replay| replay.moves.p99_us <= MOVE_BUDGET_US);
    let report = Report {
        grid,
        replay,
        comparison,
        move_budget_us: MOVE_BUDGET_US,
        within_budget,
    };
    print_report(&report);
    if let Some(path) = json {
        let text = serde_json::to_string_pretty(&report).expect("the report serializes");
        if let Err(error) = std::fs::write(&path, text) {
            eprintln!("couldn't write {path}: {error}");
            return ExitCode::from(2);
        }
    }
    if check && !report.within_budget {
        eprintln!("the move budget was missed");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// A timed move: the Room.Info read, the map updated, the batch queued.
fn timed_move(map: &mut LiveMap, writer: &Writer, json: &str, now: Timestamp) -> Duration {
    let started = Instant::now();
    map.room_info(json, now);
    if let Some(commit) = map.commit(now) {
        writer.submit(commit.batch).expect("the writer runs");
    }
    started.elapsed()
}

/// Maps a grid of uniquely named rooms by walking every row and column,
/// then wanders it.
fn measure_grid(folder: &Path) -> GridReport {
    let database = Database::open(folder, "grid").expect("the database opens");
    let writer = database.writer().expect("the writer starts");
    let mut map = LiveMap::new(Box::new(GridRules));
    let mut clock = 0;
    let mut mapping = Vec::new();
    let mut visit = |map: &mut LiveMap, direction: Option<&str>, x: i64, y: i64| {
        clock += ROOM_INFO_MS;
        let now = Timestamp(clock);
        if let Some(direction) = direction {
            map.sent(direction, now);
        }
        timed_move(map, &writer, &grid_room(x, y), now)
    };

    mapping.push(visit(&mut map, None, 0, 0));
    for y in 0..GRID_SIDE {
        let east = y % 2 == 0;
        for step in 1..GRID_SIDE {
            let x = if east { step } else { GRID_SIDE - 1 - step };
            mapping.push(visit(&mut map, Some(if east { "e" } else { "w" }), x, y));
        }
        if y + 1 < GRID_SIDE {
            let x = if east { GRID_SIDE - 1 } else { 0 };
            mapping.push(visit(&mut map, Some("n"), x, y + 1));
        }
    }
    let (mut x, mut y) = (0, GRID_SIDE - 1);
    for column in 0..GRID_SIDE {
        let south = column % 2 == 0;
        for _ in 1..GRID_SIDE {
            y += if south { -1 } else { 1 };
            mapping.push(visit(&mut map, Some(if south { "s" } else { "n" }), x, y));
        }
        if column + 1 < GRID_SIDE {
            x += 1;
            mapping.push(visit(&mut map, Some("e"), x, y));
        }
    }

    // A wander that stays on the map, from a fixed seed.
    let mut seed: u64 = 0x5eed;
    let mut walk = Vec::with_capacity(GRID_WALK_MOVES);
    while walk.len() < GRID_WALK_MOVES {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let (direction, dx, dy) =
            [("n", 0, 1), ("e", 1, 0), ("s", 0, -1), ("w", -1, 0)][(seed >> 33) as usize % 4];
        let (nx, ny) = (x + dx, y + dy);
        if !(0..GRID_SIDE).contains(&nx) || !(0..GRID_SIDE).contains(&ny) {
            continue;
        }
        (x, y) = (nx, ny);
        walk.push(visit(&mut map, Some(direction), x, y));
    }
    let failed_batches = flush_failures(&writer);
    GridReport {
        rooms: map.room_count(),
        links: map.rooms().map(|room| room.links.len()).sum(),
        mapping: Summary::of(&mapping).expect("moves were timed"),
        walk: Summary::of(&walk).expect("moves were timed"),
        failed_batches,
    }
}

fn grid_room(x: i64, y: i64) -> String {
    format!(
        r#"{{"name": "Plain {x} {y}", "zone": "The Grid", "environment": "field", "exits": {{"n": "O", "e": "O", "s": "O", "w": "O"}}}}"#
    )
}

/// The grid's rooms have no numbers: told apart by name.
struct GridRules;

impl MapRules for GridRules {
    fn identity(&self) -> RoomIdentity {
        RoomIdentity::Text
    }
}

/// Replays every log in the folder, oldest first, through one map.
fn replay(logs: &Path, folder: &Path) -> std::io::Result<(ReplayReport, LiveMap)> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(logs)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|extension| extension == "log"))
        .collect();
    paths.sort();
    let database = Database::open(folder, "replay").expect("the database opens");
    let writer = database.writer().expect("the writer starts");
    let mut map = LiveMap::new(Box::new(PrototypeGameRules));
    let mut moves = Vec::new();
    let mut clock = 0;
    for path in &paths {
        let bytes = std::fs::read(path)?;
        let log = String::from_utf8_lossy(&bytes);
        let mut ansi = AnsiParser::default();
        clock += SESSION_GAP_MS;
        for raw in log.split('\n') {
            let line = raw.trim_matches('\r');
            if let Some(message) = line.strip_prefix("[GMCP] ") {
                let (package, json) = message.split_once(' ').unwrap_or((message, ""));
                if package == "Room.Info" {
                    clock += ROOM_INFO_MS;
                    moves.push(timed_move(&mut map, &writer, json, Timestamp(clock)));
                } else {
                    map.gmcp(package, json, Timestamp(clock));
                }
            } else if let Some(note) = line.strip_prefix("[MAP] ") {
                let command = note.strip_prefix("move ").unwrap_or(note);
                map.sent(command, Timestamp(clock));
            } else if !is_note(line) {
                let text = ansi.parse_line(line).text;
                map.line(&text, Timestamp(clock));
            }
        }
    }
    let failed_batches = flush_failures(&writer);
    // No moves would pass any budget: that's a wrong folder, not a result.
    let Some(summary) = Summary::of(&moves) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "no Room.Info in {} log(s) in {}",
                paths.len(),
                logs.display()
            ),
        ));
    };
    let report = ReplayReport {
        logs: paths.len(),
        room_infos: moves.len(),
        moves: summary,
        rooms: map.room_count(),
        links: map.rooms().map(|room| room.links.len()).sum(),
        failed_batches,
    };
    Ok((report, map))
}

/// The prototype's own notes in a log, which aren't the game's text.
fn is_note(line: &str) -> bool {
    ["[telnet] ", "[auto-login] ", "[disconnected] "]
        .iter()
        .any(|prefix| line.starts_with(prefix))
        || line == "[connected]"
}

fn flush_failures(writer: &Writer) -> usize {
    match writer.flush() {
        Ok(()) => 0,
        Err(mudling_core::persist::Error::BatchesFailed(failures)) => {
            eprintln!(
                "{} batch(es) failed, the latest: {:?}",
                failures.count(),
                failures.errors.back()
            );
            failures.count()
        }
        Err(error) => panic!("the writer stopped: {error}"),
    }
}

/// Stands in for the prototype game's plugin, which comes with the plugin
/// host: its messages and area names live here, never in the core.
struct PrototypeGameRules;

impl MapRules for PrototypeGameRules {
    fn identity(&self) -> RoomIdentity {
        RoomIdentity::Text
    }

    /// "{ 1   4} Builder    Area Name": a level range, the builder, then
    /// the name, separated by two or more spaces.
    fn area_name(&self, zone: &str) -> AreaName {
        let (levels, rest) = match zone.strip_prefix('{').and_then(|rest| rest.split_once('}')) {
            Some((levels, rest)) => {
                let bounds: Vec<&str> = levels.split_whitespace().collect();
                (Some(bounds.join("–")), rest)
            }
            None => (None, zone),
        };
        let name = rest
            .split("  ")
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .last()
            .unwrap_or(rest)
            .to_owned();
        AreaName { name, levels }
    }

    fn failed_move(&self, line: &str) -> bool {
        line.contains("No way!") && line.contains("You are still fighting!")
            || line.contains("You are too exhausted")
    }

    fn fled(&self, line: &str) -> Option<Direction> {
        let rest = &line[line.find("You flee ")? + "You flee ".len()..];
        Direction::parse(rest.split(|c: char| !c.is_alphabetic()).next()?)
            .filter(|direction| rest.starts_with(direction.name()))
    }

    fn dark_room(&self, info: &RoomInfo) -> bool {
        info.details.iter().any(|detail| detail == "dark")
    }

    fn aggressor(&self, line: &str) -> Option<String> {
        line.trim()
            .split_once(" GLARES at you and SNARLS!")
            .map(|(name, _)| name.to_owned())
    }

    fn exits_line(&self, line: &str) -> bool {
        line.starts_with("[Exits:")
    }
}

/// Compares the replayed map with a copy of the prototype's saved world.
fn compare(map: &LiveMap, world: &Path, folder: &Path) -> std::io::Result<Comparison> {
    let copy = folder.join("prototype-world.json");
    std::fs::copy(world, &copy)?;
    let saved: Value = serde_json::from_slice(&std::fs::read(&copy)?)?;
    let rooms = saved["map"]["rooms"]
        .as_object()
        .cloned()
        .unwrap_or_default();

    let mut prototype: BTreeMap<String, usize> = BTreeMap::new();
    let mut prototype_links = 0;
    for room in rooms.values() {
        let text = |key: &str| room[key].as_str().unwrap_or_default().to_owned();
        *prototype
            .entry(room_key(&text("zone"), &text("name"), &text("description")))
            .or_default() += 1;
        prototype_links += room["links"].as_object().map_or(0, |links| links.len());
    }
    let mut replayed: BTreeMap<String, usize> = BTreeMap::new();
    for room in map.rooms() {
        *replayed
            .entry(room_key(&room.zone, &room.name, &room.description))
            .or_default() += 1;
    }

    let surplus = |a: &BTreeMap<String, usize>, b: &BTreeMap<String, usize>| {
        let mut total = 0;
        let mut examples = Vec::new();
        for (key, count) in a {
            let extra = count.saturating_sub(b.get(key).copied().unwrap_or(0));
            if extra > 0 {
                total += extra;
                if examples.len() < EXAMPLES {
                    examples.push(format!("{extra} × {key}"));
                }
            }
        }
        (total, examples)
    };
    let (only_in_prototype, examples_only_in_prototype) = surplus(&prototype, &replayed);
    let (only_in_replay, examples_only_in_replay) = surplus(&replayed, &prototype);
    Ok(Comparison {
        prototype_rooms: rooms.len(),
        prototype_links,
        replay_rooms: map.room_count(),
        replay_links: map.rooms().map(|room| room.links.len()).sum(),
        only_in_prototype,
        only_in_replay,
        examples_only_in_prototype,
        examples_only_in_replay,
    })
}

fn room_key(zone: &str, name: &str, description: &str) -> String {
    format!("{zone} / {name} / {description}")
}

fn print_report(report: &Report) {
    let summary = |label: &str, summary: &Summary| {
        println!(
            "  {label:<22} p50 {:>7.1} µs  p99 {:>7.1} µs  max {:>8.1} µs  ({} moves)",
            summary.p50_us, summary.p99_us, summary.max_us, summary.samples
        );
    };
    let grid = &report.grid;
    println!(
        "Synthetic map: {} rooms, {} links, {} failed batches",
        grid.rooms, grid.links, grid.failed_batches
    );
    summary("mapping it", &grid.mapping);
    summary("wandering it", &grid.walk);
    if let Some(replay) = &report.replay {
        println!(
            "Replay: {} logs, {} Room.Info, {} rooms, {} links, {} failed batches",
            replay.logs, replay.room_infos, replay.rooms, replay.links, replay.failed_batches
        );
        summary("replayed moves", &replay.moves);
    }
    if let Some(comparison) = &report.comparison {
        println!(
            "Against the prototype's world: {} rooms / {} links there, {} / {} replayed",
            comparison.prototype_rooms,
            comparison.prototype_links,
            comparison.replay_rooms,
            comparison.replay_links
        );
        println!("  only in the prototype: {}", comparison.only_in_prototype);
        for example in &comparison.examples_only_in_prototype {
            println!("    {example}");
        }
        println!("  only in the replay: {}", comparison.only_in_replay);
        for example in &comparison.examples_only_in_replay {
            println!("    {example}");
        }
    }
    println!(
        "Move budget p99 ≤ {} µs: {}",
        report.move_budget_us,
        if report.within_budget {
            "met"
        } else {
            "MISSED"
        }
    );
}
