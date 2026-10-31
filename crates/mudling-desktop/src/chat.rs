//! The chat: a virtualized view onto the shell's copy of the terminal store.
//!
//! Servers wrap their own text to 80 columns, so the column keeps that much
//! room and most lines take one row. A longer line wraps rather than lose its
//! text, as does any line in a window too narrow for 80 columns. So lines
//! differ in height; [`lines`] lays out only the ones in view, however long the
//! scrollback is, and holds a scrolled-back view on its top line as lines are
//! trimmed or the column changes width.
//!
//! Reading back splits the chat: a history pane holds still at the reading
//! position over a live pane pinned to the newest lines. They are two views of
//! one scrollback, so between them they draw no more lines than a whole chat.
//!
//! Accessibility: the live pane is a `Log` node and the history pane a group,
//! whose visible lines are `Label` nodes, readable line by line however many
//! rows each wraps to. New lines are announced through a separate polite live
//! region, so lines scrolling into view are never announced.

mod find;
mod lines;

use crate::assets::Icon;
use crate::theme::Theme;
pub use find::CloseFind;
use find::{Find, Results};
use gpui::{
    AnyElement, App, AvailableSpace, Bounds, Context, Div, DragMoveEvent, ElementId, Entity,
    FontWeight, HighlightStyle, KeyBinding, Pixels, Role, ScrollEvent, SharedString, StyledText,
    Window, actions, div, font, point, prelude::*, px, relative, size,
};
use lines::{LineList, LinePosition, Pane};
use mudling_core::store::{Diff, TerminalChange, TerminalState, Versioned};
use mudling_core::{Line, LineKind, Scrollback};
use std::collections::HashMap;

/// A row at the default chat size, and what a notch of the wheel scrolls.
pub const LINE_HEIGHT: Pixels = px(20.);
/// A comfortable reading width: about 90 columns of the chat font, however
/// wide the window is.
pub const COLUMN_WIDTH: Pixels = px(800.);
/// The chat column's padding on each side.
pub const COLUMN_PADDING: Pixels = px(32.);
/// What servers wrap their text to, and tables and maps are drawn for.
const MIN_COLUMNS: f32 = 80.;
/// The live pane's share of the chat's height, until its divider is dragged.
const LIVE_SHARE: f32 = 1. / 3.;
/// How far the divider drags: neither pane closes.
const LIVE_SHARES: std::ops::RangeInclusive<f32> = 0.15..=0.85;
const DIVIDER_GRIP_HEIGHT: Pixels = px(9.);
const CHIP_HEIGHT: Pixels = px(28.);
const CONTEXT: &str = "ChatKeys";
/// Added to [`CONTEXT`] while the find bar is open.
const FIND_OPEN: &str = "find_open";

actions!(chat, [BackToLive, PageBack, PageForward]);

/// Keys that reach the chat wherever the focus is, unless what has it binds
/// them too. They name the root's key context because gpui ranks a binding
/// with no context above every other. The shell routes the command bar's End.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("pageup", PageBack, Some(CONTEXT)),
        KeyBinding::new("pagedown", PageForward, Some(CONTEXT)),
        KeyBinding::new("escape", BackToLive, Some(CONTEXT)),
        KeyBinding::new("end", BackToLive, Some(CONTEXT)),
    ]);
    find::bind_keys(cx);
}

/// Handles those keys on `root`, an ancestor of everything that takes focus:
/// the chat itself never does, so typing stays in the command bar.
pub fn handle_keys(root: Div, chat: &Entity<Chat>, cx: &App) -> Div {
    let (back, forward, live) = (chat.clone(), chat.clone(), chat.clone());
    let context = match chat.read(cx).find_is_open() {
        true => format!("{CONTEXT} {FIND_OPEN}"),
        false => CONTEXT.to_owned(),
    };
    root.key_context(context.as_str())
        .on_action(move |_: &PageBack, _, cx| back.update(cx, |chat, cx| chat.page_back(cx)))
        .on_action(move |_: &PageForward, _, cx| {
            forward.update(cx, |chat, cx| chat.page_forward(cx))
        })
        .on_action(move |_: &BackToLive, _, cx| live.update(cx, |chat, cx| chat.back_to_live(cx)))
}

/// How wide the chat column has to be to show [`MIN_COLUMNS`] of the chosen
/// chat font on one row each.
pub fn min_column_width(cx: &App) -> Pixels {
    let text = cx.text_system();
    let choice = cx.global::<Theme>().choice;
    let advance = text.ch_advance(text.resolve_font(&font(choice.font)), choice.chat_size());
    (advance.unwrap_or_default() * MIN_COLUMNS).ceil() + COLUMN_PADDING * 2.
}

/// How wide the chat column grows: its reading width, or more if the chosen
/// font needs it for [`MIN_COLUMNS`].
pub fn column_width(cx: &App) -> Pixels {
    COLUMN_WIDTH.max(min_column_width(cx))
}

/// How many of the newest lines one announcement may carry. A burst of
/// server text is summarized by its tail rather than read in full.
const ANNOUNCED_LINES: usize = 5;

/// The open split's history pane.
struct History {
    position: LinePosition,
    /// The number the first line to arrive after the split opened took.
    first_new_line: u64,
    /// How many that arrived since are only a prompt.
    new_prompts: u64,
}

/// Marks the drag of the divider.
struct DraggingDivider;

impl Render for DraggingDivider {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

pub struct Chat {
    terminal: Versioned<TerminalState>,
    /// Always follows the newest line. Without a split it fills the chat.
    live: LinePosition,
    history: Option<History>,
    /// Kept while the split is closed, so it reopens as the player left it.
    live_share: f32,
    /// As the player chose, as of the last frame. Hidden prompts stay in the
    /// scrollback with no height, so a change reaches every line at once and
    /// costs a frame no more than it costs to walk past them.
    prompts_shown: bool,
    unannounced: Vec<SharedString>,
    announcement: Option<(u64, SharedString)>,
    find: Option<Find>,
}

/// What of `line` the chat draws, if anything.
fn shown(line: &Line, prompts_shown: bool) -> Option<Line> {
    if prompts_shown {
        Some(line.clone())
    } else {
        line.without_prompt()
    }
}

impl Chat {
    pub fn new(terminal: Versioned<TerminalState>) -> Chat {
        Chat {
            terminal,
            live: LinePosition::default(),
            history: None,
            live_share: LIVE_SHARE,
            prompts_shown: false,
            unannounced: Vec::new(),
            announcement: None,
            find: None,
        }
    }

    /// Applies a frame's new lines and queues what shows of the server's for
    /// the live region.
    pub fn apply(&mut self, diff: &Diff<TerminalState>, cx: &mut Context<Chat>) {
        let TerminalChange::Append(lines) = &diff.change;
        let server_lines = lines.iter().filter(|l| l.kind() == LineKind::Server);
        let showing = server_lines.filter_map(|line| shown(line, self.prompts_shown));
        let announced: Vec<Line> = showing.rev().take(ANNOUNCED_LINES).collect();
        for line in announced.into_iter().rev() {
            if self.unannounced.len() == ANNOUNCED_LINES {
                self.unannounced.remove(0);
            }
            self.unannounced
                .push(SharedString::from(line.plain().to_owned()));
        }
        if let Some(history) = &mut self.history {
            let prompts = lines.iter().filter(|line| line.is_only_prompt());
            history.new_prompts += prompts.count() as u64;
        }
        self.terminal.apply_diff(diff);
        self.keep_find_current(cx);
        cx.notify();
    }

    /// Shows a command that went nowhere, because no server is connected. It
    /// isn't announced: the player just typed it.
    pub fn echo_unsent(&mut self, command: &str, cx: &mut Context<Chat>) {
        let echo = TerminalChange::Append(vec![Line::command(command)]);
        self.terminal.write(&echo);
        self.keep_find_current(cx);
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
        let mut lines = self.lines().iter().rev();
        let newest = lines.find_map(|line| shown(line, self.prompts_shown))?;
        Some(newest.plain().to_owned())
    }

    /// Whether the chat is whole, on the newest line. A scroll counts at once,
    /// not from the next frame.
    #[cfg(test)]
    pub fn follows_newest(&self) -> bool {
        self.history.is_none()
    }

    /// The number the next line will take. Trimming doesn't change it.
    fn next_line_number(&self) -> u64 {
        self.lines().evicted() + self.len() as u64
    }

    fn open_history(&mut self, position: LinePosition) {
        let first_new_line = self.next_line_number();
        self.history = Some(History {
            position,
            first_new_line,
            new_prompts: 0,
        });
    }

    /// Scrolls back through history by `distance`, as a reader would.
    pub fn scroll_back(&mut self, distance: Pixels, cx: &mut Context<Chat>) {
        self.scroll_down(-distance, cx);
    }

    /// Moves the history pane, which is every scroll's target, wherever the
    /// pointer is. Scrolling up a whole chat opens the split from the view it
    /// was showing, so what stays on screen doesn't move.
    fn scroll_down(&mut self, distance: Pixels, cx: &mut Context<Chat>) {
        if let Some(history) = &mut self.history {
            history.position.scroll(distance);
        } else if distance < px(0.) && self.live.has_lines_above(self.lines().evicted()) {
            let mut position = self.live.clone();
            position.scroll(distance);
            self.open_history(position);
        } else {
            return;
        }
        cx.notify();
    }

    /// A page of what the history pane shows or would show, less a line kept
    /// for context.
    fn page(&self) -> Pixels {
        let shown = match &self.history {
            Some(history) => history.position.height(),
            None => self.live.height() * (1. - self.live_share),
        };
        (shown - LINE_HEIGHT).max(LINE_HEIGHT)
    }

    fn page_back(&mut self, cx: &mut Context<Chat>) {
        self.scroll_back(self.page(), cx);
    }

    fn page_forward(&mut self, cx: &mut Context<Chat>) {
        self.scroll_down(self.page(), cx);
    }

    /// Opens the history pane with the line numbered `number` on top: how find
    /// shows a result. A line's number is [`Scrollback::evicted`] plus its
    /// index. A number past the newest line means the newest, and one already
    /// trimmed the oldest. New lines count from the first time the split
    /// opened.
    pub fn open_history_at(&mut self, number: u64, cx: &mut Context<Chat>) {
        let newest = self.next_line_number().saturating_sub(1);
        let position = LinePosition::reading_from(number.min(newest));
        match &mut self.history {
            Some(history) => history.position = position,
            None => self.open_history(position),
        }
        cx.notify();
    }

    /// Closes the split, if it's open.
    pub fn back_to_live(&mut self, cx: &mut Context<Chat>) {
        if self.history.take().is_some() {
            cx.notify();
        }
    }

    /// Lines that arrived since the split opened and show.
    fn new_lines(&self, history: &History) -> u64 {
        let hidden = if self.prompts_shown {
            0
        } else {
            history.new_prompts
        };
        self.next_line_number() - history.first_new_line - hidden
    }

    fn drag_divider(&mut self, y: Pixels, chat: Bounds<Pixels>, cx: &mut Context<Chat>) {
        let share = (chat.bottom() - y) / chat.size.height;
        self.live_share = share.clamp(*LIVE_SHARES.start(), *LIVE_SHARES.end());
        cx.notify();
    }

    #[cfg(test)]
    pub fn line_text(&self, index: usize) -> Option<String> {
        self.lines().get(index).map(|line| line.plain().to_owned())
    }

    /// Where the view that scrolls is.
    #[cfg(test)]
    fn reading(&self) -> &LinePosition {
        self.history
            .as_ref()
            .map_or(&self.live, |history| &history.position)
    }

    #[cfg(test)]
    pub fn scroll_offset(&self) -> lines::Anchor {
        self.reading().top()
    }

    /// How tall the line at the top of the view drew.
    #[cfg(test)]
    pub fn top_line_height(&self) -> Pixels {
        self.reading().top_height()
    }

    /// Lays out this frame's lines of `pane` inside `bounds`, top to bottom,
    /// the order a screen reader walks them in. A history pane scrolled down
    /// to the newest line closes the split.
    fn place_lines(
        &mut self,
        pane: Pane,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Chat>,
    ) -> Vec<AnyElement> {
        let position = match (pane, &mut self.history) {
            (Pane::Live, _) => &mut self.live,
            (Pane::History, Some(history)) => &mut history.position,
            (Pane::History, None) => return Vec::new(),
        };
        let found = match (pane, &self.find) {
            (Pane::History, Some(find)) => find.results.as_ref(),
            _ => None,
        };
        let theme = *cx.global::<Theme>();
        let lines = self.terminal.value().lines();
        let evicted = lines.evicted();
        let prompts_shown = self.prompts_shown;
        let space = size(
            AvailableSpace::Definite(bounds.size.width),
            AvailableSpace::MinContent,
        );
        // Placing a line can ask for its height more than once.
        let mut measured = HashMap::new();
        let placement = position.place(evicted, lines.len(), bounds.size, |index| {
            let line = lines
                .get(index)
                .expect("placed lines are in the scrollback");
            if !prompts_shown && line.is_only_prompt() {
                return px(0.);
            }
            let (_, line_size) = measured.entry(index).or_insert_with(|| {
                let line = shown(line, prompts_shown).expect("it isn't only a prompt");
                let mut element =
                    render_line(&line, evicted + index as u64, found, &theme).into_any_element();
                let line_size = element.layout_as_root(space, window, cx);
                (element, line_size)
            });
            line_size.height
        });
        if pane == Pane::History && position.is_following() {
            // This frame is already laid out, and a notify would be lost in it.
            cx.defer_in(window, |chat, _, cx| chat.back_to_live(cx));
        }
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

    /// One pane of the capped, centered column.
    fn pane(&self, pane: Pane, cx: &mut Context<Chat>) -> gpui::Stateful<Div> {
        let (id, role, label) = match pane {
            Pane::History => ("chat-history", Role::Group, "Chat history"),
            Pane::Live => ("chat-live", Role::Log, "Chat"),
        };
        div()
            .id(id)
            .role(role)
            .aria_label(label)
            .w_full()
            .max_w(column_width(cx))
            .px(COLUMN_PADDING)
            .child(LineList::new(cx.entity(), pane))
    }

    /// The line between the panes, over them, which grabs a few pixels to each
    /// side, and the chip on it.
    fn divider(&self, history: &History, cx: &mut Context<Chat>) -> Div {
        let theme = *cx.global::<Theme>();
        let new_lines = match self.new_lines(history) {
            0 => SharedString::from("Back to live"),
            1 => "Back to live · 1 new line".into(),
            count => format!("Back to live · {count} new lines").into(),
        };
        let grip = div()
            .id("chat-divider")
            .debug_selector(|| "chat-divider".to_owned())
            .absolute()
            .left_0()
            .right_0()
            .top(-DIVIDER_GRIP_HEIGHT / 2.)
            .h(DIVIDER_GRIP_HEIGHT)
            .cursor_row_resize()
            .on_drag(DraggingDivider, |_, _, _, cx| {
                cx.stop_propagation();
                cx.new(|_| DraggingDivider)
            });
        let chip = div()
            .id("back-to-live")
            .debug_selector(|| "back-to-live".to_owned())
            .role(Role::Button)
            .aria_label(new_lines.clone())
            .h(CHIP_HEIGHT)
            .px_3()
            .flex()
            .items_center()
            .gap_1p5()
            .rounded_md()
            .border_1()
            .border_color(theme.accent)
            .bg(theme.surface_raised)
            .hover(|style| style.bg(theme.hover))
            .font_family(crate::assets::UI_FONT)
            .text_size(px(13.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.accent)
            .cursor_pointer()
            .on_click(cx.listener(|chat, _, _, cx| chat.back_to_live(cx)))
            .child(Icon::ArrowDown.svg().size(px(14.)).text_color(theme.accent))
            .child(new_lines);
        div()
            .absolute()
            .left_0()
            .right_0()
            .top(relative(1. - self.live_share))
            .h(px(1.))
            .bg(theme.divider)
            .child(grip)
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(-CHIP_HEIGHT / 2.)
                    .flex()
                    .justify_center()
                    .child(chip),
            )
    }
}

/// How much of `line` to lay out. Trailing spaces pad a line without showing,
/// so a padded table row that fits doesn't wrap on them. Painted ones stay.
fn drawn_len(line: &Line) -> usize {
    let painted = line.styled().iter().filter(|span| span.style.bg.is_some());
    let painted_end = painted.map(|span| span.end as usize).max().unwrap_or(0);
    line.plain().trim_end_matches(' ').len().max(painted_end)
}

/// One line, with what `found` found in it highlighted.
fn render_line(
    line: &Line,
    number: u64,
    found: Option<&Results>,
    theme: &Theme,
) -> impl IntoElement {
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
    // A prompt the game didn't color recedes behind what the game said.
    let quiet_prompt = line.styled().is_empty().then(|| {
        let style = HighlightStyle {
            color: Some(theme.text_muted),
            ..HighlightStyle::default()
        };
        (0..line.prompt_len().min(drawn), style)
    });
    let quiet_prompt = quiet_prompt.filter(|(range, _)| !range.is_empty());
    let matches = found.map_or_else(Vec::new, |found| {
        found.paint(line.plain(), drawn, number, theme)
    });
    let styled =
        StyledText::new(SharedString::from(line.plain()[..drawn].to_owned())).with_highlights(
            find::paint_matches(highlights.chain(quiet_prompt).collect(), matches),
        );
    let row = div()
        .id(ElementId::Integer(number))
        .role(Role::Label)
        .aria_value(SharedString::from(line.plain().to_owned()))
        .min_h(theme.choice.chat_line_height())
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
        let choice = cx.global::<Theme>().choice;
        self.prompts_shown = choice.shows_prompts();
        let line_height = choice.chat_line_height();
        if !self.unannounced.is_empty() {
            let batch = self.announcement.as_ref().map_or(0, |(batch, _)| batch + 1);
            let text = std::mem::take(&mut self.unannounced).join("\n");
            self.announcement = Some((batch, text.into()));
        }

        let mut live = self
            .pane(Pane::Live, cx)
            .debug_selector(|| "chat-area".to_owned());
        let mut history = None;
        let mut divider = None;
        if let Some(open) = &self.history {
            let pane = self.pane(Pane::History, cx);
            history = Some(
                pane.debug_selector(|| "chat-history".to_owned())
                    .flex_1()
                    .min_h_0()
                    .pt_4(),
            );
            divider = Some(self.divider(open, cx));
            live = live.flex_none().h(relative(self.live_share)).pt_2();
        } else {
            live = live.flex_1().min_h_0().pt_4();
        }

        div()
            .id("chat")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .items_center()
            .font_family(choice.font)
            .text_size(choice.chat_size())
            .line_height(line_height)
            .on_scroll(cx.listener(move |chat, event: &ScrollEvent, _, cx| {
                chat.scroll_down(-event.delta.pixel_delta(line_height).y, cx);
            }))
            .on_drag_move(
                cx.listener(|chat, event: &DragMoveEvent<DraggingDivider>, _, cx| {
                    chat.drag_divider(event.event.position.y, event.bounds, cx);
                }),
            )
            .children(self.find_bar(cx))
            .children(history)
            .child(live)
            .children(divider)
            .children(
                self.announcement
                    .clone()
                    .map(|(batch, text)| live_region("new-lines", batch, text)),
            )
    }
}

/// A polite live region holding `text`. Each batch is a new node, so screen
/// readers announce it even when the text repeats itself. The text is both the
/// label and the value: the Windows and Linux adapters announce a status node's
/// name (its label), while macOS announces its value.
fn live_region(name: &'static str, batch: u64, text: SharedString) -> impl IntoElement {
    div()
        .id(ElementId::NamedInteger(name.into(), batch))
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
    use crate::theme::Appearance;
    use gpui::{
        Modifiers, MouseButton, ScrollDelta, TestAppContext, VisualTestContext, WindowAppearance,
    };
    use mudling_core::LinePipeline;
    use mudling_core::store::{Revision, TERMINAL_CAPACITY};

    pub(super) fn server_lines(texts: impl IntoIterator<Item = String>) -> Diff<TerminalState> {
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

    pub(super) fn chat_window(
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
            self.text_of(self.reading().top().line)
        }
    }

    /// A chat with a page of lines, split `distance` back from the newest.
    fn split_window(
        cx: &mut TestAppContext,
        distance: Pixels,
    ) -> (Entity<Chat>, &mut VisualTestContext) {
        let (chat, cx) = chat_window(cx, 1_000);
        cx.run_until_parked();
        chat.update(cx, |chat, cx| chat.scroll_back(distance, cx));
        cx.run_until_parked();
        (chat, cx)
    }

    pub(super) fn is_split(chat: &Entity<Chat>, cx: &mut VisualTestContext) -> bool {
        !chat.read_with(cx, |chat, _| chat.follows_newest())
    }

    fn wheel(lines: f32, cx: &mut VisualTestContext) {
        cx.simulate_event(ScrollEvent {
            position: point(px(10.), px(10.)),
            delta: ScrollDelta::Lines(point(0., lines)),
            modifiers: Modifiers::none(),
            ..ScrollEvent::default()
        });
        cx.run_until_parked();
    }

    pub(super) fn push(chat: &mut Chat, count: usize, cx: &mut Context<Chat>) {
        chat.apply(&server_lines((0..count).map(|n| format!("new {n}"))), cx);
    }

    fn top_line(chat: &Entity<Chat>, cx: &mut VisualTestContext) -> String {
        chat.read_with(cx, |chat, _| chat.top_text())
    }

    #[gpui::test]
    fn follows_the_newest_line_while_pinned(cx: &mut TestAppContext) {
        let (chat, cx) = chat_window(cx, 1_000);
        cx.run_until_parked();
        assert!(!is_split(&chat, cx));
        let before = top_line(&chat, cx);

        chat.update(cx, |chat, cx| push(chat, 5, cx));
        cx.run_until_parked();
        assert!(!is_split(&chat, cx));
        assert_ne!(top_line(&chat, cx), before);
    }

    #[gpui::test]
    fn the_wheel_splits_the_chat_and_scrolling_to_the_bottom_closes_it(cx: &mut TestAppContext) {
        let (chat, cx) = chat_window(cx, 1_000);
        cx.run_until_parked();

        wheel(3., cx);
        assert!(is_split(&chat, cx));
        wheel(-1_000., cx);
        assert!(!is_split(&chat, cx));
    }

    #[gpui::test]
    fn the_wheel_scrolls_the_history_pane_from_over_the_live_pane(cx: &mut TestAppContext) {
        let (chat, cx) = split_window(cx, LINE_HEIGHT * 100.);
        let reading = chat.read_with(cx, |chat, _| chat.scroll_offset());

        let live = cx.debug_bounds("chat-area").unwrap().center();
        cx.simulate_event(ScrollEvent {
            position: live,
            delta: ScrollDelta::Lines(point(0., 3.)),
            modifiers: Modifiers::none(),
            ..ScrollEvent::default()
        });
        cx.run_until_parked();
        assert_ne!(chat.read_with(cx, |chat, _| chat.scroll_offset()), reading);
    }

    #[gpui::test]
    fn lines_that_all_fit_have_nothing_to_split_for(cx: &mut TestAppContext) {
        let (chat, cx) = chat_window(cx, 3);
        cx.run_until_parked();
        wheel(3., cx);
        assert!(!is_split(&chat, cx));
    }

    #[gpui::test]
    fn the_history_pane_holds_still_and_the_live_pane_follows(cx: &mut TestAppContext) {
        let (chat, cx) = split_window(cx, LINE_HEIGHT * 100.);
        let reading = top_line(&chat, cx);
        let live_top = chat.read_with(cx, |chat, _| chat.live.top());

        chat.update(cx, |chat, cx| push(chat, 5, cx));
        cx.run_until_parked();
        assert_eq!(top_line(&chat, cx), reading);
        assert_eq!(
            chat.read_with(cx, |chat, _| chat.live.top().line),
            live_top.line + 5
        );
    }

    #[gpui::test]
    fn the_panes_share_the_chat_a_third_of_it_live(cx: &mut TestAppContext) {
        let (chat, cx) = split_window(cx, LINE_HEIGHT * 100.);
        let live = cx.debug_bounds("chat-area").unwrap();
        let divider = cx.debug_bounds("chat-divider").unwrap();
        let height = cx.update(|window, _| window.viewport_size().height);

        assert_eq!(live.size.height, height * LIVE_SHARE);
        assert_eq!(live.bottom(), height);
        assert_eq!(divider.center().y.round(), live.top().round());
        let shown = chat.read_with(cx, |chat, _| {
            chat.history.as_ref().unwrap().position.height()
        });
        assert!(shown >= height / 2., "the history pane drew {shown:?}");
    }

    #[gpui::test]
    fn the_chip_counts_new_lines_and_closes_the_split(cx: &mut TestAppContext) {
        let (chat, cx) = split_window(cx, LINE_HEIGHT * 100.);
        let count = |chat: &Entity<Chat>, cx: &mut VisualTestContext| {
            chat.read_with(cx, |chat, _| chat.new_lines(chat.history.as_ref().unwrap()))
        };
        assert_eq!(count(&chat, cx), 0);

        chat.update(cx, |chat, cx| {
            push(chat, 13, cx);
            chat.echo_unsent("look", cx);
        });
        cx.run_until_parked();
        assert_eq!(count(&chat, cx), 14);

        let chip = cx.debug_bounds("back-to-live").unwrap();
        cx.simulate_click(chip.center(), Modifiers::none());
        cx.run_until_parked();
        assert!(!is_split(&chat, cx));
    }

    /// `count` lines, each a prompt the HUD repeats alone, after a server line.
    fn prompts(count: usize) -> Diff<TerminalState> {
        let Diff { change, revision } = server_lines((0..count).map(|n| format!("<{n}hp> ")));
        let TerminalChange::Append(lines) = change;
        let change = TerminalChange::Append(
            lines
                .iter()
                .map(|line| line.with_prompt(line.plain().len()))
                .collect(),
        );
        Diff { change, revision }
    }

    fn show_prompts(show_prompts: bool, cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            let choice = cx.global::<Theme>().choice;
            cx.set_global(Theme::chosen(
                Appearance {
                    show_prompts,
                    ..choice
                },
                WindowAppearance::Dark,
            ));
            window.refresh();
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn hidden_prompts_take_no_room_and_the_setting_reaches_the_scrollback(cx: &mut TestAppContext) {
        let (chat, cx) = chat_window(cx, 3);
        cx.run_until_parked();
        chat.update(cx, |chat, cx| chat.apply(&prompts(2), cx));
        cx.run_until_parked();
        let newest = |chat: &Entity<Chat>, cx: &mut VisualTestContext| {
            chat.read_with(cx, |chat, _| chat.newest_text().unwrap())
        };
        assert_eq!(chat.read_with(cx, |chat, _| chat.len()), 5);
        assert_eq!(newest(&chat, cx), "history line 2");

        show_prompts(true, cx);
        assert_eq!(newest(&chat, cx), "<1hp> ");

        show_prompts(false, cx);
        assert_eq!(newest(&chat, cx), "history line 2");
    }

    #[gpui::test]
    fn a_hidden_prompt_is_not_announced_or_counted_but_what_follows_it_is(cx: &mut TestAppContext) {
        let (chat, cx) = split_window(cx, LINE_HEIGHT * 100.);
        let glued = {
            let Diff { change, revision } = server_lines(["<9hp> You rest.".to_owned()]);
            let TerminalChange::Append(lines) = change;
            let change = TerminalChange::Append(vec![lines[0].with_prompt(6)]);
            Diff { change, revision }
        };
        chat.update(cx, |chat, cx| {
            chat.apply(&prompts(3), cx);
            chat.apply(&glued, cx);
        });
        cx.run_until_parked();
        let count = |chat: &Entity<Chat>, cx: &mut VisualTestContext| {
            chat.read_with(cx, |chat, _| chat.new_lines(chat.history.as_ref().unwrap()))
        };
        assert_eq!(count(&chat, cx), 1);
        let announced = chat.read_with(cx, |chat, _| chat.announcement.clone());
        assert_eq!(announced.unwrap().1, "You rest.");

        show_prompts(true, cx);
        assert_eq!(count(&chat, cx), 4);
    }

    #[gpui::test]
    fn the_count_survives_lines_being_trimmed(cx: &mut TestAppContext) {
        let (chat, cx) = chat_window(cx, TERMINAL_CAPACITY);
        cx.run_until_parked();
        chat.update(cx, |chat, cx| {
            chat.scroll_back(LINE_HEIGHT * 100., cx);
            push(chat, 10, cx);
        });
        chat.read_with(cx, |chat, _| {
            assert_eq!(chat.new_lines(chat.history.as_ref().unwrap()), 10);
        });
    }

    #[gpui::test]
    fn dragging_the_divider_resizes_the_panes_and_a_new_split_remembers(cx: &mut TestAppContext) {
        let (chat, cx) = split_window(cx, LINE_HEIGHT * 100.);
        let height = cx.update(|window, _| window.viewport_size().height);
        let drag_divider_to = |y: Pixels, cx: &mut VisualTestContext| {
            // To the side of the chip, which sits on the divider's middle.
            let from = cx.debug_bounds("chat-divider").unwrap().origin + point(px(20.), px(4.));
            let to = point(from.x, y);
            cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
            // The first move past gpui's threshold only starts the drag.
            for _ in 0..2 {
                cx.simulate_mouse_move(to, MouseButton::Left, Modifiers::none());
            }
            cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
            cx.run_until_parked();
        };

        drag_divider_to(height / 2., cx);
        let live = cx.debug_bounds("chat-area").unwrap().size.height;
        assert_eq!(live, height / 2.);

        chat.update(cx, |chat, cx| chat.back_to_live(cx));
        cx.run_until_parked();
        chat.update(cx, |chat, cx| chat.scroll_back(LINE_HEIGHT * 100., cx));
        cx.run_until_parked();
        assert_eq!(cx.debug_bounds("chat-area").unwrap().size.height, live);

        drag_divider_to(px(0.), cx);
        assert_eq!(
            cx.debug_bounds("chat-area").unwrap().size.height,
            height * *LIVE_SHARES.end()
        );
    }

    #[gpui::test]
    fn widening_the_column_keeps_a_reader_near_the_newest_line(cx: &mut TestAppContext) {
        let (chat, cx) = chat_window(cx, 0);
        narrow_window(cx);
        chat.update(cx, |chat, cx| chat.apply(&long_lines(500), cx));
        cx.run_until_parked();
        // Five wrapped lines from the bottom: wider, they'd all fit the pane.
        chat.update(cx, |chat, cx| chat.open_history_at(495, cx));
        cx.run_until_parked();
        let reading = top_line(&chat, cx);

        cx.simulate_resize(size(px(1_600.), px(1_000.)));
        cx.run_until_parked();
        assert!(is_split(&chat, cx));
        assert_eq!(top_line(&chat, cx), reading);
    }

    #[gpui::test]
    fn find_can_open_the_pane_at_a_line(cx: &mut TestAppContext) {
        let (chat, cx) = chat_window(cx, 1_000);
        cx.run_until_parked();
        chat.update(cx, |chat, cx| chat.open_history_at(40, cx));
        cx.run_until_parked();
        assert_eq!(top_line(&chat, cx), "history line 40");

        chat.update(cx, |chat, cx| {
            push(chat, 2, cx);
            chat.open_history_at(7, cx);
        });
        cx.run_until_parked();
        assert_eq!(top_line(&chat, cx), "history line 7");
        chat.read_with(cx, |chat, _| {
            assert_eq!(chat.new_lines(chat.history.as_ref().unwrap()), 2);
        });
    }

    #[gpui::test]
    fn opening_the_pane_past_the_newest_line_opens_it_on_the_newest(cx: &mut TestAppContext) {
        let (chat, cx) = chat_window(cx, 1_000);
        cx.run_until_parked();
        chat.update(cx, |chat, cx| chat.open_history_at(5_000, cx));
        cx.run_until_parked();
        assert_eq!(top_line(&chat, cx), "history line 999");
    }

    #[gpui::test]
    fn opening_the_pane_on_an_empty_scrollback_leaves_nothing_open(cx: &mut TestAppContext) {
        let (chat, cx) = chat_window(cx, 0);
        cx.run_until_parked();
        chat.update(cx, |chat, cx| chat.open_history_at(5, cx));
        cx.run_until_parked();
        assert!(!is_split(&chat, cx));
    }

    #[gpui::test]
    fn opening_the_pane_on_a_trimmed_line_opens_it_on_the_oldest(cx: &mut TestAppContext) {
        let (chat, cx) = chat_window(cx, TERMINAL_CAPACITY);
        chat.update(cx, |chat, cx| {
            push(chat, 10, cx);
            chat.open_history_at(3, cx);
        });
        cx.run_until_parked();
        assert_eq!(top_line(&chat, cx), "history line 10");
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
            let reading = chat.text_of(chat.reading().top().line - 1);
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
            assert!(chat.top_line_height() > LINE_HEIGHT);
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
        let height = chat.read_with(cx, |chat, _| chat.top_line_height());

        narrow_window(cx);
        assert_eq!(top_line(&chat, cx), reading);
        assert!(chat.read_with(cx, |chat, _| chat.top_line_height()) > height);
    }

    #[gpui::test]
    fn lines_that_fit_keep_their_one_row(cx: &mut TestAppContext) {
        let (chat, cx) = chat_window(cx, 0);
        // Padded past the column's edge, as a server's table rows can be.
        let row = format!("| north | {:<400}", "gate |");
        chat.update(cx, |chat, cx| chat.apply(&server_lines([row]), cx));
        narrow_window(cx);
        assert_eq!(
            chat.read_with(cx, |chat, _| chat.top_line_height()),
            LINE_HEIGHT
        );
    }

    #[gpui::test]
    fn eighty_columns_take_one_row_and_a_longer_line_wraps(cx: &mut TestAppContext) {
        let (chat, cx) = chat_window(cx, 0);
        let column = cx.update(|_, cx| min_column_width(cx));
        // One row high below the top padding, so the newest line is on top.
        cx.simulate_resize(size(column, LINE_HEIGHT + px(16.)));
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
