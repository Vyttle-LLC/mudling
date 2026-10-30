//! Picks a room's description out of the text the game prints on arrival:
//! the room's name, then its exits line, then the description, just before
//! the Room.Info. The description's first line tells apart rooms that share
//! a name.

use super::rules::MapRules;
use std::collections::VecDeque;

/// Lines kept while waiting for a Room.Info.
const KEPT_LINES: usize = 40;

#[derive(Clone, Debug, Default)]
pub(super) struct RoomText {
    /// Recent lines, trimmed, newest last.
    recent: VecDeque<String>,
}

impl RoomText {
    pub fn read(&mut self, line: &str) {
        if self.recent.len() == KEPT_LINES {
            self.recent.pop_front();
        }
        self.recent.push_back(line.trim().to_owned());
    }

    /// The first description line printed for the room just entered, if
    /// its name and exits lines were seen; then starts fresh. The name line
    /// may carry tags after the name ("A cave (Water)") or a prompt before
    /// it.
    pub fn take_description(&mut self, name: &str, rules: &dyn MapRules) -> String {
        let recent = std::mem::take(&mut self.recent);
        let Some(exits) = recent.iter().rposition(|line| rules.exits_line(line)) else {
            return String::new();
        };
        let Some(name_line) = recent.range(..exits).rev().find(|line| !line.is_empty()) else {
            return String::new();
        };
        let names_room = name_line.ends_with(name)
            || name_line
                .find(name)
                .is_some_and(|start| name_line[start + name.len()..].starts_with(" ("));
        if !names_room || name.is_empty() {
            return String::new();
        }
        recent
            .range(exits + 1..)
            .find(|line| !line.is_empty())
            .cloned()
            .unwrap_or_default()
    }
}
