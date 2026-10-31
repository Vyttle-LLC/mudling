//! GMCP `Room.Info`: where the player is, as the game reports it, and the
//! other messages that say something about that room.

use super::geometry::{DirMap, Direction};
use super::rules::MapRules;
use crate::ansi::AnsiParser;
use crate::telnet::gmcp;
use serde_json::Value;

/// Char.Status's key for the room's exits.
pub(super) const ROOM_EXITS: &str = "room_exits";
/// Char.Items's location for what lies in the room.
const ROOM_LOCATION: &str = "room";
/// Char.Items's type for a fountain.
const FOUNTAIN: &str = "fountain";

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

fn exits(value: Option<&Value>, rules: &dyn MapRules) -> DirMap<ExitInfo> {
    value
        .and_then(Value::as_object)
        .map(|exits| {
            exits
                .iter()
                .filter_map(|(key, value)| Some((Direction::parse(key)?, exit(value, rules))))
                .collect()
        })
        .unwrap_or_default()
}

/// The room's exits in a Char.Status, sent when a door changes in place.
/// `None` when it doesn't list them.
pub(super) fn status_exits(json: &str, rules: &dyn MapRules) -> Option<DirMap<ExitInfo>> {
    let status: Value = gmcp::from_json(json)?;
    let listed = status.get(ROOM_EXITS)?;
    Some(exits(Some(listed), rules))
}

/// The room lines of the mobs in a Room.Players or Room.AddPlayer. Mobs are
/// keyed by number; players, by name.
pub(super) fn mob_titles(json: &str) -> Vec<String> {
    let Some(Value::Object(occupants)) = gmcp::from_json(json) else {
        return Vec::new();
    };
    occupants
        .iter()
        .filter_map(|(key, occupant)| {
            let name = occupant.get("name").and_then(Value::as_str).unwrap_or(key);
            if name.parse::<i64>().is_err() {
                return None;
            }
            let title = occupant.get("fullname").and_then(Value::as_str);
            Some(plain(title.unwrap_or(name)))
        })
        .collect()
}

fn plain(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    AnsiParser::default().feed(text, &mut plain, &mut Vec::new());
    plain
}

/// A change to the items lying in the room, from a Char.Items message.
pub(super) struct RoomItems {
    /// A whole list, not one more item.
    pub(super) whole: bool,
    pub(super) fountain: bool,
}

impl RoomItems {
    /// `None` when the message is about somewhere else.
    pub(super) fn read(json: &str) -> Option<RoomItems> {
        let message: Value = gmcp::from_json(json)?;
        if message.get("location")?.as_str()? != ROOM_LOCATION {
            return None;
        }
        let is_fountain = |item: &Value| item.get("type").and_then(Value::as_str) == Some(FOUNTAIN);
        Some(match message.get("items").and_then(Value::as_array) {
            Some(items) => RoomItems {
                whole: true,
                fountain: items.iter().any(is_fountain),
            },
            None => RoomItems {
                whole: false,
                fountain: message.get("item").is_some_and(is_fountain),
            },
        })
    }
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
        let exits = exits(object.get("exits"), rules);
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
