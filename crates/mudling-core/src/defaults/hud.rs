//! HUD fields: which GMCP field holds each vital, each of the character
//! sheet's values, and the opponent's.
//!
//! The defaults read the keys of the first `Char.Vitals` message and match
//! them by common names (`hp`, `HP`, `health`; `maxhp`, `max_hp`; …). A
//! plugin's HUD point maps fields itself, and its mapping wins, the highest
//! layer last. The sheet, the opponent and affects come only from HUD points
//! (see [`GameFields`]): GMCP has few common names for them.

use crate::plugin::PluginHost;
use crate::plugin::points::{Hud, OpponentField};
use crate::store::Affect;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

pub const VITALS: &str = "Char.Vitals";

/// A vital the HUD shows.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Vital {
    Hp,
    MaxHp,
    Mana,
    MaxMana,
    Moves,
    MaxMoves,
    /// Experience still to earn for the next level.
    ToNextLevel,
    /// The whole of this level's experience.
    MaxToNextLevel,
    /// A vital only a plugin names, such as a game's own resource.
    Other(String),
}

impl Vital {
    /// The vital a field or HUD name stands for, by common names; case,
    /// `_` and `-` are ignored.
    pub fn from_name(name: &str) -> Vital {
        let key: String = name
            .chars()
            .filter(|c| !matches!(c, '_' | '-'))
            .collect::<String>()
            .to_lowercase();
        match key.as_str() {
            "hp" | "health" | "hits" | "hitpoints" | "curhp" => Vital::Hp,
            "maxhp" | "hpmax" | "maxhealth" | "healthmax" | "maxhits" | "maxhitpoints" => {
                Vital::MaxHp
            }
            "mana" | "mp" | "sp" | "curmana" => Vital::Mana,
            "maxmana" | "manamax" | "maxmp" | "mpmax" | "maxsp" => Vital::MaxMana,
            "moves" | "move" | "mv" | "movement" | "curmoves" => Vital::Moves,
            "maxmoves" | "maxmove" | "movesmax" | "maxmv" | "mvmax" | "maxmovement" => {
                Vital::MaxMoves
            }
            "tnl" | "tonextlevel" | "exptnl" | "experiencetnl" => Vital::ToNextLevel,
            "maxtnl" | "tnlmax" | "maxexptnl" | "experiencetnlmax" => Vital::MaxToNextLevel,
            _ => Vital::Other(name.to_owned()),
        }
    }

    /// The vital's common name, which [`Vital::from_name`] reads back.
    pub fn name(&self) -> &str {
        match self {
            Vital::Hp => "hp",
            Vital::MaxHp => "maxhp",
            Vital::Mana => "mana",
            Vital::MaxMana => "maxmana",
            Vital::Moves => "moves",
            Vital::MaxMoves => "maxmoves",
            Vital::ToNextLevel => "tnl",
            Vital::MaxToNextLevel => "maxtnl",
            Vital::Other(name) => name,
        }
    }
}

/// Where a vital's value is: a key of a GMCP message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub package: String,
    pub key: String,
}

impl Field {
    /// Reads `Char.Vitals.hp`: the package, then the key after the last `.`.
    pub fn parse(path: &str) -> Option<Field> {
        let (package, key) = path.rsplit_once('.')?;
        (!package.is_empty() && !key.is_empty()).then(|| Field {
            package: package.to_owned(),
            key: key.to_owned(),
        })
    }
}

/// Each vital and the field it's read from.
pub type FieldMap = BTreeMap<Vital, Field>;

/// The fields learned from the first `Char.Vitals`.
#[derive(Debug, Default)]
pub struct HudFields {
    learned: Option<FieldMap>,
}

impl HudFields {
    /// Reads a `Char.Vitals` message; only the first one's keys are learned.
    pub fn vitals_arrived(&mut self, message: &Map<String, Value>) {
        if self.learned.is_some() {
            return;
        }
        let learned = message
            .keys()
            .filter_map(|key| match Vital::from_name(key) {
                Vital::Other(_) => None,
                vital => Some((
                    vital,
                    Field {
                        package: VITALS.to_owned(),
                        key: key.clone(),
                    },
                )),
            })
            .collect();
        self.learned = Some(learned);
    }

    /// Whether a `Char.Vitals` has come, so the HUD has numbers to show.
    pub fn has_learned(&self) -> bool {
        self.learned.is_some()
    }

    /// The learned fields, with every enabled plugin's HUD vitals over them
    /// in run order, so the highest layer wins.
    pub fn fields(&self, host: &PluginHost) -> FieldMap {
        let mut fields = self.learned.clone().unwrap_or_default();
        for (_, hud) in host.declared(|points| points.hud.as_ref()) {
            for (name, path) in &hud.vitals {
                if let Some(field) = Field::parse(path) {
                    fields.insert(Vital::from_name(name), field);
                }
            }
        }
        fields
    }
}

/// Where GMCP holds the character sheet's values and the opponent bar's: no
/// game's names are the core's, so every one comes from a HUD point, the
/// defaults' included, the highest layer's field for each key winning.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GameFields {
    /// Each sheet key, such as `name`, `level`, `position` or `str`, and its
    /// field.
    pub character: BTreeMap<String, Field>,
    pub opponent: BTreeMap<OpponentField, Field>,
}

impl GameFields {
    /// Every enabled plugin's, in run order.
    pub fn of(host: &PluginHost) -> GameFields {
        GameFields::from_huds(
            host.declared(|points| points.hud.as_ref())
                .map(|(_, hud)| hud),
        )
    }

    /// These HUD points', later ones winning.
    pub fn from_huds<'a>(huds: impl IntoIterator<Item = &'a Hud>) -> GameFields {
        let mut fields = GameFields::default();
        for hud in huds {
            for (key, path) in &hud.character {
                if let Some(field) = Field::parse(path) {
                    fields.character.insert(key.clone(), field);
                }
            }
            for (key, path) in &hud.opponent {
                if let Some(field) = Field::parse(path) {
                    fields.opponent.insert(*key, field);
                }
            }
        }
        fields
    }

    /// Whether a message of `package` holds any of them, so one that doesn't
    /// isn't parsed.
    pub fn read_from(&self, package: &str) -> bool {
        let mut all = self.character.values().chain(self.opponent.values());
        all.any(|field| field.package.eq_ignore_ascii_case(package))
    }
}

/// Where the highest layer's HUD point says affects are listed.
pub fn affects_field(host: &PluginHost) -> Option<Field> {
    host.declared(|points| points.hud.as_ref())
        .filter_map(|(_, hud)| Field::parse(hud.affects.as_deref()?))
        .last()
}

/// What `message` lists at `field`, an object of each affect and how long it
/// lasts; anything else there means none. `None` if it doesn't say. A game
/// may file an affect under a kind, as `Kind: name`.
pub fn affects(field: &Field, message: &Map<String, Value>) -> Option<Vec<Affect>> {
    let listed = message.get(&field.key)?;
    let Some(listed) = listed.as_object() else {
        return Some(Vec::new());
    };
    let named = |(name, detail): (&String, &Value)| Affect {
        name: name.rsplit(": ").next().unwrap_or(name).to_owned(),
        detail: detail.as_str().unwrap_or_default().to_owned(),
    };
    Some(listed.iter().map(named).collect())
}

/// The vitals `message` (of `package`, in any case, as GMCP allows) holds,
/// by `fields`. Games send numbers or numeric strings; anything else is
/// skipped.
pub fn values(fields: &FieldMap, package: &str, message: &Map<String, Value>) -> Vec<(Vital, i64)> {
    fields
        .iter()
        .filter(|(_, field)| field.package.eq_ignore_ascii_case(package))
        .filter_map(|(vital, field)| {
            let value = match message.get(&field.key)? {
                Value::Number(number) => number.as_i64()?,
                Value::String(text) => text.trim().parse().ok()?,
                _ => return None,
            };
            Some((vital.clone(), value))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn message(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn experience_to_the_next_level_is_a_vital_by_its_common_names() {
        assert_eq!(Vital::from_name("tnl"), Vital::ToNextLevel);
        assert_eq!(Vital::from_name("max_tnl"), Vital::MaxToNextLevel);
        assert_eq!(
            Vital::from_name(Vital::ToNextLevel.name()),
            Vital::ToNextLevel
        );
    }

    #[test]
    fn affects_read_with_their_kind_dropped_and_nothing_listed_means_none() {
        let field = Field::parse("Char.Status.affects").unwrap();
        let listed = message(json!({"affects": {"Kind: bless": "for 6 hours", "fatigue": 3}}));
        let affects = affects_of(&field, &listed);
        assert_eq!(affects[0].name, "bless");
        assert_eq!(affects[0].detail, "for 6 hours");
        assert_eq!(affects[1].name, "fatigue");
        assert_eq!(affects[1].detail, "");

        assert_eq!(affects_of(&field, &message(json!({"affects": null}))), []);
        assert_eq!(affects_of(&field, &message(json!({"affects": ""}))), []);
        assert_eq!(super::affects(&field, &message(json!({"hp": 1}))), None);
    }

    fn affects_of(field: &Field, message: &Map<String, Value>) -> Vec<Affect> {
        super::affects(field, message).unwrap()
    }
}
