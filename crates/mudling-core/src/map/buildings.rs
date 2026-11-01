//! Buildings: indoor rooms an area can't be drawn without lifting out, as
//! when two moves through a building join neighboring squares outside.
//! Indoor rooms start as ordinary squares; straightening lifts a group into
//! a building of its own only when that draws the area better than long
//! paths (see `straighten`). A building's rooms never take or bend squares
//! outdoors, and it's drawn as a house at its doors. Its floors belong to
//! it. Indoor rooms that lead straight on into another area are that
//! area's entrance and never lifted.
//!
//! Story wings are buildings too: a hub with doors every way opens on
//! stories that don't line up with each other or the area, so each story
//! behind a door gets a map of its own. The way back out of the area stays
//! on the area's map.

use super::geometry::{Direction, GridPoint};
use super::layout::{Layout, Step};
use super::room::{MapRoom, RoomId};
use std::collections::{BTreeSet, HashSet};

/// A way between a building and the outdoors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapDoor {
    /// The outdoor room the door opens from.
    pub outside: RoomId,
    /// The way in, from the outdoor room.
    pub direction: Direction,
    /// The building's room just inside.
    pub inside: RoomId,
}

impl Layout {
    /// The building a room met for the first time belongs to, reached from
    /// `previous` going `direction`: more rooms or floors of the building
    /// the player is in; otherwise none.
    pub(super) fn building_for_new_room(
        &self,
        step: &Step,
        previous: &MapRoom,
        direction: Direction,
    ) -> Option<RoomId> {
        let building = previous.building?;
        if step.zone != previous.zone {
            return None;
        }
        (self.is_wing(building) || step.indoor || direction.is_vertical()).then_some(building)
    }

    /// Whether a room met for the first time is behind one of a hub's
    /// doors, and so starts a story wing of its own.
    pub(super) fn starts_wing(
        &self,
        step: &Step,
        previous: &MapRoom,
        direction: Direction,
    ) -> bool {
        previous.is_hub() && previous.doorways.contains(direction) && step.zone == previous.zone
    }

    /// Makes each story behind a hub's doors a wing of its own, named for
    /// its first room, unless it leads out of the area: that's the way in.
    pub(super) fn assign_wings(&mut self) {
        let hubs: Vec<RoomId> = self.hubs().map(|hub| hub.id).collect();
        for hub_id in hubs {
            let Some(hub) = self.room(hub_id).cloned() else {
                continue;
            };
            for direction in hub.doorways.iter() {
                let Some(first) = hub.links.get_copied(direction) else {
                    continue;
                };
                if !self
                    .room(first)
                    .is_some_and(|room| room.zone == hub.zone && room.building.is_none())
                {
                    continue;
                }
                // Paths walked one way only (out of a maze) still join a story.
                let mut story = HashSet::from([first]);
                let mut queue = vec![first];
                let mut leads_out = false;
                while let Some(id) = queue.pop() {
                    let neighbors: Vec<RoomId> = self.neighbors(id).collect();
                    for next in neighbors {
                        if next == hub.id || story.contains(&next) {
                            continue;
                        }
                        let Some(room) = self.room(next) else {
                            continue;
                        };
                        if room.zone != hub.zone {
                            leads_out = true;
                        } else {
                            story.insert(next);
                            queue.push(next);
                        }
                    }
                }
                if leads_out {
                    continue;
                }
                for id in story {
                    self.update(id, |room| room.building = Some(first));
                }
            }
        }
    }

    /// Puts every building back on its area's grid, at its first door, and
    /// redraws the areas, lifting out again only what can't be drawn
    /// otherwise, with mazes as one square and stories as wings. Run on
    /// load: what's learned since a building was lifted can undo the reason.
    pub(super) fn rebuild_buildings(&mut self) {
        let buildings: BTreeSet<RoomId> = self.buildings().collect();
        for building in buildings {
            let members: BTreeSet<RoomId> =
                self.building_members(building).iter().copied().collect();
            let shift = self.door_shift(&members).unwrap_or_default();
            for id in members {
                self.update(id, |room| {
                    room.building = None;
                    room.position = room.position + shift;
                });
            }
        }
        let ids: Vec<RoomId> = {
            let ids: BTreeSet<RoomId> = self.rooms().map(|room| room.id).collect();
            ids.into_iter().collect()
        };
        for id in &ids {
            if self.contains(*id) {
                self.collapse_maze(*id);
            }
        }
        self.assign_wings();
        let mut straightened = HashSet::new();
        let mut may_be_misdrawn = self.rooms_leading_to_misdrawing();
        for id in ids {
            if straightened.contains(&id) || !may_be_misdrawn.contains(&id) {
                continue;
            }
            if let Some(area) = self.misdrawn_area(id) {
                let moved = self.straighten(&area);
                self.note_moved_rooms(&moved, &mut may_be_misdrawn);
                straightened.extend(self.flat_area_around(id));
            }
        }
    }

    /// How far to move a building's rooms to put the room inside its first
    /// door one square from the outdoor room it opens from.
    fn door_shift(&self, members: &BTreeSet<RoomId>) -> Option<GridPoint> {
        for inside in members.iter().filter_map(|id| self.room(*id)) {
            for direction in Direction::FLAT {
                let Some(outside) = inside
                    .links
                    .get_copied(direction)
                    .and_then(|to| self.room(to))
                else {
                    continue;
                };
                if outside.building.is_none() && outside.zone == inside.zone {
                    return Some(
                        outside.position + direction.opposite().offset() - inside.position,
                    );
                }
            }
        }
        None
    }

    /// Whether indoor rooms are a way into another area: a room with a door
    /// from this area's outdoors that also leads straight on elsewhere.
    pub(super) fn is_area_entrance(&self, members: &BTreeSet<RoomId>) -> bool {
        members.iter().any(|id| {
            let Some(room) = self.room(*id) else {
                return false;
            };
            let neighbors: Vec<&MapRoom> = Direction::FLAT
                .into_iter()
                .filter_map(|direction| room.links.get_copied(direction))
                .filter_map(|to| self.room(to))
                .collect();
            let has_door = neighbors
                .iter()
                .any(|next| next.zone == room.zone && !next.indoor && next.building.is_none());
            has_door && neighbors.iter().any(|next| next.zone != room.zone)
        })
    }

    /// The doors between a building and the outdoors.
    pub(super) fn doors(&self, building: RoomId) -> Vec<MapDoor> {
        let wing = self.is_wing(building);
        let mut doors: Vec<MapDoor> = self
            .building_members(building)
            .iter()
            .filter_map(|id| self.room(*id))
            .flat_map(|inside| {
                self.inbound(inside.id)
                    .iter()
                    .filter_map(move |(from, direction)| {
                        let outside = self.room(*from)?;
                        // A building's floors are its own; a story up or
                        // down from a hub has a door.
                        let door = outside.building.is_none()
                            && outside.zone == inside.zone
                            && (!direction.is_vertical() || wing);
                        door.then_some(MapDoor {
                            outside: outside.id,
                            direction: *direction,
                            inside: inside.id,
                        })
                    })
            })
            .collect();
        doors.sort_by_key(|door| (door.outside, door.direction));
        doors
    }

    /// How far to shift a building's rooms to draw them on the outdoor map,
    /// when it fits there: every door one square from its outdoor room, and
    /// no square already taken outdoors. `None` for a building that's
    /// bigger inside than out, drawn as a house at its doors instead.
    pub(super) fn footprint_offset(
        &self,
        building: RoomId,
        doors: &[MapDoor],
    ) -> Option<GridPoint> {
        // Stories overlap each other wherever they'd fit.
        if self.is_wing(building) {
            return None;
        }
        let offsets: HashSet<GridPoint> = doors
            .iter()
            .filter_map(|door| {
                let outside = self.room(door.outside)?;
                let inside = self.room(door.inside)?;
                Some(outside.position + door.direction.offset() - inside.position)
            })
            .collect();
        let mut offsets = offsets.into_iter();
        let offset = offsets.next()?;
        if offsets.next().is_some() {
            return None;
        }
        let fits = self
            .building_members(building)
            .iter()
            .filter_map(|id| self.room(*id))
            .all(|room| {
                self.room_at(room.position + offset, &room.zone, None)
                    .is_none()
            });
        fits.then_some(offset)
    }
}
