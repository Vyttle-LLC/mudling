//! A plugin's cost per line against PRD §11's budget, by the bench's measure
//! (`mudling-bench`'s `game_plugin`): the host's p99 with the plugin minus
//! its p99 without. Hooks run off the line thread, so they aren't counted
//! here; their own limits are.
//!
//! The two hosts read each line in turn, so both see the same machine. Stalls
//! only ever add time, so each host's best round is its truest, and a plugin
//! over budget stays over it in every round.

use super::replay::host_with;
use crate::line::{Line, LinePipeline};
use crate::plugin::{Plugin, PluginError, PluginEvent, PluginHost, PluginSink};
use crate::stats::Summary;
use std::fmt;
use std::time::{Duration, Instant};

/// Microseconds a plugin may add to a line at p99. A debug build reads a line
/// about 10× slower than a release build, so it gets 20×: enough that a busy
/// machine can't fail a plugin that's fine, with the gate still in `cargo test`.
const BUDGET_US: f64 = if cfg!(debug_assertions) { 100.0 } else { 5.0 };
const ROUNDS: usize = 5;
/// No more rounds after this long: a plugin that slow is over budget anyway.
const ENOUGH_TIME: Duration = Duration::from_secs(2);
/// Lines each round reads: a short log goes round, a long one is cut.
const FEWEST_LINES: usize = 5_000;
const MOST_LINES: usize = 100_000;

pub struct Cost {
    pub without_us: f64,
    pub with_us: f64,
    pub budget_us: f64,
}

impl Cost {
    pub fn added_us(&self) -> f64 {
        self.with_us - self.without_us
    }

    pub fn within_budget(&self) -> bool {
        self.added_us() <= self.budget_us
    }
}

impl fmt::Display for Cost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "+{:.2} µs a line at p99 ({:.2} with, {:.2} without), budget {} µs",
            self.added_us(),
            self.with_us,
            self.without_us,
            self.budget_us
        )
    }
}

/// Measures `plugin` over `lines`, of which there must be at least one.
pub fn measure(plugin: &Plugin, lines: &[String]) -> Result<Cost, PluginError> {
    let lines = split(lines);
    let mut without = host_with(None)?;
    let mut with = host_with(Some(plugin))?;
    let (mut without_us, mut with_us) = (f64::INFINITY, f64::INFINITY);
    let started = Instant::now();
    for _ in 0..ROUNDS {
        let (without_p99, with_p99) = round(&mut without, &mut with, &lines);
        without_us = without_us.min(without_p99);
        with_us = with_us.min(with_p99);
        if started.elapsed() > ENOUGH_TIME {
            break;
        }
    }
    Ok(Cost {
        without_us,
        with_us,
        budget_us: BUDGET_US,
    })
}

fn split(source: &[String]) -> Vec<Line> {
    let count = source.len().clamp(FEWEST_LINES, MOST_LINES);
    let mut pipeline = LinePipeline::default();
    let mut lines = Vec::with_capacity(count);
    for text in source.iter().cycle().take(count) {
        pipeline.push_line(text.as_bytes(), &mut lines);
    }
    lines
}

/// Each host's p99 over `lines`.
fn round(without: &mut PluginHost, with: &mut PluginHost, lines: &[Line]) -> (f64, f64) {
    let mut without_samples = Vec::with_capacity(lines.len());
    let mut with_samples = Vec::with_capacity(lines.len());
    for (index, line) in lines.iter().enumerate() {
        // Alternate which goes first, so neither always reads a warm line.
        if index % 2 == 0 {
            without_samples.push(time(without, line));
            with_samples.push(time(with, line));
        } else {
            with_samples.push(time(with, line));
            without_samples.push(time(without, line));
        }
    }
    (p99(&without_samples), p99(&with_samples))
}

fn time(host: &mut PluginHost, line: &Line) -> Duration {
    let started = Instant::now();
    std::hint::black_box(host.read_line(line, &mut Discard));
    started.elapsed()
}

fn p99(samples: &[Duration]) -> f64 {
    Summary::of(samples).map_or(0.0, |summary| summary.p99_us)
}

struct Discard;

impl PluginSink for Discard {
    fn event(&mut self, event: PluginEvent) {
        std::hint::black_box(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cost(without_us: f64, with_us: f64) -> Cost {
        Cost {
            without_us,
            with_us,
            budget_us: 5.0,
        }
    }

    #[test]
    fn a_plugin_is_judged_by_what_it_adds_to_the_host() {
        assert!(cost(10.0, 15.0).within_budget());
        assert!(!cost(10.0, 15.1).within_budget());
        assert!(cost(10.0, 8.0).within_budget(), "a faster run adds nothing");
    }
}
