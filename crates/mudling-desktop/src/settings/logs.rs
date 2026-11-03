//! The Logs section: the debug log and each server's chat log turned on or
//! off, and what's saved, with its size, to pin, export or clear.
//!
//! Everything here touches the disk, so it runs off the UI thread, one job
//! after another in the order the player clicked. Clearing asks first, as
//! removing a server does: Keep takes the place of the Clear button.

use super::triggers::switch;
use super::{
    Pane, Section, SettingsSheet, SheetEvent, muted_line, problem_line, question_line,
    section_heading, text_button,
};
use crate::assets::UI_FONT;
use crate::theme::Theme;
use gpui::{Context, Div, FontWeight, Role, SharedString, Task, div, prelude::*, px};
use mudling_core::logs::{Kind, LogFile, LogSettings, Logs, MOST_BYTES, MOST_LOGS};
use std::io;

const ON_THIS_DEVICE: &str = "Logs stay on this device and never go into your repo. Export is \
                              the only way one leaves.";
const DEBUG_NOTE: &str = "For bug reports and plugin authors: what the server sent, never what \
                          you type. Off by default.";
const CHAT_NOTE: &str = "What you saw, for the servers you choose, never what you typed with \
                         echo off.";
const NO_SERVERS: &str = "Save a server to keep its chat.";
const KEEP_DEBUG: &str = "Keep debug logs";
const CAP: &str = "Keeps the last 10 or 100 MB, unless pinned.";
const BYTES_PER_KB: f64 = 1024.;
const BYTES_PER_MB: f64 = 1024. * 1024.;

// `CAP` spells the core's limits out.
const _: () = assert!(MOST_LOGS == 10 && MOST_BYTES == 100 * 1024 * 1024);

/// What the section shows.
#[derive(Default)]
pub(super) struct LogsPanel {
    logs: Option<Logs>,
    settings: LogSettings,
    saved: Vec<LogFile>,
    /// The kind whose clearing is being asked about.
    clearing: Option<Kind>,
    /// What the last thing done to the logs ran into.
    problem: Option<SharedString>,
    /// The job clicked last, which waits for the ones before it.
    last_job: Option<Task<()>>,
}

impl LogsPanel {
    pub(super) fn stop_asking(&mut self) {
        self.clearing = None;
    }
}

fn size_text(bytes: u64) -> String {
    let bytes = bytes as f64;
    if bytes < BYTES_PER_MB {
        format!("{:.0} KB", (bytes / BYTES_PER_KB).ceil())
    } else {
        format!("{:.1} MB", bytes / BYTES_PER_MB)
    }
}

/// How many logs of a kind there are, and what they take up.
fn summary(logs: &[&LogFile]) -> String {
    let total = logs.iter().map(|log| log.bytes).sum();
    match logs.len() {
        1 => format!("1 log, {}", size_text(total)),
        count => format!("{count} logs, {}", size_text(total)),
    }
}

/// Says what goes and what stays.
fn clearing_question(kind: Kind, logs: &[&LogFile]) -> String {
    let name = kind.name();
    let goes = logs.iter().filter(|log| !log.live).count();
    let pinned = logs.iter().filter(|log| log.pinned && !log.live).count();
    let mut question = format!("Delete {goes} {name} logs? This can't be undone.");
    if pinned > 0 {
        question.push_str(&format!(" {pinned} pinned go too."));
    }
    if logs.iter().any(|log| log.live) {
        question.push_str(" The log of the game you're in stays.");
    }
    question
}

impl SettingsSheet {
    /// Gives the section the logs to show.
    pub fn use_logs(&mut self, logs: Logs, cx: &mut Context<SettingsSheet>) {
        self.logs.logs = Some(logs);
        self.refresh_logs(cx);
    }

    /// Reads what's saved again, as a game may have added to it.
    pub(super) fn refresh_logs(&mut self, cx: &mut Context<SettingsSheet>) {
        self.logs_job(false, |_| Ok(()), cx);
    }

    pub fn show_logs(
        &mut self,
        settings: LogSettings,
        saved: Vec<LogFile>,
        cx: &mut Context<SettingsSheet>,
    ) {
        self.logs.settings = settings;
        self.logs.saved = saved;
        cx.notify();
    }

    /// Runs `job` once those before it are done, then shows what's on the
    /// disk. `settings_changed` tells the session afterwards.
    fn logs_job(
        &mut self,
        settings_changed: bool,
        job: impl FnOnce(&Logs) -> io::Result<()> + Send + 'static,
        cx: &mut Context<SettingsSheet>,
    ) {
        let Some(logs) = self.logs.logs.clone() else {
            return;
        };
        let earlier = self.logs.last_job.take();
        self.logs.last_job = Some(cx.spawn(async move |sheet, cx| {
            if let Some(earlier) = earlier {
                earlier.await;
            }
            let (done, settings, saved) = cx
                .background_spawn(async move {
                    let done = job(&logs);
                    (done, logs.settings(), logs.list())
                })
                .await;
            let problem = (done.err().as_ref())
                .or(saved.as_ref().err())
                .map(|error| error.to_string().into());
            let _ = sheet.update(cx, |sheet, cx| {
                sheet.logs.problem = problem;
                sheet.show_logs(settings, saved.unwrap_or_default(), cx);
                if settings_changed {
                    cx.emit(SheetEvent::LoggingChanged);
                }
            });
        }));
    }

    fn change_log_settings(
        &mut self,
        change: impl FnOnce(&mut LogSettings),
        cx: &mut Context<SettingsSheet>,
    ) {
        change(&mut self.logs.settings);
        let settings = self.logs.settings.clone();
        cx.notify();
        self.logs_job(true, move |logs| logs.save_settings(&settings), cx);
    }

    fn export_log(&mut self, log: LogFile, cx: &mut Context<SettingsSheet>) {
        let start_in = std::env::home_dir().unwrap_or_default();
        let picked = cx.prompt_for_new_path(&start_in, Some(&log.file_name()));
        cx.spawn(async move |sheet, cx| {
            if let Ok(Ok(Some(to))) = picked.await {
                let _ = sheet.update(cx, |sheet, cx| {
                    sheet.logs_job(false, move |logs| logs.export(&log, &to), cx);
                });
            }
        })
        .detach();
    }

    /// The hosts the player can turn a chat log on for: the saved servers,
    /// and the one in play.
    fn log_hosts(&self) -> Vec<SharedString> {
        let saved = self.servers.iter().map(|server| server.host.clone());
        let mut hosts: Vec<SharedString> = saved.chain(self.playing.clone()).collect();
        hosts.sort();
        hosts.dedup();
        hosts
    }

    pub(super) fn fill_logs(
        &mut self,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) {
        pane.row(muted_line(ON_THIS_DEVICE, theme));
        if let Some(problem) = self.logs.problem.clone() {
            pane.row(problem_line(problem, theme).debug_selector(|| "logs-problem".to_owned()));
        }

        pane.row(section_heading("Debug logs", theme));
        pane.row(muted_line(DEBUG_NOTE, theme));
        let on = self.logs.settings.debug;
        let row = switch_row(
            "log-debug".to_owned(),
            KEEP_DEBUG,
            on,
            move |sheet, cx| {
                sheet.change_log_settings(|settings| settings.debug = !on, cx);
            },
            pane,
            theme,
            cx,
        );
        pane.row(row);
        self.fill_saved(Kind::Debug, pane, theme, cx);

        pane.row(section_heading("Chat logs", theme));
        pane.row(muted_line(CHAT_NOTE, theme));
        let hosts = self.log_hosts();
        if hosts.is_empty() {
            pane.row(muted_line(NO_SERVERS, theme));
        }
        for (number, host) in hosts.into_iter().enumerate() {
            let on = self.logs.settings.keeps_chat(&host);
            let row = switch_row(
                format!("log-chat-{number}"),
                &format!("Keep chat logs for {host}"),
                on,
                move |sheet, cx| {
                    sheet.change_log_settings(|settings| settings.set_chat(&host, !on), cx);
                },
                pane,
                theme,
                cx,
            );
            pane.row(row);
        }
        self.fill_saved(Kind::Chat, pane, theme, cx);
    }

    /// What's saved of `kind`: the size, Clear with its question, and a row
    /// for each log.
    fn fill_saved(
        &mut self,
        kind: Kind,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) {
        let logs: Vec<LogFile> = (self.logs.saved.iter())
            .filter(|log| log.kind == kind)
            .cloned()
            .collect();
        let shown: Vec<&LogFile> = logs.iter().collect();
        let name = kind.name();
        let asking = self.logs.clearing == Some(kind);
        let can_clear = logs.iter().any(|log| !log.live);
        let clear = (can_clear && !asking).then(|| {
            text_button(format!("clear-{name}-logs"), "Clear", &pane.stop(cx), theme)
                .aria_label(format!("Clear {name} logs"))
                .on_click(cx.listener(move |sheet, _, _, cx| {
                    sheet.logs.clearing = Some(kind);
                    cx.notify();
                }))
        });
        let text = format!("{} · {CAP}", summary(&shown));
        pane.row(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_4()
                .pt_2()
                .child(muted_line(text, theme).flex_1().min_w_0())
                .children(clear),
        );
        if asking && can_clear {
            let keep = text_button(format!("keep-{name}-logs"), "Keep", &pane.stop(cx), theme)
                .on_click(cx.listener(|sheet, _, _, cx| {
                    sheet.logs.clearing = None;
                    cx.notify();
                }));
            let id = format!("confirm-clear-{name}-logs");
            let delete = text_button(id, "Delete", &pane.stop(cx), theme)
                .aria_label(format!("Delete {name} logs"))
                .on_click(cx.listener(move |sheet, _, window, cx| {
                    sheet.logs.clearing = None;
                    // This stop goes with the question.
                    sheet.focus_tab(Section::Logs, window, cx);
                    sheet.logs_job(false, move |logs| logs.clear(kind), cx);
                }));
            pane.row(
                div()
                    .pt_2()
                    .child(question_line(clearing_question(kind, &shown), theme))
                    .child(div().flex().gap_2().child(keep).child(delete)),
            );
        }
        for (number, log) in logs.into_iter().enumerate() {
            let row = self.log_row(number, log, pane, theme, cx);
            pane.row(row);
        }
    }

    fn log_row(
        &self,
        number: usize,
        log: LogFile,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) -> impl IntoElement + use<> {
        let name = log.kind.name();
        let started = log.started_text();
        let which = format!("{name} log of {}, {started}", log.server);
        let mut detail = format!("{started} UTC · {}", size_text(log.bytes));
        if log.live {
            detail.push_str(" · being written");
        }
        if log.pinned {
            detail.push_str(" · pinned");
        }
        let pin = {
            let (pinned, log) = (log.pinned, log.clone());
            let label = if pinned { "Unpin" } else { "Pin" };
            text_button(
                format!("pin-{name}-log-{number}"),
                label,
                &pane.stop(cx),
                theme,
            )
            .aria_label(format!("Pin {which}"))
            .aria_toggled(pinned.into())
            .on_click(cx.listener(move |sheet, _, _, cx| {
                let log = log.clone();
                sheet.logs_job(false, move |logs| logs.set_pinned(&log, !pinned), cx);
            }))
        };
        let export = {
            let id = format!("export-{name}-log-{number}");
            text_button(id, "Export", &pane.stop(cx), theme)
                .aria_label(format!("Export {which}"))
                .on_click(cx.listener({
                    let log = log.clone();
                    move |sheet, _, _, cx| sheet.export_log(log.clone(), cx)
                }))
        };
        div()
            .id(SharedString::from(format!("{name}-log-{number}")))
            .role(Role::Label)
            .aria_value(format!("{which}: {detail}"))
            .py_2()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(theme.divider)
            .font_family(UI_FONT)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(log.server),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(theme.text_muted)
                            .child(detail),
                    ),
            )
            .child(pin)
            .child(export)
    }
}

/// A line of text with its switch.
fn switch_row(
    id: String,
    what: &str,
    on: bool,
    toggle: impl Fn(&mut SettingsSheet, &mut Context<SettingsSheet>) + 'static,
    pane: &mut Pane,
    theme: &Theme,
    cx: &mut Context<SettingsSheet>,
) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .py_1()
        .font_family(UI_FONT)
        .text_size(px(15.))
        .text_color(theme.text)
        .child(what.to_owned())
        .child(
            switch(id, what, on, &pane.stop(cx), theme)
                .on_click(cx.listener(move |sheet, _, _, cx| toggle(sheet, cx))),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::SavedServer;
    use crate::shell::{Shell, tests::window_with_keys};
    use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
    use mudling_core::logs::LOGS_FOLDER;
    use mudling_core::persist::Timestamp;
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    const HOST: &str = "mud.example";

    fn click(id: &'static str, cx: &mut VisualTestContext) {
        let bounds = cx.debug_bounds(id).expect("the control is drawn");
        cx.simulate_click(bounds.center(), Modifiers::none());
        cx.run_until_parked();
    }

    fn saved_log(folder: &Path, kind: &str, started: i64) -> PathBuf {
        let path = folder
            .join(LOGS_FOLDER)
            .join(kind)
            .join(format!("{started}-{HOST}.log"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "[connected]\nOne line.\n").unwrap();
        path
    }

    /// The sheet open on the section, over a folder of logs.
    fn open_on<'a>(
        folder: &TempDir,
        cx: &'a mut TestAppContext,
    ) -> (Entity<Shell>, &'a mut VisualTestContext) {
        let (shell, cx) = window_with_keys(cx);
        let logs = Logs::in_data_folder(folder.path());
        shell.update_in(cx, |shell, window, cx| {
            shell.settings_sheet().update(cx, |sheet, cx| {
                let server = SavedServer {
                    name: HOST.into(),
                    host: HOST.into(),
                    port: 4000,
                    characters: Vec::new(),
                };
                sheet.show_servers(vec![server], None, cx);
                sheet.use_logs(logs, cx);
            });
            shell.open_settings(window, cx);
            shell
                .settings_sheet()
                .update(cx, |sheet, cx| sheet.select(Section::Logs, cx));
        });
        cx.run_until_parked();
        (shell, cx)
    }

    #[gpui::test]
    fn each_switch_keeps_its_choice_and_both_start_off(cx: &mut TestAppContext) {
        let folder = TempDir::new().unwrap();
        let (_shell, cx) = open_on(&folder, cx);
        let logs = Logs::in_data_folder(folder.path());
        assert_eq!(logs.settings(), LogSettings::default());

        click("log-debug", cx);
        click("log-chat-0", cx);
        let settings = logs.settings();
        assert!(settings.debug);
        assert!(settings.keeps_chat(HOST));

        click("log-chat-0", cx);
        assert!(!logs.settings().keeps_chat(HOST));
    }

    #[gpui::test]
    fn the_section_tells_the_session_once_a_choice_is_saved(cx: &mut TestAppContext) {
        let folder = TempDir::new().unwrap();
        let (shell, cx) = open_on(&folder, cx);
        let told = std::rc::Rc::new(std::cell::Cell::new(false));
        let seen = told.clone();
        let sheet = shell.read_with(cx, |shell, _| shell.settings_sheet());
        let _listening = cx.update(|_, cx| {
            cx.subscribe(&sheet, move |_, event, _| {
                seen.set(matches!(event, SheetEvent::LoggingChanged));
            })
        });
        click("log-debug", cx);
        assert!(told.get());
    }

    #[gpui::test]
    fn clearing_asks_first_and_keep_loses_nothing(cx: &mut TestAppContext) {
        let folder = TempDir::new().unwrap();
        let log = saved_log(folder.path(), "debug", 1_000);
        let chat = saved_log(folder.path(), "chat", 2_000);
        let (_shell, cx) = open_on(&folder, cx);

        click("clear-debug-logs", cx);
        assert!(log.exists());
        assert!(
            cx.debug_bounds("clear-debug-logs").is_none(),
            "Keep took its place"
        );
        click("keep-debug-logs", cx);
        assert!(log.exists());

        click("clear-debug-logs", cx);
        click("confirm-clear-debug-logs", cx);
        assert!(!log.exists());
        assert!(chat.exists(), "only what was asked about goes");
    }

    #[gpui::test]
    fn a_pin_is_saved_with_its_log(cx: &mut TestAppContext) {
        let folder = TempDir::new().unwrap();
        saved_log(folder.path(), "debug", 1_000);
        let (shell, cx) = open_on(&folder, cx);

        click("pin-debug-log-0", cx);
        let listed = Logs::in_data_folder(folder.path()).list().unwrap();
        assert!(listed[0].pinned);
        let shown = shell.read_with(cx, |shell, cx| {
            shell.settings_sheet().read(cx).logs.saved[0].pinned
        });
        assert!(shown);

        click("pin-debug-log-0", cx);
        assert!(!Logs::in_data_folder(folder.path()).list().unwrap()[0].pinned);
    }

    #[gpui::test]
    fn export_copies_to_where_the_player_picked_and_nowhere_without_a_pick(
        cx: &mut TestAppContext,
    ) {
        let folder = TempDir::new().unwrap();
        let picked = TempDir::new().unwrap();
        saved_log(folder.path(), "debug", 1_000);
        let (_shell, cx) = open_on(&folder, cx);

        click("export-debug-log-0", cx);
        cx.simulate_new_path_selection(|_| None);
        cx.run_until_parked();
        assert_eq!(fs::read_dir(picked.path()).unwrap().count(), 0);

        let to = picked.path().join("report.log");
        let chosen = to.clone();
        click("export-debug-log-0", cx);
        cx.simulate_new_path_selection(move |_| Some(chosen));
        cx.run_until_parked();
        assert_eq!(fs::read_to_string(to).unwrap(), "[connected]\nOne line.\n");
    }

    #[gpui::test]
    fn the_keyboard_reaches_every_control_of_the_section(cx: &mut TestAppContext) {
        let folder = TempDir::new().unwrap();
        saved_log(folder.path(), "debug", 1_000);
        saved_log(folder.path(), "chat", 2_000);
        let (shell, cx) = open_on(&folder, cx);
        let sheet = shell.read_with(cx, |shell, _| shell.settings_sheet());
        let (walk, controls) = sheet.read_with(cx, |sheet, _| {
            let controls = sheet.stops.in_use().to_vec();
            (sheet.tabs.len() + 1 + controls.len(), controls)
        });
        // Two switches, then Clear, Pin and Export for each kind.
        assert_eq!(controls.len(), 8);

        let mut reached = 0;
        for _ in 0..walk {
            cx.simulate_keystrokes("tab");
            cx.run_until_parked();
            let on_a_control =
                cx.update(|window, _| controls.iter().any(|stop| stop.is_focused(window)));
            reached += usize::from(on_a_control);
        }
        assert_eq!(reached, controls.len());
    }

    #[test]
    fn sizes_read_at_a_glance() {
        assert_eq!(size_text(0), "0 KB");
        assert_eq!(size_text(1), "1 KB");
        assert_eq!(size_text(2_500_000), "2.4 MB");
    }

    #[test]
    fn the_question_says_what_goes_and_what_stays() {
        let log = |pinned, live| LogFile {
            kind: Kind::Chat,
            path: PathBuf::new(),
            server: HOST.to_owned(),
            started: Timestamp(0),
            bytes: 0,
            pinned,
            live,
        };
        let (plain, pinned, live) = (log(false, false), log(true, false), log(false, true));
        assert_eq!(
            clearing_question(Kind::Chat, &[&plain, &pinned, &live]),
            "Delete 2 chat logs? This can't be undone. 1 pinned go too. \
             The log of the game you're in stays."
        );
    }
}
