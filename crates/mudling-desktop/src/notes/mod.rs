//! Notes (PRD §8): a note on each room and a notepad for the server, in plain
//! text, shared by every character on it.
//!
//! [`Notes`] holds them and the one [`Draft`] being edited. It keeps the words
//! on the UI thread and the keys in memory: a save goes out a moment after the
//! typing stops, as an event the shell queues for the core, which writes it on
//! the persistence writer's thread. A draft belongs to the room it was opened
//! on, so walking on while typing changes nothing about where it's saved.
//!
//! Three views draw them: [`NotesPanel`], the right panel's Notes tab, and a
//! [`RoomCard`] for a room's note and features, in the Room panel and under
//! the map. Only one of them holds the editor at a time.
//!
//! Nothing here can send a command. What's typed leaves only as a
//! [`MapEdit`] or [`RuleEdit`]; a Walk's command is made of a route's
//! directions and none of the note's words.

mod book;

use crate::assets::HEADING_FONT;
use crate::input::CommandInput;
use crate::room_panel::{button, row_name, section_label};
use crate::theme::Theme;
use book::NoteBook;
pub use book::NoteRow;
use gpui::{
    AnyView, App, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, Hsla,
    KeyBinding, Role, ScrollStrategy, SharedString, Stateful, StyleRefinement, Subscription, Task,
    UniformListScrollHandle, WeakEntity, Window, actions, div, prelude::*, px, uniform_list,
};
use mudling_core::map::{MapRoom, RoomId, WalkStep, feature, walk_command};
use mudling_core::session::{MapEdit, RuleEdit};
use mudling_core::store::{Diff, MapState, Versioned};
use std::collections::BTreeSet;
use std::ops::Range;
use std::time::Duration;

const EDITOR_CONTEXT: &str = "NoteEditor";
const LIST_CONTEXT: &str = "NoteList";
const NOTE_ROW_HEIGHT: gpui::Pixels = px(62.);
/// How long the typing has to stop before what's typed is saved.
const SAVE_AFTER: Duration = Duration::from_millis(400);
const BUTTON_HEIGHT: gpui::Pixels = px(28.);
const NOTEPAD_MOST_HEIGHT: gpui::Pixels = px(132.);
const NOTE_PLACEHOLDER: &str = "What's worth remembering?";
const FOOTER: &str = "Plain text, saved as you type. Never sent to the game.";
/// What a note says of a room that left the map under it.
const MOVED_TO_NOTEPAD: &str = "That room left the map, so this note moved to the notepad.";

actions!(
    notes,
    [FinishNote, PreviousNote, NextNote, GoToNote, WalkToNote]
);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", FinishNote, Some(EDITOR_CONTEXT)),
        KeyBinding::new("up", PreviousNote, Some(LIST_CONTEXT)),
        KeyBinding::new("down", NextNote, Some(LIST_CONTEXT)),
        KeyBinding::new("enter", GoToNote, Some(LIST_CONTEXT)),
        KeyBinding::new("w", WalkToNote, Some(LIST_CONTEXT)),
    ]);
}

/// What a draft is saved to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteTarget {
    Room(RoomId),
    Notepad,
}

/// Where the editor sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Host {
    NotesTab,
    RoomPanel,
    Map,
}

/// The note being edited. The words are in [`Notes::editor`]; `text` is a
/// copy from the last key.
struct Draft {
    target: NoteTarget,
    host: Host,
    /// The room's name, which outlasts the room.
    heading: String,
    text: String,
    /// What was last handed to the core.
    sent: String,
}

/// A change for the session to save.
#[derive(Clone, Debug, PartialEq)]
pub enum NoteEdit {
    Map(MapEdit),
    Rules(RuleEdit),
}

pub enum NotesEvent {
    Edit(NoteEdit),
    /// A walk to send, as speedwalk.
    Walk(String),
    /// Show this room on the map.
    GoTo(RoomId),
}

impl EventEmitter<NotesEvent> for Notes {}

/// A player's word on a feature, in the order the card offers them.
fn feature_label(name: &str) -> &'static str {
    match name {
        feature::AGGRESSIVE_MOB => "Aggressive",
        feature::FOUNTAIN => "Fountain",
        feature::SHOP => "Shop",
        feature::TRAINER => "Trainer",
        feature::DANGER => "Danger",
        _ => "Gear",
    }
}

pub struct Notes {
    book: NoteBook,
    notepad: String,
    server: SharedString,
    editor: Entity<CommandInput>,
    list: Entity<NoteList>,
    draft: Option<Draft>,
    /// Waits out the typing, then saves.
    saving: Option<Task<()>>,
    walking: Option<Task<()>>,
    /// What came of the last Walk, or of a room leaving the map.
    message: Option<SharedString>,
    _edited: Subscription,
}

impl Notes {
    pub fn new(cx: &mut Context<Notes>) -> Notes {
        let editor = cx.new(|cx| CommandInput::notepad("Note", NOTE_PLACEHOLDER, cx));
        let edited = cx.observe(&editor, |notes, _, cx| notes.edited(cx));
        let this = cx.weak_entity();
        Notes {
            book: NoteBook::default(),
            notepad: String::new(),
            server: SharedString::default(),
            list: cx.new(|cx| NoteList {
                notes: this,
                rows: Vec::new(),
                focus_handle: cx.focus_handle().tab_stop(true),
                scroll: UniformListScrollHandle::new(),
                at: None,
            }),
            editor,
            draft: None,
            saving: None,
            walking: None,
            message: None,
            _edited: edited,
        }
    }

    /// Starts over on another session's map. Any draft is the old session's:
    /// the shell saves it with [`Notes::unsaved`] before it lets go.
    pub fn use_map(&mut self, snapshot: Versioned<MapState>, cx: &mut Context<Notes>) {
        self.book = NoteBook::new(snapshot);
        self.draft = None;
        self.saving = None;
        self.walking = None;
        self.message = None;
        self.show_rows(cx);
        cx.notify();
    }

    /// Takes a frame's diff of the map store.
    pub fn apply(&mut self, diff: &Diff<MapState>, window: &mut Window, cx: &mut Context<Notes>) {
        if self.book.apply(diff) {
            self.show_rows(cx);
        }
        self.rescue_draft(window, cx);
        cx.notify();
    }

    /// The server's name and notepad, from the rules store.
    pub fn show_server(&mut self, server: &str, notepad: &str, cx: &mut Context<Notes>) {
        if self.server != server || self.notepad != notepad {
            self.server = server.to_owned().into();
            notepad.clone_into(&mut self.notepad);
            cx.notify();
        }
    }

    fn show_rows(&self, cx: &mut Context<Notes>) {
        let rows = self.book.rows().to_vec();
        self.list.update(cx, |list, cx| list.show(rows, cx));
    }

    /// A draft's room can leave the map under it, when the map finds it was
    /// the same as another or goes back to a saved version. The words move to
    /// the notepad, where they overwrite nothing.
    fn rescue_draft(&mut self, window: &mut Window, cx: &mut Context<Notes>) {
        let gone = |room| self.book.room(room).is_none();
        let Some(draft) = self.draft.as_mut().filter(|draft| match draft.target {
            NoteTarget::Room(room) => gone(room),
            NoteTarget::Notepad => false,
        }) else {
            return;
        };
        if draft.text.trim().is_empty() {
            self.draft = None;
            return;
        }
        let rescued = format!("{}: {}", draft.heading, draft.text);
        let text = match self.notepad.is_empty() {
            true => rescued,
            false => format!("{}\n\n{rescued}", self.notepad),
        };
        draft.target = NoteTarget::Notepad;
        draft.sent = self.notepad.clone();
        draft.text = text.clone();
        self.message = Some(MOVED_TO_NOTEPAD.into());
        self.editor.update(cx, |editor, cx| {
            let end = text.len();
            editor.show(text, end, window, cx);
        });
        self.save(cx);
    }

    // MARK: Editing

    /// Opens the editor on `target`, in `host`, after saving any other draft.
    pub fn edit(
        &mut self,
        target: NoteTarget,
        host: Host,
        window: &mut Window,
        cx: &mut Context<Notes>,
    ) {
        self.save(cx);
        let (heading, text) = match target {
            NoteTarget::Room(room) => {
                let Some(room) = self.book.room(room) else {
                    return;
                };
                (room.name.clone(), room.notes.clone())
            }
            NoteTarget::Notepad => (String::new(), self.notepad.clone()),
        };
        self.message = None;
        self.draft = Some(Draft {
            target,
            host,
            heading,
            sent: text.clone(),
            text: text.clone(),
        });
        self.editor.update(cx, |editor, cx| {
            let end = text.len();
            editor.show(text, end, window, cx);
        });
        cx.notify();
        // It can't take focus until it's drawn.
        cx.spawn_in(window, async move |notes, cx| {
            notes
                .update_in(cx, |notes, window, cx| {
                    window.focus(&notes.editor.focus_handle(cx), cx);
                })
                .ok();
        })
        .detach();
    }

    /// Saves and closes the editor. Typing goes back to the command bar.
    pub fn finish(&mut self, cx: &mut Context<Notes>) {
        self.save(cx);
        self.saving = None;
        self.draft = None;
        cx.notify();
    }

    fn edited(&mut self, cx: &mut Context<Notes>) {
        let content = self.editor.read(cx).content();
        let Some(draft) = self.draft.as_mut().filter(|draft| draft.text != content) else {
            return;
        };
        content.clone_into(&mut draft.text);
        let wait = cx.background_executor().timer(SAVE_AFTER);
        self.saving = Some(cx.spawn(async move |notes, cx| {
            wait.await;
            notes.update(cx, |notes, cx| notes.save(cx)).ok();
        }));
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Notes>) {
        if let Some(edit) = self.unsaved() {
            cx.emit(NotesEvent::Edit(edit));
            cx.notify();
        }
    }

    /// What's typed and not yet handed to the core, marked as handed over. A
    /// shell ending a session takes this itself: an event would come too late.
    pub fn unsaved(&mut self) -> Option<NoteEdit> {
        let draft = self.draft.as_mut()?;
        let text = draft.text.trim_end();
        if text == draft.sent {
            return None;
        }
        text.clone_into(&mut draft.sent);
        Some(match draft.target {
            NoteTarget::Room(room) => NoteEdit::Map(MapEdit::Note {
                room,
                text: text.to_owned(),
            }),
            NoteTarget::Notepad => NoteEdit::Rules(RuleEdit::SetNotepad(text.to_owned())),
        })
    }

    /// The words on record for `target`, as the stores show them.
    fn stored(&self, target: NoteTarget) -> &str {
        match target {
            NoteTarget::Room(room) => self.book.room(room).map_or("", |room| &room.notes),
            NoteTarget::Notepad => &self.notepad,
        }
    }

    /// Whether the stores show what's typed: the core has it, and its writer
    /// thread reports anything that goes wrong from there.
    fn is_saved(&self) -> bool {
        self.draft
            .as_ref()
            .is_none_or(|draft| draft.text.trim_end() == self.stored(draft.target))
    }

    /// The draft, if `host` holds the editor and it's on `target`.
    fn draft_in(&self, host: Host, target: NoteTarget) -> Option<&Draft> {
        let draft = self.draft.as_ref()?;
        (draft.host == host && draft.target == target).then_some(draft)
    }

    /// The room a card in `host` should stay on while a draft is open there.
    fn draft_room(&self, host: Host) -> Option<RoomId> {
        match self
            .draft
            .as_ref()
            .filter(|draft| draft.host == host)?
            .target
        {
            NoteTarget::Room(room) => Some(room),
            NoteTarget::Notepad => None,
        }
    }

    // MARK: Going there

    /// Works out the way to `room` off the UI thread, then walks it.
    pub fn walk(&mut self, room: RoomId, cx: &mut Context<Notes>) {
        let Some(start) = self.book.here().map(|here| here.id) else {
            self.say("The map doesn't know where you are yet.", cx);
            return;
        };
        if start == room {
            self.say("You're already there.", cx);
            return;
        }
        let map = self.book.map().clone();
        let found = cx.background_spawn(async move { map.route(start, room) });
        self.walking = Some(cx.spawn(async move |notes, cx| {
            let steps = found.await;
            notes
                .update(cx, |notes, cx| notes.walk_found(start, steps, cx))
                .ok();
        }));
    }

    /// A player who moved on while the way was worked out isn't where it
    /// starts, so it's dropped.
    fn walk_found(&mut self, start: RoomId, steps: Option<Vec<WalkStep>>, cx: &mut Context<Notes>) {
        if self.book.here().map(|here| here.id) != Some(start) {
            return;
        }
        match steps {
            Some(steps) => {
                self.message = None;
                cx.emit(NotesEvent::Walk(walk_command(&steps)));
                cx.notify();
            }
            None => self.say("No explored path leads there.", cx),
        }
    }

    fn say(&mut self, message: &'static str, cx: &mut Context<Notes>) {
        self.message = Some(message.into());
        cx.notify();
    }

    /// The editor, for a measured run to type in.
    pub fn editor(&self) -> &Entity<CommandInput> {
        &self.editor
    }

    #[cfg(test)]
    pub fn draft_target(&self) -> Option<NoteTarget> {
        self.draft.as_ref().map(|draft| draft.target)
    }

    #[cfg(test)]
    pub fn rows(&self) -> &[NoteRow] {
        self.book.rows()
    }

    #[cfg(test)]
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }
}

/// A plain button with a word on it.
fn text_button(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    name: SharedString,
    theme: &Theme,
) -> Stateful<Div> {
    button(id, name, theme)
        .h(BUTTON_HEIGHT)
        .px_3()
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.accent)
        .child(label)
}

/// The note's words, in a box of their own.
fn note_box(theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .bg(theme.surface_raised)
}

/// A line that says how things stand. It's no live region: a screen reader
/// shouldn't speak over the typing each time a note saves.
fn about_line(id: &'static str, text: SharedString, color: Hsla) -> impl IntoElement {
    div()
        .id(id)
        .role(Role::Label)
        .aria_value(text.clone())
        .text_size(px(12.))
        .text_color(color)
        .child(text)
}

/// A line that says what just happened, which a screen reader speaks.
fn status_line(id: &'static str, text: SharedString, color: Hsla) -> impl IntoElement {
    div()
        .id(id)
        .role(Role::Status)
        .aria_label(text.clone())
        .text_size(px(12.))
        .text_color(color)
        .child(text)
}

/// The note on one room, or the notepad, with the button that edits it.
fn render_note(
    notes: &Notes,
    entity: &Entity<Notes>,
    target: NoteTarget,
    host: Host,
    theme: &Theme,
) -> Div {
    if notes.draft_in(host, target).is_some() {
        let finish = entity.clone();
        return note_box(theme)
            .child(
                div()
                    .key_context(EDITOR_CONTEXT)
                    .on_action(move |_: &FinishNote, _, cx| {
                        finish.update(cx, |notes, cx| notes.finish(cx));
                    })
                    .child(notes.editor.clone()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(about_line(
                        "note-status",
                        if notes.is_saved() {
                            "Saved"
                        } else {
                            "Saving…"
                        }
                        .into(),
                        theme.text_muted,
                    ))
                    .child(
                        text_button("note-done", "Done", "Done".into(), theme).on_click({
                            let done = entity.clone();
                            move |_, _, cx| done.update(cx, |notes, cx| notes.finish(cx))
                        }),
                    ),
            );
    }
    let text = match notes.draft.as_ref().filter(|draft| draft.target == target) {
        Some(draft) => draft.text.clone(),
        None => notes.stored(target).to_owned(),
    };
    let (label, name) = match (target, text.is_empty()) {
        (NoteTarget::Notepad, _) => ("Edit", "Edit the notepad"),
        (NoteTarget::Room(_), true) => ("Add a note here", "Add a note here"),
        (NoteTarget::Room(_), false) => ("Edit", "Edit the note"),
    };
    let open = entity.clone();
    let id = match host {
        Host::NotesTab if target == NoteTarget::Notepad => "edit-notepad",
        Host::NotesTab => "edit-note-in-tab",
        Host::RoomPanel => "edit-note-in-room",
        Host::Map => "edit-note-on-map",
    };
    let mut body = note_box(theme);
    if !text.is_empty() {
        let shown = SharedString::from(text);
        let words = div()
            .id(("note-text", id.len()))
            .role(Role::Label)
            .aria_value(shown.clone())
            .text_size(px(14.))
            .text_color(theme.text)
            .child(shown);
        body = body.child(if target == NoteTarget::Notepad {
            words.max_h(NOTEPAD_MOST_HEIGHT).overflow_y_scroll()
        } else {
            words
        });
    }
    body.child(
        div()
            .flex()
            .child(
                text_button(id, label, name.into(), theme).on_click(move |_, window, cx| {
                    open.update(cx, |notes, cx| notes.edit(target, host, window, cx));
                }),
            ),
    )
}

/// The Notes tab.
pub struct NotesPanel {
    focus_handle: FocusHandle,
    notes: Entity<Notes>,
    _changed: Subscription,
}

impl NotesPanel {
    pub fn new(notes: Entity<Notes>, cx: &mut Context<NotesPanel>) -> NotesPanel {
        let changed = cx.observe(&notes, |_, _, cx| cx.notify());
        NotesPanel {
            focus_handle: cx.focus_handle(),
            notes,
            _changed: changed,
        }
    }

    /// Moves the keyboard to the panel's first button, or to the panel itself
    /// when it has none.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<NotesPanel>) {
        window.focus(&self.focus_handle, cx);
        window.focus_next(cx);
        if !self.focus_handle.contains_focused(window, cx) {
            window.focus(&self.focus_handle, cx);
        }
    }
}

impl Render for NotesPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<NotesPanel>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let entity = self.notes.clone();
        let notes = self.notes.read(cx);
        let saved = if notes.is_saved() {
            "saved"
        } else {
            "saving…"
        };
        let about = format!("{} · shared by your characters · {saved}", notes.server);
        let here = notes
            .draft_room(Host::NotesTab)
            .and_then(|room| notes.book.room(room))
            .or(notes.book.here());
        let heading = match here {
            Some(room) => format!("THIS ROOM · {}", room.name.to_uppercase()),
            None => "THIS ROOM".to_owned(),
        };
        let moved_on = notes.draft_room(Host::NotesTab).is_some()
            && notes.draft_room(Host::NotesTab) != notes.book.here().map(|room| room.id);
        let this_room = match here {
            Some(room) => {
                let target = NoteTarget::Room(room.id);
                render_note(notes, &entity, target, Host::NotesTab, &theme)
            }
            None => div()
                .text_color(theme.text_muted)
                .child("Walk somewhere to leave a note on it."),
        };
        let list = notes.list.clone();
        let count = notes.book.rows().len();
        let notepad = render_note(notes, &entity, NoteTarget::Notepad, Host::NotesTab, &theme);
        let message = notes.message.clone();
        let list: AnyView = list.into();
        let list = if window.is_a11y_active() {
            list.into_any_element()
        } else {
            list.cached(StyleRefinement::default().size_full())
                .into_any_element()
        };
        div()
            .id("notes-panel")
            .debug_selector(|| "notes-panel".to_owned())
            .track_focus(&self.focus_handle)
            // The mouse never moves focus here: typing stays with the bar.
            .capture_any_mouse_down(|_, window, _| window.prevent_default())
            .role(Role::Group)
            .aria_label("Notes")
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .pt_2()
            .text_size(px(14.))
            .text_color(theme.text)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .id("notes-heading")
                            .role(Role::Heading)
                            .aria_level(2)
                            .aria_label("Notes")
                            .font_family(HEADING_FONT)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_size(px(21.))
                            .child("Notes"),
                    )
                    .child(about_line("notes-about", about.into(), theme.text_muted)),
            )
            .child(section_label(&heading, &theme))
            .when(moved_on, |panel| {
                panel.child(about_line(
                    "note-moved-on",
                    "You've moved on. This note stays with the room it was opened on.".into(),
                    theme.text_muted,
                ))
            })
            .child(this_room)
            .child(section_label(&format!("ALL NOTES · {count}"), &theme))
            .children(
                message.map(|message| status_line("notes-message", message, theme.text_muted)),
            )
            .child(div().flex_1().min_h_0().child(list))
            .child(section_label("NOTEPAD", &theme))
            .child(notepad)
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(theme.text_muted)
                    .child(FOOTER),
            )
    }
}

/// Every room's note. A view of its own, cached, so typing in the editor
/// above doesn't draw a long list again. It draws only the rows in view: even
/// a cached row costs every frame. The keyboard has one stop here and moves
/// along the list with the arrows. A screen reader gets every row, each with
/// its buttons, since it reads what's drawn.
pub struct NoteList {
    notes: WeakEntity<Notes>,
    rows: Vec<NoteRow>,
    focus_handle: FocusHandle,
    scroll: UniformListScrollHandle,
    /// The row the arrows are on.
    at: Option<usize>,
}

impl NoteList {
    fn show(&mut self, rows: Vec<NoteRow>, cx: &mut Context<NoteList>) {
        self.rows = rows;
        self.at = self.at.filter(|at| *at < self.rows.len());
        cx.notify();
    }

    fn step(&mut self, forward: bool, cx: &mut Context<NoteList>) {
        let last = self.rows.len().checked_sub(1);
        let at = match (self.at, forward) {
            (None, _) => last.map(|_| 0),
            (Some(at), true) => last.map(|last| (at + 1).min(last)),
            (Some(at), false) => Some(at.saturating_sub(1)),
        };
        self.at = at;
        if let Some(at) = at {
            self.scroll.scroll_to_item(at, ScrollStrategy::Nearest);
        }
        cx.notify();
    }

    fn room_at(&self) -> Option<RoomId> {
        Some(self.rows.get(self.at?)?.room)
    }

    fn render_row(
        &self,
        index: usize,
        row: &NoteRow,
        by_tab: bool,
        theme: &Theme,
    ) -> impl IntoElement + use<> {
        let room = row.room;
        let name = SharedString::from(row.name.clone());
        let first_line = row.note.lines().next().unwrap_or_default().to_owned();
        let (go, walk) = (self.notes.clone(), self.notes.clone());
        let summary = format!("{}: {}", row.name, row.note);
        div()
            .id(("note-row", index))
            .role(Role::ListItem)
            .aria_label(summary)
            .h(NOTE_ROW_HEIGHT)
            .flex()
            .items_center()
            .gap_2()
            .when(self.at == Some(index), |row| row.bg(theme.hover))
            .child(
                button(("note-go", index), format!("Go to {name}").into(), theme)
                    .tab_stop(by_tab)
                    .flex_1()
                    .min_w_0()
                    .flex_col()
                    .items_start()
                    .px_2()
                    .py_1()
                    .on_click(move |_, _, cx| {
                        go.update(cx, |_, cx| cx.emit(NotesEvent::GoTo(room))).ok();
                    })
                    .child(row_name(name.clone()))
                    .child(
                        div()
                            .w_full()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(12.))
                            .text_color(theme.text_muted)
                            .child(first_line),
                    ),
            )
            .child(
                text_button(
                    ("note-walk", index),
                    "Walk",
                    format!("Walk to {name}").into(),
                    theme,
                )
                .tab_stop(by_tab)
                .on_click(move |_, _, cx| {
                    walk.update(cx, |notes, cx| notes.walk(room, cx)).ok();
                }),
            )
    }
}

impl Render for NoteList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<NoteList>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let list = div()
            .id("All notes")
            .role(Role::List)
            .aria_label("All notes")
            .size_full();
        if self.rows.is_empty() {
            return list
                .text_color(theme.text_muted)
                .child("A note left on a room shows here.")
                .into_any_element();
        }
        if window.is_a11y_active() {
            let rows = self.rows.iter().enumerate();
            return list
                .overflow_y_scroll()
                .children(rows.map(|(index, row)| self.render_row(index, row, true, &theme)))
                .into_any_element();
        }
        let in_view = uniform_list(
            "note-rows",
            self.rows.len(),
            cx.processor(move |list, range: Range<usize>, _, _| {
                range
                    .map(|index| list.render_row(index, &list.rows[index], false, &theme))
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.scroll)
        .size_full();
        list.debug_selector(|| "note-list".to_owned())
            .key_context(LIST_CONTEXT)
            .track_focus(&self.focus_handle)
            .rounded_md()
            .border_1()
            .border_color(theme.surface)
            .focus_visible(|list| list.border_color(theme.accent_hover))
            .on_action(cx.listener(|list, _: &PreviousNote, _, cx| list.step(false, cx)))
            .on_action(cx.listener(|list, _: &NextNote, _, cx| list.step(true, cx)))
            .on_action(cx.listener(|list, _: &GoToNote, _, cx| {
                if let Some(room) = list.room_at() {
                    list.notes
                        .update(cx, |_, cx| cx.emit(NotesEvent::GoTo(room)))
                        .ok();
                }
            }))
            .on_action(cx.listener(|list, _: &WalkToNote, _, cx| {
                if let Some(room) = list.room_at() {
                    list.notes.update(cx, |notes, cx| notes.walk(room, cx)).ok();
                }
            }))
            .child(in_view)
            .into_any_element()
    }
}

/// A room's note and features: the Room panel's for the player's room, the
/// map's for the room selected on it.
pub struct RoomCard {
    notes: Entity<Notes>,
    host: Host,
    /// The room selected, for a card that doesn't follow the player.
    selected: Option<RoomId>,
    _changed: Subscription,
}

impl RoomCard {
    pub fn new(notes: Entity<Notes>, host: Host, cx: &mut Context<RoomCard>) -> RoomCard {
        let changed = cx.observe(&notes, |_, _, cx| cx.notify());
        RoomCard {
            notes,
            host,
            selected: None,
            _changed: changed,
        }
    }

    pub fn show(&mut self, room: Option<RoomId>, cx: &mut Context<RoomCard>) {
        self.selected = room;
        cx.notify();
    }

    fn room<'a>(&self, notes: &'a Notes) -> Option<&'a MapRoom> {
        let open = notes.draft_room(self.host);
        match self.host {
            Host::Map => open
                .or(self.selected)
                .and_then(|room| notes.book.room(room)),
            _ => open
                .and_then(|room| notes.book.room(room))
                .or(notes.book.here()),
        }
    }

    fn render_features(
        &self,
        id: RoomId,
        features: &BTreeSet<String>,
        theme: &Theme,
        cx: &mut Context<RoomCard>,
    ) -> impl IntoElement {
        let host = self.host as usize;
        let toggles = feature::PLAYER_SET.iter().enumerate().map(|(index, name)| {
            let name = *name;
            let on = features.contains(name);
            let label = feature_label(name);
            let verb = if on { "Clear" } else { "Set" };
            button(
                ("feature", host * 10 + index),
                format!("{verb} {label}").into(),
                theme,
            )
            .aria_toggled(on.into())
            .h(px(24.))
            .px_2()
            .text_size(px(12.))
            .text_color(if on { theme.accent } else { theme.text_muted })
            .when(on, |toggle| toggle.bg(theme.selected))
            .on_click(cx.listener(move |card, _, _, cx| {
                card.notes.update(cx, |_, cx| {
                    cx.emit(NotesEvent::Edit(NoteEdit::Map(MapEdit::Feature {
                        room: id,
                        feature: name.to_owned(),
                        present: !on,
                    })));
                });
            }))
            .child(label)
        });
        div()
            .id(("features", host))
            .role(Role::Group)
            .aria_label("Room features")
            .flex()
            .flex_wrap()
            .gap_1()
            .children(toggles.collect::<Vec<_>>())
    }
}

/// A room card's frame. The mouse never moves focus here, under the map or in
/// the Room panel: typing stays with the bar.
fn card() -> Div {
    div()
        .debug_selector(|| "room-card".to_owned())
        .capture_any_mouse_down(|_, window, _| window.prevent_default())
        .flex()
        .flex_col()
        .gap_2()
}

impl Render for RoomCard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<RoomCard>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let entity = self.notes.clone();
        let notes = self.notes.read(cx);
        // The card keeps a draft moved to the notepad: the editor can't vanish
        // from under the typing and leave the next words to the command bar.
        if notes.draft_in(self.host, NoteTarget::Notepad).is_some() {
            let message = notes.message.clone();
            return card()
                .children(message.map(|text| status_line("card-message", text, theme.text_muted)))
                .child(section_label("NOTEPAD", &theme))
                .child(render_note(
                    notes,
                    &entity,
                    NoteTarget::Notepad,
                    self.host,
                    &theme,
                ))
                .into_any_element();
        }
        let Some(room) = self.room(notes) else {
            return div().into_any_element();
        };
        let (id, name, features) = (room.id, room.name.clone(), room.features.clone());
        let note = render_note(notes, &entity, NoteTarget::Room(id), self.host, &theme);
        let message = notes.message.clone().filter(|_| self.host == Host::Map);
        let features = self.render_features(id, &features, &theme, cx);
        let walk = entity.clone();
        card()
            .when(self.host == Host::Map, |card| {
                card.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(row_name(name.clone().into()))
                        .child(
                            text_button(
                                "map-walk",
                                "Walk",
                                format!("Walk to {name}").into(),
                                &theme,
                            )
                            .on_click(move |_, _, cx| {
                                walk.update(cx, |notes, cx| notes.walk(id, cx));
                            }),
                        ),
                )
            })
            .children(message.map(|text| status_line("card-message", text, theme.text_muted)))
            .child(section_label("NOTE", &theme))
            .child(note)
            .child(section_label("FEATURES", &theme))
            .child(features)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests;
