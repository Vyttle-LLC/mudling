//! The plugin host's share of the §6 line budgets, with a synthetic plugin
//! set of 300 patterns over six plugins:
//!
//! - **whole line**: split, decode, ANSI strip and every plugin, ≤ 20 µs p99;
//! - **one more plugin**: the p99 with all six minus the p99 without one,
//!   ≤ 5 µs; and each plugin timed alone by the host's own cost sampling,
//!   ≤ 5 µs p99.
//!
//! It also reports the heap a host holds after replaying every line, matcher
//! caches included, with six plugins and with twenty.

use crate::HEAP_BYTES;
use mudling_core::line::{Line, LinePipeline};
use mudling_core::plugin::{Plugin, PluginEvent, PluginHost, PluginSink};
use mudling_core::stats::Summary;
use serde::Serialize;
use std::fmt::Write;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

pub const LINE_BUDGET_US: f64 = 20.0;
pub const PLUGIN_BUDGET_US: f64 = 5.0;
const MIN_LINES: usize = 100_000;
const PLUGINS: usize = 6;
const MANY_PLUGINS: usize = 20;
/// Text patterns per plugin, then color rules and prompt hints.
const EVENT_RULES: usize = 40;
const COLOR_RULES: usize = 9;
const PROMPT_HINTS: usize = 1;

/// Generic MUD wording; no game's.
const WORDS: [&str; 60] = [
    "hit", "slash", "pierce", "crush", "bite", "claw", "kick", "punch", "bash", "smite", "cast", "chant", "flee",
    "arrive", "leave", "enter", "drop", "get", "give", "wear", "wield", "remove", "open", "close", "lock", "unlock",
    "eat", "drink", "sleep", "rest", "stand", "sit", "say", "shout", "yell", "tell", "whisper", "sell", "buy", "kill",
    "die", "bleed", "heal", "glow", "hum", "shimmer", "gold", "sword", "shield", "door", "gate", "north", "south",
    "east", "west", "potion", "scroll", "armor", "guard", "dragon",
];

const EVENT_TEMPLATES: [&str; 8] = [
    r"^You {w} (?<target>.+?)\.$",
    r"^(?<actor>[A-Z]\w*) {w}s (?<target>.+?)[.!]$",
    r"^(?<who>\w+) {w}s '(?<message>.*)'$",
    r"(?<count>\d+) {w}s?\b",
    r"\b{w}\b.*\bhere\b",
    r"^\[(?<channel>\w+)\] (?<who>\w+): .*{w}",
    r"^The {w} (?<rest>.+)$",
    r"^(?<current>\d+)/(?<max>\d+) \w+$",
];

#[derive(Serialize)]
pub struct Report {
    pub lines: usize,
    pub plugins: usize,
    pub patterns: usize,
    pub events: usize,
    /// Line pipeline plus every plugin, per server line.
    pub whole_line: Summary,
    /// The host alone, all plugins on, per line.
    pub host_all: Summary,
    /// The host alone, one game plugin off.
    pub host_without_one: Summary,
    pub one_more_plugin_p99_us: f64,
    /// Each plugin's p99 as the host's sampling timed it alone.
    pub alone_p99_us: Vec<(String, f64)>,
    pub host_heap: Vec<HostHeap>,
    pub line_budget_us: f64,
    pub plugin_budget_us: f64,
    pub within_budget: bool,
}

pub fn run(source: &[String]) -> Report {
    let count = source.len().max(MIN_LINES);
    let plugins = synthetic_plugins(PLUGINS);
    let patterns = PLUGINS * (EVENT_RULES + COLOR_RULES + PROMPT_HINTS);

    let mut host = PluginHost::new(plugins.clone()).expect("the synthetic plugins compile");
    let mut sink = Counter::default();
    let whole_line = time_whole_lines(source, count, &mut host, &mut sink);

    let lines = split(source, count);
    let all = time_host(&mut PluginHost::new(plugins.clone()).expect("compiles"), &lines);
    let without: Vec<Plugin> = plugins.iter().filter(|plugin| plugin.id().as_str() != "bench-game-4").cloned().collect();
    let without_one = time_host(&mut PluginHost::new(without).expect("compiles"), &lines);

    let mut sampled = PluginHost::new(plugins).expect("compiles");
    sampled.set_cost_sampling(1);
    time_host(&mut sampled, &lines);
    let alone_p99_us: Vec<(String, f64)> = sampled
        .costs()
        .into_iter()
        .map(|cost| (cost.plugin.to_string(), cost.per_line.map_or(0.0, |summary| summary.p99_us)))
        .collect();
    drop(sampled);
    let host_heap = [PLUGINS, MANY_PLUGINS].map(|plugins| host_heap(plugins, &lines)).into();

    let one_more_plugin_p99_us = all.p99_us - without_one.p99_us;
    let within_budget = whole_line.p99_us <= LINE_BUDGET_US
        && one_more_plugin_p99_us <= PLUGIN_BUDGET_US
        && alone_p99_us.iter().all(|(_, p99)| *p99 <= PLUGIN_BUDGET_US);
    Report {
        lines: count,
        plugins: PLUGINS,
        patterns,
        events: sink.events,
        whole_line,
        host_all: all,
        host_without_one: without_one,
        one_more_plugin_p99_us,
        alone_p99_us,
        host_heap,
        line_budget_us: LINE_BUDGET_US,
        plugin_budget_us: PLUGIN_BUDGET_US,
        within_budget,
    }
}

pub fn print(report: &Report) {
    let verdict = if report.within_budget { "ok" } else { "MISSED" };
    let summary = |s: &Summary| format!("p50 {:.2} µs, p99 {:.2} µs, max {:.1} µs", s.p50_us, s.p99_us, s.max_us);
    println!(
        "plugin host, {} plugins / {} patterns, {} events over {} lines:",
        report.plugins, report.patterns, report.events, report.lines
    );
    println!("  whole line (pipeline + plugins): {}; budget p99 ≤ {} µs", summary(&report.whole_line), report.line_budget_us);
    println!("  host, all plugins: {}", summary(&report.host_all));
    println!("  host, one plugin off: {}", summary(&report.host_without_one));
    let alone = report.alone_p99_us.iter().map(|(id, p99)| format!("{id} {p99:.2}")).collect::<Vec<_>>().join(", ");
    println!(
        "  one more plugin: +{:.2} µs p99; each alone p99 (µs): {alone}; budget ≤ {} µs: {verdict}",
        report.one_more_plugin_p99_us, report.plugin_budget_us
    );
    for heap in &report.host_heap {
        println!(
            "  host heap, {} plugins: loaded {:.1} MB, built {:.1} MB, after the replay {:.1} MB",
            heap.plugins, heap.loaded_mb, heap.built_mb, heap.after_replay_mb
        );
    }
}

/// The heap a host of `plugins` synthetic plugins holds, by stage: the
/// loaded plugins (each pattern compiled for its captures), the host's sets
/// built, and every line read, so the matcher caches have grown as far as
/// these lines take them.
fn host_heap(plugins: usize, lines: &[Line]) -> HostHeap {
    let before = HEAP_BYTES.load(Ordering::Relaxed);
    let held = || HEAP_BYTES.load(Ordering::Relaxed).saturating_sub(before) as f64 / (1024.0 * 1024.0);
    let loaded = synthetic_plugins(plugins);
    let loaded_mb = held();
    let mut host = PluginHost::new(loaded).expect("compiles");
    let built_mb = held();
    let mut sink = Counter::default();
    for line in lines {
        std::hint::black_box(host.read_line(line, &mut sink));
    }
    HostHeap { plugins, loaded_mb, built_mb, after_replay_mb: held() }
}

#[derive(Serialize)]
pub struct HostHeap {
    pub plugins: usize,
    pub loaded_mb: f64,
    pub built_mb: f64,
    pub after_replay_mb: f64,
}

/// Bytes in, through the pipeline and every plugin, timed per server line.
fn time_whole_lines(source: &[String], count: usize, host: &mut PluginHost, sink: &mut Counter) -> Summary {
    let mut pipeline = LinePipeline::default();
    let mut lines = Vec::with_capacity(1);
    let mut bytes = Vec::new();
    let mut samples = Vec::with_capacity(count);
    for raw in source.iter().cycle().take(count) {
        bytes.clear();
        bytes.extend_from_slice(raw.as_bytes());
        bytes.extend_from_slice(b"\n\r");
        let started = Instant::now();
        pipeline.push(&bytes, &mut lines);
        for line in lines.drain(..) {
            std::hint::black_box(host.read_line(&line, sink));
        }
        if let Some(pending) = pipeline.pending() {
            std::hint::black_box(host.read_pending(&pending));
        }
        samples.push(started.elapsed());
    }
    Summary::of(&samples).expect("lines were timed")
}

fn time_host(host: &mut PluginHost, lines: &[Line]) -> Summary {
    let mut sink = Counter::default();
    let samples: Vec<Duration> = lines
        .iter()
        .map(|line| {
            let started = Instant::now();
            std::hint::black_box(host.read_line(line, &mut sink));
            started.elapsed()
        })
        .collect();
    Summary::of(&samples).expect("lines were timed")
}

fn split(source: &[String], count: usize) -> Vec<Line> {
    let mut pipeline = LinePipeline::default();
    let mut lines = Vec::with_capacity(count);
    for raw in source.iter().cycle().take(count) {
        pipeline.push(raw.as_bytes(), &mut lines);
        pipeline.push(b"\n", &mut lines);
    }
    lines
}

#[derive(Default)]
struct Counter {
    events: usize,
}

impl PluginSink for Counter {
    fn event(&mut self, event: PluginEvent) {
        self.events += 1;
        std::hint::black_box(event);
    }
}

/// `count` plugins, each with its own words: defaults, games, then an
/// override.
fn synthetic_plugins(count: usize) -> Vec<Plugin> {
    (0..count)
        .map(|index| {
            let (id, layer) = match index {
                0 => ("bench-defaults".to_owned(), "default"),
                last if last == count - 1 => ("bench-override".to_owned(), "override"),
                game => (format!("bench-game-{game}"), "game"),
            };
            let servers = if layer == "default" { "" } else { "[servers]\nhosts = [\"bench.example\"]\n" };
            let manifest = format!(
                "id = \"{id}\"\nname = \"{id}\"\nversion = \"1.0.0\"\napi = 1\nlicense = \"MIT\"\nlayer = \"{layer}\"\n\
                 {servers}[points]\ntext = \"text.toml\"\n"
            );
            let text = text_toml(index);
            Plugin::from_files(|file| Ok(if file == "text.toml" { text.clone() } else { manifest.clone() }))
                .unwrap_or_else(|error| panic!("{error}"))
        })
        .collect()
}

fn text_toml(plugin: usize) -> String {
    let word = |rule: usize| WORDS[(plugin * 7 + rule) % WORDS.len()];
    let mut text = String::new();
    for rule in 0..EVENT_RULES {
        let pattern = EVENT_TEMPLATES[rule % EVENT_TEMPLATES.len()].replace("{w}", word(rule));
        writeln!(text, "[[pattern]]\nname = \"rule-{rule}\"\npattern = '''{pattern}'''\n").unwrap();
    }
    for rule in 0..COLOR_RULES {
        let fg = rule % 16;
        writeln!(text, "[[color]]\npattern = '''\\b{}\\b'''\nfg = {fg}\n", word(EVENT_RULES + rule)).unwrap();
    }
    writeln!(text, "[[prompt]]\npattern = '''^<(?<hp>\\d+)hp {plugin}> $'''").unwrap();
    text
}
