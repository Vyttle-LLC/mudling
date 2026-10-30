//! Between the map and its tables: a changed room becomes only the rows that
//! changed, and saved rows become rooms again.

use super::geometry::{Direction, GridPoint};
use super::room::{ExitState, MapRoom};
use super::rules::MapRules;
use crate::persist::{
    Batch, Doorway, Exit, Key, Link, MapRows, Room, RoomFeature, RoomNote, Sighting, Timestamp,
};
use std::collections::BTreeMap;

/// Adds to `batch` what turns the stored `old` room into `new`.
pub(super) fn write_room(
    batch: &mut Batch,
    old: Option<&MapRoom>,
    new: Option<&MapRoom>,
    now: Timestamp,
) {
    let Some(new) = new else {
        if let Some(old) = old {
            // Deleting the room deletes its rows, and the links into it.
            batch.delete(Key::Room(old.id));
        }
        return;
    };
    let id = new.id;
    if old.is_none_or(|old| room_row_differs(old, new)) {
        batch.upsert(Room {
            id,
            name: new.name.clone(),
            area: new.zone.clone(),
            environment: new.environment.clone(),
            description: new.description.clone(),
            x: new.position.x,
            y: new.position.y,
            z: new.position.z,
            building: new.building,
            tangled: new.tangled,
            maze: new.maze,
            updated: now,
        });
    }
    let empty = MapRoom::empty(id);
    let old = old.unwrap_or(&empty);

    for direction in Direction::ALL {
        let direction_name = || direction.name().to_owned();
        match (old.exits.get(direction), new.exits.get(direction)) {
            (before, Some(exit)) if before != Some(exit) => {
                batch.upsert(Exit {
                    room_id: id,
                    direction: direction_name(),
                    state: exit.state.clone(),
                    updated: now,
                });
            }
            (Some(_), None) => {
                batch.delete(Key::Exit {
                    room_id: id,
                    direction: direction_name(),
                });
            }
            _ => {}
        }
        match (
            old.links.get_copied(direction),
            new.links.get_copied(direction),
        ) {
            (before, Some(to_room)) if before != Some(to_room) => {
                batch.upsert(Link {
                    room_id: id,
                    direction: direction_name(),
                    to_room,
                    updated: now,
                });
            }
            (Some(_), None) => {
                batch.delete(Key::Link {
                    room_id: id,
                    direction: direction_name(),
                });
            }
            _ => {}
        }
        match (
            old.doorways.contains(direction),
            new.doorways.contains(direction),
        ) {
            (false, true) => {
                batch.upsert(Doorway {
                    room_id: id,
                    direction: direction_name(),
                    updated: now,
                });
            }
            (true, false) => {
                batch.delete(Key::Doorway {
                    room_id: id,
                    direction: direction_name(),
                });
            }
            _ => {}
        }
    }

    if old.notes != new.notes {
        if new.notes.is_empty() {
            batch.delete(Key::RoomNote(id));
        } else {
            batch.upsert(RoomNote {
                room_id: id,
                text: new.notes.clone(),
                updated: now,
            });
        }
    }
    for feature in new.features.difference(&old.features) {
        batch.upsert(RoomFeature {
            room_id: id,
            feature: feature.clone(),
            updated: now,
        });
    }
    for feature in old.features.difference(&new.features) {
        batch.delete(Key::RoomFeature {
            room_id: id,
            feature: feature.clone(),
        });
    }
    for (mob, seen) in &new.sightings {
        if old.sightings.get(mob) != Some(seen) {
            batch.upsert(Sighting {
                room_id: id,
                mob: mob.clone(),
                updated: *seen,
            });
        }
    }
    for mob in old
        .sightings
        .keys()
        .filter(|mob| !new.sightings.contains_key(*mob))
    {
        batch.delete(Key::Sighting {
            room_id: id,
            mob: mob.clone(),
        });
    }
}

fn room_row_differs(old: &MapRoom, new: &MapRoom) -> bool {
    old.name != new.name
        || old.zone != new.zone
        || old.environment != new.environment
        || old.description != new.description
        || old.position != new.position
        || old.building != new.building
        || old.tangled != new.tangled
        || old.maze != new.maze
}

/// The rooms saved in `rows`. Rows naming a direction the map doesn't know
/// are left out.
pub(super) fn read_rooms(rows: &MapRows, rules: &dyn MapRules) -> Vec<MapRoom> {
    let mut rooms: BTreeMap<i64, MapRoom> = rows
        .rooms
        .iter()
        .map(|row| {
            let mut room = MapRoom::empty(row.id);
            room.name = row.name.clone();
            room.zone = row.area.clone();
            room.indoor = rules.indoor(&row.environment);
            room.environment = row.environment.clone();
            room.description = row.description.clone();
            room.position = GridPoint::new(row.x, row.y, row.z);
            room.building = row.building;
            room.tangled = row.tangled;
            room.maze = row.maze;
            (row.id, room)
        })
        .collect();
    for exit in &rows.exits {
        if let (Some(room), Some(direction)) = (
            rooms.get_mut(&exit.room_id),
            Direction::parse(&exit.direction),
        ) {
            room.exits.insert(
                direction,
                ExitState {
                    closed: rules.closed_door(&exit.state),
                    state: exit.state.clone(),
                },
            );
        }
    }
    for link in &rows.links {
        if let (Some(room), Some(direction)) = (
            rooms.get_mut(&link.room_id),
            Direction::parse(&link.direction),
        ) {
            room.links.insert(direction, link.to_room);
        }
    }
    for doorway in &rows.doorways {
        if let (Some(room), Some(direction)) = (
            rooms.get_mut(&doorway.room_id),
            Direction::parse(&doorway.direction),
        ) {
            room.doorways.insert(direction);
        }
    }
    for note in &rows.notes {
        if let Some(room) = rooms.get_mut(&note.room_id) {
            room.notes = note.text.clone();
        }
    }
    for feature in &rows.features {
        if let Some(room) = rooms.get_mut(&feature.room_id) {
            room.features.insert(feature.feature.clone());
        }
    }
    for sighting in &rows.sightings {
        if let Some(room) = rooms.get_mut(&sighting.room_id) {
            room.sightings
                .insert(sighting.mob.clone(), sighting.updated);
        }
    }
    rooms.into_values().collect()
}
