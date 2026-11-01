//! The notes as the panel lists them, with no gpui in it.
//!
//! It holds a copy of the map store, fed by store diffs, and keeps the rows
//! in step with only the rooms each diff names: a frame never scans the map.
//! The one scan is of a session's first snapshot, which a player opens once.

use mudling_core::map::{MapRoom, RoomId};
use mudling_core::store::{Diff, MapState, Versioned};

/// A room with a note.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoteRow {
    pub room: RoomId,
    pub name: String,
    pub area: String,
    pub note: String,
}

impl NoteRow {
    fn of(room: &MapRoom) -> NoteRow {
        NoteRow {
            room: room.id,
            name: room.name.clone(),
            area: room.zone.clone(),
            note: room.notes.clone(),
        }
    }

    /// Rows read by area, then by room.
    fn sorts_before(&self, other: &NoteRow) -> bool {
        (&self.area, &self.name, self.room) < (&other.area, &other.name, other.room)
    }
}

#[derive(Default)]
pub struct NoteBook {
    map: Versioned<MapState>,
    rows: Vec<NoteRow>,
}

impl NoteBook {
    /// Starts from a snapshot of the store.
    pub fn new(snapshot: Versioned<MapState>) -> NoteBook {
        let mut rows: Vec<NoteRow> = snapshot
            .value()
            .rooms()
            .filter(|room| !room.notes.is_empty())
            .map(|room| NoteRow::of(room))
            .collect();
        rows.sort_by(|a, b| (&a.area, &a.name, a.room).cmp(&(&b.area, &b.name, b.room)));
        NoteBook {
            map: snapshot,
            rows,
        }
    }

    /// Takes a frame's diff of the map store. Returns whether a row changed.
    pub fn apply(&mut self, diff: &Diff<MapState>) -> bool {
        self.map.apply_diff(diff);
        let mut changed = false;
        for room in &diff.change.rooms {
            changed |= self.place(room);
        }
        for id in &diff.change.removed {
            changed |= self.take_out(*id);
        }
        changed
    }

    fn place(&mut self, room: &MapRoom) -> bool {
        let at = self.rows.iter().position(|row| row.room == room.id);
        if room.notes.is_empty() {
            return at.is_some() && self.take_out(room.id);
        }
        let row = NoteRow::of(room);
        if at.is_some_and(|at| self.rows[at] == row) {
            return false;
        }
        self.take_out(room.id);
        let at = self.rows.partition_point(|other| other.sorts_before(&row));
        self.rows.insert(at, row);
        true
    }

    fn take_out(&mut self, room: RoomId) -> bool {
        let before = self.rows.len();
        self.rows.retain(|row| row.room != room);
        self.rows.len() != before
    }

    pub fn rows(&self) -> &[NoteRow] {
        &self.rows
    }

    pub fn room(&self, id: RoomId) -> Option<&MapRoom> {
        self.map.value().room(id).map(|room| &**room)
    }

    /// The player's room.
    pub fn here(&self) -> Option<&MapRoom> {
        self.room(self.map.value().whereabouts().room?)
    }

    pub fn map(&self) -> &MapState {
        self.map.value()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map_view::harness::{grid_map, room, versioned};
    use mudling_core::map::MapChange;
    use mudling_core::store::Revision;
    use std::sync::Arc;

    fn noted(id: RoomId, note: &str) -> MapRoom {
        MapRoom {
            notes: note.to_owned(),
            ..room(id, id, 0)
        }
    }

    fn diff(rooms: Vec<MapRoom>, removed: Vec<RoomId>) -> Diff<MapState> {
        Diff {
            revision: Revision(2),
            change: MapChange {
                revision: 2,
                rooms: rooms.into_iter().map(Arc::new).collect(),
                removed,
                ..MapChange::default()
            },
        }
    }

    fn listed(book: &NoteBook) -> Vec<(RoomId, &str)> {
        let rows = book.rows().iter();
        rows.map(|row| (row.room, row.note.as_str())).collect()
    }

    #[test]
    fn a_note_comes_and_goes_with_the_rooms_a_diff_names() {
        let mut book = NoteBook::new(versioned(grid_map(3)));
        assert!(book.rows().is_empty());

        assert!(book.apply(&diff(vec![noted(1, "herbs")], Vec::new())));
        assert!(book.apply(&diff(vec![noted(0, "a well")], Vec::new())));
        assert_eq!(listed(&book), [(0, "a well"), (1, "herbs")]);

        assert!(book.apply(&diff(vec![noted(1, "more herbs")], Vec::new())));
        assert_eq!(listed(&book), [(0, "a well"), (1, "more herbs")]);

        assert!(book.apply(&diff(vec![noted(1, "")], Vec::new())));
        assert!(book.apply(&diff(Vec::new(), vec![0])));
        assert!(book.rows().is_empty());
    }

    #[test]
    fn a_room_changing_without_its_note_changes_no_row() {
        let mut book = NoteBook::new(versioned(grid_map(3)));
        book.apply(&diff(vec![noted(1, "herbs")], Vec::new()));
        assert!(!book.apply(&diff(vec![noted(1, "herbs")], Vec::new())));
        assert!(!book.apply(&diff(vec![noted(2, "")], Vec::new())));
    }
}
