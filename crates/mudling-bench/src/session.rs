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
//!   game plugin and one without; the gate is the difference in their p99s.
//! - **A move:** a `Room.Info` through the session, the map updated, its
//!   rows queued for the writer and the stores written, walking a map up to
//!   10,000 rooms and then wandering it.

use crate::plugin_host::{PLUGIN_BUDGET_US, realistic_plugins};
use mudling_core::persist::Timestamp;
use mudling_core::plugin::Layer;
use mudling_core::session::{Engine, SessionConfig, Typing};
use mudling_core::stats::Summary;
use mudling_core::store::{MapState, RoomState, StoreHub, TerminalState, VitalsState};
use mudling_core::telnet::Bytes;
use mudling_core::telnet::command::{GA, IAC, SB, SE};
use mudling_core::telnet::option::GMCP;
use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const LINE_BUDGET_US: f64 = 20.0;
const MOVE_BUDGET_US: f64 = 1_000.0;
const HOST: &str = "bench.example";
const MIN_LINES: usize = 100_000;
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
    pub line_budget_us: f64,
    pub plugin_budget_us: f64,
    pub move_budget_us: f64,
    pub within_budget: bool,
}

pub fn run(source: &[String]) -> Report {
    let count = source.len().max(MIN_LINES);
    let reads: Vec<Bytes> = source
        .iter()
        .cycle()
        .take(count)
        .enumerate()
        .map(|(index, line)| read(index, line))
        .collect();

    let whole_line = measure_lines(&reads);
    let (without_game_plugin, with_game_plugin) = measure_one_more_plugin(&reads);
    let one_more_plugin_p99_us = with_game_plugin.p99_us - without_game_plugin.p99_us;
    let (mapping, wandering) = measure_moves();
    Report {
        lines: count,
        within_budget: whole_line.p99_us <= LINE_BUDGET_US
            && one_more_plugin_p99_us <= PLUGIN_BUDGET_US
            && mapping.p99_us <= MOVE_BUDGET_US
            && wandering.p99_us <= MOVE_BUDGET_US,
        whole_line,
        without_game_plugin,
        with_game_plugin,
        one_more_plugin_p99_us,
        moves: (GRID_SIDE * GRID_SIDE + WANDER_MOVES) as usize,
        mapping,
        wandering,
        line_budget_us: LINE_BUDGET_US,
        plugin_budget_us: PLUGIN_BUDGET_US,
        move_budget_us: MOVE_BUDGET_US,
    }
}

pub fn print(report: &Report) {
    let verdict = |ok: bool| if ok { "ok" } else { "MISSED" };
    let summary = |s: &Summary| {
        format!(
            "p50 {:.2} µs, p99 {:.2} µs, max {:.1} µs",
            s.p50_us, s.p99_us, s.max_us
        )
    };
    println!(
        "session over {} lines (telnet → plugins → stores, writer on its thread):",
        report.lines
    );
    println!(
        "  whole line: {}; budget p99 ≤ {} µs: {}",
        summary(&report.whole_line),
        report.line_budget_us,
        verdict(report.whole_line.p99_us <= report.line_budget_us)
    );
    println!(
        "  one more plugin: {} without, {} with; +{:.2} µs p99; budget ≤ {} µs: {}",
        summary(&report.without_game_plugin),
        summary(&report.with_game_plugin),
        report.one_more_plugin_p99_us,
        report.plugin_budget_us,
        verdict(report.one_more_plugin_p99_us <= report.plugin_budget_us)
    );
    println!(
        "  a move, {} of them over {} rooms: mapping {}; wandering {}; budget p99 ≤ {} µs: {}",
        report.moves,
        GRID_SIDE * GRID_SIDE,
        summary(&report.mapping),
        summary(&report.wandering),
        report.move_budget_us,
        verdict(
            report.mapping.p99_us <= report.move_budget_us
                && report.wandering.p99_us <= report.move_budget_us
        )
    );
}

/// One line as the server sends it: every few, with a `Char.Vitals` and a
/// prompt ended by GA after it.
fn read(index: usize, line: &str) -> Bytes {
    let mut bytes = line.as_bytes().to_vec();
    bytes.extend_from_slice(b"\n\r");
    if index % LINES_PER_PROMPT == LINES_PER_PROMPT - 1 {
        let (hp, moves) = (index % 500, index % 97);
        push_gmcp(
            &mut bytes,
            &format!(r#"Char.Vitals {{"hp": {hp}, "maxhp": 500, "mv": {moves}}}"#),
        );
        bytes.extend_from_slice(format!("<{hp}hp {moves}mv> ").as_bytes());
        bytes.extend_from_slice(&[IAC, GA]);
    }
    bytes.into()
}

fn push_gmcp(bytes: &mut Vec<u8>, message: &str) {
    bytes.extend_from_slice(&[IAC, SB, GMCP]);
    bytes.extend_from_slice(message.as_bytes());
    bytes.extend_from_slice(&[IAC, SE]);
}

/// A connected session with a shell subscribed, in a folder of its own.
struct Bench {
    engine: Engine,
    hub: Arc<StoreHub>,
    _folder: tempfile::TempDir,
}

impl Bench {
    fn new(with_game: bool) -> Bench {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let hub = Arc::new(StoreHub::new(|| {}));
        hub.subscribe::<TerminalState>();
        hub.subscribe::<VitalsState>();
        hub.subscribe::<RoomState>();
        hub.subscribe::<MapState>();
        let plugins = realistic_plugins()
            .into_iter()
            .filter(|plugin| match plugin.layer() {
                Layer::Default => false,
                Layer::Game => with_game,
                Layer::Override => true,
            })
            .collect();
        let config = SessionConfig {
            plugins,
            ..SessionConfig::new(folder.path(), HOST, 4000)
        };
        let typing = Arc::new(Mutex::new(Typing::default()));
        let mut engine = Engine::open(config, Arc::clone(&hub), typing).expect("the session opens");
        engine.start_connection().expect("the session connects");
        engine.connected();
        Bench {
            engine,
            hub,
            _folder: folder,
        }
    }

    /// One read, through to the stores. Its bytes are made before the clock
    /// starts, as the socket read makes them.
    fn read(&mut self, bytes: Bytes) -> Duration {
        let now = Timestamp(0);
        let started = Instant::now();
        self.engine.receive(bytes, now).expect("the stream is good");
        self.engine.settle(now);
        drop(self.engine.take_output());
        started.elapsed()
    }

    fn finish(self) {
        self.engine.finish().expect("every write reaches the disk");
    }
}

fn measure_lines(reads: &[Bytes]) -> Summary {
    let mut bench = Bench::new(true);
    let mut samples = Vec::with_capacity(reads.len());
    for (index, read) in reads.iter().enumerate() {
        samples.push(bench.read(read.clone()));
        if index % LINES_PER_FRAME == 0 {
            bench.hub.drain();
        }
    }
    bench.finish();
    Summary::of(&samples).expect("lines were timed")
}

fn measure_one_more_plugin(reads: &[Bytes]) -> (Summary, Summary) {
    let mut without = Bench::new(false);
    let mut with = Bench::new(true);
    let mut without_samples = Vec::with_capacity(reads.len());
    let mut with_samples = Vec::with_capacity(reads.len());
    for (index, read) in reads.iter().enumerate() {
        // Alternate which goes first, so neither always reads a warm line.
        if index % 2 == 0 {
            without_samples.push(without.read(read.clone()));
            with_samples.push(with.read(read.clone()));
        } else {
            with_samples.push(with.read(read.clone()));
            without_samples.push(without.read(read.clone()));
        }
        if index % LINES_PER_FRAME == 0 {
            without.hub.drain();
            with.hub.drain();
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
    push_gmcp(
        &mut bytes,
        &format!(
            r#"Room.Info {{"num": {id}, "name": "Room {id}", "zone": "Grid", "exits": {{{}}}}}"#,
            exits.join(", ")
        ),
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

fn measure_moves() -> (Summary, Summary) {
    let mut bench = Bench::new(true);
    let walk = serpentine();
    let mut mapping = Vec::with_capacity(walk.len());
    for (x, y, direction) in &walk {
        bench.engine.send(direction, Timestamp(0));
        mapping.push(bench.read(room_info(*x, *y)));
        bench.hub.drain();
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
        wandering.push(bench.read(room_info(x, y)));
        bench.hub.drain();
    }
    bench.finish();
    (
        Summary::of(&mapping).expect("moves were timed"),
        Summary::of(&wandering).expect("moves were timed"),
    )
}
