//! The measuring mode (`--measure`): frame times while replaying.
//!
//! gpui times every frame it draws (layout, prepaint, paint and the hand-off
//! to the GPU) and prints `frame duration: …` to stderr when
//! `ZED_MEASUREMENTS=1`. The binary re-launches itself with that set, runs
//! the phases below in the child, and sorts the child's frame times by the
//! phase markers it prints on the same stream, so their order is exact:
//!
//! 1. `startup`: the window opens on the full scrollback, then settles.
//! 2. `stream`: lines arrive at the replay pace, pinned to the newest line.
//! 3. `scroll`: the view scrolls back through history while lines still
//!    arrive, so every frame lays out lines it hasn't shown before.
//!
//! The child is paced by gpui's frame callbacks, not by timers: on a busy
//! machine timers fire late and would thin out the frames being measured.
//! Lines are due by the clock, so the pace stays exact either way.

use crate::chat::Chat;
use gpui::{ActivityGuard, AnyWindowHandle, App, Entity, Window, px};
use mudling_core::replay::ReplayFeed;
use mudling_core::stats::Summary;
use serde::Serialize;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

const MARKER: &str = "mudling-measure: ";
const FRAME_PREFIX: &str = "frame duration: ";
const FRAME_BUDGET_US: f64 = 8_000.0;
const SETTLE: Duration = Duration::from_secs(1);
const SCROLL_DURATION: Duration = Duration::from_secs(5);
/// Scrolled per display frame: about a line and a half.
const SCROLL_STEP: f32 = 30.;

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
    Settle,
    Stream,
    Scroll,
}

struct Pacer {
    chat: Entity<Chat>,
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
            cx.quit();
            return;
        };
        if phase != self.phase {
            self.phase = phase;
            mark(match phase {
                Phase::Settle => "phase startup",
                Phase::Stream => "phase stream",
                Phase::Scroll => "phase scroll",
            });
        }
        if phase != Phase::Settle {
            let stream_started = *self.stream_started.get_or_insert_with(Instant::now);
            let due = (stream_started.elapsed().as_secs_f64() * self.lines_per_second) as u64;
            let (chat, feed) = (&self.chat, &mut self.feed);
            chat.update(cx, |chat, cx| {
                for _ in self.lines_sent..due {
                    chat.push_server_line(feed.next_line(), cx);
                }
                if phase == Phase::Scroll {
                    chat.scroll_back(px(SCROLL_STEP), cx);
                }
            });
            self.lines_sent = due;
        }
        window.on_next_frame(move |window, cx| self.on_frame(window, cx));
    }
}

/// Runs the measured phases in the child's window, then quits the app.
pub fn drive(
    window: AnyWindowHandle,
    chat: Entity<Chat>,
    feed: ReplayFeed,
    lines_per_second: f64,
    stream_for: Duration,
    cx: &mut App,
) {
    let pacer = Pacer {
        chat,
        feed,
        lines_per_second,
        stream_for,
        phase: Phase::Settle,
        started: Instant::now(),
        stream_started: None,
        lines_sent: 0,
        _no_app_nap: cx
            .background_executor()
            .prevent_app_nap("MUDling measured replay"),
    };
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
        scope: "chat and command input only; Phase 0 has no panels or split",
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
