//! One room found on two devices, under two ids.
//!
//! A game with no room numbers gives the map nothing to agree on, so each
//! device names the rooms it finds. Reading alike doesn't make two rooms one,
//! because a game can hold two corridors that read the same. When unsure, both
//! stay: a duplicate on the map is visible and fixable, but a wrong union loses
//! a room.
//!
//! Two rooms unite only if all of this holds:
//! - Their whole identity (name, zone, description and exits) matches, exactly
//!   one room on each side has it, and each id is on one side only.
//! - They share a link: the same direction to or from one room, or a pair that
//!   unites too. Coordinates can't say this, because each device counts them
//!   from its own first room.
//! - No direction leads to two rooms that stay apart.
//! - No room would come to reach another by two different directions, which
//!   the two sides don't each show. A maze gets no pass.
//!
//! The lower id stays, everything that hung off the higher one moves to it, and
//! a tombstone clears the higher one from any device.
//!
//! The rules depend on each other: a link is evidence only if the rooms at its
//! far end unite, and a union can break another. So the pairs are worked out
//! as sets, dropping what breaks a rule and then keeping what's proven until
//! nothing changes. Nothing depends on the order of rooms or of the two sides,
//! so both devices reach one result.
//!
//! After a first sync, devices share rooms, and a new room off one has a link
//! to prove it. Two devices that have never synced share no room, so a first
//! [`join`](super::join) of two separate maps keeps both copies of any room
//! they both found. A union runs only while the copies are on one side each, so
//! such a duplicate stays until the player joins the two rooms by hand.

use crate::map::{CURRENT_ROOM, RoomId, UNNUMBERED_IDS};
use crate::persist::{Batch, Change, Key, MapRows, Record, Timestamp, Tombstone};
use crate::repo::format::ServerRows;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

type Identity<'a> = (&'a str, &'a str, &'a str, BTreeSet<&'a str>);

/// Rooms gone from a server, each with the room that took its place, or `None`
/// if the room was deleted.
pub type RoomMoves = BTreeMap<RoomId, Option<RoomId>>;

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

/// The rooms in `before` that `after` no longer holds, each with the room that
/// took its place: the one that reads alike, whichever device united them. A
/// room with no such match was deleted, and maps to `None`.
pub(super) fn retired_rooms(before: &ServerRows, after: &ServerRows) -> RoomMoves {
    let held = room_ids(after);
    let mut gone: RoomMoves = before
        .map
        .rooms
        .iter()
        .filter(|room| !held.contains(&room.id))
        .map(|room| (room.id, None))
        .collect();
    if gone.is_empty() {
        return gone;
    }
    let successors = identities(after);
    for (identity, ids) in identities(before) {
        let Some([kept]) = successors.get(&identity).map(Vec::as_slice) else {
            continue;
        };
        for id in ids {
            if let Some(place) = gone.get_mut(&id) {
                *place = Some(*kept);
            }
        }
    }
    gone
}

/// Puts `moves` on rows a session wrote while its rooms were being merged:
/// what it wrote about a retired room lands on the room that took its place,
/// and what it wrote about a deleted room goes, so a child with no parent can't
/// fail the whole batch.
pub(crate) fn repoint_batch(batch: &mut Batch, moves: &RoomMoves) {
    if moves.is_empty() {
        return;
    }
    batch.changes.retain_mut(|change| match change {
        Change::Upsert(record) => repoint_record(record, moves),
        Change::Delete { key, .. } => match key {
            Key::Room(id) => !moves.contains_key(id),
            key => repoint_owned_key(key, moves),
        },
        _ => true,
    });
}

/// Moves one record off retired rooms. `false` if it has nothing to hang off.
fn repoint_record(record: &mut Record, moves: &RoomMoves) -> bool {
    let moved = |id: &mut RoomId| match moves.get(id) {
        Some(Some(kept)) => {
            *id = *kept;
            true
        }
        Some(None) => false,
        None => true,
    };
    match record {
        Record::Room(room) => {
            room.building.take_if(|building| !moved(building));
            moved(&mut room.id)
        }
        Record::Exit(row) => moved(&mut row.room_id),
        Record::Link(row) => moved(&mut row.room_id) && moved(&mut row.to_room),
        Record::Doorway(row) => moved(&mut row.room_id),
        Record::RoomNote(row) => moved(&mut row.room_id),
        Record::RoomFeature(row) => moved(&mut row.room_id),
        Record::Sighting(row) => moved(&mut row.room_id),
        Record::Meta(row) if row.key == CURRENT_ROOM => {
            let id = row.value.parse::<RoomId>();
            match id.ok().map(|id| moves.get(&id)) {
                Some(Some(Some(kept))) => row.value = kept.to_string(),
                Some(Some(None)) => return false,
                _ => {}
            }
            true
        }
        _ => true,
    }
}

/// The same for the key of something in a room.
fn repoint_owned_key(key: &mut Key, moves: &RoomMoves) -> bool {
    if let Key::Exit { room_id, .. }
    | Key::Link { room_id, .. }
    | Key::Doorway { room_id, .. }
    | Key::RoomNote(room_id)
    | Key::RoomFeature { room_id, .. }
    | Key::Sighting { room_id, .. } = key
        && let Some(moved) = moves.get(room_id)
    {
        let Some(kept) = moved else {
            return false;
        };
        *room_id = *kept;
    }
    true
}

/// Each higher id, with the lower id it's one room with.
fn pairs(ours: &ServerRows, theirs: &ServerRows) -> BTreeMap<RoomId, RoomId> {
    let (our_side, their_side) = (Side::new(ours), Side::new(theirs));
    let mut pairs = candidates(ours, theirs);
    loop {
        let sound = without_broken(pairs.clone(), &our_side, &their_side);
        let proven = with_evidence(&sound, &our_side, &their_side);
        if proven == pairs {
            return pairs.iter().map(|p| (p.retired(), p.kept())).collect();
        }
        pairs = proven;
    }
}

/// A room on each side that may be one room.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Pair {
    ours: RoomId,
    theirs: RoomId,
}

impl Pair {
    fn kept(self) -> RoomId {
        self.ours.min(self.theirs)
    }

    fn retired(self) -> RoomId {
        self.ours.max(self.theirs)
    }
}

type Links<'a> = HashMap<RoomId, Vec<(&'a str, RoomId)>>;

/// What one side knows of its map, for looking rooms up by id.
struct Side<'a> {
    /// Each room's links, as direction and the room at the far end.
    out_of: Links<'a>,
    /// The links into each room, as direction and where they start.
    into: Links<'a>,
}

impl Side<'_> {
    fn new(rows: &ServerRows) -> Side<'_> {
        let mut side = Side {
            out_of: Links::new(),
            into: Links::new(),
        };
        for link in &rows.map.links {
            let direction = link.direction.as_str();
            let out = side.out_of.entry(link.room_id).or_default();
            out.push((direction, link.to_room));
            let inward = side.into.entry(link.to_room).or_default();
            inward.push((direction, link.room_id));
        }
        side
    }
}

/// The one id each room in `pairs` goes by.
fn united_ids(pairs: &BTreeSet<Pair>) -> HashMap<RoomId, RoomId> {
    pairs
        .iter()
        .flat_map(|pair| [(pair.ours, pair.kept()), (pair.theirs, pair.kept())])
        .collect()
}

fn one_id(united: &HashMap<RoomId, RoomId>, id: RoomId) -> RoomId {
    united.get(&id).copied().unwrap_or(id)
}

/// Rooms that read alike, unique on each side. A room both sides already hold
/// under one id needs nothing, and a lower room deleted on purpose stays
/// deleted, leaving the other room as it is.
fn candidates(ours: &ServerRows, theirs: &ServerRows) -> BTreeSet<Pair> {
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
            let pair = Pair {
                ours: *a,
                theirs: *b,
            };
            let apart = !our_ids.contains(b) && !their_ids.contains(a);
            (apart && survives(pair.kept())).then_some(pair)
        })
        .collect()
}

/// Drops pairs that break a rule if `pairs` all unite, until none does.
fn without_broken(mut pairs: BTreeSet<Pair>, ours: &Side<'_>, theirs: &Side<'_>) -> BTreeSet<Pair> {
    loop {
        let united = united_ids(&pairs);
        let doubled = reached_two_ways(&united, ours, theirs);
        let before = pairs.len();
        pairs.retain(|pair| {
            !doubled.contains(&pair.kept()) && !leads_apart(*pair, &united, ours, theirs)
        });
        if pairs.len() == before {
            return pairs;
        }
    }
}

/// Whether one direction from the pair leads to rooms that stay apart.
fn leads_apart(
    pair: Pair,
    united: &HashMap<RoomId, RoomId>,
    ours: &Side<'_>,
    theirs: &Side<'_>,
) -> bool {
    links_of(&ours.out_of, pair.ours).iter().any(|(way, to)| {
        links_of(&theirs.out_of, pair.theirs)
            .iter()
            .any(|(other, their_to)| {
                way == other && one_id(united, *to) != one_id(united, *their_to)
            })
    })
}

/// Rooms at either end of two rooms the union joins by two directions. Both
/// showing the same ways, or one side showing them all, is the map as it is.
fn reached_two_ways(
    united: &HashMap<RoomId, RoomId>,
    ours: &Side<'_>,
    theirs: &Side<'_>,
) -> HashSet<RoomId> {
    let mut ways: HashMap<(RoomId, RoomId), [BTreeSet<&str>; 2]> = HashMap::new();
    for (index, side) in [ours, theirs].into_iter().enumerate() {
        for (from, links) in &side.out_of {
            for (way, to) in links {
                let ends = (one_id(united, *from), one_id(united, *to));
                ways.entry(ends).or_default()[index].insert(way);
            }
        }
    }
    ways.into_iter()
        .filter(|(_, [ours, theirs])| !ours.is_subset(theirs) && !theirs.is_subset(ours))
        .flat_map(|((from, to), _)| [from, to])
        .collect()
}

/// The pairs that can be proven, each by evidence not resting on itself.
fn with_evidence(pairs: &BTreeSet<Pair>, ours: &Side<'_>, theirs: &Side<'_>) -> BTreeSet<Pair> {
    let mut proven = BTreeSet::new();
    loop {
        let united = united_ids(&proven);
        let more: Vec<Pair> = pairs
            .difference(&proven)
            .filter(|pair| has_evidence(**pair, &united, ours, theirs))
            .copied()
            .collect();
        if more.is_empty() {
            return proven;
        }
        proven.extend(more);
    }
}

fn has_evidence(
    pair: Pair,
    united: &HashMap<RoomId, RoomId>,
    ours: &Side<'_>,
    theirs: &Side<'_>,
) -> bool {
    let shares = |our_links: &Links<'_>, their_links: &Links<'_>| {
        links_of(our_links, pair.ours).iter().any(|(way, end)| {
            links_of(their_links, pair.theirs)
                .iter()
                .any(|(other, their_end)| {
                    way == other && one_id(united, *end) == one_id(united, *their_end)
                })
        })
    };
    shares(&ours.out_of, &theirs.out_of) || shares(&ours.into, &theirs.into)
}

fn links_of<'s, 'a>(links: &'s Links<'a>, id: RoomId) -> &'s [(&'a str, RoomId)] {
    links.get(&id).map_or(&[], Vec::as_slice)
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
