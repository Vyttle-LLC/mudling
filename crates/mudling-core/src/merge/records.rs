//! Merging two copies of a server's rows, record by record.

use super::rooms::unite_rooms;
use crate::persist::{CharacterRows, GearRows, Key, Keyed, MapRows, Timestamp, Tombstone};
use crate::repo::format::{ServerRows, json};
use serde::Serialize;
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

/// The newest copy of every record, without what a tombstone rules out.
/// Parents go before their children, which can't outlive them.
pub(super) fn merge_server(mut ours: ServerRows, mut theirs: ServerRows) -> ServerRows {
    unite_rooms(&mut ours, &mut theirs);
    let mut merger = Merger::new(ours.tombstones, theirs.tombstones);
    ServerRows {
        servers: merger.rows(ours.servers, theirs.servers),
        meta: merger.rows(ours.meta, theirs.meta),
        map: merger.map(ours.map, theirs.map),
        marks: merger.rows(ours.marks, theirs.marks),
        characters: merger.characters(ours.characters, theirs.characters),
        gear: merger.gear(ours.gear, theirs.gear),
        triggers: merger.rows(ours.triggers, theirs.triggers),
        aliases: merger.rows(ours.aliases, theirs.aliases),
        tombstones: merger.tombstones(),
    }
}

struct Merger {
    deleted: BTreeMap<Key, Timestamp>,
    /// Every record kept so far.
    kept: BTreeSet<Key>,
}

impl Merger {
    fn new(ours: Vec<Tombstone>, theirs: Vec<Tombstone>) -> Merger {
        let mut deleted: BTreeMap<Key, Timestamp> = BTreeMap::new();
        for Tombstone { key, deleted: at } in ours.into_iter().chain(theirs) {
            let newest = deleted.entry(key).or_default();
            *newest = at.max(*newest);
        }
        Merger {
            deleted,
            kept: BTreeSet::new(),
        }
    }

    fn map(&mut self, ours: MapRows, theirs: MapRows) -> MapRows {
        MapRows {
            rooms: self.rows(ours.rooms, theirs.rooms),
            exits: self.rows(ours.exits, theirs.exits),
            links: self.rows(ours.links, theirs.links),
            doorways: self.rows(ours.doorways, theirs.doorways),
            notes: self.rows(ours.notes, theirs.notes),
            features: self.rows(ours.features, theirs.features),
            sightings: self.rows(ours.sightings, theirs.sightings),
        }
    }

    fn characters(&mut self, ours: CharacterRows, theirs: CharacterRows) -> CharacterRows {
        CharacterRows {
            characters: self.rows(ours.characters, theirs.characters),
            stats: self.rows(ours.stats, theirs.stats),
            skills: self.rows(ours.skills, theirs.skills),
            affects: self.rows(ours.affects, theirs.affects),
            worn: self.rows(ours.worn, theirs.worn),
            gear_sets: self.rows(ours.gear_sets, theirs.gear_sets),
        }
    }

    fn gear(&mut self, ours: GearRows, theirs: GearRows) -> GearRows {
        GearRows {
            copies: self.rows(ours.copies, theirs.copies),
            facts: self.rows(ours.facts, theirs.facts),
            identifications: self.rows(ours.identifications, theirs.identifications),
        }
    }

    /// One record type's merged rows, in key order, as a database reads them.
    fn rows<T: Keyed + Serialize>(&mut self, ours: Vec<T>, theirs: Vec<T>) -> Vec<T> {
        let mut newest: BTreeMap<Key, T> = BTreeMap::new();
        for record in ours.into_iter().chain(theirs) {
            match newest.entry(record.key()) {
                Entry::Vacant(entry) => {
                    entry.insert(record);
                }
                Entry::Occupied(mut entry) => {
                    if outranks(&record, entry.get()) {
                        entry.insert(record);
                    }
                }
            }
        }
        newest.retain(|key, record| self.survives(key, record));
        self.kept.extend(newest.keys().cloned());
        newest.into_values().collect()
    }

    /// A tombstone rules out what was last changed before it, for the record
    /// and for its children. A child newer than its owner's delete still goes
    /// if the owner stayed deleted, because it has nothing to hang off.
    fn survives<T: Keyed>(&self, key: &Key, record: &T) -> bool {
        let updated = record.updated();
        let rules_out = |key: &Key| self.deleted.get(key).is_some_and(|at| *at > updated);
        !rules_out(key)
            && record
                .owners()
                .iter()
                .all(|owner| self.kept.contains(owner) && !rules_out(owner))
    }

    fn tombstones(self) -> Vec<Tombstone> {
        self.deleted
            .into_iter()
            .map(|(key, deleted)| Tombstone { key, deleted })
            .collect()
    }
}

/// Whether `record` beats `other`. Equal times fall to the record's bytes, so
/// both devices pick the same copy.
pub(super) fn outranks<T: Keyed + Serialize>(record: &T, other: &T) -> bool {
    match record.updated().cmp(&other.updated()) {
        std::cmp::Ordering::Equal => json(record) > json(other),
        order => order.is_gt(),
    }
}
