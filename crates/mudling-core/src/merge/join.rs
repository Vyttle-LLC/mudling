//! Joining two rooms by hand, and undoing it, as rows.
//!
//! A join adds its record and runs the merge itself over the device's own rows,
//! so a retired room's rows reach the kept room by the path a union takes, and
//! a device hearing of the join later works out the same rows. Only what the
//! merge changed is written: the kept room, with the links into it, and a
//! tombstone for the retired room.
//!
//! The version saved before a join holds both rooms as they were, and undoing
//! puts back what the join changed and nothing else: a row edited since is the
//! player's, so it stays. The rooms come back as an edit of their own, which
//! other devices take as they take any later edit, and the record goes with a
//! tombstone. Saved versions stay on the device, so a join can be undone only
//! where it was made, and only while its version is kept.
//!
//! A restore doesn't undo a join: a version from before one holds both rooms,
//! and what it holds for the retired room lands on the kept room. A version
//! with the retired room but not the kept one has no room to join to, so the
//! record goes and the retired room is a room again.
//!
//! A join's record is never older than a delete this device knows of, however
//! its clock stands, so what it moves outranks every delete from before it.
//! `rooms.rs` lets a delete made since the join rule a moved row out.

use super::follow_current_room;
use super::records::merge_server;
use super::rooms::RoomMoves;
use crate::map::{
    LiveMap, RoomId, RoomPair, VERSION_JOIN, join_label, joined_key, joins_in, undo_key, undo_value,
};
use crate::persist::{Batch, Key, Keyed, MapRows, Meta, Reader, Record, Room, Timestamp};
use crate::repo::format::ServerRows;
use std::collections::{BTreeMap, HashSet};

/// The rows `ids` own, with the links that lead to them.
fn about(map: &MapRows, ids: &[RoomId]) -> MapRows {
    let on = |id: &RoomId| ids.contains(id);
    fn rows<T: Clone>(rows: &[T], keep: impl Fn(&T) -> bool) -> Vec<T> {
        rows.iter().filter(|row| keep(row)).cloned().collect()
    }
    MapRows {
        rooms: rows(&map.rooms, |row| on(&row.id)),
        exits: rows(&map.exits, |row| on(&row.room_id)),
        links: rows(&map.links, |row| on(&row.room_id) || on(&row.to_room)),
        doorways: rows(&map.doorways, |row| on(&row.room_id)),
        notes: rows(&map.notes, |row| on(&row.room_id)),
        features: rows(&map.features, |row| on(&row.room_id)),
        sightings: rows(&map.sightings, |row| on(&row.room_id)),
    }
}

/// The merge run over `rows` alone, which carries out every join they hold,
/// as a batch: what's left of the rooms in `kept`, and a tombstone for each room
/// in `retired`.
fn carried(rows: ServerRows, retired: &[RoomId], kept: &[RoomId]) -> Batch {
    let merged = merge_server(rows.clone(), rows);
    let meta =
        (merged.meta.iter()).filter(|row| retired.iter().any(|id| row.key == joined_key(*id)));
    ServerRows {
        meta: meta.cloned().collect(),
        map: about(&merged.map, kept),
        tombstones: (merged.tombstones.iter())
            .filter(|tombstone| matches!(tombstone.key, Key::Room(id) if retired.contains(&id)))
            .cloned()
            .collect(),
        ..ServerRows::default()
    }
    .into_batch()
}

/// What joining `pair` writes, and what it's called, or why it can't be done.
pub(crate) fn join_rooms(
    rows: &ServerRows,
    (kept, retired): RoomPair,
    reader: &Reader,
    now: Timestamp,
) -> Result<(Batch, String), String> {
    let name = |id: RoomId| {
        let room = rows.map.rooms.iter().find(|room| room.id == id);
        room.map(|room| room.name.clone())
    };
    let (Some(kept_name), Some(retired_name)) = (name(kept), name(retired)) else {
        return Err("one of those rooms is gone".to_owned());
    };
    let label = join_label(&kept_name, &retired_name);
    let mut batch = LiveMap::save_version(VERSION_JOIN, &label, now);
    batch.upsert(Meta {
        key: undo_key(now),
        value: undo_value((kept, retired)),
        updated: now,
    });
    let mut local = rows.clone();
    let at = past_deletes(rows, now);
    local.meta.retain(|row| row.key != joined_key(retired));
    local.meta.push(join_row((kept, retired), at));
    batch.extend(carried(local, &[retired], &[kept]));
    let moves = RoomMoves::from([(retired, Some(kept))]);
    follow_current_room(reader, &moves, &mut batch).map_err(|error| error.to_string())?;
    Ok((batch, label))
}

/// `now`, or the newest delete in `rows` if that's later.
fn past_deletes(rows: &ServerRows, now: Timestamp) -> Timestamp {
    let deletes = rows.tombstones.iter().map(|tombstone| tombstone.deleted);
    deletes.fold(now, Timestamp::max)
}

fn join_row((kept, retired): RoomPair, updated: Timestamp) -> Meta {
    Meta {
        key: joined_key(retired),
        value: kept.to_string(),
        updated,
    }
}

/// What undoing the join `pair` writes, from `before`, the version saved
/// before it, or why it can't be done.
pub(crate) fn undo_join(
    rows: &ServerRows,
    before: &MapRows,
    (kept, retired): RoomPair,
    (version_id, created): (i64, Timestamp),
    now: Timestamp,
) -> Result<Batch, String> {
    let standing = (rows.meta.iter())
        .find(|row| row.key == joined_key(retired) && row.value == kept.to_string());
    let holds = |id: RoomId| rows.map.rooms.iter().any(|room| room.id == id);
    let Some(record) = standing.filter(|_| !holds(retired)) else {
        return Err("that join has been undone already".to_owned());
    };
    if !holds(kept) {
        return Err("that room has since been joined to another: undo that first".to_owned());
    }
    let ids = [kept, retired];
    // The join as it ran, with the deletes it saw: not one made since.
    let view = ServerRows {
        map: before.clone(),
        meta: vec![record.clone()],
        tombstones: (rows.tombstones.iter())
            .filter(|tombstone| tombstone.deleted <= record.updated)
            .cloned()
            .collect(),
        ..ServerRows::default()
    };
    let joined = about(&merge_server(view.clone(), view).map, &ids);
    let (before, current) = (about(before, &ids), about(&rows.map, &ids));

    let retired_key = Key::Room(retired);
    let buried = rows
        .tombstones
        .iter()
        .find(|tombstone| tombstone.key == retired_key)
        .map_or_else(Timestamp::default, |tombstone| tombstone.deleted);
    let at = now.max(Timestamp(buried.0 + 1));

    let mut batch = Batch::new();
    batch.delete(Key::Meta(joined_key(retired)));
    batch.delete(Key::Meta(undo_key(created)));
    batch.delete(Key::MapVersion(version_id));
    let batch_ = &mut batch;
    put_back(
        batch_,
        &before.rooms,
        &joined.rooms,
        &current.rooms,
        |row| Room { updated: at, ..row },
    );
    put_back(
        batch_,
        &before.exits,
        &joined.exits,
        &current.exits,
        |mut row| {
            row.updated = at;
            row
        },
    );
    put_back(
        batch_,
        &before.links,
        &joined.links,
        &current.links,
        |mut row| {
            row.updated = at;
            row
        },
    );
    put_back(
        batch_,
        &before.doorways,
        &joined.doorways,
        &current.doorways,
        |mut row| {
            row.updated = at;
            row
        },
    );
    put_back(
        batch_,
        &before.notes,
        &joined.notes,
        &current.notes,
        |mut row| {
            row.updated = at;
            row
        },
    );
    put_back(
        batch_,
        &before.features,
        &joined.features,
        &current.features,
        |mut row| {
            row.updated = at;
            row
        },
    );
    // A sighting's time says when the mob was seen, so it moves only as far
    // as it must to outlive the room's tombstone.
    put_back(
        batch_,
        &before.sightings,
        &joined.sightings,
        &current.sightings,
        |mut row| {
            row.updated = row.updated.max(buried);
            row
        },
    );
    Ok(batch)
}

/// Takes one record type back from what the join made it to what it was,
/// where it's still as the join left it.
fn put_back<T>(
    batch: &mut Batch,
    before: &[T],
    joined: &[T],
    current: &[T],
    stamped: impl Fn(T) -> T,
) where
    T: Keyed + Clone + PartialEq + Into<Record>,
{
    let by_key = |rows: &[T]| -> BTreeMap<Key, T> {
        rows.iter().map(|row| (row.key(), row.clone())).collect()
    };
    let (mut before, joined, current) = (by_key(before), by_key(joined), by_key(current));
    for (key, row) in &joined {
        let was = before.remove(key);
        if current.get(key) != Some(row) || was.as_ref() == Some(row) {
            continue;
        }
        match was {
            Some(was) => {
                batch.upsert(stamped(was));
            }
            None => {
                batch.delete(key.clone());
            }
        }
    }
    // What the retired room alone held, which the join took off it.
    for (key, was) in before {
        if !current.contains_key(&key) {
            batch.upsert(stamped(was));
        }
    }
}

/// The records restoring `version` cancels: joins of a room it holds to a room
/// it doesn't.
pub(crate) fn joins_cancelled_by(version: &MapRows, meta: &[Meta]) -> Vec<Key> {
    let held: HashSet<RoomId> = version.rooms.iter().map(|room| room.id).collect();
    (joins_in(meta).into_iter())
        .filter(|(retired, kept)| held.contains(retired) && !held.contains(kept))
        .map(|(retired, _)| Key::Meta(joined_key(retired)))
        .collect()
}

/// What moves the rooms a restore just brought back onto the rooms that hold
/// them now. Nothing, if it brought none back. Their records take the
/// restore's time, since the deletes it just wrote are from before they move.
pub(crate) fn rejoin_restored(mut rows: ServerRows, now: Timestamp) -> Batch {
    let at = past_deletes(&rows, now);
    let held: HashSet<RoomId> = rows.map.rooms.iter().map(|room| room.id).collect();
    let joins = joins_in(&rows.meta);
    let back: Vec<(RoomId, RoomId)> = (joins.into_iter())
        .filter(|(retired, kept)| held.contains(retired) && held.contains(kept))
        .collect();
    if back.is_empty() {
        return Batch::new();
    }
    let retired: Vec<RoomId> = back.iter().map(|(retired, _)| *retired).collect();
    let kept: Vec<RoomId> = back.iter().map(|(_, kept)| *kept).collect();
    for row in &mut rows.meta {
        if retired.iter().any(|id| row.key == joined_key(*id)) {
            row.updated = at;
        }
    }
    carried(rows, &retired, &kept)
}
