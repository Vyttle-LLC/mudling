//! Debug logs and chat logs (PRD §9): the only record of play MUDling keeps,
//! both off until the player turns them on.
//!
//! - A **debug log** is what the server sent: text with its colors and GMCP,
//!   never input.
//! - A **chat log** is what the player saw, for the servers they chose.
//!
//! Neither holds anything from while the server has echo off. Both are in
//! the format [`crate::replay`] reads, so `mudling-bench <log>` plays them.
//! A debug log can't say which moves were typed, so it replays with none.
//!
//! Each connection is one log, in `logs/debug` or `logs/chat` under the data
//! folder. That folder is the player's device's alone: the repo's export
//! writes only the files it renders, and its `.gitignore` names `logs/` as a
//! second guard. Sync refuses a repo that is the data folder or holds it, so
//! nothing here is meant to be committed. A log leaves only by
//! [`Logs::export`], to a place the player picks.
//!
//! Each kind keeps its last [`MOST_LOGS`] logs or [`MOST_BYTES`], whichever
//! is less, deleting the oldest. A pinned log stays out of that count and
//! stays until it's cleared; the log being written is never deleted.
//!
//! Calls here block on the disk, so a shell makes them off its UI thread.
//! [`SessionLogs`] is what a session holds, and it never waits on the disk.

mod session;
mod writer;

pub(crate) use session::SessionLogs;

use crate::persist::Timestamp;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// The folder of logs in the data folder.
pub const LOGS_FOLDER: &str = "logs";
/// How many logs of a kind are kept, pins aside.
pub const MOST_LOGS: usize = 10;
/// How many bytes of logs of a kind are kept, pins aside. One log stops here.
pub const MOST_BYTES: u64 = 100 * 1024 * 1024;

const SETTINGS_FILE: &str = "settings.json";
const EXTENSION: &str = "log";
const PIN_EXTENSION: &str = "pinned";

/// The logs being written in this process, which nothing may delete.
static LIVE: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

fn live() -> MutexGuard<'static, BTreeSet<PathBuf>> {
    LIVE.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Debug,
    Chat,
}

impl Kind {
    pub const ALL: [Kind; 2] = [Kind::Debug, Kind::Chat];

    fn folder(self) -> &'static str {
        match self {
            Kind::Debug => "debug",
            Kind::Chat => "chat",
        }
    }

    /// The kind, as a player reads it.
    pub fn name(self) -> &'static str {
        self.folder()
    }
}

/// What the player turned on. It stays on this device, like the logs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LogSettings {
    pub debug: bool,
    /// The servers that keep chat logs, by host.
    pub chat: BTreeSet<String>,
}

impl LogSettings {
    pub fn keeps_chat(&self, host: &str) -> bool {
        self.chat.contains(host)
    }

    pub fn set_chat(&mut self, host: &str, on: bool) {
        if on {
            self.chat.insert(host.to_owned());
        } else {
            self.chat.remove(host);
        }
    }
}

/// One log on the disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogFile {
    pub kind: Kind,
    pub path: PathBuf,
    pub server: String,
    pub started: Timestamp,
    pub bytes: u64,
    pub pinned: bool,
    /// It's being written now.
    pub live: bool,
}

impl LogFile {
    /// Deletes the log and its pin. Another session pruning can get there
    /// first, and a log has no pin unless it was pinned.
    fn delete(&self) -> io::Result<()> {
        for path in [self.path.clone(), pin_of(&self.path)] {
            match fs::remove_file(path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                _ => {}
            }
        }
        Ok(())
    }

    /// The name to offer for an export.
    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// When it started, in UTC, as `2026-11-01 14:03`.
    pub fn started_text(&self) -> String {
        const MS_PER_MINUTE: i64 = 60_000;
        const MINUTES_PER_DAY: i64 = 24 * 60;
        let minutes = self.started.0.div_euclid(MS_PER_MINUTE);
        let (days, minute) = (
            minutes.div_euclid(MINUTES_PER_DAY),
            minutes.rem_euclid(MINUTES_PER_DAY),
        );
        let (year, month, day) = civil_date(days);
        format!(
            "{year}-{month:02}-{day:02} {:02}:{:02}",
            minute / 60,
            minute % 60
        )
    }
}

/// The date `days` after 1970-01-01, on the Gregorian calendar: Howard
/// Hinnant's `civil_from_days`, whose years start in March.
fn civil_date(days: i64) -> (i64, i64, i64) {
    const DAYS_PER_ERA: i64 = 146_097;
    let shifted = days + 719_468;
    let era = shifted.div_euclid(DAYS_PER_ERA);
    let day_of_era = shifted.rem_euclid(DAYS_PER_ERA);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// The logs folder of one data folder.
#[derive(Clone, Debug)]
pub struct Logs {
    folder: PathBuf,
}

impl Logs {
    pub fn in_data_folder(data_folder: &Path) -> Logs {
        Logs {
            folder: data_folder.join(LOGS_FOLDER),
        }
    }

    /// What the player turned on. Anything unreadable means off.
    pub fn settings(&self) -> LogSettings {
        fs::read(self.folder.join(SETTINGS_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save_settings(&self, settings: &LogSettings) -> io::Result<()> {
        fs::create_dir_all(&self.folder)?;
        fs::write(
            self.folder.join(SETTINGS_FILE),
            serde_json::to_vec_pretty(settings)?,
        )
    }

    /// Every log, newest first.
    pub fn list(&self) -> io::Result<Vec<LogFile>> {
        let mut logs = Vec::new();
        for kind in Kind::ALL {
            logs.extend(self.of_kind(kind)?);
        }
        logs.sort_by_key(|log| std::cmp::Reverse(log.started));
        Ok(logs)
    }

    fn of_kind(&self, kind: Kind) -> io::Result<Vec<LogFile>> {
        let folder = self.folder.join(kind.folder());
        let entries = match fs::read_dir(&folder) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            entries => entries?,
        };
        let live = live().clone();
        let mut logs = Vec::new();
        let mut pins = HashSet::new();
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|end| end == PIN_EXTENSION) {
                pins.insert(path);
                continue;
            }
            let (Some((started, server)), Ok(metadata)) = (parse_name(&path), entry.metadata())
            else {
                continue;
            };
            logs.push(LogFile {
                kind,
                started,
                server,
                bytes: metadata.len(),
                pinned: false,
                live: live.contains(&path),
                path,
            });
        }
        for log in &mut logs {
            log.pinned = pins.contains(&pin_of(&log.path));
        }
        logs.sort_by_key(|log| std::cmp::Reverse(log.started));
        Ok(logs)
    }

    /// Keeps `log` past the cap, or lets it go back under it.
    pub fn set_pinned(&self, log: &LogFile, pinned: bool) -> io::Result<()> {
        match pinned {
            true => fs::write(pin_of(&log.path), []),
            false => fs::remove_file(pin_of(&log.path)),
        }
    }

    pub fn export(&self, log: &LogFile, to: &Path) -> io::Result<()> {
        fs::copy(&log.path, to).map(drop)
    }

    /// Deletes every log of `kind`, pinned ones too. The log being written
    /// stays: turning the kind off ends it first.
    pub fn clear(&self, kind: Kind) -> io::Result<()> {
        for log in self.of_kind(kind)?.iter().filter(|log| !log.live) {
            log.delete()?;
        }
        Ok(())
    }

    /// Deletes the oldest logs past the cap.
    fn prune(&self, kind: Kind) -> io::Result<()> {
        let (mut kept, mut bytes) = (0, 0);
        for log in self.of_kind(kind)?.iter().filter(|log| !log.pinned) {
            kept += 1;
            bytes += log.bytes;
            if !log.live && (kept > MOST_LOGS || bytes > MOST_BYTES) {
                log.delete()?;
            }
        }
        Ok(())
    }

    fn new_path(&self, kind: Kind, host: &str, started: Timestamp) -> PathBuf {
        let host: String = host
            .chars()
            .map(|c| match c {
                'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' => c,
                _ => '_',
            })
            .collect();
        let name = format!("{}-{host}.{EXTENSION}", started.0);
        self.folder.join(kind.folder()).join(name)
    }
}

fn pin_of(log: &Path) -> PathBuf {
    let mut name = log.as_os_str().to_owned();
    name.push(".");
    name.push(PIN_EXTENSION);
    PathBuf::from(name)
}

/// A log's name holds when it started and for which server.
fn parse_name(path: &Path) -> Option<(Timestamp, String)> {
    if path.extension()? != EXTENSION {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    let (started, server) = stem.split_once('-')?;
    Some((Timestamp(started.parse().ok()?), server.to_owned()))
}

#[cfg(test)]
mod tests;
