//! GMCP `Room.Info`: where the player is, as the game reports it.

use super::geometry::{DirMap, Direction};
use super::rules::MapRules;
use crate::telnet::gmcp;
use serde_json::Value;

/// A Room.Info, read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RoomInfo {
    /// The game's room number, when it sends one.
    pub number: Option<i64>,
    pub name: String,
    /// The game's area, as it sends it.
    pub zone: String,
    pub environment: String,
    pub exits: DirMap<ExitInfo>,
    /// Tags the game adds to the room.
    pub details: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ExitInfo {
    /// The exit's state as the game sends it (a door open or shut), or
    /// empty when it sends the room the exit leads to instead.
    pub state: String,
    pub closed: bool,
    /// The room number the exit leads to, when the game sends it.
    pub to: Option<i64>,
}

impl RoomInfo {
    /// Reads a Room.Info payload. `None` when it isn't a JSON object.
    pub fn parse(json: &str, rules: &dyn MapRules) -> Option<RoomInfo> {
        let value: Value = gmcp::from_json(json)?;
        let object = value.as_object()?;
        let text = |keys: &[&str]| {
            keys.iter()
                .find_map(|key| object.get(*key).and_then(Value::as_str))
                .unwrap_or_default()
                .to_owned()
        };
        let exits = object
            .get("exits")
            .and_then(Value::as_object)
            .map(|exits| {
                exits
                    .iter()
                    .filter_map(|(key, value)| Some((Direction::parse(key)?, exit(value, rules))))
                    .collect()
            })
            .unwrap_or_default();
        let details = object
            .get("details")
            .and_then(Value::as_array)
            .map(|details| {
                details
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        Some(RoomInfo {
            number: ["num", "vnum", "id"]
                .iter()
                .find_map(|key| object.get(*key).and_then(number)),
            name: text(&["name"]),
            zone: text(&["zone", "area"]),
            environment: text(&["environment", "terrain"]),
            exits,
            details,
        })
    }
}

fn exit(value: &Value, rules: &dyn MapRules) -> ExitInfo {
    match number(value) {
        Some(to) => ExitInfo {
            to: Some(to),
            ..ExitInfo::default()
        },
        None => {
            let state = value.as_str().unwrap_or_default().to_owned();
            ExitInfo {
                closed: rules.closed_door(&state),
                state,
                to: None,
            }
        }
    }
}

/// A number, or a string holding one: games send both.
fn number(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
}
