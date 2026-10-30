//! The cost of a Luau hook call, for a trivial hook and for a hook that
//! splits a table's columns (the core's `columns` test fixture), against the
//! §11 budget of 5 µs per plugin per line.
//!
//! A hook runs only on the lines its pattern matched, off the line thread.
//! So the line thread pays only the hand-off, on hooked lines, and the hook's
//! own cost is amortized over the lines that don't reach it: here a table row
//! in every [`HOOKED_EVERY`] lines.

use mudling_core::line::{Line, LinePipeline};
use mudling_core::plugin::luau::{
    HookLimits, HookOutput, HookRunner, HookSink, LuauHooks, OVERRUNS_TO_TURN_OFF, StateView, Value,
};
use mudling_core::plugin::{Plugin, PluginEvent, PluginHost, PluginSink};
use mudling_core::stats::Summary;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const PLUGIN_BUDGET_US: f64 = 5.0;
/// One line in this many reaches the hook: a table row now and then.
pub const HOOKED_EVERY: f64 = 20.0;
const CALLS: usize = 100_000;
/// A well-behaved hook may overrun at most once in this many calls, 10 of
/// the 100,000 here. A shared CI machine can stall the worker thread long
/// enough to trip the box now and then; a bug that charges hooks for work
/// that isn't theirs trips it on far more calls than this.
const FALSE_OVERRUNS_EVERY: usize = 10_000;

const TRIVIAL_MANIFEST: &str = r#"
id = "bench-trivial"
name = "Trivial hook"
version = "1.0.0"
api = 1
license = "MIT"
layer = "default"

[points]
text = "text.toml"

[hooks]
files = ["trivial.luau"]
"#;
const TRIVIAL_TEXT: &str =
    "[[pattern]]\nname = \"row\"\npattern = '''^  \\S'''\nhook = \"on_row\"\n";
const TRIVIAL_HOOK: &str = "function on_row(event) end";
const RUNAWAY_HOOK: &str = "function on_row(event) while true do end end";
const RUNAWAY_CALLS: usize = 1_000;

const COLUMNS: [(&str, &str); 3] = [
    (
        "plugin.toml",
        include_str!("../../mudling-core/tests/fixtures/plugins/columns/plugin.toml"),
    ),
    (
        "text.toml",
        include_str!("../../mudling-core/tests/fixtures/plugins/columns/text.toml"),
    ),
    (
        "columns.luau",
        include_str!("../../mudling-core/tests/fixtures/plugins/columns/columns.luau"),
    ),
];

const WORDS: [&str; 12] = [
    "climb",
    "swim",
    "fishing",
    "weaving",
    "cooking",
    "map reading",
    "riding",
    "herbalism",
    "tracking",
    "first aid",
    "lockpicking",
    "haggling",
];

#[derive(Serialize)]
pub struct Report {
    pub calls: usize,
    /// The line thread's hand-off of one hooked event to the worker.
    pub hand_off: Summary,
    pub trivial: Summary,
    pub trivial_overruns: Overruns,
    pub columns: Summary,
    pub columns_overruns: Overruns,
    /// How long the default budget lets an endless loop run.
    pub runaway_stopped: Summary,
    pub hooked_every: f64,
    /// The column hook's p99 spread over the lines that don't reach it,
    /// plus the hand-off on the line that does.
    pub columns_amortized_p99_us: f64,
    pub plugin_budget_us: f64,
    pub within_budget: bool,
}

pub fn run() -> Report {
    let trivial = plugin(&[
        ("plugin.toml", TRIVIAL_MANIFEST),
        ("text.toml", TRIVIAL_TEXT),
        ("trivial.luau", TRIVIAL_HOOK),
    ]);
    let runaway = plugin(&[
        ("plugin.toml", TRIVIAL_MANIFEST),
        ("text.toml", TRIVIAL_TEXT),
        ("trivial.luau", RUNAWAY_HOOK),
    ]);
    let columns = plugin(&COLUMNS);
    let rows = rows();

    let hand_off = time_hand_off(&trivial, &rows);
    let (trivial, trivial_overruns) = time_hook(&trivial, &rows);
    let (columns, columns_overruns) = time_hook(&columns, &rows);
    let runaway_stopped = time_runaway(&runaway, &rows[..RUNAWAY_CALLS]);
    let columns_amortized_p99_us = (columns.p99_us + hand_off.p99_us) / HOOKED_EVERY;
    let within_budget = hand_off.p99_us <= PLUGIN_BUDGET_US
        && columns_amortized_p99_us <= PLUGIN_BUDGET_US
        && trivial_overruns.acceptable(CALLS)
        && columns_overruns.acceptable(CALLS);
    Report {
        calls: CALLS,
        hand_off,
        trivial,
        trivial_overruns,
        columns,
        columns_overruns,
        runaway_stopped,
        hooked_every: HOOKED_EVERY,
        columns_amortized_p99_us,
        plugin_budget_us: PLUGIN_BUDGET_US,
        within_budget,
    }
}

pub fn print(report: &Report) {
    let verdict = if report.within_budget { "ok" } else { "MISSED" };
    let summary = |s: &Summary| {
        format!(
            "p50 {:.2} µs, p99 {:.2} µs, max {:.1} µs",
            s.p50_us, s.p99_us, s.max_us
        )
    };
    println!("luau hooks, {} calls each:", report.calls);
    println!(
        "  hand-off on the line thread: {}",
        summary(&report.hand_off)
    );
    let overruns = |o: &Overruns| match &o.first {
        None => "no overruns".to_owned(),
        Some(first) => format!("{} overran (first: {first})", o.count),
    };
    println!(
        "  trivial hook on the worker: {}; {}",
        summary(&report.trivial),
        overruns(&report.trivial_overruns)
    );
    println!(
        "  column-splitting hook on the worker: {}; {}",
        summary(&report.columns),
        overruns(&report.columns_overruns)
    );
    println!(
        "  an endless loop, stopped by the default budget: {}",
        summary(&report.runaway_stopped)
    );
    println!(
        "  column hook amortized over 1 line in {}: {:.2} µs p99 per line; budget ≤ {} µs: {verdict}",
        report.hooked_every, report.columns_amortized_p99_us, report.plugin_budget_us
    );
}

/// What the line thread pays per hooked event: queueing it for the worker.
/// The events go in batches smaller than the queue, waiting (untimed) for
/// the worker between batches, so every one is queued, none dropped.
fn time_hand_off(plugin: &Plugin, rows: &[Line]) -> Summary {
    let limits = HookLimits::default();
    let state: Arc<dyn StateView> = Arc::new(HashMap::<String, Value>::new());
    let mut hooks = LuauHooks::start([plugin], limits, state, Discard).expect("the worker starts");
    assert!(
        hooks.failed_to_load().is_empty(),
        "{:?}",
        hooks.failed_to_load()
    );
    let mut samples = Vec::with_capacity(rows.len());
    let mut events = events(plugin, rows).into_iter().peekable();
    while events.peek().is_some() {
        for event in events.by_ref().take(limits.queue / 2) {
            let started = Instant::now();
            hooks.event(event);
            samples.push(started.elapsed());
        }
        hooks.wait_until_idle();
    }
    assert!(
        hooks.dropped().is_empty(),
        "the queue filled: {:?}",
        hooks.dropped()
    );
    Summary::of(&samples).expect("hand-offs were timed")
}

/// A well-behaved hook's calls that its time box stopped anyway.
#[derive(Default, Serialize)]
pub struct Overruns {
    pub count: usize,
    /// The first one's reason, for the CI log.
    pub first: Option<String>,
}

impl Overruns {
    /// At most 1 call in [`FALSE_OVERRUNS_EVERY`] may overrun.
    fn acceptable(&self, calls: usize) -> bool {
        self.count * FALSE_OVERRUNS_EVERY <= calls
    }
}

/// One hook call on the worker's runner, with its outputs. A call the time
/// box stops is counted, not fatal: a shared CI machine can stall a thread
/// past it. The plugin is reloaded, untimed, before overruns turn it off.
fn time_hook(plugin: &Plugin, rows: &[Line]) -> (Summary, Overruns) {
    let events = events(plugin, rows);
    let mut runner = HookRunner::new(
        HookLimits::default(),
        Arc::new(HashMap::<String, Value>::new()),
    );
    runner.add(plugin).expect("the hooks load");
    let mut outputs = Vec::new();
    let mut overruns = Overruns::default();
    let mut samples = Vec::with_capacity(events.len());
    for event in &events {
        outputs.clear();
        let started = Instant::now();
        runner.run(event, &mut outputs);
        samples.push(started.elapsed());
        for output in &outputs {
            match output {
                HookOutput::HookFailed(failed) if failed.overran => {
                    overruns.count += 1;
                    overruns.first.get_or_insert_with(|| failed.message.clone());
                    if overruns.count % (OVERRUNS_TO_TURN_OFF as usize - 1) == 0 {
                        runner.add(plugin).expect("the hooks load");
                    }
                }
                HookOutput::HookFailed(failed) => panic!("{failed:?}"),
                _ => {}
            }
        }
    }
    (Summary::of(&samples).expect("hooks were timed"), overruns)
}

/// Each call in a fresh VM, so three overruns never turn the plugin off.
fn time_runaway(plugin: &Plugin, rows: &[Line]) -> Summary {
    let mut runner = HookRunner::new(
        HookLimits::default(),
        Arc::new(HashMap::<String, Value>::new()),
    );
    let mut outputs = Vec::new();
    let samples: Vec<Duration> = events(plugin, rows)
        .iter()
        .map(|event| {
            runner.add(plugin).expect("the hooks load");
            let started = Instant::now();
            runner.run(event, &mut outputs);
            started.elapsed()
        })
        .collect();
    assert!(
        outputs
            .iter()
            .all(|output| matches!(output, HookOutput::HookFailed(failed) if failed.overran))
    );
    Summary::of(&samples).expect("runaways were timed")
}

fn plugin(files: &[(&str, &str)]) -> Plugin {
    Plugin::from_files(|name| {
        Ok(files
            .iter()
            .find(|(file, _)| *file == name)
            .expect("a bench file")
            .1
            .to_owned())
    })
    .unwrap_or_else(|error| panic!("{error}"))
}

/// Table rows of three cells, each `  name  NN%`.
fn rows() -> Vec<Line> {
    let mut pipeline = LinePipeline::default();
    let mut lines = Vec::with_capacity(CALLS);
    for row in 0..CALLS {
        let cells: String = (0..3)
            .map(|column| {
                format!(
                    "  {:<15} {:>3}%",
                    WORDS[(row + column) % WORDS.len()],
                    (row * 7 + column) % 101
                )
            })
            .collect();
        pipeline.push(cells.as_bytes(), &mut lines);
        pipeline.push(b"\n", &mut lines);
    }
    lines
}

fn events(plugin: &Plugin, rows: &[Line]) -> Vec<PluginEvent> {
    let mut host = PluginHost::new([plugin.clone()]).expect("the plugin compiles");
    let mut events = Vec::with_capacity(rows.len());
    for row in rows {
        host.read_line(row, &mut events);
    }
    assert_eq!(events.len(), rows.len(), "every row reaches the hook");
    events
}

struct Discard;

impl HookSink for Discard {
    fn output(&mut self, output: HookOutput) {
        std::hint::black_box(output);
    }
}
