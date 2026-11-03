//! What `mudling plugin test` checks (PRD §11), for the command line and for
//! the app, which checks a plugin added from a git link the same way before
//! installing it: it loads, its own tests replay to what they expect, its
//! hooks don't fail, and it stays within its budget a line.
//!
//! A plugin's own tests are `tests/<name>.txt`, a log of made-up text, each
//! with a `tests/<name>.expected`, the transcript it should produce.

mod budget;
mod patterns;
mod replay;

pub use budget::Cost;
pub use patterns::{Read, Rule};
pub use replay::{Caught, HookFailures, Replay};

use crate::git::FolderFiles;
use crate::plugin::{Plugin, PluginError};
use std::fmt;

pub const TESTS_FOLDER: &str = "tests";
pub const LOG_EXTENSION: &str = "txt";
pub const EXPECTED_EXTENSION: &str = "expected";

/// Why a plugin failed: what follows `fail ` on `mudling plugin test`'s line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    Load(String),
    /// Its tests can't be replayed.
    Tests(String),
    Hook {
        hook: String,
        times: usize,
        first: String,
    },
    Overran {
        hook: String,
        times: usize,
        first: String,
    },
    /// Over budget a line, by this measure.
    Budget(String),
    /// No transcript to compare with.
    NoExpected(String),
    Expected {
        file: String,
        line: usize,
        wanted: String,
        got: String,
    },
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Load(why) => write!(f, "load: {why}"),
            Self::Tests(why) => write!(f, "tests: {why}"),
            Self::Hook { hook, times, first } => {
                write!(f, "hook: {hook} failed {times} times: {first}")
            }
            Self::Overran { hook, times, first } => {
                write!(f, "budget: the hook {hook} overran {times} times: {first}")
            }
            Self::Budget(measured) => write!(f, "budget: {measured}"),
            Self::NoExpected(file) => write!(f, "expected: couldn't read {file}"),
            Self::Expected {
                file,
                line,
                wanted,
                got,
            } => write!(
                f,
                "expected: {file} line {line}: wanted {wanted}, got {got}"
            ),
        }
    }
}

impl From<PluginError> for Failure {
    fn from(error: PluginError) -> Self {
        Self::Load(one_line(&error.to_string()))
    }
}

/// Replays `lines` through `plugin` and measures its cost a line. An error
/// means the plugin can't run at all.
pub fn replay_and_measure(
    plugin: &Plugin,
    lines: &[String],
) -> Result<(Replay, Cost), PluginError> {
    Ok((
        replay::replay(plugin, lines)?,
        budget::measure(plugin, lines)?,
    ))
}

/// What the hooks did wrong in `replay`.
pub fn hook_failures(replay: &Replay) -> Vec<Failure> {
    let failures = replay.failures.iter();
    failures
        .map(|((hook, overran), failures)| {
            let (hook, times, first) = (hook.clone(), failures.times, one_line(&failures.first));
            match overran {
                true => Failure::Overran { hook, times, first },
                false => Failure::Hook { hook, times, first },
            }
        })
        .collect()
}

/// `transcript` against `expected`, the file named `file`, if it could be
/// read.
pub fn compare(file: &str, expected: Option<&str>, transcript: &[String]) -> Option<Failure> {
    let Some(expected) = expected else {
        return Some(Failure::NoExpected(file.to_owned()));
    };
    let expected: Vec<&str> = expected.lines().collect();
    let at = (0..expected.len().max(transcript.len()))
        .find(|&at| expected.get(at).copied() != transcript.get(at).map(String::as_str))?;
    let shown = |line: Option<&str>| line.unwrap_or("(nothing)").to_owned();
    Some(Failure::Expected {
        file: file.to_owned(),
        line: at + 1,
        wanted: shown(expected.get(at).copied()),
        got: shown(transcript.get(at).map(String::as_str)),
    })
}

/// Checks `plugin` against its own tests among `files`, its folder's files,
/// as `mudling plugin test` does with no log given. The first failure says
/// why it didn't pass.
pub fn own_tests(plugin: &Plugin, files: &FolderFiles) -> Result<(), Failure> {
    check_own_tests(plugin, files, true)
}

/// [`own_tests`] without the cost a line, which depends on how fast the
/// machine is: for tests of everything else a plugin must pass.
pub fn own_tests_except_cost(plugin: &Plugin, files: &FolderFiles) -> Result<(), Failure> {
    check_own_tests(plugin, files, false)
}

fn check_own_tests(plugin: &Plugin, files: &FolderFiles, judge_cost: bool) -> Result<(), Failure> {
    let logs: Vec<(&String, &Vec<u8>)> = (files.iter())
        .filter(|(path, _)| is_test_log(path))
        .collect();
    if logs.is_empty() {
        return Err(no_tests());
    }
    for (path, bytes) in logs {
        let log = String::from_utf8_lossy(bytes);
        let lines: Vec<String> = (crate::replay::server_lines(&log).into_iter())
            .map(str::to_owned)
            .collect();
        if lines.is_empty() {
            return Err(Failure::Tests(format!("{path} has no server text")));
        }
        let replay = replay::replay(plugin, &lines)?;
        if let Some(failure) = hook_failures(&replay).into_iter().next() {
            return Err(failure);
        }
        if judge_cost {
            let cost = budget::measure(plugin, &lines)?;
            if !cost.within_budget() {
                return Err(Failure::Budget(cost.to_string()));
            }
        }
        let expected_path = expected_path(path);
        let expected = (files.get(&expected_path)).map(|bytes| String::from_utf8_lossy(bytes));
        if let Some(failure) = compare(&expected_path, expected.as_deref(), &replay.transcript) {
            return Err(failure);
        }
    }
    Ok(())
}

pub fn no_tests() -> Failure {
    Failure::Tests(format!("no {TESTS_FOLDER}/*.{LOG_EXTENSION} to replay"))
}

/// `tests/<name>.txt`, directly in the tests folder.
fn is_test_log(path: &str) -> bool {
    path.strip_prefix(&format!("{TESTS_FOLDER}/"))
        .and_then(|name| name.strip_suffix(&format!(".{LOG_EXTENSION}")))
        .is_some_and(|name| !name.is_empty() && !name.contains('/'))
}

fn expected_path(log: &str) -> String {
    let stem = log.strip_suffix(LOG_EXTENSION).unwrap_or(log);
    format!("{stem}{EXPECTED_EXTENSION}")
}

/// A reason that may run over several lines, as TOML errors and Luau
/// tracebacks do, on one: a script reads a `fail` line alone.
pub fn one_line(reason: &str) -> String {
    reason.split_whitespace().collect::<Vec<_>>().join(" ")
}
