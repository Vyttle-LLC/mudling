//! One shape for what either client ended with, so they can be compared:
//! rooms by id, and characters and gear as flat keys and values.

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Room {
    pub id: i64,
    pub name: String,
    pub zone: String,
    pub description: String,
    pub environment: String,
    /// Direction to state, as the game sent it.
    pub exits: BTreeMap<String, String>,
    pub links: BTreeMap<String, i64>,
    pub position: (i64, i64, i64),
    pub building: Option<i64>,
    pub doorways: BTreeSet<String>,
    pub tangled: bool,
    pub maze: bool,
    pub features: BTreeSet<String>,
    pub sightings: usize,
}

impl Room {
    /// What two rooms must share to be the same room, as far as anything
    /// but the map's own paths can tell.
    pub fn identity(&self) -> (String, String, String, String) {
        (
            self.zone.clone(),
            self.name.clone(),
            self.description.clone(),
            self.exit_list(),
        )
    }

    pub fn exit_list(&self) -> String {
        let exits: Vec<&str> = self.exits.keys().map(String::as_str).collect();
        exits.join(",")
    }

    pub fn label(&self) -> String {
        format!("{} ({})", self.name, self.zone)
    }
}

#[derive(Debug, Default)]
pub struct World {
    pub rooms: BTreeMap<i64, Room>,
    pub aggressive: BTreeSet<String>,
    /// By lowercase character name. Keys: `title`, `stat.<key>`,
    /// `skill.<name>.<field>`, `affect.<name>: <detail>`, `worn.<slot>.<n>`.
    pub characters: BTreeMap<String, BTreeMap<String, String>>,
    /// `slot.<item>` and `keywords.<item>`.
    pub gear: BTreeMap<String, String>,
    /// Where each character last saw an item worn; only the prototype keeps
    /// this per character. By character, then `<item>`.
    pub item_slots: BTreeMap<String, BTreeMap<String, String>>,
    /// Every reading known, by the game's words for the item.
    pub identifies: BTreeMap<String, Vec<BTreeMap<String, String>>>,
    /// Items with a measured effect.
    pub measured: BTreeSet<String>,
    /// Copies known by the game's id.
    pub copies: usize,
}

/// `eq`'s slot labels, as the plugin's short names.
fn short_slot(slot: &str) -> String {
    let short = match slot {
        "used as light" => "light",
        "worn on finger" => "finger",
        "worn around neck" => "neck",
        "worn on body" => "body",
        "worn on head" => "head",
        "worn on legs" => "legs",
        "worn on feet" => "feet",
        "worn on hands" => "hands",
        "worn on arms" => "arms",
        "held in offhand" => "offhand",
        "worn about body" => "about",
        "worn about waist" => "waist",
        "worn on wrist" => "wrist",
        "wielded" | "wielded as weapon" => "wield",
        "held" => "held",
        other => other,
    };
    short.to_owned()
}

/// An item as other lines give it: without tags, lowercase.
pub fn plain_item(name: &str) -> String {
    let mut plain = String::new();
    let mut depth = 0usize;
    for c in name.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => plain.push(c),
            _ => {}
        }
    }
    let words: Vec<&str> = plain.split_whitespace().collect();
    words.join(" ").to_lowercase()
}

fn text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn set_of_keys(value: &Value) -> BTreeSet<String> {
    match value {
        Value::Array(items) => items.iter().filter_map(text).collect(),
        Value::Object(items) => items.keys().cloned().collect(),
        _ => BTreeSet::new(),
    }
}

/// `{"north":"O"}`, or the list of pairs Swift writes for an enum key.
fn pairs(value: &Value) -> Vec<(String, Value)> {
    match value {
        Value::Object(items) => items.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        Value::Array(items) => items
            .chunks(2)
            .filter_map(|pair| Some((text(pair.first()?)?, pair.get(1)?.clone())))
            .collect(),
        _ => Vec::new(),
    }
}

fn prototype_room(id: i64, room: &Value) -> Room {
    let field = |key: &str| room[key].as_str().unwrap_or("").to_owned();
    let position = &room["position"];
    let at = |axis: &str| position[axis].as_i64().unwrap_or(0);
    Room {
        id,
        name: field("name"),
        zone: field("zone"),
        description: field("description"),
        environment: field("environment"),
        exits: pairs(&room["exits"])
            .into_iter()
            .map(|(direction, state)| (direction, text(&state).unwrap_or_default()))
            .collect(),
        links: pairs(&room["links"])
            .into_iter()
            .filter_map(|(direction, to)| Some((direction, to.as_i64()?)))
            .collect(),
        position: (at("x"), at("y"), at("z")),
        building: room["building"].as_i64(),
        doorways: set_of_keys(&room["doorways"]),
        tangled: room["isTangled"].as_bool().unwrap_or(false),
        maze: room["isMaze"].as_bool().unwrap_or(false),
        features: set_of_keys(&room["features"]),
        sightings: pairs(&room["sightings"]).len(),
    }
}

fn prototype_character(record: &Value) -> BTreeMap<String, String> {
    let mut flat = BTreeMap::new();
    if let Some(title) = record["title"].as_str() {
        flat.insert("title".to_owned(), title.to_owned());
    }
    if let Some(age) = record["age"].as_str() {
        flat.insert("stat.age".to_owned(), age.to_owned());
    }
    for (key, value) in pairs(&record["stats"]) {
        flat.insert(format!("stat.{key}"), text(&value).unwrap_or_default());
    }
    for (key, value) in pairs(&record["scoreFacts"]) {
        let key = match key.as_str() {
            "In Bank" => "bank".to_owned(),
            other => other.to_lowercase(),
        };
        flat.insert(format!("stat.{key}"), text(&value).unwrap_or_default());
    }
    for (name, skill) in pairs(&record["skills"]) {
        for field in ["learned", "level", "mana"] {
            if let Some(value) = text(&skill[field]) {
                flat.insert(format!("skill.{name}.{field}"), value);
            }
        }
    }
    for affect in record["affects"].as_array().into_iter().flatten() {
        let name = affect["name"].as_str().unwrap_or("");
        let detail = affect["detail"].as_str().unwrap_or("");
        flat.insert(format!("affect.{name}: {detail}"), "on".to_owned());
    }
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for worn in record["equipment"].as_array().into_iter().flatten() {
        let slot = short_slot(worn["slot"].as_str().unwrap_or(""));
        let item = worn["item"].as_str().unwrap_or("");
        if item.is_empty() {
            continue;
        }
        let count = counts.entry(slot.clone()).or_default();
        *count += 1;
        flat.insert(format!("worn.{slot}.{count}"), plain_item(item));
    }
    flat
}

fn identification(reading: &Value) -> BTreeMap<String, String> {
    let mut flat = BTreeMap::new();
    if let Some(kind) = reading["type"].as_str() {
        flat.insert("type".to_owned(), kind.to_owned());
    }
    let flags: Vec<String> = reading["flags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(text)
        .collect();
    flat.insert("flags".to_owned(), flags.join(" "));
    for field in ["level", "quality"] {
        if let Some(value) = text(&reading[field]) {
            flat.insert(field.to_owned(), value);
        }
    }
    for (stat, amount) in pairs(&reading["base"]) {
        flat.insert(format!("base.{stat}"), text(&amount).unwrap_or_default());
    }
    for (spell, totals) in pairs(&reading["added"]) {
        for (stat, amount) in pairs(&totals) {
            flat.insert(
                format!("added.{spell}.{stat}"),
                text(&amount).unwrap_or_default(),
            );
        }
    }
    flat
}

/// The prototype's saved JSON: its own save, or the baseline in that shape.
/// `gear` is absent from a saved world's main file, so it's passed apart.
pub fn prototype_world(world: &Value, gear: &Value) -> World {
    let mut out = World::default();
    for (id, room) in pairs(&world["map"]["rooms"]) {
        if let Ok(id) = id.parse() {
            out.rooms.insert(id, prototype_room(id, &room));
        }
    }
    out.aggressive = set_of_keys(&world["mobs"]["aggressiveNouns"]);
    for (name, record) in pairs(&world["characters"]) {
        let name = name.to_lowercase();
        let slots = pairs(&record["itemSlots"]).into_iter();
        let slots =
            slots.filter_map(|(item, slot)| Some((plain_item(&item), short_slot(slot.as_str()?))));
        out.item_slots.insert(name.clone(), slots.collect());
        out.characters.insert(name, prototype_character(&record));
    }
    let keep = |out: &mut World, reading: &Value| {
        if let Some(words) = reading["keywords"].as_str() {
            out.identifies
                .entry(words.to_owned())
                .or_default()
                .push(identification(reading));
        }
    };
    for (key, item) in pairs(&gear["items"]) {
        let key = plain_item(&key);
        if let Some(slot) = item["slot"].as_str() {
            out.gear.insert(format!("slot.{key}"), short_slot(slot));
        }
        if let Some(words) = item["keywords"].as_str() {
            out.gear.insert(format!("keywords.{key}"), words.to_owned());
        }
        if item.get("measured").is_some() {
            out.measured.insert(key);
        }
        for reading in item["identifies"].as_array().into_iter().flatten() {
            keep(&mut out, reading);
        }
    }
    for (_, copy) in pairs(&gear["copies"]) {
        out.copies += 1;
        if copy.get("identified").is_some() {
            keep(&mut out, &copy["identified"]);
        }
        if copy.get("measured").is_some() {
            out.measured
                .insert(plain_item(copy["name"].as_str().unwrap_or("")));
        }
    }
    out
}
