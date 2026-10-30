//! MUD areas are mostly grids, but some paths skip or stretch. Placing
//! rooms one move at a time can't know that, so when an area's paths stop
//! lining up, it's redrawn as a whole with the fewest fixes: a path drawn
//! long, or a group of indoor rooms lifted out into a building. Every other
//! path is one square and no two rooms share a square. Each map is redrawn
//! on its own: an area's outdoors, or one building.

use super::geometry::{Direction, GridPoint};
use super::layout::{Layout, is_maze_path};
use super::room::{MapRoom, RoomId};
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};

mod drawing;

use drawing::{Level, best_drawing};

/// Areas bigger than this are left as placed.
const LARGEST_AREA: usize = 250;
/// Paths that could be the long ones; more means the area is too tangled
/// to work out quickly.
const MOST_SUSPECTS: usize = 30;
/// At most this many fixes are tried at once.
const MOST_FIXES: usize = 3;
/// Bigger indoor groups aren't lifted: they're a part of the area.
const LARGEST_BUILDING: usize = 8;

/// One explored path between two rooms of an area, stored going north or
/// east so each pair appears once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Passage {
    from: RoomId,
    to: RoomId,
    direction: Direction,
}

/// One way to make an area line up.
#[derive(Clone, Debug)]
enum Fix {
    /// Draw this path longer than a square.
    Long(Passage),
    /// Lift these indoor rooms out into a building of their own.
    Lift(BTreeSet<RoomId>),
}

/// The rooms of one level of one map, joined by flat paths.
pub(super) struct FlatArea {
    pub rooms: Vec<RoomId>,
    /// Changes whenever a room, link or position in the area does, so an
    /// area that couldn't be straightened isn't tried again unchanged.
    pub fingerprint: u64,
}

impl Layout {
    /// The area around the room when it has a path that doesn't line up or
    /// two rooms on one square; `None` when it's drawn fine.
    pub(super) fn misdrawn_area(&self, id: RoomId) -> Option<FlatArea> {
        let start = self.room(id).filter(|room| !room.tangled)?;
        let area = self.flat_area(start);
        let mut squares = HashSet::with_capacity(area.rooms.len());
        let crowded = area
            .rooms
            .iter()
            .filter_map(|id| self.room(*id))
            .any(|room| !squares.insert(room.position));
        let misdrawn = crowded || area.rooms.iter().any(|id| self.has_misdrawn_path(*id));
        misdrawn.then_some(area)
    }

    /// Redraws the area so its paths line up, keeping its oldest room where
    /// it is. Leaves it alone when no layout with a few fixes works (a real
    /// maze). A long path costs the squares it stretches; a building costs
    /// one square, and loses ties, so indoor rooms stay on the grid unless
    /// lifting them really helps.
    pub(super) fn straighten(&mut self, area: &FlatArea) {
        let Some(start) = area.rooms.first().and_then(|id| self.room(*id)).cloned() else {
            return;
        };
        if area.rooms.len() > LARGEST_AREA {
            return;
        }
        let Some(&anchor) = area.rooms.iter().min() else {
            return;
        };
        let members: HashSet<RoomId> = area.rooms.iter().copied().collect();
        let passages = self.passages(&members);
        let suspects = self.suspect_passages(&area.rooms, &passages, anchor);
        if suspects.len() > MOST_SUSPECTS {
            return;
        }
        let mut fixes: Vec<Fix> = suspects.iter().copied().map(Fix::Long).collect();
        fixes.extend(
            self.liftable_groups(&area.rooms, &suspects)
                .into_iter()
                .map(Fix::Lift),
        );
        let level = Level {
            zone: &start.zone,
            building: start.building,
            members: &members,
        };
        let Some(best) = best_drawing(self, &area.rooms, &passages, &fixes, &level) else {
            return;
        };
        let mut placed = best.positions;
        placed.sort_unstable();
        for (id, position) in &placed {
            if self
                .room(*id)
                .is_some_and(|room| room.position != *position)
            {
                self.update(*id, |room| room.position = *position);
            }
        }
        let outdoors: HashSet<RoomId> = placed.iter().map(|(id, _)| *id).collect();
        for group in best.lifted {
            self.lift_into_building(&group, &outdoors);
        }
    }

    /// Rooms of the same area and level as `start`, joined by flat paths.
    fn flat_area(&self, start: &MapRoom) -> FlatArea {
        let mut seen = HashSet::from([start.id]);
        let mut rooms = vec![start.id];
        let mut fingerprint = DefaultHasher::new();
        let mut head = 0;
        while head < rooms.len() && rooms.len() <= LARGEST_AREA {
            let id = rooms[head];
            head += 1;
            let Some(room) = self.room(id) else { continue };
            (id, room.position, &room.links, room.tangled, room.maze).hash(&mut fingerprint);
            for direction in Direction::FLAT {
                let Some(next) = room.links.get_copied(direction) else {
                    continue;
                };
                if seen.contains(&next) {
                    continue;
                }
                let Some(other) = self.room(next) else {
                    continue;
                };
                if other.zone == start.zone
                    && other.building == start.building
                    && other.position.z == start.position.z
                    && !other.tangled
                {
                    seen.insert(next);
                    rooms.push(next);
                }
            }
        }
        // Rooms past the limit weren't visited; they still count.
        rooms[head..].hash(&mut fingerprint);
        FlatArea {
            rooms,
            fingerprint: fingerprint.finish(),
        }
    }

    fn passages(&self, members: &HashSet<RoomId>) -> Vec<Passage> {
        let mut found = HashSet::new();
        for id in members {
            let Some(room) = self.room(*id) else { continue };
            for direction in Direction::FLAT {
                let Some(next) = room.links.get_copied(direction) else {
                    continue;
                };
                let Some(other) = self.room(next) else {
                    continue;
                };
                if !members.contains(&next) || is_maze_path(room, other) {
                    continue;
                }
                found.insert(if matches!(direction, Direction::North | Direction::East) {
                    Passage {
                        from: *id,
                        to: next,
                        direction,
                    }
                } else {
                    Passage {
                        from: next,
                        to: *id,
                        direction: direction.opposite(),
                    }
                });
            }
        }
        // The prototype's order, which settles ties between equal drawings.
        let mut found: Vec<Passage> = found.into_iter().collect();
        found.sort_by_key(|p| (p.from, p.to, p.direction.name()));
        found
    }

    /// The paths that could be the long ones: those on a loop that doesn't
    /// close on the grid, or that walks two rooms into the same square.
    fn suspect_passages(
        &self,
        area: &[RoomId],
        passages: &[Passage],
        anchor: RoomId,
    ) -> Vec<Passage> {
        // Lay the area out along a breadth-first tree and see where it fails.
        let mut touching: HashMap<RoomId, Vec<Passage>> = HashMap::new();
        for passage in passages {
            touching.entry(passage.from).or_default().push(*passage);
            touching.entry(passage.to).or_default().push(*passage);
        }
        let mut parent: HashMap<RoomId, Passage> = HashMap::new();
        let mut spot: HashMap<RoomId, GridPoint> = HashMap::from([(anchor, GridPoint::default())]);
        let mut depth: HashMap<RoomId, usize> = HashMap::from([(anchor, 0)]);
        let mut queue = VecDeque::from([anchor]);
        while let Some(id) = queue.pop_front() {
            for passage in touching.get(&id).into_iter().flatten() {
                let forward = passage.from == id;
                let next = if forward { passage.to } else { passage.from };
                if spot.contains_key(&next) {
                    continue;
                }
                let offset = if forward {
                    passage.direction.offset()
                } else {
                    passage.direction.opposite().offset()
                };
                spot.insert(next, spot[&id] + offset);
                parent.insert(next, *passage);
                depth.insert(next, depth[&id] + 1);
                queue.push_back(next);
            }
        }

        let tree_path = |mut a: RoomId, mut b: RoomId| {
            let mut path = Vec::new();
            while a != b {
                let depth_of = |id: RoomId| depth.get(&id).copied().unwrap_or(0);
                if depth_of(a) >= depth_of(b)
                    && let Some(step) = parent.get(&a)
                {
                    path.push(*step);
                    a = if step.from == a { step.to } else { step.from };
                } else if let Some(step) = parent.get(&b) {
                    path.push(*step);
                    b = if step.from == b { step.to } else { step.from };
                } else {
                    break;
                }
            }
            path
        };

        let mut suspects: HashSet<Passage> = HashSet::new();
        let tree: HashSet<Passage> = parent.values().copied().collect();
        for passage in passages.iter().filter(|passage| !tree.contains(passage)) {
            let (Some(from), Some(to)) = (spot.get(&passage.from), spot.get(&passage.to)) else {
                continue;
            };
            if *to == *from + passage.direction.offset() {
                continue;
            }
            suspects.insert(*passage);
            suspects.extend(tree_path(passage.from, passage.to));
        }
        let mut by_spot: HashMap<Option<GridPoint>, Vec<RoomId>> = HashMap::new();
        for id in area {
            by_spot.entry(spot.get(id).copied()).or_default().push(*id);
        }
        for rooms in by_spot.values().filter(|rooms| rooms.len() > 1) {
            for other in &rooms[1..] {
                suspects.extend(tree_path(rooms[0], *other));
            }
        }
        passages
            .iter()
            .copied()
            .filter(|passage| suspects.contains(passage))
            .collect()
    }

    /// Indoor rooms of the area, in groups joined by flat paths, that could
    /// be a building: small, with a door to the area's outdoors, not a way
    /// into another area, and touching a path that doesn't line up.
    fn liftable_groups(&self, area: &[RoomId], suspects: &[Passage]) -> Vec<BTreeSet<RoomId>> {
        let in_area: HashSet<RoomId> = area.iter().copied().collect();
        let indoor: BTreeSet<RoomId> = area
            .iter()
            .copied()
            .filter(|id| self.room(*id).is_some_and(|room| room.indoor))
            .collect();
        let involved: HashSet<RoomId> = suspects.iter().flat_map(|p| [p.from, p.to]).collect();
        let mut groups = Vec::new();
        let mut seen = HashSet::new();
        for &start in &indoor {
            if seen.contains(&start) {
                continue;
            }
            let mut group = BTreeSet::from([start]);
            let mut queue = vec![start];
            while let Some(id) = queue.pop() {
                let Some(room) = self.room(id) else { continue };
                for direction in Direction::FLAT {
                    if let Some(next) = room.links.get_copied(direction)
                        && indoor.contains(&next)
                        && group.insert(next)
                    {
                        queue.push(next);
                    }
                }
            }
            seen.extend(group.iter().copied());
            let has_door = group.iter().any(|id| {
                self.room(*id).is_some_and(|room| {
                    Direction::FLAT.into_iter().any(|direction| {
                        room.links
                            .get_copied(direction)
                            .is_some_and(|to| in_area.contains(&to) && !indoor.contains(&to))
                    })
                })
            });
            let near_suspect = group.iter().any(|id| involved.contains(id));
            if has_door
                && group.len() <= LARGEST_BUILDING
                && near_suspect
                && !self.is_area_entrance(&group)
            {
                groups.push(group);
            }
        }
        groups
    }

    /// Makes a group of rooms a building, with its floors: the same area's
    /// rooms reached from it going up or down, and the indoor rooms beside
    /// those.
    fn lift_into_building(&mut self, group: &BTreeSet<RoomId>, outdoors: &HashSet<RoomId>) {
        let mut members = group.clone();
        let mut queue: Vec<RoomId> = group.iter().copied().collect();
        while let Some(id) = queue.pop() {
            let Some(room) = self.room(id) else { continue };
            for (direction, next) in room.links.iter() {
                if members.contains(next) || outdoors.contains(next) {
                    continue;
                }
                let Some(other) = self.room(*next) else {
                    continue;
                };
                if other.zone == room.zone
                    && other.building.is_none()
                    && (direction.is_vertical() || other.indoor)
                {
                    members.insert(*next);
                    queue.push(*next);
                }
            }
        }
        let Some(&building) = members.first() else {
            return;
        };
        for id in members {
            self.update(id, |room| room.building = Some(building));
        }
    }
}
