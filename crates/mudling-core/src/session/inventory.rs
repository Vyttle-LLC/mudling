//! What the player carries, from standard GMCP `Char.Items`, as hooks read
//! it: `inventory.count`, then `inventory.<n>.id` and `inventory.<n>.name`
//! for n from 1. Entries past `count` are stale. The session asks for it on
//! entering the game, so it's kept for the connection and never saved.
//!
//! A carried item of the type [`BAG_TYPE`] is a bag. The session asks once what
//! each is holding, and the game's lists and changes then name the bag's id as
//! their location.

use crate::ansi;
use crate::gear::plain_name;
use crate::plugin::luau::Value;
use crate::store::{CHARACTER_INVENTORY, CarriedBag, CarriedItem};
use crate::telnet::gmcp;
use serde::{Deserialize, Deserializer};
use std::collections::{BTreeMap, BTreeSet};

const LOCATION: &str = "inv";
const BAG_TYPE: &str = "container";

/// The message that asks the server for the whole inventory.
pub(super) const ASK: &str = "Char.Items.Inv";
/// The message that asks the server what's in a bag, by its id.
pub(super) const ASK_BAG: &str = "Char.Items.Contents";

#[derive(Clone, Debug, Deserialize)]
pub(super) struct Item {
    pub(super) id: String,
    #[serde(deserialize_with = "without_colors")]
    pub(super) name: String,
    #[serde(default, rename = "type")]
    kind: Option<String>,
}

impl Item {
    fn is_bag(&self) -> bool {
        self.kind.as_deref() == Some(BAG_TYPE)
    }

    fn carried(&self) -> CarriedItem {
        CarriedItem {
            id: self.id.clone(),
            name: self.name.clone(),
        }
    }
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
    /// What's in each bag, by the bag's id, as the game last said.
    bagged: BTreeMap<String, Vec<Item>>,
    /// The bags asked about on this connection.
    asked: BTreeSet<String>,
}

impl Inventory {
    /// Reads a GMCP message for a change to what's in a bag carried. Returns
    /// whether it was one.
    pub(super) fn read_bag(&mut self, package: &str, json: &str) -> bool {
        let is_bag = |items: &[Item], location: &str| {
            items.iter().any(|item| item.id == location && item.is_bag())
        };
        if package.eq_ignore_ascii_case("Char.Items.List") {
            let Some(list) = gmcp::from_json::<List>(json) else {
                return false;
            };
            if !is_bag(&self.items, &list.location) {
                return false;
            }
            self.bagged.insert(list.location, list.items);
            return true;
        }
        let adding = package.eq_ignore_ascii_case("Char.Items.Add");
        if !adding && !package.eq_ignore_ascii_case("Char.Items.Remove") {
            return false;
        }
        let Some(change) = gmcp::from_json::<Change>(json) else {
            return false;
        };
        if !is_bag(&self.items, &change.location) {
            return false;
        }
        let inside = self.bagged.entry(change.location).or_default();
        inside.retain(|item| item.id != change.item.id);
        if adding {
            inside.push(change.item);
        }
        true
    }

    /// The bags not asked about yet. Each is only given out once.
    pub(super) fn bags_to_ask(&mut self) -> Vec<String> {
        let bags = self.items.iter().filter(|item| item.is_bag());
        let new = bags.filter(|bag| self.asked.insert(bag.id.clone()));
        new.map(|bag| bag.id.clone()).collect()
    }

    pub(super) fn bags(&self) -> Vec<CarriedBag> {
        let bags = self.items.iter().filter(|item| item.is_bag());
        bags.map(|bag| CarriedBag {
            id: bag.id.clone(),
            name: bag.name.clone(),
            items: (self.bagged.get(&bag.id).into_iter().flatten())
                .map(Item::carried)
                .collect(),
        })
        .collect()
    }

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
    /// A set's plan says which copy it named; the game picks among any others.
    pub(super) fn wore(&mut self, key: &str, named: Option<&str>) -> Option<String> {
        let exact = named.and_then(|id| self.items.iter().position(|item| item.id == id));
        let first = || (self.items.iter()).position(|item| plain_name(&item.name) == key);
        let at = exact.or_else(first)?;
        Some(self.items.remove(at).id)
    }

    /// Puts a copy back that came off.
    pub(super) fn took_off(&mut self, id: String, name: String) {
        self.items.retain(|item| item.id != id);
        self.items.push(Item {
            id,
            name,
            kind: None,
        });
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

    fn with_a_bag() -> Inventory {
        let mut inventory = Inventory::default();
        let list = r#"{"location":"inv","items":[
            {"id":"30","name":"a satchel","type":"container"},
            {"id":"31","name":"a rope","type":"gear"}]}"#;
        inventory.read("Char.Items.List", list);
        inventory
    }

    #[test]
    fn a_bag_is_asked_about_once_and_lists_what_the_game_says() {
        let mut inventory = with_a_bag();
        assert_eq!(inventory.bags_to_ask(), ["30"]);
        assert!(inventory.bags_to_ask().is_empty());

        let list = r#"{"location":"30","items":[{"id":"1","name":"a robe","type":"armor"}]}"#;
        assert!(inventory.read_bag("Char.Items.List", list));
        let added = r#"{"location":"30","item":{"id":"2","name":"a mask","type":"armor"}}"#;
        assert!(inventory.read_bag("Char.Items.Add", added));
        let gone = r#"{"location":"30","item":{"id":"1","name":"a robe"}}"#;
        assert!(inventory.read_bag("Char.Items.Remove", gone));

        let bags = inventory.bags();
        assert_eq!(bags[0].name, "a satchel");
        let names: Vec<&str> = bags[0].items.iter().map(|item| item.name.as_str()).collect();
        assert_eq!(names, ["a mask"]);
    }

    #[test]
    fn only_a_bag_carried_is_read_as_one() {
        let mut inventory = with_a_bag();
        let room = r#"{"location":"room","items":[{"id":"3","name":"a rock"}]}"#;
        assert!(!inventory.read_bag("Char.Items.List", room));
        let not_a_bag = r#"{"location":"31","items":[]}"#;
        assert!(!inventory.read_bag("Char.Items.List", not_a_bag));
        assert!(!inventory.read_bag("Char.Vitals", "{}"));
    }

    #[test]
    fn the_copy_a_plan_named_is_the_one_that_goes_on() {
        let mut inventory = Inventory::default();
        let list = r#"{"location":"inv","items":[
            {"id":"1","name":"an amulet"},{"id":"2","name":"an amulet"}]}"#;
        inventory.read("Char.Items.List", list);
        assert_eq!(inventory.wore("an amulet", Some("2")).as_deref(), Some("2"));
        let left: Vec<&str> = inventory.items().map(|item| item.id.as_str()).collect();
        assert_eq!(left, ["1"]);
    }
}
