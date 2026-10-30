//! The schema, as numbered migrations. `PRAGMA user_version` holds the last
//! one applied; opening a database applies the rest in one transaction.
//!
//! Every record carries `updated` (milliseconds since the Unix epoch), so a
//! later sync can merge two copies record by record, newest first. Columns
//! hold no game's wording: directions, features, marks, slots and stat keys
//! are data.
//!
//! Foreign keys are deferred to commit, so a batch may write a child before
//! its parent; a batch that leaves one dangling fails as a whole.

use rusqlite::{Connection, TransactionBehavior};

/// Migration `i` takes the schema from version `i` to version `i + 1`.
/// Append new ones; never edit one that has shipped.
const MIGRATIONS: &[&str] = &[V1];

pub const LATEST_VERSION: u32 = MIGRATIONS.len() as u32;

/// Brings the schema up to [`LATEST_VERSION`], returning the version found.
/// The version is read under the write lock, so two connections opening one
/// file can't both apply the same migration.
pub fn migrate(connection: &mut Connection) -> rusqlite::Result<u32> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let found: u32 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    for sql in MIGRATIONS.iter().skip(found as usize) {
        tx.execute_batch(sql)?;
    }
    if found < LATEST_VERSION {
        tx.pragma_update(None, "user_version", LATEST_VERSION)?;
    }
    tx.commit()?;
    Ok(found)
}

const V1: &str = r#"
CREATE TABLE servers (
    id       TEXT PRIMARY KEY,
    name     TEXT NOT NULL,
    host     TEXT NOT NULL,
    port     INTEGER NOT NULL,
    settings TEXT NOT NULL,
    updated  INTEGER NOT NULL
);

CREATE TABLE characters (
    id        TEXT PRIMARY KEY,
    server_id TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    name      TEXT NOT NULL,
    updated   INTEGER NOT NULL
);

CREATE TABLE meta (
    key     TEXT PRIMARY KEY,
    value   TEXT NOT NULL,
    updated INTEGER NOT NULL
);

CREATE TABLE rooms (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    area        TEXT NOT NULL,
    environment TEXT NOT NULL,
    description TEXT NOT NULL,
    x           INTEGER NOT NULL,
    y           INTEGER NOT NULL,
    z           INTEGER NOT NULL,
    building    INTEGER,
    tangled     INTEGER NOT NULL,
    maze        INTEGER NOT NULL,
    updated     INTEGER NOT NULL
);

-- A room's exits as the game lists them now, links explored, and doorways
-- seen are kept apart: a link or a doorway outlives the room's listing.
CREATE TABLE exits (
    room_id   INTEGER NOT NULL REFERENCES rooms(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    direction TEXT NOT NULL,
    state     TEXT NOT NULL,
    updated   INTEGER NOT NULL,
    PRIMARY KEY (room_id, direction)
) WITHOUT ROWID;

CREATE TABLE links (
    room_id   INTEGER NOT NULL REFERENCES rooms(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    direction TEXT NOT NULL,
    to_room   INTEGER NOT NULL REFERENCES rooms(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    updated   INTEGER NOT NULL,
    PRIMARY KEY (room_id, direction)
) WITHOUT ROWID;
CREATE INDEX links_to_room ON links(to_room);

CREATE TABLE doorways (
    room_id   INTEGER NOT NULL REFERENCES rooms(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    direction TEXT NOT NULL,
    updated   INTEGER NOT NULL,
    PRIMARY KEY (room_id, direction)
) WITHOUT ROWID;

CREATE TABLE room_notes (
    room_id INTEGER PRIMARY KEY REFERENCES rooms(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    text    TEXT NOT NULL,
    updated INTEGER NOT NULL
);

CREATE TABLE room_features (
    room_id INTEGER NOT NULL REFERENCES rooms(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    feature TEXT NOT NULL,
    updated INTEGER NOT NULL,
    PRIMARY KEY (room_id, feature)
) WITHOUT ROWID;

CREATE TABLE sightings (
    room_id INTEGER NOT NULL REFERENCES rooms(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    mob     TEXT NOT NULL,
    updated INTEGER NOT NULL,
    PRIMARY KEY (room_id, mob)
) WITHOUT ROWID;

CREATE TABLE mob_marks (
    name    TEXT NOT NULL,
    mark    TEXT NOT NULL,
    updated INTEGER NOT NULL,
    PRIMARY KEY (name, mark)
) WITHOUT ROWID;

CREATE TABLE character_stats (
    character_id TEXT NOT NULL REFERENCES characters(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    key          TEXT NOT NULL,
    value        TEXT NOT NULL,
    updated      INTEGER NOT NULL,
    PRIMARY KEY (character_id, key)
) WITHOUT ROWID;

CREATE TABLE skills (
    character_id TEXT NOT NULL REFERENCES characters(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    name         TEXT NOT NULL,
    level        INTEGER,
    learned      INTEGER,
    cost         INTEGER,
    updated      INTEGER NOT NULL,
    PRIMARY KEY (character_id, name)
) WITHOUT ROWID;

CREATE TABLE affects (
    character_id TEXT NOT NULL REFERENCES characters(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    name         TEXT NOT NULL,
    detail       TEXT,
    updated      INTEGER NOT NULL,
    PRIMARY KEY (character_id, name)
) WITHOUT ROWID;

CREATE TABLE worn (
    character_id TEXT NOT NULL REFERENCES characters(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    position     INTEGER NOT NULL,
    slot         TEXT NOT NULL,
    item         TEXT NOT NULL,
    copy_id      TEXT,
    updated      INTEGER NOT NULL,
    PRIMARY KEY (character_id, position)
) WITHOUT ROWID;

CREATE TABLE gear_sets (
    id           TEXT PRIMARY KEY,
    character_id TEXT NOT NULL REFERENCES characters(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
    name         TEXT NOT NULL,
    container    TEXT,
    pieces       TEXT NOT NULL,
    updated      INTEGER NOT NULL
);
CREATE INDEX gear_sets_character ON gear_sets(character_id);

CREATE TABLE gear_copies (
    id       TEXT PRIMARY KEY,
    name     TEXT NOT NULL,
    measured TEXT,
    updated  INTEGER NOT NULL
);

CREATE TABLE item_facts (
    name     TEXT PRIMARY KEY,
    keywords TEXT,
    slot     TEXT,
    measured TEXT,
    updated  INTEGER NOT NULL
);

CREATE TABLE identifications (
    id        TEXT PRIMARY KEY,
    item_name TEXT NOT NULL,
    copy_id   TEXT,
    keywords  TEXT NOT NULL,
    item_type TEXT NOT NULL,
    flags     TEXT NOT NULL,
    level     INTEGER,
    quality   TEXT,
    base      TEXT NOT NULL,
    added     TEXT NOT NULL,
    details   TEXT NOT NULL,
    updated   INTEGER NOT NULL
);
CREATE INDEX identifications_item ON identifications(item_name);
CREATE INDEX identifications_copy ON identifications(copy_id);

-- Map history. Listing reads only this header table; each version's rooms
-- live in the version_* tables below, copied from the live map in SQL.
CREATE TABLE map_versions (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    created    INTEGER NOT NULL,
    kind       TEXT NOT NULL,
    label      TEXT NOT NULL,
    room_count INTEGER NOT NULL,
    -- Rooms per area, as JSON: {"area": count}.
    areas      TEXT NOT NULL,
    updated    INTEGER NOT NULL
);
CREATE INDEX map_versions_created ON map_versions(created);

CREATE TABLE version_rooms (
    version_id  INTEGER NOT NULL REFERENCES map_versions(id) ON DELETE CASCADE,
    id          INTEGER NOT NULL,
    name        TEXT NOT NULL,
    area        TEXT NOT NULL,
    environment TEXT NOT NULL,
    description TEXT NOT NULL,
    x           INTEGER NOT NULL,
    y           INTEGER NOT NULL,
    z           INTEGER NOT NULL,
    building    INTEGER,
    tangled     INTEGER NOT NULL,
    maze        INTEGER NOT NULL,
    updated     INTEGER NOT NULL,
    PRIMARY KEY (version_id, id)
) WITHOUT ROWID;

CREATE TABLE version_exits (
    version_id INTEGER NOT NULL REFERENCES map_versions(id) ON DELETE CASCADE,
    room_id    INTEGER NOT NULL,
    direction  TEXT NOT NULL,
    state      TEXT NOT NULL,
    updated    INTEGER NOT NULL,
    PRIMARY KEY (version_id, room_id, direction)
) WITHOUT ROWID;

CREATE TABLE version_links (
    version_id INTEGER NOT NULL REFERENCES map_versions(id) ON DELETE CASCADE,
    room_id    INTEGER NOT NULL,
    direction  TEXT NOT NULL,
    to_room    INTEGER NOT NULL,
    updated    INTEGER NOT NULL,
    PRIMARY KEY (version_id, room_id, direction)
) WITHOUT ROWID;

CREATE TABLE version_doorways (
    version_id INTEGER NOT NULL REFERENCES map_versions(id) ON DELETE CASCADE,
    room_id    INTEGER NOT NULL,
    direction  TEXT NOT NULL,
    updated    INTEGER NOT NULL,
    PRIMARY KEY (version_id, room_id, direction)
) WITHOUT ROWID;

CREATE TABLE version_room_notes (
    version_id INTEGER NOT NULL REFERENCES map_versions(id) ON DELETE CASCADE,
    room_id    INTEGER NOT NULL,
    text       TEXT NOT NULL,
    updated    INTEGER NOT NULL,
    PRIMARY KEY (version_id, room_id)
) WITHOUT ROWID;

CREATE TABLE version_room_features (
    version_id INTEGER NOT NULL REFERENCES map_versions(id) ON DELETE CASCADE,
    room_id    INTEGER NOT NULL,
    feature    TEXT NOT NULL,
    updated    INTEGER NOT NULL,
    PRIMARY KEY (version_id, room_id, feature)
) WITHOUT ROWID;

CREATE TABLE version_sightings (
    version_id INTEGER NOT NULL REFERENCES map_versions(id) ON DELETE CASCADE,
    room_id    INTEGER NOT NULL,
    mob        TEXT NOT NULL,
    updated    INTEGER NOT NULL,
    PRIMARY KEY (version_id, room_id, mob)
) WITHOUT ROWID;
"#;
