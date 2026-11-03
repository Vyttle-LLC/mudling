//! The Gear panel (PRD §8): what's worn, in the game plugin's slot order with
//! what it adds up to and the empty slots on one line, and what's carried,
//! with tags, a level lock, what it would change against what's worn in its
//! slot, and actions. Each item takes two lines: its name, then its stats.
//!
//! The panel holds what the shell gave it and works out its rows when that
//! changes, so a frame only draws them. It never sends a command itself: it
//! emits [`SendCommand`] and the shell sends it, refusing it while the server
//! asks for a password.
//!
//! Each worn or carried row is labelled with all it knows, for a screen
//! reader; what the keyboard reaches in it is its buttons, and More opens the
//! rest in the row. The items the chat named lately are listed too, each a tab
//! stop, so what a tooltip shows on hover reaches the keyboard.
//!
//! Gear sets are listed above Worn, and edited in the panel's place: see
//! [`sets`].

pub mod sets;
pub mod text;

use crate::assets::HEADING_FONT;
use crate::room_panel::{SendCommand, Verbs, button, row_name, section_label, unavailable};
use crate::theme::Theme;
use gpui::{
    AnyView, App, ClickEvent, Context, Div, ElementId, EventEmitter, FocusHandle, FontWeight,
    HighlightStyle, Hsla, KeyBinding, Pixels, Render, Role, SharedString, Stateful, StyledText,
    Window, actions, div, prelude::*, px, transparent_black,
};
use mudling_core::gear::{ItemMatcher, ItemRef, ItemStats, plain_name, shown_name, tags};
use mudling_core::store::{CHARACTER_LEVEL, CarriedItem, CharactersState, ItemsState, WornItem};
use std::collections::VecDeque;
use std::sync::Arc;
use text::Marked;

actions!(gear_panel, [CloseEditor, CloseWords]);

/// The set editor's key context.
const EDITOR_CONTEXT: &str = "GearSetEditor";
/// The Words field's, nearer than the editor's: its Escape leaves only the field.
const WORDS_CONTEXT: &str = "GearSetWords";
const BUTTON_HEIGHT: Pixels = px(28.);
const TAB_HEIGHT: Pixels = px(26.);
/// A row's own buttons, small enough for its two lines.
const ROW_BUTTON_HEIGHT: Pixels = px(22.);
const SLOT_WIDTH: Pixels = px(50.);
/// The group a row's hover lights its primary action from.
const ROW_GROUP: &str = "gear-row";
/// How many items the panel remembers the chat naming.
const MOST_NAMED: usize = 6;
const LOCKED_NOTE: &str = "Gear actions are off while the game asks for a password.";
const NOT_READ_NOTE: &str = "What's worn is read when the game lists it.";
const NOTHING_CARRIED: &str = "You're carrying nothing, as far as the game has said.";
const EMPTY: &str = "Empty";
const WEAR: &str = "wear";
const WIELD: &str = "wield";
const HOLD: &str = "hold";
const REMOVE: &str = "remove";
const EXAMINE: &str = "examine";
/// The plugin's slots an item takes a hand for. A game with others offers
/// Wear first, and Wield and Hold under More.
const HELD_SLOTS: [&str; 2] = ["held", "light"];
const WIELD_SLOT: &str = "wield";

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", CloseEditor, Some(EDITOR_CONTEXT)),
        KeyBinding::new("escape", CloseWords, Some(WORDS_CONTEXT)),
    ]);
}

/// The two views.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum View {
    #[default]
    Worn,
    Carrying,
}

impl View {
    const ALL: [View; 2] = [View::Worn, View::Carrying];

    fn name(self) -> &'static str {
        match self {
            View::Worn => "Worn",
            View::Carrying => "Carrying",
        }
    }
}

/// A command a button sends, and what it's called.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Action {
    name: &'static str,
    verb: &'static str,
    target: String,
}

fn action(name: &'static str, verb: &'static str, target: &Option<String>) -> Option<Action> {
    Some(Action {
        name,
        verb,
        target: target.clone()?,
    })
}

/// A row of either view, all worked out.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    /// What keeps it open, whichever row it moves to.
    key: String,
    /// The worn slot it's in, for a worn row.
    slot: Option<String>,
    name: SharedString,
    /// How many copies it stands for.
    count: usize,
    /// The game's tags.
    tags: Vec<String>,
    /// The level it needs, when the character hasn't reached it.
    needs_level: Option<i64>,
    /// What the row offers without opening.
    primary: Option<Action>,
    /// What More offers.
    more: Vec<Action>,
    /// The second line: its stats, and for a carried item what wearing it
    /// would change.
    line: Marked,
    /// What More shows: what's known, then what it would change.
    known: Vec<String>,
}

impl Row {
    /// What a screen reader reads, all of what a row shows.
    fn label(&self) -> String {
        let mut parts = Vec::new();
        parts.extend(self.slot.clone());
        parts.push(self.counted_name());
        parts.extend(self.tags.iter().cloned());
        parts.extend(self.needs_level.map(|level| format!("Needs level {level}")));
        parts.push(self.line.text.clone());
        parts.join(", ")
    }

    fn counted_name(&self) -> String {
        match self.count {
            0 | 1 => self.name.to_string(),
            count => format!("{} ×{count}", self.name),
        }
    }
}

fn what_is_known(stats: Option<&ItemStats>) -> String {
    stats.map_or_else(|| text::NOT_LEARNED.to_owned(), text::describe)
}

/// What the book knows of an item the chat named.
pub fn tip(items: &ItemsState, name: &str) -> String {
    what_is_known(items.book.stats(ItemRef { name, copy: None }).as_ref())
}

fn worn_item(worn: &WornItem) -> ItemRef<'_> {
    ItemRef {
        name: &worn.item,
        copy: worn.copy.as_deref(),
    }
}

fn worn_row(items: &ItemsState, worn: &WornItem) -> Row {
    let item = worn_item(worn);
    let target = items.book.target(item);
    let stats = items.book.stats(item);
    let mut line = Marked::default();
    line.push(&text::brief(stats.as_ref()));
    Row {
        key: format!("{}:{}", worn.slot, plain_name(&worn.item)),
        slot: Some(worn.slot.clone()),
        name: shown_name(&worn.item).into(),
        count: 1,
        tags: tags(&worn.item),
        needs_level: None,
        primary: action("Remove", REMOVE, &target),
        more: action("Examine", EXAMINE, &target).into_iter().collect(),
        line,
        known: vec![what_is_known(stats.as_ref())],
    }
}

/// What's worn, in the plugin's slot order; a slot the plugin doesn't name
/// comes last.
fn filled_slots(items: &ItemsState) -> impl Iterator<Item = &WornItem> {
    let in_order = (items.slots.iter())
        .flat_map(|slot| items.worn.iter().filter(move |worn| worn.slot == *slot));
    let others = (items.worn.iter()).filter(|worn| !items.slots.contains(&worn.slot));
    in_order.chain(others).filter(|worn| !worn.item.is_empty())
}

fn worn_rows(items: &ItemsState) -> Vec<Row> {
    (filled_slots(items))
        .map(|worn| worn_row(items, worn))
        .collect()
}

/// The plugin's slots with nothing in them, in its order, even those the
/// game hasn't listed yet.
fn empty_slots(items: &ItemsState) -> Vec<String> {
    let filled =
        |slot: &String| (items.worn.iter()).any(|worn| worn.slot == *slot && !worn.item.is_empty());
    (items.slots.iter())
        .filter(|slot| !filled(slot))
        .cloned()
        .collect()
}

/// What `worn` adds up to, each item's stats or `None` where nothing is
/// known; nothing when nothing is worn.
fn worn_total(worn: impl IntoIterator<Item = Option<ItemStats>>) -> Option<String> {
    let mut total = text::StatRanges::new();
    let (mut count, mut unknown) = (0, 0);
    for stats in worn {
        count += 1;
        let Some(stats) = stats else {
            unknown += 1;
            continue;
        };
        for (stat, range) in text::stat_ranges(&stats) {
            let sum = total.entry(stat).or_insert(0..=0);
            *sum = sum.start() + range.start()..=sum.end() + range.end();
        }
    }
    (count > 0).then(|| text::worn_total(&total, unknown))
}

/// The commands for what a carried item can do, its slot's first.
fn carrying_actions(slot: Option<&str>, target: &Option<String>) -> (Option<Action>, Vec<Action>) {
    let wear = action("Wear", WEAR, target);
    let wield = action("Wield", WIELD, target);
    let hold = action("Hold", HOLD, target);
    let examine = action("Examine", EXAMINE, target);
    let (primary, others) = match slot {
        Some(WIELD_SLOT) => (wield, [None, wear, hold]),
        Some(slot) if HELD_SLOTS.contains(&slot) => (hold, [None, wear, wield]),
        Some(_) => (wear, [None, wield, hold]),
        None => (None, [wear, wield, hold]),
    };
    let more = others.into_iter().chain([examine]).flatten().collect();
    (primary, more)
}

fn carried_row(items: &ItemsState, copies: &[&CarriedItem], level: Option<i64>) -> Row {
    let first = copies[0];
    let item = ItemRef {
        name: &first.name,
        copy: Some(&first.id),
    };
    let key = plain_name(&first.name);
    let known = items.book.item(&key);
    let slot = known.and_then(|known| known.facts.slot.clone());
    let needs = known.and_then(|known| known.level());
    let needs_level = needs.filter(|needs| level.is_some_and(|level| level < *needs));
    let target = items.book.target(item);
    let (primary, mut more) = carrying_actions(slot.as_deref(), &target);
    let primary = if needs_level.is_some() {
        more.splice(0..0, primary);
        None
    } else {
        primary
    };
    let in_use = (items.worn.iter())
        .filter(|worn| plain_name(&worn.item) == key)
        .count();
    let in_slot: Vec<ItemRef> = (items.worn.iter())
        .filter(|worn| Some(&worn.slot) == slot.as_ref() && !worn.item.is_empty())
        .map(worn_item)
        .collect();
    let compared = items.book.compare(item, in_slot.iter().copied());
    let stats = items.book.stats(item);
    let mut line = Marked::default();
    line.push(&text::brief(stats.as_ref()));
    if !compared.is_empty() {
        line.push(" → ");
        for (at, comparison) in compared.iter().enumerate() {
            if at > 0 {
                line.push(" or ");
            }
            line.push_comparison(comparison);
        }
    } else if let Some(slot) = slot.as_ref().filter(|_| in_slot.is_empty()) {
        line.push(&format!(" → {slot} empty"));
    }
    if in_use > 0 {
        line.push(&format!(" · {in_use} worn"));
    }
    let mut known_lines = vec![what_is_known(stats.as_ref())];
    known_lines.extend(compared.iter().map(text::comparison));
    Row {
        key,
        slot: None,
        name: shown_name(&first.name).into(),
        count: copies.len(),
        tags: tags(&first.name),
        needs_level,
        primary,
        more,
        line,
        known: known_lines,
    }
}

/// One row per item, its copies counted, in the order the game lists them.
fn carried_rows(items: &ItemsState, level: Option<i64>) -> Vec<Row> {
    let mut groups: Vec<(String, Vec<&CarriedItem>)> = Vec::new();
    for carried in items.carried.iter() {
        let key = plain_name(&carried.name);
        match groups.iter_mut().find(|(group, _)| *group == key) {
            Some((_, copies)) => copies.push(carried),
            None => groups.push((key, vec![carried])),
        }
    }
    (groups.iter())
        .map(|(_, copies)| carried_row(items, copies, level))
        .collect()
}

pub struct GearPanel {
    focus_handle: FocusHandle,
    verbs: Verbs,
    view: View,
    items: ItemsState,
    level: Option<i64>,
    locked: bool,
    /// The row open, by its key.
    open: Option<String>,
    worn: Vec<Row>,
    /// The plugin's slots nothing is worn in.
    empty: Vec<String>,
    /// What's worn adds up to.
    total: Option<String>,
    carried: Vec<Row>,
    sets: Vec<sets::SetRow>,
    /// The set being made or changed: it takes the panel's place.
    editor: Option<sets::Editor>,
    /// Items the chat named, newest first, by plain name.
    named: VecDeque<String>,
}

impl EventEmitter<SendCommand> for GearPanel {}
impl EventEmitter<sets::SetAction> for GearPanel {}

impl GearPanel {
    pub fn new(verbs: Verbs, cx: &mut Context<GearPanel>) -> GearPanel {
        GearPanel {
            focus_handle: cx.focus_handle(),
            verbs,
            view: View::default(),
            items: ItemsState::default(),
            level: None,
            locked: false,
            open: None,
            worn: Vec::new(),
            empty: Vec::new(),
            total: None,
            carried: Vec::new(),
            sets: Vec::new(),
            editor: None,
            named: VecDeque::new(),
        }
    }

    /// Switches to a new game's words for the verbs.
    pub fn use_verbs(&mut self, verbs: Verbs, cx: &mut Context<GearPanel>) {
        self.verbs = verbs;
        cx.notify();
    }

    /// Takes the store's new state.
    pub fn show_items(&mut self, items: ItemsState, cx: &mut Context<GearPanel>) {
        self.items = items;
        self.rebuild(cx);
    }

    #[cfg(test)]
    pub fn items(&self) -> &ItemsState {
        &self.items
    }

    /// Takes the character's level, for the level locks.
    pub fn show_character(&mut self, character: &CharactersState, cx: &mut Context<GearPanel>) {
        let level = match character.0.get(CHARACTER_LEVEL) {
            Some(mudling_core::plugin::luau::Value::Number(level)) => Some(*level as i64),
            _ => None,
        };
        if self.level != level {
            self.level = level;
            self.rebuild(cx);
        }
    }

    pub fn set_locked(&mut self, locked: bool, cx: &mut Context<GearPanel>) {
        if self.locked != locked {
            self.locked = locked;
            cx.notify();
        }
    }

    fn rebuild(&mut self, cx: &mut Context<GearPanel>) {
        self.worn = worn_rows(&self.items);
        self.empty = empty_slots(&self.items);
        let worn = filled_slots(&self.items).map(|worn| self.items.book.stats(worn_item(worn)));
        self.total = worn_total(worn);
        self.carried = carried_rows(&self.items, self.level);
        self.sets = sets::set_rows(&self.items);
        self.refresh_editor(cx);
        cx.notify();
    }

    /// Notes the items a line of the chat names.
    pub fn hear(&mut self, line: &str, cx: &mut Context<GearPanel>) {
        let matcher: Arc<ItemMatcher> = Arc::clone(&self.items.matcher);
        for mention in matcher.find(line) {
            let at = self.named.iter().position(|name| name == mention.item);
            if at == Some(0) {
                continue;
            }
            at.map(|at| self.named.remove(at));
            self.named.push_front(mention.item.to_owned());
            self.named.truncate(MOST_NAMED);
            cx.notify();
        }
    }

    /// Opens the Carrying view, for a sample.
    pub fn show_carrying(&mut self, cx: &mut Context<GearPanel>) {
        self.choose(View::Carrying, cx);
    }

    /// Opens the Worn view, for a sample.
    pub fn show_worn(&mut self, cx: &mut Context<GearPanel>) {
        self.choose(View::Worn, cx);
    }

    fn choose(&mut self, view: View, cx: &mut Context<GearPanel>) {
        if self.view != view {
            self.view = view;
            self.open = None;
            cx.notify();
        }
    }

    fn toggle(&mut self, key: &str, cx: &mut Context<GearPanel>) {
        self.open = match self.open.as_deref() {
            Some(open) if open == key => None,
            _ => Some(key.to_owned()),
        };
        cx.notify();
    }

    fn press(
        &mut self,
        action: &Action,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<GearPanel>,
    ) {
        if self.locked {
            return;
        }
        let by_keyboard = event.is_keyboard();
        if by_keyboard {
            window.focus(&self.focus_handle, cx);
        }
        cx.emit(SendCommand {
            command: self.verbs.command(action.verb, &action.target),
            by_keyboard,
        });
    }

    fn rows(&self) -> &[Row] {
        match self.view {
            View::Worn => &self.worn,
            View::Carrying => &self.carried,
        }
    }

    fn render_views(&self, theme: &Theme, cx: &mut Context<GearPanel>) -> impl IntoElement {
        let tabs = View::ALL.map(|view| {
            let selected = view == self.view;
            let name: SharedString = self.tab_name(view).into();
            button(("gear-view", view as usize), name.clone(), theme)
                .role(Role::Tab)
                .aria_selected(selected)
                .h(TAB_HEIGHT)
                .px(px(10.))
                .border_color(theme.segment)
                .text_size(px(12.))
                .text_color(theme.text_muted)
                .when(selected, |tab| {
                    tab.bg(theme.selected)
                        .text_color(theme.text)
                        .font_weight(FontWeight::MEDIUM)
                })
                .on_click(cx.listener(move |panel, _, _, cx| panel.choose(view, cx)))
                .child(name)
        });
        div()
            .id("gear-views")
            .role(Role::TabList)
            .aria_label("Gear view")
            .flex()
            .flex_none()
            .p(px(3.))
            .rounded_lg()
            .bg(theme.segment)
            .border_1()
            .border_color(theme.border)
            .children(tabs)
    }

    /// A button that sends `action` for `row`'s item, unstyled for its place.
    fn action_button(
        &self,
        id: ElementId,
        action: &Action,
        row: &Row,
        theme: &Theme,
        cx: &mut Context<GearPanel>,
    ) -> Stateful<Div> {
        let label = format!("{} {}", action.name, row.name);
        let pressed = action.clone();
        let button = button(id, label.into(), theme)
            .font_weight(FontWeight::MEDIUM)
            .child(action.name);
        if self.locked {
            unavailable(button, LOCKED_NOTE)
        } else {
            button.on_click(cx.listener(move |panel, event, window, cx| {
                panel.press(&pressed, event, window, cx);
            }))
        }
    }

    fn render_open(
        &self,
        index: usize,
        row: &Row,
        theme: &Theme,
        cx: &mut Context<GearPanel>,
    ) -> Div {
        let lines = row.known.iter().enumerate().map(|(line, known)| {
            div()
                .id(("gear-known", index * 100 + line))
                .role(Role::Label)
                .aria_value(SharedString::from(known.clone()))
                .text_size(px(12.))
                .text_color(theme.text_muted)
                .child(known.clone())
        });
        let actions = row.more.iter().enumerate().map(|(at, action)| {
            let id = ElementId::NamedInteger("gear-more".into(), (index * 10 + at) as u64);
            self.action_button(id, action, row, theme, cx)
                .h(BUTTON_HEIGHT)
                .px_3()
                .text_color(theme.text)
        });
        div()
            .flex()
            .flex_col()
            .gap_2()
            .pt_2()
            .children(lines)
            .child(div().flex().flex_wrap().gap_2().children(actions))
    }

    fn render_row(
        &self,
        index: usize,
        row: &Row,
        theme: &Theme,
        cx: &mut Context<GearPanel>,
    ) -> Stateful<Div> {
        let is_open = self.open.as_deref() == Some(row.key.as_str());
        let slot = row.slot.clone().map(|slot| {
            div()
                .flex_none()
                .self_start()
                .w(SLOT_WIDTH)
                .pt(px(1.))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_size(px(11.5))
                .text_color(theme.text_muted)
                .child(slot)
        });
        let tags = (row.tags.iter()).map(|tag| chip(tag.clone(), theme.text_muted, theme.border));
        let count = (row.count > 1).then(|| {
            div()
                .flex_none()
                .text_size(px(12.))
                .text_color(theme.text_muted)
                .child(format!("×{}", row.count))
        });
        let lock = row.needs_level.map(|level| {
            let lock = chip(
                format!("Lv {level}"),
                theme.warning,
                theme.warning.opacity(0.35),
            );
            lock.id(("gear-lock", index))
                .debug_selector(move || format!("gear-lock-{index}"))
                .bg(theme.warning_ground)
        });
        let primary = row.primary.as_ref().map(|action| {
            let id = ElementId::NamedInteger("gear-primary".into(), index as u64);
            self.action_button(id, action, row, theme, cx)
                .h(ROW_BUTTON_HEIGHT)
                .px(px(6.))
                .rounded(px(4.))
                .border_color(transparent_black())
                .text_size(px(12.))
                .text_color(theme.accent)
                .group_hover(ROW_GROUP, |style| style.bg(theme.hover))
        });
        let key = row.key.clone();
        let label = format!("More about {}", row.name);
        let more = button(("gear-open", index), label.into(), theme)
            .aria_expanded(is_open)
            .h(ROW_BUTTON_HEIGHT)
            .w(px(22.))
            .rounded(px(4.))
            .border_color(transparent_black())
            .text_color(theme.text_muted)
            .on_click(cx.listener(move |panel, _, _, cx| panel.toggle(&key, cx)))
            .child("···");
        div()
            .id(("gear-row", index))
            .debug_selector(move || format!("gear-row-{index}"))
            .group(ROW_GROUP)
            .role(Role::ListItem)
            .aria_label(SharedString::from(row.label()))
            .flex()
            .flex_col()
            .px_1()
            .rounded(px(5.))
            .hover(|style| style.bg(theme.hover))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .py(px(2.))
                    .children(slot)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .text_size(px(13.5))
                                    .line_height(px(17.))
                                    .text_color(theme.text)
                                    .child(
                                        div()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .text_ellipsis()
                                            .child(row.name.clone()),
                                    )
                                    .children(tags)
                                    .children(count)
                                    .children(lock),
                            )
                            .child(
                                div()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_size(px(11.5))
                                    .line_height(px(15.))
                                    .text_color(theme.text_muted)
                                    .child(marked(&row.line, theme)),
                            ),
                    )
                    .children(primary)
                    .child(more),
            )
            .when(is_open, |open| {
                open.child(self.render_open(index, row, theme, cx))
            })
    }

    /// What's worn adds up to, above the worn rows.
    fn render_total(&self, theme: &Theme) -> Option<impl IntoElement + use<>> {
        let total = self.total.clone()?;
        let heading = HighlightStyle {
            color: Some(theme.text),
            font_weight: Some(FontWeight::MEDIUM),
            ..HighlightStyle::default()
        };
        let text =
            StyledText::new(total.clone()).with_highlights([(0..text::WORN_TOTAL.len(), heading)]);
        Some(
            div()
                .id("gear-total")
                .debug_selector(|| "gear-total".to_owned())
                .role(Role::Label)
                .aria_value(SharedString::from(total))
                .flex_none()
                .px_2()
                .py(px(5.))
                .rounded(px(6.))
                .bg(theme.surface_raised)
                .text_size(px(11.5))
                .text_color(theme.text_muted)
                .child(text),
        )
    }

    /// The slots nothing is worn in, below the worn rows.
    fn render_empty(&self, theme: &Theme) -> Option<impl IntoElement + use<>> {
        if self.empty.is_empty() {
            return None;
        }
        let empty = format!("{EMPTY} · {}", self.empty.join(" · "));
        let heading = HighlightStyle {
            color: Some(theme.placeholder),
            font_weight: Some(FontWeight::MEDIUM),
            ..HighlightStyle::default()
        };
        let text = StyledText::new(empty.clone()).with_highlights([(0..EMPTY.len(), heading)]);
        Some(
            div()
                .id("gear-empty-slots")
                .debug_selector(|| "gear-empty-slots".to_owned())
                .role(Role::Label)
                .aria_value(SharedString::from(empty))
                .flex_none()
                .text_size(px(11.5))
                .text_color(theme.text_muted)
                .child(text),
        )
    }

    /// The items the chat named, each saying what its tooltip says.
    fn render_named(&self, theme: &Theme) -> Option<impl IntoElement + use<>> {
        if self.named.is_empty() {
            return None;
        }
        let rows = self.named.iter().enumerate().map(|(index, key)| {
            let known = self.items.book.item(key);
            let name = known.map_or_else(|| key.clone(), |known| known.name.clone());
            let what = tip(&self.items, key);
            div()
                .id(("gear-named", index))
                .debug_selector(move || format!("gear-named-{index}"))
                .role(Role::ListItem)
                .aria_label(SharedString::from(format!("{name}: {what}")))
                .focusable()
                .tab_stop(true)
                .rounded_md()
                .focus_visible(|row| row.bg(theme.hover))
                .py_1()
                .child(row_name(name.into()))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme.text_muted)
                        .child(what),
                )
        });
        Some(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(section_label("NAMED IN CHAT", theme))
                .child(
                    div()
                        .id("gear-named")
                        .role(Role::List)
                        .aria_label("Items named in chat")
                        .children(rows),
                ),
        )
    }

    fn notes(&self) -> Vec<&'static str> {
        let mut notes = Vec::new();
        if self.view == View::Worn && self.items.worn.is_empty() {
            notes.push(NOT_READ_NOTE);
        }
        if self.view == View::Carrying && self.carried.is_empty() {
            notes.push(NOTHING_CARRIED);
        }
        if self.locked {
            notes.push(LOCKED_NOTE);
        }
        notes
    }

    /// "Worn", or "Carrying · 23".
    fn tab_name(&self, view: View) -> String {
        match view {
            View::Worn => view.name().to_owned(),
            View::Carrying => {
                let count: usize = self.carried.iter().map(|row| row.count).sum();
                format!("{} · {count}", view.name())
            }
        }
    }
}

/// The set editor's heading.
fn render_heading(name: &'static str, theme: &Theme) -> impl IntoElement + use<> {
    div()
        .id("gear-heading")
        .role(Role::Heading)
        .aria_level(2)
        .aria_label(name)
        .font_family(HEADING_FONT)
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(px(21.))
        .text_color(theme.text)
        .child(name)
}

/// A tag or a lock after an item's name.
fn chip(text: String, color: Hsla, border: Hsla) -> Div {
    div()
        .flex_none()
        .px(px(4.))
        .rounded(px(4.))
        .border_1()
        .border_color(border)
        .text_size(px(10.5))
        .line_height(px(14.))
        .text_color(color)
        .child(text)
}

/// `line`, its gains and losses in the theme's colors.
fn marked(line: &Marked, theme: &Theme) -> StyledText {
    let highlights = line.marks.iter().map(|(range, better)| {
        let color = if *better { theme.better } else { theme.worse };
        let style = HighlightStyle {
            color: Some(color),
            ..HighlightStyle::default()
        };
        (range.clone(), style)
    });
    StyledText::new(line.text.clone()).with_highlights(highlights)
}

impl Render for GearPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<GearPanel>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        if let Some(editor) = &self.editor {
            return self.render_editor(editor, &theme, cx);
        }
        let rows: Vec<_> = (self.rows().iter().enumerate())
            .map(|(index, row)| self.render_row(index, row, &theme, cx))
            .collect();
        let notes = self.notes().into_iter().enumerate().map(|(index, note)| {
            div()
                .id(("gear-note", index))
                .debug_selector(move || format!("gear-note-{index}"))
                .role(Role::Label)
                .aria_value(note)
                .text_size(px(12.))
                .text_color(theme.text_muted)
                .child(note)
        });
        let worn = self.view == View::Worn;
        div()
            .id("gear-panel")
            .track_focus(&self.focus_handle)
            .role(Role::Group)
            .aria_label("Gear")
            .size_full()
            .flex()
            .flex_col()
            .gap(px(10.))
            .p_4()
            .overflow_y_scroll()
            .child(div().flex().child(self.render_views(&theme, cx)))
            .when(worn, |panel| panel.child(self.render_sets(&theme, cx)))
            .children(notes)
            .when(worn, |panel| panel.children(self.render_total(&theme)))
            .child(
                div()
                    .id("gear-rows")
                    .role(Role::List)
                    .aria_label(self.view.name())
                    .flex()
                    .flex_col()
                    .children(rows),
            )
            .when(worn, |panel| panel.children(self.render_empty(&theme)))
            .children(self.render_named(&theme))
            .into_any_element()
    }
}

/// The chat's tooltip: an item's name, and what's known.
pub struct ItemTip {
    name: SharedString,
    known: SharedString,
}

/// A tooltip over `name`, which is what a line called it.
pub fn item_tip(name: String, known: String, cx: &mut App) -> AnyView {
    let (name, known) = (name.into(), known.into());
    cx.new(|_| ItemTip { name, known }).into()
}

impl Render for ItemTip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<ItemTip>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        div()
            .id("item-tip")
            .debug_selector(|| "item-tip".to_owned())
            .max_w(px(360.))
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_raised)
            .text_size(px(12.))
            .text_color(theme.text)
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(self.name.clone()),
            )
            .child(div().text_color(theme.text_muted).child(self.known.clone()))
    }
}

#[cfg(test)]
mod tests;
