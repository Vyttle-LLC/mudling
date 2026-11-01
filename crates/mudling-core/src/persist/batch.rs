//! What callers hand the writer: a batch of row changes, applied in one
//! transaction.

use super::records::*;
use rusqlite::{OptionalExtension as _, ToSql, Transaction, params};
use serde::{Deserialize, Serialize};

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

    /// Deletes a record and leaves a [`Tombstone`], so a sync can't bring it
    /// back from another device. The time is taken here, not when the writer
    /// gets to it, so a record added again right after still counts as newer.
    pub fn delete(&mut self, key: Key) -> &mut Batch {
        self.changes.push(Change::Delete {
            key,
            at: Timestamp::now(),
        });
        self
    }

    /// Takes in another device's copy of a record, unless what's here by the
    /// time the writer runs outranks it: a newer row, or a newer delete of
    /// the record or of what it hangs off. Unlike a check made while reading,
    /// this also sees what the game wrote a moment ago.
    pub fn adopt(&mut self, record: impl Into<Record>) -> &mut Batch {
        self.changes.push(Change::Adopt(record.into()));
        self
    }

    /// Takes in another device's delete: keeps its tombstone, and deletes the
    /// row here unless the row is newer.
    pub fn bury(&mut self, tombstone: Tombstone) -> &mut Batch {
        self.changes.push(Change::Bury(tombstone));
        self
    }

    /// Forgets tombstones from before `before`.
    pub fn forget_tombstones(&mut self, before: Timestamp) -> &mut Batch {
        self.changes.push(Change::ForgetTombstones(before));
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

    /// Deletes all but the newest `keep` versions of `kind`.
    pub fn prune_map_versions(&mut self, kind: &str, keep: i64) -> &mut Batch {
        self.changes.push(Change::PruneMapVersions {
            kind: kind.to_owned(),
            keep,
        });
        self
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Adds `later`'s changes after these.
    pub(crate) fn extend(&mut self, later: Batch) {
        self.changes.extend(later.changes);
    }
}

// Records live inline in the batch's one Vec; boxing them would cost the
// caller an allocation per change.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Change {
    Upsert(Record),
    Adopt(Record),
    Delete {
        key: Key,
        at: Timestamp,
    },
    Bury(Tombstone),
    ForgetTombstones(Timestamp),
    SaveMapVersion {
        kind: String,
        label: String,
        created: Timestamp,
    },
    RestoreMapVersion {
        version_id: i64,
        at: Timestamp,
    },
    PruneMapVersions {
        kind: String,
        keep: i64,
    },
}

impl Change {
    pub(crate) fn apply(&self, tx: &Transaction<'_>) -> rusqlite::Result<()> {
        match self {
            Change::Upsert(record) => record.upsert(tx),
            Change::Adopt(record) => record.adopt(tx),
            Change::Delete { key, at } => key.delete(tx, *at),
            Change::Bury(tombstone) => bury(tx, tombstone),
            Change::ForgetTombstones(before) => {
                tx.prepare_cached("DELETE FROM tombstones WHERE deleted < ?1")?
                    .execute(params![before])?;
                Ok(())
            }
            Change::SaveMapVersion {
                kind,
                label,
                created,
            } => save_map_version(tx, kind, label, *created),
            Change::RestoreMapVersion { version_id, at } => {
                restore_map_version(tx, *version_id, *at)
            }
            Change::PruneMapVersions { kind, keep } => {
                tx.prepare_cached(
                    "DELETE FROM map_versions WHERE kind = ?1 AND id NOT IN
                         (SELECT id FROM map_versions WHERE kind = ?1
                          ORDER BY created DESC, id DESC LIMIT ?2)",
                )?
                .execute(params![kind, keep])?;
                Ok(())
            }
        }
    }
}

/// What a merge needs to know about a record type. `owners` are the records
/// this one can't outlive: deleting one of them cascades to it.
pub(crate) trait Keyed: Stored {
    const TABLE: &'static str;

    fn key(&self) -> Key;
    fn updated(&self) -> Timestamp;
    fn updated_mut(&mut self) -> &mut Timestamp;
    fn owners(&self) -> Vec<Key>;
}

macro_rules! records {
    ($($kind:ident, $table:literal, |$row:ident| $key:expr, [$($owner:expr),*];)*) => {
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

        $(impl Keyed for $kind {
            const TABLE: &'static str = $table;

            fn key(&self) -> Key {
                let $row = self;
                $key
            }

            fn updated(&self) -> Timestamp {
                self.updated
            }

            fn updated_mut(&mut self) -> &mut Timestamp {
                &mut self.updated
            }

            fn owners(&self) -> Vec<Key> {
                let $row = self;
                let _ = $row;
                vec![$($owner),*]
            }
        })*

        impl Record {
            fn upsert(&self, tx: &Transaction<'_>) -> rusqlite::Result<()> {
                match self {
                    $(Record::$kind(record) => upsert(tx, record),)*
                }
            }

            fn adopt(&self, tx: &Transaction<'_>) -> rusqlite::Result<()> {
                match self {
                    $(Record::$kind(record) => adopt(tx, record),)*
                }
            }
        }
    };
}

records!(
    Server, "servers", |row| Key::Server(row.id.clone()), [];
    Character, "characters", |row| Key::Character(row.id.clone()), [];
    Meta, "meta", |row| Key::Meta(row.key.clone()), [];
    Room, "rooms", |row| Key::Room(row.id), [];
    Exit, "exits", |row| Key::Exit {
        room_id: row.room_id,
        direction: row.direction.clone(),
    }, [Key::Room(row.room_id)];
    Link, "links", |row| Key::Link {
        room_id: row.room_id,
        direction: row.direction.clone(),
    }, [Key::Room(row.room_id), Key::Room(row.to_room)];
    Doorway, "doorways", |row| Key::Doorway {
        room_id: row.room_id,
        direction: row.direction.clone(),
    }, [Key::Room(row.room_id)];
    RoomNote, "room_notes", |row| Key::RoomNote(row.room_id), [Key::Room(row.room_id)];
    RoomFeature, "room_features", |row| Key::RoomFeature {
        room_id: row.room_id,
        feature: row.feature.clone(),
    }, [Key::Room(row.room_id)];
    Sighting, "sightings", |row| Key::Sighting {
        room_id: row.room_id,
        mob: row.mob.clone(),
    }, [Key::Room(row.room_id)];
    MobMark, "mob_marks", |row| Key::MobMark {
        name: row.name.clone(),
        mark: row.mark.clone(),
    }, [];
    CharacterStat, "character_stats", |row| Key::CharacterStat {
        character_id: row.character_id.clone(),
        key: row.key.clone(),
    }, [Key::Character(row.character_id.clone())];
    Skill, "skills", |row| Key::Skill {
        character_id: row.character_id.clone(),
        name: row.name.clone(),
    }, [Key::Character(row.character_id.clone())];
    Affect, "affects", |row| Key::Affect {
        character_id: row.character_id.clone(),
        name: row.name.clone(),
    }, [Key::Character(row.character_id.clone())];
    Worn, "worn", |row| Key::Worn {
        character_id: row.character_id.clone(),
        position: row.position,
    }, [Key::Character(row.character_id.clone())];
    GearSet, "gear_sets", |row| Key::GearSet(row.id.clone()),
        [Key::Character(row.character_id.clone())];
    GearCopy, "gear_copies", |row| Key::GearCopy(row.id.clone()), [];
    ItemFacts, "item_facts", |row| Key::ItemFacts(row.name.clone()), [];
    Identification, "identifications", |row| Key::Identification(row.id.clone()), [];
    Trigger, "triggers", |row| Key::Trigger(row.id.clone()), [];
    Alias, "aliases", |row| Key::Alias(row.id.clone()), [];
);

fn upsert<T: Stored>(tx: &Transaction<'_>, record: &T) -> rusqlite::Result<()> {
    record.upsert(&mut *tx.prepare_cached(T::UPSERT)?)?;
    Ok(())
}

/// An equal `updated` lets the file's copy in, so an edit by hand still
/// lands, and two devices that tied settle on the merge's choice.
fn adopt<T: Keyed>(tx: &Transaction<'_>, record: &T) -> rusqlite::Result<()> {
    if outranked_by_delete(tx, record)? {
        return Ok(());
    }
    let sql = format!(
        "{} WHERE excluded.updated >= {}.updated",
        T::UPSERT,
        T::TABLE
    );
    record.upsert(&mut *tx.prepare_cached(&sql)?)?;
    Ok(())
}

/// Whether a delete here rules `record` out. A newer owner may have come back
/// since it was deleted, but a child whose owner stayed gone can't be stored.
fn outranked_by_delete<T: Keyed>(tx: &Transaction<'_>, record: &T) -> rusqlite::Result<bool> {
    let updated = record.updated();
    if deleted_at(tx, &record.key())?.is_some_and(|deleted| deleted > updated) {
        return Ok(true);
    }
    for owner in record.owners() {
        let Some(deleted) = deleted_at(tx, &owner)? else {
            continue;
        };
        if deleted > updated || !owner.exists(tx)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn deleted_at(tx: &Transaction<'_>, key: &Key) -> rusqlite::Result<Option<Timestamp>> {
    tx.prepare_cached("SELECT deleted FROM tombstones WHERE key = ?1")?
        .query_row(params![Json(key)], |row| row.get(0))
        .optional()
}

fn bury(tx: &Transaction<'_>, tombstone: &Tombstone) -> rusqlite::Result<()> {
    let Tombstone { key, deleted } = tombstone;
    if !key.syncs() {
        return Ok(());
    }
    upsert(tx, tombstone)?;
    key.with_row(|row, params| -> rusqlite::Result<()> {
        let sql = format!("DELETE FROM {row} AND updated < ?{}", params.len() + 1);
        let mut params = params.to_vec();
        params.push(deleted);
        tx.prepare_cached(&sql)?.execute(&*params)?;
        Ok(())
    })?;
    bury_children(tx, key, *deleted)
}

/// A row newer than the delete survives it, but the delete still took the
/// older rows that hang off it, as it does in a merge.
fn bury_children(tx: &Transaction<'_>, key: &Key, deleted: Timestamp) -> rusqlite::Result<()> {
    let run = |table: &str, owner: &str, id: &dyn ToSql| -> rusqlite::Result<()> {
        tx.prepare_cached(&format!(
            "DELETE FROM {table} WHERE {owner} = ?1 AND updated < ?2"
        ))?
        .execute(params![id, deleted])?;
        Ok(())
    };
    match key {
        Key::Room(id) => {
            run("links", "to_room", id)?;
            for table in ROOM_CHILDREN {
                run(table, "room_id", id)?;
            }
        }
        Key::Character(id) => {
            for table in CHARACTER_CHILDREN {
                run(table, "character_id", id)?;
            }
        }
        _ => {}
    }
    Ok(())
}

const ROOM_CHILDREN: [&str; 6] = [
    "exits",
    "links",
    "doorways",
    "room_notes",
    "room_features",
    "sightings",
];
const CHARACTER_CHILDREN: [&str; 5] = ["character_stats", "skills", "affects", "worn", "gear_sets"];

/// A record's key, for [`Batch::delete`]. Deleting a room deletes its
/// exits, links, doorways, note, features and sightings, and the links
/// that led to it; deleting a character deletes its stats, skills, affects, worn items and
/// gear sets.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Key {
    Server(String),
    Character(String),
    Meta(String),
    Room(i64),
    Exit { room_id: i64, direction: String },
    Link { room_id: i64, direction: String },
    Doorway { room_id: i64, direction: String },
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
    Trigger(String),
    Alias(String),
    MapVersion(i64),
}

impl Key {
    /// Whether a delete of this key travels to other devices. A database's
    /// own server row and its map history never do.
    fn syncs(&self) -> bool {
        !matches!(self, Key::Server(_) | Key::MapVersion(_))
    }

    /// Calls `run` with the key's table and condition, and the condition's
    /// parameters.
    fn with_row<R>(&self, run: impl FnOnce(&str, &[&dyn ToSql]) -> R) -> R {
        match self {
            Key::Server(id) => run("servers WHERE id = ?1", params![id]),
            Key::Character(id) => run("characters WHERE id = ?1", params![id]),
            Key::Meta(key) => run("meta WHERE key = ?1", params![key]),
            Key::Room(id) => run("rooms WHERE id = ?1", params![id]),
            Key::Exit { room_id, direction } => run(
                "exits WHERE room_id = ?1 AND direction = ?2",
                params![room_id, direction],
            ),
            Key::Link { room_id, direction } => run(
                "links WHERE room_id = ?1 AND direction = ?2",
                params![room_id, direction],
            ),
            Key::Doorway { room_id, direction } => run(
                "doorways WHERE room_id = ?1 AND direction = ?2",
                params![room_id, direction],
            ),
            Key::RoomNote(room_id) => run("room_notes WHERE room_id = ?1", params![room_id]),
            Key::RoomFeature { room_id, feature } => run(
                "room_features WHERE room_id = ?1 AND feature = ?2",
                params![room_id, feature],
            ),
            Key::Sighting { room_id, mob } => run(
                "sightings WHERE room_id = ?1 AND mob = ?2",
                params![room_id, mob],
            ),
            Key::MobMark { name, mark } => run(
                "mob_marks WHERE name = ?1 AND mark = ?2",
                params![name, mark],
            ),
            Key::CharacterStat { character_id, key } => run(
                "character_stats WHERE character_id = ?1 AND key = ?2",
                params![character_id, key],
            ),
            Key::Skill { character_id, name } => run(
                "skills WHERE character_id = ?1 AND name = ?2",
                params![character_id, name],
            ),
            Key::Affect { character_id, name } => run(
                "affects WHERE character_id = ?1 AND name = ?2",
                params![character_id, name],
            ),
            Key::Worn {
                character_id,
                position,
            } => run(
                "worn WHERE character_id = ?1 AND position = ?2",
                params![character_id, position],
            ),
            Key::GearSet(id) => run("gear_sets WHERE id = ?1", params![id]),
            Key::GearCopy(id) => run("gear_copies WHERE id = ?1", params![id]),
            Key::ItemFacts(name) => run("item_facts WHERE name = ?1", params![name]),
            Key::Identification(id) => run("identifications WHERE id = ?1", params![id]),
            Key::Trigger(id) => run("triggers WHERE id = ?1", params![id]),
            Key::Alias(id) => run("aliases WHERE id = ?1", params![id]),
            Key::MapVersion(id) => run("map_versions WHERE id = ?1", params![id]),
        }
    }

    fn exists(&self, tx: &Transaction<'_>) -> rusqlite::Result<bool> {
        self.with_row(|row, params| {
            tx.prepare_cached(&format!("SELECT 1 FROM {row}"))?
                .exists(params)
        })
    }

    /// The tombstone stays newer than the row it replaces, however far apart
    /// two devices' clocks are.
    fn delete(&self, tx: &Transaction<'_>, at: Timestamp) -> rusqlite::Result<()> {
        let updated: Option<Timestamp> = self.with_row(|row, params| {
            tx.prepare_cached(&format!("DELETE FROM {row} RETURNING updated"))?
                .query_row(params, |row| row.get(0))
                .optional()
        })?;
        match updated {
            Some(updated) if self.syncs() => upsert(
                tx,
                &Tombstone {
                    key: self.clone(),
                    deleted: at.max(Timestamp(updated.0 + 1)),
                },
            ),
            _ => Ok(()),
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
        "INSERT INTO map_versions (created, kind, label, room_count, areas, updated)
         VALUES (?1, ?2, ?3, (SELECT count(*) FROM rooms),
             (SELECT json_group_object(area, total) FROM (SELECT area, count(*) AS total FROM rooms GROUP BY area)),
             ?1)",
    )?
    .execute(params![created, kind, label])?;
    let version_id = tx.last_insert_rowid();
    for table in MAP_TABLES {
        let MapTable {
            live,
            version,
            columns,
            ..
        } = table;
        tx.prepare_cached(&format!(
            "INSERT INTO {version} (version_id, {columns}, updated) SELECT ?1, {columns}, updated FROM {live}"
        ))?
        .execute(params![version_id])?;
    }
    Ok(())
}

/// Clears the live map and copies a version back in. A version that
/// doesn't exist fails the batch rather than leave the map empty. Rows the
/// version doesn't hold are deletes like any other, so they get tombstones.
/// A restore is an edit at `at` only of what it changes: a row the version
/// holds the same as the live map keeps its time, so restoring never beats
/// another device's newer edit of a room it left alone. A row that differs,
/// or comes back, is stamped `at` and wins as any later edit does.
/// A sighting brought back keeps its time unless a tombstone for it or its
/// room is newer, which another device may hold too: bringing it back is a
/// change of its own, so it takes that tombstone's time and outranks it.
fn restore_map_version(
    tx: &Transaction<'_>,
    version_id: i64,
    at: Timestamp,
) -> rusqlite::Result<()> {
    tx.prepare_cached("SELECT 1 FROM map_versions WHERE id = ?1")?
        .query_row(params![version_id], |_| Ok(()))?;
    for table in MAP_TABLES {
        let MapTable { live, columns, .. } = table;
        tx.execute_batch(&format!(
            "CREATE TEMP TABLE prior_{live} AS SELECT {columns}, updated FROM {live}"
        ))?;
    }
    for table in MAP_TABLES {
        let MapTable {
            live,
            version,
            order_by,
            key_json,
            ..
        } = table;
        tx.prepare_cached(&format!(
            "INSERT INTO tombstones (key, deleted)
             SELECT {key_json}, max(?2, updated + 1) FROM {live}
             WHERE ({order_by}) NOT IN (SELECT {order_by} FROM {version} WHERE version_id = ?1)
             ON CONFLICT (key) DO UPDATE SET deleted = max(deleted, excluded.deleted)"
        ))?
        .execute(params![version_id, at])?;
    }
    // Deleting the rooms deletes every other map row with them.
    tx.prepare_cached("DELETE FROM rooms")?.execute([])?;
    for table in MAP_TABLES {
        let MapTable {
            live,
            version,
            columns,
            order_by,
            key_json,
            restore_stamps_updated,
        } = table;
        if restore_stamps_updated {
            tx.prepare_cached(&format!(
                "INSERT INTO {live} ({columns}, updated)
                 SELECT {columns}, updated FROM prior_{live}
                 WHERE ({order_by}) IN (SELECT {order_by} FROM
                     (SELECT {columns} FROM {version} WHERE version_id = ?1
                      INTERSECT SELECT {columns} FROM prior_{live}))"
            ))?
            .execute(params![version_id])?;
            tx.prepare_cached(&format!(
                "INSERT INTO {live} ({columns}, updated)
                 SELECT {columns}, ?2 FROM {version} WHERE version_id = ?1
                 AND ({order_by}) NOT IN (SELECT {order_by} FROM {live})"
            ))?
            .execute(params![version_id, at])?;
        } else {
            let deleted = |key: &str| {
                format!("coalesce((SELECT deleted FROM tombstones WHERE key = {key}), 0)")
            };
            let (own, room) = (deleted(key_json), deleted("json_object('room', room_id)"));
            tx.prepare_cached(&format!(
                "INSERT INTO {live} ({columns}, updated)
                 SELECT {columns}, max(updated, {own}, {room}) FROM {version} WHERE version_id = ?1"
            ))?
            .execute(params![version_id])?;
        }
    }
    for table in MAP_TABLES {
        tx.execute_batch(&format!("DROP TABLE prior_{}", table.live))?;
    }
    Ok(())
}
