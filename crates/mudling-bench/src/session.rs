//! The PRD §6 budgets, through the whole session: real telnet parsing, the
//! line pipeline, plugins, defaults, stores with a shell subscribed, and the
//! SQLite writer on its own thread.
//!
//! The [`Engine`] is driven directly, so each read gets its own timing: a
//! sample is one read fed in and everything it changed published.
//!
//! - **Whole line:** each source line as one read, with `Char.Vitals` and a
//!   prompt every few lines, as a game sends them.
//! - **One more plugin:** two sessions read each line in turn, one with the
//!   last game plugin and one without; the gate is the difference in their
//!   p99s. Its hooks run and its Map rules read every line, as in play.
//! - **A move:** a `Room.Info` through the session, the map updated, its
//!   rows queued for the writer and the stores written, walking a map up to
//!   10,000 rooms and then wandering it.
//! - **Memory:** the live Rust heap of the session and a shell's stores with
//!   that map and 100,000 lines. SQLite's and Luau's C heaps aren't counted.
//! - **Cold start:** the session opened on that map's database, as far as
//!   connected: no socket, so no network time.

use crate::plugin_host::PLUGIN_BUDGET_US;
use crate::{HEAP_BYTES, heap_mb, verdict};
use mudling_core::persist::Timestamp;
use mudling_core::plugin::{Layer, Plugin};
use mudling_core::session::{Engine, SessionConfig, Typing};
use mudling_core::stats::Summary;
use mudling_core::store::{StoreHub, Stores};
use mudling_core::telnet::command::{GA, IAC};
use mudling_core::telnet::{Bytes, gmcp};
use serde::Serialize;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const LINE_BUDGET_US: f64 = 20.0;
pub const MOVE_BUDGET_US: f64 = 1_000.0;
const MEMORY_BUDGET_MB: f64 = 200.0;
const COLD_START_BUDGET_US: f64 = 300_000.0;
const COLD_STARTS: usize = 10;
pub const HOST: &str = "bench.example";
pub const MIN_LINES: usize = 100_000;
/// A `Char.Vitals` and a prompt after this many lines.
const LINES_PER_PROMPT: usize = 4;
/// Lines between a shell's drains: its frame.
const LINES_PER_FRAME: usize = 8;
const GRID_SIDE: i64 = 100;
const WANDER_MOVES: i64 = 5_000;

#[derive(Serialize)]
pub struct Report {
    pub lines: usize,
    pub whole_line: Summary,
    pub without_game_plugin: Summary,
    pub with_game_plugin: Summary,
    pub one_more_plugin_p99_us: f64,
    pub moves: usize,
    pub mapping: Summary,
    pub wandering: Summary,
    pub memory_mb: f64,
    pub cold_start: Summary,
    pub line_budget_us: f64,
    pub plugin_budget_us: f64,
    pub move_budget_us: f64,
    pub memory_budget_mb: f64,
    pub cold_start_budget_us: f64,
    pub within_budget: bool,
}

pub fn run(source: &[String], plugins: &[Plugin]) -> Report {
    let count = source.len().max(MIN_LINES);
    let reads: Vec<Bytes> = source
        .iter()
        .cycle()
        .take(count)
        .enumerate()
        .map(|(index, line)| read(index, line))
        .collect();

    let whole_line = measure_lines(&reads, plugins);
    let (without_game_plugin, with_game_plugin) = measure_one_more_plugin(&reads, plugins);
    let one_more_plugin_p99_us = with_game_plugin.p99_us - without_game_plugin.p99_us;
    let Moves {
        mapping,
        wandering,
        memory_mb,
        cold_start,
    } = measure_world(&reads, plugins);
    Report {
        lines: count,
        within_budget: whole_line.p99_us <= LINE_BUDGET_US
            && one_more_plugin_p99_us <= PLUGIN_BUDGET_US
            && mapping.p99_us <= MOVE_BUDGET_US
            && wandering.p99_us <= MOVE_BUDGET_US
            && memory_mb <= MEMORY_BUDGET_MB
            && cold_start.p99_us <= COLD_START_BUDGET_US,
        whole_line,
        without_game_plugin,
        with_game_plugin,
        one_more_plugin_p99_us,
        moves: (GRID_SIDE * GRID_SIDE + WANDER_MOVES) as usize,
        mapping,
        wandering,
        memory_mb,
        cold_start,
        line_budget_us: LINE_BUDGET_US,
        plugin_budget_us: PLUGIN_BUDGET_US,
        move_budget_us: MOVE_BUDGET_US,
        memory_budget_mb: MEMORY_BUDGET_MB,
        cold_start_budget_us: COLD_START_BUDGET_US,
    }
}

pub fn print(report: &Report) {
    println!(
        "session over {} lines (telnet → plugins → stores, writer on its thread):",
        report.lines
    );
    println!(
        "  whole line: {}; budget p99 ≤ {} µs: {}",
        report.whole_line,
        report.line_budget_us,
        verdict(report.whole_line.p99_us <= report.line_budget_us)
    );
    println!(
        "  one more plugin: {} without, {} with; +{:.2} µs p99; budget ≤ {} µs: {}",
        report.without_game_plugin,
        report.with_game_plugin,
        report.one_more_plugin_p99_us,
        report.plugin_budget_us,
        verdict(report.one_more_plugin_p99_us <= report.plugin_budget_us)
    );
    println!(
        "  a move, {} of them over {} rooms: mapping {}; wandering {}; budget p99 ≤ {} µs: {}",
        report.moves,
        GRID_SIDE * GRID_SIDE,
        report.mapping,
        report.wandering,
        report.move_budget_us,
        verdict(
            report.mapping.p99_us <= report.move_budget_us
                && report.wandering.p99_us <= report.move_budget_us
        )
    );
    println!(
        "  memory with that map and {} lines: {:.1} MB of Rust heap; budget ≤ {} MB: {}",
        report.lines,
        report.memory_mb,
        report.memory_budget_mb,
        verdict(report.memory_mb <= report.memory_budget_mb)
    );
    println!(
        "  cold start to connected on that map: p50 {:.1} ms, p99 {:.1} ms; budget ≤ {} ms: {}",
        report.cold_start.p50_us / 1e3,
        report.cold_start.p99_us / 1e3,
        report.cold_start_budget_us / 1e3,
        verdict(report.cold_start.p99_us <= report.cold_start_budget_us)
    );
}

/// One line as the server sends it: every few, with a `Char.Vitals` and a
/// prompt ended by GA after it.
fn read(index: usize, line: &str) -> Bytes {
    let mut bytes = line.as_bytes().to_vec();
    bytes.extend_from_slice(b"\n\r");
    if index % LINES_PER_PROMPT == LINES_PER_PROMPT - 1 {
        let (hp, moves) = (index % 500, index % 97);
        gmcp::encode(
            "Char.Vitals",
            &format!(r#"{{"hp": {hp}, "maxhp": 500, "mv": {moves}}}"#),
            &mut bytes,
        );
        bytes.extend_from_slice(format!("<{hp}hp {moves}mv> ").as_bytes());
        bytes.extend_from_slice(&[IAC, GA]);
    }
    bytes.into()
}

/// The session's config, for a folder: every plugin given that a session
/// loads itself rather than as the built-in defaults, each turned on.
pub fn config(folder: &Path, plugins: Vec<Plugin>) -> SessionConfig {
    let plugins: Vec<Plugin> = plugins
        .into_iter()
        .filter(|plugin| plugin.layer() != Layer::Default)
        .collect();
    let enabled = plugins
        .iter()
        .filter(|plugin| plugin.layer() == Layer::Game)
        .map(|plugin| plugin.id().clone())
        .collect();
    SessionConfig {
        plugins,
        enabled,
        ..SessionConfig::new(folder, HOST, 4000)
    }
}

/// A shell's copy of the stores it shows.
pub fn subscribe(hub: &StoreHub) -> Stores {
    Stores {
        terminal: hub.subscribe(),
        vitals: hub.subscribe(),
        room: hub.subscribe(),
        map: hub.subscribe(),
        ..Stores::default()
    }
}

/// A connected session with a shell subscribed, in a folder of its own.
pub struct Bench {
    pub engine: Engine,
    pub hub: Arc<StoreHub>,
    pub shell: Stores,
    folder: tempfile::TempDir,
}

impl Bench {
    pub fn new(plugins: Vec<Plugin>) -> Bench {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let hub = Arc::new(StoreHub::new(|| {}));
        let shell = subscribe(&hub);
        let typing = Arc::new(Mutex::new(Typing::default()));
        let config = config(folder.path(), plugins);
        let mut engine = Engine::open(config, Arc::clone(&hub), typing).expect("the session opens");
        engine.start_connection().expect("the session connects");
        engine.connected();
        Bench {
            engine,
            hub,
            shell,
            folder,
        }
    }

    /// One read, through to the stores. Its bytes are made before the clock
    /// starts, as the socket read makes them.
    pub fn read(&mut self, bytes: Bytes, now: Timestamp) -> Duration {
        let started = Instant::now();
        self.engine.receive(bytes, now).expect("the stream is good");
        self.engine.settle(now);
        drop(self.engine.take_output());
        started.elapsed()
    }

    fn drain(&mut self) {
        if let Some(batch) = self.hub.drain() {
            self.shell.apply_batch(&batch);
        }
    }

    /// Ends the session, handing back its folder and how long the writer
    /// took to catch up.
    pub fn finish(self) -> (tempfile::TempDir, Duration) {
        let started = Instant::now();
        self.engine.finish().expect("every write reaches the disk");
        (self.folder, started.elapsed())
    }
}

fn measure_lines(reads: &[Bytes], plugins: &[Plugin]) -> Summary {
    let mut bench = Bench::new(plugins.to_vec());
    let mut samples = Vec::with_capacity(reads.len());
    for (index, read) in reads.iter().enumerate() {
        samples.push(bench.read(read.clone(), Timestamp(0)));
        if index % LINES_PER_FRAME == 0 {
            bench.drain();
        }
    }
    bench.finish();
    Summary::of(&samples).expect("lines were timed")
}

fn measure_one_more_plugin(reads: &[Bytes], plugins: &[Plugin]) -> (Summary, Summary) {
    let mut others = plugins.to_vec();
    if let Some(game) = plugins
        .iter()
        .rposition(|plugin| plugin.layer() == Layer::Game)
    {
        others.remove(game);
    }
    let mut without = Bench::new(others);
    let mut with = Bench::new(plugins.to_vec());
    let mut without_samples = Vec::with_capacity(reads.len());
    let mut with_samples = Vec::with_capacity(reads.len());
    for (index, read) in reads.iter().enumerate() {
        // Alternate which goes first, so neither always reads a warm line.
        if index % 2 == 0 {
            without_samples.push(without.read(read.clone(), Timestamp(0)));
            with_samples.push(with.read(read.clone(), Timestamp(0)));
        } else {
            with_samples.push(with.read(read.clone(), Timestamp(0)));
            without_samples.push(without.read(read.clone(), Timestamp(0)));
        }
        if index % LINES_PER_FRAME == 0 {
            without.drain();
            with.drain();
        }
    }
    without.finish();
    with.finish();
    (
        Summary::of(&without_samples).expect("lines were timed"),
        Summary::of(&with_samples).expect("lines were timed"),
    )
}

/// One room of the grid, as the game reports it.
fn room_info(x: i64, y: i64) -> Bytes {
    let id = y * GRID_SIDE + x;
    let mut exits = Vec::new();
    for (direction, dx, dy) in [("n", 0, -1), ("s", 0, 1), ("e", 1, 0), ("w", -1, 0)] {
        let (to_x, to_y) = (x + dx, y + dy);
        if (0..GRID_SIDE).contains(&to_x) && (0..GRID_SIDE).contains(&to_y) {
            exits.push(format!(r#""{direction}": {}"#, to_y * GRID_SIDE + to_x));
        }
    }
    let mut bytes = Vec::new();
    gmcp::encode(
        "Room.Info",
        &format!(
            r#"{{"num": {id}, "name": "Room {id}", "zone": "Grid", "exits": {{{}}}}}"#,
            exits.join(", ")
        ),
        &mut bytes,
    );
    bytes.into()
}

/// A walk over every room, along each row in turn.
fn serpentine() -> Vec<(i64, i64, &'static str)> {
    let mut walk = Vec::new();
    for y in 0..GRID_SIDE {
        let forward = y % 2 == 0;
        let row: Vec<i64> = if forward {
            (0..GRID_SIDE).collect()
        } else {
            (0..GRID_SIDE).rev().collect()
        };
        for (step, x) in row.into_iter().enumerate() {
            let direction = match (step, forward) {
                (0, _) => "south",
                (_, true) => "east",
                (_, false) => "west",
            };
            walk.push((x, y, direction));
        }
    }
    walk
}

struct Moves {
    mapping: Summary,
    wandering: Summary,
    memory_mb: f64,
    cold_start: Summary,
}

/// Builds the §6 world: maps 10,000 rooms and wanders them, timing each move,
/// then fills the terminal to weigh it all, and opens it again from the disk.
fn measure_world(reads: &[Bytes], plugins: &[Plugin]) -> Moves {
    let heap_before = HEAP_BYTES.load(Ordering::Relaxed);
    let mut bench = Bench::new(plugins.to_vec());
    let walk = serpentine();
    let mut mapping = Vec::with_capacity(walk.len());
    for (x, y, direction) in &walk {
        bench.engine.send(direction, Timestamp(0));
        mapping.push(bench.read(room_info(*x, *y), Timestamp(0)));
        bench.drain();
    }
    // Back and forth along the last row, over rooms already mapped.
    let mut wandering = Vec::with_capacity(WANDER_MOVES as usize);
    let y = GRID_SIDE - 1;
    for step in 0..WANDER_MOVES {
        let (x, direction) = if step % 2 == 0 {
            (1, "east")
        } else {
            (0, "west")
        };
        bench.engine.send(direction, Timestamp(0));
        wandering.push(bench.read(room_info(x, y), Timestamp(0)));
        bench.drain();
    }
    for (index, read) in reads.iter().enumerate() {
        bench.read(read.clone(), Timestamp(0));
        if index % LINES_PER_FRAME == 0 {
            bench.drain();
        }
    }
    bench.drain();
    let memory_mb = heap_mb(HEAP_BYTES.load(Ordering::Relaxed) - heap_before);
    let (folder, _) = bench.finish();
    Moves {
        mapping: Summary::of(&mapping).expect("moves were timed"),
        wandering: Summary::of(&wandering).expect("moves were timed"),
        memory_mb,
        cold_start: measure_cold_start(folder.path(), plugins),
    }
}

/// Opens the session on a saved map. Each loads the plugins and reads the
/// whole map, as an app start does.
fn measure_cold_start(folder: &Path, plugins: &[Plugin]) -> Summary {
    let samples: Vec<Duration> = (0..COLD_STARTS)
        .map(|_| {
            let config = config(folder, plugins.to_vec());
            let hub = Arc::new(StoreHub::new(|| {}));
            let typing = Arc::new(Mutex::new(Typing::default()));
            let started = Instant::now();
            let mut engine = Engine::open(config, hub, typing).expect("the session opens");
            engine.start_connection().expect("the session connects");
            engine.connected();
            let elapsed = started.elapsed();
            engine.finish().expect("every write reaches the disk");
            elapsed
        })
        .collect();
    Summary::of(&samples).expect("starts were timed")
}
