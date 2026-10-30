//! Everything placement decides: rooms, links, and where the player is.
//!
//! A layout is a value, so each explanation of an unsure walk keeps its own.
//! Its maps are persistent (copying one shares everything), and indexes
//! answer every question a move asks (rooms that look alike, the room on a
//! square, the rooms linking to one), so no move scans the world.

use super::geometry::{DirMap, DirSet, Direction, GridPoint};
use super::room::{ExitState, MapRoom, RoomId};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

/// Ids the map gives rooms that have no game number, when the game numbers
/// its rooms: far above any game's numbers, so the two never meet.
pub(super) const UNNUMBERED_IDS: RoomId = 1 << 40;

type Map<K, V> = imbl::HashMap<K, V>;

#[derive(Clone, Debug, Default)]
pub(super) struct Layout {
    rooms: Map<RoomId, Arc<MapRoom>>,
    index: Index,
    pub current: Option<RoomId>,
    /// The id the next room without a game number gets.
    pub next_id: RoomId,
    /// Rooms reached by an untraceable jump start a separate island far away.
    pub islands: i64,
    /// Where to rebuild after the player forgets the room they're in.
    pub restart: Option<GridPoint>,
    /// Rooms changed since the map was last written out.
    dirty: imbl::HashSet<RoomId>,
}

/// One arrival in a room, as the game showed it.
#[derive(Clone, Debug)]
pub(super) struct Step {
    /// The move that led here; `None` when no move explains the arrival.
    pub direction: Option<Direction>,
    pub number: Option<RoomId>,
    pub name: String,
    pub zone: String,
    pub environment: String,
    pub indoor: bool,
    pub exits: DirMap<ExitState>,
    /// Where exits lead, when the game numbers its rooms.
    pub targets: DirMap<RoomId>,
    pub description: String,
    /// Out of the dark or a maze: where the moves made in it lead.
    pub landing: Option<Landing>,
}

impl Step {
    fn exit_directions(&self) -> DirSet {
        self.exits.directions()
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Landing {
    pub position: GridPoint,
    /// The last move, to go on along if the square is taken.
    pub heading: Direction,
    /// The building whose map the square is on.
    pub building: Option<RoomId>,
}

/// What placing one arrival did.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Arrival {
    pub new_room: bool,
    pub exits_changed: bool,
}

impl Layout {
    pub fn new(first_id: RoomId) -> Layout {
        Layout {
            next_id: first_id,
            ..Layout::default()
        }
    }

    pub fn room(&self, id: RoomId) -> Option<&MapRoom> {
        self.rooms.get(&id).map(|room| &**room)
    }

    pub fn shared_room(&self, id: RoomId) -> Option<&Arc<MapRoom>> {
        self.rooms.get(&id)
    }

    pub fn contains(&self, id: RoomId) -> bool {
        self.rooms.contains_key(&id)
    }

    pub fn rooms(&self) -> impl Iterator<Item = &MapRoom> {
        self.rooms.values().map(|room| &**room)
    }

    pub fn len(&self) -> usize {
        self.rooms.len()
    }

    pub fn current_room(&self) -> Option<&MapRoom> {
        self.current.and_then(|id| self.room(id))
    }

    pub fn dirty(&self) -> &imbl::HashSet<RoomId> {
        &self.dirty
    }

    pub fn clear_dirty(&mut self) {
        self.dirty = imbl::HashSet::new();
    }

    // MARK: Changing rooms. Every change goes through these, so the
    // indexes and the dirty set never miss one.

    pub fn insert(&mut self, room: MapRoom) {
        let id = room.id;
        let before = self.rooms.get(&id).map(|room| Keys::of(room));
        self.index.reindex(id, before, Some(&room));
        self.rooms.insert(id, Arc::new(room));
        self.dirty.insert(id);
    }

    /// Changes a room in place; `None` if there's no such room.
    pub fn update<R>(&mut self, id: RoomId, change: impl FnOnce(&mut MapRoom) -> R) -> Option<R> {
        let shared = self.rooms.get_mut(&id)?;
        let before = Keys::of(shared);
        let room = Arc::make_mut(shared);
        let result = change(room);
        self.index.reindex(id, Some(before), Some(room));
        self.dirty.insert(id);
        Some(result)
    }

    /// Removes a room and every link to it, so no link ever dangles.
    pub fn remove(&mut self, id: RoomId) -> Option<Arc<MapRoom>> {
        if !self.contains(id) {
            return None;
        }
        for (from, direction) in self.inbound(id).to_vec() {
            self.update(from, |room| room.links.remove(direction));
        }
        let room = self.rooms.remove(&id)?;
        self.index.reindex(id, Some(Keys::of(&room)), None);
        self.dirty.insert(id);
        Some(room)
    }

    pub fn update_current(&mut self, change: impl FnOnce(&mut MapRoom)) {
        if let Some(id) = self.current {
            self.update(id, change);
        }
    }

    // MARK: Questions the indexes answer.

    /// Rooms with this name in this area.
    pub fn look_alikes<'a>(
        &'a self,
        name: &'a str,
        zone: &'a str,
    ) -> impl Iterator<Item = &'a MapRoom> + 'a {
        self.index
            .looks
            .get(&look_key(name, zone))
            .into_iter()
            .flatten()
            .filter_map(|id| self.room(*id))
            .filter(move |room| room.name == name && room.zone == zone)
    }

    /// Rooms on a square of one map: an area's outdoors, or a building.
    pub fn rooms_at<'a>(
        &'a self,
        position: GridPoint,
        zone: &'a str,
        building: Option<RoomId>,
    ) -> impl Iterator<Item = &'a MapRoom> + 'a {
        self.index
            .squares
            .get(&square_key(zone, building, position))
            .into_iter()
            .flatten()
            .filter_map(|id| self.room(*id))
            .filter(move |room| {
                room.position == position && room.zone == zone && room.building == building
            })
    }

    /// The room on a square, the oldest when several share it.
    pub fn room_at(
        &self,
        position: GridPoint,
        zone: &str,
        building: Option<RoomId>,
    ) -> Option<RoomId> {
        self.rooms_at(position, zone, building)
            .map(|room| room.id)
            .min()
    }

    /// The rooms with a link to `id`, and which way each goes.
    pub fn inbound(&self, id: RoomId) -> &[(RoomId, Direction)] {
        self.index.inbound.get(&id).map_or(&[], Vec::as_slice)
    }

    pub fn hubs(&self) -> impl Iterator<Item = &MapRoom> {
        self.index.hubs.iter().filter_map(|id| self.room(*id))
    }

    pub fn building_members(&self, building: RoomId) -> &[RoomId] {
        self.index
            .buildings
            .get(&building)
            .map_or(&[], Vec::as_slice)
    }

    pub fn buildings(&self) -> impl Iterator<Item = RoomId> + '_ {
        self.index.buildings.keys().copied()
    }

    /// Rooms per area.
    pub fn area_counts(&self) -> impl Iterator<Item = (&str, usize)> {
        self.index
            .areas
            .values()
            .map(|(zone, count)| (zone.as_str(), *count))
    }

    /// Rooms joined to `id` by a link either way.
    pub fn neighbors(&self, id: RoomId) -> impl Iterator<Item = RoomId> + '_ {
        let outbound = self
            .room(id)
            .into_iter()
            .flat_map(|room| room.links.values().copied());
        outbound.chain(self.inbound(id).iter().map(|(from, _)| *from))
    }

    /// Whether a building is a story behind a hub's door, by its first room.
    pub fn is_wing(&self, building: RoomId) -> bool {
        self.hubs().any(|hub| {
            hub.building != Some(building) && hub.links.values().any(|to| *to == building)
        })
    }

    pub fn is_stray(&self, room: &MapRoom) -> bool {
        room.links.is_empty()
            && room.notes.is_empty()
            && room.features.is_empty()
            && self.inbound(room.id).is_empty()
    }

    // MARK: Recognizing rooms

    /// Could be the same room: the same game number, or the same name and
    /// area with descriptions that agree when both are known.
    pub fn matches_identity(&self, stored: &MapRoom, step: &Step) -> bool {
        if let Some(number) = step.number {
            return stored.id == number;
        }
        stored.name == step.name
            && stored.zone == step.zone
            && (stored.description.is_empty()
                || step.description.is_empty()
                || stored.description == step.description)
    }

    /// The same identity and set of exits.
    pub fn matches_exactly(&self, stored: &MapRoom, step: &Step) -> bool {
        self.matches_identity(stored, step) && stored.exit_directions() == step.exit_directions()
    }

    /// Known rooms the step's room could be; those with the very same
    /// description first-class, when there are any.
    pub fn candidates(&self, step: &Step) -> Vec<RoomId> {
        if let Some(number) = step.number {
            return self
                .contains(number)
                .then_some(number)
                .into_iter()
                .collect();
        }
        let possible: Vec<&MapRoom> = self
            .look_alikes(&step.name, &step.zone)
            .filter(|room| self.matches_exactly(room, step))
            .collect();
        let described: Vec<RoomId> = possible
            .iter()
            .filter(|room| !step.description.is_empty() && room.description == step.description)
            .map(|room| room.id)
            .collect();
        let mut ids = if described.is_empty() {
            possible.iter().map(|room| room.id).collect()
        } else {
            described
        };
        ids.sort_unstable();
        ids
    }

    /// The one known room with this exact name, area, description and set
    /// of exits: certain, wherever the grid says it should be. Exits count
    /// because games reuse descriptions across rooms of one name.
    pub fn described_match(&self, step: &Step) -> Option<RoomId> {
        if let Some(number) = step.number {
            return self.contains(number).then_some(number);
        }
        if step.description.is_empty() {
            return None;
        }
        let mut matches = self.look_alikes(&step.name, &step.zone).filter(|room| {
            room.description == step.description && room.exit_directions() == step.exit_directions()
        });
        let only = matches.next()?;
        matches.next().is_none().then_some(only.id)
    }

    // MARK: Placing rooms

    /// Moves the player into the step's room: `identity` if it's a room
    /// already mapped, otherwise a new one.
    pub fn arrive(&mut self, step: &Step, identity: Option<RoomId>) -> Arrival {
        let mut arrival = Arrival::default();
        let id = match identity {
            Some(id) => {
                if let (Some(previous), Some(direction)) = (self.current, step.direction) {
                    self.link(previous, direction, id);
                }
                arrival.exits_changed = self
                    .room(id)
                    .is_some_and(|room| room.exit_directions() != step.exit_directions());
                id
            }
            None => {
                arrival.new_room = true;
                self.place_new(step)
            }
        };
        self.update(id, |room| refresh(room, step));
        for (direction, to) in step.targets.iter() {
            if self.contains(*to) {
                self.update(id, |room| room.links.insert(direction, *to));
            }
        }
        self.current = Some(id);
        arrival
    }

    /// A room the map hasn't seen, one grid step beyond the last. MUD
    /// geometry isn't a grid: a square already taken means the path is
    /// stretched, so the room goes further along.
    fn place_new(&mut self, step: &Step) -> RoomId {
        let id = self.new_id(step);
        let from = self.current.and_then(|id| self.room(id).cloned());
        let (Some(previous), Some(direction)) = (from, step.direction) else {
            let (position, building) = self.unlinked_spot(step);
            self.place(step, id, position, building);
            return id;
        };
        let building = if self.starts_wing(step, &previous, direction) {
            Some(id)
        } else {
            self.building_for_new_room(step, &previous, direction)
        };
        let mut target = previous.position + direction.offset();
        while self.room_at(target, &step.zone, building).is_some() {
            target = target + direction.offset();
        }
        self.place(step, id, target, building);
        self.link(previous.id, direction, id);
        id
    }

    /// Where a room nothing leads to goes: where the moves in the dark led,
    /// where the player forgot their room, or a new island.
    fn unlinked_spot(&mut self, step: &Step) -> (GridPoint, Option<RoomId>) {
        if let Some(landing) = step.landing {
            return (self.free_square(landing, &step.zone), landing.building);
        }
        if let Some(spot) = self.restart
            && self.room_at(spot, &step.zone, None).is_none()
        {
            self.restart = None;
            return (spot, None);
        }
        self.islands += 1;
        (GridPoint::new((self.islands - 1) * 1000, 0, 0), None)
    }

    fn new_id(&mut self, step: &Step) -> RoomId {
        step.number.unwrap_or_else(|| {
            let id = self.next_id;
            self.next_id += 1;
            id
        })
    }

    fn place(&mut self, step: &Step, id: RoomId, position: GridPoint, building: Option<RoomId>) {
        self.insert(MapRoom {
            name: step.name.clone(),
            zone: step.zone.clone(),
            environment: step.environment.clone(),
            indoor: step.indoor,
            position,
            building,
            exits: step.exits.clone(),
            ..MapRoom::empty(id)
        });
    }

    /// Links both ways when the far room has the matching return exit. The
    /// two things only a maze does mark the rooms tangled: an exit that led
    /// somewhere else before, and a way back that leads somewhere else.
    pub fn link(&mut self, from: RoomId, direction: Direction, to: RoomId) {
        if let Some(earlier) = self
            .room(from)
            .and_then(|room| room.links.get_copied(direction))
            && earlier != to
        {
            self.mark_tangled(&[from, earlier, to]);
        }
        self.update(from, |room| room.links.insert(direction, to));
        let Some(far) = self.room(to) else { return };
        // A maze's way back is anyone's guess.
        if far.maze || !far.exits.contains(direction.opposite()) {
            return;
        }
        match far.links.get_copied(direction.opposite()) {
            None => {
                self.update(to, |room| room.links.insert(direction.opposite(), from));
            }
            Some(back) if back == from => {}
            Some(_) => self.mark_tangled(&[from, to]),
        }
    }

    pub fn mark_tangled(&mut self, ids: &[RoomId]) {
        for id in ids {
            self.update(*id, |room| room.tangled = true);
        }
    }

    /// Where the dark walk ended, or further along its heading if a room
    /// is already there.
    fn free_square(&self, landing: Landing, zone: &str) -> GridPoint {
        let mut square = landing.position;
        while self.room_at(square, zone, landing.building).is_some() {
            square = square + landing.heading.offset();
        }
        square
    }

    /// Three or more rooms that look exactly alike, each a crossroads,
    /// leading into one another along paths that don't add up: one maze
    /// square, since they can't be told apart. A corridor of look-alikes
    /// lines up, so it stays a row; copies of one crossroads mapped twice
    /// never lead into each other. The rooms around a maze were only
    /// tangled by it. Returns whether one formed.
    pub fn collapse_maze(&mut self, around: RoomId) -> bool {
        let Some(here) = self.room(around) else {
            return false;
        };
        if here.maze || here.exits.len() < 3 {
            return false;
        }
        let mut twins: Vec<RoomId> = self
            .look_alikes(&here.name, &here.zone)
            .filter(|room| {
                room.building == here.building
                    && room.description == here.description
                    && room.exit_directions() == here.exit_directions()
            })
            .map(|room| room.id)
            .collect();
        twins.sort_unstable();
        let tangled = twins.iter().any(|id| {
            self.room(*id).is_some_and(|room| room.tangled) || self.has_misdrawn_path(*id)
        });
        let joined = twins.iter().any(|id| {
            self.room(*id).is_some_and(|room| {
                room.links
                    .values()
                    .any(|to| to != id && twins.binary_search(to).is_ok())
            })
        });
        if twins.len() < 3 || !tangled || !joined {
            return false;
        }
        let maze = twins[0];
        let merged = &twins[1..];
        for other in merged {
            for (from, direction) in self.inbound(*other).to_vec() {
                self.update(from, |room| room.links.insert(direction, maze));
            }
            let Some(room) = self.remove(*other) else {
                continue;
            };
            self.update(maze, |maze| {
                for (direction, to) in room.links.iter() {
                    if !maze.links.contains(direction) {
                        maze.links.insert(direction, *to);
                    }
                }
            });
        }
        // Moves within the maze lead nowhere the map can draw.
        let ways = self.update(maze, |room| {
            room.links.retain(|_, to| *to != maze);
            room.maze = true;
            room.links.clone()
        });
        for neighbor in ways.iter().flat_map(|ways| ways.values()) {
            self.update(*neighbor, |room| room.tangled = false);
        }
        for (from, _) in self.inbound(maze).to_vec() {
            self.update(from, |room| room.tangled = false);
        }
        if self
            .current
            .is_some_and(|current| merged.contains(&current))
        {
            self.current = Some(maze);
        }
        true
    }

    /// Whether a flat path from the room doesn't line up on the grid.
    pub fn has_misdrawn_path(&self, id: RoomId) -> bool {
        let Some(room) = self.room(id) else {
            return false;
        };
        if room.tangled {
            return false;
        }
        Direction::FLAT.into_iter().any(|direction| {
            let Some(next) = room
                .links
                .get_copied(direction)
                .and_then(|to| self.room(to))
            else {
                return false;
            };
            next.building == room.building
                && next.zone == room.zone
                && next.position.z == room.position.z
                && !is_maze_path(room, next)
                && next.position != room.position + direction.offset()
        })
    }
}

/// A path into or out of a maze square says nothing about the grid.
pub(super) fn is_maze_path(a: &MapRoom, b: &MapRoom) -> bool {
    a.maze || b.maze
}

/// What a revisit learns: the exits now, the environment, and the
/// description if the room had none.
pub(super) fn refresh(room: &mut MapRoom, step: &Step) {
    room.note_exits(step.exits.clone());
    if room.environment != step.environment {
        room.environment = step.environment.clone();
        room.indoor = step.indoor;
    }
    if room.description.is_empty() && !step.description.is_empty() {
        room.description = step.description.clone();
    }
}

/// The parts of a room the indexes key on.
struct Keys {
    look: u64,
    square: u64,
    zone: u64,
    links: DirMap<RoomId>,
    hub: bool,
    building: Option<RoomId>,
}

impl Keys {
    fn of(room: &MapRoom) -> Keys {
        Keys {
            look: look_key(&room.name, &room.zone),
            square: square_key(&room.zone, room.building, room.position),
            zone: hash(&room.zone),
            links: room.links.clone(),
            hub: room.is_hub(),
            building: room.building,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct Index {
    /// Rooms by name and area.
    looks: Map<u64, Vec<RoomId>>,
    /// Rooms by square, area and building.
    squares: Map<u64, Vec<RoomId>>,
    /// For each room, the rooms that link to it and which way.
    inbound: Map<RoomId, Vec<(RoomId, Direction)>>,
    hubs: imbl::OrdSet<RoomId>,
    /// Each building's rooms, by its first room's id.
    buildings: Map<RoomId, Vec<RoomId>>,
    /// Rooms per area, by the area's hash.
    areas: Map<u64, (String, usize)>,
}

impl Index {
    fn reindex(&mut self, id: RoomId, before: Option<Keys>, after: Option<&MapRoom>) {
        let after_keys = after.map(Keys::of);
        let (old, new) = (before.as_ref(), after_keys.as_ref());
        let changed = |key: fn(&Keys) -> u64| old.map(key) != new.map(key);

        if changed(|keys| keys.look) {
            old.inspect(|keys| remove_from(&mut self.looks, keys.look, id));
            new.inspect(|keys| add_to(&mut self.looks, keys.look, id));
        }
        if changed(|keys| keys.square) {
            old.inspect(|keys| remove_from(&mut self.squares, keys.square, id));
            new.inspect(|keys| add_to(&mut self.squares, keys.square, id));
        }
        if changed(|keys| keys.zone) {
            if let Some(keys) = old
                && let Some((_, count)) = self.areas.get_mut(&keys.zone)
            {
                *count -= 1;
                if *count == 0 {
                    self.areas.remove(&keys.zone);
                }
            }
            if let (Some(keys), Some(room)) = (new, after) {
                self.areas
                    .entry(keys.zone)
                    .or_insert_with(|| (room.zone.clone(), 0))
                    .1 += 1;
            }
        }
        for direction in Direction::ALL {
            let from = old.and_then(|keys| keys.links.get_copied(direction));
            let to = new.and_then(|keys| keys.links.get_copied(direction));
            if from == to {
                continue;
            }
            if let Some(target) = from {
                remove_from(&mut self.inbound, target, (id, direction));
            }
            if let Some(target) = to {
                add_to(&mut self.inbound, target, (id, direction));
            }
        }
        let hub = new.is_some_and(|keys| keys.hub);
        if old.is_some_and(|keys| keys.hub) != hub {
            if hub {
                self.hubs.insert(id);
            } else {
                self.hubs.remove(&id);
            }
        }
        let building = |keys: Option<&Keys>| keys.and_then(|keys| keys.building);
        if building(old) != building(new) {
            if let Some(building) = building(old) {
                remove_from(&mut self.buildings, building, id);
            }
            if let Some(building) = building(new) {
                add_to(&mut self.buildings, building, id);
            }
        }
    }
}

fn add_to<K: Hash + Eq + Clone, T: Clone>(map: &mut Map<K, Vec<T>>, key: K, value: T) {
    map.entry(key).or_default().push(value);
}

fn remove_from<K: Hash + Eq + Clone, T: Clone + PartialEq>(
    map: &mut Map<K, Vec<T>>,
    key: K,
    value: T,
) {
    if let Some(values) = map.get_mut(&key) {
        values.retain(|other| *other != value);
        if values.is_empty() {
            map.remove(&key);
        }
    }
}

fn hash(value: &impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn look_key(name: &str, zone: &str) -> u64 {
    hash(&(name, zone))
}

fn square_key(zone: &str, building: Option<RoomId>, position: GridPoint) -> u64 {
    hash(&(zone, building, position))
}
