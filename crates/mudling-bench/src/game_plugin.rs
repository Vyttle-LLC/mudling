//! A real plugin's share of the line budget (PRD §11: ≤ 5 µs per line at
//! p99): the host's p99 per line over the replayed text with the plugin as
//! the game plugin beside the synthetic defaults and override, minus the
//! p99 without it. The two
//! hosts read each line in turn, so both see the same machine state. Its
//! Luau hooks run on their own thread, off the line, and aren't counted.

use crate::plugin_host::{Counter, PLUGIN_BUDGET_US, realistic_plugins, split};
use mudling_core::line::Line;
use mudling_core::plugin::{Plugin, PluginHost};
use mudling_core::stats::Summary;
use serde::Serialize;
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Serialize)]
pub struct Report {
    pub plugin: String,
    pub patterns: usize,
    pub lines: usize,
    pub events: usize,
    pub host_without: Summary,
    pub host_with: Summary,
    pub marginal_p99_us: f64,
    /// The plugin timed alone by the host's own cost sampling: an upper
    /// bound on its share, reported but not gated on.
    pub alone_p99_us: f64,
    pub budget_us: f64,
    pub within_budget: bool,
}

pub fn run(dir: &Path, source: &[String]) -> Result<Report, String> {
    let plugin = Plugin::load(dir).map_err(|error| error.to_string())?;
    let id = plugin.id().clone();
    let patterns = plugin
        .points
        .text
        .as_ref()
        .map_or(0, |text| text.patterns.len());
    let compile =
        |plugins: Vec<Plugin>| PluginHost::new(plugins).map_err(|error| error.to_string());
    // A server runs one game plugin: this one takes the synthetic game's place.
    let others = || {
        realistic_plugins()
            .into_iter()
            .filter(|other| other.id().as_str() != "bench-game")
    };
    let mut without = compile(others().collect())?;
    let mut with = compile(others().chain([plugin.clone()]).collect())?;

    let lines = split(source, source.len());
    let (mut without_samples, mut with_samples) = (
        Vec::with_capacity(lines.len()),
        Vec::with_capacity(lines.len()),
    );
    let mut sink = Counter::default();
    let mut ignored = Counter::default();
    for (index, line) in lines.iter().enumerate() {
        // Alternate which goes first, so neither always reads a warm line.
        if index % 2 == 0 {
            without_samples.push(time(&mut without, line, &mut ignored));
            with_samples.push(time(&mut with, line, &mut sink));
        } else {
            with_samples.push(time(&mut with, line, &mut sink));
            without_samples.push(time(&mut without, line, &mut ignored));
        }
    }
    let host_without = Summary::of(&without_samples).ok_or("no lines to replay")?;
    let host_with = Summary::of(&with_samples).ok_or("no lines to replay")?;
    // Sampling every line in a pass of its own, so it doesn't slow the
    // timed one.
    let mut sampled = compile(others().chain([plugin]).collect())?;
    sampled.set_cost_sampling(1);
    let mut uncounted = Counter::default();
    lines
        .iter()
        .for_each(|line| drop(sampled.read_line(line, &mut uncounted)));
    let alone_p99_us = sampled
        .costs()
        .into_iter()
        .find(|cost| cost.plugin == id)
        .and_then(|cost| cost.per_line)
        .map_or(0.0, |summary| summary.p99_us);
    let marginal_p99_us = host_with.p99_us - host_without.p99_us;
    Ok(Report {
        plugin: id.to_string(),
        patterns,
        lines: lines.len(),
        events: sink.events.saturating_sub(ignored.events),
        within_budget: marginal_p99_us <= PLUGIN_BUDGET_US,
        host_without,
        host_with,
        marginal_p99_us,
        alone_p99_us,
        budget_us: PLUGIN_BUDGET_US,
    })
}

fn time(host: &mut PluginHost, line: &Line, sink: &mut Counter) -> Duration {
    let started = Instant::now();
    std::hint::black_box(host.read_line(line, sink));
    started.elapsed()
}

pub fn print(report: &Report) {
    let verdict = if report.within_budget { "ok" } else { "MISSED" };
    println!(
        "plugin {} ({} patterns) over {} lines, {} events: host p99 {:.2} µs without, {:.2} µs with; \
         +{:.2} µs p99; budget ≤ {} µs: {verdict}; alone p99 {:.2} µs (sampled, an upper bound; informational)",
        report.plugin,
        report.patterns,
        report.lines,
        report.events,
        report.host_without.p99_us,
        report.host_with.p99_us,
        report.marginal_p99_us,
        report.budget_us,
        report.alone_p99_us
    );
}
