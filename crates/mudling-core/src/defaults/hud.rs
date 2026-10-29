//! HUD fields: which GMCP field holds each vital.
//!
//! The defaults read the keys of the first `Char.Vitals` message and match
//! them by common names (`hp`, `HP`, `health`; `maxhp`, `max_hp`; …). A
//! plugin's HUD point maps fields itself, and its mapping wins, the highest
//! layer last.

use crate::plugin::PluginHost;
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
    /// A vital only a plugin names, such as a game's own resource.
    Other(String),
}

impl Vital {
    /// The vital a field or HUD name stands for, by common names; case,
    /// `_` and `-` are ignored.
    pub fn from_name(name: &str) -> Vital {
        let key: String = name.chars().filter(|c| !matches!(c, '_' | '-')).collect::<String>().to_lowercase();
        match key.as_str() {
            "hp" | "health" | "hits" | "hitpoints" | "curhp" => Vital::Hp,
            "maxhp" | "hpmax" | "maxhealth" | "healthmax" | "maxhits" | "maxhitpoints" => Vital::MaxHp,
            "mana" | "mp" | "sp" | "curmana" => Vital::Mana,
            "maxmana" | "manamax" | "maxmp" | "mpmax" | "maxsp" => Vital::MaxMana,
            "moves" | "move" | "mv" | "movement" | "curmoves" => Vital::Moves,
            "maxmoves" | "maxmove" | "movesmax" | "maxmv" | "mvmax" | "maxmovement" => Vital::MaxMoves,
            _ => Vital::Other(name.to_owned()),
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
        (!package.is_empty() && !key.is_empty()).then(|| Field { package: package.to_owned(), key: key.to_owned() })
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
                vital => Some((vital, Field { package: VITALS.to_owned(), key: key.clone() })),
            })
            .collect();
        self.learned = Some(learned);
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
