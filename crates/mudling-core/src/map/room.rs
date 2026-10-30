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
    /// Important gear is here.
    pub const GEAR: &str = "gear";
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
    /// Explored connections, which outlive the room's listing.
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

    /// Doors in four or more directions: each a way into a story of its own.
    pub fn is_hub(&self) -> bool {
        self.doorways.len() >= 4
    }

    pub fn exit_directions(&self) -> DirSet {
        self.exits.directions()
    }

    /// Takes the exits the room lists now. Links stay: a room that lists
    /// fewer exits later keeps the paths explored before.
    pub(super) fn note_exits(&mut self, exits: DirMap<ExitState>) {
        for (direction, exit) in exits.iter() {
            if exit.closed {
                self.doorways.insert(direction);
            }
        }
        self.exits = exits;
    }
}
