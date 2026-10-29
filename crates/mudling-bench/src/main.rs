//! `mudling-bench`: measures the scrollback against the PRD §6 budgets,
//! headless and repeatably.
//!
//! ```text
//! mudling-bench [--check] [--json <report.json>] [<session.log>…]
//! ```
//!
//! With logs, it replays their server text; without, it uses synthetic lines.
//! It fills a 100,000-line scrollback, then times 100,000 more appends (each
//! one parses a raw line and evicts the oldest), and finds over the full
//! buffer. `--check` exits non-zero when a budget is missed.
//! It also times the line pipeline per line over the same text.

mod line_pipeline;

use mudling_core::replay::{self, ReplayFeed};
use mudling_core::stats::Summary;
use mudling_core::{AnsiParser, Scrollback};
use serde::Serialize;
use std::alloc::{GlobalAlloc, Layout, System};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const SCROLLBACK_LINES: usize = 100_000;
const TIMED_APPENDS: usize = 100_000;
const APPEND_BUDGET_US: f64 = 200.0;
const FIND_BUDGET_US: f64 = 50_000.0;
/// Common words, a phrase, and a word that never matches (a full scan).
const FIND_NEEDLES: [&str; 4] = ["you", "north", "the guard", "xyzzy-absent"];
const FIND_ROUNDS: usize = 10;

/// Counts live heap bytes, so the report can say what 100,000 lines cost.
struct CountingAllocator;

static HEAP_BYTES: AtomicUsize = AtomicUsize::new(0);

// GlobalAlloc is an unsafe trait. This forwards every call to the system
// allocator unchanged and only keeps a running byte count.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        HEAP_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        HEAP_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        HEAP_BYTES.fetch_add(new_size, Ordering::Relaxed);
        HEAP_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

struct Options {
    check: bool,
    json: Option<String>,
    logs: Vec<String>,
}

#[derive(Serialize)]
struct Report {
    source: String,
    source_lines: usize,
    scrollback_lines: usize,
    fill_ms: f64,
    scrollback_heap_mb: f64,
    append: Summary,
    find: Summary,
    find_matches: Vec<(String, usize)>,
    append_budget_us: f64,
    find_budget_us: f64,
    append_within_budget: bool,
    find_within_budget: bool,
    line_pipeline: line_pipeline::Report,
}

fn main() -> ExitCode {
    let options = match parse_options() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}\nusage: mudling-bench [--check] [--json <report.json>] [<session.log>…]");
            return ExitCode::from(2);
        }
    };
    let (source, lines) = if options.logs.is_empty() {
        ("synthetic".to_owned(), replay::synthetic_lines(SCROLLBACK_LINES + TIMED_APPENDS))
    } else {
        match replay::load_logs(&options.logs) {
            Ok(lines) if !lines.is_empty() => (format!("{} log(s)", options.logs.len()), lines),
            Ok(_) => {
                eprintln!("the logs hold no server text");
                return ExitCode::from(2);
            }
            Err(error) => {
                eprintln!("couldn't read the logs: {error}");
                return ExitCode::from(2);
            }
        }
    };

    let line_pipeline = line_pipeline::run(&lines);
    let report = run(source, ReplayFeed::new(lines), line_pipeline);
    print_report(&report);
    line_pipeline::print(&report.line_pipeline);
    if let Some(path) = &options.json {
        let json = serde_json::to_string_pretty(&report).expect("the report serializes");
        if let Err(error) = std::fs::write(path, json) {
            eprintln!("couldn't write {path}: {error}");
            return ExitCode::from(2);
        }
    }
    if options.check
        && !(report.append_within_budget && report.find_within_budget && report.line_pipeline.within_budget)
    {
        eprintln!("a budget was missed");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn parse_options() -> Result<Options, String> {
    let mut options = Options { check: false, json: None, logs: Vec::new() };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--check" => options.check = true,
            "--json" => options.json = Some(args.next().ok_or("--json needs a path")?),
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            _ => options.logs.push(arg),
        }
    }
    Ok(options)
}

fn run(source: String, mut feed: ReplayFeed, line_pipeline: line_pipeline::Report) -> Report {
    let mut parser = AnsiParser::default();
    let heap_before = HEAP_BYTES.load(Ordering::Relaxed);
    let fill_started = Instant::now();
    let mut scrollback = Scrollback::with_capacity(SCROLLBACK_LINES);
    for _ in 0..SCROLLBACK_LINES {
        scrollback.push(parser.parse_line(feed.next_line()));
    }
    let fill = fill_started.elapsed();
    let scrollback_heap = HEAP_BYTES.load(Ordering::Relaxed) - heap_before;

    let mut appends = Vec::with_capacity(TIMED_APPENDS);
    for _ in 0..TIMED_APPENDS {
        let raw = feed.next_line();
        let started = Instant::now();
        scrollback.push(parser.parse_line(raw));
        appends.push(started.elapsed());
    }

    let mut finds = Vec::with_capacity(FIND_NEEDLES.len() * FIND_ROUNDS);
    let mut find_matches = Vec::new();
    for needle in FIND_NEEDLES {
        let mut matches = 0;
        for _ in 0..FIND_ROUNDS {
            let started = Instant::now();
            matches = std::hint::black_box(scrollback.find(needle)).len();
            finds.push(started.elapsed());
        }
        find_matches.push((needle.to_owned(), matches));
    }

    let append = Summary::of(&appends).expect("appends were timed");
    let find = Summary::of(&finds).expect("finds were timed");
    Report {
        source,
        source_lines: feed.source_len(),
        scrollback_lines: scrollback.len(),
        fill_ms: millis(fill),
        scrollback_heap_mb: scrollback_heap as f64 / (1024.0 * 1024.0),
        append_within_budget: append.p99_us <= APPEND_BUDGET_US,
        find_within_budget: find.p99_us <= FIND_BUDGET_US,
        append,
        find,
        find_matches,
        append_budget_us: APPEND_BUDGET_US,
        find_budget_us: FIND_BUDGET_US,
        line_pipeline,
    }
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e3
}

fn print_report(report: &Report) {
    let verdict = |ok: bool| if ok { "ok" } else { "MISSED" };
    println!(
        "source: {} ({} lines); scrollback: {} lines",
        report.source, report.source_lines, report.scrollback_lines
    );
    println!("fill: {:.1} ms; scrollback heap: {:.1} MB", report.fill_ms, report.scrollback_heap_mb);
    println!(
        "append (parse + push + evict): p50 {:.2} µs, p99 {:.2} µs, max {:.1} µs; budget p99 ≤ {} µs: {}",
        report.append.p50_us,
        report.append.p99_us,
        report.append.max_us,
        report.append_budget_us,
        verdict(report.append_within_budget)
    );
    println!(
        "find over {} lines: p50 {:.2} ms, p99 {:.2} ms, max {:.2} ms; budget p99 ≤ {} ms: {}",
        report.scrollback_lines,
        report.find.p50_us / 1e3,
        report.find.p99_us / 1e3,
        report.find.max_us / 1e3,
        report.find_budget_us / 1e3,
        verdict(report.find_within_budget)
    );
}
