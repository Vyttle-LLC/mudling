//! The plugin inspector: a page of Plugins that answers which plugin claimed
//! a line, and how long it took (PRD §11).
//!
//! The session records only while this page is open, and forgets it all when
//! the page closes, so play pays nothing for it.

use super::plugins::micros;
use super::{Page, Pane, Section, SettingsSheet, SheetEvent, muted_line, text_button};
use crate::assets::UI_FONT;
use crate::theme::{CHAT_FONTS, Theme};
use gpui::{
    ContentMask, Context, DecorationRun, Hsla, LineLayout, Pixels, Role, SharedString, TextAlign,
    TextRun, Window, canvas, div, font, point, prelude::*, px,
};
use mudling_core::plugin::inspect::{Claim, InspectedLine};
use mudling_core::store::{INSPECTED_LINES, InspectorChange};
use std::cell::OnceCell;
use std::collections::VecDeque;
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

const NOTHING_CLAIMED: &str = "Nothing claimed this line";
const NOTHING_YET: &str = "Lines the game sends will show here.";
/// What a claim says when a plugin only colored the line.
const COLORED: &str = "coloring";
const ELLIPSIS: &str = "…";
const ROW_HEIGHT: Pixels = px(52.);
const LINE_SIZE: Pixels = px(13.);
const NOTE_SIZE: Pixels = px(12.);
/// How soon the page shows lines that came after it last drew. A draw of the
/// sheet costs a frame 1 to 2 ms, and more than one frame in a hundred over
/// the 8 ms budget misses it, so this can't be much shorter.
pub(super) const REFRESH_EVERY: Duration = Duration::from_secs(1);
/// The tallest sheet shows 13 rows, one of them cut off at the top.
const ROWS_DRAWN: usize = 16;
const ROWS_BEYOND_THE_VIEW: usize = 2;

/// A recorded line. Each draw shows up to 16 lines it hasn't shown before, and
/// shaping them all in that one frame took more than the rest of the draw, so
/// the newest lines of each batch are shaped as they arrive. An older line is
/// shaped when a draw first shows it.
pub(super) struct InspectedRow {
    text: SharedString,
    who: SharedString,
    took: SharedString,
    claimed: bool,
    /// What a screen reader hears for the row.
    said: SharedString,
    shaped: OnceCell<ShapedRow>,
}

struct ShapedRow {
    text: Arc<LineLayout>,
    who: Arc<LineLayout>,
    took: Arc<LineLayout>,
}

impl InspectedRow {
    fn shaped(&self, window: &Window) -> &ShapedRow {
        self.shaped.get_or_init(|| ShapedRow {
            text: shaped(&self.text, CHAT_FONTS[0], LINE_SIZE, window),
            who: shaped(&self.who, UI_FONT, NOTE_SIZE, window),
            took: shaped(&self.took, CHAT_FONTS[0], NOTE_SIZE, window),
        })
    }
}

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

fn shaped(text: &str, family: &'static str, size: Pixels, window: &Window) -> Arc<LineLayout> {
    let run = TextRun {
        len: text.len(),
        font: font(family),
        color: Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window.text_system().layout_line(text, size, &[run], None)
}

/// gpui's default line height for text of `size`.
fn line_height(size: Pixels) -> Pixels {
    size * 1.618
}

/// How much of `layout` to show in `width`: up to the glyph that would cross it.
fn cut_short(layout: &LineLayout, width: Pixels) -> Pixels {
    let crossing = layout.index_for_x(width).unwrap_or(layout.len);
    layout.x_for_index(crossing)
}

/// Paints a line shaped beforehand, cut short with an ellipsis where it
/// doesn't fit.
fn painted(layout: &Arc<LineLayout>, family: &'static str, color: Hsla) -> impl IntoElement {
    let layout = layout.clone();
    let height = line_height(layout.font_size);
    let paint = move |bounds: gpui::Bounds<Pixels>, (), window: &mut Window, cx: &mut gpui::App| {
        let in_color = |len: usize| {
            [DecorationRun {
                len: len as u32,
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            }]
        };
        let mut shown = bounds;
        if layout.width > bounds.size.width {
            let ellipsis = shaped(ELLIPSIS, family, layout.font_size, window);
            shown.size.width = cut_short(&layout, bounds.size.width - ellipsis.width);
            let after = point(bounds.origin.x + shown.size.width, bounds.origin.y);
            let decorations = in_color(ellipsis.len);
            (ellipsis.paint(
                after,
                height,
                TextAlign::Left,
                None,
                &decorations,
                window,
                cx,
            ))
            .ok();
        }
        let decorations = in_color(layout.len);
        window.with_content_mask(Some(ContentMask { bounds: shown }), |window| {
            let origin = bounds.origin;
            (layout.paint(
                origin,
                height,
                TextAlign::Left,
                None,
                &decorations,
                window,
                cx,
            ))
            .ok();
        });
    };
    canvas(|_, _, _| {}, paint).w_full().h(height)
}

impl SettingsSheet {
    pub fn open_inspector(&mut self, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        cx.emit(SheetEvent::Inspect(true));
        self.go_to_page(Page::Inspector, window, cx);
    }

    /// New lines from the session, for as long as the page is open. The page
    /// draws at once, then no more often than `REFRESH_EVERY`: the sheet is
    /// laid out afresh each time, and lines arrive on every frame.
    pub fn show_inspected(
        &mut self,
        change: &InspectorChange,
        window: &Window,
        cx: &mut Context<SettingsSheet>,
    ) {
        if !self.record(change, window) {
            return;
        }
        if self.inspector_cooldown.is_some() {
            self.inspector_late = true;
            return;
        }
        cx.notify();
        self.inspector_cooldown = Some(cx.spawn(async move |sheet, cx| {
            loop {
                cx.background_executor().timer(REFRESH_EVERY).await;
                let drawn_late = sheet.update(cx, |sheet, cx| {
                    let late = std::mem::take(&mut sheet.inspector_late);
                    if late {
                        cx.notify();
                    } else {
                        sheet.inspector_cooldown = None;
                    }
                    late
                });
                if !matches!(drawn_late, Ok(true)) {
                    return;
                }
            }
        }));
    }

    /// Keeps the newest `INSPECTED_LINES`, and says whether that changed them.
    fn record(&mut self, change: &InspectorChange, window: &Window) -> bool {
        let cleared = change.clear && !self.inspector.is_empty();
        if change.clear {
            self.inspector.clear();
        }
        let kept = change.lines.len().saturating_sub(INSPECTED_LINES);
        let rows = change.lines[kept..].iter();
        let rows = rows.map(|line| self.inspected_row(line));
        let rows = rows.collect::<Vec<_>>();
        let unshown = rows.len().saturating_sub(ROWS_DRAWN);
        rows[unshown..]
            .iter()
            .for_each(|row| _ = row.shaped(window));
        self.inspector.extend(rows);
        let overflow = self.inspector.len().saturating_sub(INSPECTED_LINES);
        self.inspector.drain(..overflow);
        cleared || !change.lines.is_empty()
    }

    /// Drops the lines, and any draw that was waiting to show them.
    pub(super) fn forget_inspected(&mut self) {
        self.inspector = VecDeque::new();
        self.inspector_cooldown = None;
        self.inspector_late = false;
    }

    fn clear_inspector(&mut self, cx: &mut Context<SettingsSheet>) {
        self.forget_inspected();
        cx.emit(SheetEvent::ClearInspector);
        cx.notify();
    }

    fn inspected_row(&self, inspected: &InspectedLine) -> InspectedRow {
        let text = inspected.line.plain();
        let claimed = claimed_by(&inspected.claims, |id| self.display_name(id));
        let took = micros(inspected.took.as_secs_f64() * 1e6);
        let who = claimed.as_deref().unwrap_or(NOTHING_CLAIMED);
        InspectedRow {
            said: format!("{text}, {who}, {took}").into(),
            text: SharedString::from(text.to_owned()),
            who: SharedString::from(who.to_owned()),
            took: took.into(),
            claimed: claimed.is_some(),
            shaped: OnceCell::new(),
        }
    }

    fn render_inspected(
        number: usize,
        row: &InspectedRow,
        theme: &Theme,
        window: &Window,
    ) -> impl IntoElement {
        let shaped = row.shaped(window);
        div()
            .id(("inspected", number))
            .debug_selector(move || format!("inspected-{number}"))
            .role(Role::Label)
            .aria_value(row.said.clone())
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
                    .child(painted(&shaped.text, CHAT_FONTS[0], theme.text))
                    .child(painted(
                        &shaped.who,
                        UI_FONT,
                        match row.claimed {
                            true => theme.accent,
                            false => theme.text_muted,
                        },
                    )),
            )
            // Rounded up, as gpui rounds an explicit width to the nearest pixel.
            .child(div().flex_none().w(shaped.took.width.ceil()).child(painted(
                &shaped.took,
                CHAT_FONTS[0],
                theme.text_muted,
            )))
    }

    pub(super) fn fill_inspector(
        &self,
        pane: &mut Pane,
        theme: &Theme,
        window: &Window,
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
        if self.inspector.is_empty() {
            pane.row(muted_line(NOTHING_YET, theme));
            return;
        }
        let lines = self.inspector.len();
        let scrolled = (-self.scroll.offset().y / ROW_HEIGHT).max(0.) as usize;
        let shown = rows_in_view(scrolled, lines);
        let height = |rows: usize| ROW_HEIGHT * rows as f32;
        let newest_first = self.inspector.iter().rev().enumerate();
        let rows = newest_first
            .skip(shown.start)
            .take(shown.len())
            .map(|(number, row)| Self::render_inspected(number, row, theme, window));
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
    use crate::shell::tests::window_with_keys;
    use gpui::{FontId, GlyphId, ShapedGlyph, ShapedRun, TestAppContext};
    use mudling_core::plugin::PluginId;

    fn id(name: &str) -> PluginId {
        PluginId::try_from(name.to_owned()).unwrap()
    }

    #[test]
    fn only_the_rows_near_the_view_are_drawn() {
        assert_eq!(rows_in_view(0, 3), 0..3);
        assert_eq!(rows_in_view(0, 200), 0..ROWS_DRAWN);
        assert_eq!(rows_in_view(100, 200), 98..98 + ROWS_DRAWN);
        assert_eq!(rows_in_view(199, 200), 197..200);
        assert_eq!(
            rows_in_view(300, 2),
            2..2,
            "scrolled past a list that shrank"
        );
    }

    fn shaped_rows(sheet: &SettingsSheet) -> usize {
        let shaped = sheet
            .inspector
            .iter()
            .filter(|row| row.shaped.get().is_some());
        shaped.count()
    }

    #[gpui::test]
    fn a_batch_shapes_only_the_lines_a_draw_can_show(cx: &mut TestAppContext) {
        let (shell, cx) = window_with_keys(cx);
        shell.update_in(cx, |shell, window, cx| shell.open_settings(window, cx));
        let sheet = shell.read_with(cx, |shell, _| shell.settings_sheet());
        sheet.update_in(cx, |sheet, window, cx| sheet.open_inspector(window, cx));
        cx.run_until_parked();
        let line = crate::measure::sample_inspected().swap_remove(0);
        let change = InspectorChange {
            clear: false,
            lines: vec![line; INSPECTED_LINES + 50],
        };
        sheet.update_in(cx, |sheet, window, cx| {
            sheet.show_inspected(&change, window, cx);
            assert_eq!(sheet.inspector.len(), INSPECTED_LINES);
            assert_eq!(shaped_rows(sheet), ROWS_DRAWN, "before any draw");
            let newest = sheet.inspector.back().expect("the line is recorded");
            assert!(newest.shaped.get().is_some(), "the newest is shaped");
        });

        sheet.update(cx, |sheet, _| {
            sheet.scroll.set_offset(point(px(0.), -ROW_HEIGHT * 100.))
        });
        sheet.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        let shaped = sheet.read_with(cx, |sheet, _| shaped_rows(sheet));
        assert_eq!(
            shaped,
            ROWS_DRAWN * 2,
            "an older line is shaped once it shows"
        );
    }

    #[test]
    fn a_long_line_is_cut_before_the_glyph_that_crosses_the_width() {
        let glyph = |index: usize| ShapedGlyph {
            id: GlyphId(0),
            position: point(px(10. * index as f32), px(0.)),
            index,
            is_emoji: false,
        };
        let layout = LineLayout {
            font_size: px(13.),
            width: px(50.),
            runs: vec![ShapedRun {
                font_id: FontId(0),
                glyphs: (0..5).map(glyph).collect(),
            }],
            len: 5,
            ..LineLayout::default()
        };
        assert_eq!(cut_short(&layout, px(35.)), px(30.));
        assert_eq!(cut_short(&layout, px(30.)), px(30.));
        assert_eq!(cut_short(&layout, px(5.)), px(0.));
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
