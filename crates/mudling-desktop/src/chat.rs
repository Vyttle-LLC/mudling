//! The chat: a virtualized view onto the core's scrollback.
//!
//! Every line has the same height, so `uniform_list` lays out only the lines
//! in view, however long the scrollback is. Trimming the oldest line never
//! re-lays out the rest; when the reader has scrolled up, the offset moves by
//! one line so the text in view stays put.
//!
//! Accessibility: the chat is a `Log` node whose visible lines are `Label`
//! nodes, readable line by line. New lines are announced through a separate
//! polite live region, so lines scrolling into view are never announced.

use crate::theme::Theme;
use gpui::{
    Context, ElementId, FontWeight, HighlightStyle, Pixels, Role, ScrollStrategy, SharedString,
    StyledText, UniformListScrollHandle, Window, div, point, prelude::*, px, uniform_list,
};
use mudling_core::replay::ReplayFeed;
use mudling_core::{AnsiParser, Line, LineKind, Scrollback};
use std::ops::Range;

pub const LINE_HEIGHT: Pixels = px(20.);
pub const FONT_SIZE: Pixels = px(14.);
/// A comfortable reading width: about 100 columns of the chat font.
pub const COLUMN_WIDTH: Pixels = px(860.);

#[cfg(target_os = "macos")]
pub const MONO_FONT: &str = "Menlo";
#[cfg(target_os = "windows")]
pub const MONO_FONT: &str = "Consolas";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub const MONO_FONT: &str = "DejaVu Sans Mono";

/// How many of the newest lines one announcement may carry. A burst of
/// server text is summarized by its tail rather than read in full.
const ANNOUNCED_LINES: usize = 5;

pub struct Chat {
    scrollback: Scrollback,
    parser: AnsiParser,
    scroll: UniformListScrollHandle,
    /// Whether the view follows the newest line. Scrolling up unpins it;
    /// scrolling back to the bottom pins it again.
    pinned: bool,
    unannounced: Vec<SharedString>,
    announcement: Option<(u64, SharedString)>,
}

impl Chat {
    pub fn new(capacity: usize) -> Chat {
        Chat {
            scrollback: Scrollback::with_capacity(capacity),
            parser: AnsiParser::default(),
            scroll: UniformListScrollHandle::new(),
            pinned: true,
            unannounced: Vec::new(),
            announcement: None,
        }
    }

    /// Loads history without announcing it, as on reconnecting to a session.
    pub fn load_history(&mut self, feed: &mut ReplayFeed, lines: usize) {
        for _ in 0..lines {
            let line = self.parser.parse_line(feed.next_line());
            self.scrollback.push(line);
        }
    }

    pub fn push_server_line(&mut self, raw: &str, cx: &mut Context<Chat>) {
        let line = self.parser.parse_line(raw);
        self.push(line, cx);
    }

    pub fn push_command(&mut self, command: &str, cx: &mut Context<Chat>) {
        self.push(Line::command(command), cx);
    }

    pub fn len(&self) -> usize {
        self.scrollback.len()
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn newest_text(&self) -> Option<String> {
        let newest = self.scrollback.len().checked_sub(1)?;
        self.scrollback
            .get(newest)
            .map(|line| line.text.to_string())
    }

    fn push(&mut self, line: Line, cx: &mut Context<Chat>) {
        let evicted_before = self.scrollback.evicted();
        if self.unannounced.len() == ANNOUNCED_LINES {
            self.unannounced.remove(0);
        }
        self.unannounced
            .push(SharedString::from(line.text.to_string()));
        self.scrollback.push(line);
        let evicted = self.scrollback.evicted() - evicted_before;
        if evicted > 0 && !self.pinned {
            self.keep_reading_position(evicted);
        }
        cx.notify();
    }

    /// Scrolls back through history by `distance`, as a reader would.
    pub fn scroll_back(&mut self, distance: Pixels, cx: &mut Context<Chat>) {
        self.shift_view_up(distance);
        cx.notify();
    }

    #[cfg(test)]
    pub fn scroll_offset(&self) -> gpui::Point<Pixels> {
        self.scroll.0.borrow().base_handle.offset()
    }

    fn keep_reading_position(&self, evicted_lines: u64) {
        self.shift_view_up(LINE_HEIGHT * evicted_lines as f32);
    }

    fn shift_view_up(&self, distance: Pixels) {
        let state = self.scroll.0.borrow();
        let offset = state.base_handle.offset();
        state
            .base_handle
            .set_offset(point(offset.x, (offset.y + distance).min(px(0.))));
    }

    fn render_lines(
        &mut self,
        range: Range<usize>,
        cx: &mut Context<Chat>,
    ) -> Vec<gpui::AnyElement> {
        let theme = cx.global::<Theme>();
        let first_number = self.scrollback.evicted();
        range
            .filter_map(|index| {
                let line = self.scrollback.get(index)?;
                let number = first_number + index as u64;
                Some(render_line(line, number, theme).into_any_element())
            })
            .collect()
    }
}

fn render_line(line: &Line, number: u64, theme: &Theme) -> impl IntoElement {
    let text = SharedString::from(line.text.to_string());
    let highlights = line.spans.iter().map(|span| {
        let style = HighlightStyle {
            color: span.style.fg.map(|c| theme.ansi(c)),
            background_color: span.style.bg.map(|c| theme.ansi(c)),
            font_weight: span.style.bold.then_some(FontWeight::BOLD),
            ..HighlightStyle::default()
        };
        (span.start as usize..span.end as usize, style)
    });
    let styled = StyledText::new(text.clone()).with_highlights(highlights.collect::<Vec<_>>());
    div()
        .id(ElementId::Integer(number))
        .role(Role::Label)
        .aria_value(text)
        .h(LINE_HEIGHT)
        .w_full()
        .overflow_hidden()
        .whitespace_nowrap()
        .when(line.kind == LineKind::Command, |line| {
            line.text_color(theme.command)
        })
        .child(styled)
}

impl Render for Chat {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Chat>) -> impl IntoElement {
        if let Some(at_end) = self.scroll.is_scrolled_to_end() {
            self.pinned = at_end;
        }
        if self.pinned {
            self.scroll
                .scroll_to_item(usize::MAX, ScrollStrategy::Bottom);
        }
        if !self.unannounced.is_empty() {
            let batch = self.announcement.as_ref().map_or(0, |(batch, _)| batch + 1);
            let text = std::mem::take(&mut self.unannounced).join("\n");
            self.announcement = Some((batch, text.into()));
        }

        let lines = uniform_list(
            "chat-lines",
            self.scrollback.len(),
            cx.processor(|chat, range, _, cx| chat.render_lines(range, cx)),
        )
        .track_scroll(&self.scroll)
        .size_full();

        div()
            .id("chat")
            .role(Role::Log)
            .aria_label("Chat")
            .size_full()
            .relative()
            .font_family(MONO_FONT)
            .text_size(FONT_SIZE)
            .line_height(LINE_HEIGHT)
            .child(lines)
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
    use gpui::{Entity, TestAppContext, VisualTestContext, WindowAppearance};

    fn chat_window(
        cx: &mut TestAppContext,
        capacity: usize,
        history: usize,
    ) -> (Entity<Chat>, &mut VisualTestContext) {
        cx.update(|cx| cx.set_global(Theme::for_appearance(WindowAppearance::Dark)));
        let lines = (0..history.max(1))
            .map(|n| format!("history line {n}"))
            .collect();
        let mut feed = ReplayFeed::new(lines);
        cx.add_window_view(|_, _| {
            let mut chat = Chat::new(capacity);
            chat.load_history(&mut feed, history);
            chat
        })
    }

    fn top_line(chat: &Entity<Chat>, cx: &mut VisualTestContext) -> String {
        chat.read_with(cx, |chat, _| {
            let top = (-chat.scroll_offset().y / LINE_HEIGHT).round() as usize;
            chat.scrollback.get(top).unwrap().text.to_string()
        })
    }

    #[gpui::test]
    fn follows_the_newest_line_while_pinned(cx: &mut TestAppContext) {
        let (chat, cx) = chat_window(cx, 1_000, 1_000);
        assert_eq!(
            chat.read_with(cx, |chat, _| chat.scroll.is_scrolled_to_end()),
            Some(true)
        );
        let before = top_line(&chat, cx);

        chat.update(cx, |chat, cx| {
            (0..5).for_each(|n| chat.push_server_line(&format!("new {n}"), cx))
        });
        cx.run_until_parked();
        assert_eq!(
            chat.read_with(cx, |chat, _| chat.scroll.is_scrolled_to_end()),
            Some(true)
        );
        assert_ne!(top_line(&chat, cx), before);
    }

    #[gpui::test]
    fn scrolled_back_view_holds_still_while_lines_are_trimmed(cx: &mut TestAppContext) {
        let (chat, cx) = chat_window(cx, 1_000, 1_000);
        chat.update(cx, |chat, cx| chat.scroll_back(LINE_HEIGHT * 200., cx));
        cx.run_until_parked();
        let reading = top_line(&chat, cx);

        // Each new line evicts the oldest, shifting every index by one.
        chat.update(cx, |chat, cx| {
            (0..10).for_each(|n| chat.push_server_line(&format!("new {n}"), cx))
        });
        cx.run_until_parked();
        assert_eq!(top_line(&chat, cx), reading);
        assert_eq!(
            chat.read_with(cx, |chat, _| chat.scroll.is_scrolled_to_end()),
            Some(false)
        );
    }
}
