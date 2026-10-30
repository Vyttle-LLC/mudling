//! The line pipeline's share of the §6 "one server line ≤ 20 µs" budget:
//! each replayed line goes through split, decode and ANSI strip on its own,
//! so every line gets its own timing.

use mudling_core::line::LinePipeline;
use mudling_core::stats::Summary;
use serde::Serialize;
use std::time::Instant;

/// Leaves the rest of the 20 µs line budget to plugins.
const BUDGET_US: f64 = 5.0;
const MIN_LINES: usize = 100_000;

#[derive(Serialize)]
pub struct Report {
    lines: usize,
    per_line: Summary,
    budget_us: f64,
    pub within_budget: bool,
}

/// Replays `source` (at least [`MIN_LINES`] lines, starting over as needed),
/// each line ended as Diku-family servers end them, with `\n\r`.
pub fn run(source: &[String]) -> Report {
    let count = source.len().max(MIN_LINES);
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
        samples.push(started.elapsed());
        lines.clear();
    }
    let per_line = Summary::of(&samples).expect("lines were timed");
    Report {
        lines: count,
        within_budget: per_line.p99_us <= BUDGET_US,
        per_line,
        budget_us: BUDGET_US,
    }
}

pub fn print(report: &Report) {
    println!(
        "line pipeline over {} lines (split + decode + ANSI strip): p50 {:.2} µs, p99 {:.2} µs, max {:.1} µs; budget p99 ≤ {} µs: {}",
        report.lines,
        report.per_line.p50_us,
        report.per_line.p99_us,
        report.per_line.max_us,
        report.budget_us,
        if report.within_budget { "ok" } else { "MISSED" }
    );
}
