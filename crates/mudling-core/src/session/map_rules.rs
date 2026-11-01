//! The map's [`MapRules`] from the enabled plugins' Map and Mobs points.
//! What their rules find in lines reaches the map as events, from the merged
//! matcher; this is what the map asks about GMCP values and room titles.

use crate::map::{AreaName, MapRules, MobNamer, RoomIdentity};
use crate::plugin::PluginHost;
use crate::plugin::points::{self, Maze, Pattern};
use std::collections::BTreeMap;
use std::sync::Arc;

/// The Map and Mobs declarations of a set of plugins, the highest layer's
/// where only one value applies.
pub(super) struct PluginRules {
    identity: RoomIdentity,
    areas: BTreeMap<String, String>,
    area_name: Option<Pattern>,
    mazes: Vec<Maze>,
    /// Highest layer first: the first to name a title wins.
    room_titles: Arc<[Pattern]>,
}

impl PluginRules {
    pub(super) fn of(host: &PluginHost) -> PluginRules {
        let mut rules = PluginRules {
            identity: match host.settings().room_identity {
                Some(points::RoomIdentity::Description) => RoomIdentity::Text,
                Some(points::RoomIdentity::Gmcp) | None => RoomIdentity::Number,
            },
            areas: BTreeMap::new(),
            area_name: host.settings().area_name.clone(),
            mazes: Vec::new(),
            room_titles: room_titles(host),
        };
        for (_, map) in host.declared(|points| points.map.as_ref()) {
            rules
                .areas
                .extend(map.areas.iter().map(|(k, v)| (k.clone(), v.clone())));
            rules.mazes.extend(map.mazes.iter().cloned());
        }
        rules
    }

    /// The zone read by the plugins' pattern, if it has a `name`.
    fn matched_area(&self, zone: &str) -> Option<AreaName> {
        let pattern = self.area_name.as_ref()?;
        let captures = pattern.captures(zone)?;
        let text = |name: &str| {
            let capture = captures.iter().find(|capture| &*capture.name == name)?;
            Some(capture.text(zone))
        };
        let levels = text("low").zip(text("high"));
        Some(AreaName {
            name: text("name")?.to_owned(),
            levels: levels.map(|(low, high)| format!("{low}-{high}")),
        })
    }
}

fn room_titles(host: &PluginHost) -> Arc<[Pattern]> {
    let rules: Vec<_> = host.declared(|points| points.mobs.as_ref()).collect();
    rules
        .into_iter()
        .rev()
        .flat_map(|(_, mobs)| &mobs.room_title)
        .map(|rule| rule.pattern.clone())
        .collect()
}

/// The text `pattern` captures as `name` in `title`.
fn named(pattern: &Pattern, title: &str) -> Option<String> {
    let captures = pattern.captures(title)?;
    let name = captures.iter().find(|capture| &*capture.name == "name")?;
    Some(name.text(title).to_owned())
}

impl MapRules for PluginRules {
    fn identity(&self) -> RoomIdentity {
        self.identity
    }

    fn mob_namer(&self) -> MobNamer {
        if self.room_titles.is_empty() {
            return MobNamer::default();
        }
        let rules = Arc::clone(&self.room_titles);
        MobNamer::new(move |title| rules.iter().find_map(|rule| named(rule, title)))
    }

    fn area_name(&self, zone: &str) -> AreaName {
        let zone = zone.trim();
        if let Some(name) = self.areas.get(zone) {
            return AreaName {
                name: name.clone(),
                levels: None,
            };
        }
        self.matched_area(zone).unwrap_or_else(|| AreaName {
            name: zone.to_owned(),
            levels: None,
        })
    }

    fn maze_room(&self, zone: &str, room: &str) -> bool {
        let area = self.area_name(zone).name;
        self.mazes.iter().any(|maze| {
            maze.area == area
                && (maze.rooms.is_empty() || maze.rooms.iter().any(|name| name == room))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::{LiveMap, Mark};
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
        assert_eq!(
            rules.area_name("[5-10] The Barrows"),
            AreaName {
                name: "The Barrows".to_owned(),
                levels: Some("5-10".to_owned())
            }
        );
        assert!(rules.maze_room("7", "Any room"));
        assert!(rules.maze_room("Barrow Field", "The Mist"));
        assert!(!rules.maze_room("Barrow Field", "The Gate"));
        assert!(!rules.maze_room("Fields", "Any room"));
    }

    #[test]
    fn a_plugins_room_title_rule_names_the_maps_mobs() {
        let mut map = LiveMap::new(Box::new(testland()));
        let mobs = map.mobs_mut();
        assert_eq!(mobs.name("The lair of  Old Bob"), "old bob");
        assert_eq!(
            mobs.name("A wolf is here, howling."),
            "a wolf",
            "a title the rule doesn't read"
        );

        mobs.set_gear("The lair of Old Bob", true);
        assert!(mobs.marks(Mark::Gear).contains("old bob"));
        assert!(mobs.is_gear("The lair of OLD BOB"));
    }
}
