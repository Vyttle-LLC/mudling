//! What each file in the repo holds, and how a server's rows become files.
//!
//! Each JSON file is pretty-printed, so one changed field is one changed
//! line. Keys keep the order the structs declare, maps sort theirs, and
//! records keep the key order the reader returns them in.

use super::Error;
use super::names::Names;
use crate::map::ISLANDS;
use crate::persist::*;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// Bumped when a change would stop an older MUDling reading the repo.
pub(super) const FORMAT_VERSION: u32 = 1;

pub(super) const MANIFEST: &str = "mudling.json";
pub(super) const SERVERS: &str = "servers.json";
pub(super) const SETTINGS: &str = "settings.json";
pub(super) const MAP_FOLDER: &str = "map";
pub(super) const CHARACTERS_FOLDER: &str = "characters";
pub(super) const GEAR_FOLDER: &str = "gear";
const MOBS: &str = "mobs.json";
const ITEMS: &str = "items.json";
const COPIES: &str = "copies.json";
const TRIGGERS: &str = "triggers.json";
const ALIASES: &str = "aliases.json";
pub(super) const NOTES: &str = "notes.txt";

/// The only [`Meta`] keys that sync. The rest stay on the device, such as
/// the room this device last stood in.
const SYNCED_META: [&str; 1] = [ISLANDS];

#[derive(Serialize, Deserialize)]
pub(super) struct Manifest {
    pub format: u32,
}

/// Settings that follow the player between devices. Window size and
/// position have no place here.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AppSettings {
    pub theme: String,
    pub font: String,
    pub shortcuts: BTreeMap<String, String>,
    pub panels: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct ServerList {
    pub servers: Vec<ServerEntry>,
}

impl ServerList {
    /// Puts the servers in id order, the order folder names are claimed in.
    pub(super) fn sort(&mut self) {
        self.servers.sort_by(|a, b| a.server.id.cmp(&b.server.id));
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct ServerEntry {
    #[serde(flatten)]
    pub server: Server,
    pub meta: Vec<Meta>,
    pub characters: Vec<Character>,
}

#[derive(Serialize, Deserialize)]
struct ZoneFile {
    zone: String,
    rooms: Vec<Room>,
    exits: Vec<Exit>,
    links: Vec<Link>,
    doorways: Vec<Doorway>,
    notes: Vec<RoomNote>,
    features: Vec<RoomFeature>,
}

#[derive(Serialize, Deserialize)]
struct MobsFile {
    marks: Vec<MobMark>,
    sightings: Vec<Sighting>,
}

#[derive(Serialize, Deserialize)]
struct CharacterFile {
    id: String,
    stats: Vec<CharacterStat>,
    skills: Vec<Skill>,
    affects: Vec<Affect>,
    worn: Vec<Worn>,
    gear_sets: Vec<GearSet>,
}

#[derive(Serialize, Deserialize)]
struct ItemsFile {
    facts: Vec<ItemFacts>,
    identifications: Vec<Identification>,
}

#[derive(Serialize, Deserialize)]
struct CopiesFile {
    copies: Vec<GearCopy>,
}

#[derive(Serialize, Deserialize)]
struct TriggersFile {
    triggers: Vec<Trigger>,
}

#[derive(Serialize, Deserialize)]
struct AliasesFile {
    aliases: Vec<Alias>,
}

/// Everything the repo keeps from one server's database.
pub(super) struct ServerRows {
    pub servers: Vec<Server>,
    meta: Vec<Meta>,
    map: MapRows,
    marks: Vec<MobMark>,
    characters: CharacterRows,
    gear: GearRows,
    triggers: Vec<Trigger>,
    aliases: Vec<Alias>,
}

impl ServerRows {
    pub(super) fn load(reader: &Reader) -> Result<ServerRows, Error> {
        Ok(ServerRows {
            servers: reader.servers()?,
            meta: reader.meta()?,
            map: reader.map()?,
            marks: reader.mob_marks()?,
            characters: reader.characters()?,
            gear: reader.gear()?,
            triggers: reader.triggers()?,
            aliases: reader.aliases()?,
        })
    }

    /// The server's line in `servers.json`.
    pub(super) fn entry(&self, server: &Server) -> ServerEntry {
        ServerEntry {
            server: server.clone(),
            meta: self
                .meta
                .iter()
                .filter(|meta| SYNCED_META.contains(&meta.key.as_str()))
                .cloned()
                .collect(),
            characters: self.characters.characters.clone(),
        }
    }

    /// The server's files, by path inside its folder.
    pub(super) fn files(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = BTreeMap::new();
        for (zone, file) in self.zones() {
            let path = Path::new(MAP_FOLDER).join(zone);
            files.insert(path, json(&file));
        }
        for (name, file) in self.character_files() {
            let path = Path::new(CHARACTERS_FOLDER).join(name);
            files.insert(path, json(&file));
        }
        let mobs = MobsFile {
            marks: self.marks.clone(),
            sightings: self.map.sightings.clone(),
        };
        let items = ItemsFile {
            facts: self.gear.facts.clone(),
            identifications: self.gear.identifications.clone(),
        };
        let copies = CopiesFile {
            copies: self.gear.copies.clone(),
        };
        let triggers = TriggersFile {
            triggers: self.triggers.clone(),
        };
        let aliases = AliasesFile {
            aliases: self.aliases.clone(),
        };
        files.insert(MOBS.into(), json(&mobs));
        files.insert(Path::new(GEAR_FOLDER).join(ITEMS), json(&items));
        files.insert(Path::new(GEAR_FOLDER).join(COPIES), json(&copies));
        files.insert(TRIGGERS.into(), json(&triggers));
        files.insert(ALIASES.into(), json(&aliases));
        files.insert(NOTES.into(), self.notepad());
        files
    }

    /// One file per zone, by file name. The zones sort, so their names are
    /// claimed in a fixed order.
    fn zones(&self) -> Vec<(String, ZoneFile)> {
        let mut zones: BTreeMap<&str, ZoneFile> = BTreeMap::new();
        let mut zone_of = HashMap::with_capacity(self.map.rooms.len());
        for room in &self.map.rooms {
            zone_of.insert(room.id, room.area.as_str());
            let zone = zones.entry(&room.area).or_insert_with(|| ZoneFile {
                zone: room.area.clone(),
                rooms: Vec::new(),
                exits: Vec::new(),
                links: Vec::new(),
                doorways: Vec::new(),
                notes: Vec::new(),
                features: Vec::new(),
            });
            zone.rooms.push(room.clone());
        }
        for exit in &self.map.exits {
            let file = file_of(&mut zones, &zone_of, exit.room_id);
            file.exits.push(exit.clone());
        }
        for link in &self.map.links {
            let file = file_of(&mut zones, &zone_of, link.room_id);
            file.links.push(link.clone());
        }
        for doorway in &self.map.doorways {
            let file = file_of(&mut zones, &zone_of, doorway.room_id);
            file.doorways.push(doorway.clone());
        }
        for note in &self.map.notes {
            let file = file_of(&mut zones, &zone_of, note.room_id);
            file.notes.push(note.clone());
        }
        for feature in &self.map.features {
            let file = file_of(&mut zones, &zone_of, feature.room_id);
            file.features.push(feature.clone());
        }
        let mut names = Names::default();
        zones
            .into_values()
            .map(|zone| (format!("{}.json", names.stem(&zone.zone)), zone))
            .collect()
    }

    /// One file per character, by file name. Names are claimed in order of
    /// name and id, so two characters sharing a name stay apart the same way
    /// on every run.
    fn character_files(&self) -> Vec<(String, CharacterFile)> {
        let rows = &self.characters;
        let mut characters: Vec<&Character> = rows.characters.iter().collect();
        characters.sort_by_key(|character| (&character.name, &character.id));
        let mut names = Names::default();
        characters
            .into_iter()
            .map(|character| {
                let id = &character.id;
                let file = CharacterFile {
                    id: id.clone(),
                    stats: of_character(&rows.stats, id, |stat| &stat.character_id),
                    skills: of_character(&rows.skills, id, |skill| &skill.character_id),
                    affects: of_character(&rows.affects, id, |affect| &affect.character_id),
                    worn: of_character(&rows.worn, id, |worn| &worn.character_id),
                    gear_sets: of_character(&rows.gear_sets, id, |set| &set.character_id),
                };
                (format!("{}.json", names.stem(&character.name)), file)
            })
            .collect()
    }

    fn notepad(&self) -> Vec<u8> {
        let text = self.meta.iter().find(|meta| meta.key == NOTEPAD);
        text.map_or_else(Vec::new, |meta| notes_bytes(&meta.value))
    }
}

/// The file of a room's zone. A foreign key keeps every row's room in the
/// same read of the map.
fn file_of<'a>(
    zones: &'a mut BTreeMap<&str, ZoneFile>,
    zone_of: &HashMap<i64, &str>,
    room_id: i64,
) -> &'a mut ZoneFile {
    zones.get_mut(zone_of[&room_id]).expect("its room's zone")
}

fn of_character<T: Clone>(rows: &[T], id: &str, owner: impl Fn(&T) -> &String) -> Vec<T> {
    rows.iter()
        .filter(|row| owner(row) == id)
        .cloned()
        .collect()
}

/// A note's text with LF endings, ending in a newline if it has any text.
fn notes_bytes(text: &str) -> Vec<u8> {
    let mut text = text.replace("\r\n", "\n");
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.into_bytes()
}

/// Adds the records a server's file holds to `batch`. `path` is relative to
/// the server's folder.
pub(super) fn read_file(path: &Path, bytes: &[u8], batch: &mut Batch) -> Result<(), Error> {
    let name = path.file_name().and_then(|name| name.to_str());
    let parent = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str());
    match (parent, name) {
        (Some(MAP_FOLDER), _) => read_zone(parse(path, bytes)?, batch),
        (Some(CHARACTERS_FOLDER), _) => read_character(parse(path, bytes)?, batch),
        (Some(GEAR_FOLDER), Some(ITEMS)) => {
            let file: ItemsFile = parse(path, bytes)?;
            upsert_all(batch, file.facts);
            upsert_all(batch, file.identifications);
        }
        (Some(GEAR_FOLDER), Some(COPIES)) => {
            upsert_all(batch, parse::<CopiesFile>(path, bytes)?.copies);
        }
        (_, Some(MOBS)) => {
            let file: MobsFile = parse(path, bytes)?;
            upsert_all(batch, file.marks);
            upsert_all(batch, file.sightings);
        }
        (_, Some(TRIGGERS)) => upsert_all(batch, parse::<TriggersFile>(path, bytes)?.triggers),
        (_, Some(ALIASES)) => upsert_all(batch, parse::<AliasesFile>(path, bytes)?.aliases),
        (_, Some(NOTES)) => {
            let value = String::from_utf8(bytes.to_vec()).map_err(|error| Error::Format {
                path: path.to_owned(),
                detail: error.to_string(),
            })?;
            // Plain text has no time of its own, so it's stamped on arrival.
            batch.upsert(Meta {
                key: NOTEPAD.to_owned(),
                value,
                updated: Timestamp::now(),
            });
        }
        _ => {}
    }
    Ok(())
}

fn read_zone(file: ZoneFile, batch: &mut Batch) {
    upsert_all(batch, file.rooms);
    upsert_all(batch, file.exits);
    upsert_all(batch, file.links);
    upsert_all(batch, file.doorways);
    upsert_all(batch, file.notes);
    upsert_all(batch, file.features);
}

fn read_character(file: CharacterFile, batch: &mut Batch) {
    upsert_all(batch, file.stats);
    upsert_all(batch, file.skills);
    upsert_all(batch, file.affects);
    upsert_all(batch, file.worn);
    upsert_all(batch, file.gear_sets);
}

fn upsert_all(batch: &mut Batch, records: Vec<impl Into<Record>>) {
    for record in records {
        batch.upsert(record);
    }
}

/// A file's bytes: pretty JSON, LF endings, a final newline.
pub(super) fn json(value: &impl Serialize) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(value).expect("plain records serialize");
    bytes.push(b'\n');
    bytes
}

pub(super) fn parse<T: DeserializeOwned>(path: &Path, bytes: &[u8]) -> Result<T, Error> {
    serde_json::from_slice(bytes).map_err(|error| Error::Format {
        path: path.to_owned(),
        detail: error.to_string(),
    })
}
