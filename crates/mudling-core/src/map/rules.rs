//! What a game decides about its map: the inputs a plugin's Map and Mobs
//! extension points supply. The core holds no game's wording; every method
//! has a neutral default, so a game with plain GMCP maps without a plugin.

use super::geometry::Direction;
use super::info::RoomInfo;

/// How rooms are told apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoomIdentity {
    /// By the GMCP room number when Room.Info has one, otherwise by name,
    /// exits and description.
    Number,
    /// By name, exits and description only: the game's numbers aren't
    /// stable or unique.
    Text,
}

/// An area's readable name, from the zone string the game sends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AreaName {
    pub name: String,
    /// The level range the area is for, when the game says.
    pub levels: Option<String>,
}

/// Game-specific readings of GMCP and text for the map. Lines arrive
/// complete and plain (no ANSI codes, no prompt).
pub trait MapRules: Send {
    fn identity(&self) -> RoomIdentity {
        RoomIdentity::Number
    }

    fn area_name(&self, zone: &str) -> AreaName {
        AreaName {
            name: zone.trim().to_owned(),
            levels: None,
        }
    }

    /// The game refused a move ("you can't go that way").
    fn failed_move(&self, _line: &str) -> bool {
        false
    }

    /// The player fled, and the game said which way.
    fn fled(&self, _line: &str) -> Option<Direction> {
        None
    }

    /// A command that shows the room again without moving.
    fn look(&self, command: &str) -> bool {
        let command = command.trim();
        command.eq_ignore_ascii_case("l") || command.eq_ignore_ascii_case("look")
    }

    /// The room is too dark to see, judged from its Room.Info.
    fn dark_room(&self, _info: &RoomInfo) -> bool {
        false
    }

    /// A line saying the room about to be shown is too dark to see.
    fn darkness(&self, _line: &str) -> bool {
        false
    }

    /// A line saying the player is in a maze, whose rooms are one square.
    fn maze(&self, _line: &str) -> bool {
        false
    }

    /// A mob attacking the player unprovoked, by name: its kind is aggressive.
    fn aggressor(&self, _line: &str) -> Option<String> {
        None
    }

    /// A mob arriving in the player's room, by its line.
    fn arrival(&self, _line: &str) -> Option<String> {
        None
    }

    /// The line listing the room's exits, which the room's description
    /// follows. With none, rooms are told apart by name and exits only.
    fn exits_line(&self, _line: &str) -> bool {
        false
    }

    /// Whether an exit's state, as Room.Info gives it, is a closed door.
    fn closed_door(&self, state: &str) -> bool {
        state.eq_ignore_ascii_case("c") || state.eq_ignore_ascii_case("closed")
    }

    /// Whether a room's environment is indoors, so it may belong to a
    /// building.
    fn indoor(&self, environment: &str) -> bool {
        let environment = environment.to_lowercase();
        ["inside", "indoor", "cave", "underground"]
            .iter()
            .any(|word| environment.contains(word))
    }
}

/// The neutral rules: GMCP alone.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlainRules;

impl MapRules for PlainRules {}
