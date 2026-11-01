//! `mudling-bench`: replays a session log through the core and measures it
//! against the PRD §6 budgets, headless and repeatably.
//!
//! ```text
//! mudling-bench [--check] [--budget-scale <n>] [--json <report.json>]
//!     [--pace <lines/s>] [--plugin <dir>…] [--world <prototype world file>]
//!     [<session.log>…]
//! mudling-bench compare <base-binary> [--json <report.json>]
//! ```
//!
//! With logs, it replays them; without, the committed synthetic fixture.
//! Logs hold other players' chat: they're read at runtime and never committed.
//! `--check` exits non-zero when a budget is missed. `--budget-scale` multiplies
//! every time budget, for runners that read slower than the machines §6 was
//! set on: memory stays as it is, and so does `compare`'s 10%. `compare` is in
//! [`compare`].
//!
//! Over the log's text it fills a 100,000-line scrollback, times 100,000 more
//! appends (each one parses a raw line and evicts the oldest), and finds over
//! the full buffer: every match (the budget), and the first page of matches.
//! The rest of the report is in the modules, one for each thing measured.
//! Each `--plugin` folder is timed as one more plugin in the host, and runs
//! in the session and the replay; without one, those run a synthetic set
//! and the fixture's game, with its Luau hooks and Map rules.
//!
//! Headless, it can't see a frame: the frame and map pan budgets need a
//! window, and a shared runner can't time one. The stores' share of a frame
//! is here.

mod compare;
mod completion;
mod defaults;
mod game_plugin;
mod line_pipeline;
mod log_replay;
mod luau_hooks;
mod map;
mod persist;
mod plugin_host;
mod session;
mod stores;
mod telnet;

use mudling_core::plugin::Plugin;
use mudling_core::replay::{self, Event, ReplayFeed};
use mudling_core::stats::Summary;
use mudling_core::{Line, LinePipeline, Scrollback};
use serde::Serialize;
use std::alloc::{GlobalAlloc, Layout, System};
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Entirely made up, and shaped like the prototype's logs; see `fixtures/`.
const FIXTURE: &str = include_str!("../fixtures/session.txt");
/// A shell's frame.
const FRAME: Duration = Duration::from_millis(8);
const SCROLLBACK_LINES: usize = 100_000;
const TIMED_APPENDS: usize = 100_000;
const APPEND_BUDGET_US: f64 = 200.0;
const FIND_BUDGET_US: f64 = 50_000.0;
/// Common words, a phrase, and a word that never matches (a full scan).
const FIND_NEEDLES: [&str; 4] = ["you", "north", "the guard", "xyzzy-absent"];
const FIND_ROUNDS: usize = 25;
/// What a Find panel shows first.
const FIND_PAGE: usize = 500;
const USAGE: &str = "usage: mudling-bench [--check] [--budget-scale <n>] [--json <report.json>] \
                     [--pace <lines/s>] [--plugin <dir>…] [--world <file>] [<session.log>…]\n\
                     \x20      mudling-bench compare <base-binary> [--json <report.json>]";

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

/// What `--budget-scale` multiplies every time budget by. PRD §6's numbers
/// stay the constants: a scale of 1 gates exactly as they read.
#[derive(Clone, Copy, Debug, PartialEq)]
struct BudgetScale(f64);

impl BudgetScale {
    const EXACT: Self = Self(1.0);

    fn parse(text: &str) -> Result<Self, String> {
        match text.parse::<f64>() {
            Ok(scale) if scale.is_finite() && scale > 0.0 => Ok(Self(scale)),
            _ => Err(format!("bad budget scale {text}")),
        }
    }

    /// A time budget as this run holds it to.
    fn time_us(self, budget_us: f64) -> f64 {
        budget_us * self.0
    }
}

struct Options {
    check: bool,
    budget_scale: BudgetScale,
    json: Option<String>,
    pace: Option<u32>,
    logs: Vec<String>,
    plugins: Vec<String>,
    world: Option<String>,
}

#[derive(Serialize)]
struct Report {
    source: String,
    source_lines: usize,
    scrollback: ScrollbackReport,
    append: Summary,
    find: Summary,
    find_first_page: Summary,
    find_matches: Vec<(String, usize)>,
    budget_scale: f64,
    append_budget_us: f64,
    find_budget_us: f64,
    append_within_budget: bool,
    find_within_budget: bool,
    line_pipeline: line_pipeline::Report,
    plugin_host: plugin_host::Report,
    defaults: defaults::Report,
    luau_hooks: luau_hooks::Report,
    game_plugins: Vec<game_plugin::Report>,
    stores: stores::Report,
    session: session::Report,
    replay: log_replay::Report,
    telnet: telnet::Report,
    completion: completion::Report,
    persist: persist::Report,
    map: map::Report,
}

impl Report {
    fn within_budget(&self) -> bool {
        self.append_within_budget
            && self.find_within_budget
            && self.line_pipeline.within_budget
            && self.plugin_host.within_budget
            && self.defaults.within_budget
            && self.luau_hooks.within_budget
            && self.game_plugins.iter().all(|plugin| plugin.within_budget)
            && self.stores.within_budget
            && self.session.within_budget
            && self.replay.within_budget
            && self.telnet.within_budget
            && self.completion.within_budget
            && self.persist.within_budget()
    }
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().is_some_and(|arg| arg == "compare") {
        return compare::run(args.skip(1));
    }
    let options = match parse_options(args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(&options) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => {
            eprintln!("a budget was missed");
            ExitCode::FAILURE
        }
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(2)
        }
    }
}

fn parse_options(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options {
        check: false,
        budget_scale: BudgetScale::EXACT,
        json: None,
        pace: None,
        logs: Vec::new(),
        plugins: Vec::new(),
        world: None,
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--check" => options.check = true,
            "--budget-scale" => {
                options.budget_scale =
                    BudgetScale::parse(&args.next().ok_or("--budget-scale needs a number")?)?;
            }
            "--json" => options.json = Some(args.next().ok_or("--json needs a path")?),
            "--pace" => {
                let pace = args.next().ok_or("--pace needs lines per second")?;
                let pace = pace.parse().map_err(|_| format!("bad pace {pace}"))?;
                options.pace = Some(pace).filter(|pace| *pace > 0);
            }
            "--plugin" => options
                .plugins
                .push(args.next().ok_or("--plugin needs a folder")?),
            "--world" => options.world = Some(args.next().ok_or("--world needs a file")?),
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            _ => options.logs.push(arg),
        }
    }
    Ok(options)
}

/// The logs' text, or the fixture's without any.
fn log_texts(paths: &[String]) -> Result<Vec<String>, String> {
    if paths.is_empty() {
        return Ok(vec![FIXTURE.to_owned()]);
    }
    paths
        .iter()
        .map(|path| {
            std::fs::read(path)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .map_err(|error| format!("couldn't read {path}: {error}"))
        })
        .collect()
}

fn run(options: &Options) -> Result<bool, String> {
    let texts = log_texts(&options.logs)?;
    let source = match options.logs.len() {
        0 => "the fixture".to_owned(),
        logs => format!("{logs} log(s)"),
    };
    let lines: Vec<String> = texts
        .iter()
        .flat_map(|text| replay::server_lines(text))
        .map(str::to_owned)
        .collect();
    if lines.is_empty() {
        return Err("the logs hold no server text".to_owned());
    }
    let logs: Vec<Vec<Event>> = texts.iter().map(|text| replay::events(text)).collect();
    let plugins = session_plugins(&options.plugins)?;

    let scale = options.budget_scale;
    let line_pipeline = line_pipeline::run(&lines, scale);
    let plugin_host = plugin_host::run(&lines, scale);
    let defaults = defaults::run(&source, &logs, scale);
    let luau_hooks = luau_hooks::run(scale);
    let mut game_plugins = Vec::new();
    for dir in &options.plugins {
        game_plugins.push(
            game_plugin::run(Path::new(dir), &lines, scale)
                .map_err(|error| format!("couldn't time the plugin in {dir}: {error}"))?,
        );
    }
    let stores = stores::run(scale);
    let session = session::run(&lines, &plugins, scale);
    let replay = log_replay::run(&source, &logs, &plugins, options.pace, scale)?;
    let telnet = telnet::run(scale);
    let completion = completion::run(scale);
    let persist = persist::run(scale);
    let map = map::run(&logs, options.world.as_deref().map(Path::new))?;
    let measured = Measured {
        line_pipeline,
        plugin_host,
        defaults,
        luau_hooks,
        game_plugins,
        stores,
        session,
        replay,
        telnet,
        completion,
        persist,
        map,
    };
    let report = measure_scrollback(source, ReplayFeed::new(lines), measured, scale);

    print_scrollback(&report);
    line_pipeline::print(&report.line_pipeline);
    plugin_host::print(&report.plugin_host);
    defaults::print(&report.defaults);
    luau_hooks::print(&report.luau_hooks);
    report.game_plugins.iter().for_each(game_plugin::print);
    stores::print(&report.stores);
    session::print(&report.session);
    log_replay::print(&report.replay);
    telnet::print(&report.telnet);
    completion::print(&report.completion);
    persist::print(&report.persist);
    map::print(&report.map);
    if let Some(path) = &options.json {
        write_report(path, &report)?;
    }
    Ok(!options.check || report.within_budget())
}

/// The fixture's game: hooks and Map rules, over the fixture's text.
fn fixture_plugin() -> Plugin {
    Plugin::from_files(|name| {
        Ok(match name {
            "plugin.toml" => include_str!("../fixtures/plugin/plugin.toml"),
            "text.toml" => include_str!("../fixtures/plugin/text.toml"),
            "map.toml" => include_str!("../fixtures/plugin/map.toml"),
            "fight.luau" => include_str!("../fixtures/plugin/fight.luau"),
            other => panic!("the fixture plugin has no {other}"),
        }
        .to_owned())
    })
    .expect("the fixture plugin loads")
}

/// The plugins in the folders given. Without any, a synthetic set of a real
/// plugin's size, then the fixture's game, which runs hooks and Map rules.
fn session_plugins(dirs: &[String]) -> Result<Vec<Plugin>, String> {
    if dirs.is_empty() {
        let mut plugins = plugin_host::realistic_plugins();
        plugins.push(fixture_plugin());
        return Ok(plugins);
    }
    dirs.iter()
        .map(|dir| Plugin::load(Path::new(dir)).map_err(|error| format!("{dir}: {error}")))
        .collect()
}

/// The verdict on a budget.
fn verdict(ok: bool) -> &'static str {
    if ok { "ok" } else { "MISSED" }
}

fn write_report(path: &str, report: &impl Serialize) -> Result<(), String> {
    let json = serde_json::to_string_pretty(report).expect("the report serializes");
    std::fs::write(path, json).map_err(|error| format!("couldn't write {path}: {error}"))
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e3
}

fn heap_mb(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

#[derive(Serialize)]
struct ScrollbackReport {
    lines: usize,
    fill_ms: f64,
    heap_mb: f64,
}

/// What's measured from the lines before they go into the feed.
struct Measured {
    line_pipeline: line_pipeline::Report,
    plugin_host: plugin_host::Report,
    defaults: defaults::Report,
    luau_hooks: luau_hooks::Report,
    game_plugins: Vec<game_plugin::Report>,
    stores: stores::Report,
    session: session::Report,
    replay: log_replay::Report,
    telnet: telnet::Report,
    completion: completion::Report,
    persist: persist::Report,
    map: map::Report,
}

fn measure_scrollback(
    source: String,
    mut feed: ReplayFeed,
    measured: Measured,
    scale: BudgetScale,
) -> Report {
    let Measured {
        line_pipeline,
        plugin_host,
        defaults,
        luau_hooks,
        game_plugins,
        stores,
        session,
        replay,
        telnet,
        completion,
        persist,
        map,
    } = measured;
    let mut pipeline = LinePipeline::default();
    let mut parsed: Vec<Line> = Vec::with_capacity(1);
    let heap_before = HEAP_BYTES.load(Ordering::Relaxed);
    let fill_started = Instant::now();
    let mut scrollback = Scrollback::with_capacity(SCROLLBACK_LINES);
    for _ in 0..SCROLLBACK_LINES {
        pipeline.push_line(feed.next_line().as_bytes(), &mut parsed);
        parsed.drain(..).for_each(|line| scrollback.push(line));
    }
    let fill = fill_started.elapsed();
    let scrollback_heap = HEAP_BYTES.load(Ordering::Relaxed) - heap_before;

    let mut appends = Vec::with_capacity(TIMED_APPENDS);
    for _ in 0..TIMED_APPENDS {
        let raw = feed.next_line();
        let started = Instant::now();
        pipeline.push_line(raw.as_bytes(), &mut parsed);
        parsed.drain(..).for_each(|line| scrollback.push(line));
        appends.push(started.elapsed());
    }

    let mut finds = Vec::with_capacity(FIND_NEEDLES.len() * FIND_ROUNDS);
    let mut first_pages = Vec::with_capacity(FIND_NEEDLES.len() * FIND_ROUNDS);
    let mut find_matches = Vec::new();
    for needle in FIND_NEEDLES {
        let mut matches = 0;
        for _ in 0..FIND_ROUNDS {
            let started = Instant::now();
            matches = std::hint::black_box(scrollback.find(needle, usize::MAX)).len();
            finds.push(started.elapsed());
            let started = Instant::now();
            std::hint::black_box(scrollback.find(needle, FIND_PAGE));
            first_pages.push(started.elapsed());
        }
        find_matches.push((needle.to_owned(), matches));
    }

    let append = Summary::of(&appends).expect("appends were timed");
    let find = Summary::of(&finds).expect("finds were timed");
    let find_first_page = Summary::of(&first_pages).expect("finds were timed");
    let append_budget_us = scale.time_us(APPEND_BUDGET_US);
    let find_budget_us = scale.time_us(FIND_BUDGET_US);
    Report {
        source,
        source_lines: feed.source_len(),
        scrollback: ScrollbackReport {
            lines: scrollback.len(),
            fill_ms: millis(fill),
            heap_mb: heap_mb(scrollback_heap),
        },
        append_within_budget: append.p99_us <= append_budget_us,
        find_within_budget: find.p99_us <= find_budget_us,
        append,
        find,
        find_first_page,
        find_matches,
        budget_scale: scale.0,
        append_budget_us,
        find_budget_us,
        line_pipeline,
        plugin_host,
        defaults,
        luau_hooks,
        game_plugins,
        stores,
        session,
        replay,
        telnet,
        completion,
        persist,
        map,
    }
}

fn print_scrollback(report: &Report) {
    let scrollback = &report.scrollback;
    println!(
        "source: {} ({} lines); scrollback: {} lines",
        report.source, report.source_lines, scrollback.lines
    );
    println!(
        "fill: {:.1} ms; scrollback heap: {:.1} MB",
        scrollback.fill_ms, scrollback.heap_mb
    );
    println!(
        "budget scale: ×{} on every time budget below (memory is not scaled)",
        report.budget_scale
    );
    println!(
        "append (parse + push + evict): {}; budget p99 ≤ {} µs: {}",
        report.append,
        report.append_budget_us,
        verdict(report.append_within_budget)
    );
    println!(
        "find over {} lines: p50 {:.2} ms, p99 {:.2} ms, max {:.2} ms; budget p99 ≤ {} ms: {}",
        scrollback.lines,
        report.find.p50_us / 1e3,
        report.find.p99_us / 1e3,
        report.find.max_us / 1e3,
        report.find_budget_us / 1e3,
        verdict(report.find_within_budget)
    );
    println!(
        "find, first {FIND_PAGE} matches: p50 {:.2} ms, p99 {:.2} ms",
        report.find_first_page.p50_us / 1e3,
        report.find_first_page.p99_us / 1e3
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLUGIN_BUDGET_US: f64 = 5.0;
    const FOUR: BudgetScale = BudgetScale(4.0);

    #[test]
    fn a_miss_at_exact_budgets_passes_at_four_times() {
        let p99_us = 12.0;
        assert!(p99_us > BudgetScale::EXACT.time_us(PLUGIN_BUDGET_US));
        assert!(p99_us <= FOUR.time_us(PLUGIN_BUDGET_US));
    }

    #[test]
    fn a_miss_at_four_times_still_fails() {
        assert!(25.0 > FOUR.time_us(PLUGIN_BUDGET_US));
    }

    #[test]
    fn the_scale_is_a_positive_number() {
        assert_eq!(BudgetScale::parse("4"), Ok(FOUR));
        for bad in ["0", "-1", "inf", "NaN", "four", ""] {
            assert!(BudgetScale::parse(bad).is_err(), "{bad:?} parsed");
        }
    }

    #[test]
    fn budgets_are_exact_unless_scaled() {
        let options = parse_options(["--check".to_owned()].into_iter()).unwrap();
        assert_eq!(options.budget_scale, BudgetScale::EXACT);
        let options = parse_options(["--budget-scale", "4"].map(str::to_owned).into_iter());
        assert_eq!(options.unwrap().budget_scale, FOUR);
    }
}
