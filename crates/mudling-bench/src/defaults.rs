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
//! - **Cost**: per line, the host's and the defaults' reading of it, then
//!   of the pending text; the budget is on their p99s added.
//!
//! Each log is a session: the defaults start over with each one.

use crate::verdict;
use mudling_core::defaults::{self, Defaults, PendingAction, hud};
use mudling_core::line::{Line, LinePipeline};
use mudling_core::plugin::PluginHost;
use mudling_core::replay::Event;
use mudling_core::stats::Summary;
use serde::Serialize;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

const BUDGET_US: f64 = 5.0;
/// Reads in the split runs are 1 to this many bytes.
const LONGEST_SPLIT_READ: usize = 48;
const SPLIT_RUNS: u64 = 3;

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

pub fn run(source: &str, logs: &[Vec<Event>]) -> Report {
    let whole = Replay::run(logs, None);
    let mut ever_learned_split = BTreeSet::new();
    for seed in 1..=SPLIT_RUNS {
        ever_learned_split.extend(Replay::run(logs, Some(seed)).ever_learned);
    }
    let is_start_of_a_top_shape = |shape: &str| {
        whole
            .top_shapes
            .iter()
            .any(|top| shape.len() < top.len() && top.starts_with(shape))
    };
    let half_shapes = whole
        .ever_learned
        .union(&ever_learned_split)
        .filter(|shape| !whole.ever_learned.contains(*shape) || is_start_of_a_top_shape(shape))
        .count();

    let per_line = Summary::of(&whole.line_samples).expect("lines were timed");
    // A log may hold no GMCP, or no text left pending, at all.
    let or_zero = |samples: &[Duration]| {
        Summary::of(samples).unwrap_or_else(|| Summary::of(&[Duration::ZERO]).unwrap())
    };
    let per_pending = or_zero(&whole.pending_samples);
    let per_gmcp = or_zero(&whole.gmcp_samples);
    Report {
        source: source.to_owned(),
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
        per_gmcp,
        budget_us: BUDGET_US,
    }
}

pub fn print(report: &Report) {
    println!(
        "defaults over {} ({} lines, {} Char.Vitals):",
        report.source, report.lines, report.vitals
    );
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
    println!(
        "  per line: {}; per pending check: {}",
        report.per_line, report.per_pending
    );
    println!("  per GMCP message: {}", report.per_gmcp);
    println!(
        "  line + pending p99: {:.2} µs; budget ≤ {} µs: {}",
        report.per_line.p99_us + report.per_pending.p99_us,
        report.budget_us,
        verdict(report.within_budget)
    );
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
    /// Replays `logs`, each text in one read, or with `seed` cut into small
    /// random reads.
    fn run(logs: &[Vec<Event>], seed: Option<u64>) -> Replay {
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
        // Each log is a session, whether or not it opens with `[connected]`.
        let sessions = logs
            .iter()
            .flat_map(|log| std::iter::once(&Event::Connected).chain(log));
        for event in sessions {
            match event {
                Event::Connected => replay.start_session(),
                Event::Command(_) => {}
                Event::Text(text) => match &mut random {
                    None => replay.push(text.as_bytes()),
                    Some(random) => {
                        let mut rest = text.as_bytes();
                        while !rest.is_empty() {
                            let (read, after) = rest
                                .split_at((1 + random.below(LONGEST_SPLIT_READ)).min(rest.len()));
                            replay.push(read);
                            rest = after;
                        }
                    }
                },
                Event::Gmcp { package, json } => {
                    if package.eq_ignore_ascii_case(hud::VITALS) {
                        replay.vitals += 1;
                        replay.after_vitals = true;
                    }
                    let started = Instant::now();
                    replay.defaults.gmcp_arrived(package, json);
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
        self.top_shapes
            .extend(self.defaults.prompt.shapes().next().map(|(shape, _)| shape));
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
                self.matched +=
                    usize::from(self.defaults.prompt.prefix_len(line.plain()).is_some());
            }
            let started = Instant::now();
            std::hint::black_box(self.host.read_line(&line, &mut Vec::new()));
            std::hint::black_box(self.defaults.read_line(&line));
            self.line_samples.push(started.elapsed());
            self.note_learned();
        }
    }

    fn check_pending(&mut self) {
        let Some(pending) = self.pipeline.pending() else {
            return;
        };
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
            self.ever_learned
                .extend(shapes.into_iter().map(|(shape, _)| shape));
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
