//! Rooms the player says are one, or says are not.
//!
//! On a game with no room numbers a duplicate that has synced stays for good
//! (see `merge::rooms`), so the player tidies up by hand. MUDling only points
//! at rooms that read alike: it never joins anything by itself.
//!
//! A join is a record, `map.joined.<retired id>` = `<kept id>`, and every merge
//! on every device moves whatever hangs off the retired room onto the kept one
//! for as long as the record stands. So an edit made on another device before
//! it heard of the join lands on the kept room rather than going with the
//! retired one. The lower id stays, as in a union, so two devices joining the
//! same pair make the same record.
//!
//! What a join does to the rows is in `merge::join`.

use super::geometry::DirSet;
use super::layout::{UNNUMBERED_IDS, id_prefix};
use super::room::{MapRoom, RoomId};
use crate::persist::{Meta, Timestamp};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// A join's record, one for each retired room.
pub(crate) const JOINED: &str = "map.joined.";
/// The player's "Not the same", one for each pair of rooms.
pub(crate) const NOT_SAME: &str = "map.not-same.";
/// This device's own: which rooms a saved version undoes, by the version's
/// time. Saved versions stay on the device, so a join can be undone only on
/// the device that made it.
const UNDO: &str = "map.join-undo.";

/// The kind of version saved before a join, which undoing it reads back.
pub const VERSION_JOIN: &str = "join";

/// Two rooms, lower id first.
pub type RoomPair = (RoomId, RoomId);

/// `first` and `second`, lower id first.
pub fn room_pair(first: RoomId, second: RoomId) -> RoomPair {
    (first.min(second), first.max(second))
}

pub(crate) fn joined_key(retired: RoomId) -> String {
    format!("{JOINED}{retired}")
}

pub(crate) fn not_same_key((low, high): RoomPair) -> String {
    format!("{NOT_SAME}{low}.{high}")
}

pub(crate) fn undo_key(created: Timestamp) -> String {
    format!("{UNDO}{}", created.0)
}

pub(crate) fn undo_value((kept, retired): RoomPair) -> String {
    format!("{kept} {retired}")
}

/// The rooms a saved version undoes, from its [`undo_key`] row.
pub(crate) fn undo_pair(meta: &[Meta], created: Timestamp) -> Option<RoomPair> {
    let key = undo_key(created);
    let row = meta.iter().find(|entry| entry.key == key)?;
    let (kept, retired) = row.value.split_once(' ')?;
    Some((kept.parse().ok()?, retired.parse().ok()?))
}

/// One join's record, if `row` is one. The lower id always stays, so a record
/// that says otherwise isn't one MUDling wrote: sync reads what any device or
/// hand left in the repo.
pub(crate) fn join_record(row: &Meta) -> Option<(RoomId, RoomId, Timestamp)> {
    let retired: RoomId = row.key.strip_prefix(JOINED)?.parse().ok()?;
    let kept: RoomId = row.value.parse().ok()?;
    (kept < retired).then_some((retired, kept, row.updated))
}

/// Each retired room with the room that holds it now, which a later join may
/// have moved again. A cycle ends where it would come back round.
pub(crate) fn follow_joins(joins: &BTreeMap<RoomId, RoomId>) -> BTreeMap<RoomId, RoomId> {
    joins
        .keys()
        .map(|retired| {
            let mut kept = *retired;
            let mut seen = BTreeSet::from([kept]);
            while let Some(next) = joins.get(&kept).filter(|next| seen.insert(**next)) {
                kept = *next;
            }
            (*retired, kept)
        })
        .collect()
}

/// The joins in `meta`, each retired room with the room holding it now.
pub(crate) fn joins_in(meta: &[Meta]) -> BTreeMap<RoomId, RoomId> {
    let records = meta.iter().filter_map(join_record);
    let joins = records.map(|(retired, kept, _)| (retired, kept)).collect();
    follow_joins(&joins)
}

/// The pairs the player said aren't the same, as the rooms that hold them now.
pub(crate) fn not_same_in(meta: &[Meta]) -> BTreeSet<RoomPair> {
    let joins = joins_in(meta);
    let now_held = |id: RoomId| joins.get(&id).copied().unwrap_or(id);
    meta.iter()
        .filter_map(|row| {
            let (low, high) = row.key.strip_prefix(NOT_SAME)?.split_once('.')?;
            let (low, high) = (low.parse().ok()?, high.parse().ok()?);
            Some(room_pair(now_held(low), now_held(high)))
        })
        .collect()
}

/// Whether the map may join `id`: only a room the game gave no number.
pub fn can_join(id: RoomId) -> bool {
    id >= UNNUMBERED_IDS
}

/// Which device mapped a room, as far as its id says.
pub fn mapper_of(id: RoomId) -> i64 {
    id_prefix(id)
}

/// What the map thinks may be duplicates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Duplicates {
    /// The game gives no room numbers, so its rooms can be joined at all. A
    /// game's rules can't say: with no plugin they read numbers it may not send.
    pub joinable: bool,
    /// Each pair to ask about, by lowest id.
    pub pairs: Vec<RoomPair>,
    /// Every room in `pairs`.
    pub rooms: BTreeSet<RoomId>,
}

/// Rooms sharing a name, zone, description and exits, on a game with no room
/// numbers, without the pairs the player answered. This walks every room, so
/// run it off the UI thread.
pub fn likely_duplicates<'a>(
    rooms: impl Iterator<Item = &'a MapRoom>,
    not_same: &BTreeSet<RoomPair>,
) -> Duplicates {
    type Reading<'a> = (&'a str, &'a str, &'a str, DirSet);
    let mut alike: HashMap<Reading<'a>, Vec<RoomId>> = HashMap::new();
    let mut any = false;
    for room in rooms {
        if !can_join(room.id) {
            return Duplicates::default();
        }
        any = true;
        let reading = (
            room.zone.as_str(),
            room.name.as_str(),
            room.description.as_str(),
            room.exit_directions(),
        );
        alike.entry(reading).or_default().push(room.id);
    }
    // Each room is asked about once, with the next room that may be it. A game
    // can hold thousands of rooms that read alike, and every pair of them is
    // far more than anyone will answer: each answer brings the next pair up.
    let mut pairs = Vec::new();
    for ids in alike.values_mut().filter(|ids| ids.len() > 1) {
        ids.sort_unstable();
        for (index, first) in ids.iter().enumerate() {
            let others = ids[index + 1..].iter();
            let second = others
                .into_iter()
                .find(|id| !not_same.contains(&(*first, **id)));
            pairs.extend(second.map(|second| (*first, *second)));
        }
    }
    pairs.sort_unstable();
    let rooms = pairs.iter().flat_map(|(a, b)| [*a, *b]).collect();
    Duplicates {
        joinable: any,
        pairs,
        rooms,
    }
}

/// What a join is called, in the map's note and in map history.
pub(crate) fn join_label(kept: &str, retired: &str) -> String {
    if kept == retired {
        format!("Joined two copies of {kept}")
    } else {
        format!("Joined {kept} and {retired}")
    }
}

/// How the player's last join or undo went, for the note over the map.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LastJoin {
    /// Counts up, so a view can tell one answer from the next.
    pub attempt: u64,
    pub outcome: Result<JoinDone, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JoinDone {
    Joined {
        label: String,
        /// Names it to undo.
        created: Timestamp,
    },
    Undone,
}

/// What views need to tidy up duplicates, apart from the rooms.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JoinFacts {
    pub not_same: BTreeSet<RoomPair>,
    /// [`mapper_of`] this device.
    pub this_device: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alike(ids: std::ops::Range<RoomId>) -> Vec<MapRoom> {
        ids.map(|id| MapRoom::empty(UNNUMBERED_IDS + id)).collect()
    }

    fn pairs(rooms: &[MapRoom], not_same: &[RoomPair]) -> Vec<RoomPair> {
        let not_same: BTreeSet<RoomPair> = not_same
            .iter()
            .map(|(a, b)| (UNNUMBERED_IDS + a, UNNUMBERED_IDS + b))
            .collect();
        let found = likely_duplicates(rooms.iter(), &not_same).pairs;
        let local = found.into_iter();
        local
            .map(|(a, b)| (a - UNNUMBERED_IDS, b - UNNUMBERED_IDS))
            .collect()
    }

    #[test]
    fn thousands_of_look_alikes_make_a_list_as_long_as_they_are() {
        let rooms = alike(0..10_000);
        assert_eq!(pairs(&rooms, &[]).len(), 9_999);
    }

    #[test]
    fn each_answer_brings_up_the_next_pair_until_all_are_answered() {
        let rooms = alike(0..3);
        assert_eq!(pairs(&rooms, &[]), [(0, 1), (1, 2)]);
        assert_eq!(pairs(&rooms, &[(0, 1)]), [(0, 2), (1, 2)]);
        assert_eq!(pairs(&rooms, &[(0, 1), (0, 2)]), [(1, 2)]);
        assert!(pairs(&rooms, &[(0, 1), (0, 2), (1, 2)]).is_empty());
    }

    fn record(retired: RoomId, kept: RoomId) -> Meta {
        Meta {
            key: joined_key(retired),
            value: kept.to_string(),
            updated: Timestamp(1),
        }
    }

    #[test]
    fn a_record_keeping_anything_but_the_lower_id_is_no_join() {
        assert!(join_record(&record(5, 4)).is_some());
        assert!(join_record(&record(5, 5)).is_none());
        assert!(join_record(&record(5, 6)).is_none());
        let joins = joins_in(&[record(5, 5), record(6, 7), record(7, 6), record(9, 8)]);
        assert_eq!(joins, BTreeMap::from([(7, 6), (9, 8)]));
    }

    #[test]
    fn following_stops_on_a_cycle() {
        let cycle = BTreeMap::from([(5, 6), (6, 7), (7, 5), (9, 5)]);
        let held = follow_joins(&cycle);
        assert_eq!(held.len(), 4, "every room gets an answer");
    }

    #[test]
    fn following_goes_on_through_a_chain() {
        let chain = BTreeMap::from([(9, 7), (7, 5)]);
        assert_eq!(follow_joins(&chain), BTreeMap::from([(7, 5), (9, 5)]));
    }

    #[test]
    fn rooms_that_differ_in_any_way_are_not_duplicates() {
        let mut rooms = alike(0..2);
        rooms[1].description = "It reads differently".to_owned();
        assert!(pairs(&rooms, &[]).is_empty());
    }
}
