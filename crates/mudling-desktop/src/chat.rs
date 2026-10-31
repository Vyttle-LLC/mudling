//! The chat: a virtualized view onto the shell's copy of the terminal store.
//!
//! Servers wrap their own text to 80 columns, so the column keeps that much
//! room and most lines take one row. A longer line wraps rather than lose its
//! text, as does any line in a window too narrow for 80 columns. So lines
//! differ in height; [`lines`] lays out only the ones in view, however long the
//! scrollback is, and holds a scrolled-back view on its top line as lines are
//! trimmed or the column changes width.
//!
//! Accessibility: the chat is a `Log` node whose visible lines are `Label`
//! nodes, readable line by line however many rows each wraps to. New lines are
//! announced through a separate polite live region, so lines scrolling into
//! view are never announced.

mod lines;

use crate::theme::Theme;
use gpui::{
    AnyElement, App, AvailableSpace, Bounds, Context, ElementId, FontWeight, HighlightStyle,
    Pixels, Role, SharedString, StyledText, Window, div, font, point, prelude::*, px, size,
};
use lines::{LineList, LinePosition};
use mudling_core::store::{Diff, TerminalChange, TerminalState, Versioned};
use mudling_core::{Line, LineKind, Scrollback};
use std::collections::HashMap;

pub const LINE_HEIGHT: Pixels = px(20.);
pub const FONT_SIZE: Pixels = px(14.);
/// A comfortable reading width: about 90 columns of the chat font, however
/// wide the window is.
pub const COLUMN_WIDTH: Pixels = px(800.);
/// The chat column's padding on each side.
pub const COLUMN_PADDING: Pixels = px(32.);
/// What servers wrap their text to, and tables and maps are drawn for.
const MIN_COLUMNS: f32 = 80.;

#[cfg(target_os = "macos")]
pub const MONO_FONT: &str = "Menlo";
#[cfg(target_os = "windows")]
pub const MONO_FONT: &str = "Consolas";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub const MONO_FONT: &str = "DejaVu Sans Mono";

/// How wide the chat column has to be to show [`MIN_COLUMNS`] of the chat font
/// on one row each.
pub fn min_column_width(cx: &App) -> Pixels {
    let text = cx.text_system();
    let advance = text.ch_advance(text.resolve_font(&font(MONO_FONT)), FONT_SIZE);
    (advance.unwrap_or_default() * MIN_COLUMNS).ceil() + COLUMN_PADDING * 2.
}

/// How many of the newest lines one announcement may carry. A burst of
/// server text is summarized by its tail rather than read in full.
const ANNOUNCED_LINES: usize = 5;

pub struct Chat {
    terminal: Versioned<TerminalState>,
    position: LinePosition,
    unannounced: Vec<SharedString>,
    announcement: Option<(u64, SharedString)>,
}

impl Chat {
    pub fn new(terminal: Versioned<TerminalState>) -> Chat {
        Chat {
            terminal,
            position: LinePosition::default(),
            unannounced: Vec::new(),
            announcement: None,
        }
    }

    /// Applies a frame's new lines and queues the server's for the live
    /// region.
    pub fn apply(&mut self, diff: &Diff<TerminalState>, cx: &mut Context<Chat>) {
        let TerminalChange::Append(lines) = &diff.change;
        let server_lines = lines.iter().filter(|l| l.kind() == LineKind::Server);
        let announced: Vec<&Line> = server_lines.rev().take(ANNOUNCED_LINES).collect();
        for line in announced.into_iter().rev() {
            if self.unannounced.len() == ANNOUNCED_LINES {
                self.unannounced.remove(0);
            }
            self.unannounced
                .push(SharedString::from(line.plain().to_owned()));
        }
        self.terminal.apply_diff(diff);
        cx.notify();
    }

    /// Shows a command that went nowhere, because no server is connected. It
    /// isn't announced: the player just typed it.
    pub fn echo_unsent(&mut self, command: &str, cx: &mut Context<Chat>) {
        let echo = TerminalChange::Append(vec![Line::command(command)]);
        self.terminal.write(&echo);
        cx.notify();
    }

    pub fn len(&self) -> usize {
        self.lines().len()
    }

    fn lines(&self) -> &Scrollback {
        self.terminal.value().lines()
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn newest_text(&self) -> Option<String> {
        let newest = self.len().checked_sub(1)?;
        self.lines().get(newest).map(|line| line.plain().to_owned())
    }

    /// Whether the view follows the newest line. A scroll counts at once, not
    /// from the next frame.
    #[cfg(test)]
    pub fn follows_newest(&self) -> bool {
        self.position.is_following()
    }

    /// Scrolls back through history by `distance`, as a reader would.
    pub fn scroll_back(&mut self, distance: Pixels, cx: &mut Context<Chat>) {
        self.scroll_down(-distance, cx);
    }

    fn scroll_down(&mut self, distance: Pixels, cx: &mut Context<Chat>) {
        self.position.scroll(distance);
        cx.notify();
    }

    #[cfg(test)]
    pub fn line_text(&self, index: usize) -> Option<String> {
        self.lines().get(index).map(|line| line.plain().to_owned())
    }

    #[cfg(test)]
    pub fn scroll_offset(&self) -> lines::Anchor {
        self.position.top()
    }

    /// How tall the line at the top of the view drew.
    #[cfg(test)]
    pub fn top_line_height(&self) -> Pixels {
        self.position.top_height()
    }

    /// Lays out this frame's lines inside `bounds`, top to bottom, the order
    /// a screen reader walks them in.
    fn place_lines(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Chat>,
    ) -> Vec<AnyElement> {
        let theme = *cx.global::<Theme>();
        let lines = self.terminal.value().lines();
        let evicted = lines.evicted();
        let space = size(
            AvailableSpace::Definite(bounds.size.width),
            AvailableSpace::MinContent,
        );
        // Placing a line can ask for its height more than once.
        let mut measured = HashMap::new();
        let placement = self
            .position
            .place(evicted, lines.len(), bounds.size, |index| {
                let (_, line_size) = measured.entry(index).or_insert_with(|| {
                    let line = lines
                        .get(index)
                        .expect("placed lines are in the scrollback");
                    let mut element =
                        render_line(line, evicted + index as u64, &theme).into_any_element();
                    let line_size = element.layout_as_root(space, window, cx);
                    (element, line_size)
                });
                line_size.height
            });
        let mut y = bounds.top() + placement.top;
        placement
            .lines
            .filter_map(|index| {
                let (mut element, line_size) = measured.remove(&index)?;
                element.prepaint_at(point(bounds.left(), y), window, cx);
                y += line_size.height;
                Some(element)
            })
            .collect()
    }
}

/// How much of `line` to lay out. Trailing spaces pad a line without showing,
/// so a padded table row that fits doesn't wrap on them. Painted ones stay.
fn drawn_len(line: &Line) -> usize {
    let painted = line.styled().iter().filter(|span| span.style.bg.is_some());
    let painted_end = painted.map(|span| span.end as usize).max().unwrap_or(0);
    line.plain().trim_end_matches(' ').len().max(painted_end)
}

fn render_line(line: &Line, number: u64, theme: &Theme) -> impl IntoElement {
    let drawn = drawn_len(line);
    let highlights = line.styled().iter().map(|span| {
        let style = HighlightStyle {
            color: span.style.fg.map(|c| theme.ansi(c)),
            background_color: span.style.bg.map(|c| theme.ansi(c)),
            font_weight: span.style.bold.then_some(FontWeight::BOLD),
            ..HighlightStyle::default()
        };
        (span.start as usize..(span.end as usize).min(drawn), style)
    });
    let highlights = highlights.filter(|(range, _)| !range.is_empty());
    let styled = StyledText::new(SharedString::from(line.plain()[..drawn].to_owned()))
        .with_highlights(highlights.collect::<Vec<_>>());
    let row = div()
        .id(ElementId::Integer(number))
        .role(Role::Label)
        .aria_value(SharedString::from(line.plain().to_owned()))
        .min_h(LINE_HEIGHT)
        .w_full();
    if line.kind() == LineKind::Command {
        row.flex()
            .gap_2()
            .child(div().text_color(theme.command).child("›"))
            .child(div().flex_1().min_w_0().child(styled))
    } else {
        row.child(styled)
    }
}

impl Render for Chat {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Chat>) -> impl IntoElement {
        if !self.unannounced.is_empty() {
            let batch = self.announcement.as_ref().map_or(0, |(batch, _)| batch + 1);
            let text = std::mem::take(&mut self.unannounced).join("\n");
            self.announcement = Some((batch, text.into()));
        }

        div()
            .id("chat")
            .role(Role::Log)
            .aria_label("Chat")
            .size_full()
            .relative()
            .font_family(MONO_FONT)
            .text_size(FONT_SIZE)
            .line_height(LINE_HEIGHT)
            .child(LineList::new(cx.entity()))
            .children(
                self.announcement
                    .clone()
                    .map(|(batch, text)| live_region(batch, text)),
            )
    }
}

/// A polite live region holding the newest lines. Each batch is a new node,
/// so screen readers announce it even when the text repeats a line. The text
/// is both the label and the value: the Windows and Linux adapters announce a
/// status node's name (its label), while macOS announces its value.
fn live_region(batch: u64, text: SharedString) -> impl IntoElement {
    div()
        .id(ElementId::NamedInteger("new-lines".into(), batch))
        .role(Role::Status)
        .aria_label(text.clone())
        .aria_value(text)
        .a11y_synthetic_children(|builder| {
            builder
                .parent_node()
                .set_live(gpui::accesskit::Live::Polite);
        })
        .absolute()
        .bottom_0()
        .left_0()
        .size(px(1.))
        .overflow_hidden()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{
        Entity, Modifiers, ScrollDelta, ScrollEvent, TestAppContext, VisualTestContext,
        WindowAppearance,
    };
    use mudling_core::LinePipeline;
    use mudling_core::store::{Revision, TERMINAL_CAPACITY};

    fn server_lines(texts: impl IntoIterator<Item = String>) -> Diff<TerminalState> {
        let mut pipeline = LinePipeline::default();
        let mut lines = Vec::new();
        for text in texts {
            pipeline.push(format!("{text}\n").as_bytes(), &mut lines);
        }
        Diff {
            revision: Revision::default(),
            change: TerminalChange::Append(lines),
        }
    }

    fn chat_window(
        cx: &mut TestAppContext,
        history: usize,
    ) -> (Entity<Chat>, &mut VisualTestContext) {
        cx.update(|cx| cx.set_global(Theme::for_appearance(WindowAppearance::Dark)));
        let mut terminal = Versioned::default();
        terminal.apply_diff(&server_lines(
            (0..history).map(|n| format!("history line {n}")),
        ));
        cx.add_window_view(|_, _| Chat::new(terminal))
    }

    impl Chat {
        fn text_of(&self, number: u64) -> String {
            let index = number - self.lines().evicted();
            self.lines().get(index as usize).unwrap().plain().to_owned()
        }

        fn top_text(&self) -> String {
            self.text_of(self.position.top().line)
        }
    }

    fn push(chat: &mut Chat, count: usize, cx: &mut Context<Chat>) {
        chat.apply(&server_lines((0..count).map(|n| format!("new {n}"))), cx);
    }

    fn top_line(chat: &Entity<Chat>, cx: &mut VisualTestContext) -> String {
        chat.read_with(cx, |chat, _| chat.top_text())
    }

    #[gpui::test]
    fn follows_the_newest_line_while_pinned(cx: &mut TestAppContext) {
        let (chat, cx) = chat_window(cx, 1_000);
        assert!(chat.read_with(cx, |chat, _| chat.follows_newest()));
        let before = top_line(&chat, cx);

        chat.update(cx, |chat, cx| push(chat, 5, cx));
        cx.run_until_parked();
        assert!(chat.read_with(cx, |chat, _| chat.follows_newest()));
        assert_ne!(top_line(&chat, cx), before);
    }

    #[gpui::test]
    fn the_wheel_scrolls_back_and_returns_to_the_newest_line(cx: &mut TestAppContext) {
        let (chat, cx) = chat_window(cx, 1_000);
        let wheel = |lines: f32, cx: &mut VisualTestContext| {
            cx.simulate_event(ScrollEvent {
                position: point(px(10.), px(10.)),
                delta: ScrollDelta::Lines(point(0., lines)),
                modifiers: Modifiers::none(),
                ..ScrollEvent::default()
            });
            cx.run_until_parked();
        };

        wheel(3., cx);
        assert!(!chat.read_with(cx, |chat, _| chat.follows_newest()));
        wheel(-3., cx);
        assert!(chat.read_with(cx, |chat, _| chat.follows_newest()));
    }

    #[gpui::test]
    fn scrolled_back_view_holds_still_while_lines_are_trimmed(cx: &mut TestAppContext) {
        let (chat, cx) = chat_window(cx, TERMINAL_CAPACITY);
        chat.update(cx, |chat, cx| chat.scroll_back(LINE_HEIGHT * 200., cx));
        cx.run_until_parked();
        let reading = top_line(&chat, cx);

        // Each new line evicts the oldest, shifting every index by one.
        chat.update(cx, |chat, cx| push(chat, 10, cx));
        cx.run_until_parked();
        assert_eq!(top_line(&chat, cx), reading);
        assert!(!chat.read_with(cx, |chat, _| chat.follows_newest()));
    }

    #[gpui::test]
    fn lines_arriving_before_the_next_frame_keep_a_fresh_scroll_still(cx: &mut TestAppContext) {
        let (chat, cx) = chat_window(cx, TERMINAL_CAPACITY);
        let reading = chat.update(cx, |chat, cx| {
            // The first line above the view, whatever height it has.
            let reading = chat.text_of(chat.position.top().line - 1);
            chat.scroll_back(LINE_HEIGHT, cx);
            // No frame runs between the scroll and these lines.
            push(chat, 10, cx);
            reading
        });
        cx.run_until_parked();
        assert_eq!(top_line(&chat, cx), reading);
    }

    const LONG_LINE: &str = "A goblin swings a rusty sword at you and misses, stumbling past \
                             the torch and down the quiet stone path towards the river.";

    fn long_lines(count: usize) -> Diff<TerminalState> {
        server_lines((0..count).map(|n| format!("{n} {LONG_LINE}")))
    }

    fn narrow_window(cx: &mut VisualTestContext) {
        cx.simulate_resize(size(px(200.), px(400.)));
        cx.run_until_parked();
    }

    #[gpui::test]
    fn a_long_line_wraps_to_the_column_as_one_line(cx: &mut TestAppContext) {
        let (chat, cx) = chat_window(cx, 0);
        chat.update(cx, |chat, cx| chat.apply(&long_lines(1), cx));
        narrow_window(cx);

        chat.read_with(cx, |chat, _| {
            assert_eq!(chat.len(), 1);
            assert!(chat.position.top_height() > LINE_HEIGHT);
        });
    }

    #[gpui::test]
    fn narrowing_the_window_rewraps_and_keeps_the_top_line(cx: &mut TestAppContext) {
        let (chat, cx) = chat_window(cx, 0);
        chat.update(cx, |chat, cx| chat.apply(&long_lines(500), cx));
        cx.run_until_parked();
        chat.update(cx, |chat, cx| chat.scroll_back(LINE_HEIGHT * 300., cx));
        cx.run_until_parked();
        let reading = top_line(&chat, cx);
        let height = chat.read_with(cx, |chat, _| chat.position.top_height());

        narrow_window(cx);
        assert_eq!(top_line(&chat, cx), reading);
        assert!(chat.read_with(cx, |chat, _| chat.position.top_height()) > height);
    }

    #[gpui::test]
    fn lines_that_fit_keep_their_one_row(cx: &mut TestAppContext) {
        let (chat, cx) = chat_window(cx, 0);
        // Padded past the column's edge, as a server's table rows can be.
        let row = format!("| north | {:<400}", "gate |");
        chat.update(cx, |chat, cx| chat.apply(&server_lines([row]), cx));
        narrow_window(cx);
        assert_eq!(
            chat.read_with(cx, |chat, _| chat.position.top_height()),
            LINE_HEIGHT
        );
    }

    #[gpui::test]
    fn eighty_columns_take_one_row_and_a_longer_line_wraps(cx: &mut TestAppContext) {
        let (chat, cx) = chat_window(cx, 0);
        let text_width = cx.update(|_, cx| min_column_width(cx)) - COLUMN_PADDING * 2.;
        // One row high, so the newest line is the top line.
        cx.simulate_resize(size(text_width, LINE_HEIGHT));
        let height_of = |columns: usize, cx: &mut VisualTestContext| {
            let line = "0".repeat(columns);
            chat.update(cx, |chat, cx| chat.apply(&server_lines([line]), cx));
            cx.run_until_parked();
            chat.read_with(cx, |chat, _| chat.top_line_height())
        };

        assert_eq!(height_of(80, cx), LINE_HEIGHT);
        assert_eq!(height_of(81, cx), LINE_HEIGHT * 2.);
    }

    #[test]
    fn only_trailing_spaces_nobody_painted_are_left_out() {
        let mut lines = Vec::new();
        let mut pipeline = LinePipeline::default();
        pipeline.push(b"plain  \n\x1b[41mred  \x1b[0m  \n  \n", &mut lines);
        assert_eq!(lines.iter().map(drawn_len).collect::<Vec<_>>(), [5, 5, 0]);
    }

    #[gpui::test]
    fn only_server_lines_are_announced(cx: &mut TestAppContext) {
        let (chat, cx) = chat_window(cx, 10);
        chat.update(cx, |chat, cx| {
            let command = TerminalChange::Append(vec![Line::command("look")]);
            chat.apply(
                &Diff {
                    revision: Revision::default(),
                    change: command,
                },
                cx,
            );
            chat.apply(&server_lines(["You see a path.".to_owned()]), cx);
        });
        cx.run_until_parked();
        let announced = chat.read_with(cx, |chat, _| {
            chat.announcement.clone().map(|(_, text)| text)
        });
        assert_eq!(announced.as_deref(), Some("You see a path."));
    }
}
