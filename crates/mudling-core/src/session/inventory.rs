//! What the player carries, from standard GMCP `Char.Items`, as hooks read
//! it: `inventory.count`, then `inventory.<n>.id` and `inventory.<n>.name`
//! for n from 1. Entries past `count` are stale. It's sent whole on login,
//! so it's kept for the connection and never saved.

use crate::plugin::luau::Value;
use serde::Deserialize;
use std::collections::BTreeMap;

const LOCATION: &str = "inv";

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
struct Item {
    id: String,
    name: String,
}

#[derive(Deserialize)]
struct List {
    location: String,
    items: Vec<Item>,
}

#[derive(Deserialize)]
struct Change {
    location: String,
    item: Item,
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
            let list: List = serde_json::from_str(json).ok()?;
            if list.location != LOCATION {
                return None;
            }
            self.items = list.items;
        } else if package.eq_ignore_ascii_case("Char.Items.Add") {
            let added: Change = serde_json::from_str(json).ok()?;
            if added.location != LOCATION {
                return None;
            }
            self.items.push(added.item);
        } else if package.eq_ignore_ascii_case("Char.Items.Remove") {
            let removed: Change = serde_json::from_str(json).ok()?;
            if removed.location != LOCATION {
                return None;
            }
            self.items.retain(|item| item.id != removed.item.id);
        } else {
            return None;
        }
        Some(self.entries())
    }

    fn entries(&self) -> BTreeMap<String, Value> {
        let mut entries = BTreeMap::new();
        let count = Value::Number(self.items.len() as f64);
        entries.insert("inventory.count".to_owned(), count);
        for (index, item) in self.items.iter().enumerate() {
            let n = index + 1;
            let id = Value::Text(item.id.clone());
            let name = Value::Text(item.name.clone());
            entries.insert(format!("inventory.{n}.id"), id);
            entries.insert(format!("inventory.{n}.name"), name);
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
