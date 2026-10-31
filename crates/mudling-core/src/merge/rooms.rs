//! One room found on two devices, under two ids.
//!
//! A game with no room numbers gives the map nothing to agree on, so each
//! device names the rooms it finds. Two rooms are one when their whole identity
//! matches (name, zone, description and exits) and exactly one room on each
//! side has it. The lower id stays, everything that hung off the higher one
//! moves to it, and a tombstone clears the higher one from any device.
//!
//! Where either side has look-alikes, they all stay: a duplicate on the map is
//! visible and fixable, but a wrong union loses a room.

use crate::map::{RoomId, UNNUMBERED_IDS};
use crate::persist::{Key, MapRows, Timestamp, Tombstone};
use crate::repo::format::ServerRows;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

type Identity<'a> = (&'a str, &'a str, &'a str, BTreeSet<&'a str>);

/// Moves both sides onto one id for each room they share, so the merge after
/// this sees two copies of one record.
pub(super) fn unite_rooms(ours: &mut ServerRows, theirs: &mut ServerRows) {
    let united = pairs(ours, theirs);
    if united.is_empty() {
        return;
    }
    // The tombstone outlives a long-unchanged room for as long as any other.
    let now = Timestamp::now();
    let retired: Vec<Tombstone> = ours
        .map
        .rooms
        .iter()
        .chain(&theirs.map.rooms)
        .filter(|room| united.contains_key(&room.id))
        .map(|room| Tombstone {
            key: Key::Room(room.id),
            deleted: now.max(Timestamp(room.updated.0 + 1)),
        })
        .collect();
    for side in [&mut *ours, &mut *theirs] {
        repoint_rows(&mut side.map, &united);
        for tombstone in &mut side.tombstones {
            repoint_key(&mut tombstone.key, &united);
        }
    }
    ours.tombstones.extend(retired);
}

/// Each higher id, with the lower id it's one room with. A room both sides
/// already hold under one id needs nothing, and a lower room deleted on
/// purpose stays deleted, leaving the other room as it is.
fn pairs(ours: &ServerRows, theirs: &ServerRows) -> BTreeMap<RoomId, RoomId> {
    let (our_ids, their_ids) = (room_ids(ours), room_ids(theirs));
    let deleted = room_deletes(ours, theirs);
    let survives = |id: RoomId| {
        let room = ours.map.rooms.iter().chain(&theirs.map.rooms);
        let updated = room.filter(|room| room.id == id).map(|room| room.updated);
        updated.max() >= deleted.get(&id).copied()
    };
    let their_rooms = identities(theirs);
    identities(ours)
        .iter()
        .filter_map(|(identity, mine)| {
            let ([a], [b]) = (mine.as_slice(), their_rooms.get(identity)?.as_slice()) else {
                return None;
            };
            let (kept, retired) = (*a.min(b), *a.max(b));
            let apart = !our_ids.contains(b) && !their_ids.contains(a);
            (apart && survives(kept)).then_some((retired, kept))
        })
        .collect()
}

fn room_ids(rows: &ServerRows) -> HashSet<RoomId> {
    rows.map.rooms.iter().map(|room| room.id).collect()
}

fn room_deletes(ours: &ServerRows, theirs: &ServerRows) -> HashMap<RoomId, Timestamp> {
    let mut deleted = HashMap::new();
    for tombstone in ours.tombstones.iter().chain(&theirs.tombstones) {
        if let Key::Room(id) = tombstone.key {
            let newest = deleted.entry(id).or_default();
            *newest = tombstone.deleted.max(*newest);
        }
    }
    deleted
}

/// The rooms with no game number, grouped by what makes them that room.
fn identities(rows: &ServerRows) -> HashMap<Identity<'_>, Vec<RoomId>> {
    let mut exits: HashMap<RoomId, BTreeSet<&str>> = HashMap::new();
    for exit in &rows.map.exits {
        exits
            .entry(exit.room_id)
            .or_default()
            .insert(&exit.direction);
    }
    let mut rooms: HashMap<Identity<'_>, Vec<RoomId>> = HashMap::new();
    for room in rows.map.rooms.iter().filter(|r| r.id >= UNNUMBERED_IDS) {
        let directions = exits.remove(&room.id).unwrap_or_default();
        rooms
            .entry((&room.area, &room.name, &room.description, directions))
            .or_default()
            .push(room.id);
    }
    rooms
}

fn repoint_rows(map: &mut MapRows, united: &BTreeMap<RoomId, RoomId>) {
    let repoint = |id: &mut RoomId| *id = united.get(id).copied().unwrap_or(*id);
    for room in &mut map.rooms {
        repoint(&mut room.id);
        room.building.iter_mut().for_each(repoint);
    }
    map.exits
        .iter_mut()
        .for_each(|row| repoint(&mut row.room_id));
    map.doorways
        .iter_mut()
        .for_each(|row| repoint(&mut row.room_id));
    map.notes
        .iter_mut()
        .for_each(|row| repoint(&mut row.room_id));
    map.features
        .iter_mut()
        .for_each(|row| repoint(&mut row.room_id));
    map.sightings
        .iter_mut()
        .for_each(|row| repoint(&mut row.room_id));
    for link in &mut map.links {
        repoint(&mut link.room_id);
        repoint(&mut link.to_room);
    }
}

/// A delete of something in the room still counts once the room has moved.
fn repoint_key(key: &mut Key, united: &BTreeMap<RoomId, RoomId>) {
    if let Key::Exit { room_id, .. }
    | Key::Link { room_id, .. }
    | Key::Doorway { room_id, .. }
    | Key::RoomNote(room_id)
    | Key::RoomFeature { room_id, .. }
    | Key::Sighting { room_id, .. } = key
    {
        *room_id = united.get(room_id).copied().unwrap_or(*room_id);
    }
}
