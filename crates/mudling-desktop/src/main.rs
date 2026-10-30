//! `mudling-desktop`: the Phase 0 spike of MUDling's gpui shell.
//!
//! ```text
//! mudling-desktop [--replay <session.log>… | --synthetic] [--rate <lines/s>]
//!                 [--measure [--seconds <n>] [--report <path>]] [--ax-check]
//! ```
//!
//! With a replay, the chat starts with 100,000 lines of history, then new
//! lines arrive at `--rate` (50 a second by default; 0 turns it off).
//! `--measure` records frame times and writes a JSON report (see `measure`).
//! `--ax-check` (macOS) checks the accessibility tree and exits (see `macos`).

/// A shortcut on the platform's main modifier: ⌘ on macOS, Ctrl elsewhere.
macro_rules! secondary {
    ($key:literal) => {
        if cfg!(target_os = "macos") {
            concat!("cmd-", $key)
        } else {
            concat!("ctrl-", $key)
        }
    };
}

mod chat;
mod input;
#[cfg(target_os = "macos")]
mod macos;
mod measure;
mod shell;
mod stream;
mod theme;

use chat::Chat;
use gpui::{
    App, AppContext, Bounds, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions, actions, px,
    size,
};
use mudling_core::replay::{self, ReplayFeed};
use shell::Shell;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

const SCROLLBACK_LINES: usize = 100_000;
const USAGE: &str = "usage: mudling-desktop [--replay <session.log>… | --synthetic] [--rate <lines/s>] \
                     [--measure [--seconds <n>] [--report <path>]] [--ax-check]";

actions!(mudling, [Quit]);

struct Options {
    replay: Vec<PathBuf>,
    synthetic: bool,
    lines_per_second: f64,
    measure: bool,
    measured_run: bool,
    stream_for: Duration,
    report: PathBuf,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    ax_check: bool,
}

fn main() -> ExitCode {
    let launched = Instant::now();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let options = match parse_options(&args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    if options.measure {
        let child_args: Vec<String> = args
            .iter()
            .filter(|arg| *arg != "--measure")
            .cloned()
            .collect();
        return measure::run_parent(
            &child_args,
            options.lines_per_second,
            options.stream_for,
            &options.report,
        );
    }

    let mut chat = Chat::new(SCROLLBACK_LINES);
    let feed = match load_feed(&options) {
        Ok(feed) => feed,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    let feed = feed.map(|mut feed| {
        chat.load_history(&mut feed, SCROLLBACK_LINES);
        feed
    });
    if options.measured_run {
        measure::mark(&format!(
            "info load_ms={:.1}",
            launched.elapsed().as_secs_f64() * 1e3
        ));
        measure::mark(&format!("info scrollback_lines={}", chat.len()));
        if let Some(feed) = &feed {
            measure::mark(&format!("info source_lines={}", feed.source_len()));
        }
        measure::mark_rss("rss_mb_after_load");
    }

    gpui_platform::application().run(move |cx: &mut App| {
        input::bind_keys(cx);
        cx.bind_keys([KeyBinding::new(secondary!("q"), Quit, None)]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_window_closed(|cx, _| cx.quit()).detach();

        let chat = cx.new(|_| chat);
        let bounds = Bounds::centered(None, size(px(1100.), px(800.)), cx);
        let options_for_window = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some("MUDling".into()),
                ..TitlebarOptions::default()
            }),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            // A measured run draws every frame even if another app has focus.
            inactive_frame_interval: if options.measured_run {
                None
            } else {
                WindowOptions::default().inactive_frame_interval
            },
            ..WindowOptions::default()
        };
        let shell_chat = chat.clone();
        let window = match cx.open_window(options_for_window, |window, cx| {
            cx.new(|cx| Shell::new(shell_chat, window, cx))
        }) {
            Ok(window) => window,
            Err(error) => {
                eprintln!("couldn't open the window: {error}");
                cx.quit();
                return;
            }
        };
        cx.activate(true);

        match feed {
            #[cfg(target_os = "macos")]
            _ if options.ax_check => macos::run_ax_check(chat, cx),
            Some(feed) if options.measured_run => {
                measure::drive(
                    window.into(),
                    chat,
                    feed,
                    options.lines_per_second,
                    options.stream_for,
                    cx,
                );
            }
            Some(feed) if options.lines_per_second > 0.0 => {
                stream::stream(chat, feed, options.lines_per_second, cx).detach();
            }
            _ => {}
        }
    });
    ExitCode::SUCCESS
}

fn parse_options(args: &[String]) -> Result<Options, String> {
    let mut options = Options {
        replay: Vec::new(),
        synthetic: false,
        lines_per_second: 50.0,
        measure: false,
        measured_run: false,
        stream_for: Duration::from_secs(20),
        report: PathBuf::from("reports/frame-report.json"),
        ax_check: false,
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--replay" => {}
            "--synthetic" => options.synthetic = true,
            "--rate" => {
                options.lines_per_second = value()?.parse().map_err(|_| "--rate needs a number")?
            }
            "--seconds" => {
                options.stream_for = Duration::from_secs(
                    value()?
                        .parse()
                        .map_err(|_| "--seconds needs a whole number")?,
                )
            }
            "--report" => options.report = PathBuf::from(value()?),
            "--measure" => options.measure = true,
            "--measured-run" => options.measured_run = true,
            "--ax-check" if cfg!(target_os = "macos") => options.ax_check = true,
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            path => options.replay.push(PathBuf::from(path)),
        }
    }
    if options.synthetic && !options.replay.is_empty() {
        return Err("use either --replay or --synthetic".into());
    }
    if (options.measure || options.measured_run) && options.replay.is_empty() && !options.synthetic
    {
        return Err("--measure needs --replay <logs> or --synthetic".into());
    }
    if options.lines_per_second < 0.0
        || ((options.measure || options.measured_run) && options.lines_per_second == 0.0)
    {
        return Err("--rate must be positive when measuring, and never negative".into());
    }
    Ok(options)
}

fn load_feed(options: &Options) -> Result<Option<ReplayFeed>, String> {
    let lines = if options.synthetic {
        replay::synthetic_lines(SCROLLBACK_LINES + 20_000)
    } else if options.replay.is_empty() {
        return Ok(None);
    } else {
        replay::load_logs(&options.replay)
            .map_err(|error| format!("couldn't read the logs: {error}"))?
    };
    if lines.is_empty() {
        return Err("the logs hold no server text".into());
    }
    Ok(Some(ReplayFeed::new(lines)))
}
