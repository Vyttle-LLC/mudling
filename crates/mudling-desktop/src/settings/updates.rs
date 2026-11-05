//! Updates in Settings: the card at the top of every section when a release
//! is ready (or when one didn't go in), Data & sync's rows, and the question
//! Restart to update asks while connected. The shell feeds it
//! [`UpdateStatus`], so it only draws and asks.

use super::appearance::toggled;
use super::plugins::switch;
use super::{
    MAKER, Pane, SettingsSheet, SheetEvent, control, muted_line, section_heading, text_button,
};
use crate::assets::UI_FONT;
use crate::theme::Theme;
use crate::update::UpdateStatus;
use chrono::{DateTime, TimeZone};
use gpui::{
    AnyElement, AnyView, App, Context, Div, FocusHandle, FontWeight, Render, Role, SharedString,
    Stateful, Window, div, prelude::*, px, transparent_black,
};
use mudling_core::persist::Timestamp;
use std::fmt::Display;

pub const RESTART_TITLE: &str = "Restart to update?";
const CHECKS_WHEN: &str = "Daily and at launch, from GitHub Releases.";
const CHECK_TIP: &str = "Check for updates";

/// What the player asked of updates, in Settings or on the empty state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateAsk {
    CheckNow,
    CheckingOn(bool),
    /// Hide the card until the next release.
    Later,
    /// Restart into the update, asking first while connected.
    Restart,
    /// The player answered Restart.
    RestartNow,
    DismissFailure,
}

/// What the version line says of a check the player asked for there.
#[derive(Clone, Debug, PartialEq, Eq)]
enum CheckResult {
    Checking,
    UpToDate,
    /// A release was staged: its version.
    Ready(String),
    Failed,
}

impl CheckResult {
    /// What `status` says of a check asked for since Settings opened.
    fn of(status: &UpdateStatus) -> CheckResult {
        if status.checking {
            CheckResult::Checking
        } else if let Some(ready) = &status.ready {
            CheckResult::Ready(ready.version.clone())
        } else if status.problem.is_some() || status.failure.is_some() {
            CheckResult::Failed
        } else {
            CheckResult::UpToDate
        }
    }

    fn words(&self) -> String {
        match self {
            CheckResult::Checking => "Checking…".to_owned(),
            CheckResult::UpToDate => "Up to date".to_owned(),
            CheckResult::Ready(version) => format!("{version} is ready"),
            CheckResult::Failed => "Couldn't check".to_owned(),
        }
    }
}

/// A button that reads as part of a line of text. The caller sets its hover.
fn link(
    id: &'static str,
    label: impl Into<SharedString>,
    stop: &FocusHandle,
    theme: &Theme,
) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(|| id.to_owned())
        .role(Role::Button)
        .aria_label(label)
        .track_focus(stop)
        .flex_none()
        .ml(px(-4.))
        .px_1()
        .rounded_md()
        .border_1()
        .border_color(transparent_black())
        .cursor_pointer()
        .focus_visible(|style| style.border_color(theme.accent))
}

/// A tooltip with one line of words.
struct Tip(SharedString);

fn tip(words: impl Into<SharedString>, cx: &mut App) -> AnyView {
    let words = words.into();
    cx.new(|_| Tip(words)).into()
}

impl Render for Tip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Tip>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        div()
            .max_w(px(320.))
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_raised)
            .font_family(UI_FONT)
            .text_size(px(12.))
            .text_color(theme.text)
            .child(self.0.clone())
    }
}

/// When the last check ran, as the player reads it: "Today at 9:12".
pub fn checked_words<Tz: TimeZone>(at: Option<Timestamp>, now: &DateTime<Tz>) -> String
where
    Tz::Offset: Display,
{
    let Some(at) = at.and_then(|at| now.timezone().timestamp_millis_opt(at.0).single()) else {
        return "Not yet".to_owned();
    };
    let time = at.format("%-H:%M");
    match (now.date_naive() - at.date_naive()).num_days() {
        0 => format!("Today at {time}"),
        1 => format!("Yesterday at {time}"),
        _ => format!("{} at {time}", at.format("%b %-d")),
    }
}

/// A raised box of rows, as the design draws a group of settings.
fn rows_box(theme: &Theme) -> Div {
    div()
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .bg(theme.surface_raised)
        .font_family(UI_FONT)
}

/// One row of [`rows_box`]: what it is, a line under it, and its control.
fn setting_row(
    id: &'static str,
    title: &'static str,
    detail: impl Into<SharedString>,
    control: Option<AnyElement>,
    theme: &Theme,
) -> Stateful<Div> {
    let detail = detail.into();
    div()
        .id(id)
        .px_4()
        .py_3()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .id(title)
                        .role(Role::Label)
                        .aria_value(title)
                        .text_size(px(14.))
                        .text_color(theme.text)
                        .child(title),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("{id}-detail")))
                        .debug_selector(move || format!("{id}-detail"))
                        .role(Role::Label)
                        .aria_value(detail.clone())
                        .text_size(px(13.))
                        .text_color(theme.text_muted)
                        .child(detail),
                ),
        )
        .children(control)
}

/// The card's frame, edged in `edge`: Moss for a release that's ready,
/// because nothing is wrong, and the plain border for one that didn't go in.
fn card(id: &'static str, title: SharedString, edge: gpui::Hsla, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .role(Role::Group)
        .aria_label(title.clone())
        .mb_4()
        .px_4()
        .py_3()
        .rounded_xl()
        .border_1()
        .border_color(edge)
        .bg(theme.surface_raised)
        .font_family(UI_FONT)
        .child(
            div()
                .id(SharedString::from(format!("{id}-title")))
                .role(Role::Heading)
                .aria_label(title.clone())
                .aria_level(3)
                .text_size(px(15.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(title),
        )
}

fn actions() -> Div {
    div().pt_3().flex().justify_end().gap_2()
}

impl SettingsSheet {
    pub fn show_update(&mut self, status: UpdateStatus, cx: &mut Context<SettingsSheet>) {
        if self.update != status {
            self.update = status;
            cx.notify();
        }
    }

    /// What the version line says of the check asked for there, if one was
    /// and this copy can update.
    fn about_result(&self) -> Option<CheckResult> {
        (self.checked_here && self.update.cant.is_none()).then(|| CheckResult::of(&self.update))
    }

    /// Shows the version line as it reads for `status`, after a click on it if
    /// `clicked`, for `--ax-check`.
    pub fn show_sample_version_check(
        &mut self,
        status: UpdateStatus,
        clicked: bool,
        cx: &mut Context<SettingsSheet>,
    ) {
        self.checked_here = clicked;
        self.show_update(status, cx);
        cx.notify();
    }

    /// The version line's check, a stop unless this copy can't update.
    pub(super) fn about_stop(&self) -> Option<&FocusHandle> {
        self.update.cant.is_none().then_some(&self.about)
    }

    /// Whether the version line offers Restart to update.
    pub(super) fn ready_to_restart(&self) -> bool {
        matches!(self.about_result(), Some(CheckResult::Ready(_)))
    }

    /// The foot of the sidebar: "MUDling 0.1.0 · MIT" and who makes it. The
    /// version checks for updates, and the line then shows how that went.
    pub(super) fn render_about(
        &self,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) -> impl IntoElement + use<> {
        let version = format!("MUDling {}", self.update.current);
        let line = div()
            .id("settings-about")
            .debug_selector(|| "settings-about".to_owned())
            .px_3()
            .pb_2()
            .text_size(px(13.))
            .text_color(theme.text_muted);
        if let Some(why) = self.update.cant {
            let said = format!("{version} · {} · {MAKER}", env!("CARGO_PKG_LICENSE"));
            return line
                .role(Role::Label)
                .aria_value(said)
                .tooltip(move |_, cx| tip(why, cx))
                .child(format!("{version} · {}", env!("CARGO_PKG_LICENSE")))
                .child(MAKER)
                .into_any_element();
        }
        let (hover, text, ok, error) = (theme.hover, theme.text, theme.accent, theme.error);
        let check = link(
            "check-for-updates-version",
            format!("{version}, check for updates"),
            &self.about,
            theme,
        )
        .hover(move |style| style.bg(hover).text_color(text).underline())
        .tooltip(|_, cx| tip(CHECK_TIP, cx))
        .on_click(cx.listener(|sheet, _, _, cx| {
            sheet.checked_here = true;
            cx.emit(SheetEvent::Update(UpdateAsk::CheckNow));
            cx.notify();
        }))
        .child(version);
        let result = self.about_result();
        let (words, color) = match &result {
            None => (env!("CARGO_PKG_LICENSE").to_owned(), theme.text_muted),
            Some(result @ CheckResult::Failed) => (result.words(), error),
            Some(result @ (CheckResult::UpToDate | CheckResult::Ready(_))) => (result.words(), ok),
            Some(result) => (result.words(), theme.text_muted),
        };
        // One node for the line's end, so a screen reader announces its changes.
        let result_line = div()
            .id("settings-check-result")
            .role(Role::Status)
            .aria_label(words.clone())
            .pl_1()
            .text_color(color)
            .child(format!("· {words}"));
        let restart = self.ready_to_restart().then(|| {
            link(
                "settings-check-restart",
                "Restart to update",
                &self.about_restart,
                theme,
            )
            .text_color(ok)
            .underline()
            .on_click(cx.listener(|_, _, _, cx| {
                cx.emit(SheetEvent::Update(UpdateAsk::Restart));
            }))
            .child("Restart to update")
        });
        line.child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .child(check)
                .child(result_line),
        )
        .children(restart.map(|restart| div().flex().child(restart)))
        .child(
            div()
                .id("settings-maker")
                .role(Role::Label)
                .aria_value(MAKER)
                .child(MAKER),
        )
        .into_any_element()
    }

    /// Asks whether to restart while connected, over everything else.
    /// `question` says what restarting drops.
    pub fn ask_restart(
        &mut self,
        question: String,
        window: &mut Window,
        cx: &mut Context<SettingsSheet>,
    ) {
        self.asking_restart = Some(question.into());
        // Cancel takes the pane's first stop when the question draws.
        if self.stops.handles.is_empty() {
            self.stops.handles.push(cx.focus_handle());
        }
        window.focus(&self.stops.handles[0], cx);
        cx.notify();
    }

    pub fn answer_restart(
        &mut self,
        restart: bool,
        window: &mut Window,
        cx: &mut Context<SettingsSheet>,
    ) {
        if self.asking_restart.take().is_none() {
            return;
        }
        window.focus(&self.tabs[self.section as usize], cx);
        if restart {
            cx.emit(SheetEvent::Update(UpdateAsk::RestartNow));
        }
        cx.notify();
    }

    /// The card at the top of every section: the release that's ready, or the
    /// one that didn't go in, said once.
    pub(super) fn fill_update_card(
        &self,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) {
        let status = &self.update;
        if let Some((title, reason)) = status.failure.clone() {
            let said = format!("{reason} You're still on {}, as before.", status.current);
            let ok = text_button("update-failure-ok", "OK", &pane.stop(cx), theme).on_click(
                cx.listener(|_, _, _, cx| cx.emit(SheetEvent::Update(UpdateAsk::DismissFailure))),
            );
            pane.row(
                card("update-failure", title.into(), theme.border, theme)
                    .child(
                        muted_line(said, theme)
                            .role(Role::Alert)
                            .pb_0()
                            .text_size(px(13.)),
                    )
                    .child(actions().child(ok)),
            );
            return;
        }
        let Some(ready) = status.card().cloned() else {
            return;
        };
        let title = format!("MUDling {} is ready", ready.version);
        let page = ready.page.clone();
        let notes = text_button("update-notes", "Release notes", &pane.stop(cx), theme)
            .on_click(move |_, _, cx| cx.open_url(&page));
        let later = text_button("update-later", "Later", &pane.stop(cx), theme)
            .on_click(cx.listener(|_, _, _, cx| cx.emit(SheetEvent::Update(UpdateAsk::Later))));
        let restart = text_button("update-restart", "Restart to update", &pane.stop(cx), theme)
            .border_color(theme.accent)
            .text_color(theme.accent)
            .on_click(cx.listener(|_, _, _, cx| cx.emit(SheetEvent::Update(UpdateAsk::Restart))));
        let summary = ready.summary.iter().map(|line| {
            div()
                .id(SharedString::from(line.clone()))
                .role(Role::ListItem)
                .aria_label(line.clone())
                .flex()
                .gap_2()
                .child("•")
                .child(div().flex_1().min_w_0().child(line.clone()))
        });
        let when = format!("It goes in when you quit. You're on {}.", status.current);
        pane.row(
            card("update-card", title.into(), theme.accent, theme)
                .child(muted_line(when, theme).pb_0().text_size(px(13.)))
                .when(!ready.summary.is_empty(), |card| {
                    card.child(
                        div()
                            .id("update-summary")
                            .role(Role::List)
                            .aria_label("What's new")
                            .pt_2()
                            .text_size(px(13.))
                            .text_color(theme.text_muted)
                            .children(summary),
                    )
                })
                .child(actions().child(notes).child(later).child(restart)),
        );
    }

    /// Data & sync's rows: the switch, when the last check ran and Check now.
    /// A copy that can't update says why instead.
    pub(super) fn fill_update_rows(
        &self,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) {
        pane.row(section_heading("Updates", theme));
        let status = &self.update;
        let version = format!("Version {}.", status.current);
        if let Some(why) = status.cant {
            let row = setting_row(
                "check-for-updates",
                "Updates",
                format!("{why} {version}"),
                None,
                theme,
            );
            pane.row(rows_box(theme).child(row));
            return;
        }
        let on = status.check_on;
        let flip = control(
            "check-for-updates-switch",
            "Check for updates",
            &pane.stop(cx),
            theme,
        )
        .role(Role::Switch)
        .aria_toggled(toggled(on))
        .child(switch(on, theme))
        .on_click(cx.listener(move |_, _, _, cx| {
            cx.emit(SheetEvent::Update(UpdateAsk::CheckingOn(!on)));
        }));
        let check_now = text_button("check-now", "Check now", &pane.stop(cx), theme)
            .on_click(cx.listener(|_, _, _, cx| cx.emit(SheetEvent::Update(UpdateAsk::CheckNow))));
        let when = match status.checking {
            true => "Checking…".to_owned(),
            false => checked_words(status.last_checked, &chrono::Local::now()),
        };
        pane.row(
            rows_box(theme)
                .child(
                    setting_row(
                        "check-for-updates",
                        "Check for updates",
                        format!("{CHECKS_WHEN} {version}"),
                        Some(flip.into_any_element()),
                        theme,
                    )
                    .border_b_1()
                    .border_color(theme.border),
                )
                .child(setting_row(
                    "last-checked",
                    "Last checked",
                    when,
                    Some(check_now.into_any_element()),
                    theme,
                )),
        );
        if let Some(problem) = status.problem.clone() {
            pane.row(
                muted_line(problem, theme)
                    .debug_selector(|| "update-problem".to_owned())
                    .role(Role::Alert)
                    .pt_2()
                    .text_color(theme.error),
            );
        }
    }

    /// The question, alone over the window: the sheet steps aside for it.
    pub(super) fn render_restart_question(
        &mut self,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) -> Option<impl IntoElement + use<>> {
        let question = self.asking_restart.clone()?;
        let mut stops = std::mem::take(&mut self.stops);
        stops.start_frame();
        let mut pane = Pane {
            rows: Vec::new(),
            stops,
            leading: None,
            trailing: None,
        };
        let cancel = text_button("restart-cancel", "Cancel", &pane.stop(cx), theme)
            .on_click(cx.listener(|sheet, _, window, cx| sheet.answer_restart(false, window, cx)));
        let restart = text_button("restart-now", "Restart", &pane.stop(cx), theme)
            .border_color(theme.accent)
            .text_color(theme.accent)
            .on_click(cx.listener(|sheet, _, window, cx| sheet.answer_restart(true, window, cx)));
        self.stops = pane.stops;
        Some(
            div()
                .w(px(420.))
                .p_5()
                .rounded_xl()
                .border_1()
                .border_color(theme.border)
                .bg(theme.surface)
                .shadow(theme.floating_shadow())
                .child(
                    div()
                        .id("restart-title")
                        .role(Role::Heading)
                        .aria_label(RESTART_TITLE)
                        .aria_level(2)
                        .pb_2()
                        .text_size(px(16.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child(RESTART_TITLE),
                )
                .child(muted_line(question, theme).pb_0())
                .child(actions().pt_4().child(cancel).child(restart)),
        )
    }

    /// Whether the restart question has the keyboard.
    pub fn asking_to_restart(&self) -> bool {
        self.asking_restart.is_some()
    }

    #[cfg(test)]
    pub fn restart_question(&self) -> Option<&str> {
        self.asking_restart.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::{Shell, tests::window_with_keys};
    use crate::update::{Checked, Eligibility, Staged, UpdateChoices, Updates};
    use chrono::Utc;
    use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn the_last_check_reads_as_a_day_and_a_time() {
        let now = Utc.with_ymd_and_hms(2026, 11, 2, 15, 30, 0).unwrap();
        let at = |day, hour, minute| {
            let then = Utc
                .with_ymd_and_hms(2026, 11, day, hour, minute, 0)
                .unwrap();
            Some(Timestamp(then.timestamp_millis()))
        };
        assert_eq!(checked_words(at(2, 9, 12), &now), "Today at 9:12");
        assert_eq!(checked_words(at(1, 23, 5), &now), "Yesterday at 23:05");
        assert_eq!(checked_words(Some(Timestamp(0)), &now), "Jan 1 at 0:00");
        assert_eq!(checked_words(None, &now), "Not yet");
    }

    fn can_update() -> UpdateStatus {
        UpdateStatus {
            cant: None,
            ..UpdateStatus::default()
        }
    }

    /// The status after a check staged `version`, and after Later if `put_off`.
    fn staged(version: &str, put_off: bool) -> UpdateStatus {
        let sample = Staged::sample(version, &[]);
        let mut updates = Updates::new(
            Eligibility::Updates(sample.installed().clone()),
            UpdateChoices::default(),
            None,
        );
        updates.begin_check(true);
        updates.finish_check(Ok(Checked::Ready(sample)), Timestamp(0));
        if put_off {
            updates.later();
        }
        updates.status()
    }

    #[test]
    fn a_check_reads_as_the_status_says() {
        let words = |status: UpdateStatus| CheckResult::of(&status).words();
        let status = |change: fn(&mut UpdateStatus)| {
            let mut status = can_update();
            change(&mut status);
            status
        };
        assert_eq!(words(can_update()), "Up to date");
        assert_eq!(words(status(|s| s.checking = true)), "Checking…");
        assert_eq!(
            words(status(|s| s.problem = Some("No network.".into()))),
            "Couldn't check"
        );
        assert_eq!(
            words(status(
                |s| s.failure = Some(("Didn't go in".into(), "Why.".into()))
            )),
            "Couldn't check"
        );
        assert_eq!(words(staged("0.1.1", false)), "0.1.1 is ready");
        // A release put off with Later is still ready to restart into.
        assert_eq!(words(staged("0.1.1", true)), "0.1.1 is ready");
    }

    fn asks(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Rc<RefCell<Vec<UpdateAsk>>> {
        let asked = Rc::new(RefCell::new(Vec::new()));
        let kept = Rc::clone(&asked);
        let sheet = shell.read_with(cx, |shell, _| shell.settings_sheet());
        cx.update(|_, cx| {
            cx.subscribe(&sheet, move |_, event: &SheetEvent, _| {
                if let SheetEvent::Update(ask) = event {
                    kept.borrow_mut().push(*ask);
                }
            })
            .detach();
        });
        asked
    }

    fn show(shell: &Entity<Shell>, status: UpdateStatus, cx: &mut VisualTestContext) {
        let sheet = shell.read_with(cx, |shell, _| shell.settings_sheet());
        sheet.update(cx, |sheet, cx| sheet.show_update(status, cx));
        cx.run_until_parked();
    }

    fn line(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Option<CheckResult> {
        let sheet = shell.read_with(cx, |shell, _| shell.settings_sheet());
        sheet.read_with(cx, |sheet, _| sheet.about_result())
    }

    fn click(id: &'static str, cx: &mut VisualTestContext) {
        let bounds = cx.debug_bounds(id).expect("the control is drawn");
        cx.simulate_click(bounds.center(), Modifiers::none());
        cx.run_until_parked();
    }

    fn open(shell: &Entity<Shell>, cx: &mut VisualTestContext) {
        shell.update_in(cx, |shell, window, cx| shell.open_settings(window, cx));
        cx.run_until_parked();
    }

    #[gpui::test]
    fn clicking_the_version_asks_for_a_check(cx: &mut TestAppContext) {
        let (shell, cx) = window_with_keys(cx);
        let asked = asks(&shell, cx);
        open(&shell, cx);
        show(&shell, can_update(), cx);

        click("check-for-updates-version", cx);
        assert_eq!(*asked.borrow(), [UpdateAsk::CheckNow]);
    }

    #[gpui::test]
    fn the_result_waits_for_a_click_and_goes_when_settings_close(cx: &mut TestAppContext) {
        let (shell, cx) = window_with_keys(cx);
        open(&shell, cx);
        // A background check found a release; the line doesn't say so.
        show(&shell, staged("0.1.1", false), cx);
        assert_eq!(line(&shell, cx), None);

        click("check-for-updates-version", cx);
        assert_eq!(line(&shell, cx), Some(CheckResult::Ready("0.1.1".into())));
        show(&shell, can_update(), cx);
        assert_eq!(line(&shell, cx), Some(CheckResult::UpToDate));

        cx.simulate_keystrokes("escape");
        open(&shell, cx);
        assert_eq!(line(&shell, cx), None);
    }

    #[gpui::test]
    fn restart_to_update_asks_to_restart(cx: &mut TestAppContext) {
        let (shell, cx) = window_with_keys(cx);
        let asked = asks(&shell, cx);
        open(&shell, cx);
        show(&shell, can_update(), cx);
        assert!(cx.debug_bounds("settings-check-restart").is_none());

        click("check-for-updates-version", cx);
        show(&shell, staged("0.1.1", false), cx);
        click("settings-check-restart", cx);
        assert_eq!(*asked.borrow(), [UpdateAsk::CheckNow, UpdateAsk::Restart]);
    }

    #[gpui::test]
    fn a_copy_that_cant_update_ignores_the_click(cx: &mut TestAppContext) {
        let (shell, cx) = window_with_keys(cx);
        let asked = asks(&shell, cx);
        open(&shell, cx);
        assert!(cx.debug_bounds("check-for-updates-version").is_none());

        click("settings-about", cx);
        assert!(asked.borrow().is_empty());
        assert_eq!(line(&shell, cx), None);
    }
}
