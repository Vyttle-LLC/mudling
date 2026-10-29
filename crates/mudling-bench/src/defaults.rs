//! The default plugins over a replay: the cost of all of them per line,
//! and a check of prompt learning.
//!
//! The logs keep the prototype's GMCP notes, so the replay has what the
//! defaults learn from: each `Char.Vitals` and the text after it. Server
//! text goes through the line pipeline and the plugin host (the built-in
//! default plugin alone) to the defaults, as the shell will drive them.
//!
//! - **Matched**: how often the text after a `Char.Vitals` opened with a
//!   prompt already learned.
//! - **Halves**: the replay runs again with the server text cut into small
//!   random reads, so prompts are split across them. A shape learned only
//!   then, or one that's the start of a session's most-seen shape, is half
//!   a prompt. There must be none.
//!
//! Each log is a session: the defaults start over with each one.
//! - **Cost**: per line, the host's and the defaults' reading of it, then
//!   of the pending text; the budget is on their p99s added.

use mudling_core::defaults::{self, Defaults, PendingAction};
use mudling_core::line::{Line, LinePipeline};
use mudling_core::plugin::PluginHost;
use mudling_core::replay;
use mudling_core::stats::Summary;
use serde::Serialize;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

pub const BUDGET_US: f64 = 5.0;
/// The prototype's notes, as `replay` lists them.
const NOTES: [&str; 6] = ["[GMCP] ", "[telnet] ", "[auto-login] ", "[disconnected] ", "[MAP] ", "[connected]"];
/// Reads in the split runs are 1 to this many bytes.
const LONGEST_SPLIT_READ: usize = 48;
const SPLIT_RUNS: u64 = 3;
const SYNTHETIC_LINES: usize = 100_000;

#[derive(Serialize)]
pub struct Report {
    pub source: String,
    pub lines: usize,
    pub vitals: usize,
    /// Texts after a `Char.Vitals` that opened with a learned prompt.
    pub prompts_matched: usize,
    pub match_rate: f64,
    /// Prompts the defaults ended in text the server hadn't.
    pub prompts_ended: usize,
    pub shapes_learned: usize,
    pub split_runs: u64,
    /// Shapes learned that are half a prompt: must be 0.
    pub half_shapes: usize,
    pub per_line: Summary,
    pub per_pending: Summary,
    pub per_gmcp: Summary,
    pub budget_us: f64,
    pub within_budget: bool,
}

pub fn run(logs: &[String]) -> Report {
    let (source, events) = if logs.is_empty() {
        ("synthetic".to_owned(), synthetic_events())
    } else {
        let mut events = Vec::new();
        for path in logs {
            let bytes = std::fs::read(path).unwrap_or_else(|error| panic!("couldn't read {path}: {error}"));
            events.extend(log_events(&String::from_utf8_lossy(&bytes)));
        }
        (format!("{} log(s)", logs.len()), events)
    };

    let whole = Replay::run(&events, None);
    let mut ever_learned_split = BTreeSet::new();
    for seed in 1..=SPLIT_RUNS {
        ever_learned_split.extend(Replay::run(&events, Some(seed)).ever_learned);
    }
    let is_start_of_a_top_shape =
        |shape: &str| whole.top_shapes.iter().any(|top| shape.len() < top.len() && top.starts_with(shape));
    let half_shapes = whole
        .ever_learned
        .union(&ever_learned_split)
        .filter(|shape| !whole.ever_learned.contains(*shape) || is_start_of_a_top_shape(shape))
        .count();

    let summary = |samples: &[Duration]| Summary::of(samples).unwrap_or(Summary::of(&[Duration::ZERO]).unwrap());
    let per_line = summary(&whole.line_samples);
    let per_pending = summary(&whole.pending_samples);
    Report {
        source,
        lines: whole.line_samples.len(),
        vitals: whole.vitals,
        prompts_matched: whole.matched,
        match_rate: whole.matched as f64 / whole.vitals.max(1) as f64,
        prompts_ended: whole.prompts_ended,
        shapes_learned: whole.ever_learned.len(),
        split_runs: SPLIT_RUNS,
        half_shapes,
        within_budget: per_line.p99_us + per_pending.p99_us <= BUDGET_US && half_shapes == 0,
        per_line,
        per_pending,
        per_gmcp: summary(&whole.gmcp_samples),
        budget_us: BUDGET_US,
    }
}

pub fn print(report: &Report) {
    let summary = |s: &Summary| format!("p50 {:.2} µs, p99 {:.2} µs, max {:.1} µs", s.p50_us, s.p99_us, s.max_us);
    println!("defaults over {} ({} lines, {} Char.Vitals):", report.source, report.lines, report.vitals);
    println!(
        "  prompt after Char.Vitals matched a learned shape: {} of {} ({:.1}%); {} prompts ended by the defaults from pending text; \
         {} shapes learned; half-prompt shapes over {} split replays: {}",
        report.prompts_matched,
        report.vitals,
        report.match_rate * 100.0,
        report.prompts_ended,
        report.shapes_learned,
        report.split_runs,
        report.half_shapes
    );
    println!("  per line: {}; per pending check: {}", summary(&report.per_line), summary(&report.per_pending));
    println!("  per GMCP message: {}", summary(&report.per_gmcp));
    println!(
        "  line + pending p99: {:.2} µs; budget ≤ {} µs: {}",
        report.per_line.p99_us + report.per_pending.p99_us,
        report.budget_us,
        if report.within_budget { "ok" } else { "MISSED" }
    );
}

#[derive(Debug, PartialEq)]
enum Event {
    /// A session starts.
    Connected,
    Text(String),
    Gmcp { package: String, json: String },
    /// GA or EOR.
    PromptEnd,
}

/// A prototype log as one session's server text and GMCP messages, in
/// order. Each note is written `"\n[TAG] …\n"`; the other notes are dropped.
fn log_events(log: &str) -> Vec<Event> {
    let mut events = vec![Event::Connected];
    let push_text = |events: &mut Vec<Event>, text: &str| {
        let text = text.strip_prefix("[connected]\n").unwrap_or(text);
        if !text.is_empty() {
            events.push(Event::Text(text.to_owned()));
        }
    };
    let (mut text_start, mut at) = (0, 0);
    while let Some(found) = log[at..].find("\n[").map(|offset| at + offset) {
        let note_start = found + 1;
        if !NOTES.iter().any(|note| log[note_start..].starts_with(note)) {
            at = note_start;
            continue;
        }
        push_text(&mut events, &log[text_start..found]);
        let note_end = log[note_start..].find('\n').map_or(log.len(), |offset| note_start + offset);
        if let Some(gmcp) = log[note_start..note_end].strip_prefix("[GMCP] ") {
            let (package, json) = gmcp.split_once(' ').unwrap_or((gmcp, ""));
            events.push(Event::Gmcp { package: package.to_owned(), json: json.to_owned() });
        }
        text_start = (note_end + 1).min(log.len());
        at = text_start;
    }
    push_text(&mut events, &log[text_start..]);
    events
}

/// Made-up server text with a `Char.Vitals` and a prompt every fourth
/// line, ended by GA as Diku-family servers end them, for runs without logs.
fn synthetic_events() -> Vec<Event> {
    let mut events = vec![Event::Connected];
    for (index, line) in replay::synthetic_lines(SYNTHETIC_LINES).into_iter().enumerate() {
        events.push(Event::Text(format!("{line}\n\r")));
        if index % 4 == 3 {
            let (hp, mv) = (index % 500, index % 97);
            events.push(Event::Gmcp { package: "Char.Vitals".into(), json: format!(r#"{{"hp": "{hp}", "mv": "{mv}"}}"#) });
            events.push(Event::Text(format!("\n\r<{hp}hp {mv}mv> ")));
            events.push(Event::PromptEnd);
        }
    }
    events
}

struct Replay {
    pipeline: LinePipeline,
    host: PluginHost,
    defaults: Defaults,
    lines: Vec<Line>,
    /// A `Char.Vitals` came, and no text after it yet.
    after_vitals: bool,
    vitals: usize,
    matched: usize,
    prompts_ended: usize,
    /// Every shape any session learned.
    ever_learned: BTreeSet<String>,
    /// Each session's most-seen shape at its end.
    top_shapes: BTreeSet<String>,
    seen_total: u32,
    line_samples: Vec<Duration>,
    pending_samples: Vec<Duration>,
    gmcp_samples: Vec<Duration>,
}

impl Replay {
    /// Replays `events`, each text in one read, or with `seed` cut into
    /// small random reads.
    fn run(events: &[Event], seed: Option<u64>) -> Replay {
        let mut replay = Replay {
            pipeline: LinePipeline::default(),
            host: PluginHost::new([defaults::plugin()]).expect("the defaults compile"),
            defaults: Defaults::default(),
            lines: Vec::new(),
            after_vitals: false,
            vitals: 0,
            matched: 0,
            prompts_ended: 0,
            ever_learned: BTreeSet::new(),
            top_shapes: BTreeSet::new(),
            seen_total: 0,
            line_samples: Vec::new(),
            pending_samples: Vec::new(),
            gmcp_samples: Vec::new(),
        };
        let mut random = seed.map(XorShift::new);
        for event in events {
            match event {
                Event::Connected => replay.start_session(),
                Event::PromptEnd => {
                    if let Some(line) = replay.pipeline.prompt_end() {
                        replay.lines.push(line);
                        replay.read_lines();
                    }
                }
                Event::Text(text) => match &mut random {
                    None => replay.push(text.as_bytes()),
                    Some(random) => {
                        let mut rest = text.as_bytes();
                        while !rest.is_empty() {
                            let (read, after) = rest.split_at((1 + random.below(LONGEST_SPLIT_READ)).min(rest.len()));
                            replay.push(read);
                            rest = after;
                        }
                    }
                },
                Event::Gmcp { package, json } => {
                    if package == "Char.Vitals" {
                        replay.vitals += 1;
                        replay.after_vitals = true;
                    }
                    let started = Instant::now();
                    replay.defaults.gmcp(package, json);
                    replay.gmcp_samples.push(started.elapsed());
                    replay.note_learned();
                    replay.check_pending();
                }
            }
        }
        replay.start_session();
        replay
    }

    /// Ends the session so far, keeping its most-seen shape, and starts
    /// another from nothing learned.
    fn start_session(&mut self) {
        self.top_shapes.extend(self.defaults.prompt.shapes().next().map(|(shape, _)| shape));
        self.pipeline = LinePipeline::default();
        self.host = PluginHost::new([defaults::plugin()]).expect("the defaults compile");
        self.defaults = Defaults::default();
        self.after_vitals = false;
        self.seen_total = 0;
    }

    fn push(&mut self, bytes: &[u8]) {
        self.pipeline.push(bytes, &mut self.lines);
        self.read_lines();
        self.check_pending();
    }

    fn read_lines(&mut self) {
        for line in std::mem::take(&mut self.lines) {
            if self.after_vitals && !line.plain().trim().is_empty() {
                self.after_vitals = false;
                self.matched += usize::from(self.defaults.prompt.prefix_len(line.plain()).is_some());
            }
            let started = Instant::now();
            std::hint::black_box(self.host.read_line(&line, &mut Vec::new()));
            std::hint::black_box(self.defaults.read_line(&line));
            self.line_samples.push(started.elapsed());
            self.note_learned();
        }
    }

    fn check_pending(&mut self) {
        let Some(pending) = self.pipeline.pending() else { return };
        let started = Instant::now();
        let kind = self.host.read_pending(&pending);
        let action = self.defaults.read_pending(&pending, kind);
        self.pending_samples.push(started.elapsed());
        if action == PendingAction::EndPrompt {
            self.prompts_ended += 1;
            let line = self.pipeline.prompt_end().expect("text is pending");
            self.lines.push(line);
            self.read_lines();
        }
    }

    /// Notes each shape when it's learned, before the store can drop it.
    fn note_learned(&mut self) {
        let shapes: Vec<(String, u32)> = self.defaults.prompt.shapes().collect();
        let seen_total = shapes.iter().map(|(_, seen)| seen).sum();
        if seen_total != self.seen_total {
            self.seen_total = seen_total;
            self.ever_learned.extend(shapes.into_iter().map(|(shape, _)| shape));
        }
    }
}

/// xorshift64: the same reads every run.
struct XorShift(u64);

impl XorShift {
    fn new(seed: u64) -> XorShift {
        XorShift(0x9E37_79B9_7F4A_7C15 ^ seed.wrapping_mul(0xBF58_476D_1CE4_E5B9))
    }

    fn below(&mut self, bound: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % bound as u64) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_server_text_and_gmcp_from_a_log() {
        // Synthetic: shaped like SessionLogger's output.
        let log = "[connected]\nWelcome.\r\n\n[telnet] verb 251 option 201\n[B]ack here\n\
                   \n[GMCP] Char.Vitals { \"hp\": \"7\" }\n\n\r<7hp> \n[GMCP] Room.Info\nYou rest.";
        assert_eq!(
            log_events(log),
            [
                Event::Connected,
                Event::Text("Welcome.\r\n".into()),
                Event::Text("[B]ack here\n".into()),
                Event::Gmcp { package: "Char.Vitals".into(), json: "{ \"hp\": \"7\" }".into() },
                Event::Text("\n\r<7hp> ".into()),
                Event::Gmcp { package: "Room.Info".into(), json: String::new() },
                Event::Text("You rest.".into()),
            ]
        );
    }
}
