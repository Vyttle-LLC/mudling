//! `mudling plugin test`: replays logs through a plugin and says what each
//! pattern caught, what went wrong, and whether it stayed within budget.
//!
//! Its own tests are `tests/<name>.txt`, a log of made-up text, each with a
//! `tests/<name>.expected`, the transcript it should produce.

use crate::budget::{self, Cost};
use crate::patterns::Read;
use crate::replay::{self, Caught, Replay};
use mudling_core::plugin::{Plugin, PluginError};
use mudling_core::replay as logs;
use std::fs;
use std::path::{Path, PathBuf};

const TESTS_FOLDER: &str = "tests";
const LOG_EXTENSION: &str = "txt";
const EXPECTED_EXTENSION: &str = "expected";

pub enum Mode {
    /// Replays one log, with nothing to compare it to.
    Log(PathBuf),
    /// Replays `tests/*.txt`. `update` rewrites their `.expected` files to
    /// what they produced, rather than comparing.
    OwnTests { update: bool },
}

/// Whether the plugin passed; an error means the command couldn't run.
pub fn run(folder: &Path, mode: &Mode) -> Result<bool, String> {
    let plugin = match Plugin::load(folder) {
        Ok(plugin) => plugin,
        Err(error) => return Ok(failed_to_load(&error)),
    };
    let manifest = &plugin.manifest;
    println!("plugin {} {}", manifest.id, manifest.version);
    let logs = match mode {
        Mode::Log(log) => vec![log.clone()],
        Mode::OwnTests { .. } => own_tests(folder)?,
    };
    let mut passed = !logs.is_empty();
    if !passed {
        println!("fail tests: no {TESTS_FOLDER}/*.{LOG_EXTENSION} to replay");
    }
    for log in &logs {
        let lines = logs::load_logs(std::slice::from_ref(log))
            .map_err(|error| format!("couldn't read {}: {error}", log.display()))?;
        if lines.is_empty() {
            return Err(format!("{} has no server text", log.display()));
        }
        println!("log {}: {} lines", log.display(), lines.len());
        let (replay, cost) = match replay_and_measure(&plugin, &lines) {
            Ok(results) => results,
            Err(error) => return Ok(failed_to_load(&error)),
        };
        print_caught(&replay.caught);
        passed &= report_hooks(&replay);
        passed &= report_cost(&cost);
        if let Mode::OwnTests { update } = mode {
            passed &= compare(log, &replay.transcript, *update)?;
        }
    }
    println!("{}", if passed { "passed" } else { "failed" });
    Ok(passed)
}

fn replay_and_measure(plugin: &Plugin, lines: &[String]) -> Result<(Replay, Cost), PluginError> {
    Ok((
        replay::replay(plugin, lines)?,
        budget::measure(plugin, lines)?,
    ))
}

fn failed_to_load(error: &PluginError) -> bool {
    println!("fail load: {}", one_line(&error.to_string()));
    println!("failed");
    false
}

/// A reason that may run over several lines, as TOML errors and Luau
/// tracebacks do, on one: a script reads a `fail` line alone.
fn one_line(reason: &str) -> String {
    reason.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `tests/*.txt`, in name order. No folder means no tests.
fn own_tests(folder: &Path) -> Result<Vec<PathBuf>, String> {
    let tests = folder.join(TESTS_FOLDER);
    let Ok(entries) = fs::read_dir(&tests) else {
        return Ok(Vec::new());
    };
    let could_not = |error| format!("couldn't read {}: {error}", tests.display());
    let mut logs = Vec::new();
    for entry in entries {
        let path = entry.map_err(could_not)?.path();
        if path.extension().is_some_and(|found| found == LOG_EXTENSION) {
            logs.push(path);
        }
    }
    logs.sort();
    Ok(logs)
}

fn print_caught(caught: &[Caught]) {
    for pattern in caught {
        let rule = &pattern.rule;
        match &rule.read {
            Read::Event { name, .. } => {
                println!("caught {} {} {name}", pattern.lines, rule.label);
            }
            Read::Line => println!("caught {} {}", pattern.lines, rule.label),
            Read::Elsewhere(why) => println!("skipped {}: {why}", rule.label),
        }
        for example in &pattern.examples {
            println!("  {example}");
        }
    }
}

/// Says what hooks did wrong; returns whether they did nothing wrong.
fn report_hooks(replay: &Replay) -> bool {
    for ((hook, overran), failures) in &replay.failures {
        let (times, first) = (failures.times, one_line(&failures.first));
        if *overran {
            println!("fail budget: the hook {hook} overran {times} times: {first}");
        } else {
            println!("fail hook: {hook} failed {times} times: {first}");
        }
    }
    replay.failures.is_empty()
}

fn report_cost(cost: &Cost) -> bool {
    let measured = format!(
        "+{:.2} µs a line at p99 ({:.2} with, {:.2} without), budget {} µs",
        cost.added_us(),
        cost.with_us,
        cost.without_us,
        cost.budget_us
    );
    let within = cost.within_budget();
    println!(
        "{} {measured}",
        if within { "cost" } else { "fail budget:" }
    );
    within
}

/// Compares `transcript` with the file beside `log`, or rewrites that file.
fn compare(log: &Path, transcript: &[String], update: bool) -> Result<bool, String> {
    let path = log.with_extension(EXPECTED_EXTENSION);
    if update {
        let text: String = transcript.iter().map(|line| format!("{line}\n")).collect();
        fs::write(&path, text)
            .map_err(|error| format!("couldn't write {}: {error}", path.display()))?;
        println!("updated {}", path.display());
        return Ok(true);
    }
    let Ok(expected) = fs::read_to_string(&path) else {
        println!("fail expected: couldn't read {}", path.display());
        return Ok(false);
    };
    let expected: Vec<&str> = expected.lines().collect();
    let difference = (0..expected.len().max(transcript.len()))
        .find(|&at| expected.get(at).copied() != transcript.get(at).map(String::as_str));
    if let Some(at) = difference {
        let shown = |line: Option<&str>| line.unwrap_or("(nothing)").to_owned();
        println!(
            "fail expected: {} line {}: wanted {}, got {}",
            path.display(),
            at + 1,
            shown(expected.get(at).copied()),
            shown(transcript.get(at).map(String::as_str))
        );
    }
    Ok(difference.is_none())
}
