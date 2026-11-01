//! The notes through the views: what's typed, where it's saved, and what the
//! keyboard reaches.

use super::*;
use crate::map_view::harness::{grid_map, noted_map, room, versioned};
use crate::room_panel::press_key;
use gpui::{Modifiers, TestAppContext, VisualTestContext, WindowAppearance};
use mudling_core::map::{MapChange, Whereabouts};
use mudling_core::store::Revision;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

type Log = Rc<RefCell<Vec<Heard>>>;

#[derive(Clone, Debug, PartialEq)]
enum Heard {
    Edit(NoteEdit),
    Walk(String),
    GoTo(RoomId),
}

struct Opened<'a> {
    notes: Entity<Notes>,
    heard: Log,
    cx: &'a mut VisualTestContext,
}

fn open(cx: &mut TestAppContext) -> Opened<'_> {
    cx.update(|cx| {
        crate::input::bind_keys(cx);
        crate::shell::bind_keys(cx);
        cx.set_global(Theme::for_appearance(WindowAppearance::Dark));
    });
    let heard = Log::default();
    let log = heard.clone();
    let slot = Rc::new(RefCell::new(None));
    let kept = slot.clone();
    let (_, cx) = cx.add_window_view(|_, cx| {
        let notes = cx.new(Notes::new);
        cx.subscribe(&notes, move |_, _, event: &NotesEvent, _| {
            log.borrow_mut().push(match event {
                NotesEvent::Edit(edit) => Heard::Edit(edit.clone()),
                NotesEvent::Walk(command) => Heard::Walk(command.clone()),
                NotesEvent::GoTo(room) => Heard::GoTo(*room),
            });
        })
        .detach();
        *kept.borrow_mut() = Some(notes.clone());
        NotesPanel::new(notes, cx)
    });
    let notes = slot.borrow_mut().take().expect("the view was built");
    notes.update(cx, |notes, cx| {
        notes.use_map(versioned(grid_map(3)), cx);
        notes.show_server("mud.example", "", cx);
    });
    cx.run_until_parked();
    Opened { notes, heard, cx }
}

impl Opened<'_> {
    fn edit(&mut self, target: NoteTarget) {
        self.notes.update_in(self.cx, |notes, window, cx| {
            notes.edit(target, Host::NotesTab, window, cx);
        });
        self.cx.run_until_parked();
    }

    fn type_text(&mut self, text: &str) {
        self.cx.simulate_input(text);
        self.cx.run_until_parked();
    }

    fn content(&mut self) -> String {
        self.notes.read_with(self.cx, |notes, cx| {
            notes.editor.read(cx).content().to_owned()
        })
    }

    fn wait_out_the_typing(&mut self) {
        self.cx.executor().advance_clock(SAVE_AFTER);
        self.cx.run_until_parked();
    }

    fn apply(&mut self, change: MapChange) {
        let diff = Diff {
            revision: Revision(2),
            change,
        };
        self.notes
            .update_in(self.cx, |notes, window, cx| notes.apply(&diff, window, cx));
        self.cx.run_until_parked();
    }

    fn move_to(&mut self, room: RoomId) {
        self.apply(MapChange {
            whereabouts: Some(Whereabouts {
                room: Some(room),
                ..Whereabouts::default()
            }),
            ..MapChange::default()
        });
    }

    fn click(&mut self, selector: &'static str) {
        let at = self.cx.debug_bounds(selector).expect(selector).center();
        self.cx.simulate_click(at, Modifiers::none());
        self.cx.run_until_parked();
    }

    fn heard(&self) -> Vec<Heard> {
        self.heard.borrow().clone()
    }
}

fn note_on(room: RoomId, text: &str) -> Heard {
    Heard::Edit(NoteEdit::Map(MapEdit::Note {
        room,
        text: text.to_owned(),
    }))
}

fn noted(id: RoomId, x: i64, note: &str) -> Arc<MapRoom> {
    Arc::new(MapRoom {
        notes: note.to_owned(),
        ..room(id, x, 0)
    })
}

#[gpui::test]
fn nothing_goes_out_until_the_typing_stops(cx: &mut TestAppContext) {
    let mut window = open(cx);
    window.edit(NoteTarget::Room(0));
    for key in ["h", "e", "r", "b", "s"] {
        window.type_text(key);
        window.cx.executor().advance_clock(SAVE_AFTER / 2);
        window.cx.run_until_parked();
    }
    assert!(window.heard().is_empty(), "every key is still typing");

    window.wait_out_the_typing();
    assert_eq!(window.heard(), [note_on(0, "herbs")]);

    window.wait_out_the_typing();
    assert_eq!(window.heard().len(), 1, "nothing new to save");
}

#[gpui::test]
fn an_unsaved_note_stays_with_its_room_when_the_room_changes(cx: &mut TestAppContext) {
    let mut window = open(cx);
    window.edit(NoteTarget::Room(0));
    window.type_text("a hidden door");

    window.move_to(4);
    window.apply(MapChange {
        rooms: vec![noted(4, 1, "somewhere else")],
        ..MapChange::default()
    });
    assert_eq!(window.content(), "a hidden door");
    assert!(window.cx.debug_bounds("Done").is_some(), "still editing");

    window.wait_out_the_typing();
    assert_eq!(window.heard(), [note_on(0, "a hidden door")]);
}

#[gpui::test]
fn the_store_catching_up_leaves_what_is_typed_alone(cx: &mut TestAppContext) {
    let mut window = open(cx);
    window.edit(NoteTarget::Room(0));
    window.type_text("first");
    window.wait_out_the_typing();
    window.type_text(" and second");
    window.apply(MapChange {
        rooms: vec![noted(0, 0, "first")],
        ..MapChange::default()
    });
    assert_eq!(window.content(), "first and second");
}

#[gpui::test]
fn a_note_whose_room_leaves_the_map_moves_to_the_notepad(cx: &mut TestAppContext) {
    let mut window = open(cx);
    window.notes.update(window.cx, |notes, cx| {
        notes.show_server("mud.example", "Goals", cx);
    });
    window.edit(NoteTarget::Room(5));
    window.type_text("a hidden door");

    window.apply(MapChange {
        removed: vec![5],
        ..MapChange::default()
    });

    let target = window
        .notes
        .read_with(window.cx, |notes, _| notes.draft_target());
    assert_eq!(target, Some(NoteTarget::Notepad));
    let kept = "Goals\n\nSquare 2, 1: a hidden door";
    assert_eq!(window.content(), kept);
    assert_eq!(
        window.heard(),
        [Heard::Edit(NoteEdit::Rules(RuleEdit::SetNotepad(
            kept.to_owned()
        )))]
    );
}

#[gpui::test]
fn escape_saves_and_closes_the_editor(cx: &mut TestAppContext) {
    let mut window = open(cx);
    window.edit(NoteTarget::Room(0));
    window.type_text("herbs");
    window.cx.simulate_keystrokes("escape");
    window.cx.run_until_parked();

    assert_eq!(window.heard(), [note_on(0, "herbs")]);
    let target = window
        .notes
        .read_with(window.cx, |notes, _| notes.draft_target());
    assert_eq!(target, None);
}

#[gpui::test]
fn return_starts_a_line_and_sends_nothing(cx: &mut TestAppContext) {
    let mut window = open(cx);
    window.edit(NoteTarget::Notepad);
    window.type_text("quit");
    window.cx.simulate_keystrokes("enter");
    window.type_text("look");
    assert_eq!(window.content(), "quit\nlook");
    assert!(window.heard().is_empty());
}

#[gpui::test]
fn a_session_ending_takes_what_is_typed_once(cx: &mut TestAppContext) {
    let mut window = open(cx);
    window.edit(NoteTarget::Room(0));
    window.type_text("herbs");
    let unsaved = |notes: &mut Notes| notes.unsaved();
    let taken = window.notes.update(window.cx, |notes, _| unsaved(notes));
    assert_eq!(
        taken,
        Some(NoteEdit::Map(MapEdit::Note {
            room: 0,
            text: "herbs".to_owned()
        }))
    );
    let again = window.notes.update(window.cx, |notes, _| unsaved(notes));
    assert_eq!(again, None);
}

#[gpui::test]
fn spaces_alone_clear_a_note(cx: &mut TestAppContext) {
    let mut window = open(cx);
    window.apply(MapChange {
        rooms: vec![noted(0, 0, "herbs")],
        ..MapChange::default()
    });
    window.edit(NoteTarget::Room(0));
    window.notes.update_in(window.cx, |notes, window, cx| {
        notes.editor.update(cx, |editor, cx| {
            editor.show("   ".to_owned(), 3, window, cx);
        });
    });
    window.wait_out_the_typing();
    assert_eq!(window.heard(), [note_on(0, "")]);
}

#[gpui::test]
fn a_row_goes_to_its_room_and_walks_there(cx: &mut TestAppContext) {
    let mut window = open(cx);
    window.apply(MapChange {
        rooms: vec![noted(8, 2, "kill all;quit")],
        ..MapChange::default()
    });
    assert_eq!(
        window
            .notes
            .read_with(window.cx, |notes, _| notes.rows().len()),
        1
    );

    window.click("Go to Square 2, 0");
    assert_eq!(window.heard(), [Heard::GoTo(8)]);

    window.click("Walk to Square 2, 0");
    let heard = window.heard();
    let Some(Heard::Walk(command)) = heard.last() else {
        panic!("no walk in {heard:?}");
    };
    assert!(
        command.chars().all(|c| "nsewud, []".contains(c)),
        "{command}"
    );
}

#[gpui::test]
fn a_walk_says_why_it_went_nowhere(cx: &mut TestAppContext) {
    let mut window = open(cx);
    window.apply(MapChange {
        rooms: vec![noted(0, 0, "here"), noted(90, 9, "an island")],
        ..MapChange::default()
    });
    window.click("Walk to Square 0, 0");
    let message = |window: &mut Opened| {
        let notes = window.notes.clone();
        notes.read_with(window.cx, |notes, _| notes.message().map(str::to_owned))
    };
    assert_eq!(
        message(&mut window).as_deref(),
        Some("You're already there.")
    );

    window.click("Walk to Square 9, 0");
    assert_eq!(
        message(&mut window).as_deref(),
        Some("No explored path leads there.")
    );
    assert!(window.heard().is_empty());
}

#[gpui::test]
fn the_keyboard_reaches_the_note_and_every_row(cx: &mut TestAppContext) {
    let mut window = open(cx);
    window.apply(MapChange {
        rooms: vec![noted(8, 2, "herbs")],
        ..MapChange::default()
    });
    window.cx.update(|window, cx| window.focus_next(cx));
    window.cx.run_until_parked();
    press_key("space", window.cx);
    window.cx.run_until_parked();
    assert_eq!(
        window
            .notes
            .read_with(window.cx, |notes, _| notes.draft_target()),
        Some(NoteTarget::Room(0)),
        "the first stop is the room's own note"
    );
    window.cx.simulate_keystrokes("escape");
    window.cx.run_until_parked();

    // Closing the editor let go of focus, so this starts over from the note.
    for _ in 0..2 {
        window.cx.update(|window, cx| window.focus_next(cx));
        window.cx.run_until_parked();
    }
    window.cx.simulate_keystrokes("down enter w");
    window.cx.run_until_parked();
    let heard = window.heard();
    assert!(heard.contains(&Heard::GoTo(8)), "{heard:?}");
    assert!(
        heard.iter().any(|heard| matches!(heard, Heard::Walk(_))),
        "{heard:?}"
    );
}

#[gpui::test]
fn only_the_rows_in_view_are_drawn_and_the_arrows_reach_the_rest(cx: &mut TestAppContext) {
    let window = open(cx);
    window.notes.update(window.cx, |notes, cx| {
        notes.use_map(versioned(noted_map(20)), cx);
    });
    window.cx.run_until_parked();
    assert_eq!(
        window
            .notes
            .read_with(window.cx, |notes, _| notes.rows().len()),
        400
    );
    assert!(window.cx.debug_bounds("Go to Square 0, 0").is_some());
    assert!(window.cx.debug_bounds("Go to Square 9, 9").is_none());

    let list = window
        .notes
        .read_with(window.cx, |notes, _| notes.list.clone());
    window.cx.update(|window, cx| {
        window.focus(&list.read(cx).focus_handle.clone(), cx);
    });
    window.cx.run_until_parked();
    for _ in 0..400 {
        window.cx.simulate_keystrokes("down");
    }
    window.cx.simulate_keystrokes("enter");
    window.cx.run_until_parked();
    let last = window.notes.read_with(window.cx, |notes, _| {
        notes.rows().last().map(|row| row.room)
    });
    assert_eq!(window.heard(), [Heard::GoTo(last.expect("a last row"))]);
}
