//! A mapped room.

use super::geometry::{DirMap, DirSet, GridPoint};
use crate::persist::Timestamp;
use std::collections::{BTreeMap, BTreeSet};

/// A room's id: the game's room number, or one the map gave it.
pub type RoomId = i64;

/// Features the map notes by itself; the player and plugins add others.
pub mod feature {
    /// An aggressive mob has been seen here.
    pub const AGGRESSIVE_MOB: &str = "aggressive-mob";
    /// A fountain lies here.
    pub const FOUNTAIN: &str = "fountain";
    /// Important gear is here.
    pub const GEAR: &str = "gear";
    pub const SHOP: &str = "shop";
    pub const TRAINER: &str = "trainer";
    pub const DANGER: &str = "danger";
    /// A mob marked for its gear has been seen here.
    pub const GEAR_MOB: &str = "gear-mob";
    /// A mob marked as someone who matters has been seen here.
    pub const IMPORTANT_MOB: &str = "important-mob";

    /// What a player sets on a room. The rest follow the mob marks.
    pub const PLAYER_SET: [&str; 6] = [AGGRESSIVE_MOB, FOUNTAIN, SHOP, TRAINER, DANGER, GEAR];
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitState {
    /// As the game sent it.
    pub state: String,
    pub closed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MapRoom {
    pub id: RoomId,
    pub name: String,
    /// The game's area (GMCP's zone), as it sends it.
    pub zone: String,
    pub environment: String,
    /// Read from `environment` by the game's rules.
    pub indoor: bool,
    /// The first line of the room's description, which tells apart rooms
    /// that share a name; empty when it wasn't read.
    pub description: String,
    pub position: GridPoint,
    /// The building this room is inside, by its first room's id; `None`
    /// outdoors. A building has a map of its own.
    pub building: Option<RoomId>,
    /// The exits the room lists now.
    pub exits: DirMap<ExitState>,
    /// Explored connections. A sure visit drops those the room no longer
    /// lists, except through a doorway.
    pub links: DirMap<RoomId>,
    /// Exits that have shown a door at some point, open or shut.
    pub doorways: DirSet,
    /// The map's geometry contradicts itself here (overlaps, one-way or
    /// shifting paths), so the position is a guess.
    pub tangled: bool,
    /// One square standing for a whole maze of look-alike rooms.
    pub maze: bool,
    /// The player's own notes.
    pub notes: String,
    pub features: BTreeSet<String>,
    /// Mobs seen here, by their room line, with when each was last seen.
    pub sightings: BTreeMap<String, Timestamp>,
}

impl MapRoom {
    /// A room with nothing known yet but its id.
    pub(super) fn empty(id: RoomId) -> MapRoom {
        MapRoom {
            id,
            name: String::new(),
            zone: String::new(),
            environment: String::new(),
            indoor: false,
            description: String::new(),
            position: GridPoint::default(),
            building: None,
            exits: DirMap::default(),
            links: DirMap::default(),
            doorways: DirSet::default(),
            tangled: false,
            maze: false,
            notes: String::new(),
            features: BTreeSet::new(),
            sightings: BTreeMap::new(),
        }
    }

    /// Whether the two differ only in what's been noted about the room, so
    /// neither its area nor its place in the map's shape changed.
    pub(super) fn same_shape(&self, other: &MapRoom) -> bool {
        self.zone == other.zone
            && self.indoor == other.indoor
            && self.position == other.position
            && self.building == other.building
            && self.links == other.links
            && self.doorways == other.doorways
    }

    /// Whether two rooms read as the same place, as a room's id alone can't
    /// say across versions of a map where ids are handed out in turn.
    pub(super) fn same_place_as(&self, other: &MapRoom) -> bool {
        self.name == other.name && self.zone == other.zone
    }

    /// Doors in four or more directions: each a way into a story of its own.
    pub fn is_hub(&self) -> bool {
        self.doorways.len() >= 4
    }

    pub fn exit_directions(&self) -> DirSet {
        self.exits.directions()
    }

    /// Takes the exits the room lists now. Links stay until the map is sure
    /// the player is here (see `Layout::drop_unlisted_links`).
    pub(super) fn note_exits(&mut self, exits: DirMap<ExitState>) {
        for (direction, exit) in exits.iter() {
            if exit.closed {
                self.doorways.insert(direction);
            }
        }
        self.exits = exits;
    }
}
