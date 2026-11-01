//! Find in the scrollback: a bar at the head of the chat, whose results open in
//! the history pane and leave the live pane alone.
//!
//! The search runs off the UI thread on snapshots of the lines, which are
//! shared, so taking them copies no text. As lines arrive, only the new ones are
//! searched, a few times a second: a redraw for each line's result would double
//! the frames of a stream of lines.
//!
//! A result is a line's number, which trimming doesn't change. Results whose
//! line has been trimmed drop out as it goes, so what's listed can be opened.
//! Only what shows is searched, so a prompt the chat hides is never a result;
//! showing or hiding prompts searches again, and leaves the reader in place.
//!
//! Results count lines, however many rows a line wraps to. The first is the
//! newest, and Next walks back through the scrollback, up the screen.
//!
//! Accessibility: the field has a name, and the count is announced through a
//! polite live region as a search finishes or a result opens. Results that
//! arrive with new lines aren't announced: a busy session would never stop.

use super::{Chat, live_region};
use crate::assets::{Icon, UI_FONT};
use crate::input::CommandInput;
use crate::theme::Theme;
use crate::title_bar::icon_button;
use aho_corasick::AhoCorasick;
use futures::future::join_all;
use gpui::{
    Action, App, Context, Div, Entity, FocusHandle, Focusable, HighlightStyle, KeyBinding, Pixels,
    Role, SharedString, Subscription, Task, Window, actions, combine_highlights, div, prelude::*,
    px,
};
use mudling_core::Scrollback;
use std::collections::VecDeque;
use std::ops::Range;
use std::time::Duration;

actions!(find, [CloseFind, NextResult, PreviousResult]);

const CONTEXT: &str = "ChatFind";
const FIELD_NAME: &str = "Find in the scrollback";
const PLACEHOLDER: &str = "Find";
const FIELD_WIDTH: Pixels = px(240.);
const COUNT_WIDTH: Pixels = px(72.);
/// How long new lines wait to be searched, so they go in batches.
const NEW_LINES_WAIT: Duration = Duration::from_millis(250);
/// How many searches share the lines. A first search touches 100,000 lines gone
/// cold in memory, which on its own can miss its budget on a busy machine.
const SEARCHES: usize = 4;

/// Keys for the bar. The Return keys outrank the field's own for being bound
/// after them, and Esc outranks going back to live the same way.
pub fn bind_keys(cx: &mut App) {
    let in_the_field = format!("{CONTEXT} > CommandInput");
    let anywhere = format!("{} && {}", super::CONTEXT, super::FIND_OPEN);
    cx.bind_keys([
        KeyBinding::new("enter", NextResult, Some(&in_the_field)),
        KeyBinding::new("shift-enter", PreviousResult, Some(&in_the_field)),
        KeyBinding::new("escape", CloseFind, Some(&anywhere)),
    ]);
}

/// The lines numbered `first` on, in up to [`SEARCHES`] pieces, oldest first,
/// each with its first line's number, for a search to own. A clone of a line
/// only counts another reader of its text.
fn snapshots(lines: &Scrollback, first: u64) -> Vec<(u64, Scrollback)> {
    let from = (first - lines.evicted()) as usize;
    let piece_len = (lines.len() - from).div_ceil(SEARCHES).max(1);
    let mut rest = lines.iter().skip(from);
    let pieces = (0..SEARCHES as u64).map(|piece| {
        let mut snapshot = Scrollback::with_capacity(piece_len);
        for line in rest.by_ref().take(piece_len) {
            snapshot.push(line.clone());
        }
        (first + piece * piece_len as u64, snapshot)
    });
    pieces.filter(|(_, piece)| !piece.is_empty()).collect()
}

/// What a search is for, which says what becomes of the results before it.
#[derive(Clone, Copy)]
enum Search {
    /// New text: its results replace them, and open the newest.
    Typed,
    /// New lines: its results join them.
    NewLines,
    /// Prompts were shown or hidden: its results replace them, where the
    /// reader is.
    Redone,
}

/// What the last finished search found, kept up to date as lines arrive.
pub(super) struct Results {
    /// Matches as the core's search does, so what's highlighted is what's
    /// counted.
    matcher: AhoCorasick,
    /// The matching lines' numbers, oldest first.
    numbers: VecDeque<u64>,
    /// The result open in the history pane.
    current: Option<u64>,
    /// The number of the first line not searched yet.
    searched_to: u64,
}

impl Results {
    fn new(needle: &str, numbers: Vec<u64>, searched_to: u64) -> Results {
        let matcher = AhoCorasick::builder()
            .ascii_case_insensitive(true)
            .build([needle])
            .expect("a single literal pattern always builds");
        Results {
            matcher,
            numbers: numbers.into(),
            current: None,
            searched_to,
        }
    }

    /// Takes `numbers` in place of what's listed. A current result no longer
    /// among them gives way to the next one back, which comes back as what's
    /// to open.
    fn replace(&mut self, numbers: Vec<u64>, searched_to: u64) -> Option<u64> {
        self.numbers = numbers.into();
        self.searched_to = searched_to;
        let was = self.current?;
        let up_to = self.numbers.partition_point(|number| *number <= was);
        let kept = up_to.checked_sub(1).or(Some(0));
        self.current = kept.and_then(|at| self.numbers.get(at).copied());
        self.current.filter(|current| *current != was)
    }

    /// Lets go of the lines trimmed from the scrollback. A current result
    /// among them gives way to the oldest left, as the history pane does.
    fn drop_trimmed(&mut self, evicted: u64) {
        let trimmed = self.numbers.partition_point(|number| *number < evicted);
        self.numbers.drain(..trimmed);
        let oldest = self.numbers.front().copied();
        self.current = self.current.and_then(|current| Some(current.max(oldest?)));
    }

    /// The next result back, from the oldest round to the newest.
    fn next(&self) -> Option<u64> {
        let newest = self.numbers.back().copied();
        let Some(current) = self.current else {
            return newest;
        };
        let older = self.numbers.partition_point(|number| *number < current);
        let older = older.checked_sub(1);
        older
            .and_then(|at| self.numbers.get(at).copied())
            .or(newest)
    }

    /// The next result forward, from the newest round to the oldest.
    fn previous(&self) -> Option<u64> {
        let Some(current) = self.current else {
            return self.numbers.back().copied();
        };
        let newer = self.numbers.partition_point(|number| *number <= current);
        (self.numbers.get(newer).or(self.numbers.front())).copied()
    }

    /// How many results there are, in words.
    fn count(&self) -> SharedString {
        match self.numbers.len() {
            0 => "No results".into(),
            1 => "1 result".into(),
            count => format!("{count} results").into(),
        }
    }

    /// What the bar says: which result is open, counting back from the newest.
    fn summary(&self) -> SharedString {
        let total = self.numbers.len();
        match self.current.filter(|_| total > 0) {
            Some(current) => {
                let older = self.numbers.partition_point(|number| *number < current);
                format!("{} of {total}", total - older).into()
            }
            None => self.count(),
        }
    }

    /// Where `line`, numbered `number`, matches, up to the `drawn` bytes shown.
    pub(super) fn paint(
        &self,
        line: &str,
        drawn: usize,
        number: u64,
        theme: &Theme,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        let style = if self.current == Some(number) {
            HighlightStyle {
                color: Some(theme.on_accent),
                background_color: Some(theme.accent),
                ..HighlightStyle::default()
            }
        } else {
            HighlightStyle {
                background_color: Some(theme.selected),
                ..HighlightStyle::default()
            }
        };
        let matches = self.matcher.find_iter(line);
        let shown = matches.map(|found| found.start()..found.end().min(drawn));
        shown
            .filter(|range| !range.is_empty())
            .map(|range| (range, style))
            .collect()
    }
}

/// `spans` with `matches` painted over them. A match's own colors win whatever
/// the server chose, or it wouldn't show on a line with a background.
pub(super) fn paint_matches(
    spans: Vec<(Range<usize>, HighlightStyle)>,
    matches: Vec<(Range<usize>, HighlightStyle)>,
) -> Vec<(Range<usize>, HighlightStyle)> {
    if matches.is_empty() {
        return spans;
    }
    let bare = matches
        .iter()
        .map(|(range, _)| (range.clone(), HighlightStyle::default()));
    // Every match's ends split the ranges, so a range is all in one or in none.
    combine_highlights(spans, bare.collect::<Vec<_>>())
        .map(|(range, style)| {
            let covering = matches
                .iter()
                .find(|(found, _)| found.start <= range.start && range.end <= found.end);
            let style = covering.map_or(style, |(_, paint)| HighlightStyle {
                color: paint.color.or(style.color),
                background_color: paint.background_color,
                ..style
            });
            (range, style)
        })
        .collect()
}

/// The open find bar.
pub(super) struct Find {
    input: Entity<CommandInput>,
    /// What's in the field. Results show until a search for it finishes.
    typed: String,
    pub(super) results: Option<Results>,
    /// The one search running.
    search: Option<Task<()>>,
    announcement: Option<(u64, SharedString)>,
    _typing: Subscription,
}

impl Find {
    fn new(cx: &mut Context<Chat>) -> Find {
        let input = cx.new(|cx| CommandInput::field(FIELD_NAME, PLACEHOLDER, cx));
        // The field tells nobody it changed, but every change redraws it.
        let typing = cx.observe(&input, |chat, input, cx| {
            chat.search_for(input.read(cx).content().to_owned(), cx);
        });
        Find {
            input,
            typed: String::new(),
            results: None,
            search: None,
            announcement: None,
            _typing: typing,
        }
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }

    /// Each announcement is a new batch: see [`live_region`].
    fn announce(&mut self, text: SharedString) {
        let batch = self.announcement.as_ref().map_or(0, |(batch, _)| batch + 1);
        self.announcement = Some((batch, text));
    }
}

impl Chat {
    /// Opens the bar, or goes back to it if it's open.
    pub fn open_find(&mut self, window: &mut Window, cx: &mut Context<Chat>) {
        let find = self.find.get_or_insert_with(|| Find::new(cx));
        window.focus(&find.focus_handle(cx), cx);
        cx.notify();
    }

    /// Closes the bar and what it found. The history pane stays where it is.
    pub fn close_find(&mut self, cx: &mut Context<Chat>) {
        self.find = None;
        cx.notify();
    }

    pub fn find_is_open(&self) -> bool {
        self.find.is_some()
    }

    /// How many lines the last finished search found.
    pub fn find_count(&self) -> Option<usize> {
        let results = self.find.as_ref()?.results.as_ref()?;
        Some(results.numbers.len())
    }

    /// Fills the field, as if the text had been typed.
    pub fn type_in_find(&mut self, text: &str, window: &mut Window, cx: &mut Context<Chat>) {
        if let Some(find) = &self.find {
            let input = find.input.clone();
            input.update(cx, |input, cx| {
                input.show(text.to_owned(), text.len(), window, cx);
            });
        }
    }

    fn search_for(&mut self, typed: String, cx: &mut Context<Chat>) {
        let Some(find) = &mut self.find else { return };
        if typed == find.typed {
            return;
        }
        find.typed = typed;
        find.search = None;
        if find.typed.is_empty() {
            find.results = None;
            cx.notify();
        } else {
            self.search_from(0, Search::Typed, cx);
        }
    }

    /// Prompts were shown or hidden, so what can be found changed.
    pub(super) fn search_again(&mut self, cx: &mut Context<Chat>) {
        let Some(find) = self.find.as_ref().filter(|find| !find.typed.is_empty()) else {
            return;
        };
        let search = match find.results {
            Some(_) => Search::Redone,
            None => Search::Typed,
        };
        self.search_from(0, search, cx);
    }

    /// Searches the lines numbered `from` on, off the UI thread.
    fn search_from(&mut self, from: u64, search: Search, cx: &mut Context<Chat>) {
        let pieces = snapshots(self.lines(), from.max(self.lines().evicted()));
        let end = self.next_line_number();
        let prompts_shown = self.prompts_shown;
        let Some(find) = &mut self.find else { return };
        let searches: Vec<_> = pieces
            .into_iter()
            .map(|(first, lines)| {
                let needle = find.typed.clone();
                cx.background_spawn(async move {
                    let indexes = lines.find(&needle, lines.len(), prompts_shown);
                    let numbers = indexes.into_iter().rev();
                    numbers
                        .map(|index| first + index as u64)
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        find.search = Some(cx.spawn(async move |chat, cx| {
            let numbers = join_all(searches).await.concat();
            chat.update(cx, |chat, cx| chat.found(numbers, end, search, cx))
                .ok();
        }));
    }

    /// Takes in a search's results, `numbers` oldest first, for the lines up
    /// to `end`. Results for new text open the newest of them, so what's typed
    /// shows at once, as in any find.
    fn found(&mut self, numbers: Vec<u64>, end: u64, search: Search, cx: &mut Context<Chat>) {
        let Some(find) = &mut self.find else { return };
        find.search = None;
        match (search, &mut find.results) {
            (Search::Typed, _) => {
                let results = Results::new(&find.typed, numbers, end);
                let count = results.count();
                find.results = Some(results);
                find.announce(count);
                self.step(Results::next, None, cx);
            }
            (Search::NewLines, Some(results)) => {
                if !numbers.is_empty() {
                    cx.notify();
                }
                results.numbers.extend(numbers);
                results.searched_to = end;
            }
            (Search::Redone, Some(results)) => {
                let to_open = results.replace(numbers, end);
                let count = results.count();
                find.announce(count);
                if let Some(number) = to_open {
                    self.open_history_at(number, cx);
                }
                cx.notify();
            }
            (_, None) => {}
        }
        self.keep_find_current(cx);
    }

    /// Brings the results up to date with the scrollback after a change.
    pub(super) fn keep_find_current(&mut self, cx: &mut Context<Chat>) {
        let evicted = self.lines().evicted();
        let next_line = self.next_line_number();
        let Some(find) = &mut self.find else { return };
        let Some(results) = &mut find.results else {
            return;
        };
        results.drop_trimmed(evicted);
        let searched_to = results.searched_to;
        if find.search.is_none() && searched_to < next_line {
            let wait = cx.background_executor().timer(NEW_LINES_WAIT);
            find.search = Some(cx.spawn(async move |chat, cx| {
                wait.await;
                chat.update(cx, |chat, cx| {
                    chat.search_from(searched_to, Search::NewLines, cx);
                })
                .ok();
            }));
        }
    }

    /// Opens the result `choose` picks, and announces it if there's a `prefix`
    /// to say it with.
    fn step(
        &mut self,
        choose: fn(&Results) -> Option<u64>,
        prefix: Option<&str>,
        cx: &mut Context<Chat>,
    ) {
        let Some(find) = &mut self.find else { return };
        let Some(results) = &mut find.results else {
            return;
        };
        results.current = choose(results);
        let number = results.current;
        let summary = results.summary();
        if let Some(prefix) = prefix.filter(|_| number.is_some()) {
            find.announce(format!("{prefix} {summary}").into());
        }
        if let Some(number) = number {
            self.open_history_at(number, cx);
        }
        cx.notify();
    }

    fn next_result(&mut self, cx: &mut Context<Chat>) {
        self.step(Results::next, Some("Result"), cx);
    }

    fn previous_result(&mut self, cx: &mut Context<Chat>) {
        self.step(Results::previous, Some("Result"), cx);
    }

    /// The bar, at the head of the chat so it covers no line: a result opens
    /// at the top of the history pane, right below it.
    pub(super) fn find_bar(&self, cx: &mut Context<Chat>) -> Option<Div> {
        let find = self.find.as_ref()?;
        let theme = *cx.global::<Theme>();
        let count = find.results.as_ref().map(Results::summary);
        let button = |id, label, icon| icon_button(id, label, icon, theme.text_muted, &theme);
        let bar = div()
            .id("find-bar")
            .debug_selector(|| "find-bar".to_owned())
            .key_context(CONTEXT)
            .role(Role::Group)
            .aria_label(FIELD_NAME)
            .flex()
            .items_center()
            .gap_1()
            .p_1()
            .rounded_xl()
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_raised)
            .shadow(theme.floating_shadow())
            .font_family(UI_FONT)
            .text_size(px(13.))
            .on_action(cx.listener(|chat, _: &NextResult, _, cx| chat.next_result(cx)))
            .on_action(cx.listener(|chat, _: &PreviousResult, _, cx| chat.previous_result(cx)))
            .child(div().w(FIELD_WIDTH).child(find.input.clone()))
            .child(
                div()
                    .id("find-count")
                    .debug_selector(|| "find-count".to_owned())
                    .role(Role::Label)
                    .aria_value(count.clone().unwrap_or_default())
                    .w(COUNT_WIDTH)
                    .flex()
                    .justify_end()
                    .whitespace_nowrap()
                    .text_size(px(12.))
                    .text_color(theme.text_muted)
                    .children(count),
            )
            .child(
                button("find-next", "Next result", Icon::ChevronUp)
                    .aria_keyshortcuts("Enter")
                    .on_click(cx.listener(|chat, _, _, cx| chat.next_result(cx))),
            )
            .child(
                button("find-previous", "Previous result", Icon::ChevronDown)
                    .aria_keyshortcuts("Shift+Enter")
                    .on_click(cx.listener(|chat, _, _, cx| chat.previous_result(cx))),
            )
            .child(
                button("find-close", "Close find", Icon::WindowClose)
                    .aria_keyshortcuts("Escape")
                    .on_click(|_, window, cx| window.dispatch_action(CloseFind.boxed_clone(), cx)),
            );
        Some(
            div()
                .relative()
                .flex_none()
                .w_full()
                .max_w(super::column_width(cx))
                .px_4()
                .pt_2()
                .flex()
                .justify_end()
                .child(bar)
                .children(
                    find.announcement
                        .clone()
                        .map(|(batch, text)| live_region("find-count-said", batch, text)),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{chat_window, is_split, push, server_lines, show_prompts};
    use super::*;
    use gpui::{Modifiers, TestAppContext, VisualTestContext, WindowAppearance};
    use mudling_core::Line;
    use mudling_core::store::{Diff, TERMINAL_CAPACITY, TerminalChange};

    fn results(numbers: &[u64]) -> Results {
        Results::new("x", numbers.to_vec(), 0)
    }

    #[test]
    fn snapshots_cover_the_lines_in_order_with_their_numbers() {
        let mut lines = Scrollback::with_capacity(10);
        (0..15).for_each(|n| lines.push(Line::command(&n.to_string())));

        let pieces = snapshots(&lines, 8);
        let starts: Vec<u64> = pieces.iter().map(|(first, _)| *first).collect();
        assert_eq!(starts, [8, 10, 12, 14]);
        let texts: Vec<&str> = pieces
            .iter()
            .flat_map(|(_, piece)| piece.iter().map(Line::plain))
            .collect();
        assert_eq!(texts, ["8", "9", "10", "11", "12", "13", "14"]);
        assert!(snapshots(&lines, 15).is_empty());
    }

    #[test]
    fn next_walks_back_and_round_and_previous_undoes_it() {
        let mut found = results(&[2, 5, 9]);
        let mut walked = Vec::new();
        for _ in 0..4 {
            found.current = found.next();
            walked.push(found.current.unwrap());
        }
        assert_eq!(walked, [9, 5, 2, 9]);

        found.current = found.previous();
        assert_eq!(found.current, Some(2));
        found.current = found.previous();
        assert_eq!(found.current, Some(5));
    }

    #[test]
    fn the_summary_counts_back_from_the_newest() {
        let mut found = results(&[2, 5, 9]);
        assert_eq!(found.summary(), "3 results");
        found.current = Some(9);
        assert_eq!(found.summary(), "1 of 3");
        found.current = Some(2);
        assert_eq!(found.summary(), "3 of 3");
        assert_eq!(results(&[]).summary(), "No results");
        assert_eq!(results(&[4]).summary(), "1 result");
    }

    #[test]
    fn a_trimmed_result_gives_way_to_the_oldest_left() {
        let mut found = results(&[2, 5, 9]);
        found.current = Some(2);
        found.drop_trimmed(3);
        assert_eq!(found.current, Some(5));
        assert_eq!(found.summary(), "2 of 2");

        found.drop_trimmed(10);
        assert_eq!(found.current, None);
        assert_eq!(found.summary(), "No results");
        assert_eq!(found.next(), None);
    }

    fn painted(needle: &str, line: &str, drawn: usize) -> Vec<Range<usize>> {
        let found = Results::new(needle, Vec::new(), 0);
        let theme = Theme::for_appearance(WindowAppearance::Dark);
        let paint = found.paint(line, drawn, 0, &theme).into_iter();
        paint.map(|(range, _)| range).collect()
    }

    #[test]
    fn highlights_ignore_ascii_case_and_stop_at_what_is_drawn() {
        assert_eq!(painted("gob", "A GOBlin, a gob", 15), [2..5, 12..15]);
        assert_eq!(painted("te ", "gate    ", 4), vec![2..4]);
        assert_eq!(painted(" ", "gate    ", 4), Vec::<Range<usize>>::new());
    }

    #[test]
    fn highlights_land_on_character_boundaries() {
        assert_eq!(
            painted("é", "Café, café", "Café, café".len()),
            [3..5, 10..12]
        );
    }

    #[test]
    fn a_match_splits_the_spans_under_it_and_shows_over_their_background() {
        let theme = Theme::for_appearance(WindowAppearance::Dark);
        let red = theme.error;
        let span = HighlightStyle {
            color: Some(red),
            background_color: Some(theme.warning),
            ..HighlightStyle::default()
        };
        let matched = HighlightStyle {
            background_color: Some(theme.selected),
            ..HighlightStyle::default()
        };
        let painted = paint_matches(vec![(0..6, span)], vec![(2..4, matched)]);
        assert_eq!(
            painted,
            [
                (0..2, span),
                (
                    2..4,
                    HighlightStyle {
                        background_color: Some(theme.selected),
                        ..span
                    }
                ),
                (4..6, span),
            ]
        );
    }

    /// A chat with `goblin` on every tenth of 1,000 lines, and the bar open.
    fn find_window(cx: &mut TestAppContext) -> (Entity<Chat>, &mut VisualTestContext) {
        let (chat, cx) = chat_window(cx, 0);
        let lines = (0..1_000).map(|n| match n % 10 {
            0 => format!("a goblin {n}"),
            _ => format!("line {n}"),
        });
        chat.update(cx, |chat, cx| chat.apply(&server_lines(lines), cx));
        open(&chat, cx);
        (chat, cx)
    }

    fn open(chat: &Entity<Chat>, cx: &mut VisualTestContext) {
        chat.update_in(cx, |chat, window, cx| chat.open_find(window, cx));
        cx.run_until_parked();
    }

    fn search(chat: &Entity<Chat>, text: &str, cx: &mut VisualTestContext) {
        chat.update_in(cx, |chat, window, cx| chat.type_in_find(text, window, cx));
        cx.run_until_parked();
    }

    /// Lets the waiting search run, and the one for lines it missed.
    fn search_new_lines(cx: &mut VisualTestContext) {
        for _ in 0..2 {
            cx.executor().advance_clock(NEW_LINES_WAIT);
            cx.run_until_parked();
        }
    }

    fn summary(chat: &Entity<Chat>, cx: &mut VisualTestContext) -> String {
        chat.read_with(cx, |chat, _| {
            let results = chat.find.as_ref().and_then(|find| find.results.as_ref());
            results.map_or_else(String::new, |results| results.summary().to_string())
        })
    }

    fn top(chat: &Entity<Chat>, cx: &mut VisualTestContext) -> u64 {
        chat.read_with(cx, |chat, _| chat.scroll_offset().line)
    }

    /// Output, output after a prompt, then a prompt with a word glued after
    /// it, then a prompt alone, each of the last three marked. The two
    /// prompts hold `goblin`, and so does what shows of the first two lines.
    fn window_with_prompts(cx: &mut TestAppContext) -> (Entity<Chat>, &mut VisualTestContext) {
        let (chat, cx) = chat_window(cx, 0);
        let texts = [
            ("a goblin", 0),
            ("<1hp> a goblin", 6),
            ("<goblin> a bat", 9),
            ("<goblin> ", 9),
        ];
        let Diff { change, revision } = server_lines(texts.map(|(text, _)| text.to_owned()));
        let TerminalChange::Append(lines) = change;
        let marked = lines
            .iter()
            .zip(texts)
            .map(|(line, (_, prompt))| match prompt {
                0 => line.clone(),
                prompt => line.with_prompt(prompt),
            });
        let change = TerminalChange::Append(marked.collect());
        chat.update(cx, |chat, cx| chat.apply(&Diff { change, revision }, cx));
        open(&chat, cx);
        (chat, cx)
    }

    #[gpui::test]
    fn a_hidden_prompt_is_never_a_result_or_counted(cx: &mut TestAppContext) {
        let (chat, cx) = window_with_prompts(cx);
        search(&chat, "goblin", cx);
        assert_eq!(summary(&chat, cx), "1 of 2");
        assert_eq!(top(&chat, cx), 1, "the newest line that shows");
        assert_eq!(chat.read_with(cx, |chat, _| chat.find_count()), Some(2));

        search(&chat, "hp", cx);
        assert_eq!(summary(&chat, cx), "No results");
    }

    #[gpui::test]
    fn showing_prompts_finds_them_and_leaves_the_reader_in_place(cx: &mut TestAppContext) {
        let (chat, cx) = window_with_prompts(cx);
        search(&chat, "goblin", cx);

        show_prompts(true, cx);
        assert_eq!(summary(&chat, cx), "3 of 4", "still on line 1");
        assert_eq!(top(&chat, cx), 1);
    }

    #[gpui::test]
    fn hiding_prompts_moves_a_result_on_a_prompt_to_one_that_shows(cx: &mut TestAppContext) {
        let (chat, cx) = window_with_prompts(cx);
        show_prompts(true, cx);
        search(&chat, "goblin", cx);
        assert_eq!(summary(&chat, cx), "1 of 4");
        assert_eq!(top(&chat, cx), 3);

        show_prompts(false, cx);
        assert_eq!(summary(&chat, cx), "1 of 2");
        assert_eq!(top(&chat, cx), 1);
    }

    #[gpui::test]
    fn new_lines_are_searched_as_the_setting_stands(cx: &mut TestAppContext) {
        let (chat, cx) = window_with_prompts(cx);
        search(&chat, "goblin", cx);
        let Diff { change, revision } = server_lines(["<goblin> ".to_owned()]);
        let TerminalChange::Append(lines) = change;
        let change = TerminalChange::Append(vec![lines[0].with_prompt(9)]);
        chat.update(cx, |chat, cx| chat.apply(&Diff { change, revision }, cx));
        search_new_lines(cx);
        assert_eq!(chat.read_with(cx, |chat, _| chat.find_count()), Some(2));
    }

    fn announced(chat: &Entity<Chat>, cx: &mut VisualTestContext) -> Option<SharedString> {
        chat.read_with(cx, |chat, _| {
            let (_, text) = chat.find.as_ref()?.announcement.as_ref()?;
            Some(text.clone())
        })
    }

    #[gpui::test]
    fn typing_finds_the_lines_and_opens_the_newest_in_the_history_pane(cx: &mut TestAppContext) {
        let (chat, cx) = find_window(cx);

        cx.simulate_input("GOBLIN");
        cx.run_until_parked();
        assert_eq!(summary(&chat, cx), "1 of 100");
        assert_eq!(top(&chat, cx), 990);
        assert!(chat.read_with(cx, |chat, _| chat.live.is_following()));
        assert_eq!(announced(&chat, cx).as_deref(), Some("100 results"));
    }

    #[gpui::test]
    fn next_and_previous_open_results_and_are_announced(cx: &mut TestAppContext) {
        let (chat, cx) = find_window(cx);
        search(&chat, "goblin", cx);
        let live = chat.read_with(cx, |chat, _| chat.live.top());

        let next = cx.debug_bounds("find-next").unwrap();
        cx.simulate_click(next.center(), Modifiers::none());
        cx.run_until_parked();
        assert_eq!(top(&chat, cx), 980);
        assert_eq!(chat.read_with(cx, |chat, _| chat.live.top()), live);
        assert_eq!(summary(&chat, cx), "2 of 100");
        assert_eq!(announced(&chat, cx).as_deref(), Some("Result 2 of 100"));

        let previous = cx.debug_bounds("find-previous").unwrap();
        cx.simulate_click(previous.center(), Modifiers::none());
        cx.run_until_parked();
        assert_eq!(top(&chat, cx), 990);
    }

    #[gpui::test]
    fn finding_nothing_leaves_the_chat_alone(cx: &mut TestAppContext) {
        let (chat, cx) = find_window(cx);
        search(&chat, "dragon", cx);
        assert_eq!(summary(&chat, cx), "No results");
        assert_eq!(announced(&chat, cx).as_deref(), Some("No results"));
        assert!(!is_split(&chat, cx));

        chat.update(cx, |chat, cx| chat.next_result(cx));
        cx.run_until_parked();
        assert!(!is_split(&chat, cx));
    }

    #[gpui::test]
    fn clearing_the_field_clears_the_results(cx: &mut TestAppContext) {
        let (chat, cx) = find_window(cx);
        search(&chat, "goblin", cx);
        search(&chat, "", cx);
        assert_eq!(summary(&chat, cx), "");
    }

    #[gpui::test]
    fn new_lines_join_the_results_unannounced_and_keep_the_reader_in_place(
        cx: &mut TestAppContext,
    ) {
        let (chat, cx) = find_window(cx);
        search(&chat, "goblin", cx);
        chat.update(cx, |chat, cx| {
            chat.apply(&server_lines(["another goblin".to_owned()]), cx);
            chat.echo_unsent("kill goblin", cx);
        });
        search_new_lines(cx);

        assert_eq!(top(&chat, cx), 990);
        assert_eq!(summary(&chat, cx), "3 of 102");
        assert_eq!(announced(&chat, cx).as_deref(), Some("100 results"));
    }

    #[gpui::test]
    fn lines_arriving_during_a_search_are_searched_after_it(cx: &mut TestAppContext) {
        let (chat, cx) = find_window(cx);
        chat.update_in(cx, |chat, window, cx| {
            chat.type_in_find("new", window, cx);
        });
        chat.update(cx, |chat, cx| push(chat, 3, cx));
        cx.run_until_parked();
        assert_eq!(summary(&chat, cx), "No results");
        search_new_lines(cx);
        assert_eq!(summary(&chat, cx), "3 results");
        assert!(!is_split(&chat, cx), "nothing was found when it finished");
    }

    #[gpui::test]
    fn a_result_trimmed_before_it_is_chosen_is_skipped(cx: &mut TestAppContext) {
        let (chat, cx) = chat_window(cx, 0);
        let lines = (0..TERMINAL_CAPACITY).map(|n| match n {
            0 | 5 => format!("goblin {n}"),
            _ => format!("line {n}"),
        });
        chat.update(cx, |chat, cx| chat.apply(&server_lines(lines), cx));
        open(&chat, cx);
        search(&chat, "goblin", cx);
        assert_eq!(summary(&chat, cx), "1 of 2");
        assert_eq!(top(&chat, cx), 5);

        // Line 0 goes, and the next result round is the line still there.
        chat.update(cx, |chat, cx| push(chat, 1, cx));
        cx.run_until_parked();
        assert_eq!(summary(&chat, cx), "1 of 1");
        chat.update(cx, |chat, cx| chat.next_result(cx));
        cx.run_until_parked();
        assert_eq!(top(&chat, cx), 5);

        // Then the last one goes too, while it's open.
        chat.update(cx, |chat, cx| push(chat, 5, cx));
        cx.run_until_parked();
        assert_eq!(summary(&chat, cx), "No results");
        chat.update(cx, |chat, cx| chat.next_result(cx));
        cx.run_until_parked();
        assert!(is_split(&chat, cx));
    }

    #[gpui::test]
    fn results_found_in_a_trimmed_snapshot_are_dropped_as_they_arrive(cx: &mut TestAppContext) {
        let (chat, cx) = chat_window(cx, 0);
        let lines = (0..TERMINAL_CAPACITY).map(|n| format!("goblin {n}"));
        chat.update(cx, |chat, cx| chat.apply(&server_lines(lines), cx));
        open(&chat, cx);
        // The search has its snapshot before these lines trim the first ten.
        chat.update_in(cx, |chat, window, cx| {
            chat.type_in_find("goblin", window, cx);
        });
        chat.update(cx, |chat, cx| push(chat, 10, cx));
        cx.run_until_parked();
        assert_eq!(
            summary(&chat, cx),
            format!("1 of {}", TERMINAL_CAPACITY - 10)
        );
    }

    #[gpui::test]
    fn the_bar_leaves_the_top_line_of_the_history_pane_uncovered(cx: &mut TestAppContext) {
        let (chat, cx) = find_window(cx);
        search(&chat, "goblin", cx);
        let bar = cx.debug_bounds("find-bar").unwrap();
        let history = cx.debug_bounds("chat-history").unwrap();
        assert!(bar.bottom() <= history.top(), "{bar:?} covers {history:?}");
    }
}
