//! The plugin host's share of the §6 line budgets, with a synthetic plugin
//! set of 300 patterns over six plugins:
//!
//! - **whole line**: split, decode, ANSI strip and every plugin, ≤ 20 µs p99;
//! - **one more plugin** (§11's measure): the host's p99 with all six minus
//!   its p99 with one off, ≤ 5 µs.
//!
//! For information only, it also reports:
//! - each plugin's cost as the host's own sampling times it, an upper bound
//!   on its share, since it runs the plugin's rules one by one;
//! - what the lines that sample cost the line thread;
//! - the heap a host holds after replaying every line, caches included, for
//!   a realistic set (defaults, a 150-pattern game, an override) and for six
//!   and twenty uniform plugins.

use crate::HEAP_BYTES;
use mudling_core::line::{Line, LinePipeline};
use mudling_core::plugin::points::Text;
use mudling_core::plugin::{DEFAULT_COST_SAMPLING, Plugin, PluginEvent, PluginHost, PluginSink};
use mudling_core::stats::Summary;
use serde::Serialize;
use std::fmt::Write;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const LINE_BUDGET_US: f64 = 20.0;
pub(crate) const PLUGIN_BUDGET_US: f64 = 5.0;
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
    lines: usize,
    plugins: usize,
    patterns: usize,
    events: usize,
    /// Line pipeline plus every plugin, per server line.
    whole_line: Summary,
    /// The host alone, all plugins on, per line.
    host_all: Summary,
    /// The host alone, one game plugin off.
    host_without_one: Summary,
    one_more_plugin_p99_us: f64,
    /// The host alone, all plugins on, on the lines it also sampled costs.
    sampled_lines: Summary,
    /// Each plugin's p99 as the host's sampling timed it: an upper bound.
    sampled_p99_us: Vec<(String, f64)>,
    host_heap: Vec<HostHeap>,
    line_budget_us: f64,
    plugin_budget_us: f64,
    pub within_budget: bool,
}

pub fn run(source: &[String]) -> Report {
    let count = source.len().max(MIN_LINES);
    let plugins = synthetic_plugins(PLUGINS);
    let patterns = pattern_count(&plugins);

    let mut host = PluginHost::new(plugins.clone()).expect("the synthetic plugins compile");
    let mut sink = Counter::default();
    let whole_line = time_whole_lines(source, count, &mut host, &mut sink);

    let lines = split(source, count);
    let all_lines = time_host(&mut PluginHost::new(plugins.clone()).expect("compiles"), &lines);
    let without: Vec<Plugin> = plugins.iter().filter(|plugin| plugin.id().as_str() != "bench-game-4").cloned().collect();
    let without_one = summary(&time_host(&mut PluginHost::new(without).expect("compiles"), &lines));
    // The host samples the 256th line it reads, the 512th, and so on.
    let every = DEFAULT_COST_SAMPLING as usize;
    let sampled: Vec<Duration> = all_lines.iter().skip(every - 1).step_by(every).copied().collect();    let all = summary(&all_lines);

    let mut sampling = PluginHost::new(plugins).expect("compiles");
    sampling.set_cost_sampling(1);
    time_host(&mut sampling, &lines);
    let sampled_p99_us: Vec<(String, f64)> = sampling
        .costs()
        .into_iter()
        .map(|cost| (cost.plugin.to_string(), cost.per_line.map_or(0.0, |summary| summary.p99_us)))
        .collect();
    drop(sampling);
    let host_heap = vec![
        host_heap("realistic", realistic_plugins, &lines),
        host_heap("uniform", || synthetic_plugins(PLUGINS), &lines),
        host_heap("uniform", || synthetic_plugins(MANY_PLUGINS), &lines),
    ];

    let one_more_plugin_p99_us = all.p99_us - without_one.p99_us;
    let within_budget = whole_line.p99_us <= LINE_BUDGET_US && one_more_plugin_p99_us <= PLUGIN_BUDGET_US;
    Report {
        lines: count,
        plugins: PLUGINS,
        patterns,
        events: sink.events,
        whole_line,
        host_all: all,
        host_without_one: without_one,
        one_more_plugin_p99_us,
        sampled_lines: summary(&sampled),
        sampled_p99_us,
        host_heap,
        line_budget_us: LINE_BUDGET_US,
        plugin_budget_us: PLUGIN_BUDGET_US,
        within_budget,
    }
}

pub fn print(report: &Report) {
    let verdict = |ok: bool| if ok { "ok" } else { "MISSED" };
    let summary = |s: &Summary| format!("p50 {:.2} µs, p99 {:.2} µs, max {:.1} µs", s.p50_us, s.p99_us, s.max_us);
    println!(
        "plugin host, {} plugins / {} patterns, {} events over {} lines:",
        report.plugins, report.patterns, report.events, report.lines
    );
    println!(
        "  whole line (pipeline + plugins): {}; budget p99 ≤ {} µs: {}",
        summary(&report.whole_line),
        report.line_budget_us,
        verdict(report.whole_line.p99_us <= report.line_budget_us)
    );
    println!("  host, all plugins: {}", summary(&report.host_all));
    println!("  host, one plugin off: {}", summary(&report.host_without_one));
    println!(
        "  one more plugin: +{:.2} µs p99; budget ≤ {} µs: {}",
        report.one_more_plugin_p99_us,
        report.plugin_budget_us,
        verdict(report.one_more_plugin_p99_us <= report.plugin_budget_us)
    );
    let sampled = report.sampled_p99_us.iter().map(|(id, p99)| format!("{id} {p99:.2}")).collect::<Vec<_>>();
    println!("  each plugin as sampled, p99 (µs, an upper bound; not gated): {}", sampled.join(", "));
    println!("  host on the lines that also sample (every {DEFAULT_COST_SAMPLING}th): {}", summary(&report.sampled_lines));
    for heap in &report.host_heap {
        println!(
            "  host heap, {} ({} plugins / {} patterns): loaded {:.1} MB, built {:.1} MB, after the replay {:.1} MB; \
             host p99 {:.2} µs",
            heap.set,
            heap.plugins,
            heap.patterns,
            heap.loaded_mb,
            heap.built_mb,
            heap.after_replay_mb,
            heap.per_line.p99_us
        );
    }
}

/// The heap a host of the plugins `load` makes holds, by stage: the loaded
/// plugins (each pattern compiled for its captures), the host's matchers
/// built, and every line read, so the caches have grown as far as these
/// lines take them. Also times each line, with the default cost sampling.
fn host_heap(set: &str, load: impl FnOnce() -> Vec<Plugin>, lines: &[Line]) -> HostHeap {
    let mut samples = Vec::with_capacity(lines.len());
    let before = HEAP_BYTES.load(Ordering::Relaxed);
    let held = || HEAP_BYTES.load(Ordering::Relaxed).saturating_sub(before) as f64 / (1024.0 * 1024.0);
    let loaded = load();
    let loaded_mb = held();
    let plugins = loaded.len();
    let patterns = pattern_count(&loaded);
    let mut host = PluginHost::new(loaded).expect("compiles");
    let built_mb = held();
    let mut sink = Counter::default();
    for line in lines {
        let started = Instant::now();
        std::hint::black_box(host.read_line(line, &mut sink));
        samples.push(started.elapsed());
    }
    let after_replay_mb = held();
    let per_line = summary(&samples);
    HostHeap { set: set.to_owned(), plugins, patterns, loaded_mb, built_mb, after_replay_mb, per_line }
}

fn pattern_count(plugins: &[Plugin]) -> usize {
    let count = |text: &Text| text.patterns.len() + text.colors.len() + text.prompts.len() + usize::from(text.pager.is_some());
    plugins.iter().filter_map(|plugin| plugin.points.text.as_ref()).map(count).sum()
}

fn summary(samples: &[Duration]) -> Summary {
    Summary::of(samples).expect("lines were timed")
}

#[derive(Serialize)]
struct HostHeap {
    set: String,
    plugins: usize,
    patterns: usize,
    loaded_mb: f64,
    built_mb: f64,
    after_replay_mb: f64,
    /// The host alone, per line, while the caches grew.
    per_line: Summary,
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
    summary(&samples)
}

/// The host alone over `lines`: each line's time, in order.
fn time_host(host: &mut PluginHost, lines: &[Line]) -> Vec<Duration> {
    let mut sink = Counter::default();
    lines
        .iter()
        .map(|line| {
            let started = Instant::now();
            std::hint::black_box(host.read_line(line, &mut sink));
            started.elapsed()
        })
        .collect()
}

pub(crate) fn split(source: &[String], count: usize) -> Vec<Line> {
    let mut pipeline = LinePipeline::default();
    let mut lines = Vec::with_capacity(count);
    for raw in source.iter().cycle().take(count) {
        pipeline.push(raw.as_bytes(), &mut lines);
        pipeline.push(b"\n", &mut lines);
    }
    lines
}

#[derive(Default)]
pub(crate) struct Counter {
    pub(crate) events: usize,
}

impl PluginSink for Counter {
    fn event(&mut self, event: PluginEvent) {
        self.events += 1;
        std::hint::black_box(event);
    }
}

/// `count` plugins of [`EVENT_RULES`] patterns and [`COLOR_RULES`] colors
/// each: defaults, games, then an override.
fn synthetic_plugins(count: usize) -> Vec<Plugin> {
    (0..count)
        .map(|index| {
            let id = match index {
                0 => "bench-defaults".to_owned(),
                last if last == count - 1 => "bench-override".to_owned(),
                game => format!("bench-game-{game}"),
            };
            synthetic_plugin(index, &id, layer(index, count), EVENT_RULES, COLOR_RULES)
        })
        .collect()
}

/// What one server might run: defaults, one game plugin of 150 patterns and
/// a small override.
pub(crate) fn realistic_plugins() -> Vec<Plugin> {
    vec![
        synthetic_plugin(0, "bench-defaults", "default", 25, 5),
        synthetic_plugin(1, "bench-game", "game", 130, 20),
        synthetic_plugin(2, "bench-override", "override", 5, 1),
    ]
}

fn layer(index: usize, count: usize) -> &'static str {
    match index {
        0 => "default",
        last if last == count - 1 => "override",
        _ => "game",
    }
}

/// A plugin with its own words, from `index`, and [`PROMPT_HINTS`] prompt
/// hints.
fn synthetic_plugin(index: usize, id: &str, layer: &str, events: usize, colors: usize) -> Plugin {
    let servers = if layer == "default" { "" } else { "[servers]\nhosts = [\"bench.example\"]\n" };
    let manifest = format!(
        "id = \"{id}\"\nname = \"{id}\"\nversion = \"1.0.0\"\napi = 1\nlicense = \"MIT\"\nlayer = \"{layer}\"\n\
         {servers}[points]\ntext = \"text.toml\"\n"
    );
    let text = text_toml(index, events, colors);
    Plugin::from_files(|file| Ok(if file == "text.toml" { text.clone() } else { manifest.clone() }))
        .unwrap_or_else(|error| panic!("{error}"))
}

fn text_toml(plugin: usize, events: usize, colors: usize) -> String {
    let word = |rule: usize| WORDS[(plugin * 7 + rule) % WORDS.len()];
    let mut text = String::new();
    for rule in 0..events {
        let pattern = EVENT_TEMPLATES[rule % EVENT_TEMPLATES.len()].replace("{w}", word(rule));
        writeln!(text, "[[pattern]]\nname = \"rule-{rule}\"\npattern = '''{pattern}'''\n").unwrap();
    }
    for rule in 0..colors {
        let fg = rule % 16;
        writeln!(text, "[[color]]\npattern = '''\\b{}\\b'''\nfg = {fg}\n", word(events + rule)).unwrap();
    }
    for hint in 0..PROMPT_HINTS {
        writeln!(text, "[[prompt]]\npattern = '''^<(?<hp>\\d+)hp {plugin}.{hint}> $'''\n").unwrap();
    }
    text
}
