//! The measuring mode (`--measure`): frame times while replaying.
//!
//! gpui times every frame it draws (layout, prepaint, paint and the hand-off
//! to the GPU) and prints `frame duration: …` to stderr when
//! `ZED_MEASUREMENTS=1`. The binary re-launches itself with that set, runs
//! the phases below in the child, and sorts the child's frame times by the
//! phase markers it prints on the same stream, so their order is exact:
//!
//! 1. `startup`: the window opens, the session replays the full scrollback
//!    into it, then it settles.
//! 2. `stream`: lines arrive at the replay pace, pinned to the newest line.
//! 3. `scroll`: the view scrolls back through history while lines still
//!    arrive, so every frame lays out lines it hasn't shown before.
//!
//! The child is paced by gpui's frame callbacks, not by timers: on a busy
//! machine timers fire late and would thin out the frames being measured.
//! Lines are due by the clock, so the pace stays exact either way.
//!
//! Lines go through `Session::replay` on the core thread, and reach the chat
//! the way a server's do: as a store diff, applied on a later frame.
//!
//! The Room panel has [`sample_room`] in it throughout, so the frames measured
//! carry a filled panel. The sample is made up: it comes from no real session.

use crate::chat::Chat;
use crate::room_panel::{Item, Occupant, RoomContents};
use crate::shell::Shell;
use crate::stream;
use gpui::{ActivityGuard, AnyWindowHandle, App, Entity, Window, px};
use mudling_core::map::{LiveMap, PlainRules};
use mudling_core::persist::Timestamp;
use mudling_core::replay::ReplayFeed;
use mudling_core::stats::Summary;
use mudling_core::store::{MobsState, RoomState};
use serde::Serialize;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MARKER: &str = "mudling-measure: ";
const FRAME_PREFIX: &str = "frame duration: ";
const FRAME_BUDGET_US: f64 = 8_000.0;
const SETTLE: Duration = Duration::from_secs(1);
const SCROLL_DURATION: Duration = Duration::from_secs(5);
/// Scrolled per display frame: about a line and a half.
const SCROLL_STEP: f32 = 30.;

/// A filled Room panel's room, what the map remembers and what's in it.
pub fn sample_room() -> (RoomState, MobsState, RoomContents) {
    let mut map = LiveMap::new(Box::new(PlainRules));
    let info = r#"{"num": 1, "name": "A Stone Hall", "zone": "The Old Town",
        "exits": {"n": 2, "s": 3, "e": "closed"}}"#;
    map.room_info(info, Timestamp(0));
    map.note_sightings(&["(Marked) a grey wolf".to_owned()], Timestamp(0));
    let room = map
        .commit(Timestamp(0))
        .and_then(|commit| commit.change.rooms.first().map(Arc::clone));
    let mobs = MobsState::default().with_changes(&[mudling_core::map::MarkChange {
        name: "wolf".to_owned(),
        mark: mudling_core::map::Mark::Aggressive,
        set: true,
    }]);
    let item = |name: &'static str, target: &str, count| Item {
        name: name.into(),
        target: target.to_owned(),
        count,
    };
    let contents = RoomContents {
        occupants: vec![Occupant {
            name: "A traveller resting by the wall".into(),
            target: "traveller".to_owned(),
        }],
        items: vec![
            item("a worn leather satchel with a strap", "satchel", 3),
            item("some coins", "coins", 4),
            item("a blanket", "blanket", 2),
            item("a stone", "stone", 2),
            item("some rope", "rope", 1),
        ],
    };
    let room = RoomState {
        room,
        whereabouts: Default::default(),
    };
    (room, mobs, contents)
}

pub fn fill_room_panel(shell: &Shell, cx: &mut App) {
    let (room, mobs, contents) = sample_room();
    shell.room_panel().update(cx, |panel, cx| {
        panel.show_room(&room, &mobs, cx);
        panel.show_contents(contents, cx);
    });
}

/// Prints one marker for the measuring parent.
pub fn mark(message: &str) {
    eprintln!("{MARKER}{message}");
}

pub fn mark_rss(label: &str) {
    if let Some(stats) = memory_stats::memory_stats() {
        mark(&format!(
            "info {label}={:.1}",
            stats.physical_mem as f64 / (1024.0 * 1024.0)
        ));
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Load,
    Settle,
    Stream,
    Scroll,
}

struct Pacer {
    shell: Entity<Shell>,
    chat: Entity<Chat>,
    launched: Instant,
    feed: ReplayFeed,
    lines_per_second: f64,
    stream_for: Duration,
    phase: Phase,
    started: Instant,
    stream_started: Option<Instant>,
    lines_sent: u64,
    _no_app_nap: ActivityGuard,
}

impl Pacer {
    /// Runs once per display frame, before gpui decides whether to draw.
    fn on_frame(mut self, window: &mut Window, cx: &mut App) {
        if self.phase == Phase::Load {
            if !self.shell.read(cx).replay_finished() {
                window.on_next_frame(move |window, cx| self.on_frame(window, cx));
                return;
            }
            mark(&format!(
                "info load_ms={:.1}",
                self.launched.elapsed().as_secs_f64() * 1e3
            ));
            mark(&format!(
                "info scrollback_lines={}",
                self.chat.read(cx).len()
            ));
            mark_rss("rss_mb_after_load");
            self.phase = Phase::Settle;
            self.started = Instant::now();
        }
        let elapsed = self.started.elapsed();
        let phase = if elapsed < SETTLE {
            Phase::Settle
        } else if elapsed < SETTLE + self.stream_for {
            Phase::Stream
        } else if elapsed < SETTLE + self.stream_for + SCROLL_DURATION {
            Phase::Scroll
        } else {
            mark_rss("rss_mb_end");
            mark("phase end");
            window.remove_window();
            return;
        };
        if phase != self.phase {
            self.phase = phase;
            mark(match phase {
                Phase::Load | Phase::Settle => "phase startup",
                Phase::Stream => "phase stream",
                Phase::Scroll => "phase scroll",
            });
        }
        if phase != Phase::Settle {
            let stream_started = *self.stream_started.get_or_insert_with(Instant::now);
            let due = (stream_started.elapsed().as_secs_f64() * self.lines_per_second) as u64;
            if due > self.lines_sent {
                let reads = stream::next_reads(&mut self.feed, due - self.lines_sent);
                self.shell.read(cx).replay(reads);
            }
            if phase == Phase::Scroll {
                self.chat
                    .update(cx, |chat, cx| chat.scroll_back(px(SCROLL_STEP), cx));
            }
            self.lines_sent = due;
        }
        window.on_next_frame(move |window, cx| self.on_frame(window, cx));
    }
}

/// Runs the measured phases in the child's window, then closes it, which
/// quits the app the way a player's close does.
pub fn drive(
    window: AnyWindowHandle,
    shell: Entity<Shell>,
    launched: Instant,
    feed: ReplayFeed,
    lines_per_second: f64,
    stream_for: Duration,
    cx: &mut App,
) {
    let pacer = Pacer {
        chat: shell.read(cx).chat(),
        shell,
        launched,
        feed,
        lines_per_second,
        stream_for,
        phase: Phase::Load,
        started: Instant::now(),
        stream_started: None,
        lines_sent: 0,
        _no_app_nap: cx
            .background_executor()
            .prevent_app_nap("MUDling measured replay"),
    };
    pacer.shell.update(cx, |shell, cx| {
        shell.open_panels(cx);
        fill_room_panel(shell, cx);
    });
    mark(&format!(
        "info window_active={}",
        u8::from(cx.active_window().is_some())
    ));
    window
        .update(cx, |_, window, _| {
            window.on_next_frame(move |window, cx| pacer.on_frame(window, cx))
        })
        .expect("the measured window is open");
}

#[derive(Serialize)]
struct FrameReport {
    os: &'static str,
    arch: &'static str,
    optimized: bool,
    /// Other work on the machine skews frame times; this says how busy it was.
    load_average_1m: Option<f64>,
    scope: &'static str,
    lines_per_second: f64,
    stream_seconds: f64,
    time_to_first_frame_ms: Option<f64>,
    first_frame_ms: Option<f64>,
    frame_budget_us: f64,
    stream_within_budget: Option<bool>,
    phases: BTreeMap<String, Summary>,
    info: BTreeMap<String, f64>,
}

/// Runs the app as a measured child and writes the report to `report_path`.
pub fn run_parent(
    child_args: &[String],
    lines_per_second: f64,
    stream_for: Duration,
    report_path: &Path,
) -> ExitCode {
    let exe = std::env::current_exe().expect("the running binary has a path");
    let load_average_1m = load_average();
    let launched = Instant::now();
    let mut child = match Command::new(exe)
        .args(child_args)
        .arg("--measured-run")
        .env("ZED_MEASUREMENTS", "1")
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            eprintln!("couldn't start the measured run: {error}");
            return ExitCode::FAILURE;
        }
    };

    let mut phase = "startup".to_owned();
    let mut frames: BTreeMap<String, Vec<Duration>> = BTreeMap::new();
    let mut info = BTreeMap::new();
    let mut time_to_first_frame = None;
    let mut first_frame = None;
    let stderr = BufReader::new(child.stderr.take().expect("stderr is piped"));
    for line in stderr.lines().map_while(Result::ok) {
        if let Some(duration) = line
            .strip_prefix(FRAME_PREFIX)
            .and_then(parse_debug_duration)
        {
            time_to_first_frame.get_or_insert_with(|| launched.elapsed());
            first_frame.get_or_insert(duration);
            frames.entry(phase.clone()).or_default().push(duration);
        } else if let Some(marker) = line.strip_prefix(MARKER) {
            if let Some(name) = marker.strip_prefix("phase ") {
                phase = name.to_owned();
            } else if let Some((key, value)) = marker
                .strip_prefix("info ")
                .and_then(|kv| kv.split_once('='))
                && let Ok(value) = value.parse()
            {
                info.insert(key.to_owned(), value);
            }
        } else if !line.starts_with("frame duration") {
            eprintln!("{line}");
        }
    }
    let status = child.wait();
    if phase != "end" {
        eprintln!("the measured run ended early ({status:?}) during phase {phase}");
        return ExitCode::FAILURE;
    }

    let phases: BTreeMap<String, Summary> = frames
        .iter()
        .filter_map(|(name, samples)| Some((name.clone(), Summary::of(samples)?)))
        .collect();
    let report = FrameReport {
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        optimized: !cfg!(debug_assertions),
        load_average_1m,
        scope: "chat, command bar and both side panels open, the Room panel filled; no split yet",
        lines_per_second,
        stream_seconds: stream_for.as_secs_f64(),
        time_to_first_frame_ms: time_to_first_frame.map(|d| d.as_secs_f64() * 1e3),
        first_frame_ms: first_frame.map(|d| d.as_secs_f64() * 1e3),
        frame_budget_us: FRAME_BUDGET_US,
        stream_within_budget: phases.get("stream").map(|s| s.p99_us <= FRAME_BUDGET_US),
        phases,
        info,
    };
    print_report(&report);
    let json = serde_json::to_string_pretty(&report).expect("the report serializes");
    if let Some(dir) = report_path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    if let Err(error) = std::fs::write(report_path, json) {
        eprintln!("couldn't write {}: {error}", report_path.display());
        return ExitCode::FAILURE;
    }
    println!("report: {}", report_path.display());
    ExitCode::SUCCESS
}

fn print_report(report: &FrameReport) {
    if let Some(load) = report.load_average_1m {
        println!("load average (1 min) at start: {load:.1}");
    }
    if let (Some(ttff), Some(first)) = (report.time_to_first_frame_ms, report.first_frame_ms) {
        println!("time to first frame: {ttff:.0} ms (first frame took {first:.1} ms)");
    }
    for (phase, summary) in &report.phases {
        println!(
            "{phase:>8}: {} frames, p50 {:.2} ms, p99 {:.2} ms, max {:.2} ms",
            summary.samples,
            summary.p50_us / 1e3,
            summary.p99_us / 1e3,
            summary.max_us / 1e3
        );
    }
    for (key, value) in &report.info {
        println!("{key}: {value}");
    }
    if let Some(ok) = report.stream_within_budget {
        println!(
            "stream frame p99 ≤ 8 ms: {}",
            if ok { "ok" } else { "MISSED" }
        );
    }
}

/// The one-minute load average, where the OS reports one without privileges.
fn load_average() -> Option<f64> {
    let text = if cfg!(target_os = "linux") {
        std::fs::read_to_string("/proc/loadavg").ok()?
    } else if cfg!(target_os = "macos") {
        let output = Command::new("sysctl")
            .args(["-n", "vm.loadavg"])
            .output()
            .ok()?;
        String::from_utf8(output.stdout).ok()?
    } else {
        return None;
    };
    text.split_whitespace().find_map(|field| field.parse().ok())
}

/// Parses a `Duration` as its `Debug` impl prints it, e.g. `1.25ms`.
fn parse_debug_duration(text: &str) -> Option<Duration> {
    let split = text.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
    let (number, unit) = text.split_at(split);
    let value: f64 = number.parse().ok()?;
    let seconds = match unit.trim() {
        "s" => value,
        "ms" => value / 1e3,
        "µs" | "us" => value / 1e6,
        "ns" => value / 1e9,
        _ => return None,
    };
    Some(Duration::from_secs_f64(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_each_duration_unit() {
        let micros = |text| parse_debug_duration(text).map(|d| d.as_secs_f64() * 1e6);
        assert_eq!(micros("1.5ms"), Some(1500.0));
        assert_eq!(micros("250µs"), Some(250.0));
        assert_eq!(micros("2s"), Some(2_000_000.0));
        assert_eq!(micros("500ns"), Some(0.5));
        assert_eq!(micros("fast"), None);
    }

    #[test]
    fn parses_what_duration_debug_prints() {
        for duration in [
            Duration::from_nanos(812),
            Duration::from_micros(4321),
            Duration::from_millis(17),
        ] {
            let parsed = parse_debug_duration(&format!("{duration:?}")).unwrap();
            assert!((parsed.as_secs_f64() - duration.as_secs_f64()).abs() < 1e-9);
        }
    }
}
