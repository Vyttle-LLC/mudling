//! What callers hand the writer: a batch of row changes, applied in one
//! transaction.

use super::records::*;
use rusqlite::{Transaction, params};

/// Changes that commit together or not at all.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Batch {
    pub(crate) changes: Vec<Change>,
}

impl Batch {
    pub fn new() -> Batch {
        Batch::default()
    }

    pub fn upsert(&mut self, record: impl Into<Record>) -> &mut Batch {
        self.changes.push(Change::Upsert(record.into()));
        self
    }

    pub fn delete(&mut self, key: Key) -> &mut Batch {
        self.changes.push(Change::Delete(key));
        self
    }

    /// Saves the live map, as it stands after the changes before this one,
    /// as a new version.
    pub fn save_map_version(&mut self, kind: &str, label: &str, created: Timestamp) -> &mut Batch {
        self.changes.push(Change::SaveMapVersion {
            kind: kind.to_owned(),
            label: label.to_owned(),
            created,
        });
        self
    }

    /// Replaces the live map with a saved version; restored rows take
    /// `at` as their `updated` time, since restoring is a change of its own.
    pub fn restore_map_version(&mut self, version_id: i64, at: Timestamp) -> &mut Batch {
        self.changes
            .push(Change::RestoreMapVersion { version_id, at });
        self
    }

    pub fn len(&self) -> usize {
        self.changes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

// Records live inline in the batch's one Vec; boxing them would cost the
// caller an allocation per change.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Change {
    Upsert(Record),
    Delete(Key),
    SaveMapVersion {
        kind: String,
        label: String,
        created: Timestamp,
    },
    RestoreMapVersion {
        version_id: i64,
        at: Timestamp,
    },
}

impl Change {
    pub(crate) fn apply(&self, tx: &Transaction<'_>) -> rusqlite::Result<()> {
        match self {
            Change::Upsert(record) => record.upsert(tx),
            Change::Delete(key) => key.delete(tx),
            Change::SaveMapVersion {
                kind,
                label,
                created,
            } => save_map_version(tx, kind, label, *created),
            Change::RestoreMapVersion { version_id, at } => {
                restore_map_version(tx, *version_id, *at)
            }
        }
    }
}

macro_rules! records {
    ($($kind:ident),* $(,)?) => {
        /// Any record, for [`Batch::upsert`].
        #[derive(Clone, Debug, PartialEq)]
        pub enum Record {
            $($kind($kind),)*
        }

        $(impl From<$kind> for Record {
            fn from(record: $kind) -> Record {
                Record::$kind(record)
            }
        })*

        impl Record {
            fn upsert(&self, tx: &Transaction<'_>) -> rusqlite::Result<()> {
                match self {
                    $(Record::$kind(record) => upsert(tx, record),)*
                }
            }
        }
    };
}

records!(
    Server,
    Character,
    Meta,
    Room,
    Exit,
    RoomNote,
    RoomFeature,
    Sighting,
    MobMark,
    CharacterStat,
    Skill,
    Affect,
    Worn,
    GearSet,
    GearCopy,
    ItemFacts,
    Identification,
);

fn upsert<T: Stored>(tx: &Transaction<'_>, record: &T) -> rusqlite::Result<()> {
    record.upsert(&mut *tx.prepare_cached(T::UPSERT)?)?;
    Ok(())
}

/// A record's key, for [`Batch::delete`]. Deleting a room deletes its
/// exits, note, features and sightings, and unlinks exits that led to it;
/// deleting a character deletes its stats, skills, affects, worn items and
/// gear sets.
#[derive(Clone, Debug, PartialEq)]
pub enum Key {
    Server(String),
    Character(String),
    Meta(String),
    Room(i64),
    Exit { room_id: i64, direction: String },
    RoomNote(i64),
    RoomFeature { room_id: i64, feature: String },
    Sighting { room_id: i64, mob: String },
    MobMark { name: String, mark: String },
    CharacterStat { character_id: String, key: String },
    Skill { character_id: String, name: String },
    Affect { character_id: String, name: String },
    Worn { character_id: String, position: i64 },
    GearSet(String),
    GearCopy(String),
    ItemFacts(String),
    Identification(String),
    MapVersion(i64),
}

impl Key {
    fn delete(&self, tx: &Transaction<'_>) -> rusqlite::Result<()> {
        let run = |sql: &str, params: &[&dyn rusqlite::ToSql]| -> rusqlite::Result<()> {
            tx.prepare_cached(sql)?.execute(params)?;
            Ok(())
        };
        match self {
            Key::Server(id) => run("DELETE FROM servers WHERE id = ?1", params![id]),
            Key::Character(id) => run("DELETE FROM characters WHERE id = ?1", params![id]),
            Key::Meta(key) => run("DELETE FROM meta WHERE key = ?1", params![key]),
            Key::Room(id) => run("DELETE FROM rooms WHERE id = ?1", params![id]),
            Key::Exit { room_id, direction } => run(
                "DELETE FROM exits WHERE room_id = ?1 AND direction = ?2",
                params![room_id, direction],
            ),
            Key::RoomNote(room_id) => run(
                "DELETE FROM room_notes WHERE room_id = ?1",
                params![room_id],
            ),
            Key::RoomFeature { room_id, feature } => run(
                "DELETE FROM room_features WHERE room_id = ?1 AND feature = ?2",
                params![room_id, feature],
            ),
            Key::Sighting { room_id, mob } => run(
                "DELETE FROM sightings WHERE room_id = ?1 AND mob = ?2",
                params![room_id, mob],
            ),
            Key::MobMark { name, mark } => run(
                "DELETE FROM mob_marks WHERE name = ?1 AND mark = ?2",
                params![name, mark],
            ),
            Key::CharacterStat { character_id, key } => run(
                "DELETE FROM character_stats WHERE character_id = ?1 AND key = ?2",
                params![character_id, key],
            ),
            Key::Skill { character_id, name } => run(
                "DELETE FROM skills WHERE character_id = ?1 AND name = ?2",
                params![character_id, name],
            ),
            Key::Affect { character_id, name } => run(
                "DELETE FROM affects WHERE character_id = ?1 AND name = ?2",
                params![character_id, name],
            ),
            Key::Worn {
                character_id,
                position,
            } => run(
                "DELETE FROM worn WHERE character_id = ?1 AND position = ?2",
                params![character_id, position],
            ),
            Key::GearSet(id) => run("DELETE FROM gear_sets WHERE id = ?1", params![id]),
            Key::GearCopy(id) => run("DELETE FROM gear_copies WHERE id = ?1", params![id]),
            Key::ItemFacts(name) => run("DELETE FROM item_facts WHERE name = ?1", params![name]),
            Key::Identification(id) => {
                run("DELETE FROM identifications WHERE id = ?1", params![id])
            }
            Key::MapVersion(id) => run("DELETE FROM map_versions WHERE id = ?1", params![id]),
        }
    }
}

/// Copies the live map into a new version in SQL, without a round trip
/// through Rust.
fn save_map_version(
    tx: &Transaction<'_>,
    kind: &str,
    label: &str,
    created: Timestamp,
) -> rusqlite::Result<()> {
    tx.prepare_cached(
        "INSERT INTO map_versions (created, kind, label, room_count, updated)
         VALUES (?1, ?2, ?3, (SELECT count(*) FROM rooms), ?1)",
    )?
    .execute(params![created, kind, label])?;
    let version_id = tx.last_insert_rowid();
    for sql in [
        "INSERT INTO version_rooms SELECT ?1, id, name, area, environment, description, x, y, z,
            building, tangled, maze, updated FROM rooms",
        "INSERT INTO version_exits SELECT ?1, room_id, direction, door, doorway, to_room, updated FROM exits",
        "INSERT INTO version_room_notes SELECT ?1, room_id, text, updated FROM room_notes",
        "INSERT INTO version_room_features SELECT ?1, room_id, feature, updated FROM room_features",
        "INSERT INTO version_sightings SELECT ?1, room_id, mob, updated FROM sightings",
    ] {
        tx.prepare_cached(sql)?.execute(params![version_id])?;
    }
    Ok(())
}

/// Clears the live map and copies a version back in. A version that
/// doesn't exist fails the batch rather than leave the map empty.
fn restore_map_version(
    tx: &Transaction<'_>,
    version_id: i64,
    at: Timestamp,
) -> rusqlite::Result<()> {
    tx.prepare_cached("SELECT 1 FROM map_versions WHERE id = ?1")?
        .query_row(params![version_id], |_| Ok(()))?;
    // Deleting the rooms deletes their exits, notes, features and sightings.
    tx.prepare_cached("DELETE FROM rooms")?.execute([])?;
    for sql in [
        "INSERT INTO rooms SELECT id, name, area, environment, description, x, y, z,
            building, tangled, maze, ?2 FROM version_rooms WHERE version_id = ?1",
        "INSERT INTO exits SELECT room_id, direction, door, doorway, to_room, ?2 FROM version_exits WHERE version_id = ?1",
        "INSERT INTO room_notes SELECT room_id, text, ?2 FROM version_room_notes WHERE version_id = ?1",
        "INSERT INTO room_features SELECT room_id, feature, ?2 FROM version_room_features WHERE version_id = ?1",
    ] {
        tx.prepare_cached(sql)?.execute(params![version_id, at])?;
    }
    // A sighting's time is when the mob was seen, so it's kept as it was.
    tx.prepare_cached("INSERT INTO sightings SELECT room_id, mob, updated FROM version_sightings WHERE version_id = ?1")?
        .execute(params![version_id])?;
    Ok(())
}
