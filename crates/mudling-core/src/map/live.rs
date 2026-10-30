//! The live map: what the game says goes in, map changes and the rows to
//! write come out.

use super::buildings::MapDoor;
use super::geometry::{Direction, GridPoint};
use super::info::RoomInfo;
use super::layout::{Layout, Step, UNNUMBERED_IDS};
use super::mobs::{MarkChange, MobMemory};
use super::room::{ExitState, MapRoom, RoomId, feature};
use super::route::{WalkStep, route};
use super::rows::{read_rooms, write_room};
use super::rules::{AreaName, MapRules, RoomIdentity};
use super::text::RoomText;
use super::tracker::{Arrived, Tracker};
use crate::persist::{Batch, Key, MapRows, Meta, MobMark, Timestamp};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

/// Meta keys the map keeps its own state under.
const CURRENT_ROOM: &str = "map.current-room";
const ISLANDS: &str = "map.islands";

/// One server's map, shared by every character on it, kept up to date from
/// GMCP, the game's text and the player's commands.
///
/// Every input updates the model in memory. [`LiveMap::commit`] then hands
/// back what changed since the last commit: a [`MapChange`] for views, and
/// a [`Batch`] of only the changed rows for the persistence writer.
pub struct LiveMap {
    rules: Box<dyn MapRules>,
    tracker: Tracker,
    mobs: MobMemory,
    text: RoomText,
    /// A line said the room about to be shown is too dark to see.
    dark_line: bool,
    /// A line said the player is in a maze.
    maze_line: bool,
    /// The map as last committed.
    committed: Layout,
    committed_whereabouts: Whereabouts,
    revision: u64,
    derived: Option<Derived>,
    area_names: HashMap<String, AreaName>,
    /// How many times derived data was computed, for tests.
    #[cfg(test)]
    pub(super) derivations: usize,
}

/// Where the player is, as far as the map can tell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Whereabouts {
    pub room: Option<RoomId>,
    /// The map hasn't yet worked out which known rooms the last few were.
    pub unsure: bool,
    /// In rooms too dark to see, followed from the player's moves.
    pub dark: bool,
    /// In the dark past the edge of the map: the square the moves lead to.
    pub dark_position: Option<GridPoint>,
}

/// What changed in the map since the last commit, for views and stores.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MapChange {
    /// Bumps once per commit that changed something.
    pub revision: u64,
    /// Rooms added or changed, as they are now.
    pub rooms: Vec<Arc<MapRoom>>,
    pub removed: Vec<RoomId>,
    /// Set when where the player is changed.
    pub whereabouts: Option<Whereabouts>,
    pub marks: Vec<MarkChange>,
}

/// One commit: the change for views and the rows for the writer.
#[derive(Clone, Debug, PartialEq)]
pub struct Commit {
    pub change: MapChange,
    pub batch: Batch,
}

/// What views draw that's computed from the rooms, worked out once per
/// revision.
#[derive(Clone, Debug, PartialEq)]
pub struct Derived {
    pub revision: u64,
    /// Every area, by zone.
    pub areas: Vec<Area>,
    pub buildings: Vec<Building>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Area {
    pub zone: String,
    pub name: AreaName,
    pub rooms: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Building {
    /// Its first room's id.
    pub id: RoomId,
    pub doors: Vec<MapDoor>,
    /// How far to shift its rooms to draw them on the outdoor map; `None`
    /// when it doesn't fit there and is drawn as a house at its doors.
    pub footprint: Option<GridPoint>,
}

impl LiveMap {
    pub fn new(rules: Box<dyn MapRules>) -> LiveMap {
        let layout = Layout::new(first_id(&*rules));
        LiveMap::with_layout(rules, layout, MobMemory::default())
    }

    /// The map as saved: its rows, the mob marks, and the server's meta
    /// values, read at startup through a [`crate::persist::Reader`].
    pub fn load(
        rules: Box<dyn MapRules>,
        map: &MapRows,
        marks: &[MobMark],
        meta: &[Meta],
    ) -> LiveMap {
        let first = first_id(&*rules);
        let mut layout = Layout::new(first);
        for room in read_rooms(map, &*rules) {
            layout.next_id = layout.next_id.max(room.id + 1);
            layout.insert(room);
        }
        let value = |key: &str| {
            meta.iter()
                .find(|entry| entry.key == key)
                .and_then(|entry| entry.value.parse::<i64>().ok())
        };
        layout.current = value(CURRENT_ROOM).filter(|id| layout.contains(*id));
        layout.islands = value(ISLANDS).unwrap_or(0);
        layout.clear_dirty();
        LiveMap::with_layout(rules, layout, MobMemory::from_marks(marks))
    }

    fn with_layout(rules: Box<dyn MapRules>, layout: Layout, mobs: MobMemory) -> LiveMap {
        let tracker = Tracker::new(layout.clone());
        let mut map = LiveMap {
            rules,
            tracker,
            mobs,
            text: RoomText::default(),
            dark_line: false,
            maze_line: false,
            committed: layout,
            committed_whereabouts: Whereabouts::default(),
            revision: 0,
            derived: None,
            area_names: HashMap::new(),
            #[cfg(test)]
            derivations: 0,
        };
        map.committed_whereabouts = map.whereabouts();
        map
    }

    // MARK: What the game says

    /// A GMCP message from the telnet layer. Returns what arriving in a
    /// room revealed, for a Room.Info.
    pub fn gmcp(&mut self, package: &str, json: &str, now: Timestamp) -> Option<Arrived> {
        if package.eq_ignore_ascii_case("Room.Info") {
            self.room_info(json, now)
        } else {
            if package.eq_ignore_ascii_case("Room.WrongDir") {
                self.tracker.move_failed(now);
            }
            None
        }
    }

    /// A Room.Info payload: the player is in this room. `None` when the
    /// payload can't be read.
    pub fn room_info(&mut self, json: &str, now: Timestamp) -> Option<Arrived> {
        let info = RoomInfo::parse(json, &*self.rules)?;
        let dark = std::mem::take(&mut self.dark_line) || self.rules.dark_room(&info);
        let maze = std::mem::take(&mut self.maze_line);
        let description = self.text.take_description(&info.name, &*self.rules);
        let numbered = self.rules.identity() == RoomIdentity::Number;
        let step = Step {
            direction: None,
            number: info.number.filter(|_| numbered),
            indoor: self.rules.indoor(&info.environment),
            exits: info
                .exits
                .iter()
                .map(|(direction, exit)| {
                    (
                        direction,
                        ExitState {
                            state: exit.state.clone(),
                            closed: exit.closed,
                        },
                    )
                })
                .collect(),
            targets: info
                .exits
                .iter()
                .filter(|_| numbered)
                .filter_map(|(direction, exit)| Some((direction, exit.to?)))
                .collect(),
            name: info.name,
            zone: info.zone,
            environment: info.environment,
            description: if dark { String::new() } else { description },
            landing: None,
        };
        let arrived = self.tracker.enter(step, dark, now);
        if maze && !dark {
            self.tracker.update_current_rooms(|room| room.maze = true);
        }
        Some(arrived)
    }

    /// One complete line of the game's text, plain.
    pub fn line(&mut self, line: &str, now: Timestamp) {
        self.text.read(line);
        if self.rules.failed_move(line) {
            self.tracker.move_failed(now);
        }
        if let Some(direction) = self.rules.fled(line) {
            self.tracker.expect_move(direction, now);
        }
        self.dark_line |= self.rules.darkness(line);
        self.maze_line |= self.rules.maze(line);
        if let Some(name) = self.rules.aggressor(line) {
            self.mobs.learn_aggressor(&name);
            self.note_feature(feature::AGGRESSIVE_MOB);
        }
        if let Some(mob) = self.rules.arrival(line) {
            self.note_sightings(&[mob], now);
        }
    }

    /// A command the player sent. Moves and looks tell the map what the
    /// next Room.Info will show; other commands leave it alone.
    pub fn sent(&mut self, command: &str, now: Timestamp) {
        if let Some(direction) = Direction::parse(command) {
            self.tracker.expect_move(direction, now);
        } else if self.rules.look(command) {
            self.tracker.expect_stay(now);
        }
    }

    /// The mobs in the player's room, by their room lines.
    pub fn note_sightings(&mut self, titles: &[String], now: Timestamp) {
        self.tracker.update_current_rooms(|room| {
            for title in titles {
                room.sightings.insert(title.clone(), now);
            }
        });
    }

    /// Something worth remembering about the player's room.
    pub fn note_feature(&mut self, feature: &str) {
        self.tracker.update_current_rooms(|room| {
            if !room.features.contains(feature) {
                room.features.insert(feature.to_owned());
            }
        });
    }

    // MARK: What the player edits. Editing takes the map as shown.

    pub fn set_notes(&mut self, id: RoomId, notes: &str) {
        self.tracker.settle();
        self.tracker
            .layout
            .update(id, |room| notes.clone_into(&mut room.notes));
    }

    pub fn set_feature(&mut self, id: RoomId, feature: &str, present: bool) {
        self.tracker.settle();
        self.tracker.layout.update(id, |room| {
            if present {
                room.features.insert(feature.to_owned());
            } else {
                room.features.remove(feature);
            }
        });
    }

    /// "I'm here": the player telling the map where they really are. The
    /// stray room the map had just invented for them (unconnected, nothing
    /// noted on it) is removed, and its description, if the real room lacks
    /// one, carries over.
    pub fn relocate(&mut self, id: RoomId) {
        self.tracker.settle();
        let layout = &mut self.tracker.layout;
        let Some(target) = layout.room(id) else {
            return;
        };
        let stray = layout
            .current_room()
            .filter(|stray| stray.id != id && layout.is_stray(stray))
            .map(|stray| (stray.id, stray.name.clone(), stray.description.clone()));
        if let Some((stray, name, description)) = stray {
            if target.description.is_empty() && target.name == name {
                layout.update(id, |room| room.description = description);
            }
            layout.remove(stray);
        }
        layout.current = Some(id);
        self.tracker.lose_track();
    }

    /// Removes rooms and every link to them. Forgetting the room the player
    /// is in rebuilds from that same spot on the next room, rather than
    /// starting a far-off island.
    pub fn forget(&mut self, ids: &[RoomId]) {
        self.tracker.settle();
        let layout = &mut self.tracker.layout;
        if let Some(current) = layout.current.filter(|current| ids.contains(current)) {
            layout.restart = layout.room(current).map(|room| room.position);
            layout.current = None;
            self.tracker.lose_track();
        }
        for id in ids {
            self.tracker.layout.remove(*id);
        }
    }

    /// "Not a maze": the player overriding the map's guess.
    pub fn clear_tangles(&mut self, ids: &[RoomId]) {
        self.tracker.settle();
        for id in ids {
            self.tracker.layout.update(*id, |room| room.tangled = false);
        }
    }

    pub fn mobs_mut(&mut self) -> &mut MobMemory {
        &mut self.mobs
    }

    // MARK: Reading the map

    pub fn room(&self, id: RoomId) -> Option<&MapRoom> {
        self.tracker.layout.room(id)
    }

    pub fn rooms(&self) -> impl Iterator<Item = &MapRoom> {
        self.tracker.layout.rooms()
    }

    pub fn room_count(&self) -> usize {
        self.tracker.layout.len()
    }

    pub fn current_room(&self) -> Option<&MapRoom> {
        self.tracker.layout.current_room()
    }

    pub fn whereabouts(&self) -> Whereabouts {
        Whereabouts {
            room: self.tracker.layout.current,
            unsure: self.tracker.is_unsure(),
            dark: self.tracker.is_in_dark(),
            dark_position: self.tracker.dark_position(),
        }
    }

    /// Rooms placed on a guess the map hasn't confirmed.
    pub fn unconfirmed_rooms(&self) -> Vec<RoomId> {
        self.tracker.unconfirmed_rooms()
    }

    pub fn mobs(&self) -> &MobMemory {
        &self.mobs
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The moves from the player's room to `goal`, opening doors known to
    /// be closed; `None` when no explored path leads there.
    pub fn route_to(&self, goal: RoomId) -> Option<Vec<WalkStep>> {
        route(&self.tracker.layout, self.tracker.layout.current?, goal)
    }

    /// Areas and buildings, computed from the rooms once per revision.
    pub fn derived(&mut self) -> &Derived {
        let stale = self
            .derived
            .as_ref()
            .is_none_or(|derived| derived.revision != self.revision);
        if stale {
            let derived = self.derive();
            self.derived = Some(derived);
        }
        self.derived.as_ref().expect("just derived")
    }

    fn derive(&mut self) -> Derived {
        #[cfg(test)]
        {
            self.derivations += 1;
        }
        let layout = &self.committed;
        let rules = &*self.rules;
        let mut areas: Vec<Area> = layout
            .area_counts()
            .map(|(zone, rooms)| Area {
                zone: zone.to_owned(),
                name: self
                    .area_names
                    .entry(zone.to_owned())
                    .or_insert_with(|| rules.area_name(zone))
                    .clone(),
                rooms,
            })
            .collect();
        areas.sort_by(|a, b| a.zone.cmp(&b.zone));
        let mut buildings: Vec<Building> = layout
            .buildings()
            .map(|id| {
                let doors = layout.doors(id);
                Building {
                    id,
                    footprint: layout.footprint_offset(id, &doors),
                    doors,
                }
            })
            .collect();
        buildings.sort_by_key(|building| building.id);
        Derived {
            revision: self.revision,
            areas,
            buildings,
        }
    }

    // MARK: Writing it out

    /// What changed since the last commit, as a change for views and the
    /// rows for the writer; `None` when nothing did. Cheap: it looks only at
    /// the rooms marked changed.
    pub fn commit(&mut self, now: Timestamp) -> Option<Commit> {
        let whereabouts = self.whereabouts();
        let unsure = whereabouts.unsure;
        let layout = &mut self.tracker.layout;
        let mut batch = Batch::new();
        let mut change = MapChange::default();
        let touched: BTreeSet<RoomId> = self
            .committed
            .dirty()
            .iter()
            .chain(layout.dirty().iter())
            .copied()
            .collect();
        for id in touched {
            let old = self.committed.shared_room(id);
            let new = layout.shared_room(id);
            let same = match (old, new) {
                (Some(old), Some(new)) => Arc::ptr_eq(old, new) || old == new,
                (None, None) => true,
                _ => false,
            };
            if same {
                continue;
            }
            write_room(
                &mut batch,
                old.map(|room| &**room),
                new.map(|room| &**room),
                now,
            );
            match new {
                Some(room) => change.rooms.push(Arc::clone(room)),
                None => change.removed.push(id),
            }
        }

        if whereabouts != self.committed_whereabouts {
            change.whereabouts = Some(whereabouts);
        }
        if layout.current != self.committed.current {
            match layout.current {
                Some(id) => meta(&mut batch, CURRENT_ROOM, id, now),
                None => {
                    batch.delete(Key::Meta(CURRENT_ROOM.to_owned()));
                }
            }
        }
        if layout.islands != self.committed.islands {
            meta(&mut batch, ISLANDS, layout.islands, now);
        }
        for mark in self.mobs.take_changes() {
            if mark.set {
                batch.upsert(MobMark {
                    name: mark.name.clone(),
                    mark: mark.mark.name().to_owned(),
                    updated: now,
                });
            } else {
                batch.delete(Key::MobMark {
                    name: mark.name.clone(),
                    mark: mark.mark.name().to_owned(),
                });
            }
            change.marks.push(mark);
        }

        // While unsure, every explanation's changes count from the last
        // sure map, so the marks stay until the map is sure again.
        if !unsure {
            layout.clear_dirty();
        }
        self.committed = layout.clone();
        self.committed_whereabouts = whereabouts;
        let changed = !change.rooms.is_empty()
            || !change.removed.is_empty()
            || change.whereabouts.is_some()
            || !change.marks.is_empty();
        if !changed && batch.is_empty() {
            return None;
        }
        if changed {
            self.revision += 1;
        }
        change.revision = self.revision;
        Some(Commit { change, batch })
    }

    /// A batch saving a new version in map history. The writer copies the
    /// map as the database holds it when it reaches this batch, so submit
    /// every commit's batch before it.
    pub fn save_version(kind: &str, label: &str, now: Timestamp) -> Batch {
        let mut batch = Batch::new();
        batch.save_map_version(kind, label, now);
        batch
    }
}

fn first_id(rules: &dyn MapRules) -> RoomId {
    match rules.identity() {
        RoomIdentity::Number => UNNUMBERED_IDS,
        RoomIdentity::Text => 1,
    }
}

fn meta(batch: &mut Batch, key: &str, value: i64, now: Timestamp) {
    batch.upsert(Meta {
        key: key.to_owned(),
        value: value.to_string(),
        updated: now,
    });
}
