//! The Data & sync section: the repo and how this device signs in to it, the
//! last sync and what changed, Sync now, View history and what's kept. The
//! shell feeds it `SyncState`, so it only draws and asks: everything that
//! touches the disk runs off the UI thread.

use super::{Pane, Section, SettingsSheet, SheetEvent, muted_line, section_heading, text_button};
use crate::assets::UI_FONT;
use crate::theme::Theme;
use gpui::{
    Context, FontWeight, PathPromptOptions, Role, SharedString, Task, Window, div, prelude::*, px,
};
use mudling_core::git::{self, SessionCommit, SignIn};
use mudling_core::persist::Timestamp;
use mudling_core::store::SyncState;
use mudling_core::sync::RepoSettings;
use std::path::{Path, PathBuf};
use std::time::Duration;

const NO_SYNC: &str = "Sync isn't running in this window, so nothing here leaves this device.";
const KEPT_HERE: &str = "Everything stays on this device. Choose a folder to keep a copy in a \
                         git repo, which can sync your other devices through a remote.";
const FIRST_SYNC: &str = "Choosing a folder exports what's here as the first commit. If the \
                          repo already holds MUDling data from another device, the two merge \
                          record by record.";
const STOP_QUESTION: &str = "Stop syncing? Everything stays on this device, and the repo stays \
                             as it is.";
const NO_HISTORY: &str = "No sessions in the repo yet.";
const NOT_SYNCED_YET: &str = "Not synced yet.";
const SYNCING: &str = "Syncing…";
/// The history lists this many sessions, the newest first.
const HISTORY_LENGTH: usize = 50;
/// PRD §10's list. Nothing here comes from a game.
const WHATS_KEPT: [(&str, &str); 3] = [
    ("Maps, characters and notes", "Synced"),
    ("Triggers, aliases, plugins and settings", "Synced"),
    ("Game logs and typing", "Never synced"),
];

/// What the section knows. `repo` is the player's choice, which sync may not
/// have managed to open.
#[derive(Default)]
pub(super) struct DataPane {
    running: bool,
    repo: Option<PathBuf>,
    sync: SyncState,
    sign_in: Option<SignIn>,
    /// `None` until the player asks, and again when the repo changes.
    history: Option<Result<Vec<SessionCommit>, String>>,
    asking_to_stop: bool,
    _reading_sign_in: Option<Task<()>>,
    _reading_history: Option<Task<()>>,
}

impl DataPane {
    /// What the sprocket and this section's tab count: a problem, and the
    /// sessions that haven't reached the remote.
    pub(super) fn problems(&self) -> usize {
        usize::from(self.sync.problem.is_some()) + self.sync.unsynced
    }
}

const AGO_STEP: Duration = Duration::from_secs(60);

/// How this device signs in, as the player reads it (PRD §10).
pub(super) fn sign_in_words(sign_in: SignIn) -> String {
    let device = if cfg!(target_os = "macos") {
        "this Mac"
    } else {
        "this computer"
    };
    match sign_in {
        SignIn::NoRemote => "No remote, so commits stay on this device.".to_owned(),
        SignIn::NotNeeded => "The remote is a folder, so no sign-in is needed.".to_owned(),
        SignIn::SshAgent => format!("Signs in with {device}'s ssh-agent."),
        SignIn::Helper(Some(helper)) => format!("Signs in with {helper}."),
        SignIn::Helper(None) => format!("Signs in with {device}'s git credential helper."),
        SignIn::Nothing => {
            format!(
                "Nothing on {device} can sign in to this remote yet. Set up ssh-agent or a git credential helper."
            )
        }
    }
}

/// The last sync, as the player reads it.
pub(super) fn last_sync_words(last_sync: Option<Timestamp>, now: Timestamp) -> String {
    last_sync.map_or(NOT_SYNCED_YET.to_owned(), |at| {
        format!("Last synced {}.", ago(at, now))
    })
}

fn ago(at: Timestamp, now: Timestamp) -> String {
    let minutes = (now.0 - at.0).max(0) / 60_000;
    let plural = |count: i64, unit: &str| match count {
        1 => format!("1 {unit} ago"),
        _ => format!("{count} {unit}s ago"),
    };
    match minutes {
        0 => "just now".to_owned(),
        1..60 => plural(minutes, "minute"),
        60..1440 => plural(minutes / 60, "hour"),
        _ => plural(minutes / 1440, "day"),
    }
}

fn not_synced_words(sessions: usize) -> String {
    match sessions {
        1 => "1 session not synced.".to_owned(),
        _ => format!("{sessions} sessions not synced."),
    }
}

fn remote_of(folder: &Path) -> String {
    RepoSettings::new(folder).remote
}

impl SettingsSheet {
    /// Sync is running here, so the section can offer it.
    pub fn show_sync(
        &mut self,
        sync: &SyncState,
        repo: Option<&Path>,
        cx: &mut Context<SettingsSheet>,
    ) {
        let data = &mut self.data;
        let moved = data.repo.as_deref() != repo;
        // A sync that landed may have brought passwords, or ones that no key
        // here opens.
        let landed = sync.last_sync.is_some() && sync.last_sync != data.sync.last_sync;
        data.running = true;
        data.repo = repo.map(Path::to_owned);
        data.sync = sync.clone();
        if moved {
            data.sign_in = None;
            data.history = None;
            data._reading_history = None;
            data.asking_to_stop = false;
            self.read_sign_in(cx);
        }
        self.look_for_vault_file(cx);
        if landed {
            self.list_vault(cx);
        }
        cx.notify();
    }

    pub(super) fn read_sign_in(&mut self, cx: &mut Context<SettingsSheet>) {
        let Some(folder) = self.data.repo.clone() else {
            return;
        };
        let read = cx.background_spawn(async move {
            git::sign_in_for(&folder, &remote_of(&folder)).unwrap_or(SignIn::NoRemote)
        });
        self.data._reading_sign_in = Some(cx.spawn(async move |sheet, cx| {
            let sign_in = read.await;
            let _ = sheet.update(cx, |sheet, cx| {
                sheet.data.sign_in = Some(sign_in);
                cx.notify();
            });
        }));
    }

    fn toggle_history(&mut self, cx: &mut Context<SettingsSheet>) {
        if self.data.history.take().is_some() {
            cx.notify();
            return;
        }
        let Some(folder) = self.data.repo.clone() else {
            return;
        };
        let read = cx.background_spawn(async move {
            git::session_history(&folder, HISTORY_LENGTH).map_err(|error| error.to_string())
        });
        self.data._reading_history = Some(cx.spawn(async move |sheet, cx| {
            let history = read.await;
            let _ = sheet.update(cx, |sheet, cx| {
                sheet.data.history = Some(history);
                cx.notify();
            });
        }));
    }

    /// The OS's own folder chooser, which the keyboard and a screen reader
    /// can already use.
    fn choose_repo(&mut self, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Use this folder".into()),
        });
        cx.spawn_in(window, async move |sheet, cx| {
            if let Ok(Ok(Some(folders))) = chosen.await
                && let Some(folder) = folders.into_iter().next()
            {
                let _ = sheet.update(cx, |_, cx| cx.emit(SheetEvent::SetRepo(folder)));
            }
        })
        .detach();
    }

    /// Fills the section with a made-up repo, on no sync: for measuring and
    /// looking at it.
    pub fn show_sample_data(&mut self, cx: &mut Context<SettingsSheet>) {
        self.data = DataPane {
            running: true,
            repo: Some("/Users/sample/mudling-data".into()),
            sync: SyncState {
                last_sync: Some(Timestamp(Timestamp::now().0 - 12 * 60_000)),
                changed: Some("Sample game · Sample · 2h10: 84 rooms, 3 skills".to_owned()),
                unsynced: 1,
                ..SyncState::default()
            },
            sign_in: Some(SignIn::SshAgent),
            history: Some(Ok(vec![SessionCommit {
                message: "Sample game · Sample · 2h10: 84 rooms, 3 skills".to_owned(),
                time: Timestamp::now().0 / 1000 - 12 * 60,
            }])),
            ..DataPane::default()
        };
        cx.notify();
    }

    /// Draws the sheet again every minute, the finest step "ago" shows. Lines
    /// arriving no longer do.
    pub(super) fn tick_ago(&self, cx: &mut Context<SettingsSheet>) -> Task<()> {
        cx.spawn(async move |sheet, cx| {
            loop {
                cx.background_executor().timer(AGO_STEP).await;
                if sheet.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        })
    }

    pub(super) fn fill_data(
        &mut self,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) {
        if !self.data.running {
            pane.row(muted_line(NO_SYNC, theme));
            return;
        }
        if let Some(problem) = self.data.sync.problem.clone() {
            pane.row(
                muted_line(problem, theme)
                    .debug_selector(|| "sync-problem".to_owned())
                    .role(Role::Alert)
                    .text_color(theme.error),
            );
        }
        pane.row(section_heading("Repo", theme));
        match self.data.repo.clone() {
            Some(folder) => self.fill_repo(&folder, pane, theme, cx),
            None => {
                pane.row(muted_line(KEPT_HERE, theme));
                pane.row(muted_line(FIRST_SYNC, theme));
                let stop = pane.stop(cx);
                let choose = text_button("choose-repo", "Choose a folder", &stop, theme)
                    .on_click(cx.listener(|sheet, _, window, cx| sheet.choose_repo(window, cx)));
                pane.row(div().pb_2().child(choose));
            }
        }
        pane.row(section_heading("What's kept", theme));
        for (what, how) in WHATS_KEPT {
            pane.row(
                div()
                    .id(what)
                    .role(Role::Label)
                    .aria_value(format!("{what}: {how}"))
                    .py_1()
                    .flex()
                    .justify_between()
                    .font_family(UI_FONT)
                    .text_size(px(14.))
                    .child(div().text_color(theme.text).child(what))
                    .child(div().text_color(theme.text_muted).child(how)),
            );
        }
    }

    fn fill_repo(
        &mut self,
        folder: &Path,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) {
        let path: SharedString = folder.display().to_string().into();
        pane.row(
            div()
                .id("sync-repo")
                .role(Role::Label)
                .aria_value(path.clone())
                .pb_1()
                .font_family(UI_FONT)
                .font_weight(FontWeight::MEDIUM)
                .text_size(px(15.))
                .text_color(theme.text)
                .child(path),
        );
        if let Some(sign_in) = self.data.sign_in {
            pane.row(muted_line(sign_in_words(sign_in), theme));
        }
        pane.row(section_heading("Last sync", theme));
        let sync = &self.data.sync;
        let when = match sync.syncing {
            true => SYNCING.to_owned(),
            false => last_sync_words(sync.last_sync, Timestamp::now()),
        };
        pane.row(muted_line(when, theme).debug_selector(|| "sync-when".to_owned()));
        if let Some(changed) = sync.changed.clone() {
            pane.row(muted_line(changed, theme));
        }
        if sync.unsynced > 0 {
            pane.row(muted_line(not_synced_words(sync.unsynced), theme));
        }
        let history_label = match self.data.history {
            Some(_) => "Hide history",
            None => "View history",
        };
        let sync_now = text_button("sync-now", "Sync now", &pane.stop(cx), theme)
            .on_click(cx.listener(|_, _, _, cx| cx.emit(SheetEvent::SyncNow)));
        let history = text_button("view-history", history_label, &pane.stop(cx), theme)
            .on_click(cx.listener(|sheet, _, _, cx| sheet.toggle_history(cx)));
        let stop = text_button("stop-sync", "Stop syncing", &pane.stop(cx), theme).on_click(
            cx.listener(|sheet, _, _, cx| {
                sheet.data.asking_to_stop = true;
                cx.notify();
            }),
        );
        pane.row(
            div()
                .pb_2()
                .flex()
                .gap_2()
                .child(sync_now)
                .child(history)
                .child(stop),
        );
        if self.data.asking_to_stop {
            self.fill_stop_question(pane, theme, cx);
        }
        self.fill_history(pane, theme);
    }

    /// The question's first stop is Keep syncing, so Tab reaches the choice
    /// that changes nothing first.
    fn fill_stop_question(&self, pane: &mut Pane, theme: &Theme, cx: &mut Context<SettingsSheet>) {
        pane.row(
            muted_line(STOP_QUESTION, theme)
                .role(Role::Alert)
                .text_color(theme.text),
        );
        let keep = text_button("keep-sync", "Keep syncing", &pane.stop(cx), theme).on_click(
            cx.listener(|sheet, _, window, cx| {
                sheet.data.asking_to_stop = false;
                // This stop goes with the question.
                window.focus(&sheet.tabs[Section::Data as usize], cx);
                cx.notify();
            }),
        );
        let stop = text_button("confirm-stop-sync", "Stop syncing", &pane.stop(cx), theme)
            .on_click(cx.listener(|sheet, _, window, cx| {
                sheet.data.asking_to_stop = false;
                // This stop goes with the question.
                window.focus(&sheet.tabs[Section::Data as usize], cx);
                cx.emit(SheetEvent::ClearRepo);
                cx.notify();
            }));
        pane.row(div().pb_2().flex().gap_2().child(keep).child(stop));
    }

    fn fill_history(&self, pane: &mut Pane, theme: &Theme) {
        let Some(history) = &self.data.history else {
            return;
        };
        pane.row(section_heading("History", theme));
        match history {
            Ok(sessions) if sessions.is_empty() => pane.row(muted_line(NO_HISTORY, theme)),
            Ok(sessions) => {
                let now = Timestamp::now();
                for session in sessions {
                    let at = Timestamp(session.time.saturating_mul(1000));
                    let line = format!("{} · {}", ago(at, now), session.message);
                    pane.row(muted_line(line, theme));
                }
            }
            Err(problem) => pane.row(
                muted_line(format!("Couldn't read the history: {problem}."), theme)
                    .role(Role::Alert)
                    .text_color(theme.error),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signing_in_reads_as_the_prd_words_it() {
        assert!(sign_in_words(SignIn::SshAgent).ends_with("'s ssh-agent."));
        assert_eq!(
            sign_in_words(SignIn::Helper(Some("the GitHub CLI"))),
            "Signs in with the GitHub CLI."
        );
    }

    #[test]
    fn time_reads_in_the_largest_unit_that_fits() {
        const MINUTE: i64 = 60_000;
        let now = Timestamp(100 * 1440 * MINUTE);
        let before = |minutes: i64| Timestamp(now.0 - minutes * MINUTE);
        assert_eq!(ago(before(0), now), "just now");
        assert_eq!(ago(before(1), now), "1 minute ago");
        assert_eq!(ago(before(59), now), "59 minutes ago");
        assert_eq!(ago(before(120), now), "2 hours ago");
        assert_eq!(ago(before(2880), now), "2 days ago");
        assert_eq!(
            ago(Timestamp(now.0 + MINUTE), now),
            "just now",
            "clock skew"
        );
    }

    #[test]
    fn no_sync_yet_is_said_plainly() {
        assert_eq!(last_sync_words(None, Timestamp(0)), NOT_SYNCED_YET);
    }
}
