//! Reads for startup and lists, on a connection of their own so they never
//! wait behind the writer thread. Reads block on the disk: call them from
//! a core thread, never the UI thread.

use super::Error;
use super::records::*;
use rusqlite::{Connection, Params, Row};

pub struct Reader {
    connection: Connection,
}

/// The rows of a map: the live one, or a saved version.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MapRows {
    pub rooms: Vec<Room>,
    pub exits: Vec<Exit>,
    pub notes: Vec<RoomNote>,
    pub features: Vec<RoomFeature>,
    pub sightings: Vec<Sighting>,
}

/// Every character's records.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CharacterRows {
    pub characters: Vec<Character>,
    pub stats: Vec<CharacterStat>,
    pub skills: Vec<Skill>,
    pub affects: Vec<Affect>,
    pub worn: Vec<Worn>,
    pub gear_sets: Vec<GearSet>,
}

/// What's known about items, shared by every character.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GearRows {
    pub copies: Vec<GearCopy>,
    pub facts: Vec<ItemFacts>,
    pub identifications: Vec<Identification>,
}

impl Reader {
    pub(crate) fn new(connection: Connection) -> Reader {
        Reader { connection }
    }

    pub fn servers(&self) -> Result<Vec<Server>, Error> {
        Ok(self.all()?)
    }

    pub fn meta(&self) -> Result<Vec<Meta>, Error> {
        Ok(self.all()?)
    }

    pub fn mob_marks(&self) -> Result<Vec<MobMark>, Error> {
        Ok(self.all()?)
    }

    /// The live map, read as one snapshot.
    pub fn map(&self) -> Result<MapRows, Error> {
        let _snapshot = self.connection.unchecked_transaction()?;
        Ok(MapRows {
            rooms: self.all()?,
            exits: self.all()?,
            notes: self.all()?,
            features: self.all()?,
            sightings: self.all()?,
        })
    }

    pub fn characters(&self) -> Result<CharacterRows, Error> {
        let _snapshot = self.connection.unchecked_transaction()?;
        Ok(CharacterRows {
            characters: self.all()?,
            stats: self.all()?,
            skills: self.all()?,
            affects: self.all()?,
            worn: self.all()?,
            gear_sets: self.all()?,
        })
    }

    pub fn gear(&self) -> Result<GearRows, Error> {
        let _snapshot = self.connection.unchecked_transaction()?;
        Ok(GearRows {
            copies: self.all()?,
            facts: self.all()?,
            identifications: self.all()?,
        })
    }

    /// Saved map versions, newest first. Reads the header table only.
    pub fn map_versions(&self) -> Result<Vec<MapVersion>, Error> {
        Ok(self.query(MapVersion::LIST, [], MapVersion::from_row)?)
    }

    /// The rows of one saved version, to preview it or compare it.
    pub fn map_version(&self, version_id: i64) -> Result<MapRows, Error> {
        let _snapshot = self.connection.unchecked_transaction()?;
        let id = [version_id];
        Ok(MapRows {
            rooms: self.query(
                "SELECT id, name, area, environment, description, x, y, z, building, tangled, maze, updated
                 FROM version_rooms WHERE version_id = ?1 ORDER BY id",
                id,
                Room::from_row,
            )?,
            exits: self.query(
                "SELECT room_id, direction, door, doorway, to_room, updated
                 FROM version_exits WHERE version_id = ?1 ORDER BY room_id, direction",
                id,
                Exit::from_row,
            )?,
            notes: self.query(
                "SELECT room_id, text, updated FROM version_room_notes WHERE version_id = ?1 ORDER BY room_id",
                id,
                RoomNote::from_row,
            )?,
            features: self.query(
                "SELECT room_id, feature, updated
                 FROM version_room_features WHERE version_id = ?1 ORDER BY room_id, feature",
                id,
                RoomFeature::from_row,
            )?,
            sightings: self.query(
                "SELECT room_id, mob, updated FROM version_sightings WHERE version_id = ?1 ORDER BY room_id, mob",
                id,
                Sighting::from_row,
            )?,
        })
    }

    fn all<T: Stored>(&self) -> rusqlite::Result<Vec<T>> {
        self.query(T::SELECT, [], T::from_row)
    }

    fn query<T>(
        &self,
        sql: &str,
        params: impl Params,
        from_row: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<Vec<T>> {
        self.connection
            .prepare_cached(sql)?
            .query_map(params, from_row)?
            .collect()
    }
}
