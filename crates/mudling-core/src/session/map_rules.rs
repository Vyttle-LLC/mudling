//! The map's [`MapRules`] from the enabled plugins' Map and Mobs points.
//!
//! Until the host's merged matcher runs every point, each rule here is its
//! own regex over each line. Plugins with a few such rules cost little;
//! real ones read these lines in their Text point, and arrive as events.

use crate::map::{AreaName, Direction, MapRules, RoomIdentity};
use crate::plugin::points::{self, EventRule, Pattern};
use crate::plugin::{Capture, PluginHost};
use std::collections::BTreeMap;

/// The Map and Mobs declarations of a set of plugins, the highest layer's
/// where only one value applies.
pub(super) struct PluginRules {
    identity: RoomIdentity,
    areas: BTreeMap<String, String>,
    mazes: Vec<String>,
    failed_moves: Vec<Pattern>,
    flees: Vec<Pattern>,
    darkness: Vec<Pattern>,
    exits: Vec<Pattern>,
    aggressors: Vec<Pattern>,
}

impl PluginRules {
    pub(super) fn of(host: &PluginHost) -> PluginRules {
        let mut rules = PluginRules {
            identity: match host.settings().room_identity {
                Some(points::RoomIdentity::Description) => RoomIdentity::Text,
                Some(points::RoomIdentity::Gmcp) | None => RoomIdentity::Number,
            },
            areas: BTreeMap::new(),
            mazes: Vec::new(),
            failed_moves: Vec::new(),
            flees: Vec::new(),
            darkness: Vec::new(),
            exits: Vec::new(),
            aggressors: Vec::new(),
        };
        let patterns = |rules: &[EventRule]| {
            rules
                .iter()
                .map(|rule| rule.pattern.clone())
                .collect::<Vec<_>>()
        };
        for (_, map) in host.declared(|points| points.map.as_ref()) {
            rules
                .areas
                .extend(map.areas.iter().map(|(k, v)| (k.clone(), v.clone())));
            rules.mazes.extend(map.mazes.iter().cloned());
            rules.failed_moves.extend(patterns(&map.failed_move));
            rules.flees.extend(patterns(&map.flee));
            rules.darkness.extend(patterns(&map.darkness));
            rules.exits.extend(patterns(&map.exits));
        }
        for (_, mobs) in host.declared(|points| points.mobs.as_ref()) {
            rules.aggressors.extend(patterns(&mobs.aggro));
        }
        rules
    }
}

fn any_match(patterns: &[Pattern], line: &str) -> bool {
    patterns.iter().any(|pattern| pattern.is_match(line))
}

/// The text of the capture called `name`, or of the first capture.
fn captured(pattern: &Pattern, line: &str, name: &str) -> Option<String> {
    let captures = pattern.captures(line)?;
    let text = |capture: &Capture| line[capture.start as usize..capture.end as usize].to_owned();
    captures
        .iter()
        .find(|capture| &*capture.name == name)
        .or(captures.first())
        .map(text)
}

impl MapRules for PluginRules {
    fn identity(&self) -> RoomIdentity {
        self.identity
    }

    fn area_name(&self, zone: &str) -> AreaName {
        let zone = zone.trim();
        AreaName {
            name: self.areas.get(zone).map_or(zone, String::as_str).to_owned(),
            levels: None,
        }
    }

    fn failed_move(&self, line: &str) -> bool {
        any_match(&self.failed_moves, line)
    }

    fn fled(&self, line: &str) -> Option<Direction> {
        self.flees
            .iter()
            .find_map(|pattern| Direction::parse(&captured(pattern, line, "dir")?))
    }

    fn darkness(&self, line: &str) -> bool {
        any_match(&self.darkness, line)
    }

    fn maze_area(&self, zone: &str) -> bool {
        let name = self.area_name(zone).name;
        self.mazes.contains(&name)
    }

    fn aggressor(&self, line: &str) -> Option<String> {
        self.aggressors
            .iter()
            .find_map(|pattern| captured(pattern, line, "name"))
    }

    fn exits_line(&self, line: &str) -> bool {
        any_match(&self.exits, line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::Plugin;
    use std::path::PathBuf;

    fn testland() -> PluginRules {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("plugins")
            .join("testland");
        let plugin = Plugin::load(dir).expect("the fixture loads");
        PluginRules::of(&PluginHost::new([plugin]).expect("it compiles"))
    }

    #[test]
    fn a_plugins_points_become_map_rules() {
        let rules = testland();
        assert_eq!(rules.identity(), RoomIdentity::Number);
        assert_eq!(rules.area_name("7").name, "The Hedge");
        assert_eq!(rules.area_name(" Fields ").name, "Fields");
        assert!(rules.maze_area("7"));
        assert!(!rules.maze_area("Fields"));
        assert!(rules.failed_move("You can't go that way."));
        assert!(!rules.failed_move("You go north."));
        assert_eq!(
            rules.aggressor("A hedge wolf lunges at you!").as_deref(),
            Some("A hedge wolf")
        );
        assert_eq!(rules.aggressor("A wolf watches you."), None);
    }
}
