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
    pub links: Vec<Link>,
    pub doorways: Vec<Doorway>,
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
            links: self.all()?,
            doorways: self.all()?,
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
        Ok(MapRows {
            rooms: self.version_rows(&ROOMS, version_id)?,
            exits: self.version_rows(&EXITS, version_id)?,
            links: self.version_rows(&LINKS, version_id)?,
            doorways: self.version_rows(&DOORWAYS, version_id)?,
            notes: self.version_rows(&NOTES, version_id)?,
            features: self.version_rows(&FEATURES, version_id)?,
            sightings: self.version_rows(&SIGHTINGS, version_id)?,
        })
    }

    fn all<T: Stored>(&self) -> rusqlite::Result<Vec<T>> {
        self.query(T::SELECT, [], T::from_row)
    }

    fn version_rows<T: Stored>(
        &self,
        table: &MapTable,
        version_id: i64,
    ) -> rusqlite::Result<Vec<T>> {
        let MapTable {
            version,
            columns,
            order_by,
            ..
        } = table;
        let sql = format!(
            "SELECT {columns}, updated FROM {version} WHERE version_id = ?1 ORDER BY {order_by}"
        );
        self.query(&sql, [version_id], T::from_row)
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
