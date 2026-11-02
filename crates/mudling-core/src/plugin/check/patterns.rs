//! Every pattern a plugin declares, with how a log can show it matching.

use crate::plugin::points::{EventRule, TableReader};
use crate::plugin::{Pattern, Plugin, Point};

/// One declared pattern.
pub struct Rule {
    /// Where it's declared: `<point>.<list>#<n>`, counting from 1, so a
    /// report finds it in the file.
    pub label: String,
    pub pattern: Pattern,
    pub read: Read,
}

pub enum Read {
    /// It caught a line if the host sent this event for that line and the
    /// pattern matches it, as a name can be shared by several rules.
    Event { point: Point, name: String },
    /// It's matched against each line, with no event to show for it.
    Line,
    /// It reads something a log's lines don't hold.
    Elsewhere(&'static str),
}

const ROOM_TITLES: &str = "reads room titles, not lines";
const ZONES: &str = "reads Room.Info's zone, not lines";

/// In the order of each point's file: `plugin.toml`'s `[points]` and each
/// list as the reference lists them.
pub fn declared(plugin: &Plugin) -> Vec<Rule> {
    let mut rules = Vec::new();
    let points = &plugin.points;
    if let Some(connection) = &points.connection {
        let logins = connection.logins.iter().map(|login| &login.pattern);
        lines(&mut rules, "connection.login", logins);
    }
    if let Some(text) = &points.text {
        lines(
            &mut rules,
            "text.pager",
            text.pager.iter().map(|p| &p.prompt),
        );
        lines(
            &mut rules,
            "text.prompt",
            text.prompts.iter().map(|p| &p.pattern),
        );
        events(&mut rules, Point::Text, "text.pattern", &text.patterns);
        lines(
            &mut rules,
            "text.color",
            text.colors.iter().map(|c| &c.pattern),
        );
        lines(
            &mut rules,
            "text.link",
            text.links.iter().map(|l| &l.pattern),
        );
    }
    if let Some(map) = &points.map {
        if let Some(area_name) = &map.area_name {
            rules.push(Rule {
                label: "map.area_name#1".to_owned(),
                pattern: area_name.clone(),
                read: Read::Elsewhere(ZONES),
            });
        }
        events(&mut rules, Point::Map, "map.exits", &map.exits);
        events(&mut rules, Point::Map, "map.failed_move", &map.failed_move);
        events(&mut rules, Point::Map, "map.darkness", &map.darkness);
        events(&mut rules, Point::Map, "map.flee", &map.flee);
    }
    if let Some(character) = &points.character {
        for reader in &character.readers {
            let place = format!("character.reader.{}", reader.name);
            table(&mut rules, Point::Character, &place, reader);
        }
        let improved = &character.skill_improved;
        events(
            &mut rules,
            Point::Character,
            "character.skill_improved",
            improved,
        );
    }
    if let Some(gear) = &points.gear {
        let readers = [
            ("identify", &gear.identify),
            ("examine", &gear.examine),
            ("equipment", &gear.equipment),
        ];
        for (name, reader) in readers {
            if let Some(reader) = reader {
                table(&mut rules, Point::Gear, &format!("gear.{name}"), reader);
            }
        }
        events(&mut rules, Point::Gear, "gear.wear", &gear.wear);
        events(&mut rules, Point::Gear, "gear.remove", &gear.remove);
        events(&mut rules, Point::Gear, "gear.put", &gear.put);
    }
    if let Some(mobs) = &points.mobs {
        events(&mut rules, Point::Mobs, "mobs.aggro", &mobs.aggro);
        for (index, rule) in mobs.room_title.iter().enumerate() {
            rules.push(Rule {
                label: format!("mobs.room_title#{}", index + 1),
                pattern: rule.pattern.clone(),
                read: Read::Elsewhere(ROOM_TITLES),
            });
        }
    }
    rules
}

fn lines<'a>(rules: &mut Vec<Rule>, list: &str, patterns: impl Iterator<Item = &'a Pattern>) {
    for (index, pattern) in patterns.enumerate() {
        rules.push(Rule {
            label: format!("{list}#{}", index + 1),
            pattern: pattern.clone(),
            read: Read::Line,
        });
    }
}

fn events(rules: &mut Vec<Rule>, point: Point, list: &str, declared: &[EventRule]) {
    for (index, rule) in declared.iter().enumerate() {
        rules.push(Rule {
            label: format!("{list}#{}", index + 1),
            pattern: rule.pattern.clone(),
            read: Read::Event {
                point,
                name: rule.name.as_str().to_owned(),
            },
        });
    }
}

fn table(rules: &mut Vec<Rule>, point: Point, place: &str, reader: &TableReader) {
    let mut add = |part: &str, patterns: &[Pattern]| {
        for (index, pattern) in patterns.iter().enumerate() {
            rules.push(Rule {
                label: format!("{place}.{part}#{}", index + 1),
                pattern: pattern.clone(),
                read: Read::Event {
                    point,
                    name: format!("{}.{part}", reader.name),
                },
            });
        }
    };
    add("start", &reader.start);
    add("row", &reader.row);
    add("end", reader.end.as_slice());
}
