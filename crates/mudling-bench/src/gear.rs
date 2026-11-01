//! The gear database at its fullest (PRD §5 #4, where the prototype spent
//! 4.9 ms a line rebuilding its tooltips): a line against one matcher over
//! every known name, and what a rebuild costs when the names change.
//!
//! Every session the bench opens starts from [`save_gear`], so its budgets
//! hold with this database behind them.

use crate::session::HOST;
use crate::verdict;
use mudling_core::gear::{GearBook, ItemMatcher, ItemRef, Reading};
use mudling_core::persist::{Batch, Database, SERVERS_FOLDER, StatMap, Timestamp};
use mudling_core::stats::Summary;
use serde::Serialize;
use std::path::Path;
use std::time::Instant;

/// More items than a long-played server's database holds.
const ITEMS: usize = 5_000;
const READINGS_PER_ITEM: usize = 2;
/// What [`save_gear`] puts in the gear store: each item's slot and keywords.
pub const SAVED_STORE_KEYS: usize = ITEMS * 2;
const REBUILDS: usize = 40;
const LINE_BUDGET_US: f64 = 5.0;
/// A rebuild has a thread of its own, so this is how long a tooltip lags a
/// new item, not a stall.
const REBUILD_BUDGET_US: f64 = 50_000.0;
const SLOTS: [&str; 4] = ["neck", "finger", "head", "feet"];

fn item_name(number: usize) -> String {
    format!("a charm of the {number}th order")
}

/// The book the bench's server holds, with its rows on `batch`.
fn full_book(batch: &mut Batch) -> GearBook {
    let now = Timestamp(0);
    let mut book = GearBook::default();
    for number in 0..ITEMS {
        let name = item_name(number);
        for copy in 0..READINGS_PER_ITEM {
            let id = format!("{number}0{copy}");
            let item = ItemRef {
                name: &name,
                copy: Some(&id),
            };
            let reading = Reading {
                keywords: format!("charm order {number}"),
                item_type: "armor".to_owned(),
                level: Some((number % 50) as i64),
                base: StatMap::from([("ac".to_owned(), -(copy as i64))]),
                ..Reading::default()
            };
            book.learn(reading, item, now, batch);
        }
        book.learn_slot(SLOTS[number % SLOTS.len()], &name, now, batch);
    }
    book
}

/// Saves a full gear database for the bench's server in `folder`.
pub fn save_gear(folder: &Path) {
    let database = Database::open(&folder.join(SERVERS_FOLDER), HOST).expect("the database opens");
    let writer = database.writer().expect("the writer starts");
    let mut batch = Batch::new();
    full_book(&mut batch);
    writer.submit(batch).expect("the writer runs");
    writer.flush().expect("the gear reaches the disk");
}

#[derive(Serialize)]
pub struct Report {
    items: usize,
    line: Summary,
    rebuild: Summary,
    line_budget_us: f64,
    rebuild_budget_us: f64,
    pub within_budget: bool,
}

pub fn run(lines: &[String]) -> Report {
    let book = full_book(&mut Batch::new());
    let mut rebuilds = Vec::with_capacity(REBUILDS);
    let mut matcher = ItemMatcher::default();
    for _ in 0..REBUILDS {
        let started = Instant::now();
        matcher = ItemMatcher::new(book.names());
        rebuilds.push(started.elapsed());
    }
    // Some lines name an item, as a chat of gear does.
    let named = |(index, line): (usize, &String)| match index % 8 {
        0 => format!("{line} You wear {}.", item_name(index % ITEMS)),
        _ => line.clone(),
    };
    let lines: Vec<String> = lines.iter().enumerate().map(named).collect();
    let samples: Vec<_> = lines
        .iter()
        .map(|line| {
            let started = Instant::now();
            std::hint::black_box(matcher.find(line));
            started.elapsed()
        })
        .collect();
    let line = Summary::of(&samples).expect("lines were timed");
    let rebuild = Summary::of(&rebuilds).expect("rebuilds were timed");
    Report {
        items: ITEMS,
        within_budget: line.p99_us <= LINE_BUDGET_US && rebuild.p99_us <= REBUILD_BUDGET_US,
        line,
        rebuild,
        line_budget_us: LINE_BUDGET_US,
        rebuild_budget_us: REBUILD_BUDGET_US,
    }
}

pub fn print(report: &Report) {
    println!(
        "item names in a line, {} known: {}; budget p99 ≤ {} µs",
        report.items, report.line, report.line_budget_us
    );
    println!(
        "  a rebuild, only when the names change: {}; budget p99 ≤ {} µs: {}",
        report.rebuild,
        report.rebuild_budget_us,
        verdict(report.within_budget)
    );
}
