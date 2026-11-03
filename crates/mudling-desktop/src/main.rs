//! `mudling-desktop`: MUDling's gpui shell, a thin window over a `Session`.
//!
//! ```text
//! mudling-desktop [--host <host> --port <port> | --replay <session.log>… | --synthetic]
//!                 [--plugin <folder>]… [--enable <plugin id>]… [--data <folder>]
//!                 [--long-lines] [--rate <lines/s>]
//!                 [--measure [--seconds <n>] [--report <path>] [--settings-open]
//!                            [--settings-section plugins|inspector|server-plugins|directory|
//!                                                approval|add-plugin|data|vault|logs]
//!                            [--hud attached|open|floating|dragged|strip|hidden]
//!                            [--find-open] [--find-panel] [--notes-open] [--character-open]
//!                            [--skills-open] [--sample-map <rooms>]]
//!                 [--ax-check]
//! ```
//!
//! With no arguments the window shows the saved servers ("Where to today?").
//! `--host` and `--port` connect to a server at once. The plugins bundled with
//! the app are installed, off, as are those in the data folder and any
//! `--plugin` folder, so game detection can suggest them; `--enable` turns one
//! on and lets it send commands. A plugin in a folder replaces a bundled one
//! with its id.
//!
//! A replay runs the same window through `Session::replay`: 100,000 lines of
//! history, then new lines at `--rate` (50 a second by default; 0 turns it
//! off), with its data in a throwaway folder. `--long-lines` joins its lines
//! into ones that wrap. `--measure` records frame times and writes a JSON
//! report (see `measure`); `--settings-open` measures it with the settings
//! sheet open, `--settings-section` with that section open and filled with
//! made-up rows, `--hud` with the HUD in that place, `--find-open` with the find
//! bar open and results showing, `--find-panel` with the Find panel showing and
//! being typed in, `--notes-open` with a note being typed in the Notes tab,
//! `--gear-open` with the Gear tab open on a full gear database,
//! `--gear-sets-open` with that tab listing gear sets above Worn,
//! `--character-open` and `--skills-open` with that tab showing a sample, and
//! `--sample-map` with a made-up map.
//! `--ax-check` (macOS) checks the
//! accessibility tree and exits (see `macos`). `--map-harness` shows the map
//! alone (see `map_view::harness`).

// A release build on Windows opens no console window. Its output to a terminal
// is lost, but measured runs still report through their piped stderr.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

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
mod character_panel;
mod chat;
mod dock;
mod dpad;
mod empty_state;
mod find_panel;
mod gear_panel;
mod hotbar;
mod hud;
mod input;
mod launcher;
#[cfg(target_os = "macos")]
mod macos;
mod map_history;
mod map_view;
mod measure;
mod motion;
mod notes;
mod room_panel;
mod session_bar;
mod settings;
mod shell;
mod skills_panel;
mod sound;
mod status;
mod stream;
mod theme;
mod title_bar;
#[cfg(all(test, target_os = "macos"))]
mod window_images;

use dock::Slot;
use futures::channel::mpsc;
use futures::future::{self, Either};
use gpui::{
    App, AppContext, AsyncApp, Bounds, Entity, KeyBinding, Task, TitlebarOptions, WindowBounds,
    WindowDecorations, WindowOptions, actions, px, size,
};
use launcher::{Launcher, Target, bundled_plugins};
use measure::HudRun;
use measure::SamplePage;
use mudling_core::git;
use mudling_core::plugin::{Plugin, PluginId};
use mudling_core::replay::{self, ReplayFeed};
use mudling_core::session::{Session, SessionConfig};
use mudling_core::sync::{RepoSettings, SyncConfig, Syncer};
use mudling_core::vault::OsKeyStore;
use room_panel::Verbs;
use settings::{PluginRow, SavedSettings, Section, app_settings};
use shell::{Game, LEAVE_WAIT, Shell};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use theme::Appearance;

const SCROLLBACK_LINES: usize = 100_000;
/// A replay has no server; its data folder holds this one's database.
const REPLAY_HOST: &str = "replay";
const USAGE: &str = "usage: mudling-desktop [--host <host> --port <port> | --replay <session.log>… | --synthetic] \
                     [--plugin <folder>]… [--enable <plugin id>]… [--data <folder>] [--long-lines] \
                     [--rate <lines/s>] [--measure [--seconds <n>] [--report <path>] \
                     [--settings-open] \
                     [--settings-section plugins|inspector|server-plugins|directory|approval|\
                     add-plugin|data|vault|logs] \
                     [--hud attached|open|floating|dragged|strip|hidden] \
                     [--find-open] [--find-panel] [--notes-open] [--gear-open] \
                     [--gear-sets-open] [--character-open] [--skills-open] \
                     [--sample-map <rooms>]] [--ax-check]";

actions!(mudling, [Quit]);

struct Options {
    host: Option<String>,
    port: Option<u16>,
    plugins: Vec<PathBuf>,
    enable: Vec<String>,
    data: Option<PathBuf>,
    replay: Vec<PathBuf>,
    synthetic: bool,
    long_lines: bool,
    lines_per_second: f64,
    measure: bool,
    measured_run: bool,
    stream_for: Duration,
    report: PathBuf,
    settings_open: bool,
    settings_section: Option<Section>,
    settings_page: Option<SamplePage>,
    hud: Option<HudRun>,
    find_open: bool,
    find_panel: bool,
    notes_open: bool,
    gear_open: bool,
    gear_sets_open: bool,
    sheet_tab: Option<Slot>,
    sample_map: Option<i64>,
    ax_check: bool,
}

fn main() -> ExitCode {
    let launched = Instant::now();
    git::prepare();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--map-harness") {
        // Its own app, kept in the background like `in_background` below.
        return map_view::harness::run(&args);
    }
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
    let waker = move || {
        let _ = wake.unbounded_send(());
    };
    let kept_settings = keep_settings(&options);
    let opened = load_feed(&options).and_then(|feed| {
        let (game, launcher, syncer) = match feed {
            Some(_) => (Some(open_replay(&options, waker)?), None, None),
            None => {
                let syncer = start_sync(&options, kept_settings.as_ref(), waker.clone());
                let launcher = launcher_for(&options, waker)?.with_sync(syncer.clone());
                let target = options.host.clone().zip(options.port);
                let game = target
                    .map(|(host, port)| launcher.open(&Target { host, port }, None, None))
                    .transpose()?;
                (game, Some(launcher), syncer)
            }
        };
        Ok((feed, game, launcher, syncer))
    });
    let (feed, game, launcher, syncer) = match opened {
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

    // A check must never come forward: the player may be working.
    let in_background = options.measured_run || options.ax_check;
    let application = gpui_platform::application().with_assets(assets::Assets);
    // Set before launch, as an accessory app: no Dock icon, no ⌘-Tab entry.
    #[cfg(target_os = "macos")]
    let application = match in_background {
        true => application.with_windowing(gpui::WindowingRequest::Headless),
        false => application,
    };
    application.run(move |cx: &mut App| {
        assets::load_fonts(cx);
        motion::follow_os(cx);
        bind_keys(cx);

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
            focus: !in_background,
            // macOS shows it only once `macos::show_unseen` has hidden it.
            show: !(in_background && cfg!(target_os = "macos")),
            // A measured run draws every frame even if another app has focus.
            inactive_frame_interval: if options.measured_run {
                None
            } else {
                WindowOptions::default().inactive_frame_interval
            },
            ..WindowOptions::default()
        };
        let window = match cx.open_window(options_for_window, |window, cx| {
            cx.new(|cx| {
                let mut shell = Shell::new(game, woken, window, cx);
                if let Some((saved, choice)) = kept_settings {
                    shell.restore_settings(saved, choice, window, cx);
                }
                if let Some(syncer) = syncer {
                    shell.use_sync(syncer, Arc::new(OsKeyStore), window, cx);
                }
                // The accessibility check reads the chat and the command bar.
                if let Some(launcher) = launcher.filter(|_| !options.ax_check) {
                    shell.use_launcher(launcher, window, cx);
                }
                shell
            })
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
        if in_background {
            #[cfg(target_os = "macos")]
            macos::show_unseen(cx);
            measure::fail_if_frames_stop(window.into(), cx);
        } else {
            cx.activate(true);
        }

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
                if options.settings_open {
                    window
                        .update(cx, |shell, window, cx| {
                            match (options.settings_page, options.settings_section) {
                                (Some(SamplePage::Inspector), _) => {
                                    measure::open_sample_inspector(shell, &[], window, cx);
                                }
                                (Some(SamplePage::ServerPlugins), _) => {
                                    measure::open_sample_server_plugins(shell, window, cx);
                                }
                                (Some(SamplePage::Directory), _) => {
                                    measure::open_sample_directory(shell, window, cx);
                                }
                                (Some(SamplePage::Approval), _) => {
                                    measure::open_sample_approval(shell, window, cx);
                                }
                                (Some(SamplePage::AddPlugin), _) => {
                                    measure::open_sample_add_plugin(shell, window, cx);
                                }
                                (None, Some(section)) => {
                                    measure::open_sample_section(shell, section, window, cx);
                                }
                                (None, None) => shell.open_settings(window, cx),
                            }
                        })
                        .expect("the measured window is open");
                }
                if let Some(hud) = options.hud {
                    window
                        .update(cx, |shell, window, cx| hud.put(shell, window, cx))
                        .expect("the measured window is open");
                }
                if options.notes_open {
                    window
                        .update(cx, measure::open_notes)
                        .expect("the measured window is open");
                }
                if options.gear_open {
                    window
                        .update(cx, |shell, _, cx| measure::fill_full_gear(shell, cx))
                        .expect("the measured window is open");
                }
                if options.gear_sets_open {
                    window
                        .update(cx, |shell, _, cx| measure::fill_full_gear_sets(shell, cx))
                        .expect("the measured window is open");
                }
                measure::drive(
                    window.into(),
                    shell,
                    launched,
                    feed,
                    measure::Run {
                        lines_per_second: options.lines_per_second,
                        stream_for: options.stream_for,
                        hud: options.hud,
                        find_open: options.find_open,
                        find_panel: options.find_panel,
                        notes_open: options.notes_open,
                        sheet_tab: options.sheet_tab,
                    },
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

/// Every shortcut. `settings::shortcuts` lists them for the player.
pub(crate) fn bind_keys(cx: &mut App) {
    input::bind_keys(cx);
    map_history::bind_keys(cx);
    map_view::bind_keys(cx);
    shell::bind_keys(cx);
    hud::bind_keys(cx);
    title_bar::bind_keys(cx);
    empty_state::bind_keys(cx);
    session_bar::bind_keys(cx);
    settings::bind_keys(cx);
    character_panel::bind_keys(cx);
    cx.bind_keys([KeyBinding::new(secondary!("q"), Quit, None)]);
}

/// Opens what keeps the player's settings. Measured runs and checks leave
/// them alone, so their results don't depend on the machine's choices. Without
/// a folder the app runs on the defaults.
fn keep_settings(options: &Options) -> Option<(SavedSettings, Appearance)> {
    if options.measured_run || options.ax_check {
        return None;
    }
    let opened = data_folder(options)
        .and_then(|folder| SavedSettings::open(&folder).map_err(|error| error.to_string()));
    opened
        .inspect_err(|error| eprintln!("settings won't be kept: {error}"))
        .ok()
}

/// Starts sync for a window that keeps the player's settings. Measured runs
/// and checks keep none, so they leave the player's repo alone too. Returns at
/// once: the first sync runs on sync's own thread.
fn start_sync(
    options: &Options,
    kept: Option<&(SavedSettings, Appearance)>,
    waker: impl Fn() + Send + Sync + 'static,
) -> Option<Syncer> {
    let (saved, appearance) = kept?;
    let choices = saved.sync_choices();
    let config = SyncConfig {
        repo: choices.repo.map(RepoSettings::new),
        settings: app_settings(*appearance, choices.settings_updated),
        ..SyncConfig::new(data_folder(options).ok()?)
    };
    Syncer::start(config, waker)
        .inspect_err(|error| eprintln!("sync won't run: {error}"))
        .ok()
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

/// Waits for [`Shell::leave`]'s task, up to [`LEAVE_WAIT`].
async fn wait_for_save(saved: Task<()>, cx: &AsyncApp) {
    let timeout = cx.background_executor().timer(LEAVE_WAIT);
    if let Either::Right(_) = future::select(saved, timeout).await {
        eprintln!("the session didn't finish saving in time");
    }
}

/// The session for a recording, with its data in a throwaway folder.
fn open_replay(
    options: &Options,
    waker: impl Fn() + Send + Sync + 'static,
) -> Result<Game, String> {
    let folder = TempDir::new()
        .map_err(|error| format!("couldn't make a folder for the replay: {error}"))?;
    let plugins = command_line_plugins(options)?;
    let enabled = enabled_plugins(&plugins, &options.enable)?;
    let verbs = Verbs::of(&plugins, &enabled);
    let mut plugin_rows = PluginRow::for_session(&plugins, &enabled, &[], REPLAY_HOST);
    PluginRow::approve(&mut plugin_rows, &enabled);
    if let Some(rooms) = options.sample_map {
        measure::save_sample_map(folder.path(), REPLAY_HOST, rooms)
            .map_err(|error| format!("couldn't save the sample map: {error}"))?;
    }
    let config = SessionConfig {
        plugins,
        enabled: enabled.clone(),
        approved: enabled.clone(),
        ..SessionConfig::new(folder.path(), REPLAY_HOST, 0)
    };
    let session = Session::open(config, waker)
        .map_err(|error| format!("couldn't start the session: {error}"))?;
    Ok(Game {
        session,
        replay_folder: Some(folder),
        enabled,
        verbs,
        plugins: plugin_rows,
        target: Target {
            host: REPLAY_HOST.to_owned(),
            port: 0,
        },
    })
}

/// Opens games on real servers, with the bundled plugins, the data folder's
/// and those the command line names, each replacing the last.
fn launcher_for(
    options: &Options,
    waker: impl Fn() + Send + Sync + 'static,
) -> Result<Launcher, String> {
    let folder = data_folder(options)?;
    let named = command_line_plugins(options)?;
    let enabled = enabled_plugins(&named, &options.enable)?;
    let launcher = Launcher::new(folder, bundled_plugins(), enabled, waker);
    Ok(launcher
        .with_named(named)
        .with_installed()
        .with_added_folders())
}

fn command_line_plugins(options: &Options) -> Result<Vec<Plugin>, String> {
    options
        .plugins
        .iter()
        .map(Plugin::load)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())
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
        long_lines: false,
        lines_per_second: 50.0,
        measure: false,
        measured_run: false,
        stream_for: Duration::from_secs(20),
        report: PathBuf::from("reports/frame-report.json"),
        settings_open: false,
        settings_section: None,
        settings_page: None,
        hud: None,
        find_open: false,
        find_panel: false,
        notes_open: false,
        gear_open: false,
        gear_sets_open: false,
        sheet_tab: None,
        sample_map: None,
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
            "--long-lines" => options.long_lines = true,
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
            "--sample-map" => {
                options.sample_map = Some(
                    value()?
                        .parse()
                        .map_err(|_| "--sample-map needs a whole number")?,
                )
            }
            "--settings-open" => options.settings_open = true,
            "--settings-section" => {
                options.settings_open = true;
                match value()?.as_str() {
                    "plugins" => options.settings_section = Some(Section::Plugins),
                    "data" => options.settings_section = Some(Section::Data),
                    "vault" => options.settings_section = Some(Section::Vault),
                    "logs" => options.settings_section = Some(Section::Logs),
                    "inspector" => options.settings_page = Some(SamplePage::Inspector),
                    "server-plugins" => options.settings_page = Some(SamplePage::ServerPlugins),
                    "directory" => options.settings_page = Some(SamplePage::Directory),
                    "approval" => options.settings_page = Some(SamplePage::Approval),
                    "add-plugin" => options.settings_page = Some(SamplePage::AddPlugin),
                    _ => {
                        return Err("--settings-section needs plugins, inspector, \
                                    server-plugins, directory, approval, add-plugin, data, \
                                    vault or logs"
                            .into());
                    }
                }
            }
            "--hud" => {
                options.hud = Some(HudRun::named(value()?).ok_or("--hud doesn't know that place")?)
            }
            "--find-open" => options.find_open = true,
            "--find-panel" => options.find_panel = true,
            "--notes-open" => options.notes_open = true,
            "--gear-open" => options.gear_open = true,
            "--gear-sets-open" => options.gear_sets_open = true,
            "--character-open" => options.sheet_tab = Some(Slot::Character),
            "--skills-open" => options.sheet_tab = Some(Slot::Skills),
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
    if options.long_lines && !replaying {
        return Err("--long-lines needs --replay <logs> or --synthetic".into());
    }
    if options.sample_map.is_some() && !replaying {
        return Err("--sample-map needs --replay <logs> or --synthetic".into());
    }
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
    if options.hud.is_some() && !options.measure && !options.measured_run {
        return Err("--hud needs --measure".into());
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
    let lines = if options.long_lines {
        stream::long_lines(&lines)
    } else {
        lines
    };
    Ok(Some(ReplayFeed::new(lines)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use launcher::bundled_plugins;

    fn parse(args: &str) -> Result<Options, String> {
        let args: Vec<String> = args.split_whitespace().map(str::to_owned).collect();
        parse_options(&args)
    }

    fn refusal(args: &str) -> String {
        parse(args).err().expect("the options are refused")
    }

    #[test]
    fn no_options_is_the_saved_servers_with_nothing_to_replay() {
        let options = parse("").unwrap();
        assert_eq!(options.host, None);
        assert!(options.replay.is_empty() && !options.synthetic && !options.measure);
    }

    #[test]
    fn a_server_and_plugins_are_read_from_their_flags() {
        let options =
            parse("--host mud.example --port 4000 --plugin a --plugin b --enable c").unwrap();
        assert_eq!(options.host.as_deref(), Some("mud.example"));
        assert_eq!(options.port, Some(4000));
        assert_eq!(options.plugins, [PathBuf::from("a"), PathBuf::from("b")]);
        assert_eq!(options.enable, ["c"]);
    }

    #[test]
    fn a_bare_path_is_a_recording_and_the_replay_flag_only_reads_better() {
        let options = parse("--replay one.log two.log --rate 10").unwrap();
        let logs = [PathBuf::from("one.log"), PathBuf::from("two.log")];
        assert_eq!(options.replay, logs);
        assert_eq!(options.lines_per_second, 10.0);
    }

    #[test]
    fn a_measured_run_needs_lines_and_a_positive_rate() {
        assert!(parse("--measure --synthetic --seconds 3").is_ok());
        assert!(refusal("--measure").contains("--measure needs"));
        assert!(refusal("--measure --synthetic --rate 0").contains("--rate"));
    }

    #[test]
    fn options_that_cannot_go_together_are_refused() {
        assert!(refusal("--host mud.example").contains("go together"));
        assert!(refusal("--port 4000").contains("go together"));
        assert!(refusal("--synthetic one.log").contains("either"));
        assert!(refusal("--host a --port 1 --synthetic").contains("either"));
        assert!(refusal("--long-lines").contains("--long-lines needs"));
    }

    #[test]
    fn bad_values_are_refused_by_name() {
        assert!(refusal("--port").contains("--port needs a value"));
        assert!(refusal("--port many").contains("port number"));
        assert!(refusal("--rate fast").contains("--rate needs"));
        assert!(refusal("--seconds 1.5").contains("whole number"));
        assert!(refusal("--nonsense").contains("unknown option --nonsense"));
    }

    #[test]
    fn enabling_a_plugin_nothing_installs_is_refused() {
        let plugins = bundled_plugins();
        let id = plugins[0].id().clone();
        let enabled = enabled_plugins(&plugins, &[id.as_str().to_owned()]).unwrap();
        assert_eq!(enabled, [id]);
        let refused = enabled_plugins(&plugins, &["nothing".to_owned()]).unwrap_err();
        assert!(refused.contains("--enable nothing"));
    }

    #[test]
    fn a_replay_opens_in_a_throwaway_folder() {
        let game = open_replay(&parse("--synthetic").unwrap(), || {}).unwrap();
        assert_eq!(game.target.host, REPLAY_HOST);
        assert!(game.replay_folder.is_some());
        game.session.quit().wait().unwrap();
    }

    #[test]
    fn a_replay_can_only_enable_a_plugin_it_was_given() {
        let options = parse("--synthetic --enable nothing").unwrap();
        let refused = open_replay(&options, || {}).err().unwrap();
        assert!(refused.contains("--enable nothing"));
    }
}
