//! The cost of a Luau hook call, for a trivial hook and for a hook that
//! splits a table's columns (the core's `columns` test fixture), against the
//! §11 budget of 5 µs per plugin per line.
//!
//! A hook runs only on the lines its pattern matched, off the line thread.
//! So the line thread pays only the hand-off, on hooked lines, and the hook's
//! own cost is amortized over the lines that don't reach it: here a table row
//! in every [`HOOKED_EVERY`] lines.

use mudling_core::line::{Line, LinePipeline};
use mudling_core::plugin::luau::{HookLimits, HookOutput, HookRunner, HookSink, LuauHooks, StateView, Value};
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
const TRIVIAL_TEXT: &str = "[[pattern]]\nname = \"row\"\npattern = '''^  \\S'''\nhook = \"on_row\"\n";
const TRIVIAL_HOOK: &str = "function on_row(event) end";
const RUNAWAY_HOOK: &str = "function on_row(event) while true do end end";
const RUNAWAY_CALLS: usize = 1_000;

const COLUMNS: [(&str, &str); 3] = [
    ("plugin.toml", include_str!("../../mudling-core/tests/fixtures/plugins/columns/plugin.toml")),
    ("text.toml", include_str!("../../mudling-core/tests/fixtures/plugins/columns/text.toml")),
    ("columns.luau", include_str!("../../mudling-core/tests/fixtures/plugins/columns/columns.luau")),
];

const WORDS: [&str; 12] = [
    "climb", "swim", "fishing", "weaving", "cooking", "map reading", "riding", "herbalism", "tracking", "first aid",
    "lockpicking", "haggling",
];

#[derive(Serialize)]
pub struct Report {
    pub calls: usize,
    /// The line thread's hand-off of one hooked event to the worker.
    pub hand_off: Summary,
    pub trivial: Summary,
    pub columns: Summary,
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
    let trivial = plugin(&[("plugin.toml", TRIVIAL_MANIFEST), ("text.toml", TRIVIAL_TEXT), ("trivial.luau", TRIVIAL_HOOK)]);
    let runaway = plugin(&[("plugin.toml", TRIVIAL_MANIFEST), ("text.toml", TRIVIAL_TEXT), ("trivial.luau", RUNAWAY_HOOK)]);
    let columns = plugin(&COLUMNS);
    let rows = rows();

    let hand_off = time_hand_off(&trivial, &rows);
    let trivial = time_hook(&trivial, &rows);
    let columns = time_hook(&columns, &rows);
    let runaway_stopped = time_runaway(&runaway, &rows[..RUNAWAY_CALLS]);
    let columns_amortized_p99_us = (columns.p99_us + hand_off.p99_us) / HOOKED_EVERY;
    Report {
        calls: CALLS,
        hand_off,
        trivial,
        columns,
        runaway_stopped,
        hooked_every: HOOKED_EVERY,
        columns_amortized_p99_us,
        plugin_budget_us: PLUGIN_BUDGET_US,
        within_budget: hand_off.p99_us <= PLUGIN_BUDGET_US && columns_amortized_p99_us <= PLUGIN_BUDGET_US,
    }
}

pub fn print(report: &Report) {
    let verdict = if report.within_budget { "ok" } else { "MISSED" };
    let summary = |s: &Summary| format!("p50 {:.2} µs, p99 {:.2} µs, max {:.1} µs", s.p50_us, s.p99_us, s.max_us);
    println!("luau hooks, {} calls each:", report.calls);
    println!("  hand-off on the line thread: {}", summary(&report.hand_off));
    println!("  trivial hook on the worker: {}", summary(&report.trivial));
    println!("  column-splitting hook on the worker: {}", summary(&report.columns));
    println!("  an endless loop, stopped by the default budget: {}", summary(&report.runaway_stopped));
    println!(
        "  column hook amortized over 1 line in {}: {:.2} µs p99 per line; budget ≤ {} µs: {verdict}",
        report.hooked_every, report.columns_amortized_p99_us, report.plugin_budget_us
    );
}

/// What the line thread pays per hooked event: queueing it for the worker.
fn time_hand_off(plugin: &Plugin, rows: &[Line]) -> Summary {
    let events = events(plugin, rows);
    let state: Arc<dyn StateView> = Arc::new(HashMap::<String, Value>::new());
    let mut hooks = LuauHooks::start([plugin], HookLimits::default(), state, Discard).expect("the hooks load");
    let samples: Vec<Duration> = events
        .into_iter()
        .map(|event| {
            let started = Instant::now();
            hooks.event(event);
            started.elapsed()
        })
        .collect();
    hooks.flush();
    Summary::of(&samples).expect("hand-offs were timed")
}

/// One hook call on the worker's runner, with its outputs.
fn time_hook(plugin: &Plugin, rows: &[Line]) -> Summary {
    let events = events(plugin, rows);
    let mut runner = HookRunner::new(HookLimits::default(), Arc::new(HashMap::<String, Value>::new()));
    runner.add(plugin).expect("the hooks load");
    let mut outputs = Vec::new();
    let samples: Vec<Duration> = events
        .iter()
        .map(|event| {
            outputs.clear();
            let started = Instant::now();
            runner.run(event, &mut outputs);
            let elapsed = started.elapsed();
            assert!(!outputs.iter().any(|output| matches!(output, HookOutput::HookFailed(_))), "{outputs:?}");
            elapsed
        })
        .collect();
    Summary::of(&samples).expect("hooks were timed")
}

/// Each call in a fresh VM, so three overruns never turn the plugin off.
fn time_runaway(plugin: &Plugin, rows: &[Line]) -> Summary {
    let mut runner = HookRunner::new(HookLimits::default(), Arc::new(HashMap::<String, Value>::new()));
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
    assert!(outputs.iter().all(|output| matches!(output, HookOutput::HookFailed(failed) if failed.overran)));
    Summary::of(&samples).expect("runaways were timed")
}

fn plugin(files: &[(&str, &str)]) -> Plugin {
    Plugin::from_files(|name| Ok(files.iter().find(|(file, _)| *file == name).expect("a bench file").1.to_owned()))
        .unwrap_or_else(|error| panic!("{error}"))
}

/// Table rows of three cells, each `  name  NN%`.
fn rows() -> Vec<Line> {
    let mut pipeline = LinePipeline::default();
    let mut lines = Vec::with_capacity(CALLS);
    for row in 0..CALLS {
        let cells: String = (0..3)
            .map(|column| format!("  {:<15} {:>3}%", WORDS[(row + column) % WORDS.len()], (row * 7 + column) % 101))
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
