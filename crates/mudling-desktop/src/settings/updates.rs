//! Updates in Settings: the card at the top of every section when a release
//! is ready (or when one didn't go in), Data & sync's rows, and the question
//! Restart to update asks while connected. The shell feeds it
//! [`UpdateStatus`], so it only draws and asks.

use super::appearance::toggled;
use super::plugins::switch;
use super::{Pane, SettingsSheet, SheetEvent, control, muted_line, section_heading, text_button};
use crate::assets::UI_FONT;
use crate::theme::Theme;
use crate::update::UpdateStatus;
use chrono::{DateTime, TimeZone};
use gpui::{
    AnyElement, Context, Div, FontWeight, Role, SharedString, Stateful, Window, div, prelude::*, px,
};
use mudling_core::persist::Timestamp;
use std::fmt::Display;

pub const RESTART_TITLE: &str = "Restart to update?";
const CHECKS_WHEN: &str = "Daily and at launch, from GitHub Releases.";

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
    use chrono::Utc;

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
}
