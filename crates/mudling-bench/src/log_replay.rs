//! A session log through the session (PRD §13), twice.
//!
//! - **Engine:** with no threads of its own, so each event gets its own
//!   timing and each store its own apply time. A short log starts over until
//!   enough lines have been timed. `--pace` plays it once at that many lines
//!   a second instead; logs hold no times, so "real pace" is a rate given.
//! - **Session:** the threads a shell runs against: the log's bytes go in
//!   with [`Session::replay`] while a shell thread drains on its frame tick.
//!
//! Each line of server text is its own read, as a line is what the budget is
//! per. Commands come from the log's map notes, so moves link up. The cost of
//! each plugin comes from a host of its own that samples every line: the
//! session keeps its host to itself.
//!
//! Writer lag is how long the writer takes to catch up once the last read is
//! in: the session has no way to show its queue. There's no sync lag to
//! report yet, as sync lands with Phase 6.

use crate::plugin_host::{Counter, split};
use crate::session::{self, Bench, LINE_BUDGET_US, MIN_LINES, MOVE_BUDGET_US};
use crate::{BudgetScale, FRAME, HEAP_BYTES, heap_mb, millis, verdict};
use mudling_core::defaults;
use mudling_core::net::DisconnectReason;
use mudling_core::persist::Timestamp;
use mudling_core::plugin::{Layer, Plugin, PluginHost};
use mudling_core::replay::Event;
use mudling_core::session::Session;
use mudling_core::stats::Summary;
use mudling_core::store::{
    CharactersState, ConnectionStatus, Diff, GearState, SettingsState, Store, Stores, Versioned,
};
use mudling_core::telnet::{Bytes, gmcp};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

/// Reads between a shell's drains: its frame.
const READS_PER_FRAME: usize = 8;
/// A second per room, as the prototype's replay counts it.
const ROOM_INFO_MS: i64 = 1_000;
const LINE: &str = "line";
const UNENDED_TEXT: &str = "unended text";
const COMMAND: &str = "command";
const MOVE: &str = "gmcp Room.Info";

#[derive(Serialize)]
pub struct Report {
    source: String,
    lines: usize,
    /// Each kind of event, from one read in to everything published.
    by_event: BTreeMap<String, Summary>,
    plugins: BTreeMap<String, Summary>,
    /// Keys the plugins' Luau hooks wrote to their stores: the hooks ran.
    hook_writes: usize,
    /// Each store's diff, applied to a shell's copy.
    store_apply: BTreeMap<String, Summary>,
    writer_catch_up_ms: f64,
    heap_mb: f64,
    threaded: Threaded,
    line_budget_us: f64,
    move_budget_us: f64,
    pub within_budget: bool,
}

/// The same log through [`Session`].
#[derive(Serialize)]
struct Threaded {
    chunks: usize,
    lines_per_second: f64,
    /// A frame tick: the drain, and the batch applied.
    frame: Summary,
    frames: usize,
    writer_catch_up_ms: f64,
}

pub fn run(
    source: &str,
    logs: &[Vec<Event>],
    plugins: &[Plugin],
    pace: Option<u32>,
    scale: BudgetScale,
) -> Result<Report, String> {
    let events: Vec<&Event> = match pace {
        Some(_) => logs.iter().flatten().collect(),
        None => until_enough_lines(logs),
    };
    let reads = reads(&events);
    let heap_before = HEAP_BYTES.load(Ordering::Relaxed);
    let mut bench = Bench::new(plugins.to_vec());
    let pause = pace.map(|lines_per_second| Duration::from_secs(1) / lines_per_second);
    let samples = play(&mut bench, &reads, pause)?;
    let heap = HEAP_BYTES.load(Ordering::Relaxed) - heap_before;
    let hook_writes = hook_writes(&bench);
    let (_folder, writer_catch_up) = bench.finish();

    let by_event = summaries(samples.by_event);
    let lines = by_event.get(LINE).map_or(0, |line| line.samples);
    let line_budget_us = scale.time_us(LINE_BUDGET_US);
    let move_budget_us = scale.time_us(MOVE_BUDGET_US);
    let within_budget = by_event
        .get(LINE)
        .is_none_or(|line| line.p99_us <= line_budget_us)
        && by_event
            .get(MOVE)
            .is_none_or(|moves| moves.p99_us <= move_budget_us);
    Ok(Report {
        source: source.to_owned(),
        lines,
        by_event,
        plugins: plugin_costs(logs, plugins),
        hook_writes,
        store_apply: summaries(samples.store_apply),
        writer_catch_up_ms: millis(writer_catch_up),
        heap_mb: heap_mb(heap),
        threaded: through_the_session(&reads, plugins, lines),
        line_budget_us,
        move_budget_us,
        within_budget,
    })
}

pub fn print(report: &Report) {
    println!(
        "replay of {} through the session, {} lines:",
        report.source, report.lines
    );
    for (event, summary) in &report.by_event {
        println!("  {event}: {summary} ({} samples)", summary.samples);
    }
    println!(
        "  budgets p99: line ≤ {} µs, move ≤ {} µs: {}",
        report.line_budget_us,
        report.move_budget_us,
        verdict(report.within_budget)
    );
    for (plugin, summary) in &report.plugins {
        println!("  plugin {plugin}, sampled on every line: {summary}");
    }
    println!("  hooks wrote {} store keys", report.hook_writes);
    for (store, summary) in &report.store_apply {
        println!(
            "  store {store}, apply: {summary} ({} diffs)",
            summary.samples
        );
    }
    println!(
        "  writer caught up {:.1} ms after the last read; heap {:.1} MB (Rust only); sync lag: no sync yet",
        report.writer_catch_up_ms, report.heap_mb
    );
    let threaded = &report.threaded;
    println!(
        "  on its threads: {:.0} lines/s over {} chunks; frame drain + apply {} over {} frames; writer caught up {:.1} ms after quit",
        threaded.lines_per_second,
        threaded.chunks,
        threaded.frame,
        threaded.frames,
        threaded.writer_catch_up_ms
    );
}

/// Feeds the reads through the engine, one by one, with a shell's frames
/// between them, and waits for the hooks at the end.
fn play(bench: &mut Bench, reads: &[Read], pause: Option<Duration>) -> Result<Samples, String> {
    let mut samples = Samples::default();
    let mut clock = 0;
    for (index, read) in reads.iter().enumerate() {
        match read {
            Read::Connected => {
                bench
                    .engine
                    .close_connection(DisconnectReason::ClosedByServer, Timestamp(clock));
                bench
                    .engine
                    .start_connection()
                    .map_err(|error| error.to_string())?;
                bench.engine.connected();
            }
            Read::Command(command) => {
                let started = Instant::now();
                bench.engine.send(command, Timestamp(clock));
                samples.event(COMMAND, started.elapsed());
            }
            Read::Bytes { kind, bytes } => {
                if *kind == MOVE {
                    clock += ROOM_INFO_MS;
                }
                let elapsed = bench.read(bytes.clone(), Timestamp(clock));
                samples.event(kind, elapsed);
            }
        }
        if index % READS_PER_FRAME == 0 {
            samples.apply(bench);
        }
        if let Some(pause) = pause {
            thread::sleep(pause);
        }
    }
    bench
        .engine
        .close_connection(DisconnectReason::ClosedByServer, Timestamp(clock));
    samples.apply(bench);
    Ok(samples)
}

/// What the plugins' hooks left in the stores.
fn hook_writes(bench: &Bench) -> usize {
    let characters = bench
        .hub
        .read(|characters: &Versioned<CharactersState>| characters.value().0.len());
    let gear = bench
        .hub
        .read(|gear: &Versioned<GearState>| gear.value().0.len());
    characters + gear
}

/// The logs in order, each with its connection, over and over until they
/// hold [`MIN_LINES`] lines.
fn until_enough_lines(logs: &[Vec<Event>]) -> Vec<&Event> {
    let once = logs.iter().flatten();
    let lines: usize = once
        .clone()
        .map(|event| match event {
            Event::Text(text) => text.matches('\n').count(),
            _ => 0,
        })
        .sum();
    let rounds = MIN_LINES.div_ceil(lines.max(1));
    std::iter::repeat_n(once, rounds).flatten().collect()
}

enum Read<'a> {
    Connected,
    Command(&'a str),
    Bytes { kind: String, bytes: Bytes },
}

/// The events as the session takes them, made before any clock starts.
fn reads<'a>(events: &[&'a Event]) -> Vec<Read<'a>> {
    let mut reads = Vec::with_capacity(events.len());
    for event in events {
        match event {
            Event::Connected => reads.push(Read::Connected),
            Event::Command(command) => reads.push(Read::Command(command)),
            Event::Text(text) => {
                reads.extend(text.split_inclusive('\n').map(|line| {
                    Read::Bytes {
                        kind: if line.ends_with('\n') {
                            LINE
                        } else {
                            UNENDED_TEXT
                        }
                        .to_owned(),
                        bytes: Bytes::copy_from_slice(line.as_bytes()),
                    }
                }));
            }
            Event::Gmcp { package, json } => {
                let mut bytes = Vec::new();
                gmcp::encode(package, json, &mut bytes);
                reads.push(Read::Bytes {
                    kind: format!("gmcp {package}"),
                    bytes: bytes.into(),
                });
            }
        }
    }
    reads
}

#[derive(Default)]
struct Samples {
    by_event: BTreeMap<String, Vec<Duration>>,
    store_apply: BTreeMap<String, Vec<Duration>>,
}

impl Samples {
    fn event(&mut self, kind: &str, elapsed: Duration) {
        match self.by_event.get_mut(kind) {
            Some(samples) => samples.push(elapsed),
            None => drop(self.by_event.insert(kind.to_owned(), vec![elapsed])),
        }
    }

    /// A shell's frame: drains, then applies each store's diff, timing each.
    fn apply(&mut self, bench: &mut Bench) {
        let Some(batch) = bench.hub.drain() else {
            return;
        };
        let Stores {
            terminal,
            vitals,
            room,
            map,
            ..
        } = &mut bench.shell;
        self.time("terminal", terminal, &batch.terminal);
        self.time("vitals", vitals, &batch.vitals);
        self.time("room", room, &batch.room);
        self.time("map", map, &batch.map);
    }

    fn time<S: Store>(
        &mut self,
        store: &'static str,
        slot: &mut Versioned<S>,
        diff: &Option<Diff<S>>,
    ) {
        if let Some(diff) = diff {
            let started = Instant::now();
            slot.apply_diff(diff);
            let elapsed = started.elapsed();
            self.store_apply
                .entry(store.to_owned())
                .or_default()
                .push(elapsed);
        }
    }
}

fn summaries(samples: BTreeMap<String, Vec<Duration>>) -> BTreeMap<String, Summary> {
    samples
        .into_iter()
        .filter_map(|(name, samples)| Some((name, Summary::of(&samples)?)))
        .collect()
}

/// Every plugin the session runs, each timed on every line of the logs.
fn plugin_costs(logs: &[Vec<Event>], plugins: &[Plugin]) -> BTreeMap<String, Summary> {
    let text: Vec<String> = logs
        .iter()
        .flatten()
        .filter_map(|event| match event {
            Event::Text(text) => Some(text.lines().map(str::to_owned)),
            _ => None,
        })
        .flatten()
        .collect();
    let lines = split(&text, text.len());
    let run = plugins
        .iter()
        .filter(|plugin| plugin.layer() != Layer::Default)
        .cloned()
        .chain([defaults::plugin()]);
    let mut host = PluginHost::new(run.collect::<Vec<_>>()).expect("the plugins compile");
    host.set_cost_sampling(1);
    let mut sink = Counter::default();
    lines
        .iter()
        .for_each(|line| drop(host.read_line(line, &mut sink)));
    host.costs()
        .into_iter()
        .filter_map(|cost| Some((cost.plugin.to_string(), cost.per_line?)))
        .collect()
}

fn through_the_session(reads: &[Read], plugins: &[Plugin], lines: usize) -> Threaded {
    let chunks: Vec<Bytes> = reads
        .iter()
        .filter_map(|read| match read {
            Read::Bytes { bytes, .. } => Some(bytes.clone()),
            _ => None,
        })
        .collect();
    let chunk_count = chunks.len();
    let folder = tempfile::tempdir().expect("a temporary folder");
    let session = Session::open(session::config(folder.path(), plugins.to_vec()), || {})
        .expect("the session opens");
    let mut shell = session::subscribe(session.stores());

    let started = Instant::now();
    session.replay(chunks);
    let mut ticks = Vec::new();
    let mut replayed = started.elapsed();
    let mut closed = false;
    while !closed {
        thread::sleep(FRAME);
        let tick = Instant::now();
        if let Some(batch) = session.stores().drain() {
            shell.apply_batch(&batch);
        }
        ticks.push(tick.elapsed());
        replayed = started.elapsed();
        closed = session.stores().read::<SettingsState, _>(|settings| {
            matches!(settings.value().connection, ConnectionStatus::Closed(_))
        });
    }
    let quitting = Instant::now();
    session.quit().wait().expect("every write reaches the disk");
    let writer_catch_up = quitting.elapsed();

    Threaded {
        chunks: chunk_count,
        lines_per_second: lines as f64 / replayed.as_secs_f64(),
        frames: ticks.len(),
        frame: Summary::of(&ticks).expect("frames were timed"),
        writer_catch_up_ms: millis(writer_catch_up),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mudling_core::store::MapState;

    fn replayed(log: &str) -> Vec<String> {
        let events = mudling_core::replay::events(log);
        let events: Vec<&Event> = events.iter().collect();
        reads(&events)
            .iter()
            .map(|read| match read {
                Read::Connected => "connected".to_owned(),
                Read::Command(command) => format!("command {command}"),
                Read::Bytes { kind, .. } => kind.clone(),
            })
            .collect()
    }

    #[test]
    fn each_line_is_a_read_and_each_gmcp_package_a_kind() {
        let log = "[connected]\nOne.\r\nTwo.\r\n<7hp> \n[GMCP] Char.Vitals {}\n\n[MAP] move east\n";
        assert_eq!(
            replayed(log),
            [
                "connected",
                LINE,
                LINE,
                UNENDED_TEXT,
                "gmcp Char.Vitals",
                "command east"
            ]
        );
    }

    /// A gate over plugins that never fire would measure nothing.
    #[test]
    fn the_fixture_plugins_hooks_and_map_rules_run_in_the_replay() {
        let replayed = |reply: &str| {
            let log = format!(
                "[connected]\nYou hit a goblin.\n\r\
                 \n[GMCP] Room.Info {{\"num\": 1, \"name\": \"A\", \"exits\": {{\"e\": \"O\"}}}}\n\
                 \n[MAP] move east\n{reply}\n\r\
                 \n[GMCP] Room.Info {{\"num\": 2, \"name\": \"B\", \"exits\": {{\"w\": \"O\"}}}}\n"
            );
            let events = mudling_core::replay::events(&log);
            let events: Vec<&Event> = events.iter().collect();
            let mut bench = Bench::new(vec![crate::fixture_plugin()]);
            play(&mut bench, &reads(&events), None).unwrap();
            let links = bench.hub.read(|map: &Versioned<MapState>| {
                let rooms = map.value().rooms();
                rooms.map(|room| room.links.len()).sum::<usize>()
            });
            let writes = hook_writes(&bench);
            bench.finish();
            (writes, links)
        };

        assert_eq!(replayed("You walk east."), (1, 2));
        assert_eq!(replayed("You can't go that way."), (1, 0));
    }

    #[test]
    fn a_short_log_starts_over_until_it_holds_enough_lines() {
        let logs = vec![mudling_core::replay::events("[connected]\nOne.\n")];
        let events = until_enough_lines(&logs);
        let lines = events
            .iter()
            .filter(|event| matches!(event, Event::Text(_)))
            .count();
        assert_eq!(lines, MIN_LINES);
    }
}
