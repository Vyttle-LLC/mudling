//! `mudling plugin test`: replays logs through a plugin and says what each
//! pattern caught, what went wrong, and whether it stayed within budget. The
//! checks are the core's, which the app runs on a plugin added from a link.

use mudling_core::plugin::Plugin;
use mudling_core::plugin::check::{
    self, Caught, EXPECTED_EXTENSION, Failure, LOG_EXTENSION, Read, TESTS_FOLDER,
};
use mudling_core::replay::load_logs;
use std::fs;
use std::path::{Path, PathBuf};

pub enum Mode {
    /// Replays one log, with nothing to compare it to.
    Log(PathBuf),
    /// Replays `tests/*.txt`. `update` rewrites their `.expected` files to
    /// what they produced, rather than comparing.
    OwnTests { update: bool },
}

/// Setting this skips the cost a line, which depends on how fast the machine
/// is. The command's own tests set it to test everything else; a timing test
/// leaves it unset.
const SKIP_COST_VARIABLE: &str = "MUDLING_SKIP_COST_CHECK";

/// Whether the plugin passed; an error means the command couldn't run.
pub fn run(folder: &Path, mode: &Mode) -> Result<bool, String> {
    let judge_cost = std::env::var_os(SKIP_COST_VARIABLE).is_none();
    let plugin = match Plugin::load(folder) {
        Ok(plugin) => plugin,
        Err(error) => return Ok(failed_to_load(error.into())),
    };
    let manifest = &plugin.manifest;
    println!("plugin {} {}", manifest.id, manifest.version);
    let logs = match mode {
        Mode::Log(log) => vec![log.clone()],
        Mode::OwnTests { .. } => own_tests(folder)?,
    };
    let mut passed = !logs.is_empty();
    if !passed {
        println!("fail {}", check::no_tests());
    }
    for log in &logs {
        let lines = load_logs(std::slice::from_ref(log))
            .map_err(|error| format!("couldn't read {}: {error}", log.display()))?;
        if lines.is_empty() {
            return Err(format!("{} has no server text", log.display()));
        }
        println!("log {}: {} lines", log.display(), lines.len());
        let replay = match check::replay(&plugin, &lines) {
            Ok(replay) => replay,
            Err(error) => return Ok(failed_to_load(error.into())),
        };
        print_caught(&replay.caught);
        let hooks = check::hook_failures(&replay);
        for failure in &hooks {
            println!("fail {failure}");
        }
        passed &= hooks.is_empty();
        if judge_cost {
            let cost = match check::measure_cost(&plugin, &lines) {
                Ok(cost) => cost,
                Err(error) => return Ok(failed_to_load(error.into())),
            };
            if cost.within_budget() {
                println!("cost {cost}");
            } else {
                println!("fail {}", Failure::Budget(cost.to_string()));
                passed = false;
            }
        }
        if let Mode::OwnTests { update } = mode {
            passed &= compare(log, &replay.transcript, *update)?;
        }
    }
    println!("{}", if passed { "passed" } else { "failed" });
    Ok(passed)
}

fn failed_to_load(failure: Failure) -> bool {
    println!("fail {failure}");
    println!("failed");
    false
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
    let expected = fs::read_to_string(&path).ok();
    let failure = check::compare(&path.display().to_string(), expected.as_deref(), transcript);
    if let Some(failure) = &failure {
        println!("fail {failure}");
    }
    Ok(failure.is_none())
}
