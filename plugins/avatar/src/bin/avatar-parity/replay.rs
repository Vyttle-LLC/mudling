//! Every log through [`Engine`] with the Avatar plugin on: one connection
//! each, on one data folder, so a log starts from what the last one saved.

use crate::log::{self, Happened};
use crate::model::{Room, World, plain_item};
use mudling_core::net::DisconnectReason;
use mudling_core::persist::{Affect, Database, MapRows, Skill, Timestamp};
use mudling_core::plugin::luau::HookLimits;
use mudling_core::session::{Credentials, Engine, SessionConfig, Typing};
use mudling_core::store::StoreHub;
use mudling_core::telnet::command::{IAC, SB, SE};
use mudling_core::telnet::{Bytes, option::GMCP};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const HOST: &str = "avatar.outland.org";
const PORT: u16 = 3000;
/// The prototype's replay counts a second per room.
const ROOM_MS: i64 = 1_000;
/// Between logs: long enough for any pending move to expire.
const SESSION_GAP_MS: i64 = 3_600_000;
/// A replay reads far faster than a server sends, and a hook mustn't miss
/// what the live queue would have held.
const HOOK_QUEUE: usize = 1 << 22;
/// An identify's hook reads what's at hand when it runs, on its own thread:
/// the events after one wait for it, or it could see a later inventory.
const IDENTIFY: &[u8] = b"Object '";
const PACED_EVENTS: usize = 4;
const HOOK_CATCH_UP: Duration = Duration::from_millis(10);
const GEAR_META: &str = "plugin.gear.";
const IDENTIFY_META: &str = "plugin.identify.";

/// A log's text as the telnet layer would receive it.
fn wire_text(text: &[u8]) -> Vec<u8> {
    let mut wire = Vec::with_capacity(text.len());
    for &byte in text {
        wire.push(byte);
        if byte == IAC {
            wire.push(IAC);
        }
    }
    wire
}

fn gmcp_frame(package: &str, json: &str) -> Vec<u8> {
    let mut frame = vec![IAC, SB, GMCP];
    frame.extend_from_slice(package.as_bytes());
    if !json.is_empty() {
        frame.push(b' ');
        frame.extend_from_slice(json.as_bytes());
    }
    frame.extend_from_slice(&[IAC, SE]);
    frame
}

pub struct Replay {
    pub world: World,
    pub notices: Vec<String>,
}

fn config(folder: &Path, character: Option<&str>) -> SessionConfig {
    let plugin = mudling_plugin_avatar::load();
    SessionConfig {
        enabled: vec![plugin.id().clone()],
        plugins: vec![plugin],
        credentials: character.map(|name| Credentials {
            name: name.to_owned(),
            password: String::new(),
        }),
        hook_limits: HookLimits {
            queue: HOOK_QUEUE,
            ..HookLimits::default()
        },
        ..SessionConfig::new(folder, HOST, PORT)
    }
}

fn read_log(
    log: &str,
    folder: &Path,
    clock: &mut i64,
    notices: &mut Vec<String>,
) -> Result<(), String> {
    let character = log::character(log);
    let hub = Arc::new(StoreHub::new(|| {}));
    let typing = Arc::new(Mutex::new(Typing::default()));
    let mut engine = Engine::open(
        config(folder, character.as_deref()),
        Arc::clone(&hub),
        typing,
    )
    .map_err(|error| error.to_string())?;
    engine
        .start_connection()
        .map_err(|error| error.to_string())?;
    engine.connected();
    let mut paced = 0;
    for event in log::events(log) {
        let now = Timestamp(*clock);
        if paced > 0 {
            paced -= 1;
            std::thread::sleep(HOOK_CATCH_UP);
        }
        match event {
            Happened::Text(text) => {
                if text.windows(IDENTIFY.len()).any(|bytes| bytes == IDENTIFY) {
                    paced = PACED_EVENTS;
                }
                engine
                    .receive(Bytes::from(wire_text(&text)), now)
                    .map_err(|error| error.to_string())?;
            }
            Happened::Gmcp { package, json } => {
                if package == "Room.Info" {
                    *clock += ROOM_MS;
                }
                engine
                    .receive(Bytes::from(gmcp_frame(package, json)), Timestamp(*clock))
                    .map_err(|error| error.to_string())?;
            }
            Happened::Command(command) => engine.send(command, now),
        }
        engine.settle(Timestamp(*clock));
        drop(engine.take_output());
    }
    engine.close_connection(DisconnectReason::ClosedByServer, Timestamp(*clock));
    notices.extend(hub.read(
        |settings: &mudling_core::store::Versioned<mudling_core::store::SettingsState>| {
            settings.value().notices.clone()
        },
    ));
    engine.finish().map_err(|error| error.to_string())
}

fn map_rooms(rows: MapRows) -> BTreeMap<i64, Room> {
    let mut rooms: BTreeMap<i64, Room> = rows
        .rooms
        .into_iter()
        .map(|room| {
            let copy = Room {
                id: room.id,
                name: room.name,
                zone: room.area,
                description: room.description,
                environment: room.environment,
                position: (room.x, room.y, room.z),
                building: room.building,
                tangled: room.tangled,
                maze: room.maze,
                ..Room::default()
            };
            (copy.id, copy)
        })
        .collect();
    for exit in rows.exits {
        if let Some(room) = rooms.get_mut(&exit.room_id) {
            room.exits.insert(exit.direction, exit.state);
        }
    }
    for link in rows.links {
        if let Some(room) = rooms.get_mut(&link.room_id) {
            room.links.insert(link.direction, link.to_room);
        }
    }
    for doorway in rows.doorways {
        if let Some(room) = rooms.get_mut(&doorway.room_id) {
            room.doorways.insert(doorway.direction);
        }
    }
    for feature in rows.features {
        if let Some(room) = rooms.get_mut(&feature.room_id) {
            room.features.insert(feature.feature);
        }
    }
    for sighting in rows.sightings {
        if let Some(room) = rooms.get_mut(&sighting.room_id) {
            room.sightings += 1;
        }
    }
    rooms
}

/// A saved value, as text: it's JSON in its row.
fn row_text(row: &str) -> String {
    match serde_json::from_str::<Value>(row) {
        Ok(Value::String(text)) => text,
        Ok(Value::Number(number)) => match number.as_f64() {
            Some(value) if value.fract() == 0.0 => format!("{value:.0}"),
            _ => number.to_string(),
        },
        _ => row.to_owned(),
    }
}

/// A character's saved stats as the shared flat keys.
fn flat_stats(stats: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut flat = BTreeMap::new();
    for (key, value) in stats {
        if let Some(stat) = key.strip_prefix("character.") {
            match stat {
                "title" => {
                    flat.insert("title".to_owned(), value.clone());
                }
                // The prototype shows these from GMCP but keeps no record.
                "score.read" | "ac" | "alignment" | "damroll" | "hitroll" => {}
                _ => {
                    flat.insert(format!("stat.{stat}"), value.clone());
                }
            }
        }
    }
    flat
}

/// A character's `skills` row as the shared flat keys.
fn flat_skill(skill: &Skill, flat: &mut BTreeMap<String, String>) {
    let fields = [
        ("learned", skill.learned),
        ("level", skill.level),
        ("mana", skill.cost),
    ];
    for (field, value) in fields {
        if let Some(value) = value {
            flat.insert(format!("skill.{}.{field}", skill.name), value.to_string());
        }
    }
}

/// A character's `affects` row as the shared flat keys. A name the game lists
/// with two details is one row, its details on a line each.
fn flat_affect(affect: &Affect, flat: &mut BTreeMap<String, String>) {
    for detail in affect.detail.as_deref().unwrap_or_default().split('\n') {
        let key = format!("affect.{}: {detail}", affect.name);
        flat.insert(key, "on".to_owned());
    }
}

/// A character's `worn` rows as the shared flat keys, without empty slots.
fn flat_worn(stats: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut flat = BTreeMap::new();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let total = stats
        .get("worn.count")
        .and_then(|count| count.parse().ok())
        .unwrap_or(0usize);
    for index in 1..=total {
        let (Some(slot), Some(item)) = (
            stats.get(&format!("worn.{index}.slot")),
            stats.get(&format!("worn.{index}.item")),
        ) else {
            continue;
        };
        if item.is_empty() {
            continue;
        }
        let count = counts.entry(slot).or_default();
        *count += 1;
        flat.insert(format!("worn.{slot}.{count}"), plain_item(item));
    }
    flat
}

pub fn run(logs: &[PathBuf], folder: &Path) -> Result<Replay, String> {
    let servers = folder.join("servers");
    let mut clock = 0;
    let mut notices = Vec::new();
    for path in logs {
        let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let log = String::from_utf8_lossy(&bytes);
        clock += SESSION_GAP_MS;
        read_log(&log, folder, &mut clock, &mut notices)?;
    }

    let database = Database::open(&servers, HOST).map_err(|error| error.to_string())?;
    let reader = database.reader().map_err(|error| error.to_string())?;
    let mut world = World {
        rooms: map_rooms(reader.map().map_err(|error| error.to_string())?),
        ..World::default()
    };
    for mark in reader.mob_marks().map_err(|error| error.to_string())? {
        if mark.mark == "aggressive" {
            world.aggressive.insert(mark.name);
        }
    }
    let rows = reader.characters().map_err(|error| error.to_string())?;
    let mut stats: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for stat in rows.stats {
        stats
            .entry(stat.character_id)
            .or_default()
            .insert(stat.key, row_text(&stat.value));
    }
    for (character, stats) in stats {
        let mut flat = flat_stats(&stats);
        flat.extend(flat_worn(&stats));
        world.characters.insert(character, flat);
    }
    for skill in &rows.skills {
        let flat = world.characters.entry(skill.character_id.clone());
        flat_skill(skill, flat.or_default());
    }
    for affect in &rows.affects {
        let flat = world.characters.entry(affect.character_id.clone());
        flat_affect(affect, flat.or_default());
    }
    let mut readings: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for row in reader.meta().map_err(|error| error.to_string())? {
        if let Some(key) = row.key.strip_prefix(GEAR_META) {
            if key.starts_with("slot.") || key.starts_with("keywords.") {
                world.gear.insert(key.to_owned(), row_text(&row.value));
            }
        } else if let Some(key) = row.key.strip_prefix(IDENTIFY_META)
            && key != "read"
        {
            identify_field(&mut readings, key, row_text(&row.value));
        }
    }
    for (words, reading) in readings {
        world.identifies.insert(words, vec![reading]);
    }
    Ok(Replay { world, notices })
}

/// Files `<keywords>.<field>`. Keywords can hold anything but the field names.
fn identify_field(
    readings: &mut BTreeMap<String, BTreeMap<String, String>>,
    key: &str,
    value: String,
) {
    const FIELDS: [&str; 7] = [
        ".name", ".type", ".flags", ".level", ".quality", ".base.", ".added.",
    ];
    let Some(at) = FIELDS.iter().filter_map(|field| key.rfind(field)).min() else {
        return;
    };
    let (words, field) = key.split_at(at);
    if field != ".name" {
        readings
            .entry(words.to_owned())
            .or_default()
            .insert(field[1..].to_owned(), value);
    }
}
