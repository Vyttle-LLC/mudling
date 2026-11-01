//! The values hooks read and write, on their way to and from the stores and
//! the database.
//!
//! A hook names its own stores and keys: their meaning is the plugin's, and
//! the core only keeps them. Gear's stores are shared by a server's
//! characters, so they're saved in its `meta` table; the rest are the
//! character's own, saved as its stats.

use crate::defaults::hud::Vital;
use crate::plugin::luau::{StateView, Value};
use crate::store::{CharactersState, GearState, RoomState, StoreHub, Versioned, VitalsState};
use serde_json::Value as Json;
use std::sync::Arc;

/// The plugin stores about gear rather than the character.
const GEAR_STORES: [&str; 3] = ["gear", "equipment", "identify"];
/// Where a gear value's `meta` key starts.
pub(super) const GEAR_META_PREFIX: &str = "plugin.";

pub(super) fn is_gear_store(store: &str) -> bool {
    GEAR_STORES.contains(&store)
}

/// A value as its database row holds it: JSON, so it reads back as the same
/// type.
pub(super) fn to_row(value: &Value) -> String {
    match value {
        Value::Bool(value) => Json::from(*value),
        Value::Number(value) => Json::from(*value),
        Value::Text(value) => Json::from(value.as_str()),
    }
    .to_string()
}

pub(super) fn from_row(row: &str) -> Option<Value> {
    match serde_json::from_str(row).ok()? {
        Json::Bool(value) => Some(Value::Bool(value)),
        Json::Number(value) => Some(Value::Number(value.as_f64()?)),
        Json::String(value) => Some(Value::Text(value)),
        _ => None,
    }
}

/// What hooks with `read_state` see: `vitals.<name>`, `room.<field>`, and
/// any `<store>.<key>` a plugin wrote.
pub(super) struct HubView(pub(super) Arc<StoreHub>);

impl StateView for HubView {
    fn get(&self, key: &str) -> Option<Value> {
        let (store, field) = key.split_once('.')?;
        match store {
            "vitals" => self.0.read(|vitals: &Versioned<VitalsState>| {
                let value = vitals.value().0.get(&Vital::from_name(field))?;
                Some(Value::Number(*value as f64))
            }),
            "room" => self.0.read(|room: &Versioned<RoomState>| {
                let room = room.value().room.as_ref()?;
                match field {
                    "id" => Some(Value::Number(room.id as f64)),
                    "name" => Some(Value::Text(room.name.clone())),
                    "zone" => Some(Value::Text(room.zone.clone())),
                    _ => None,
                }
            }),
            _ if is_gear_store(store) => self
                .0
                .read(|gear: &Versioned<GearState>| gear.value().0.get(key).cloned()),
            _ => self.0.read(|character: &Versioned<CharactersState>| {
                character.value().0.get(key).cloned()
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_reads_back_from_its_row_as_the_same_type() {
        for value in [
            Value::Bool(true),
            Value::Number(12.0),
            Value::Number(-1.5),
            Value::Text("a \"quoted\" name".to_owned()),
        ] {
            assert_eq!(from_row(&to_row(&value)), Some(value));
        }
    }

    #[test]
    fn hooks_read_the_stores_by_key() {
        let hub = Arc::new(StoreHub::new(|| {}));
        hub.write::<VitalsState>(VitalsState([(Vital::Hp, 7)].into()));
        hub.write::<GearState>(GearState(
            [("gear.slot.ring".to_owned(), Value::Bool(true))].into(),
        ));
        let view = HubView(hub);
        assert_eq!(view.get("vitals.hp"), Some(Value::Number(7.0)));
        assert_eq!(view.get("gear.slot.ring"), Some(Value::Bool(true)));
        assert_eq!(view.get("skills.dodge"), None);
        assert_eq!(view.get("nothing"), None);
    }
}
