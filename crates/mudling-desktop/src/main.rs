//! `mudling-desktop`: MUDling's gpui shell, a thin window over a `Session`.
//!
//! ```text
//! mudling-desktop [--host <host> --port <port> | --replay <session.log>… | --synthetic]
//!                 [--plugin <folder>]… [--enable <plugin id>]… [--data <folder>]
//!                 [--rate <lines/s>] [--measure [--seconds <n>] [--report <path>]]
//!                 [--ax-check]
//! ```
//!
//! `--host` and `--port` connect to a server. `--plugin` installs a plugin
//! from its folder, off, so game detection can suggest it; `--enable` turns
//! one on and lets it send commands.
//!
//! A replay runs the same window through `Session::replay`: 100,000 lines of
//! history, then new lines at `--rate` (50 a second by default; 0 turns it
//! off), with its data in a throwaway folder. `--measure` records frame times
//! and writes a JSON report (see `measure`). `--ax-check` (macOS) checks the
//! accessibility tree and exits (see `macos`).

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

mod assets;
mod chat;
mod dock;
mod dpad;
mod hud;
mod input;
#[cfg(target_os = "macos")]
mod macos;
mod measure;
mod motion;
mod room_panel;
mod shell;
mod status;
mod stream;
mod theme;
mod title_bar;
#[cfg(all(test, target_os = "macos"))]
mod window_images;

use futures::channel::mpsc;
use futures::future::{self, Either};
use gpui::{
    App, AppContext, AsyncApp, Bounds, Entity, KeyBinding, Task, TitlebarOptions, WindowBounds,
    WindowDecorations, WindowOptions, actions, px, size,
};
use mudling_core::plugin::{Plugin, PluginId};
use mudling_core::replay::{self, ReplayFeed};
use mudling_core::session::{Session, SessionConfig};
use room_panel::Verbs;
use shell::{Game, Shell};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const SCROLLBACK_LINES: usize = 100_000;
/// A replay has no server; its data folder holds this one's database.
const REPLAY_HOST: &str = "replay";
/// How long a closed window waits for the session to reach the disk. A stuck
/// hook or disk can lose what's unsaved, but can't keep the app from quitting.
const QUIT_WAIT: Duration = Duration::from_secs(5);
const USAGE: &str = "usage: mudling-desktop [--host <host> --port <port> | --replay <session.log>… | --synthetic] \
                     [--plugin <folder>]… [--enable <plugin id>]… [--data <folder>] [--rate <lines/s>] \
                     [--measure [--seconds <n>] [--report <path>]] [--ax-check]";

actions!(mudling, [Quit]);

struct Options {
    host: Option<String>,
    port: Option<u16>,
    plugins: Vec<PathBuf>,
    enable: Vec<String>,
    data: Option<PathBuf>,
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

    // Everything that reads the disk happens here, before the app starts:
    // from `run` on, this is the UI thread.
    let (wake, woken) = mpsc::unbounded();
    let opened = load_feed(&options).and_then(|feed| {
        let game = open_game(&options, feed.is_some(), move || {
            let _ = wake.unbounded_send(());
        })?;
        Ok((feed, game))
    });
    let (feed, game) = match opened {
        Ok(opened) => opened,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    let feed = feed.map(|mut feed| {
        let history = stream::next_reads(&mut feed, SCROLLBACK_LINES as u64);
        (feed, history)
    });
    if options.measured_run
        && let Some((feed, _)) = &feed
    {
        measure::mark(&format!("info source_lines={}", feed.source_len()));
    }
    let fullest_room = options
        .measured_run
        .then(|| measure::fullest_room(&options.replay))
        .flatten();

    let application = gpui_platform::application().with_assets(assets::Assets);
    application.run(move |cx: &mut App| {
        assets::load_fonts(cx);
        motion::follow_os(cx);
        input::bind_keys(cx);
        shell::bind_keys(cx);
        title_bar::bind_keys(cx);
        cx.bind_keys([KeyBinding::new(secondary!("q"), Quit, None)]);

        let bounds = Bounds::centered(None, size(px(1300.), px(800.)), cx);
        let options_for_window = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some("MUDling".into()),
                appears_transparent: true,
                traffic_light_position: Some(title_bar::TRAFFIC_LIGHTS),
            }),
            window_decorations: cfg!(target_os = "linux").then_some(WindowDecorations::Client),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(480.), px(400.))),
            // A measured run draws every frame even if another app has focus.
            inactive_frame_interval: if options.measured_run {
                None
            } else {
                WindowOptions::default().inactive_frame_interval
            },
            ..WindowOptions::default()
        };
        let window = match cx.open_window(options_for_window, |window, cx| {
            cx.new(|cx| Shell::new(game, woken, window, cx))
        }) {
            Ok(window) => window,
            Err(error) => {
                eprintln!("couldn't open the window: {error}");
                cx.quit();
                return;
            }
        };
        let shell = window.entity(cx).expect("the window was opened just now");
        cx.on_action({
            let shell = shell.clone();
            move |_: &Quit, cx| shut_down(&shell, cx)
        });
        cx.on_window_closed({
            let shell = shell.clone();
            move |cx, _| shut_down(&shell, cx)
        })
        .detach();
        cx.activate(true);

        let feed = feed.map(|(feed, history)| {
            shell.read(cx).replay(history);
            feed
        });
        match feed {
            #[cfg(target_os = "macos")]
            _ if options.ax_check => macos::run_ax_check(shell, cx),
            Some(feed) if options.measured_run => {
                shell.update(cx, |shell, cx| {
                    measure::fill_room_panel(shell, fullest_room, cx);
                });
                measure::drive(
                    window.into(),
                    shell,
                    launched,
                    feed,
                    options.lines_per_second,
                    options.stream_for,
                    cx,
                );
            }
            Some(feed) if options.lines_per_second > 0.0 => {
                stream::stream(shell, feed, options.lines_per_second, cx).detach();
            }
            _ => {}
        }
    });
    ExitCode::SUCCESS
}

/// Ends the session, then the app, without holding up the UI thread.
fn shut_down(shell: &Entity<Shell>, cx: &mut App) {
    let Some(saved) = shell.update(cx, |shell, cx| shell.leave(cx)) else {
        return;
    };
    cx.spawn(async move |cx| {
        wait_for_save(saved, cx).await;
        cx.update(|cx| cx.quit());
    })
    .detach();
}

/// Waits for [`Shell::leave`]'s task, up to [`QUIT_WAIT`].
async fn wait_for_save(saved: Task<()>, cx: &AsyncApp) {
    let timeout = cx.background_executor().timer(QUIT_WAIT);
    if let Either::Right(_) = future::select(saved, timeout).await {
        eprintln!("the session didn't finish saving in time");
    }
}

/// The session the options ask for. `None` with neither a server nor a
/// recording.
fn open_game(
    options: &Options,
    replaying: bool,
    waker: impl Fn() + Send + Sync + 'static,
) -> Result<Option<Game>, String> {
    let replay_folder = replaying
        .then(TempDir::new)
        .transpose()
        .map_err(|error| format!("couldn't make a folder for the replay: {error}"))?;
    let (host, port, data_folder) = match (&options.host, options.port, &replay_folder) {
        (_, _, Some(folder)) => (REPLAY_HOST, 0, folder.path().to_owned()),
        (Some(host), Some(port), None) => (host.as_str(), port, data_folder(options)?),
        _ => return Ok(None),
    };
    let plugins = options
        .plugins
        .iter()
        .map(Plugin::load)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let enabled = enabled_plugins(&plugins, &options.enable)?;
    let verbs = Verbs::of(&plugins, &enabled);
    let config = SessionConfig {
        plugins,
        enabled: enabled.clone(),
        approved: enabled.clone(),
        ..SessionConfig::new(data_folder, host, port)
    };
    let session = Session::open(config, waker)
        .map_err(|error| format!("couldn't start the session: {error}"))?;
    Ok(Some(Game {
        session,
        replay_folder,
        enabled,
        verbs,
    }))
}

fn data_folder(options: &Options) -> Result<PathBuf, String> {
    options
        .data
        .clone()
        .or_else(mudling_core::data_folder)
        .ok_or_else(|| "no home folder; pass --data <folder>".to_owned())
}

/// The ids `--enable` named, each checked against what's installed.
fn enabled_plugins(plugins: &[Plugin], enable: &[String]) -> Result<Vec<PluginId>, String> {
    enable
        .iter()
        .map(|id| {
            plugins
                .iter()
                .map(Plugin::id)
                .find(|plugin| plugin.as_str() == id)
                .cloned()
                .ok_or_else(|| format!("--enable {id}: no --plugin folder installs that id"))
        })
        .collect()
}

fn parse_options(args: &[String]) -> Result<Options, String> {
    let mut options = Options {
        host: None,
        port: None,
        plugins: Vec::new(),
        enable: Vec::new(),
        data: None,
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
            "--host" => options.host = Some(value()?.clone()),
            "--port" => {
                options.port = Some(value()?.parse().map_err(|_| "--port needs a port number")?)
            }
            "--plugin" => options.plugins.push(PathBuf::from(value()?)),
            "--enable" => options.enable.push(value()?.clone()),
            "--data" => options.data = Some(PathBuf::from(value()?)),
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
    let replaying = options.synthetic || !options.replay.is_empty();
    if options.host.is_some() != options.port.is_some() {
        return Err("--host and --port go together".into());
    }
    if options.host.is_some() && replaying {
        return Err("use either --host or a replay".into());
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
