//! `mudling-bench compare <base-binary>`: fails when this build loses more
//! than 10% against another build of the benchmark, on one machine.
//!
//! A shared machine can't be trusted across runs, so a stored baseline is out.
//! Both builds run in one job, in turns (base, head, head, base, …), so a slow
//! spell hits both alike. Stalls only ever add time, so each build's best
//! round is its truest: a block of rounds compares the lowest value each
//! reached. One lucky round can still set that, so a metric that looks worse
//! gets fresh blocks, and it's lost only if it loses in every one. Over 30
//! rounds of one build on a loaded machine, that fails 0.1% of same-build
//! pairings, 5% of a true +5%, 82% of a true +10%, and every +15%.
//!
//! Both builds replay the same log, this build's fixture, written to a file.
//! A metric the base's report doesn't have is new: it's skipped.

use serde::Serialize;
use serde_json::Value;
use std::path::Path;
use std::process::{Command, ExitCode};

/// More than this much worse is a loss.
const MOST_LOST: f64 = 1.10;
const ROUNDS_PER_BLOCK: usize = 3;
const BLOCKS: usize = 3;

/// What the budgets are measured by. Differences of two p99s (the "one more
/// plugin" budgets) aren't here: they're mostly noise, and both sides of the
/// difference are. Nor are two p99s that one build can't hold steady, which
/// `--check` still gates: a move's enqueue times the writer thread being
/// woken (9 to 35 µs), and a store frame's apply is too short to time (0.7
/// to 1.7 µs). Work too short for a clock tick to time per sample is compared
/// by its mean, which many samples resolve where a percentile can't.
const METRICS: [&str; 20] = [
    "/append/mean_us",
    "/find/p99_us",
    "/line_pipeline/per_line/mean_us",
    "/plugin_host/whole_line/p99_us",
    "/plugin_host/host_all/p99_us",
    "/defaults/per_line/mean_us",
    "/luau_hooks/hand_off/mean_us",
    "/luau_hooks/columns/p99_us",
    "/session/whole_line/p99_us",
    "/session/whole_line/mean_us",
    "/session/mapping/p99_us",
    "/session/wandering/p99_us",
    "/session/memory_mb",
    "/session/cold_start/p50_us",
    "/replay/by_event/line/p99_us",
    "/replay/by_event/gmcp Room.Info/p99_us",
    "/telnet/plain/per_chunk/p99_us",
    "/telnet/mccp2/per_chunk/p99_us",
    "/completion/complete/p99_us",
    "/persist/list/p50_us",
];

#[derive(Debug, Serialize)]
struct Comparison {
    metric: String,
    base: f64,
    head: f64,
    lost: bool,
}

#[derive(Serialize)]
struct Report {
    rounds: usize,
    comparisons: Vec<Comparison>,
    new_metrics: Vec<&'static str>,
}

/// Runs `compare <base-binary> [--json <report.json>]`.
pub fn run(args: impl Iterator<Item = String>) -> ExitCode {
    match compare(args) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => {
            eprintln!("a metric lost more than 10% against the base");
            ExitCode::FAILURE
        }
        Err(message) => {
            eprintln!(
                "{message}\nusage: mudling-bench compare <base-binary> [--json <report.json>]"
            );
            ExitCode::from(2)
        }
    }
}

fn compare(mut args: impl Iterator<Item = String>) -> Result<bool, String> {
    let base = args.next().ok_or("compare needs the base's benchmark")?;
    let mut json = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json = Some(args.next().ok_or("--json needs a path")?),
            other => return Err(format!("unknown option {other}")),
        }
    }
    let head = std::env::current_exe().map_err(|error| error.to_string())?;
    let folder = tempfile::tempdir().map_err(|error| error.to_string())?;
    let log = folder.path().join("fixture.log");
    std::fs::write(&log, crate::FIXTURE).map_err(|error| error.to_string())?;
    let run = |binary: &Path, name: &str| report(binary, &log, &folder.path().join(name));
    let block = || -> Result<Vec<Comparison>, String> {
        let (mut bases, mut heads) = (Vec::new(), Vec::new());
        for round in 0..ROUNDS_PER_BLOCK {
            // In turns, so neither build always runs first.
            if round % 2 == 0 {
                bases.push(run(Path::new(&base), "base.json")?);
                heads.push(run(&head, "head.json")?);
            } else {
                heads.push(run(&head, "head.json")?);
                bases.push(run(Path::new(&base), "base.json")?);
            }
        }
        check_metrics(&heads[0])?;
        Ok(compared(&bases, &heads))
    };

    let mut comparisons = block()?;
    let mut blocks = 1;
    while blocks < BLOCKS && comparisons.iter().any(|comparison| comparison.lost) {
        keep_repeated_losses(&mut comparisons, &block()?);
        blocks += 1;
    }
    let report = Report {
        rounds: blocks * ROUNDS_PER_BLOCK,
        new_metrics: METRICS
            .into_iter()
            .filter(|metric| !comparisons.iter().any(|known| known.metric == *metric))
            .collect(),
        comparisons,
    };
    print(&report);
    if let Some(path) = json {
        crate::write_report(&path, &report)?;
    }
    Ok(!report.comparisons.iter().any(|comparison| comparison.lost))
}

fn report(binary: &Path, log: &Path, json: &Path) -> Result<Value, String> {
    let output = Command::new(binary)
        .arg("--json")
        .arg(json)
        .arg(log)
        .output()
        .map_err(|error| format!("couldn't run {}: {error}", binary.display()))?;
    if !output.status.success() {
        return Err(format!(
            "{} failed: {}",
            binary.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let text = std::fs::read(json).map_err(|error| error.to_string())?;
    serde_json::from_slice(&text).map_err(|error| error.to_string())
}

/// A path this build's own report doesn't hold is a typo here, which would
/// otherwise pass as a new metric forever.
fn check_metrics(head: &Value) -> Result<(), String> {
    match METRICS.iter().find(|metric| head.pointer(metric).is_none()) {
        Some(metric) => Err(format!("this build's report has no {metric}")),
        None => Ok(()),
    }
}

fn compared(bases: &[Value], heads: &[Value]) -> Vec<Comparison> {
    METRICS
        .iter()
        .filter_map(|metric| {
            let base = lowest(bases, metric)?;
            let head = lowest(heads, metric)?;
            Some(Comparison {
                metric: (*metric).to_owned(),
                base,
                head,
                lost: head > base * MOST_LOST,
            })
        })
        .collect()
}

/// A loss stands only if the fresh block lost the same metric.
fn keep_repeated_losses(comparisons: &mut [Comparison], again: &[Comparison]) {
    for comparison in comparisons {
        comparison.lost &= again
            .iter()
            .any(|fresh| fresh.metric == comparison.metric && fresh.lost);
    }
}

fn lowest(reports: &[Value], metric: &str) -> Option<f64> {
    reports
        .iter()
        .filter_map(|report| report.pointer(metric)?.as_f64())
        .min_by(f64::total_cmp)
}

fn print(report: &Report) {
    println!(
        "{} rounds; best of the first {ROUNDS_PER_BLOCK} each:",
        report.rounds
    );
    for Comparison {
        metric,
        base,
        head,
        lost,
    } in &report.comparisons
    {
        println!(
            "  {metric}: base {base:.2}, head {head:.2} ({:+.1}%){}",
            (head / base - 1.0) * 100.0,
            if *lost { " LOST" } else { "" }
        );
    }
    for metric in &report.new_metrics {
        println!("  {metric}: not in the base's report");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const METRIC: &str = "/append/mean_us";

    fn reports(values: &[f64]) -> Vec<Value> {
        values
            .iter()
            .map(|value| json!({ "append": { "mean_us": value } }))
            .collect()
    }

    fn lost(bases: &[f64], heads: &[f64]) -> bool {
        let comparisons = compared(&reports(bases), &reports(heads));
        assert_eq!(comparisons[0].metric, METRIC);
        comparisons[0].lost
    }

    #[test]
    fn a_loss_over_ten_percent_fails() {
        assert!(lost(&[100.0], &[111.0]));
    }

    #[test]
    fn a_loss_within_ten_percent_passes() {
        assert!(!lost(&[100.0], &[109.0]));
    }

    #[test]
    fn each_build_counts_by_its_best_round() {
        assert!(!lost(&[100.0, 400.0], &[300.0, 105.0]));
        assert!(lost(&[100.0, 400.0], &[300.0, 150.0]));
    }

    #[test]
    fn a_loss_on_the_shortest_work_counts_too() {
        assert!(lost(&[0.08], &[0.09]));
    }

    #[test]
    fn a_metric_the_base_lacks_is_skipped() {
        let comparisons = compared(&[json!({})], &reports(&[100.0]));
        assert!(comparisons.is_empty());
    }

    #[test]
    fn a_loss_must_repeat_in_a_fresh_block() {
        let lost = |lost| {
            vec![Comparison {
                metric: METRIC.to_owned(),
                base: 100.0,
                head: 120.0,
                lost,
            }]
        };
        let mut comparisons = lost(true);
        keep_repeated_losses(&mut comparisons, &lost(true));
        assert!(comparisons[0].lost);
        keep_repeated_losses(&mut comparisons, &lost(false));
        assert!(!comparisons[0].lost);
    }

    #[test]
    fn a_metric_this_build_lacks_is_a_mistake() {
        assert!(check_metrics(&json!({})).is_err());
    }
}
