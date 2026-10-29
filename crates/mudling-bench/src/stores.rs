//! Stores against the frame budget (PRD §6: a frame ≤ 8 ms at 50 lines/s).
//!
//! Two measurements:
//! - **Frame apply:** a shell drains one frame's batch and applies it to its
//!   copy of the stores, with a full 100,000-line terminal.
//! - **Burst:** a core thread writes 50 lines/s, in bursts, one write per
//!   line, while a shell ticks every 8 ms and drains once per tick. The shell
//!   must get at most one batch per frame, and be woken once per batch, not
//!   once per write.

use mudling_core::replay;
use mudling_core::stats::Summary;
use mudling_core::store::{
    Fields, RoomState, StoreHub, Stores, TERMINAL_CAPACITY, TerminalChange, TerminalLine,
    TerminalState, VitalsState,
};
use serde::Serialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const FRAME: Duration = Duration::from_millis(8);
const FRAME_BUDGET_US: f64 = FRAME.as_micros() as f64;
const APPLY_ROUNDS: usize = 10_000;
/// A typical frame while text arrives: a few lines, the vitals and the room.
const LINES_PER_FRAME: usize = 10;
const BURST_LINES_PER_SECOND: usize = 50;
const BURST_LINES: usize = 10;
const BURST_SECONDS: u64 = 2;

#[derive(Serialize)]
pub struct Report {
    pub frame_apply: Summary,
    pub burst_line_writes: usize,
    pub burst_frames: usize,
    pub burst_batches: usize,
    pub burst_wakes: usize,
    pub burst_frame: Summary,
    pub within_budget: bool,
}

pub fn run() -> Report {
    let frame_apply = measure_frame_apply();
    let burst = measure_burst();
    Report {
        within_budget: frame_apply.p99_us <= FRAME_BUDGET_US
            && burst.frame.p99_us <= FRAME_BUDGET_US
            && burst.batches <= burst.frames
            && burst.wakes <= burst.batches,
        frame_apply,
        burst_line_writes: burst.line_writes,
        burst_frames: burst.frames,
        burst_batches: burst.batches,
        burst_wakes: burst.wakes,
        burst_frame: burst.frame,
    }
}

pub fn print(report: &Report) {
    let verdict = if report.within_budget { "ok" } else { "MISSED" };
    let apply = &report.frame_apply;
    println!(
        "store frame apply ({LINES_PER_FRAME} lines + vitals + room, {TERMINAL_CAPACITY}-line terminal): \
         p50 {:.2} µs, p99 {:.2} µs, max {:.1} µs",
        apply.p50_us, apply.p99_us, apply.max_us
    );
    let frame = &report.burst_frame;
    println!(
        "store burst ({BURST_LINES_PER_SECOND} lines/s): {} line writes in {} batches over {} frames, \
         {} wakes; frame drain + apply p50 {:.2} µs, p99 {:.2} µs, max {:.1} µs; \
         budget p99 ≤ {} ms, ≤ 1 batch per frame, ≤ 1 wake per batch: {verdict}",
        report.burst_line_writes,
        report.burst_batches,
        report.burst_frames,
        report.burst_wakes,
        frame.p50_us,
        frame.p99_us,
        frame.max_us,
        FRAME_BUDGET_US / 1e3,
    );
}

fn measure_frame_apply() -> Summary {
    let hub = StoreHub::new(|| {});
    let mut shell = subscribed_shell(&hub);
    hub.write::<TerminalState>(append(
        replay::synthetic_lines(TERMINAL_CAPACITY).into_iter(),
    ));
    shell.apply_batch(&hub.drain().expect("the terminal filled"));

    let mut lines = replay::synthetic_lines(1_000).into_iter().cycle();
    let mut samples = Vec::with_capacity(APPLY_ROUNDS);
    for round in 0..APPLY_ROUNDS {
        hub.write::<TerminalState>(append(lines.by_ref().take(LINES_PER_FRAME)));
        hub.write::<VitalsState>(vitals(round));
        hub.write::<RoomState>(room(round));
        let started = Instant::now();
        if let Some(batch) = hub.drain() {
            shell.apply_batch(&batch);
        }
        samples.push(started.elapsed());
    }
    Summary::of(&samples).expect("frames were timed")
}

struct Burst {
    line_writes: usize,
    frames: usize,
    batches: usize,
    wakes: usize,
    frame: Summary,
}

fn measure_burst() -> Burst {
    let wakes = Arc::new(AtomicUsize::new(0));
    let hub = Arc::new(StoreHub::new({
        let wakes = Arc::clone(&wakes);
        move || {
            wakes.fetch_add(1, Ordering::Relaxed);
        }
    }));
    let mut shell = subscribed_shell(&hub);

    let bursts = BURST_SECONDS as usize * BURST_LINES_PER_SECOND / BURST_LINES;
    let burst_gap = Duration::from_secs(BURST_SECONDS) / bursts as u32;
    let writer = std::thread::spawn({
        let hub = Arc::clone(&hub);
        move || {
            let mut lines = replay::synthetic_lines(BURST_LINES).into_iter().cycle();
            let started = Instant::now();
            for burst in 0..bursts {
                std::thread::sleep(
                    (started + burst_gap * burst as u32).saturating_duration_since(Instant::now()),
                );
                for line in lines.by_ref().take(BURST_LINES) {
                    hub.write::<TerminalState>(append(std::iter::once(line)));
                }
                hub.write::<VitalsState>(vitals(burst));
            }
            bursts * BURST_LINES
        }
    });

    let mut frames = 0;
    let mut batches = 0;
    let mut samples = Vec::new();
    let started = Instant::now();
    while !writer.is_finished() || frames == 0 {
        frames += 1;
        std::thread::sleep(
            (started + FRAME * frames as u32).saturating_duration_since(Instant::now()),
        );
        let tick = Instant::now();
        if let Some(batch) = hub.drain() {
            shell.apply_batch(&batch);
            batches += 1;
        }
        samples.push(tick.elapsed());
    }
    let line_writes = writer.join().expect("the writer finished");
    if let Some(batch) = hub.drain() {
        shell.apply_batch(&batch);
        batches += 1;
        frames += 1;
    }

    Burst {
        line_writes,
        frames,
        batches,
        wakes: wakes.load(Ordering::Relaxed),
        frame: Summary::of(&samples).expect("frames were timed"),
    }
}

fn subscribed_shell(hub: &StoreHub) -> Stores {
    Stores {
        terminal: hub.subscribe(),
        vitals: hub.subscribe(),
        room: hub.subscribe(),
        ..Stores::default()
    }
}

fn append(texts: impl Iterator<Item = String>) -> TerminalChange {
    TerminalChange::Append(texts.map(|text| TerminalLine { text }).collect())
}

fn vitals(n: usize) -> VitalsState {
    let hp = (n % 100).to_string();
    VitalsState(Fields::from([("hp", hp.as_str()), ("maxhp", "100")]))
}

fn room(n: usize) -> RoomState {
    let id = (n % 1_000).to_string();
    RoomState(Fields::from([
        ("num", id.as_str()),
        ("name", "A synthetic room"),
    ]))
}
