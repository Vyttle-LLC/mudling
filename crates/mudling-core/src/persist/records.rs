//! The records MUDling keeps for a server, one type per table. Each knows
//! its own upsert and select, so the writer and reader stay generic.
//!
//! Values a game decides (directions, door states, features, mob marks,
//! slots, stat keys) are plain strings: the schema has no game's words.

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, ValueRef};
use rusqlite::{Row, Statement, ToSql, params};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// When a record last changed, in milliseconds since the Unix epoch. A
/// later sync merges two copies of a record by keeping the newer one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(pub i64);

impl Timestamp {
    pub fn now() -> Timestamp {
        let since_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Timestamp(since_epoch.as_millis() as i64)
    }
}

impl ToSql for Timestamp {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        self.0.to_sql()
    }
}

impl FromSql for Timestamp {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        i64::column_result(value).map(Timestamp)
    }
}

/// Stat changes by stat key: `{"dex": 1, "ac": -4}`.
pub type StatMap = BTreeMap<String, i64>;

/// A record type stored as rows of one table.
pub(crate) trait Stored: Sized {
    /// Inserts the record, or updates the row with its key in place. (Never
    /// `INSERT OR REPLACE`: that deletes the old row and its children.)
    const UPSERT: &'static str;
    /// Every row of the table, in key order.
    const SELECT: &'static str;

    fn upsert(&self, statement: &mut Statement<'_>) -> rusqlite::Result<usize>;
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self>;
}

/// The server this database belongs to.
#[derive(Clone, Debug, PartialEq)]
pub struct Server {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    /// Connection settings as JSON, shaped by whoever owns them.
    pub settings: String,
    pub updated: Timestamp,
}

impl Stored for Server {
    const UPSERT: &'static str = "INSERT INTO servers (id, name, host, port, settings, updated)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        ON CONFLICT (id) DO UPDATE SET name = excluded.name, host = excluded.host, port = excluded.port,
            settings = excluded.settings, updated = excluded.updated";
    const SELECT: &'static str =
        "SELECT id, name, host, port, settings, updated FROM servers ORDER BY id";

    fn upsert(&self, statement: &mut Statement<'_>) -> rusqlite::Result<usize> {
        statement.execute(params![
            self.id,
            self.name,
            self.host,
            self.port,
            self.settings,
            self.updated
        ])
    }

    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Server {
            id: row.get(0)?,
            name: row.get(1)?,
            host: row.get(2)?,
            port: row.get(3)?,
            settings: row.get(4)?,
            updated: row.get(5)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Character {
    pub id: String,
    pub server_id: String,
    pub name: String,
    pub updated: Timestamp,
}

impl Stored for Character {
    const UPSERT: &'static str = "INSERT INTO characters (id, server_id, name, updated) VALUES (?1, ?2, ?3, ?4)
        ON CONFLICT (id) DO UPDATE SET server_id = excluded.server_id, name = excluded.name, updated = excluded.updated";
    const SELECT: &'static str = "SELECT id, server_id, name, updated FROM characters ORDER BY id";

    fn upsert(&self, statement: &mut Statement<'_>) -> rusqlite::Result<usize> {
        statement.execute(params![self.id, self.server_id, self.name, self.updated])
    }

    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Character {
            id: row.get(0)?,
            server_id: row.get(1)?,
            name: row.get(2)?,
            updated: row.get(3)?,
        })
    }
}

/// A small per-server value, such as the room the player was last in.
#[derive(Clone, Debug, PartialEq)]
pub struct Meta {
    pub key: String,
    pub value: String,
    pub updated: Timestamp,
}

impl Stored for Meta {
    const UPSERT: &'static str = "INSERT INTO meta (key, value, updated) VALUES (?1, ?2, ?3)
        ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated = excluded.updated";
    const SELECT: &'static str = "SELECT key, value, updated FROM meta ORDER BY key";

    fn upsert(&self, statement: &mut Statement<'_>) -> rusqlite::Result<usize> {
        statement.execute(params![self.key, self.value, self.updated])
    }

    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Meta {
            key: row.get(0)?,
            value: row.get(1)?,
            updated: row.get(2)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Room {
    pub id: i64,
    pub name: String,
    /// The game's area (GMCP's zone), as it sends it.
    pub area: String,
    pub environment: String,
    /// The first line of the room's description, which tells apart rooms
    /// that share a name.
    pub description: String,
    /// Map position: x grows east, y north, z up.
    pub x: i64,
    pub y: i64,
    pub z: i64,
    /// The building this room is inside, by its first room's id.
    pub building: Option<i64>,
    /// The map's geometry contradicts itself here, so the position is a guess.
    pub tangled: bool,
    /// One square standing for a whole maze of look-alike rooms.
    pub maze: bool,
    pub updated: Timestamp,
}

impl Stored for Room {
    const UPSERT: &'static str = "INSERT INTO rooms
            (id, name, area, environment, description, x, y, z, building, tangled, maze, updated)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
        ON CONFLICT (id) DO UPDATE SET name = excluded.name, area = excluded.area,
            environment = excluded.environment, description = excluded.description,
            x = excluded.x, y = excluded.y, z = excluded.z, building = excluded.building,
            tangled = excluded.tangled, maze = excluded.maze, updated = excluded.updated";
    const SELECT: &'static str =
        "SELECT id, name, area, environment, description, x, y, z, building, tangled, maze, updated
        FROM rooms ORDER BY id";

    fn upsert(&self, statement: &mut Statement<'_>) -> rusqlite::Result<usize> {
        statement.execute(params![
            self.id,
            self.name,
            self.area,
            self.environment,
            self.description,
            self.x,
            self.y,
            self.z,
            self.building,
            self.tangled,
            self.maze,
            self.updated
        ])
    }

    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Room {
            id: row.get(0)?,
            name: row.get(1)?,
            area: row.get(2)?,
            environment: row.get(3)?,
            description: row.get(4)?,
            x: row.get(5)?,
            y: row.get(6)?,
            z: row.get(7)?,
            building: row.get(8)?,
            tangled: row.get(9)?,
            maze: row.get(10)?,
            updated: row.get(11)?,
        })
    }
}

/// An exit the room lists now. When the room stops listing it, the row goes,
/// but a [`Link`] or [`Doorway`] that way stays.
#[derive(Clone, Debug, PartialEq)]
pub struct Exit {
    pub room_id: i64,
    pub direction: String,
    /// The exit's state as the game reports it (a door open or shut).
    pub state: String,
    pub updated: Timestamp,
}

impl Stored for Exit {
    const UPSERT: &'static str = "INSERT INTO exits (room_id, direction, state, updated)
        VALUES (?1, ?2, ?3, ?4)
        ON CONFLICT (room_id, direction) DO UPDATE SET state = excluded.state, updated = excluded.updated";
    const SELECT: &'static str =
        "SELECT room_id, direction, state, updated FROM exits ORDER BY room_id, direction";

    fn upsert(&self, statement: &mut Statement<'_>) -> rusqlite::Result<usize> {
        statement.execute(params![
            self.room_id,
            self.direction,
            self.state,
            self.updated
        ])
    }

    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Exit {
            room_id: row.get(0)?,
            direction: row.get(1)?,
            state: row.get(2)?,
            updated: row.get(3)?,
        })
    }
}

/// An explored connection: going `direction` from the room led to `to_room`.
/// Deleting either room deletes the link.
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub room_id: i64,
    pub direction: String,
    pub to_room: i64,
    pub updated: Timestamp,
}

impl Stored for Link {
    const UPSERT: &'static str = "INSERT INTO links (room_id, direction, to_room, updated)
        VALUES (?1, ?2, ?3, ?4)
        ON CONFLICT (room_id, direction) DO UPDATE SET to_room = excluded.to_room, updated = excluded.updated";
    const SELECT: &'static str =
        "SELECT room_id, direction, to_room, updated FROM links ORDER BY room_id, direction";

    fn upsert(&self, statement: &mut Statement<'_>) -> rusqlite::Result<usize> {
        statement.execute(params![
            self.room_id,
            self.direction,
            self.to_room,
            self.updated
        ])
    }

    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Link {
            room_id: row.get(0)?,
            direction: row.get(1)?,
            to_room: row.get(2)?,
            updated: row.get(3)?,
        })
    }
}

/// A direction that has shown a door at some point, open or shut.
#[derive(Clone, Debug, PartialEq)]
pub struct Doorway {
    pub room_id: i64,
    pub direction: String,
    pub updated: Timestamp,
}

impl Stored for Doorway {
    const UPSERT: &'static str =
        "INSERT INTO doorways (room_id, direction, updated) VALUES (?1, ?2, ?3)
        ON CONFLICT (room_id, direction) DO UPDATE SET updated = excluded.updated";
    const SELECT: &'static str =
        "SELECT room_id, direction, updated FROM doorways ORDER BY room_id, direction";

    fn upsert(&self, statement: &mut Statement<'_>) -> rusqlite::Result<usize> {
        statement.execute(params![self.room_id, self.direction, self.updated])
    }

    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Doorway {
            room_id: row.get(0)?,
            direction: row.get(1)?,
            updated: row.get(2)?,
        })
    }
}

/// The player's own notes on a room.
#[derive(Clone, Debug, PartialEq)]
pub struct RoomNote {
    pub room_id: i64,
    pub text: String,
    pub updated: Timestamp,
}

impl Stored for RoomNote {
    const UPSERT: &'static str =
        "INSERT INTO room_notes (room_id, text, updated) VALUES (?1, ?2, ?3)
        ON CONFLICT (room_id) DO UPDATE SET text = excluded.text, updated = excluded.updated";
    const SELECT: &'static str = "SELECT room_id, text, updated FROM room_notes ORDER BY room_id";

    fn upsert(&self, statement: &mut Statement<'_>) -> rusqlite::Result<usize> {
        statement.execute(params![self.room_id, self.text, self.updated])
    }

    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(RoomNote {
            room_id: row.get(0)?,
            text: row.get(1)?,
            updated: row.get(2)?,
        })
    }
}

/// Something worth remembering about a room (water, a shop, danger).
#[derive(Clone, Debug, PartialEq)]
pub struct RoomFeature {
    pub room_id: i64,
    pub feature: String,
    pub updated: Timestamp,
}

impl Stored for RoomFeature {
    const UPSERT: &'static str =
        "INSERT INTO room_features (room_id, feature, updated) VALUES (?1, ?2, ?3)
        ON CONFLICT (room_id, feature) DO UPDATE SET updated = excluded.updated";
    const SELECT: &'static str =
        "SELECT room_id, feature, updated FROM room_features ORDER BY room_id, feature";

    fn upsert(&self, statement: &mut Statement<'_>) -> rusqlite::Result<usize> {
        statement.execute(params![self.room_id, self.feature, self.updated])
    }

    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(RoomFeature {
            room_id: row.get(0)?,
            feature: row.get(1)?,
            updated: row.get(2)?,
        })
    }
}

/// A mob seen in a room; `updated` is when it was last seen.
#[derive(Clone, Debug, PartialEq)]
pub struct Sighting {
    pub room_id: i64,
    pub mob: String,
    pub updated: Timestamp,
}

impl Stored for Sighting {
    const UPSERT: &'static str = "INSERT INTO sightings (room_id, mob, updated) VALUES (?1, ?2, ?3)
        ON CONFLICT (room_id, mob) DO UPDATE SET updated = excluded.updated";
    const SELECT: &'static str =
        "SELECT room_id, mob, updated FROM sightings ORDER BY room_id, mob";

    fn upsert(&self, statement: &mut Statement<'_>) -> rusqlite::Result<usize> {
        statement.execute(params![self.room_id, self.mob, self.updated])
    }

    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Sighting {
            room_id: row.get(0)?,
            mob: row.get(1)?,
            updated: row.get(2)?,
        })
    }
}

/// A mark on a mob, or a kind of mob, by name: learned (aggressive) or set
/// by the player (drops gear, matters).
#[derive(Clone, Debug, PartialEq)]
pub struct MobMark {
    pub name: String,
    pub mark: String,
    pub updated: Timestamp,
}

impl Stored for MobMark {
    const UPSERT: &'static str = "INSERT INTO mob_marks (name, mark, updated) VALUES (?1, ?2, ?3)
        ON CONFLICT (name, mark) DO UPDATE SET updated = excluded.updated";
    const SELECT: &'static str = "SELECT name, mark, updated FROM mob_marks ORDER BY name, mark";

    fn upsert(&self, statement: &mut Statement<'_>) -> rusqlite::Result<usize> {
        statement.execute(params![self.name, self.mark, self.updated])
    }

    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(MobMark {
            name: row.get(0)?,
            mark: row.get(1)?,
            updated: row.get(2)?,
        })
    }
}

/// One stat or score fact about a character, by key ("level", "str").
#[derive(Clone, Debug, PartialEq)]
pub struct CharacterStat {
    pub character_id: String,
    pub key: String,
    pub value: String,
    pub updated: Timestamp,
}

impl Stored for CharacterStat {
    const UPSERT: &'static str = "INSERT INTO character_stats (character_id, key, value, updated) VALUES (?1, ?2, ?3, ?4)
        ON CONFLICT (character_id, key) DO UPDATE SET value = excluded.value, updated = excluded.updated";
    const SELECT: &'static str =
        "SELECT character_id, key, value, updated FROM character_stats ORDER BY character_id, key";

    fn upsert(&self, statement: &mut Statement<'_>) -> rusqlite::Result<usize> {
        statement.execute(params![
            self.character_id,
            self.key,
            self.value,
            self.updated
        ])
    }

    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(CharacterStat {
            character_id: row.get(0)?,
            key: row.get(1)?,
            value: row.get(2)?,
            updated: row.get(3)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Skill {
    pub character_id: String,
    pub name: String,
    /// The level the character can learn it at.
    pub level: Option<i64>,
    /// How well it's learned, 0–100.
    pub learned: Option<i64>,
    /// What a use costs (mana for a spell).
    pub cost: Option<i64>,
    pub updated: Timestamp,
}

impl Stored for Skill {
    const UPSERT: &'static str = "INSERT INTO skills (character_id, name, level, learned, cost, updated)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        ON CONFLICT (character_id, name) DO UPDATE SET level = excluded.level, learned = excluded.learned,
            cost = excluded.cost, updated = excluded.updated";
    const SELECT: &'static str = "SELECT character_id, name, level, learned, cost, updated FROM skills ORDER BY character_id, name";

    fn upsert(&self, statement: &mut Statement<'_>) -> rusqlite::Result<usize> {
        statement.execute(params![
            self.character_id,
            self.name,
            self.level,
            self.learned,
            self.cost,
            self.updated
        ])
    }

    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Skill {
            character_id: row.get(0)?,
            name: row.get(1)?,
            level: row.get(2)?,
            learned: row.get(3)?,
            cost: row.get(4)?,
            updated: row.get(5)?,
        })
    }
}

/// A spell or skill affecting the character now.
#[derive(Clone, Debug, PartialEq)]
pub struct Affect {
    pub character_id: String,
    pub name: String,
    pub detail: Option<String>,
    pub updated: Timestamp,
}

impl Stored for Affect {
    const UPSERT: &'static str = "INSERT INTO affects (character_id, name, detail, updated) VALUES (?1, ?2, ?3, ?4)
        ON CONFLICT (character_id, name) DO UPDATE SET detail = excluded.detail, updated = excluded.updated";
    const SELECT: &'static str =
        "SELECT character_id, name, detail, updated FROM affects ORDER BY character_id, name";

    fn upsert(&self, statement: &mut Statement<'_>) -> rusqlite::Result<usize> {
        statement.execute(params![
            self.character_id,
            self.name,
            self.detail,
            self.updated
        ])
    }

    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Affect {
            character_id: row.get(0)?,
            name: row.get(1)?,
            detail: row.get(2)?,
            updated: row.get(3)?,
        })
    }
}

/// An item the character wears, at its place in the game's slot order.
#[derive(Clone, Debug, PartialEq)]
pub struct Worn {
    pub character_id: String,
    pub position: i64,
    pub slot: String,
    pub item: String,
    /// The game's id for the copy (GMCP), when known.
    pub copy_id: Option<String>,
    pub updated: Timestamp,
}

impl Stored for Worn {
    const UPSERT: &'static str = "INSERT INTO worn (character_id, position, slot, item, copy_id, updated)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        ON CONFLICT (character_id, position) DO UPDATE SET slot = excluded.slot, item = excluded.item,
            copy_id = excluded.copy_id, updated = excluded.updated";
    const SELECT: &'static str = "SELECT character_id, position, slot, item, copy_id, updated FROM worn ORDER BY character_id, position";

    fn upsert(&self, statement: &mut Statement<'_>) -> rusqlite::Result<usize> {
        statement.execute(params![
            self.character_id,
            self.position,
            self.slot,
            self.item,
            self.copy_id,
            self.updated
        ])
    }

    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Worn {
            character_id: row.get(0)?,
            position: row.get(1)?,
            slot: row.get(2)?,
            item: row.get(3)?,
            copy_id: row.get(4)?,
            updated: row.get(5)?,
        })
    }
}

/// A named set of gear to change into. It's one record: its pieces change
/// together.
#[derive(Clone, Debug, PartialEq)]
pub struct GearSet {
    pub id: String,
    pub character_id: String,
    pub name: String,
    /// Where whatever comes off goes; `None` leaves it carried.
    pub container: Option<String>,
    pub pieces: Vec<GearPiece>,
    pub updated: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GearPiece {
    pub item: String,
    pub slot: Option<String>,
    /// The exact copy chosen, by the game's id for it.
    pub copy_id: Option<String>,
}

impl Stored for GearSet {
    const UPSERT: &'static str =
        "INSERT INTO gear_sets (id, character_id, name, container, pieces, updated)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        ON CONFLICT (id) DO UPDATE SET character_id = excluded.character_id, name = excluded.name,
            container = excluded.container, pieces = excluded.pieces, updated = excluded.updated";
    const SELECT: &'static str =
        "SELECT id, character_id, name, container, pieces, updated FROM gear_sets ORDER BY id";

    fn upsert(&self, statement: &mut Statement<'_>) -> rusqlite::Result<usize> {
        statement.execute(params![
            self.id,
            self.character_id,
            self.name,
            self.container,
            Json(&self.pieces),
            self.updated
        ])
    }

    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(GearSet {
            id: row.get(0)?,
            character_id: row.get(1)?,
            name: row.get(2)?,
            container: row.get(3)?,
            pieces: row.get::<_, Json<_>>(4)?.0,
            updated: row.get(5)?,
        })
    }
}

/// One copy of an item, by the game's id for it (GMCP).
#[derive(Clone, Debug, PartialEq)]
pub struct GearCopy {
    pub id: String,
    pub name: String,
    /// What it changed when it went on or came off alone.
    pub measured: Option<StatMap>,
    pub updated: Timestamp,
}

impl Stored for GearCopy {
    const UPSERT: &'static str = "INSERT INTO gear_copies (id, name, measured, updated) VALUES (?1, ?2, ?3, ?4)
        ON CONFLICT (id) DO UPDATE SET name = excluded.name, measured = excluded.measured, updated = excluded.updated";
    const SELECT: &'static str = "SELECT id, name, measured, updated FROM gear_copies ORDER BY id";

    fn upsert(&self, statement: &mut Statement<'_>) -> rusqlite::Result<usize> {
        statement.execute(params![
            self.id,
            self.name,
            self.measured.as_ref().map(Json),
            self.updated
        ])
    }

    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(GearCopy {
            id: row.get(0)?,
            name: row.get(1)?,
            measured: row.get::<_, Option<Json<_>>>(2)?.map(|json| json.0),
            updated: row.get(3)?,
        })
    }
}

/// What every copy of an item shares, by its plain name.
#[derive(Clone, Debug, PartialEq)]
pub struct ItemFacts {
    pub name: String,
    /// The words commands use for it.
    pub keywords: Option<String>,
    pub slot: Option<String>,
    /// What it changed when no copy's id was known.
    pub measured: Option<StatMap>,
    pub updated: Timestamp,
}

impl Stored for ItemFacts {
    const UPSERT: &'static str = "INSERT INTO item_facts (name, keywords, slot, measured, updated)
        VALUES (?1, ?2, ?3, ?4, ?5)
        ON CONFLICT (name) DO UPDATE SET keywords = excluded.keywords, slot = excluded.slot,
            measured = excluded.measured, updated = excluded.updated";
    const SELECT: &'static str =
        "SELECT name, keywords, slot, measured, updated FROM item_facts ORDER BY name";

    fn upsert(&self, statement: &mut Statement<'_>) -> rusqlite::Result<usize> {
        statement.execute(params![
            self.name,
            self.keywords,
            self.slot,
            self.measured.as_ref().map(Json),
            self.updated
        ])
    }

    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(ItemFacts {
            name: row.get(0)?,
            keywords: row.get(1)?,
            slot: row.get(2)?,
            measured: row.get::<_, Option<Json<_>>>(3)?.map(|json| json.0),
            updated: row.get(4)?,
        })
    }
}

/// One identify of an item: the history an estimate is built from. `base`
/// is what every copy does; `added` is what was added to this copy (an
/// enchant), by what added it, so estimates can leave it out.
#[derive(Clone, Debug, PartialEq)]
pub struct Identification {
    pub id: String,
    /// The item's plain name.
    pub item_name: String,
    /// The copy identified, when its id was known.
    pub copy_id: Option<String>,
    pub keywords: String,
    pub item_type: String,
    pub flags: Vec<String>,
    pub level: Option<i64>,
    pub quality: Option<String>,
    pub base: StatMap,
    pub added: BTreeMap<String, StatMap>,
    /// Everything else the game said: weight, value, damage.
    pub details: Vec<String>,
    pub updated: Timestamp,
}

impl Stored for Identification {
    const UPSERT: &'static str = "INSERT INTO identifications
            (id, item_name, copy_id, keywords, item_type, flags, level, quality, base, added, details, updated)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
        ON CONFLICT (id) DO UPDATE SET item_name = excluded.item_name, copy_id = excluded.copy_id,
            keywords = excluded.keywords, item_type = excluded.item_type, flags = excluded.flags,
            level = excluded.level, quality = excluded.quality, base = excluded.base,
            added = excluded.added, details = excluded.details, updated = excluded.updated";
    const SELECT: &'static str =
        "SELECT id, item_name, copy_id, keywords, item_type, flags, level, quality,
            base, added, details, updated
        FROM identifications ORDER BY id";

    fn upsert(&self, statement: &mut Statement<'_>) -> rusqlite::Result<usize> {
        statement.execute(params![
            self.id,
            self.item_name,
            self.copy_id,
            self.keywords,
            self.item_type,
            Json(&self.flags),
            self.level,
            self.quality,
            Json(&self.base),
            Json(&self.added),
            Json(&self.details),
            self.updated
        ])
    }

    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Identification {
            id: row.get(0)?,
            item_name: row.get(1)?,
            copy_id: row.get(2)?,
            keywords: row.get(3)?,
            item_type: row.get(4)?,
            flags: row.get::<_, Json<_>>(5)?.0,
            level: row.get(6)?,
            quality: row.get(7)?,
            base: row.get::<_, Json<_>>(8)?.0,
            added: row.get::<_, Json<_>>(9)?.0,
            details: row.get::<_, Json<_>>(10)?.0,
            updated: row.get(11)?,
        })
    }
}

/// A saved version of the map, as listed: the header row only.
#[derive(Clone, Debug, PartialEq)]
pub struct MapVersion {
    pub id: i64,
    pub created: Timestamp,
    /// Why it was saved (automatic, before a risky change, named by the
    /// player); a pruning policy reads it.
    pub kind: String,
    pub label: String,
    pub room_count: i64,
    /// Rooms per area, so versions can be compared without their rooms.
    pub areas: BTreeMap<String, i64>,
    pub updated: Timestamp,
}

impl MapVersion {
    /// Newest first.
    pub(crate) const LIST: &'static str =
        "SELECT id, created, kind, label, room_count, areas, updated
        FROM map_versions ORDER BY created DESC, id DESC";

    pub(crate) fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(MapVersion {
            id: row.get(0)?,
            created: row.get(1)?,
            kind: row.get(2)?,
            label: row.get(3)?,
            room_count: row.get(4)?,
            areas: row.get::<_, Json<_>>(5)?.0,
            updated: row.get(6)?,
        })
    }
}

/// A live map table and its copy in map history, with its columns listed
/// once, in `from_row` order. Every table ends with `updated`, left out here
/// because restoring a version may stamp it.
pub(crate) struct MapTable {
    pub live: &'static str,
    pub version: &'static str,
    pub columns: &'static str,
    pub order_by: &'static str,
    /// Restoring stamps rows with the restore's time. A sighting's time is
    /// when the mob was seen, so it's kept.
    pub restore_stamps_updated: bool,
}

const fn map_table(
    live: &'static str,
    version: &'static str,
    columns: &'static str,
    order_by: &'static str,
) -> MapTable {
    MapTable {
        live,
        version,
        columns,
        order_by,
        restore_stamps_updated: true,
    }
}

pub(crate) const ROOMS: MapTable = map_table(
    "rooms",
    "version_rooms",
    "id, name, area, environment, description, x, y, z, building, tangled, maze",
    "id",
);
pub(crate) const EXITS: MapTable = map_table(
    "exits",
    "version_exits",
    "room_id, direction, state",
    "room_id, direction",
);
pub(crate) const LINKS: MapTable = map_table(
    "links",
    "version_links",
    "room_id, direction, to_room",
    "room_id, direction",
);
pub(crate) const DOORWAYS: MapTable = map_table(
    "doorways",
    "version_doorways",
    "room_id, direction",
    "room_id, direction",
);
pub(crate) const NOTES: MapTable = map_table(
    "room_notes",
    "version_room_notes",
    "room_id, text",
    "room_id",
);
pub(crate) const FEATURES: MapTable = map_table(
    "room_features",
    "version_room_features",
    "room_id, feature",
    "room_id, feature",
);
pub(crate) const SIGHTINGS: MapTable = MapTable {
    restore_stamps_updated: false,
    ..map_table(
        "sightings",
        "version_sightings",
        "room_id, mob",
        "room_id, mob",
    )
};

/// Every table a map version copies, parents first.
pub(crate) const MAP_TABLES: [MapTable; 7] =
    [ROOMS, EXITS, LINKS, DOORWAYS, NOTES, FEATURES, SIGHTINGS];

/// A small value object kept as JSON text in one column.
struct Json<T>(T);

impl<T: Serialize> ToSql for Json<&T> {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        let text = serde_json::to_string(self.0)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(error.into()))?;
        Ok(ToSqlOutput::from(text))
    }
}

impl<T: DeserializeOwned> FromSql for Json<T> {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let text = value.as_str()?;
        serde_json::from_str(text)
            .map(Json)
            .map_err(|error| FromSqlError::Other(error.into()))
    }
}
