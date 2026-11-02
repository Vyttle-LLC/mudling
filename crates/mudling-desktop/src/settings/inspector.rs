//! The plugin inspector: a page of Plugins that answers which plugin claimed
//! a line, and how long it took (PRD §11).
//!
//! The session records only while this page is open, and forgets it all when
//! the page closes, so play pays nothing for it.

use super::plugins::micros;
use super::{Page, Pane, Section, SettingsSheet, SheetEvent, muted_line, text_button};
use crate::assets::UI_FONT;
use crate::theme::{CHAT_FONTS, Theme};
use gpui::{Context, Pixels, Role, SharedString, Window, div, prelude::*, px};
use mudling_core::plugin::inspect::{Claim, InspectedLine};
use mudling_core::store::{INSPECTED_LINES, InspectorChange, InspectorState, StoreState};
use std::ops::Range;

const NOTHING_CLAIMED: &str = "Nothing claimed this line";
const NOTHING_YET: &str = "Lines the game sends will show here.";
/// What a claim says when a plugin only colored the line.
const COLORED: &str = "coloring";
const ROW_HEIGHT: Pixels = px(52.);
/// More than the tallest sheet shows, past what a page key moves.
const ROWS_DRAWN: usize = 24;
const ROWS_BEYOND_THE_VIEW: usize = 6;

/// Which of `lines` rows to draw with the first `scrolled` above the view.
/// Lines arrive on every frame, so the rest only hold the list's height.
fn rows_in_view(scrolled: usize, lines: usize) -> Range<usize> {
    let start = scrolled.saturating_sub(ROWS_BEYOND_THE_VIEW).min(lines);
    start..(start + ROWS_DRAWN).min(lines)
}

/// "Avatar · exits · Your overrides · coloring", or nothing if none claimed it.
fn claimed_by(claims: &[Claim], name_of: impl Fn(&str) -> SharedString) -> Option<String> {
    let said = claims.iter().map(|claim| {
        let name = name_of(claim.plugin.as_str());
        match claim.events.as_slice() {
            [] => format!("{name} · {COLORED}"),
            events => format!("{name} · {}", events.join(", ")),
        }
    });
    let said = said.collect::<Vec<_>>().join(" · ");
    (!said.is_empty()).then_some(said)
}

impl SettingsSheet {
    pub fn open_inspector(&mut self, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        cx.emit(SheetEvent::Inspect(true));
        self.go_to_page(Page::Inspector, window, cx);
    }

    /// New lines from the session, for as long as the page is open.
    pub fn show_inspected(&mut self, change: &InspectorChange, cx: &mut Context<SettingsSheet>) {
        if self.inspector.apply(change) {
            cx.notify();
        }
    }

    fn clear_inspector(&mut self, cx: &mut Context<SettingsSheet>) {
        self.inspector = InspectorState::default();
        cx.emit(SheetEvent::ClearInspector);
        cx.notify();
    }

    fn inspected_row(
        &self,
        number: usize,
        inspected: &InspectedLine,
        theme: &Theme,
    ) -> impl IntoElement + use<> {
        let text = SharedString::from(inspected.line.plain().to_owned());
        let claimed = claimed_by(&inspected.claims, |id| self.display_name(id));
        let took = micros(inspected.took.as_secs_f64() * 1e6);
        let who = claimed
            .clone()
            .unwrap_or_else(|| NOTHING_CLAIMED.to_owned());
        div()
            .id(("inspected", number))
            .debug_selector(move || format!("inspected-{number}"))
            .role(Role::Label)
            .aria_value(format!("{text}, {who}, {took}"))
            .h(ROW_HEIGHT)
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .when(number > 0, |row| row.border_t_1())
            .border_color(theme.divider)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .font_family(CHAT_FONTS[0])
                            .text_size(px(13.))
                            .text_color(theme.text)
                            .child(text),
                    )
                    .child(
                        div()
                            .truncate()
                            .font_family(UI_FONT)
                            .text_size(px(12.))
                            .text_color(if claimed.is_some() {
                                theme.accent
                            } else {
                                theme.text_muted
                            })
                            .child(who),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .font_family(CHAT_FONTS[0])
                    .text_size(px(12.))
                    .text_color(theme.text_muted)
                    .child(took),
            )
    }

    pub(super) fn fill_inspector(
        &self,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) {
        let back = self.back_button(Section::Plugins, pane, theme, cx);
        pane.leading = Some(back.into_any_element());
        let clear = text_button("inspector-clear", "Clear", &pane.stop(cx), theme)
            .on_click(cx.listener(|sheet, _, _, cx| sheet.clear_inspector(cx)));
        pane.trailing = Some(clear.into_any_element());
        pane.row(muted_line(
            format!(
                "Recording the newest {INSPECTED_LINES} lines while this page is open. Newest \
                 first."
            ),
            theme,
        ));
        if self.inspector.lines.is_empty() {
            pane.row(muted_line(NOTHING_YET, theme));
            return;
        }
        let lines = self.inspector.lines.len();
        let scrolled = (-self.scroll.offset().y / ROW_HEIGHT).max(0.) as usize;
        let shown = rows_in_view(scrolled, lines);
        let height = |rows: usize| ROW_HEIGHT * rows as f32;
        let newest_first = self.inspector.lines.iter().rev().enumerate();
        let rows = newest_first
            .skip(shown.start)
            .take(shown.len())
            .map(|(number, line)| self.inspected_row(number, line, theme));
        pane.row(
            div()
                .id("inspected-lines")
                .role(Role::List)
                .aria_label("Lines and what claimed them")
                .mt_1()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .child(div().h(height(shown.start)))
                .children(rows)
                .child(div().h(height(lines - shown.end))),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mudling_core::plugin::PluginId;
    use std::sync::Arc;

    fn id(name: &str) -> PluginId {
        PluginId::try_from(name.to_owned()).unwrap()
    }

    #[test]
    fn only_the_rows_near_the_view_are_drawn() {
        assert_eq!(rows_in_view(0, 3), 0..3);
        assert_eq!(rows_in_view(0, 200), 0..ROWS_DRAWN);
        assert_eq!(rows_in_view(100, 200), 94..94 + ROWS_DRAWN);
        assert_eq!(rows_in_view(199, 200), 193..200);
        assert_eq!(
            rows_in_view(300, 2),
            2..2,
            "scrolled past a list that shrank"
        );
    }

    #[test]
    fn a_line_says_who_claimed_it_and_how() {
        let claims = [
            Claim {
                plugin: id("avatar"),
                events: vec![Arc::from("room"), Arc::from("exits")],
            },
            Claim {
                plugin: id("mine"),
                events: Vec::new(),
            },
        ];
        let name = |id: &str| SharedString::from(id.to_uppercase());
        assert_eq!(
            claimed_by(&claims, name).unwrap(),
            "AVATAR · room, exits · MINE · coloring"
        );
        assert_eq!(claimed_by(&[], name), None);
    }
}
