//! Gear sets in the Gear panel: the section above Worn, and the editor that
//! takes the panel's place.
//!
//! The editor holds the set's pieces and works its rows and preview out when
//! they, or the store, change, so a frame only draws them. The preview is the
//! core's plan, the one Wear sends. Saving, deleting and wearing go to the
//! shell as a [`SetAction`]: the panel never touches the session.
//!
//! Each row is labelled with all it shows. The keyboard reaches its buttons,
//! and the preview as one stop that reads every command.

use super::{BUTTON_HEIGHT, GearPanel, LOCKED_NOTE};
use crate::input::{CommandInput, InputEvent};
use crate::room_panel::{button, row_name, section_label};
use crate::theme::Theme;
use gpui::{
    AnyElement, Context, Div, Entity, FocusHandle, Focusable, FontWeight, Pixels, Role,
    SharedString, Stateful, Subscription, Window, div, prelude::*, px,
};
use mudling_core::gear::sets::{Candidate, WearPlan, candidates, match_pieces};
use mudling_core::gear::{plain_name, shown_name};
use mudling_core::persist::{GearPiece, GearSet, Timestamp};
use mudling_core::session::GearEdit;
use mudling_core::store::ItemsState;

const SMALL_BUTTON_HEIGHT: Pixels = px(24.);
const NO_CHARACTER_NOTE: &str = "Sets are kept per character, once the game says who's playing.";
const NO_SETS_NOTE: &str = "Put on what you wear for levelling, tanking or hitting and save it as \
                            a set. One press then finds each piece, even in a bag.";
const NEW_SET: &str = "New set";
const NAME_LABEL: &str = "Set name";
const NAME_PLACEHOLDER: &str = "Set name, like Levelling, Tank or Hit";
const WORDS_LABEL: &str = "Words the game knows it by";
const WORDS_PLACEHOLDER: &str = "Leave empty to let MUDling guess";
const WORDS_NOTE: &str = "Several words must all match, so only this item answers.";
const SAVE_WORDS: &str = "Save words";
const CONTAINER_LABEL: &str = "What comes off goes in";
const KEEP_CARRIED: &str = "Nothing: keep it carried";
const NOT_SEEN: &str = "not seen right now";
const EXACT_COPY: &str = "this exact copy";
const PREVIEW_TITLE: &str = "Wearing it now would send";
const IDS_NOTE: &str = "Numbers are the game's ids for exact copies.";
const NOTHING_CHOSEN: &str = "Add the pieces above.";
const ALL_ON: &str = "Nothing: it's all on.";

/// What the panel asks of the shell.
#[derive(Clone, Debug, PartialEq)]
pub enum SetAction {
    Wear {
        character: String,
        id: String,
        by_keyboard: bool,
    },
    Edit(GearEdit),
}

/// A set in the list, all worked out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SetRow {
    pub(super) id: String,
    pub(super) name: SharedString,
    /// "18 pieces · what comes off goes in a canvas backpack".
    pub(super) summary: String,
}

fn count(number: usize, one: &str, many: &str) -> String {
    let noun = if number == 1 { one } else { many };
    format!("{number} {noun}")
}

fn set_row(items: &ItemsState, set: &GearSet) -> SetRow {
    let mut parts = vec![count(set.pieces.len(), "piece", "pieces")];
    parts.push(match &set.container {
        Some(container) => format!("what comes off goes in {}", shown_name(container)),
        None => "what comes off stays carried".to_owned(),
    });
    let unseen = items.plan(set).missing.len();
    if unseen > 0 {
        parts.push(format!("{unseen} {NOT_SEEN}"));
    }
    SetRow {
        id: set.id.clone(),
        name: set.name.clone().into(),
        summary: parts.join(" · "),
    }
}

pub(super) fn set_rows(items: &ItemsState) -> Vec<SetRow> {
    (items.sets.iter()).map(|set| set_row(items, set)).collect()
}

/// A piece of the set being edited, as its row shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PieceRow {
    /// Where it is in the set.
    at: usize,
    name: String,
    /// The piece's slot, where it is now, and how it's named.
    detail: String,
    seen: bool,
    /// The copy to pin it to, when one is in view.
    copy: Option<String>,
    pinned: bool,
}

/// Where a candidate is now, in a word or two.
fn place(candidate: &Candidate) -> String {
    match (&candidate.worn_in, &candidate.bag) {
        (Some(slot), _) => format!("worn: {slot}"),
        (None, Some(bag)) => format!("in {bag}"),
        (None, None) => "carried".to_owned(),
    }
}

/// `'ring hematite'`, or nothing until the game's words are known.
fn known_words(items: &ItemsState, name: &str) -> Option<String> {
    let item = items.book.item(name)?;
    let words = item.facts.keywords.as_ref()?;
    Some(format!("'{words}'"))
}

fn piece_row(
    items: &ItemsState,
    at: usize,
    piece: &GearPiece,
    found: Option<&Candidate>,
) -> PieceRow {
    let mut parts: Vec<String> = piece.slot.iter().cloned().collect();
    match found {
        None => parts.push(NOT_SEEN.to_owned()),
        Some(found) if found.worn_in.is_none() => parts.push(place(found)),
        Some(_) => {}
    }
    let pinned = piece.copy_id.is_some();
    if pinned {
        parts.push(EXACT_COPY.to_owned());
    }
    parts.extend(known_words(items, &piece.item));
    PieceRow {
        at,
        name: shown_name(&piece.item),
        detail: parts.join(" · "),
        seen: found.is_some(),
        copy: found.and_then(|found| found.copy.clone()),
        pinned,
    }
}

/// The set being made or changed.
pub(super) struct Editor {
    /// Empty for a new set.
    id: String,
    /// Who it's for: the editor closes if they go.
    character: String,
    name: Entity<CommandInput>,
    pieces: Vec<GearPiece>,
    container: Option<String>,
    picking_container: bool,
    /// The item whose words are being edited.
    words_of: Option<String>,
    words: Entity<CommandInput>,
    asking_to_delete: bool,
    /// The bags that can be chosen, by name.
    bags: Vec<String>,
    rows: Vec<PieceRow>,
    at_hand: Vec<Candidate>,
    preview: WearPlan,
    preview_stop: FocusHandle,
    /// Takes focus when deleting asks first.
    keep: FocusHandle,
    _field_events: [Subscription; 2],
}

impl Editor {
    #[cfg(test)]
    pub(super) fn pieces(&self) -> &[GearPiece] {
        &self.pieces
    }

    #[cfg(test)]
    pub(super) fn preview(&self) -> &WearPlan {
        &self.preview
    }

    #[cfg(test)]
    pub(super) fn container(&self) -> Option<&str> {
        self.container.as_deref()
    }

    #[cfg(test)]
    pub(super) fn preview_stop(&self) -> &FocusHandle {
        &self.preview_stop
    }

    #[cfg(test)]
    pub(super) fn fields(&self) -> (Entity<CommandInput>, Entity<CommandInput>) {
        (self.name.clone(), self.words.clone())
    }

    fn set(&self, name: &str) -> GearSet {
        GearSet {
            id: self.id.clone(),
            character_id: self.character.clone(),
            name: name.trim().to_owned(),
            container: self.container.clone(),
            pieces: self.pieces.clone(),
            updated: Timestamp(0),
        }
    }

    /// Works out what the editor shows from its pieces and the store.
    fn refresh(&mut self, items: &ItemsState) {
        let found = candidates(items.gear_now());
        let matched = match_pieces(&self.pieces, &found);
        let piece_rows = self.pieces.iter().zip(&matched).enumerate();
        self.rows = piece_rows
            .map(|(at, (piece, candidate))| {
                piece_row(items, at, piece, candidate.map(|index| &found[index]))
            })
            .collect();
        let taken: Vec<usize> = matched.iter().copied().flatten().collect();
        self.at_hand = (found.into_iter().enumerate())
            .filter(|(index, _)| !taken.contains(index))
            .map(|(_, candidate)| candidate)
            .collect();
        let bags = items.bags.iter().map(|bag| shown_name(&bag.name));
        self.bags = bags.collect();
        let chosen = self.container.iter();
        let gone = chosen.filter(|name| !self.bags.iter().any(|bag| same_name(bag, name)));
        let gone: Vec<String> = gone.cloned().collect();
        self.bags.extend(gone);
        self.preview = items.plan(&self.set(""));
    }

    /// Takes the worn pieces as the set, each knowing its slot and copy.
    fn use_whats_worn(&mut self, items: &ItemsState) {
        self.pieces = worn_pieces(items);
    }
}

/// Adds a candidate. In a slot that's full it takes the place of the piece
/// added there first: a set can't wear both.
fn add_piece(pieces: &mut Vec<GearPiece>, candidate: &Candidate, items: &ItemsState) {
    let known = || items.book.item(&candidate.name)?.facts.slot.clone();
    let slot = candidate.worn_in.clone().or_else(known);
    if let Some(slot) = &slot {
        let there = |piece: &GearPiece| piece.slot.as_ref() == Some(slot);
        let filled = pieces.iter().filter(|piece| there(piece)).count();
        if filled >= items.rules.capacity(slot) {
            let first = pieces.iter().position(there);
            first.map(|at| pieces.remove(at));
        }
    }
    pieces.push(GearPiece {
        item: candidate.name.clone(),
        slot,
        copy_id: candidate.copy.clone(),
    });
}

fn same_name(one: &str, other: &str) -> bool {
    plain_name(one) == plain_name(other)
}

fn worn_pieces(items: &ItemsState) -> Vec<GearPiece> {
    let found = candidates(items.gear_now());
    let worn = found.into_iter().filter(|found| found.worn_in.is_some());
    worn.map(|found| GearPiece {
        item: found.name,
        slot: found.worn_in,
        copy_id: found.copy,
    })
    .collect()
}

/// The lines of a preview: each command, then what it couldn't do.
fn preview_lines(preview: &WearPlan, pieces: usize) -> Vec<String> {
    let missing = preview.missing.iter();
    let missing = missing.map(|item| format!("can't find {}", shown_name(item)));
    let stand_ins = preview.stand_ins.iter().map(|item| {
        let item = shown_name(item);
        format!("(the copy chosen of {item} isn't seen: another is on)")
    });
    let mut lines: Vec<String> = (preview.commands.iter().cloned())
        .chain(missing)
        .chain(stand_ins)
        .collect();
    if lines.is_empty() {
        let none = if pieces == 0 { NOTHING_CHOSEN } else { ALL_ON };
        lines.push(none.to_owned());
    }
    lines
}

/// What the preview says of what stays on.
fn left_alone_note(left_alone: usize) -> String {
    match left_alone {
        0 => format!("Pieces already on are left alone. {IDS_NOTE}"),
        1 => format!("1 piece already on is left alone. {IDS_NOTE}"),
        many => format!("{many} pieces already on are left alone. {IDS_NOTE}"),
    }
}

impl GearPanel {
    /// The store changed: the editor follows, or closes if its character went.
    pub(super) fn refresh_editor(&mut self, cx: &mut Context<GearPanel>) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        if editor.character != *self.items.character {
            self.editor = None;
            cx.notify();
            return;
        }
        editor.refresh(&self.items);
    }

    fn open_editor(
        &mut self,
        set: Option<GearSet>,
        window: &mut Window,
        cx: &mut Context<GearPanel>,
    ) {
        let name = cx.new(|cx| CommandInput::field(NAME_LABEL, NAME_PLACEHOLDER, cx));
        let words = cx.new(|cx| CommandInput::field(WORDS_LABEL, WORDS_PLACEHOLDER, cx));
        let typing = cx.observe(&name, |_, _, cx| cx.notify());
        let submitted = cx.subscribe_in(&words, window, |panel, _, event, window, cx| {
            if let InputEvent::Submitted(words) = event {
                panel.save_words(words.clone(), window, cx);
            }
        });
        let text = set.as_ref().map(|set| set.name.clone()).unwrap_or_default();
        let cursor = text.len();
        name.update(cx, |field, cx| field.show(text, cursor, window, cx));
        window.focus(&name.focus_handle(cx), cx);
        let first_bag = (self.items.bags.first()).map(|bag| shown_name(&bag.name));
        let mut editor = Editor {
            id: set.as_ref().map(|set| set.id.clone()).unwrap_or_default(),
            character: self.items.character.to_string(),
            name,
            pieces: set
                .as_ref()
                .map_or_else(|| worn_pieces(&self.items), |set| set.pieces.clone()),
            container: set.map_or(first_bag, |set| set.container),
            picking_container: false,
            words_of: None,
            words,
            asking_to_delete: false,
            bags: Vec::new(),
            rows: Vec::new(),
            at_hand: Vec::new(),
            preview: WearPlan::default(),
            preview_stop: cx.focus_handle().tab_stop(true),
            keep: cx.focus_handle().tab_stop(true),
            _field_events: [typing, submitted],
        };
        editor.refresh(&self.items);
        self.editor = Some(editor);
        cx.notify();
    }

    /// Opens the first set in the editor, for a sample.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn edit_first_set(&mut self, window: &mut Window, cx: &mut Context<GearPanel>) {
        let first = self.items.sets.first().cloned();
        self.open_editor(first, window, cx);
    }

    /// Closes the editor a sample opened.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn close_sample_editor(&mut self, cx: &mut Context<GearPanel>) {
        self.editor = None;
        cx.notify();
    }

    fn close_editor(&mut self, window: &mut Window, cx: &mut Context<GearPanel>) {
        self.editor = None;
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// Changes the editor and works out what it shows again.
    fn edit_set(
        &mut self,
        cx: &mut Context<GearPanel>,
        change: impl FnOnce(&mut Editor, &ItemsState),
    ) {
        if let Some(editor) = &mut self.editor {
            change(editor, &self.items);
            editor.refresh(&self.items);
            cx.notify();
        }
    }

    fn save_set(&mut self, window: &mut Window, cx: &mut Context<GearPanel>) {
        let Some(editor) = &self.editor else { return };
        let name = editor.name.read(cx).content().to_owned();
        if name.trim().is_empty() || editor.pieces.is_empty() {
            return;
        }
        cx.emit(SetAction::Edit(GearEdit::SaveSet(editor.set(&name))));
        self.close_editor(window, cx);
    }

    fn delete_set(&mut self, window: &mut Window, cx: &mut Context<GearPanel>) {
        let Some(editor) = &self.editor else { return };
        cx.emit(SetAction::Edit(GearEdit::RemoveSet {
            character: editor.character.clone(),
            id: editor.id.clone(),
        }));
        self.close_editor(window, cx);
    }

    fn open_words(&mut self, item: String, window: &mut Window, cx: &mut Context<GearPanel>) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        let known = self.items.book.item(&item);
        let words = known.and_then(|known| known.facts.keywords.clone());
        let text = words.unwrap_or_default();
        let cursor = text.len();
        let field = editor.words.clone();
        editor.words_of = Some(item);
        field.update(cx, |field, cx| field.show(text, cursor, window, cx));
        window.focus(&field.focus_handle(cx), cx);
        cx.notify();
    }

    fn save_words(&mut self, words: String, window: &mut Window, cx: &mut Context<GearPanel>) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        let Some(item) = editor.words_of.take() else {
            return;
        };
        cx.emit(SetAction::Edit(GearEdit::SetKeywords { item, words }));
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn press_wear(&mut self, id: &str, by_keyboard: bool, cx: &mut Context<GearPanel>) {
        if !self.locked {
            cx.emit(SetAction::Wear {
                character: self.items.character.to_string(),
                id: id.to_owned(),
                by_keyboard,
            });
        }
    }

    fn render_set(
        &self,
        index: usize,
        row: &SetRow,
        theme: &Theme,
        cx: &mut Context<GearPanel>,
    ) -> impl IntoElement + use<> {
        let edit_id = row.id.clone();
        let edit = button(
            ("gear-set-edit", index),
            format!("Edit {}", row.name).into(),
            theme,
        )
        .h(BUTTON_HEIGHT)
        .px_3()
        .on_click(cx.listener(move |panel, _, window, cx| {
            let set = panel.items.sets.iter().find(|set| set.id == edit_id);
            panel.open_editor(set.cloned(), window, cx);
        }))
        .child("Edit");
        let wear_id = row.id.clone();
        let wear = button(
            ("gear-set-wear", index),
            format!("Wear {}", row.name).into(),
            theme,
        )
        .h(BUTTON_HEIGHT)
        .px_3()
        .border_color(theme.accent)
        .text_color(theme.accent)
        .font_weight(FontWeight::MEDIUM)
        .child("Wear");
        let wear = if self.locked {
            wear.opacity(0.5).aria_description(LOCKED_NOTE)
        } else {
            wear.on_click(cx.listener(move |panel, event: &gpui::ClickEvent, _, cx| {
                panel.press_wear(&wear_id, event.is_keyboard(), cx);
            }))
        };
        div()
            .id(("gear-set", index))
            .debug_selector(move || format!("gear-set-{index}"))
            .role(Role::ListItem)
            .aria_label(SharedString::from(format!("{}: {}", row.name, row.summary)))
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_raised)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_color(theme.text)
                            .font_weight(FontWeight::MEDIUM)
                            .child(row_name(row.name.clone())),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(theme.text_muted)
                            .child(row.summary.clone()),
                    ),
            )
            .child(edit)
            .child(wear)
    }

    /// The section above Worn.
    pub(super) fn render_sets(&self, theme: &Theme, cx: &mut Context<GearPanel>) -> Div {
        let no_one = self.items.character.is_empty();
        let new_set = button("gear-set-new", NEW_SET.into(), theme)
            .h(BUTTON_HEIGHT)
            .px_3()
            .child(NEW_SET);
        let new_set = if no_one {
            new_set.opacity(0.5).aria_description(NO_CHARACTER_NOTE)
        } else {
            new_set.on_click(cx.listener(|panel, _, window, cx| {
                panel.open_editor(None, window, cx);
            }))
        };
        let note = match (no_one, self.sets.is_empty()) {
            (true, _) => Some(NO_CHARACTER_NOTE),
            (false, true) => Some(NO_SETS_NOTE),
            (false, false) => None,
        };
        let note = note.map(|note| {
            div()
                .id("gear-sets-note")
                .debug_selector(|| "gear-sets-note".to_owned())
                .role(Role::Label)
                .aria_value(note)
                .text_size(px(12.))
                .text_color(theme.text_muted)
                .child(note)
        });
        let rows: Vec<_> = (self.sets.iter().enumerate())
            .map(|(index, row)| self.render_set(index, row, theme, cx))
            .collect();
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(section_label("GEAR SETS", theme))
                    .child(new_set),
            )
            .children(note)
            .child(
                div()
                    .id("gear-sets")
                    .role(Role::List)
                    .aria_label("Gear sets")
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(rows),
            )
    }

    fn small_button(
        &self,
        id: gpui::ElementId,
        label: String,
        shown: &'static str,
        theme: &Theme,
    ) -> Stateful<Div> {
        button(id, label.into(), theme)
            .h(SMALL_BUTTON_HEIGHT)
            .px_2()
            .text_size(px(12.))
            .text_color(theme.text)
            .child(shown)
    }

    /// The container row, and its choices while they're open.
    fn render_container(
        &self,
        editor: &Editor,
        theme: &Theme,
        cx: &mut Context<GearPanel>,
    ) -> impl IntoElement + use<> {
        let chosen = editor.container.as_deref().unwrap_or(KEEP_CARRIED);
        let open = editor.picking_container;
        let label = format!("{CONTAINER_LABEL}: {chosen}");
        let picker = button("gear-set-container", label.into(), theme)
            .aria_expanded(open)
            .h(BUTTON_HEIGHT)
            .px_2()
            .gap_1()
            .on_click(cx.listener(|panel, _, _, cx| {
                panel.edit_set(cx, |editor, _| {
                    editor.picking_container = !editor.picking_container;
                });
            }))
            .child(chosen.to_owned())
            .child("⌄");
        let choices = std::iter::once(None)
            .chain(editor.bags.iter().map(Some))
            .enumerate()
            .map(|(at, bag)| {
                let name = bag.map_or(KEEP_CARRIED, String::as_str).to_owned();
                let selected = editor.container.as_ref() == bag;
                let bag = bag.cloned();
                button(("gear-set-bag", at), name.clone().into(), theme)
                    .role(Role::RadioButton)
                    .aria_toggled(selected.into())
                    .h(SMALL_BUTTON_HEIGHT)
                    .px_2()
                    .text_size(px(12.))
                    .text_color(theme.text)
                    .when(selected, |button| button.bg(theme.selected))
                    .on_click(cx.listener(move |panel, _, _, cx| {
                        let bag = bag.clone();
                        panel.edit_set(cx, |editor, _| {
                            editor.container = bag;
                            editor.picking_container = false;
                        });
                    }))
                    .child(name)
            });
        let choices: Vec<_> = choices.collect();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .text_size(px(13.))
                    .text_color(theme.text)
                    .child(CONTAINER_LABEL)
                    .child(picker),
            )
            .when(open, |row| {
                row.child(
                    div()
                        .id("gear-set-bags")
                        .role(Role::RadioGroup)
                        .aria_label(CONTAINER_LABEL)
                        .flex()
                        .flex_wrap()
                        .gap_1()
                        .children(choices),
                )
            })
    }

    fn render_piece(
        &self,
        editor: &Editor,
        row: &PieceRow,
        theme: &Theme,
        cx: &mut Context<GearPanel>,
    ) -> impl IntoElement + use<> {
        let at = row.at;
        let name = row.name.clone();
        let drop = self
            .small_button(
                gpui::ElementId::NamedInteger("gear-piece-drop".into(), at as u64),
                format!("Drop {name}"),
                "Drop",
                theme,
            )
            .on_click(
                cx.listener(move |panel, event: &gpui::ClickEvent, window, cx| {
                    panel.edit_set(cx, |editor, _| {
                        editor.pieces.remove(at);
                    });
                    if event.is_keyboard() {
                        window.focus(&panel.focus_handle, cx);
                    }
                }),
            );
        let copy = row.copy.clone().filter(|_| row.seen);
        let exact = copy.map(|copy| {
            self.small_button(
                gpui::ElementId::NamedInteger("gear-piece-exact".into(), at as u64),
                format!("Exact copy of {name}"),
                "Exact",
                theme,
            )
            .role(Role::CheckBox)
            .aria_toggled(row.pinned.into())
            .when(row.pinned, |button| {
                button
                    .bg(theme.selected)
                    .border_color(theme.accent)
                    .text_color(theme.accent)
            })
            .on_click(cx.listener(move |panel, _, _, cx| {
                let copy = copy.clone();
                panel.edit_set(cx, |editor, _| {
                    let piece = &mut editor.pieces[at];
                    piece.copy_id = match piece.copy_id {
                        Some(_) => None,
                        None => Some(copy),
                    };
                });
            }))
        });
        let item = name.clone();
        let words = row.seen.then(|| {
            self.small_button(
                gpui::ElementId::NamedInteger("gear-piece-words".into(), at as u64),
                format!("Words for {name}"),
                "Words",
                theme,
            )
            .on_click(cx.listener(move |panel, _, window, cx| {
                panel.open_words(item.clone(), window, cx);
            }))
        });
        let detail_color = if row.seen {
            theme.text_muted
        } else {
            theme.warning
        };
        let index = at;
        let words_field = (editor.words_of.as_deref() == Some(row.name.as_str())).then(|| {
            let field = editor.words.clone();
            let save = self
                .small_button(
                    "gear-piece-words-save".into(),
                    SAVE_WORDS.to_owned(),
                    SAVE_WORDS,
                    theme,
                )
                .on_click(cx.listener(move |panel, _, window, cx| {
                    let words = field.read(cx).content().to_owned();
                    panel.save_words(words, window, cx);
                }));
            div()
                .flex()
                .flex_col()
                .gap_1()
                .pt_1()
                .child(editor.words.clone())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .text_size(px(12.))
                        .text_color(theme.text_muted)
                        .child(WORDS_NOTE)
                        .child(save),
                )
        });
        div()
            .id(("gear-piece", at))
            .debug_selector(move || format!("gear-piece-{index}"))
            .role(Role::ListItem)
            .aria_label(SharedString::from(format!("{}, {}", row.name, row.detail)))
            .flex()
            .flex_col()
            .py_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_color(theme.text)
                                    .child(row_name(row.name.clone().into())),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(detail_color)
                                    .child(row.detail.clone()),
                            ),
                    )
                    .children(exact)
                    .children(words)
                    .child(drop),
            )
            .children(words_field)
    }

    fn render_at_hand(
        &self,
        index: usize,
        candidate: &Candidate,
        theme: &Theme,
        cx: &mut Context<GearPanel>,
    ) -> impl IntoElement + use<> {
        let shown = place(candidate);
        let mut parts = vec![shown.clone()];
        parts.extend(known_words(&self.items, &candidate.name));
        let detail = parts.join(" · ");
        let pressed = candidate.clone();
        let add = self
            .small_button(
                gpui::ElementId::NamedInteger("gear-candidate-add".into(), index as u64),
                format!("Add {}, {shown}", candidate.name),
                "Add",
                theme,
            )
            .on_click(
                cx.listener(move |panel, event: &gpui::ClickEvent, window, cx| {
                    panel.edit_set(cx, |editor, items| {
                        add_piece(&mut editor.pieces, &pressed, items);
                    });
                    if event.is_keyboard() {
                        window.focus(&panel.focus_handle, cx);
                    }
                }),
            );
        div()
            .id(("gear-candidate", index))
            .role(Role::ListItem)
            .aria_label(SharedString::from(format!("{}, {detail}", candidate.name)))
            .flex()
            .items_center()
            .gap_1()
            .py_1()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_color(theme.text)
                            .child(row_name(candidate.name.clone().into())),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(theme.text_muted)
                            .child(detail),
                    ),
            )
            .child(add)
    }

    /// One stop that reads every line, and each line in the tree under it.
    fn render_preview(&self, editor: &Editor, theme: &Theme) -> impl IntoElement + use<> {
        let lines = preview_lines(&editor.preview, editor.pieces.len());
        let note = left_alone_note(editor.preview.left_alone);
        let spoken = format!("{PREVIEW_TITLE}: {}. {note}", lines.join(", "));
        let shown = lines.into_iter().enumerate().map(|(at, line)| {
            div()
                .id(("gear-preview-line", at))
                .debug_selector(move || format!("gear-preview-line-{at}"))
                .role(Role::Label)
                .aria_value(SharedString::from(line.clone()))
                .child(line)
        });
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(section_label(PREVIEW_TITLE, theme))
            .child(
                div()
                    .id("gear-preview")
                    .debug_selector(|| "gear-preview".to_owned())
                    .role(Role::Group)
                    .aria_label(SharedString::from(spoken))
                    .track_focus(&editor.preview_stop)
                    .focus_visible(|preview| preview.border_color(theme.accent))
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.surface_raised)
                    .font_family(theme.choice.font)
                    .text_size(px(12.))
                    .text_color(theme.text)
                    .children(shown),
            )
            .child(
                div()
                    .id("gear-preview-note")
                    .role(Role::Label)
                    .aria_value(SharedString::from(note.clone()))
                    .text_size(px(12.))
                    .text_color(theme.text_muted)
                    .child(note),
            )
    }

    fn render_editor_actions(
        &self,
        editor: &Editor,
        theme: &Theme,
        cx: &mut Context<GearPanel>,
    ) -> impl IntoElement + use<> {
        if editor.asking_to_delete {
            let name = editor.name.read(cx).content().trim().to_owned();
            let question = format!("Delete {name}? This can't be undone.");
            return div()
                .id("gear-set-delete-question")
                .role(Role::Group)
                .aria_label(SharedString::from(question.clone()))
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .text_size(px(13.))
                .text_color(theme.text)
                .child(question)
                .child(
                    button("gear-set-keep", "Keep it".into(), theme)
                        .track_focus(&editor.keep)
                        .h(BUTTON_HEIGHT)
                        .px_3()
                        .on_click(cx.listener(|panel, _, window, cx| {
                            panel.edit_set(cx, |editor, _| editor.asking_to_delete = false);
                            window.focus(&panel.focus_handle, cx);
                        }))
                        .child("Keep it"),
                )
                .child(
                    button("gear-set-delete-now", "Delete".into(), theme)
                        .h(BUTTON_HEIGHT)
                        .px_3()
                        .border_color(theme.warning)
                        .text_color(theme.warning)
                        .on_click(cx.listener(|panel, _, window, cx| {
                            panel.delete_set(window, cx);
                        }))
                        .child("Delete"),
                );
        }
        let wearing = button(
            "gear-set-use-worn",
            "Use what I'm wearing now".into(),
            theme,
        )
        .h(BUTTON_HEIGHT)
        .px_3()
        .on_click(cx.listener(|panel, _, _, cx| {
            panel.edit_set(cx, Editor::use_whats_worn);
        }))
        .child("Use what I'm wearing now");
        let keep = editor.keep.clone();
        let delete = (!editor.id.is_empty()).then(|| {
            button("gear-set-delete", "Delete set".into(), theme)
                .h(BUTTON_HEIGHT)
                .px_3()
                .on_click(cx.listener(move |panel, _, window, cx| {
                    panel.edit_set(cx, |editor, _| editor.asking_to_delete = true);
                    window.focus(&keep, cx);
                }))
                .child("Delete set")
        });
        let cancel = button("gear-set-cancel", "Cancel".into(), theme)
            .h(BUTTON_HEIGHT)
            .px_3()
            .on_click(cx.listener(|panel, _, window, cx| panel.close_editor(window, cx)))
            .child("Cancel");
        let can_save =
            !editor.pieces.is_empty() && !editor.name.read(cx).content().trim().is_empty();
        let save = button("gear-set-save", "Save".into(), theme)
            .h(BUTTON_HEIGHT)
            .px_3()
            .border_color(theme.accent)
            .text_color(theme.accent)
            .font_weight(FontWeight::MEDIUM)
            .child("Save");
        let save = if can_save {
            save.on_click(cx.listener(|panel, _, window, cx| panel.save_set(window, cx)))
        } else {
            save.opacity(0.5)
                .aria_description("A set needs a name and at least one piece.")
        };
        div()
            .id("gear-set-actions")
            .flex()
            .flex_wrap()
            .gap_2()
            .child(wearing)
            .children(delete)
            .child(cancel)
            .child(save)
    }

    pub(super) fn render_editor(
        &self,
        editor: &Editor,
        theme: &Theme,
        cx: &mut Context<GearPanel>,
    ) -> AnyElement {
        let heading = if editor.id.is_empty() {
            "Gear · New set"
        } else {
            "Gear · Edit set"
        };
        let pieces: Vec<_> = (editor.rows.iter())
            .map(|row| self.render_piece(editor, row, theme, cx))
            .collect();
        let at_hand: Vec<_> = (editor.at_hand.iter().enumerate())
            .map(|(index, candidate)| self.render_at_hand(index, candidate, theme, cx))
            .collect();
        div()
            .id("gear-set-editor")
            .debug_selector(|| "gear-set-editor".to_owned())
            .key_context(super::EDITOR_CONTEXT)
            .on_action(cx.listener(|panel, _: &super::CloseEditor, window, cx| {
                panel.close_editor(window, cx);
            }))
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .overflow_y_scroll()
            .child(super::render_heading(heading, theme))
            .child(editor.name.clone())
            .child(self.render_container(editor, theme, cx))
            .child(section_label("PIECES", theme))
            .child(
                div()
                    .id("gear-set-pieces")
                    .role(Role::List)
                    .aria_label("Pieces in this set")
                    .flex()
                    .flex_col()
                    .children(pieces),
            )
            .child(self.render_preview(editor, theme))
            .child(self.render_editor_actions(editor, theme, cx))
            .when(!at_hand.is_empty(), |editor_view| {
                editor_view.child(section_label("AT HAND", theme)).child(
                    div()
                        .id("gear-set-at-hand")
                        .role(Role::List)
                        .aria_label("Also at hand")
                        .flex()
                        .flex_col()
                        .children(at_hand),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measure::sample_items_with_sets;

    #[test]
    fn a_set_says_its_pieces_its_container_and_whats_not_seen() {
        let rows = set_rows(&sample_items_with_sets());
        assert_eq!(rows[0].name, "Levelling");
        assert_eq!(
            rows[0].summary,
            "4 pieces · what comes off goes in a canvas backpack"
        );
        assert_eq!(
            rows[1].summary,
            "3 pieces · what comes off stays carried · 1 not seen right now"
        );
    }

    #[test]
    fn a_full_slot_gives_up_the_piece_added_there_first() {
        let items = sample_items_with_sets();
        let robe = |item: &str| GearPiece {
            item: item.to_owned(),
            slot: Some("body".to_owned()),
            copy_id: None,
        };
        let mut pieces = vec![robe("a linen robe")];
        let candidate = Candidate {
            name: "a silk robe".to_owned(),
            copy: Some("9".to_owned()),
            worn_in: Some("body".to_owned()),
            bag: None,
        };
        add_piece(&mut pieces, &candidate, &items);
        let names: Vec<&str> = pieces.iter().map(|piece| piece.item.as_str()).collect();
        assert_eq!(names, ["a silk robe"]);
        assert_eq!(pieces[0].copy_id.as_deref(), Some("9"));
    }

    #[test]
    fn two_necklaces_both_stay_where_a_slot_holds_two() {
        let items = sample_items_with_sets();
        let necklace = |name: &str| Candidate {
            name: name.to_owned(),
            copy: None,
            worn_in: Some("neck".to_owned()),
            bag: None,
        };
        let mut pieces = Vec::new();
        add_piece(&mut pieces, &necklace("a jade necklace"), &items);
        add_piece(&mut pieces, &necklace("a pearl necklace"), &items);
        assert_eq!(pieces.len(), 2);
    }

    #[test]
    fn the_preview_says_what_it_cant_do_and_when_its_nothing() {
        let plan = WearPlan {
            commands: vec!["wear 1".to_owned()],
            missing: vec!["(Glowing) an alligator tooth".to_owned()],
            ..WearPlan::default()
        };
        assert_eq!(
            preview_lines(&plan, 2),
            ["wear 1", "can't find an alligator tooth"]
        );
        assert_eq!(preview_lines(&WearPlan::default(), 0), [NOTHING_CHOSEN]);
        assert_eq!(preview_lines(&WearPlan::default(), 2), [ALL_ON]);
    }

    #[test]
    fn the_preview_counts_what_it_leaves_alone() {
        assert!(left_alone_note(16).starts_with("16 pieces already on are left alone."));
        assert!(left_alone_note(1).starts_with("1 piece already on is left alone."));
        assert!(left_alone_note(0).starts_with("Pieces already on are left alone."));
    }
}
