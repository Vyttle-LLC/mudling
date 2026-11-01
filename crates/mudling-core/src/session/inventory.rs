//! What the player carries, from standard GMCP `Char.Items`, as hooks read
//! it: `inventory.count`, then `inventory.<n>.id` and `inventory.<n>.name`
//! for n from 1. Entries past `count` are stale. The session asks for it on
//! entering the game, so it's kept for the connection and never saved.

use crate::ansi;
use crate::gear::plain_name;
use crate::plugin::luau::Value;
use crate::store::CHARACTER_INVENTORY;
use crate::telnet::gmcp;
use serde::{Deserialize, Deserializer};
use std::collections::BTreeMap;

const LOCATION: &str = "inv";

/// The message that asks the server for the whole inventory.
pub(super) const ASK: &str = "Char.Items.Inv";

#[derive(Debug, Deserialize)]
pub(super) struct Item {
    pub(super) id: String,
    #[serde(deserialize_with = "without_colors")]
    pub(super) name: String,
}

/// Some games color item names.
fn without_colors<'de, D: Deserializer<'de>>(names: D) -> Result<String, D::Error> {
    Ok(ansi::plain(&String::deserialize(names)?))
}

#[derive(Deserialize)]
pub(super) struct List {
    pub(super) location: String,
    pub(super) items: Vec<Item>,
}

#[derive(Deserialize)]
pub(super) struct Change {
    pub(super) location: String,
    pub(super) item: Item,
}

#[derive(Debug, Default)]
pub(super) struct Inventory {
    items: Vec<Item>,
}

impl Inventory {
    /// Reads a GMCP message. Returns the state view's entries if it changed
    /// what's carried.
    pub(super) fn read(&mut self, package: &str, json: &str) -> Option<BTreeMap<String, Value>> {
        if package.eq_ignore_ascii_case("Char.Items.List") {
            let list: List = gmcp::from_json(json)?;
            if list.location != LOCATION {
                return None;
            }
            self.items = list.items;
        } else if package.eq_ignore_ascii_case("Char.Items.Add") {
            let added: Change = gmcp::from_json(json)?;
            if added.location != LOCATION {
                return None;
            }
            self.items.push(added.item);
        } else if package.eq_ignore_ascii_case("Char.Items.Remove") {
            let removed: Change = gmcp::from_json(json)?;
            if removed.location != LOCATION {
                return None;
            }
            self.items.retain(|item| item.id != removed.item.id);
        } else {
            return None;
        }
        Some(self.entries())
    }

    pub(super) fn items(&self) -> impl Iterator<Item = &Item> {
        self.items.iter()
    }

    /// The game says nothing here when gear goes on, so its message moves the
    /// copy. Takes the copy of the plain name `key`, returning its id.
    pub(super) fn wore(&mut self, key: &str) -> Option<String> {
        let at = (self.items.iter()).position(|item| plain_name(&item.name) == key)?;
        Some(self.items.remove(at).id)
    }

    /// Puts a copy back that came off.
    pub(super) fn took_off(&mut self, id: String, name: String) {
        self.items.retain(|item| item.id != id);
        self.items.push(Item { id, name });
    }

    pub(super) fn entries(&self) -> BTreeMap<String, Value> {
        let mut entries = BTreeMap::new();
        let count = Value::Number(self.items.len() as f64);
        entries.insert(format!("{CHARACTER_INVENTORY}.count"), count);
        for (index, item) in self.items.iter().enumerate() {
            let n = index + 1;
            let id = Value::Text(item.id.clone());
            let name = Value::Text(item.name.clone());
            entries.insert(format!("{CHARACTER_INVENTORY}.{n}.id"), id);
            entries.insert(format!("{CHARACTER_INVENTORY}.{n}.name"), name);
        }
        entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(entries: &BTreeMap<String, Value>, n: usize) -> Option<&Value> {
        entries.get(&format!("inventory.{n}.name"))
    }

    #[test]
    fn raw_color_codes_in_a_name_are_read_and_stripped() {
        let mut inventory = Inventory::default();
        let added = "{\"location\":\"inv\",\"item\":{\"id\":\"1\",\
             \"name\":\"\x1b[1;35ma round shield\x1b[0;32m\"}}";
        let entries = inventory.read("Char.Items.Add", added).unwrap();
        assert_eq!(entries["inventory.count"], Value::Number(1.0));
        assert_eq!(
            name(&entries, 1),
            Some(&Value::Text("a round shield".to_owned()))
        );
        let entries = inventory.read("Char.Items.Remove", added).unwrap();
        assert_eq!(entries["inventory.count"], Value::Number(0.0));

        let list = "{\"location\":\"inv\",\"items\":[{\"id\":\"2\",\
             \"name\":\"\x1b[1;33ma lamp\x1b[0m\"}]}";
        let entries = inventory.read("Char.Items.List", list).unwrap();
        assert_eq!(name(&entries, 1), Some(&Value::Text("a lamp".to_owned())));
    }

    #[test]
    fn items_come_and_go_and_other_places_are_ignored() {
        let mut inventory = Inventory::default();
        let list = r#"{"location":"inv","items":[
            {"id":"1","name":"a rope","type":"gear"},{"id":"2","name":"a lamp"}]}"#;
        let entries = inventory.read("Char.Items.List", list).unwrap();
        assert_eq!(entries["inventory.count"], Value::Number(2.0));
        assert_eq!(name(&entries, 2), Some(&Value::Text("a lamp".to_owned())));

        let room = r#"{"location":"room","item":{"id":"3","name":"a rock"}}"#;
        assert_eq!(inventory.read("Char.Items.Add", room), None);

        let gone = r#"{"location":"inv","item":{"id":"1","name":"a rope"}}"#;
        let entries = inventory.read("Char.Items.Remove", gone).unwrap();
        assert_eq!(entries["inventory.count"], Value::Number(1.0));
        assert_eq!(name(&entries, 1), Some(&Value::Text("a lamp".to_owned())));

        assert_eq!(inventory.read("Char.Vitals", "{}"), None);
    }
}
