//! What a player has saved, across every server's database: enough for a
//! shell to list where they can go, before any session opens.
//!
//! The servers folder is the list: a server is saved once its database
//! exists. Each call opens databases, so call it from a core thread or
//! another thread that may wait on the disk, never the UI thread.

use super::records::{Meta, Server};
use super::{Batch, Database, Error, Timestamp};
use crate::map::CURRENT_ROOM;
use std::path::Path;

/// Under the data folder, one database per server.
pub const SERVERS_FOLDER: &str = "servers";
/// A server's database file ends with this.
const EXTENSION: &str = "sqlite";
/// The `meta` key the game plugins the player turned on are kept under, as a
/// JSON list of plugin ids.
const ENABLED_PLUGINS: &str = "session.enabled-plugins";
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

/// A character on a saved server. Each field is `None` until the game has
/// told MUDling about it.
#[derive(Clone, Debug, PartialEq)]
pub struct SavedCharacter {
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
    let Some(server) = reader.servers()?.into_iter().next() else {
        return Ok(None);
    };
    let last_room = reader
        .meta()?
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
    let characters = rows
        .characters
        .iter()
        .map(|character| SavedCharacter {
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

/// Saves a server the player added. It blocks until the row is on the disk.
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
    let writer = Database::open(&folder, host)?.writer()?;
    writer.submit(batch)?;
    writer.flush()
}

/// The game plugins the player turned on for `host`.
pub fn enabled_plugins(data_folder: &Path, host: &str) -> Result<Vec<String>, Error> {
    let folder = data_folder.join(SERVERS_FOLDER);
    let meta = Database::open(&folder, host)?.reader()?.meta()?;
    let enabled = meta.iter().find(|meta| meta.key == ENABLED_PLUGINS);
    Ok(enabled
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
    let folder = data_folder.join(SERVERS_FOLDER);
    let mut batch = Batch::new();
    batch.upsert(Meta {
        key: ENABLED_PLUGINS.to_owned(),
        value: serde_json::Value::from(plugins).to_string(),
        updated: Timestamp::now(),
    });
    let writer = Database::open(&folder, host)?.writer()?;
    writer.submit(batch)?;
    writer.flush()
}
