//! `completion-bench`: times a Tab completion against 20,000 known words
//! alongside a full history of 500 commands, and checks it against the
//! budget of 1 ms at p99.
//!
//! ```text
//! completion-bench [--check] [--json <report.json>]
//! ```
//!
//! The words are synthetic. Prefixes run from one to three letters, so the
//! first Tab sees both many matches and few.

use mudling_core::completion::{Completer, History, KnownWords};
use mudling_core::stats::Summary;
use serde::Serialize;
use std::process::ExitCode;
use std::time::Instant;

const KNOWN_WORDS: usize = 20_000;
const TIMED_COMPLETIONS: usize = 20_000;
const BUDGET_US: f64 = 1_000.0;
const SOURCES: [&str; 3] = ["room", "mob", "item"];

#[derive(Serialize)]
struct Report {
    known_words: usize,
    history_commands: usize,
    complete: Summary,
    budget_us: f64,
    within_budget: bool,
}

fn main() -> ExitCode {
    let mut check = false;
    let mut json = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--check" => check = true,
            "--json" => match args.next() {
                Some(path) => json = Some(path),
                None => return usage("--json needs a path"),
            },
            other => return usage(&format!("unknown argument {other}")),
        }
    }

    let report = run();
    println!(
        "complete against {} known words and {} commands: p50 {:.2} µs, p99 {:.2} µs, max {:.1} µs; budget p99 ≤ {} µs: {}",
        report.known_words,
        report.history_commands,
        report.complete.p50_us,
        report.complete.p99_us,
        report.complete.max_us,
        report.budget_us,
        if report.within_budget { "ok" } else { "MISSED" }
    );
    if let Some(path) = json {
        let text = serde_json::to_string_pretty(&report).expect("the report serializes");
        if let Err(error) = std::fs::write(&path, text) {
            eprintln!("couldn't write {path}: {error}");
            return ExitCode::from(2);
        }
    }
    if check && !report.within_budget {
        eprintln!("the budget was missed");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn usage(problem: &str) -> ExitCode {
    eprintln!("{problem}\nusage: completion-bench [--check] [--json <report.json>]");
    ExitCode::from(2)
}

/// A small deterministic generator, so every run times the same words.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn word(&mut self) -> String {
        let length = 4 + self.next() % 7;
        (0..length)
            .map(|_| char::from(b'a' + (self.next() % 26) as u8))
            .collect()
    }
}

fn run() -> Report {
    let mut random = Lcg(1);
    let mut known = KnownWords::new();
    let mut added = 0;
    while known.len() < KNOWN_WORDS {
        known.add_words(SOURCES[added % SOURCES.len()], [random.word()]);
        added += 1;
    }
    let mut history = History::new();
    for _ in 0..mudling_core::completion::HISTORY_LIMIT {
        let command = format!("{} {} {}", random.word(), random.word(), random.word());
        history.record(&command, false);
    }

    let mut completer = Completer::new();
    let mut samples = Vec::with_capacity(TIMED_COMPLETIONS);
    for _ in 0..TIMED_COMPLETIONS {
        let length = 1 + random.next() as usize % 3;
        let prefix: String = (0..length)
            .map(|_| char::from(b'a' + (random.next() % 26) as u8))
            .collect();
        let line = format!("kill {prefix}");
        let started = Instant::now();
        let completion = completer.complete(&line, line.len(), &history, &known);
        samples.push(started.elapsed());
        std::hint::black_box(completion);
    }

    let complete = Summary::of(&samples).expect("completions were timed");
    Report {
        known_words: known.len(),
        history_commands: history.entries().len(),
        within_budget: complete.p99_us <= BUDGET_US,
        complete,
        budget_us: BUDGET_US,
    }
}
