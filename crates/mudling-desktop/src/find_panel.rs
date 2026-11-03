//! The Find panel (PRD §8): the left panel's search across rooms, mobs and
//! items, nearest first, each with where it is and Walk.
//!
//! The panel holds its own copies of the stores it reads and draws only the
//! last [`FindResults`]. Searching and working out a walk both run off the UI
//! thread, one search at a time, so typing never waits for either. A search
//! starts from `render`, which only runs while the panel shows: a map that
//! changes behind the Map tab costs nothing here.
//!
//! Only the rows in view are laid out, or each search would lay out them all.
//! Rows out of view aren't tab stops, so Tab moves between rows itself.
//!
//! The panel never sends a command itself: it emits [`SendCommand`] and the
//! shell sends it, refusing it while the server asks for a password.

use crate::chat::live_region;
use crate::input::CommandInput;
use crate::room_panel::{SendCommand, button, row_name, section_label, unavailable};
use crate::shell::FocusNext;
use crate::theme::Theme;
use crate::title_bar::FocusPreviousControl;
use gpui::{
    App, ClickEvent, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    Pixels, Role, ScrollStrategy, SharedString, Stateful, Subscription, Task,
    UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};
use mudling_core::map::{
    FindCache, FindFilter, FindKind, FindPlace, FindResults, FindRow, FindStores, MOST_RESULTS,
    Mark, Reach, RoomId, speedwalk,
};
use mudling_core::store::Batch;
use std::ops::Range;

/// The key context while focus is in the panel.
pub const CONTEXT: &str = "FindPanel";
const FIELD_NAME: &str = "Find on the map";
const PLACEHOLDER: &str = "Find a room, mob or item";
const BUTTON_HEIGHT: Pixels = px(28.);
const FILTER_HEIGHT: Pixels = px(30.);
const ROW_HEIGHT: Pixels = px(56.);
const LOCKED_NOTE: &str = "Walk is off while the game asks for a password.";
const LOST_NOTE: &str = "Walk needs the map to know where you are.";
/// What the Items filter can't see yet: rooms keep no memory of what lies in
/// them.
const ITEMS_NOTE: &str = "Items lists what you wear and carry, gear you've learned about, what's \
                          on the ground here, and rooms noted for gear or a fountain. Items left \
                          in other rooms aren't tracked yet.";

/// The key context of the panel's field.
pub fn field_context() -> String {
    format!("{CONTEXT} > CommandInput")
}

fn filter_name(filter: FindFilter) -> &'static str {
    match filter {
        FindFilter::All => "All",
        FindFilter::Rooms => "Rooms",
        FindFilter::Mobs => "Mobs",
        FindFilter::Items => "Items",
    }
}

/// Why the list is empty. With [`FindFilter::Items`], [`ITEMS_NOTE`] says it.
fn empty_line(filter: FindFilter, query: &str, results: &FindResults) -> Option<String> {
    if !results.rows.is_empty() {
        return None;
    }
    if results.known > 0 {
        return Some(format!("Nothing on your map matches \"{}\".", query.trim()));
    }
    match filter {
        FindFilter::All => {
            Some("Nothing to find yet. Rooms and mobs show up here as you explore.".to_owned())
        }
        FindFilter::Rooms => Some("No rooms on your map yet. Explore to add them.".to_owned()),
        FindFilter::Mobs => Some("No mobs seen yet. Those you meet show up here.".to_owned()),
        FindFilter::Items => None,
    }
}

/// How far, in words.
fn reach_words(reach: Reach) -> Option<String> {
    match reach {
        Reach::Here => Some("You're here".to_owned()),
        Reach::Steps(1) => Some("1 step away".to_owned()),
        Reach::Steps(steps) => Some(format!("{steps} steps away")),
        Reach::NoPath => Some("No known path from here".to_owned()),
        Reach::PlayerLost | Reach::NotARoom => None,
    }
}

/// The row's second line: where it is, then how far.
fn where_line(row: &FindRow) -> String {
    let place = match &row.place {
        FindPlace::Room { area, .. } if row.kind == FindKind::Room => area.clone(),
        FindPlace::Room { name, area, .. } if area.is_empty() => name.clone(),
        FindPlace::Room { name, area, .. } => format!("{name} · {area}"),
        FindPlace::Carried => "Carried".to_owned(),
        FindPlace::Worn => "Worn".to_owned(),
        FindPlace::Known => "Known gear, not with you".to_owned(),
        FindPlace::Unseen => "Not seen on your map yet".to_owned(),
    };
    match reach_words(row.reach) {
        Some(reach) if place.is_empty() => reach,
        Some(reach) => format!("{place} · {reach}"),
        None => place,
    }
}

fn shown_name(row: &FindRow) -> SharedString {
    match row.kind {
        FindKind::Note => format!("Note: {}", row.name).into(),
        _ => row.name.clone().into(),
    }
}

/// What a screen reader reads for a name and its marks, whose own text is
/// out of its reach.
pub fn marked_label(name: &str, marks: &[Mark]) -> String {
    let marks = marks.iter().map(|mark| mark.name());
    let parts: Vec<&str> = [name].into_iter().chain(marks).collect();
    parts.join(", ")
}

/// What a screen reader reads for a row.
fn row_label(name: &str, marks: &[Mark], place: &str) -> String {
    format!("{}, {place}", marked_label(name, marks))
}

/// The room Walk goes to, unless there's nothing to walk.
fn goal(row: &FindRow) -> Option<RoomId> {
    match (&row.place, row.reach) {
        (FindPlace::Room { id, .. }, Reach::Steps(_)) => Some(*id),
        _ => None,
    }
}

/// Why Walk does nothing, for a screen reader.
fn no_walk(reach: Reach) -> &'static str {
    match reach {
        Reach::Here => "You're already here",
        Reach::NoPath => "No explored path leads there",
        Reach::PlayerLost => LOST_NOTE,
        Reach::NotARoom | Reach::Steps(_) => "It isn't in a room you can walk to",
    }
}

/// What a search was for: when this differs, the results are out of date.
#[derive(Clone, PartialEq)]
struct Asked {
    query: String,
    filter: FindFilter,
    stores: u64,
}

impl EventEmitter<SendCommand> for FindPanel {}

pub struct FindPanel {
    focus_handle: FocusHandle,
    input: Entity<CommandInput>,
    stores: FindStores,
    /// Bumps with each change to `stores`.
    stores_version: u64,
    filter: FindFilter,
    /// Away with the search running.
    cache: Option<FindCache>,
    /// The last search started.
    asked: Option<Asked>,
    /// What the rows show, and what they were found for. `None` until the
    /// first search is back.
    results: Option<(Asked, FindResults)>,
    search: Option<Task<()>>,
    /// The walk being worked out.
    walk: Option<Task<()>>,
    /// The server turned echo off.
    locked: bool,
    announcement: Option<(u64, SharedString)>,
    /// A handle for each row's Walk, so Tab can land on a row out of view.
    walks: Vec<FocusHandle>,
    scroll: UniformListScrollHandle,
    _typing: Subscription,
}

impl FindPanel {
    pub fn new(stores: FindStores, cx: &mut Context<FindPanel>) -> FindPanel {
        let input = cx.new(|cx| CommandInput::field(FIELD_NAME, PLACEHOLDER, cx));
        // The field tells nobody it changed, but every change redraws it.
        let typing = cx.observe(&input, |_, _, cx| cx.notify());
        FindPanel {
            focus_handle: cx.focus_handle(),
            input,
            stores,
            stores_version: 0,
            filter: FindFilter::default(),
            cache: Some(FindCache::default()),
            asked: None,
            results: None,
            search: None,
            walk: None,
            locked: false,
            announcement: None,
            walks: (0..MOST_RESULTS)
                .map(|_| cx.focus_handle().tab_stop(true))
                .collect(),
            scroll: UniformListScrollHandle::new(),
            _typing: typing,
        }
    }

    /// Starts over on another session's stores, whose revisions say nothing
    /// about what's cached from this one's. What's typed stays.
    pub fn start_over(&mut self, stores: FindStores, cx: &mut Context<FindPanel>) {
        self.stores = stores;
        self.stores_version += 1;
        self.cache = Some(FindCache::default());
        self.search = None;
        self.walk = None;
        self.results = None;
        self.locked = false;
        cx.notify();
    }

    /// Takes a frame's diffs.
    pub fn apply(&mut self, batch: &Batch, cx: &mut Context<FindPanel>) {
        if self.stores.apply(batch) {
            self.stores_version += 1;
            cx.notify();
        }
    }

    /// Shows that Walk is off, as the hotbar does its chips.
    pub fn set_locked(&mut self, locked: bool, cx: &mut Context<FindPanel>) {
        if self.locked != locked {
            self.locked = locked;
            cx.notify();
        }
    }

    pub fn choose(&mut self, filter: FindFilter, cx: &mut Context<FindPanel>) {
        self.filter = filter;
        cx.notify();
    }

    /// Moves the keyboard to the field.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<FindPanel>) {
        window.focus(&self.input.focus_handle(cx), cx);
    }

    /// Fills the field, as if the text had been typed.
    pub fn type_text(&self, text: &str, window: &mut Window, cx: &mut Context<FindPanel>) {
        self.input.update(cx, |input, cx| {
            input.show(text.to_owned(), text.len(), window, cx);
        });
    }

    /// How many rows are showing, once the search for what's typed is back.
    pub fn rows_for_typed(&self, cx: &App) -> Option<usize> {
        let (asked, results) = self.results.as_ref()?;
        (asked.query == self.input.read(cx).content()).then_some(results.rows.len())
    }

    fn wanted(&self, cx: &App) -> Asked {
        Asked {
            query: self.input.read(cx).content().to_owned(),
            filter: self.filter,
            stores: self.stores_version,
        }
    }

    /// Searches if the rows are out of date and no search is running. The
    /// running one comes back here when it's done.
    fn search_if_stale(&mut self, cx: &mut Context<FindPanel>) {
        let wanted = self.wanted(cx);
        if self.asked.as_ref() == Some(&wanted) {
            return;
        }
        let Some(mut cache) = self.cache.take() else {
            return;
        };
        self.asked = Some(wanted.clone());
        let stores = self.stores.clone();
        let found = cx.background_spawn(async move {
            let results = cache.search(&stores, &wanted.query, wanted.filter);
            (cache, wanted, results)
        });
        self.search = Some(cx.spawn(async move |panel, cx| {
            let (cache, asked, results) = found.await;
            panel
                .update(cx, |panel, cx| {
                    panel.cache = Some(cache);
                    panel.show(asked, results, cx);
                })
                .ok();
        }));
    }

    fn show(&mut self, asked: Asked, results: FindResults, cx: &mut Context<FindPanel>) {
        let last = self.results.as_ref();
        let retyped = last.is_none_or(|(last, _)| {
            (last.query.as_str(), last.filter) != (asked.query.as_str(), asked.filter)
        });
        if retyped {
            let text = match results.matched {
                1 => "1 result".to_owned(),
                count => format!("{count} results"),
            };
            let batch = self.announcement.as_ref().map_or(0, |(batch, _)| batch + 1);
            self.announcement = Some((batch, text.into()));
            self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        }
        self.results = Some((asked, results));
        cx.notify();
    }

    /// Works out the moves from where the player is by now, then sends them.
    fn walk_to(&mut self, goal: RoomId, by_keyboard: bool, cx: &mut Context<FindPanel>) {
        let Some(start) = self.stores.walk_start().filter(|_| !self.locked) else {
            return;
        };
        let map = self.stores.map().clone();
        let route = cx.background_spawn(async move { map.route(start, goal) });
        self.walk = Some(cx.spawn(async move |panel, cx| {
            let Some(steps) = route.await.filter(|steps| !steps.is_empty()) else {
                return;
            };
            panel
                .update(cx, |_, cx| {
                    cx.emit(SendCommand {
                        command: speedwalk(&steps),
                        by_keyboard,
                    });
                })
                .ok();
        }));
    }

    /// The row whose Walk has the keyboard.
    fn focused_row(&self, window: &Window) -> Option<usize> {
        self.walks.iter().position(|walk| walk.is_focused(window))
    }

    fn row_count(&self) -> usize {
        self.results.as_ref().map_or(0, |(_, r)| r.rows.len())
    }

    /// Moves the keyboard to `row`'s Walk, scrolling it into view.
    fn focus_row(&mut self, row: usize, window: &mut Window, cx: &mut Context<FindPanel>) {
        self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        window.focus(&self.walks[row], cx);
        cx.notify();
    }

    fn tab(&mut self, window: &mut Window, cx: &mut Context<FindPanel>) {
        match self.focused_row(window).map(|row| row + 1) {
            Some(next) if next < self.row_count() => self.focus_row(next, window, cx),
            _ => window.focus_next(cx),
        }
    }

    fn shift_tab(&mut self, window: &mut Window, cx: &mut Context<FindPanel>) {
        match self.focused_row(window).and_then(|row| row.checked_sub(1)) {
            Some(previous) => self.focus_row(previous, window, cx),
            None => window.focus_prev(cx),
        }
    }

    fn render_filters(&self, theme: &Theme, cx: &mut Context<FindPanel>) -> impl IntoElement {
        let tabs = FindFilter::ALL.map(|filter| {
            let selected = filter == self.filter;
            let name = filter_name(filter);
            button(("find-filter", filter as usize), name.into(), theme)
                .role(Role::Tab)
                .aria_selected(selected)
                .flex_grow(1.)
                .h(FILTER_HEIGHT)
                .border_color(theme.segment)
                .text_size(px(12.))
                .text_color(theme.text_muted)
                .when(selected, |tab| {
                    tab.bg(theme.selected)
                        .text_color(theme.text)
                        .font_weight(FontWeight::MEDIUM)
                })
                .on_click(cx.listener(move |panel, _, _, cx| panel.choose(filter, cx)))
                .child(name)
        });
        div()
            .id("find-filters")
            .role(Role::TabList)
            .aria_label("Find in")
            .flex()
            .flex_none()
            .p(px(3.))
            .rounded_lg()
            .bg(theme.segment)
            .border_1()
            .border_color(theme.border)
            // The mouse never moves focus here: typing stays in the field.
            .capture_any_mouse_down(|_, window, _| window.prevent_default())
            .children(tabs)
    }

    fn render_row(
        &self,
        index: usize,
        row: &FindRow,
        theme: &Theme,
        cx: &mut Context<FindPanel>,
    ) -> Stateful<Div> {
        let name = shown_name(row);
        let place = where_line(row);
        let goal = goal(row).filter(|_| !self.locked);
        let marks = row.marks.iter().map(|mark| {
            let color = match mark {
                Mark::Aggressive => theme.danger,
                Mark::Gear | Mark::Important => theme.text_muted,
            };
            div()
                .flex_none()
                .text_size(px(12.))
                .text_color(color)
                .child(mark.name())
        });
        let walk = button(
            ("find-walk", index),
            format!("Walk to {name}").into(),
            theme,
        )
        .track_focus(&self.walks[index])
        .h(BUTTON_HEIGHT)
        .px_3()
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.accent)
        .child("Walk")
        .map(|walk| match goal {
            Some(goal) => walk.on_click(cx.listener(move |panel, event, _, cx| {
                let by_keyboard = ClickEvent::is_keyboard(event);
                panel.walk_to(goal, by_keyboard, cx);
            })),
            None if self.locked => unavailable(walk, LOCKED_NOTE),
            None => unavailable(walk, no_walk(row.reach)),
        });
        div()
            .id(("find-row", index))
            .debug_selector(move || format!("find-row-{index}"))
            .role(Role::ListItem)
            .aria_label(row_label(&name, &row.marks, &place))
            .flex()
            .flex_none()
            .w_full()
            .h(ROW_HEIGHT)
            .items_center()
            .gap_2()
            .px_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(row_name(name))
                            .children(marks.collect::<Vec<_>>()),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(theme.text_muted)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(place),
                    ),
            )
            .child(walk)
    }

    /// Lines under the list that say what it leaves out, and why.
    fn notes(&self, query: &str, results: &FindResults) -> Vec<String> {
        let mut notes: Vec<String> = empty_line(self.filter, query, results)
            .into_iter()
            .collect();
        if results.matched > results.rows.len() {
            notes.push(format!(
                "The nearest {} of {}. Type more to narrow it down.",
                results.rows.len(),
                results.matched
            ));
        }
        if self.filter == FindFilter::Items {
            notes.push(ITEMS_NOTE.to_owned());
        }
        if self.locked {
            notes.push(LOCKED_NOTE.to_owned());
        } else if !results.rows.is_empty() && self.stores.walk_start().is_none() {
            notes.push(LOST_NOTE.to_owned());
        }
        notes
    }

    fn render_results(&self, theme: &Theme, cx: &mut Context<FindPanel>) -> Stateful<Div> {
        let Some((asked, results)) = &self.results else {
            return div()
                .id("find-searching")
                .role(Role::Status)
                .aria_label("Searching")
                .text_size(px(12.))
                .text_color(theme.text_muted)
                .child("Searching…");
        };
        let rows = cx.processor(|panel, range: Range<usize>, _, cx| {
            let theme = *cx.global::<Theme>();
            let Some((_, results)) = &panel.results else {
                return Vec::new();
            };
            let end = range.end.min(results.rows.len());
            let in_view = results.rows[range.start..end].iter().enumerate();
            in_view
                .map(|(offset, row)| panel.render_row(range.start + offset, row, &theme, cx))
                .collect()
        });
        let row_count = results.rows.len();
        let label = format!("RESULTS · {}", results.matched);
        let notes = self.notes(&asked.query, results);
        div()
            .id("find-results")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_2()
            .when(row_count > 0, |results| {
                results.child(section_label(&label, theme)).child(
                    div()
                        .id("find-list")
                        .debug_selector(|| "find-list".to_owned())
                        .role(Role::List)
                        .aria_label("Results")
                        .flex_1()
                        .min_h_0()
                        // The mouse never moves focus here either.
                        .capture_any_mouse_down(|_, window, _| window.prevent_default())
                        .child(
                            uniform_list("find-rows", row_count, rows)
                                .track_scroll(&self.scroll)
                                .size_full(),
                        ),
                )
            })
            .children(notes.into_iter().enumerate().map(|(index, note)| {
                div()
                    .id(("find-note", index))
                    .debug_selector(move || format!("find-note-{index}"))
                    .role(Role::Label)
                    .aria_value(note.clone())
                    .flex_none()
                    .text_size(px(12.))
                    .text_color(theme.text_muted)
                    .child(note)
            }))
    }
}

impl Render for FindPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<FindPanel>) -> impl IntoElement {
        self.search_if_stale(cx);
        let theme = *cx.global::<Theme>();
        div()
            .id("find-panel")
            .debug_selector(|| "find-panel".to_owned())
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .role(Role::Group)
            .aria_label("Find")
            .on_action(cx.listener(|panel, _: &FocusNext, window, cx| panel.tab(window, cx)))
            .on_action(cx.listener(|panel, _: &FocusPreviousControl, window, cx| {
                panel.shift_tab(window, cx);
            }))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .pt_2()
            .text_size(px(14.))
            .text_color(theme.text)
            .child(self.input.clone())
            .child(self.render_filters(&theme, cx))
            .child(self.render_results(&theme, cx))
            .children(
                self.announcement
                    .clone()
                    .map(|(batch, text)| live_region("find-panel-said", batch, text)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measure::sample_find;
    use crate::room_panel::press_key;
    use gpui::{Modifiers, TestAppContext, VisualTestContext, WindowAppearance};
    use mudling_core::map::{LiveMap, MapChange, PlainRules, Whereabouts};
    use mudling_core::persist::Timestamp;
    use mudling_core::store::{Diff, Revision, Versioned};
    use std::cell::RefCell;
    use std::rc::Rc;

    type Sent = Rc<RefCell<Vec<(String, bool)>>>;

    fn panel_window(
        stores: FindStores,
        cx: &mut TestAppContext,
    ) -> (Entity<FindPanel>, &mut VisualTestContext, Sent) {
        cx.update(|cx| {
            crate::bind_keys(cx);
            cx.set_global(Theme::for_appearance(WindowAppearance::Dark));
        });
        let sent = Sent::default();
        let log = sent.clone();
        let (panel, cx) = cx.add_window_view(|_, cx| FindPanel::new(stores, cx));
        cx.update(|_, cx| {
            cx.subscribe(&panel, move |_, event: &SendCommand, _| {
                log.borrow_mut()
                    .push((event.command.clone(), event.by_keyboard));
            })
            .detach();
        });
        cx.run_until_parked();
        (panel, cx, sent)
    }

    fn type_in(panel: &Entity<FindPanel>, text: &str, cx: &mut VisualTestContext) {
        panel.update_in(cx, |panel, window, cx| panel.type_text(text, window, cx));
        cx.run_until_parked();
    }

    fn names(panel: &Entity<FindPanel>, cx: &mut VisualTestContext) -> Vec<String> {
        panel.read_with(cx, |panel, _| {
            let (_, results) = panel.results.as_ref().expect("a search finished");
            results.rows.iter().map(|row| row.name.clone()).collect()
        })
    }

    fn notes(panel: &Entity<FindPanel>, cx: &mut VisualTestContext) -> Vec<String> {
        panel.read_with(cx, |panel, _| {
            let (asked, results) = panel.results.as_ref().expect("a search finished");
            panel.notes(&asked.query, results)
        })
    }

    fn click(name: &'static str, cx: &mut VisualTestContext) {
        let button = cx.debug_bounds(name).expect("it's drawn").center();
        cx.simulate_click(button, Modifiers::none());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn typing_narrows_the_list_and_each_row_says_where_it_is(cx: &mut TestAppContext) {
        let (panel, cx, _) = panel_window(sample_find(), cx);
        type_in(&panel, "wolf", cx);
        assert_eq!(names(&panel, cx), ["A grey wolf is here, howling."]);
        let line = panel.read_with(cx, |panel, _| {
            where_line(&panel.results.as_ref().expect("searched").1.rows[0])
        });
        assert_eq!(line, "The North Gate · The Old Town · 2 steps away");
        assert!(cx.debug_bounds("find-row-0").is_some());
        assert!(cx.debug_bounds("find-row-1").is_none());
    }

    #[gpui::test]
    fn walk_lines_up_however_long_the_row_is(cx: &mut TestAppContext) {
        let (_, cx, _) = panel_window(sample_find(), cx);
        click("Rooms", cx);
        let right = |name: &'static str, cx: &mut VisualTestContext| {
            cx.debug_bounds(name).expect("it's drawn").right()
        };
        assert_eq!(
            right("Walk to A Stone Hall", cx),
            right("Walk to The Market Square", cx)
        );
    }

    #[test]
    fn a_rows_marks_are_read_with_it() {
        assert_eq!(
            row_label("A wolf", &[Mark::Aggressive, Mark::Gear], "A gate"),
            "A wolf, aggressive, gear, A gate"
        );
        assert_eq!(row_label("A road", &[], "Fields"), "A road, Fields");
    }

    #[gpui::test]
    fn a_filter_shows_only_its_kind(cx: &mut TestAppContext) {
        let (panel, cx, _) = panel_window(sample_find(), cx);
        click("Mobs", cx);
        let mobs = names(&panel, cx);
        assert!(mobs.contains(&"A healer tends her herbs.".to_owned()));
        assert!(!mobs.contains(&"A Stone Hall".to_owned()));

        click("Items", cx);
        let items = names(&panel, cx);
        assert!(items.contains(&"a brass lantern".to_owned()));
        assert!(items.contains(&"some coins ×4".to_owned()));
        assert!(!items.contains(&"A Stone Hall".to_owned()));
    }

    #[gpui::test]
    fn walk_sends_a_speedwalk_worked_out_from_where_the_player_is(cx: &mut TestAppContext) {
        let (panel, cx, sent) = panel_window(sample_find(), cx);
        type_in(&panel, "gate", cx);
        click("Walk to The North Gate", cx);
        assert_eq!(*sent.borrow(), [("2n".to_owned(), false)]);
    }

    #[gpui::test]
    fn the_keyboard_walks_and_keeps_its_place(cx: &mut TestAppContext) {
        let (panel, cx, sent) = panel_window(sample_find(), cx);
        type_in(&panel, "market", cx);
        panel.update_in(cx, |panel, window, cx| panel.focus(window, cx));
        // All, Rooms, Mobs, Items, then the first Walk.
        for _ in 0..5 {
            press_key("tab", cx);
        }
        cx.run_until_parked();
        panel.update_in(cx, |panel, window, _| {
            assert!(panel.walks[0].is_focused(window));
        });
        press_key("enter", cx);
        cx.run_until_parked();
        assert_eq!(*sent.borrow(), [("1n".to_owned(), true)]);
    }

    /// `count` rooms in a line going north from the player.
    fn long_road(count: i64) -> FindStores {
        let mut live = LiveMap::new(Box::new(PlainRules));
        let now = Timestamp(0);
        for id in 1..=count {
            live.room_info(
                &format!(
                    r#"{{"num": {id}, "name": "Road {id}", "exits": {{"n": {}, "s": {}}}}}"#,
                    id + 1,
                    id - 1
                ),
                now,
            );
            live.sent("north", now);
        }
        let mut map = Versioned::default();
        let commit = live.commit(now).expect("rooms were mapped");
        map.write(&commit.change);
        FindStores::of(
            map,
            Versioned::default(),
            Versioned::default(),
            Versioned::default(),
        )
    }

    #[gpui::test]
    fn tab_reaches_every_row_scrolled_out_of_view_and_comes_back(cx: &mut TestAppContext) {
        let last = MOST_RESULTS - 1;
        let (panel, cx, _) = panel_window(long_road(MOST_RESULTS as i64), cx);
        cx.run_until_parked();
        assert!(cx.debug_bounds("find-row-0").is_some());
        assert!(
            cx.debug_bounds("find-row-49").is_none(),
            "only what's in view is drawn"
        );
        panel.update_in(cx, |panel, window, cx| panel.focus(window, cx));
        cx.run_until_parked();
        for _ in 0..5 + last {
            press_key("tab", cx);
            cx.run_until_parked();
        }
        panel.update_in(cx, |panel, window, _| {
            assert_eq!(panel.focused_row(window), Some(last));
        });
        assert!(
            cx.debug_bounds("find-row-49").is_some(),
            "it scrolled there"
        );

        for _ in 0..last {
            cx.simulate_keystrokes("shift-tab");
            cx.run_until_parked();
        }
        panel.update_in(cx, |panel, window, _| {
            assert_eq!(panel.focused_row(window), Some(0));
        });
        assert!(cx.debug_bounds("find-row-0").is_some());
    }

    #[gpui::test]
    fn a_new_session_never_shows_the_last_ones_rooms(cx: &mut TestAppContext) {
        let (panel, cx, _) = panel_window(sample_find(), cx);
        assert!(names(&panel, cx).contains(&"A Stone Hall".to_owned()));

        // The same revisions as the sample's, as a new session's can be.
        panel.update(cx, |panel, cx| panel.start_over(long_road(1), cx));
        cx.run_until_parked();
        assert_eq!(names(&panel, cx), ["Road 1"]);
    }

    #[gpui::test]
    fn a_walk_still_being_worked_out_is_dropped_with_its_session(cx: &mut TestAppContext) {
        let (panel, cx, sent) = panel_window(sample_find(), cx);
        type_in(&panel, "gate", cx);
        panel.update(cx, |panel, cx| {
            let row = &panel.results.as_ref().expect("searched").1.rows[0];
            let gate = goal(row).expect("the gate can be walked to");
            panel.walk_to(gate, false, cx);
            panel.start_over(sample_find(), cx);
        });
        cx.run_until_parked();
        assert!(sent.borrow().is_empty());
    }

    #[gpui::test]
    fn walk_does_nothing_while_echo_is_off_and_says_why(cx: &mut TestAppContext) {
        let (panel, cx, sent) = panel_window(sample_find(), cx);
        type_in(&panel, "gate", cx);
        panel.update(cx, |panel, cx| panel.set_locked(true, cx));
        cx.run_until_parked();
        click("Walk to The North Gate", cx);
        assert!(sent.borrow().is_empty());
        assert!(notes(&panel, cx).contains(&LOCKED_NOTE.to_owned()));
    }

    #[gpui::test]
    fn the_room_youre_in_and_an_unreachable_one_offer_no_walk(cx: &mut TestAppContext) {
        let (panel, cx, sent) = panel_window(sample_find(), cx);
        type_in(&panel, "hall", cx);
        click("Walk to A Stone Hall", cx);
        type_in(&panel, "hut", cx);
        click("Walk to A Hermit's Hut", cx);
        assert!(sent.borrow().is_empty());
    }

    #[gpui::test]
    fn the_list_is_never_empty_without_a_line_saying_why(cx: &mut TestAppContext) {
        let (panel, cx, _) = panel_window(FindStores::default(), cx);
        for filter in FindFilter::ALL {
            panel.update(cx, |panel, cx| panel.choose(filter, cx));
            cx.run_until_parked();
            assert!(!notes(&panel, cx).is_empty(), "{filter:?}");
        }
        assert_eq!(notes(&panel, cx), [ITEMS_NOTE]);

        panel.update(cx, |panel, cx| {
            panel.start_over(sample_find(), cx);
            panel.choose(FindFilter::Rooms, cx);
        });
        type_in(&panel, "  nothing like it ", cx);
        assert_eq!(
            notes(&panel, cx),
            ["Nothing on your map matches \"nothing like it\"."]
        );
    }

    #[gpui::test]
    fn walk_says_when_the_map_has_lost_the_player(cx: &mut TestAppContext) {
        let (panel, cx, sent) = panel_window(sample_find(), cx);
        let lost = Batch {
            map: Some(Diff {
                revision: Revision(9),
                change: MapChange {
                    revision: 9,
                    whereabouts: Some(Whereabouts::default()),
                    ..MapChange::default()
                },
            }),
            ..Batch::default()
        };
        panel.update(cx, |panel, cx| panel.apply(&lost, cx));
        cx.run_until_parked();
        type_in(&panel, "gate", cx);
        click("Walk to The North Gate", cx);
        assert!(sent.borrow().is_empty());
        assert!(notes(&panel, cx).contains(&LOST_NOTE.to_owned()));
    }

    #[gpui::test]
    fn what_was_typed_during_a_search_is_searched_next(cx: &mut TestAppContext) {
        let (panel, cx, _) = panel_window(sample_find(), cx);
        panel.update_in(cx, |panel, window, cx| panel.type_text("wolf", window, cx));
        panel.update_in(cx, |panel, window, cx| {
            panel.type_text("healer", window, cx);
        });
        cx.run_until_parked();
        assert_eq!(names(&panel, cx), ["A healer tends her herbs."]);
    }

    #[gpui::test]
    fn the_count_is_announced_for_a_new_query_and_not_for_a_move(cx: &mut TestAppContext) {
        let (panel, cx, _) = panel_window(sample_find(), cx);
        type_in(&panel, "wolf", cx);
        let said = |panel: &Entity<FindPanel>, cx: &mut VisualTestContext| {
            panel.read_with(cx, |panel, _| panel.announcement.clone())
        };
        let announced = said(&panel, cx);
        assert_eq!(
            announced.as_ref().map(|(_, text)| text.as_str()),
            Some("1 result")
        );

        let moved = Batch {
            map: Some(Diff {
                revision: Revision(9),
                change: MapChange {
                    revision: 9,
                    whereabouts: Some(Whereabouts {
                        room: Some(2),
                        ..Whereabouts::default()
                    }),
                    ..MapChange::default()
                },
            }),
            ..Batch::default()
        };
        panel.update(cx, |panel, cx| panel.apply(&moved, cx));
        cx.run_until_parked();
        assert_eq!(said(&panel, cx), announced);
        let line = panel.read_with(cx, |panel, _| {
            where_line(&panel.results.as_ref().expect("searched").1.rows[0])
        });
        assert!(line.ends_with("1 step away"), "{line}");
    }

    #[test]
    fn distance_reads_in_words() {
        assert_eq!(reach_words(Reach::Steps(1)).as_deref(), Some("1 step away"));
        assert_eq!(
            reach_words(Reach::Steps(7)).as_deref(),
            Some("7 steps away")
        );
        assert_eq!(reach_words(Reach::Here).as_deref(), Some("You're here"));
    }
}
