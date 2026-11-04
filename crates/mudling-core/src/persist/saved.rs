//! What a player has saved, across every server's database: enough for a
//! shell to list where they can go, before any session opens.
//!
//! The servers folder is the list: a server is saved once its database
//! exists, unless the player removed it. Each call opens databases, so call it
//! from a core thread or another thread that may wait on the disk, never the
//! UI thread.

use super::records::{Alias, Character, Meta, Server, Trigger};
use super::{Batch, Database, Error, Key, Timestamp};
use crate::map::CURRENT_ROOM;
use std::path::Path;

/// Under the data folder, one database per server.
pub const SERVERS_FOLDER: &str = "servers";
/// A server's database file ends with this.
const EXTENSION: &str = "sqlite";
/// The `meta` key the game plugins the player turned on are kept under, as a
/// JSON list of plugin ids.
const ENABLED_PLUGINS: &str = "session.enabled-plugins";
/// The same for the overrides the player turned off.
const OVERRIDES_OFF: &str = "session.overrides-off";
/// The `meta` key that hides a server the player removed from their list.
pub(crate) const REMOVED: &str = "server.removed";
/// The `meta` keys, one per character id after the dot, that hide a character
/// the player removed. A character played or added again since is back.
pub(crate) const CHARACTER_REMOVED: &str = "character.removed.";
/// Who a session plays when no credentials name a character.
pub(crate) const UNNAMED_CHARACTER: &str = "player";
/// Stat keys: a hook's `character` store, as the engine saves it.
const NAME_STAT: &str = "character.name";
const LEVEL_STAT: &str = "character.level";

/// A saved server and who has played on it.
#[derive(Clone, Debug, PartialEq)]
pub struct SavedServer {
    pub host: String,
    pub port: u16,
    pub updated: Timestamp,
    /// The room the player was last in, when the map knows one.
    pub last_room: Option<String>,
    pub characters: Vec<SavedCharacter>,
}

/// A character on a saved server. Each field but `id` is `None` until the
/// game, or the player adding it, has told MUDling about it.
#[derive(Clone, Debug, PartialEq)]
pub struct SavedCharacter {
    /// The lowercased login name, as the engine keys its rows.
    pub id: String,
    pub name: Option<String>,
    pub level: Option<i64>,
    pub last_played: Timestamp,
}

/// Every saved server, and what went wrong with those that can't be read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SavedServers {
    pub servers: Vec<SavedServer>,
    pub problems: Vec<String>,
}

/// Lists the saved servers in `data_folder`, by host. One unreadable database
/// doesn't hide the rest. A folder with none yet is an empty list.
pub fn saved_servers(data_folder: &Path) -> SavedServers {
    let folder = data_folder.join(SERVERS_FOLDER);
    let mut saved = SavedServers::default();
    let files = match std::fs::read_dir(&folder) {
        Ok(files) => files,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return saved,
        Err(error) => {
            saved.problems.push(Error::Io(error).to_string());
            return saved;
        }
    };
    for file in files.flatten() {
        let path = file.path();
        if path
            .extension()
            .is_none_or(|extension| extension != EXTENSION)
        {
            continue;
        }
        let name = path.file_stem().unwrap_or_default().to_string_lossy();
        match read_server(&folder, &name) {
            Ok(server) => saved.servers.extend(server),
            Err(error) => saved.problems.push(format!("{name}: {error}")),
        }
    }
    saved.servers.sort_by(|a, b| a.host.cmp(&b.host));
    saved
}

fn read_server(folder: &Path, name: &str) -> Result<Option<SavedServer>, Error> {
    let reader = Database::open(folder, name)?.reader()?;
    let meta = reader.meta()?;
    if meta.iter().any(|meta| meta.key == REMOVED) {
        return Ok(None);
    }
    let Some(server) = reader.servers()?.into_iter().next() else {
        return Ok(None);
    };
    let last_room = meta
        .iter()
        .find(|meta| meta.key == CURRENT_ROOM)
        .and_then(|meta| meta.value.parse().ok())
        .map(|room| reader.room_name(room))
        .transpose()?
        .flatten();
    let rows = reader.characters()?;
    let stat = |character: &str, key: &str| {
        rows.stats
            .iter()
            .find(|stat| stat.character_id == character && stat.key == key)
            .map(|stat| stat.value.clone())
    };
    let removed = |character: &Character| {
        let key = format!("{CHARACTER_REMOVED}{}", character.id);
        meta.iter()
            .any(|meta| meta.key == key && meta.updated >= character.updated)
    };
    let characters = rows
        .characters
        .iter()
        .filter(|character| !removed(character))
        .map(|character| SavedCharacter {
            id: character.id.clone(),
            name: stat(&character.id, NAME_STAT)
                .and_then(|name| serde_json::from_str(&name).ok())
                .or_else(|| (character.id != UNNAMED_CHARACTER).then(|| character.name.clone())),
            level: stat(&character.id, LEVEL_STAT)
                .and_then(|level| level.parse::<f64>().ok())
                .map(|level| level as i64),
            last_played: character.updated,
        })
        .collect();
    Ok(Some(SavedServer {
        host: server.host,
        port: server.port,
        updated: server.updated,
        last_room,
        characters,
    }))
}

/// Every other saved server's triggers and aliases, each with the server
/// whose database keeps it, and what went wrong with those that can't be read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OtherServersRows {
    pub triggers: Vec<(String, Trigger)>,
    pub aliases: Vec<(String, Alias)>,
    pub problems: Vec<String>,
}

/// Reads the rules of every saved server but `host`.
pub fn other_servers_rows(data_folder: &Path, host: &str) -> OtherServersRows {
    let folder = data_folder.join(SERVERS_FOLDER);
    let mut rows = OtherServersRows::default();
    let Ok(files) = std::fs::read_dir(&folder) else {
        return rows;
    };
    for file in files.flatten() {
        let path = file.path();
        let name = path.file_stem().unwrap_or_default().to_string_lossy();
        if name == host || path.extension().is_none_or(|e| e != EXTENSION) {
            continue;
        }
        let read = Database::open(&folder, &name)
            .and_then(|database| database.reader())
            .and_then(|reader| Ok((reader.triggers()?, reader.aliases()?)));
        match read {
            Ok((triggers, aliases)) => {
                let keeper = || name.to_string();
                rows.triggers
                    .extend(triggers.into_iter().map(|row| (keeper(), row)));
                rows.aliases
                    .extend(aliases.into_iter().map(|row| (keeper(), row)));
            }
            Err(error) => rows.problems.push(format!("{name}: {error}")),
        }
    }
    rows
}

/// Commits `batch` to `host`'s database. It blocks until that's on the disk.
pub fn write_to_server(data_folder: &Path, host: &str, batch: Batch) -> Result<(), Error> {
    let folder = data_folder.join(SERVERS_FOLDER);
    let writer = Database::open(&folder, host)?.writer()?;
    writer.submit(batch)?;
    writer.flush()
}

/// Saves a server the player added. A server they removed comes back with
/// everything it had. It blocks until the row is on the disk.
pub fn save_server(data_folder: &Path, host: &str, port: u16) -> Result<(), Error> {
    let folder = data_folder.join(SERVERS_FOLDER);
    let mut batch = Batch::new();
    batch.upsert(Server {
        id: host.to_owned(),
        name: host.to_owned(),
        host: host.to_owned(),
        port,
        settings: "{}".to_owned(),
        updated: Timestamp::now(),
    });
    batch.delete(Key::Meta(REMOVED.to_owned()));
    let writer = Database::open(&folder, host)?.writer()?;
    writer.submit(batch)?;
    writer.flush()
}

/// Takes `host` off the player's list. Its map, notes and characters stay in
/// its database: deleting the server's row would take the characters with it,
/// and adding the server or connecting to it again brings it all back. It
/// blocks until that's on the disk.
pub fn remove_server(data_folder: &Path, host: &str) -> Result<(), Error> {
    let folder = data_folder.join(SERVERS_FOLDER);
    let mut batch = Batch::new();
    batch.upsert(Meta {
        key: REMOVED.to_owned(),
        value: "true".to_owned(),
        updated: Timestamp::now(),
    });
    let writer = Database::open(&folder, host)?.writer()?;
    writer.submit(batch)?;
    writer.flush()
}

/// The id a character's rows are kept by: its name, lowercased, however the
/// player or the game capitalized it.
pub fn character_id(name: &str) -> String {
    name.to_lowercase()
}

/// Saves a character the player added on `host`, a saved server, keyed by
/// [`character_id`] as the engine keys who logs in. One they removed comes
/// back with everything it had. It blocks until the row is on the disk.
pub fn save_character(data_folder: &Path, host: &str, name: &str) -> Result<(), Error> {
    let id = character_id(name);
    let mut batch = Batch::new();
    batch.delete(Key::Meta(format!("{CHARACTER_REMOVED}{id}")));
    batch.upsert(Character {
        id,
        server_id: host.to_owned(),
        name: name.to_owned(),
        updated: Timestamp::now(),
    });
    write_to_server(data_folder, host, batch)
}

/// Takes the character `id` off `host`'s list. Its stats, gear and history
/// stay in the database, so playing it again brings it all back. It blocks
/// until that's on the disk.
pub fn remove_character(data_folder: &Path, host: &str, id: &str) -> Result<(), Error> {
    let mut batch = Batch::new();
    batch.upsert(Meta {
        key: format!("{CHARACTER_REMOVED}{id}"),
        value: "true".to_owned(),
        updated: Timestamp::now(),
    });
    write_to_server(data_folder, host, batch)
}

/// The game plugins the player turned on for `host`.
pub fn enabled_plugins(data_folder: &Path, host: &str) -> Result<Vec<String>, Error> {
    plugin_list(data_folder, host, ENABLED_PLUGINS)
}

/// The overrides the player turned off for `host`.
pub fn overrides_off(data_folder: &Path, host: &str) -> Result<Vec<String>, Error> {
    plugin_list(data_folder, host, OVERRIDES_OFF)
}

/// How many triggers and aliases `host` keeps, whoever they're for.
pub fn rule_counts(data_folder: &Path, host: &str) -> Result<(usize, usize), Error> {
    let folder = data_folder.join(SERVERS_FOLDER);
    let reader = Database::open(&folder, host)?.reader()?;
    Ok((reader.triggers()?.len(), reader.aliases()?.len()))
}

fn plugin_list(data_folder: &Path, host: &str, key: &str) -> Result<Vec<String>, Error> {
    let folder = data_folder.join(SERVERS_FOLDER);
    let meta = Database::open(&folder, host)?.reader()?.meta()?;
    let list = meta.iter().find(|meta| meta.key == key);
    Ok(list
        .and_then(|meta| serde_json::from_str(&meta.value).ok())
        .unwrap_or_default())
}

/// Saves which game plugins the player turned on for `host`. It blocks until
/// they're on the disk.
pub fn save_enabled_plugins(
    data_folder: &Path,
    host: &str,
    plugins: &[String],
) -> Result<(), Error> {
    save_plugin_list(data_folder, host, ENABLED_PLUGINS, plugins)
}

/// Saves which overrides the player turned off for `host`. It blocks until
/// they're on the disk.
pub fn save_overrides_off(data_folder: &Path, host: &str, plugins: &[String]) -> Result<(), Error> {
    save_plugin_list(data_folder, host, OVERRIDES_OFF, plugins)
}

fn save_plugin_list(
    data_folder: &Path,
    host: &str,
    key: &str,
    plugins: &[String],
) -> Result<(), Error> {
    let folder = data_folder.join(SERVERS_FOLDER);
    let mut batch = Batch::new();
    batch.upsert(Meta {
        key: key.to_owned(),
        value: serde_json::Value::from(plugins).to_string(),
        updated: Timestamp::now(),
    });
    let writer = Database::open(&folder, host)?.writer()?;
    writer.submit(batch)?;
    writer.flush()
}
