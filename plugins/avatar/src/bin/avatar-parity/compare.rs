//! What differs between two worlds, by kind, each with what it's about.

use crate::model::{Room, World};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize)]
pub struct Difference {
    pub kind: &'static str,
    pub name: String,
    pub detail: String,
}

struct Differences(Vec<Difference>);

impl Differences {
    fn add(&mut self, kind: &'static str, name: impl Into<String>, detail: impl Into<String>) {
        self.0.push(Difference {
            kind,
            name: name.into(),
            detail: detail.into(),
        });
    }
}

/// Which of `theirs` each of `ours` is: rooms with the same `key`, in order
/// of id. Ids themselves don't match: both clients number rooms as they
/// find them, and the prototype spends some on a maze before it knows one.
fn pair_by<K: Ord>(
    ours: &mut BTreeSet<i64>,
    theirs: &mut BTreeSet<i64>,
    rooms: (&BTreeMap<i64, Room>, &BTreeMap<i64, Room>),
    pairs: &mut BTreeMap<i64, i64>,
    key: impl Fn(&Room) -> K,
) {
    let (our_rooms, their_rooms) = rooms;
    let mut waiting: BTreeMap<K, Vec<i64>> = BTreeMap::new();
    for id in theirs.iter().rev() {
        waiting.entry(key(&their_rooms[id])).or_default().push(*id);
    }
    for id in ours.clone() {
        if let Some(their_id) = waiting.get_mut(&key(&our_rooms[&id])).and_then(Vec::pop) {
            pairs.insert(id, their_id);
            ours.remove(&id);
            theirs.remove(&their_id);
        }
    }
}

fn compare_rooms(a: &Room, b: &Room, pairs: &BTreeMap<i64, i64>, out: &mut Differences) {
    let name = a.label();
    if a.description != b.description {
        out.add(
            "map.description",
            &name,
            format!("{:?} / {:?}", a.description, b.description),
        );
    }
    if a.exit_list() != b.exit_list() {
        out.add(
            "map.exits",
            &name,
            format!("{} / {}", a.exit_list(), b.exit_list()),
        );
    } else if a.exits != b.exits {
        out.add(
            "map.exit-state",
            &name,
            format!("{:?} / {:?}", a.exits, b.exits),
        );
    }
    if a.environment != b.environment {
        out.add(
            "map.environment",
            &name,
            format!("{} / {}", a.environment, b.environment),
        );
    }
    if a.position != b.position {
        out.add(
            "map.position",
            &name,
            format!("{:?} / {:?}", a.position, b.position),
        );
    }
    if a.building.and_then(|id| pairs.get(&id).copied()) != b.building {
        out.add(
            "map.building",
            &name,
            format!("{:?} / {:?}", a.building, b.building),
        );
    }
    if a.tangled != b.tangled {
        out.add(
            "map.tangled",
            &name,
            format!("{} / {}", a.tangled, b.tangled),
        );
    }
    if a.maze != b.maze {
        out.add("map.maze", &name, format!("{} / {}", a.maze, b.maze));
    }
    if a.doorways != b.doorways {
        out.add(
            "map.doorways",
            &name,
            format!("{:?} / {:?}", a.doorways, b.doorways),
        );
    }
    if a.sightings != b.sightings {
        out.add(
            "map.sightings",
            &name,
            format!("{} / {}", a.sightings, b.sightings),
        );
    }
    let features: BTreeSet<String> = a.features.iter().map(|f| feature_name(f)).collect();
    if features != b.features {
        out.add(
            "map.features",
            &name,
            format!("{features:?} / {:?}", b.features),
        );
    }
    let directions: BTreeSet<&String> = a.links.keys().chain(b.links.keys()).collect();
    for direction in directions {
        let ours = a.links.get(direction);
        let theirs = b.links.get(direction);
        match (ours, theirs) {
            (Some(ours), Some(theirs)) if pairs.get(ours) == Some(theirs) => {}
            (Some(ours), Some(theirs)) => out.add(
                "map.link-target",
                &name,
                format!("{direction}: #{ours} / #{theirs}"),
            ),
            (Some(ours), None) => out.add(
                "map.link-only-in-prototype",
                &name,
                format!("{direction}: #{ours}"),
            ),
            (None, Some(theirs)) => out.add(
                "map.link-only-in-mudling",
                &name,
                format!("{direction}: #{theirs}"),
            ),
            (None, None) => {}
        }
    }
}

/// The prototype's feature names, as the map's.
fn feature_name(feature: &str) -> String {
    match feature {
        "aggressiveMob" => "aggressive-mob".to_owned(),
        other => other.to_owned(),
    }
}

fn compare_map(prototype: &World, mudling: &World, out: &mut Differences) {
    let rooms = (&prototype.rooms, &mudling.rooms);
    let mut ours: BTreeSet<i64> = prototype.rooms.keys().copied().collect();
    let mut theirs: BTreeSet<i64> = mudling.rooms.keys().copied().collect();
    let mut pairs = BTreeMap::new();
    pair_by(&mut ours, &mut theirs, rooms, &mut pairs, Room::identity);
    pair_by(&mut ours, &mut theirs, rooms, &mut pairs, |room| {
        (room.zone.clone(), room.name.clone(), room.exit_list())
    });
    pair_by(&mut ours, &mut theirs, rooms, &mut pairs, |room| {
        (room.zone.clone(), room.name.clone())
    });
    for id in &ours {
        let room = &prototype.rooms[id];
        out.add(
            "map.room-only-in-prototype",
            room.label(),
            format!("#{id} {:?} exits {}", room.description, room.exit_list()),
        );
    }
    for id in &theirs {
        let room = &mudling.rooms[id];
        out.add(
            "map.room-only-in-mudling",
            room.label(),
            format!("#{id} {:?} exits {}", room.description, room.exit_list()),
        );
    }
    for (ours, theirs) in &pairs {
        compare_rooms(&prototype.rooms[ours], &mudling.rooms[theirs], &pairs, out);
    }
}

/// The part of a flat key that says what sort of fact it is.
fn fact_kind(key: &str) -> &'static str {
    match key.split('.').next() {
        Some("title") => "character.title",
        Some("stat") => "character.stat",
        Some("skill") => "character.skill",
        Some("affect") => "character.affect",
        Some("worn") => "character.worn",
        _ => "character.other",
    }
}

fn compare_flat(
    name: &str,
    ours: &BTreeMap<String, String>,
    theirs: &BTreeMap<String, String>,
    kind: impl Fn(&str) -> &'static str,
    out: &mut Differences,
) {
    let keys: BTreeSet<&String> = ours.keys().chain(theirs.keys()).collect();
    for key in keys {
        let (a, b) = (ours.get(key), theirs.get(key));
        if a != b {
            out.add(kind(key), format!("{name} {key}"), format!("{a:?} / {b:?}"));
        }
    }
}

fn compare_people(prototype: &World, mudling: &World, out: &mut Differences) {
    let names: BTreeSet<&String> = prototype
        .characters
        .keys()
        .chain(mudling.characters.keys())
        .collect();
    let empty = BTreeMap::new();
    for name in names {
        compare_flat(
            name,
            prototype.characters.get(name).unwrap_or(&empty),
            mudling.characters.get(name).unwrap_or(&empty),
            fact_kind,
            out,
        );
    }
    // MUDling keeps where an item goes once per server.
    for (name, slots) in &prototype.item_slots {
        for (item, slot) in slots {
            let shared = mudling.gear.get(&format!("slot.{item}"));
            if shared != Some(slot) {
                out.add(
                    "character.item-slot",
                    format!("{name} {item}"),
                    format!("{slot} / {shared:?}"),
                );
            }
        }
    }
}

fn compare_gear(prototype: &World, mudling: &World, out: &mut Differences) {
    compare_flat(
        "gear",
        &prototype.gear,
        &mudling.gear,
        |key| {
            if key.starts_with("slot.") {
                "gear.slot"
            } else {
                "gear.keywords"
            }
        },
        out,
    );
    let words: BTreeSet<&String> = prototype
        .identifies
        .keys()
        .chain(mudling.identifies.keys())
        .collect();
    for words in words {
        match (
            prototype.identifies.get(words),
            mudling.identifies.get(words),
        ) {
            (Some(ours), Some(theirs)) => {
                for reading in theirs {
                    if !ours.contains(reading) {
                        out.add(
                            "gear.identify-more-readings",
                            words.clone(),
                            format!("{ours:?} / {reading:?}"),
                        );
                    }
                }
                let mut seen = Vec::new();
                for reading in ours {
                    if !theirs.contains(reading) && !seen.contains(&reading) {
                        seen.push(reading);
                        out.add(
                            "gear.identify-reading-missing",
                            words.clone(),
                            format!("{reading:?} / none"),
                        );
                    }
                }
            }
            (Some(ours), None) => out.add(
                "gear.identify-only-in-prototype",
                words.clone(),
                format!("{ours:?}"),
            ),
            (None, Some(theirs)) => out.add(
                "gear.identify-only-in-mudling",
                words.clone(),
                format!("{theirs:?}"),
            ),
            (None, None) => {}
        }
    }
    for item in &prototype.measured {
        if !mudling.measured.contains(item) {
            out.add(
                "gear.measured-only-in-prototype",
                item.clone(),
                "measured / none",
            );
        }
    }
    for item in mudling.measured.difference(&prototype.measured) {
        out.add(
            "gear.measured-only-in-mudling",
            item.clone(),
            "none / measured",
        );
    }
    if prototype.copies != mudling.copies {
        out.add(
            "gear.copies",
            "copies by game id",
            format!("{} / {}", prototype.copies, mudling.copies),
        );
    }
}

/// Every difference, `prototype` / `mudling` in each detail.
pub fn compare(prototype: &World, mudling: &World) -> Vec<Difference> {
    let mut out = Differences(Vec::new());
    compare_map(prototype, mudling, &mut out);
    for noun in prototype.aggressive.difference(&mudling.aggressive) {
        out.add("mobs.aggressive", noun.clone(), "only in the prototype");
    }
    for noun in mudling.aggressive.difference(&prototype.aggressive) {
        out.add("mobs.aggressive", noun.clone(), "only in MUDling");
    }
    compare_people(prototype, mudling, &mut out);
    compare_gear(prototype, mudling, &mut out);
    out.0
}
