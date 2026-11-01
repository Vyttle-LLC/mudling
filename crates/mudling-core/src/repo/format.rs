//! What each file in the repo holds, and how a server's rows become files.
//!
//! Each JSON file is pretty-printed, so one changed field is one changed
//! line. Keys keep the order the structs declare, maps sort theirs, and
//! records keep the key order the reader returns them in.

use super::Error;
use super::names::Names;
use super::write::is_json;
use crate::defaults::hotbar::HOTBAR_META;
use crate::map::{ISLANDS, JOINED, NOT_SAME};
use crate::persist::*;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// Bumped when a change would stop an older MUDling reading the repo.
pub(super) const FORMAT_VERSION: u32 = 1;

/// Refuses a manifest a newer MUDling wrote. No manifest has nothing to protect.
pub(crate) fn check_manifest(bytes: Option<&[u8]>) -> Result<(), Error> {
    let Some(bytes) = bytes else {
        return Ok(());
    };
    let Manifest { format } = parse(Path::new(MANIFEST), bytes)?;
    if format > FORMAT_VERSION {
        return Err(Error::NewerFormat(format));
    }
    Ok(())
}

pub(crate) const MANIFEST: &str = "mudling.json";
pub(crate) const SERVERS: &str = "servers.json";
pub(crate) const SETTINGS: &str = "settings.json";
pub(super) const GITIGNORE: &str = ".gitignore";
pub(super) const GITATTRIBUTES: &str = ".gitattributes";
pub(crate) const MAP_FOLDER: &str = "map";
pub(crate) const CHARACTERS_FOLDER: &str = "characters";
pub(crate) const GEAR_FOLDER: &str = "gear";
pub(crate) const MOBS: &str = "mobs.json";
const ITEMS: &str = "items.json";
const COPIES: &str = "copies.json";
const TRIGGERS: &str = "triggers.json";
const ALIASES: &str = "aliases.json";
pub(crate) const NOTES: &str = "notes.txt";
pub(crate) const TOMBSTONES: &str = "tombstones.json";

/// What stays out of git, whatever else a player's `.gitignore` says: the
/// vault's half-written copies, MUDling's own, and all logs (PRD §10).
pub(super) const IGNORED: [&str; 7] = [
    ".secrets.vault.*",
    "*.mudling-tmp",
    "*.log",
    "logs/",
    ".DS_Store",
    "Thumbs.db",
    "desktop.ini",
];

/// Git leaves every file's bytes alone, so Windows can't turn LF into CRLF.
pub(super) const ATTRIBUTES: [&str; 1] = ["* -text"];

/// The only [`Meta`] keys that sync besides the notepad, which has a file of
/// its own. The rest stay on the device: the room this device last stood in,
/// and the id prefix and next id that set its rooms' ids apart from other
/// devices'. Saved versions of the map stay too, since their rows hold ids a
/// union may have retired; a restore reaches other devices as the rows it
/// changes, and as tombstones for the rooms it drops.
///
/// A key ending in a dot is a family. [`JOINED`] travels because a join has to
/// reach every device and last, unlike a tombstone: it's how a copy of the
/// retired room finds its way to the kept room, however late it turns up.
/// [`NOT_SAME`] travels because the player's answer holds for the map, and no
/// device should ask it again.
///
/// What a game's plugin learns about an item travels as its name and its
/// keyword (`plugin.gear.name.` and `plugin.gear.keyword.`), since a device
/// that hasn't met the item can use them. Its slots and the plugin's other
/// words are rows (`item_facts`) or aren't the player's. `plugin.gear.holds.`
/// and `plugin.gear.verb.` stay: the plugin says them again on every
/// connection, so a copy from another device is only ever stale. They stay out
/// by name, so no wider `plugin.gear.` is listed here, and `.keywords.`, which
/// an older version moved to rows, can't match `.keyword.`.
const SYNCED_META: [&str; 6] = [
    ISLANDS,
    HOTBAR_META,
    JOINED,
    NOT_SAME,
    GEAR_NAME,
    GEAR_KEYWORD,
];
const GEAR_NAME: &str = "plugin.gear.name.";
const GEAR_KEYWORD: &str = "plugin.gear.keyword.";

/// Whether a [`Meta`] row's key says it holds the game's words for an item,
/// which can say anything, such as `a password scroll`.
pub(crate) fn is_game_words(key: &str) -> bool {
    key.starts_with(GEAR_NAME) || key.starts_with(GEAR_KEYWORD)
}

/// Whether a tombstone for `key` names only what the game wrote: its map,
/// characters, gear and mobs, and an item's name or word. What the player
/// wrote, a trigger, an alias or a setting, is not.
pub(crate) fn is_game_tombstone(key: &Key) -> bool {
    match key {
        Key::Trigger(_) | Key::Alias(_) => false,
        Key::Meta(key) => is_game_words(key),
        _ => true,
    }
}

fn is_synced_meta(key: &str) -> bool {
    SYNCED_META
        .iter()
        .any(|synced| key == *synced || (synced.ends_with('.') && key.starts_with(synced)))
}

/// How long a delete is remembered. A device offline for longer brings back
/// what was deleted in the meantime: the cost of not keeping them forever.
const TOMBSTONE_LIFE_MS: i64 = 90 * 24 * 60 * 60 * 1000;

/// The oldest tombstone still worth keeping.
pub(crate) fn tombstone_cutoff() -> Timestamp {
    Timestamp(Timestamp::now().0 - TOMBSTONE_LIFE_MS)
}

#[derive(Serialize, Deserialize)]
pub(super) struct Manifest {
    pub format: u32,
}

/// Settings that follow the player between devices. Window size and
/// position have no place here. They change as one record, and `updated`
/// says which device's copy wins.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AppSettings {
    pub theme: String,
    pub font: String,
    pub shortcuts: BTreeMap<String, String>,
    pub panels: BTreeMap<String, String>,
    pub updated: Timestamp,
}

#[derive(Default, Serialize, Deserialize)]
pub(crate) struct ServerList {
    pub servers: Vec<ServerEntry>,
}

impl ServerList {
    /// Puts the servers in id order, the order folder names are claimed in.
    pub(crate) fn sort(&mut self) {
        self.servers.sort_by(|a, b| a.server.id.cmp(&b.server.id));
    }

    /// Each server with its folder. Every listed server claims its name, so
    /// folders never shift.
    pub(crate) fn with_folders(&self) -> impl Iterator<Item = (&ServerEntry, PathBuf)> {
        let mut names = Names::default();
        self.servers
            .iter()
            .map(move |entry| (entry, PathBuf::from(names.folder(&entry.server.id))))
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ServerEntry {
    #[serde(flatten)]
    pub server: Server,
    pub meta: Vec<Meta>,
    pub characters: Vec<Character>,
    /// When the text in `notes.txt` last changed, which the file can't hold.
    pub notes_updated: Timestamp,
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

#[derive(Default, Serialize, Deserialize)]
struct TombstonesFile {
    tombstones: Vec<Tombstone>,
}

/// Everything the repo keeps from one server's database.
#[derive(Clone, Default)]
pub(crate) struct ServerRows {
    pub servers: Vec<Server>,
    pub meta: Vec<Meta>,
    pub map: MapRows,
    pub marks: Vec<MobMark>,
    pub characters: CharacterRows,
    pub gear: GearRows,
    pub triggers: Vec<Trigger>,
    pub aliases: Vec<Alias>,
    pub tombstones: Vec<Tombstone>,
}

impl ServerRows {
    pub(crate) fn load(reader: &Reader) -> Result<ServerRows, Error> {
        let cutoff = tombstone_cutoff();
        let mut tombstones = reader.tombstones()?;
        tombstones.retain(|tombstone| tombstone.deleted >= cutoff && syncs(&tombstone.key));
        Ok(ServerRows {
            servers: reader.servers()?,
            meta: reader.meta()?,
            map: reader.map()?,
            marks: reader.mob_marks()?,
            characters: reader.characters()?,
            gear: reader.gear()?,
            triggers: reader.triggers()?,
            aliases: reader.aliases()?,
            tombstones,
        })
    }

    /// The rows one server's files hold, as `files` wrote them: `paths` are
    /// relative to its folder, and any that aren't [record files] are skipped.
    ///
    /// [record files]: is_record_file
    pub(crate) fn parse<'a>(
        entry: &ServerEntry,
        files: impl Iterator<Item = (&'a Path, &'a [u8])>,
    ) -> Result<ServerRows, Error> {
        let mut rows = ServerRows::default();
        rows.add_entry(entry);
        for (path, bytes) in files {
            if is_record_file(path) {
                rows.read_file(path, bytes, entry.notes_updated)?;
            }
        }
        Ok(rows)
    }

    pub(super) fn add_entry(&mut self, entry: &ServerEntry) {
        self.servers.push(entry.server.clone());
        self.meta.extend(entry.meta.iter().cloned());
        self.characters
            .characters
            .extend(entry.characters.iter().cloned());
    }

    /// The server's line in `servers.json`.
    pub(crate) fn entry(&self, server: &Server) -> ServerEntry {
        ServerEntry {
            server: server.clone(),
            meta: self
                .meta
                .iter()
                .filter(|meta| is_synced_meta(&meta.key))
                .cloned()
                .collect(),
            characters: self.characters.characters.clone(),
            notes_updated: self
                .meta
                .iter()
                .find(|meta| meta.key == NOTEPAD)
                .map_or_else(Timestamp::default, |meta| meta.updated),
        }
    }

    /// The server's files, by path inside its folder.
    pub(crate) fn files(&self) -> BTreeMap<PathBuf, Vec<u8>> {
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
        let tombstones = TombstonesFile {
            tombstones: self.tombstones.clone(),
        };
        files.insert(TOMBSTONES.into(), json(&tombstones));
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

    /// Adds the records a server's file holds. `path` is relative to the
    /// server's folder.
    pub(super) fn read_file(
        &mut self,
        path: &Path,
        bytes: &[u8],
        notes_updated: Timestamp,
    ) -> Result<(), Error> {
        match (parent_name(path), file_name(path)) {
            (Some(MAP_FOLDER), _) => self.read_zone(parse(path, bytes)?),
            (Some(CHARACTERS_FOLDER), _) => self.read_character(parse(path, bytes)?),
            (Some(GEAR_FOLDER), Some(ITEMS)) => {
                let file: ItemsFile = parse(path, bytes)?;
                self.gear.facts.extend(file.facts);
                self.gear.identifications.extend(file.identifications);
            }
            (Some(GEAR_FOLDER), Some(COPIES)) => {
                let file: CopiesFile = parse(path, bytes)?;
                self.gear.copies.extend(file.copies);
            }
            (None, Some(MOBS)) => {
                let file: MobsFile = parse(path, bytes)?;
                self.marks.extend(file.marks);
                self.map.sightings.extend(file.sightings);
            }
            (None, Some(TRIGGERS)) => {
                self.triggers
                    .extend(parse::<TriggersFile>(path, bytes)?.triggers);
            }
            (None, Some(ALIASES)) => {
                self.aliases
                    .extend(parse::<AliasesFile>(path, bytes)?.aliases);
            }
            (None, Some(TOMBSTONES)) => {
                self.tombstones
                    .extend(parse::<TombstonesFile>(path, bytes)?.tombstones);
            }
            (None, Some(NOTES)) => {
                let value = String::from_utf8(bytes.to_vec()).map_err(|error| Error::Format {
                    path: path.to_owned(),
                    detail: error.to_string(),
                })?;
                self.meta.push(Meta {
                    key: NOTEPAD.to_owned(),
                    value,
                    updated: notes_updated,
                });
            }
            _ => {}
        }
        Ok(())
    }

    fn read_zone(&mut self, file: ZoneFile) {
        self.map.rooms.extend(file.rooms);
        self.map.exits.extend(file.exits);
        self.map.links.extend(file.links);
        self.map.doorways.extend(file.doorways);
        self.map.notes.extend(file.notes);
        self.map.features.extend(file.features);
    }

    fn read_character(&mut self, file: CharacterFile) {
        let rows = &mut self.characters;
        rows.stats.extend(file.stats);
        rows.skills.extend(file.skills);
        rows.affects.extend(file.affects);
        rows.worn.extend(file.worn);
        rows.gear_sets.extend(file.gear_sets);
    }

    /// The batch that brings a database up to these rows without overruling
    /// anything newer in it. Deletes go first and parents before children,
    /// so a record meets the tombstones and rows it depends on.
    pub(crate) fn into_batch(self) -> Batch {
        let mut batch = Batch::new();
        for tombstone in self.tombstones {
            batch.bury(tombstone);
        }
        adopt_all(&mut batch, self.servers);
        adopt_all(&mut batch, self.meta);
        adopt_all(&mut batch, self.characters.characters);
        adopt_all(&mut batch, self.characters.stats);
        adopt_all(&mut batch, self.characters.skills);
        adopt_all(&mut batch, self.characters.affects);
        adopt_all(&mut batch, self.characters.worn);
        adopt_all(&mut batch, self.characters.gear_sets);
        adopt_all(&mut batch, self.map.rooms);
        adopt_all(&mut batch, self.map.exits);
        adopt_all(&mut batch, self.map.links);
        adopt_all(&mut batch, self.map.doorways);
        adopt_all(&mut batch, self.map.notes);
        adopt_all(&mut batch, self.map.features);
        adopt_all(&mut batch, self.map.sightings);
        adopt_all(&mut batch, self.marks);
        adopt_all(&mut batch, self.gear.copies);
        adopt_all(&mut batch, self.gear.facts);
        adopt_all(&mut batch, self.gear.identifications);
        adopt_all(&mut batch, self.triggers);
        adopt_all(&mut batch, self.aliases);
        batch
    }
}

/// Whether a delete of `key` is worth telling other devices about.
fn syncs(key: &Key) -> bool {
    match key {
        Key::Meta(key) => key == NOTEPAD || is_synced_meta(key),
        _ => true,
    }
}

/// Whether `path`, relative to a server's folder, is a file of records.
/// Anything else that lands in the folder is the player's own.
pub(crate) fn is_record_file(path: &Path) -> bool {
    let Some(name) = file_name(path) else {
        return false;
    };
    match parent_name(path) {
        None => [MOBS, TRIGGERS, ALIASES, NOTES, TOMBSTONES].contains(&name),
        Some(GEAR_FOLDER) => path.components().count() == 2 && [ITEMS, COPIES].contains(&name),
        Some(MAP_FOLDER | CHARACTERS_FOLDER) => {
            path.components().count() == 2 && is_json(Path::new(name))
        }
        Some(_) => false,
    }
}

fn file_name(path: &Path) -> Option<&str> {
    path.file_name().and_then(|name| name.to_str())
}

fn parent_name(path: &Path) -> Option<&str> {
    path.parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
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

fn adopt_all(batch: &mut Batch, records: Vec<impl Into<Record>>) {
    for record in records {
        batch.adopt(record);
    }
}

/// `current`, with any of the `required` lines it lacks added at the end.
pub(super) fn with_lines(current: Option<&[u8]>, required: &[&str]) -> Vec<u8> {
    let mut bytes = current.unwrap_or_default().to_vec();
    if bytes.last().is_some_and(|last| *last != b'\n') {
        bytes.push(b'\n');
    }
    let text = String::from_utf8_lossy(current.unwrap_or_default()).into_owned();
    for line in required {
        if !text.lines().any(|present| present.trim() == *line) {
            bytes.extend_from_slice(line.as_bytes());
            bytes.push(b'\n');
        }
    }
    bytes
}

/// A file's bytes: pretty JSON, LF endings, a final newline.
pub(crate) fn json(value: &impl Serialize) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(value).expect("plain records serialize");
    bytes.push(b'\n');
    bytes
}

pub(crate) fn parse<T: DeserializeOwned>(path: &Path, bytes: &[u8]) -> Result<T, Error> {
    serde_json::from_slice(bytes).map_err(|error| Error::Format {
        path: path.to_owned(),
        detail: error.to_string(),
    })
}
