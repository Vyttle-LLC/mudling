//! What GMCP says about the game in play: who the character is, who it's
//! fighting, and who and what is in the room. Kept for the connection and
//! never saved.
//!
//! The character and the fight are read where the HUD points' [`GameFields`]
//! say, so no game's names for them are here.
//!
//! `Room.Players` lists players by name and mobs by a number. `Char.Items`
//! with location `room` lists what's on the ground. Neither gives the word
//! the game takes in a command, so that's worked out from the name shown,
//! and only letters of it ever reach a [`Occupant::target`] or
//! [`Item::target`]: a name from the server can't carry a command with it.

use super::inventory::{self, Change, List};
use crate::ansi;
use crate::defaults::hud::{Field, GameFields};
use crate::map::mob_name;
use crate::plugin::luau::Value;
use crate::plugin::points::OpponentField;
use crate::store::{
    CHARACTER_LEVEL, CHARACTER_NAME, CHARACTER_POSITION, Item, Occupant, RoomContents, Target,
    TargetState,
};
use crate::telnet::gmcp;
use serde::Deserialize;
use serde_json::{Map, Value as Json};
use std::collections::BTreeMap;

const ROOM: &str = "room";
const ARTICLES: [&str; 4] = ["a", "an", "the", "some"];
/// Words that end the noun phrase opening a mob's line.
const PHRASE_ENDS: [&str; 7] = ["is", "are", "of", "here", "who", "that", "with"];
/// Words that follow the noun in an item's name.
const ITEM_NOUN_ENDS: [&str; 3] = ["of", "that", "with"];
/// What a sheet key is filed under in the characters store.
const SHEET: &str = "character.";

#[derive(Deserialize)]
struct RoomPlayer {
    name: Option<String>,
    fullname: Option<String>,
}

/// Someone the server listed in the room.
#[derive(Debug)]
struct Present {
    name: String,
    /// The line the game shows for them.
    title: String,
    /// What to target them by: worked out once, as the room's contents are
    /// published after every change to it.
    keyword: Option<String>,
}

impl Present {
    fn of(key: String, player: RoomPlayer) -> Present {
        let name = player.name.unwrap_or(key);
        let title = ansi::plain(player.fullname.as_deref().unwrap_or(&name));
        let mut present = Present {
            name,
            title,
            keyword: None,
        };
        present.keyword = present.target_keyword();
        present
    }

    /// Mobs go by a number, players by name.
    fn is_mob(&self) -> bool {
        !self.name.is_empty() && self.name.bytes().all(|byte| byte.is_ascii_digit())
    }

    fn target_keyword(&self) -> Option<String> {
        if self.is_mob() {
            mob_keyword(&self.title)
        } else {
            letters(&self.name)
        }
    }
}

/// What one GMCP message told us, for the engine to pass on.
#[derive(Debug, Default)]
pub(super) struct Heard {
    /// Values for the characters store.
    pub(super) character: BTreeMap<String, Value>,
    /// The fight's new state, if the message touched it.
    pub(super) target: Option<TargetState>,
    /// The room lines of mobs just seen in the room.
    pub(super) mobs: Vec<String>,
    /// The room's contents changed.
    pub(super) contents: bool,
}

#[derive(Debug, Default)]
pub(super) struct GameFeed {
    /// By the server's key for each.
    present: BTreeMap<String, Present>,
    items: Vec<inventory::Item>,
    position: Option<String>,
    target: Option<Target>,
}

impl GameFeed {
    /// What the character is doing as the game words it, once it's said.
    pub(super) fn position(&self) -> Option<&str> {
        self.position.as_deref()
    }

    pub(super) fn in_fight(&self) -> bool {
        self.target.is_some()
    }

    pub(super) fn read(&mut self, package: &str, json: &str, fields: &GameFields) -> Heard {
        let is = |name: &str| package.eq_ignore_ascii_case(name);
        let mut heard = Heard::default();
        if fields.read_from(package)
            && let Some(message) = gmcp::from_json::<Map<String, Json>>(json)
        {
            // What this message holds at a field, if it's the field's message.
            let value = |field: &Field| {
                let here = field.package.eq_ignore_ascii_case(package);
                here.then(|| message.get(&field.key)).flatten()
            };
            self.read_character(&fields.character, value, &mut heard);
            self.read_opponent(&fields.opponent, value, &mut heard);
        }
        if is("Room.Info") {
            // What's listed next is the new room's.
            self.present.clear();
            self.items.clear();
            heard.contents = true;
        } else if is("Room.Players") {
            self.present.clear();
            self.add_players(json, &mut heard);
            heard.contents = true;
        } else if is("Room.AddPlayer") {
            self.add_players(json, &mut heard);
        } else if is("Room.RemovePlayer") {
            let gone: Option<String> = gmcp::from_json(json);
            heard.contents = gone.is_some_and(|key| self.present.remove(&key).is_some());
        } else if is("Char.Items.List") {
            if let Some(list) = gmcp::from_json::<List>(json).filter(|list| list.location == ROOM) {
                self.items = list.items;
                heard.contents = true;
            }
        } else if is("Char.Items.Add") {
            if let Some(added) = room_change(json) {
                self.items.retain(|item| item.id != added.item.id);
                self.items.push(added.item);
                heard.contents = true;
            }
        } else if is("Char.Items.Remove")
            && let Some(removed) = room_change(json)
        {
            self.items.retain(|item| item.id != removed.item.id);
            heard.contents = true;
        }
        heard
    }

    /// What's in the room, as shown: `character` isn't among those there, and
    /// copies of an item are one row.
    pub(super) fn contents(&self, character: Option<&str>) -> RoomContents {
        let is_you = |present: &Present| {
            character.is_some_and(|name| present.name.eq_ignore_ascii_case(name))
        };
        let occupants = self
            .present
            .values()
            .filter(|present| !is_you(present))
            .map(|present| Occupant {
                name: present.title.clone(),
                target: present.keyword.clone(),
            })
            .collect();
        let mut items: Vec<Item> = Vec::new();
        for item in &self.items {
            if let Some(same) = items.iter_mut().find(|same| same.name == item.name) {
                same.count += 1;
            } else if let Some(target) = item_keyword(&item.name) {
                items.push(Item {
                    name: item.name.clone(),
                    target,
                    count: 1,
                });
            }
        }
        RoomContents { occupants, items }
    }

    fn add_players(&mut self, json: &str, heard: &mut Heard) {
        let Some(players) = gmcp::from_json::<BTreeMap<String, RoomPlayer>>(json) else {
            return;
        };
        for (key, player) in players {
            let present = Present::of(key.clone(), player);
            if present.is_mob() {
                heard.mobs.push(present.title.clone());
            }
            self.present.insert(key, present);
            heard.contents = true;
        }
    }

    /// The sheet's values a message holds. A game may send only what changed.
    fn read_character<'a>(
        &mut self,
        fields: &BTreeMap<String, Field>,
        value: impl Fn(&Field) -> Option<&'a Json>,
        heard: &mut Heard,
    ) {
        for (key, field) in fields {
            let Some(value) = value(field) else { continue };
            let filed = format!("{SHEET}{key}");
            if filed == CHARACTER_NAME {
                let Some(name) = value.as_str().map(ansi::plain) else {
                    continue;
                };
                if !name.is_empty() {
                    heard.character.insert(filed, Value::Text(name));
                    heard.contents = true;
                }
            } else if filed == CHARACTER_LEVEL {
                if let Some(level) = number(Some(value)) {
                    heard.character.insert(filed, Value::Number(level as f64));
                }
            } else if filed == CHARACTER_POSITION {
                let Some(position) = value.as_str() else {
                    continue;
                };
                if self.position.as_deref() != Some(position) {
                    heard
                        .character
                        .insert(filed, Value::Text(position.to_owned()));
                    self.position = Some(position.to_owned());
                }
            } else if let Some(value) = sheet_value(value) {
                heard.character.insert(filed, value);
            }
        }
    }

    /// The fight's state a message holds. A game may send only what changed,
    /// so a switch of opponent keeps what the message leaves out.
    fn read_opponent<'a>(
        &mut self,
        fields: &BTreeMap<OpponentField, Field>,
        value: impl Fn(&Field) -> Option<&'a Json>,
        heard: &mut Heard,
    ) {
        let sent = |which: OpponentField| {
            let value = fields.get(&which).and_then(&value);
            value.filter(|value| !value.is_null())
        };
        let level = number(sent(OpponentField::Level)).filter(|level| *level > 0);
        let percent = number(sent(OpponentField::Percent));
        let health = number(sent(OpponentField::Health)).or(percent);
        let max_health = number(sent(OpponentField::MaxHealth))
            .or(percent.map(|_| 100))
            .filter(|max| *max > 0);
        let name = sent(OpponentField::Name).and_then(Json::as_str);
        match name.map(ansi::plain) {
            Some(name) if name.is_empty() => self.target = None,
            Some(name) => self.target.get_or_insert_default().name = name,
            None => {}
        }
        if let Some(target) = &mut self.target {
            target.level = level.or(target.level);
            target.health = health.or(target.health);
            target.max_health = max_health.or(target.max_health);
        }
        if fields.keys().any(|which| sent(*which).is_some()) {
            heard.target = Some(TargetState(self.target.clone()));
        }
    }
}

fn room_change(json: &str) -> Option<Change> {
    gmcp::from_json::<Change>(json).filter(|change| change.location == ROOM)
}

/// A field as the sheet keeps it: a number if it is one.
fn sheet_value(value: &Json) -> Option<Value> {
    if let Some(number) = number(Some(value)) {
        return Some(Value::Number(number as f64));
    }
    let text = ansi::plain(value.as_str()?);
    (!text.is_empty()).then_some(Value::Text(text))
}

/// Games send numbers as numbers or as text.
fn number(value: Option<&Json>) -> Option<i64> {
    match value? {
        Json::Number(number) => number.as_i64(),
        Json::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

/// The first run of letters in `word`, lowercase: all a target ever holds.
fn letters(word: &str) -> Option<String> {
    word.split(|c: char| !c.is_alphabetic())
        .find(|run| !run.is_empty())
        .map(str::to_lowercase)
}

/// The word to target a mob by, from its room line: the last word of the
/// noun phrase it opens with, which ends at the first verb, "of", or a comma
/// that closes more than adjectives. "A grey wolf looks at you" → "wolf". A
/// guess by nature, as Diku-style games name mobs freely.
fn mob_keyword(title: &str) -> Option<String> {
    let line = mob_name(title);
    let mut phrase: Vec<&str> = Vec::new();
    for clause in line.split(',') {
        let words = clause
            .split(|c: char| !c.is_alphabetic() && c != '\'')
            .filter(|word| !word.is_empty());
        let mut ended = false;
        for word in words {
            if PHRASE_ENDS.contains(&word) || (!phrase.is_empty() && looks_like_verb(word)) {
                ended = true;
                break;
            }
            phrase.push(word);
        }
        // "An old, wrinkled nanny": a comma after one adjective goes on.
        let nouns = phrase.iter().filter(|word| !ARTICLES.contains(word));
        if ended || nouns.count() > 1 {
            break;
        }
    }
    let noun = phrase.iter().rev().find(|word| !ARTICLES.contains(word))?;
    letters(noun)
}

/// A present-tense verb such as "sits". Plural nouns rarely open a mob's
/// line, so the trade is a fair one.
fn looks_like_verb(word: &str) -> bool {
    word.chars().count() > 3 && word.ends_with('s') && !word.ends_with("ss") && !word.contains('\'')
}

/// The word to target an item by, from its name: "a silver ring" → "ring",
/// "corpse of a wolf" → "corpse".
fn item_keyword(name: &str) -> Option<String> {
    let name = mob_name(name);
    let words: Vec<&str> = name
        .split(|c: char| !c.is_alphabetic())
        .filter(|word| !word.is_empty())
        .collect();
    let noun_end = words
        .iter()
        .position(|word| ITEM_NOUN_ENDS.contains(word))
        .filter(|end| *end > 0);
    let noun = match noun_end {
        Some(end) => words.get(end - 1),
        None => words.iter().rev().find(|word| !ARTICLES.contains(word)),
    };
    noun.map(|word| (*word).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ESC: char = '\u{1b}';

    fn feed(messages: &[(&str, &str)]) -> GameFeed {
        let mut feed = GameFeed::default();
        for (package, json) in messages {
            feed.hear(package, json);
        }
        feed
    }

    /// A game's fields as its HUD point names them, over the defaults'.
    const HUD: &str = r#"
        [character]
        name = "Char.Status.character_name"
        position = "Char.Vitals.position"
        race = "Char.Status.race"
        hitroll = "Char.Status.hitroll"
        ac = "Char.Status.ac"
        gold = "Char.Status.gold"
        [opponent]
        name = "Char.Status.opponent_name"
        level = "Char.Status.opponent_level"
        health = "Char.Status.opponent_health"
        max_health = "Char.Status.opponent_health_max"
    "#;

    impl GameFeed {
        fn hear(&mut self, package: &str, json: &str) -> Heard {
            let defaults = crate::defaults::plugin().points.hud.unwrap();
            let game = toml::from_str(HUD).unwrap();
            self.read(package, json, &GameFields::from_huds([&defaults, &game]))
        }
    }

    fn targets(contents: &RoomContents) -> Vec<&str> {
        contents
            .occupants
            .iter()
            .map(|occupant| occupant.target.as_deref().unwrap_or_default())
            .collect()
    }

    #[test]
    fn a_mob_with_no_keyword_still_stands_in_the_room() {
        let players = r#"{"1": {"name": "1", "fullname": "A humongeous bear towers over you."}}"#;
        let contents = feed(&[("Room.Players", players)]).contents(None);
        assert_eq!(
            contents.occupants,
            [Occupant {
                name: "A humongeous bear towers over you.".to_owned(),
                target: None
            }]
        );
    }

    #[test]
    fn players_and_mobs_fill_the_room_and_the_character_is_left_out() {
        let players = r#"{
            "Bren": {"name": "Bren", "fullname": "Bren the Tall\u001b[0;36m is here.", "race": "Gnome"},
            "Mira": {"name": "Mira", "fullname": "Mira the Quick is here."},
            "111": {"name": "111", "fullname": "(Glowing) A grey wolf sits here."}
        }"#;
        let mut feed = feed(&[("Room.Players", players)]);
        assert_eq!(targets(&feed.contents(None)), ["wolf", "bren", "mira"]);

        let heard = feed.hear("Char.Status", r#"{"character_name": "Bren"}"#);
        assert!(heard.contents, "the list changes with the name");
        let contents = feed.contents(Some("bren"));
        assert_eq!(targets(&contents), ["wolf", "mira"]);
        assert_eq!(
            contents.occupants[0].name,
            "(Glowing) A grey wolf sits here."
        );
    }

    #[test]
    fn a_mob_is_seen_when_it_comes_and_gone_when_it_leaves() {
        let mut feed = GameFeed::default();
        let bear = r#"{"222": {"name": "222", "fullname": "A brown bear stands here."}}"#;
        let heard = feed.hear("Room.AddPlayer", bear);
        assert_eq!(heard.mobs, ["A brown bear stands here."]);
        assert!(heard.contents);
        assert_eq!(targets(&feed.contents(None)), ["bear"]);

        assert!(feed.hear("Room.RemovePlayer", r#""222""#).contents);
        assert!(feed.contents(None).occupants.is_empty());
        assert!(!feed.hear("Room.RemovePlayer", r#""222""#).contents);
    }

    #[test]
    fn players_are_no_sightings() {
        let mut feed = GameFeed::default();
        let heard = feed.hear(
            "Room.Players",
            r#"{"Mira": {"name": "Mira", "fullname": "Mira is here."}}"#,
        );
        assert!(heard.mobs.is_empty());
    }

    #[test]
    fn items_on_the_ground_come_and_go_and_copies_are_one_row() {
        let list = r#"{"location": "room", "items": [
            {"id": "1", "name": "a silver ring", "type": "armor"},
            {"id": "2", "name": "a silver ring", "type": "armor"},
            {"id": "3", "name": "corpse of a wolf", "type": "npc corpse"}]}"#;
        let mut feed = feed(&[("Char.Items.List", list)]);
        let items = feed.contents(None).items;
        assert_eq!(items.len(), 2);
        assert_eq!((items[0].target.as_str(), items[0].count), ("ring", 2));
        assert_eq!((items[1].target.as_str(), items[1].count), ("corpse", 1));

        let carried = r#"{"location": "inv", "item": {"id": "9", "name": "a rope"}}"#;
        assert!(!feed.hear("Char.Items.Add", carried).contents);
        let gone = r#"{"location": "room", "item": {"id": "1", "name": "a silver ring"}}"#;
        assert!(feed.hear("Char.Items.Remove", gone).contents);
        assert_eq!(feed.contents(None).items[0].count, 1);
        let added = r#"{"location": "room", "item": {"id": "4", "name": "a portal"}}"#;
        feed.hear("Char.Items.Add", added);
        assert_eq!(feed.contents(None).items[2].target, "portal");
    }

    #[test]
    fn a_new_room_starts_empty() {
        let mut feed = feed(&[
            (
                "Room.Players",
                r#"{"111": {"name": "111", "fullname": "A rat is here."}}"#,
            ),
            (
                "Char.Items.List",
                r#"{"location": "room", "items": [{"id": "1", "name": "a lamp"}]}"#,
            ),
        ]);
        assert!(feed.hear("Room.Info", "{}").contents);
        assert_eq!(feed.contents(None), RoomContents::default());
    }

    /// A name from the server is shown, but no part of it but letters can
    /// reach a command.
    #[test]
    fn a_name_from_the_server_cannot_smuggle_a_command() {
        let colored = format!("{ESC}[1;31ma blade{ESC}[0m;quit");
        let players = r#"{"1": {"name": "1", "fullname": "A rat;quit\ndelete is here."},
              "Bob;quit": {"name": "Bob;quit", "fullname": "Bob"},
              "Eve|x": {"name": "Eve|x", "fullname": "Eve"}}"#;
        let items = format!(
            "{{\"location\": \"room\", \"items\": [{{\"id\": \"1\", \"name\": \"{colored}\"}}]}}"
        );
        let feed = feed(&[("Room.Players", players), ("Char.Items.List", &items)]);
        let contents = feed.contents(None);
        assert_eq!(targets(&contents), ["quitdelete", "bob", "eve"]);
        let item = &contents.items[0];
        assert_eq!(item.name, "a blade;quit");
        assert_eq!(item.target, "quit");
        for target in contents
            .occupants
            .iter()
            .filter_map(|o| o.target.as_ref())
            .chain([&item.target])
        {
            assert!(target.chars().all(char::is_alphabetic), "{target}");
        }
    }

    #[test]
    fn a_name_with_nothing_to_target_is_left_out() {
        let items = r#"{"location": "room", "items": [{"id": "1", "name": "123"}]}"#;
        assert!(
            feed(&[("Char.Items.List", items)])
                .contents(None)
                .items
                .is_empty()
        );
    }

    #[test]
    fn a_mob_goes_by_the_noun_its_line_opens_with() {
        for (title, word) in [
            ("A grey wolf looks at you and sighs.", "wolf"),
            ("(Flying) (Glowing) The tall guard stands here.", "guard"),
            ("An old, wrinkled nanny is here.", "nanny"),
            ("A statue of a king is here.", "statue"),
            ("The miller's boy sits by the door.", "boy"),
            ("The miller's is here.", "miller"),
        ] {
            assert_eq!(mob_keyword(title).as_deref(), Some(word), "{title}");
        }
    }

    #[test]
    fn an_item_goes_by_its_noun() {
        for (name, word) in [
            ("a silver ring", "ring"),
            ("corpse of a wolf", "corpse"),
            ("a belt that says, hello", "belt"),
            ("5 standard arrows", "arrows"),
            ("(Glowing) a sword with a story", "sword"),
        ] {
            assert_eq!(item_keyword(name).as_deref(), Some(word), "{name}");
        }
    }

    #[test]
    fn the_character_comes_from_status_and_vitals() {
        let mut feed = GameFeed::default();
        let heard = feed.hear(
            "Char.Status",
            r#"{"character_name": "Bren", "level": "22"}"#,
        );
        assert_eq!(
            heard.character[CHARACTER_NAME],
            Value::Text("Bren".to_owned())
        );
        assert_eq!(heard.character[CHARACTER_LEVEL], Value::Number(22.0));

        let heard = feed.hear("Char.Status", r#"{"level": 23, "affects": null}"#);
        assert_eq!(heard.character[CHARACTER_LEVEL], Value::Number(23.0));

        let stand = r#"{"hp": "30", "position": "Stand", "monhp": null}"#;
        let heard = feed.hear("Char.Vitals", stand);
        assert_eq!(
            heard.character[CHARACTER_POSITION],
            Value::Text("Stand".to_owned())
        );
        assert!(feed.hear("Char.Vitals", stand).character.is_empty());
    }

    #[test]
    fn status_fields_for_the_sheet_keep_their_type() {
        let mut feed = GameFeed::default();
        let heard = feed.hear(
            "Char.Status",
            r#"{"hitroll": "12", "ac": -4, "race": "Harpy", "gold": null, "room_name": "A hall"}"#,
        );
        assert_eq!(heard.character["character.hitroll"], Value::Number(12.));
        assert_eq!(heard.character["character.ac"], Value::Number(-4.));
        assert_eq!(
            heard.character["character.race"],
            Value::Text("Harpy".to_owned())
        );
        assert_eq!(heard.character.len(), 3);
    }

    #[test]
    fn a_fight_starts_goes_on_and_ends_in_status_diffs() {
        let mut feed = GameFeed::default();
        assert!(
            feed.hear("Char.Status", r#"{"gold": "3"}"#)
                .target
                .is_none()
        );
        let heard = feed.hear(
            "Char.Status",
            r#"{"opponent_name": "A Trog", "opponent_level": "15",
                "opponent_health": "100", "opponent_health_max": "100"}"#,
        );
        let target = heard.target.unwrap().0.unwrap();
        assert_eq!(target.name, "A Trog");
        assert_eq!(target.level, Some(15));

        let heard = feed.hear("Char.Status", r#"{"opponent_health": "36"}"#);
        let target = heard.target.unwrap().0.unwrap();
        assert_eq!((target.name.as_str(), target.health), ("A Trog", Some(36)));
        assert_eq!(target.max_health, Some(100));

        let heard = feed.hear(
            "Char.Status",
            r#"{"opponent_name": "A Trog Child", "opponent_health": "90"}"#,
        );
        let target = heard.target.unwrap().0.unwrap();
        assert_eq!(
            (target.name.as_str(), target.health),
            ("A Trog Child", Some(90))
        );
        assert_eq!(target.max_health, Some(100), "unchanged, so left out");
        assert_eq!(target.level, Some(15), "unchanged, so left out");

        let heard = feed.hear(
            "Char.Status",
            r#"{"opponent_name": "", "opponent_level": "0",
                "opponent_health": "0", "opponent_health_max": "0"}"#,
        );
        assert_eq!(heard.target, Some(TargetState(None)));
        let heard = feed.hear("Char.Status", r#"{"opponent_health": "5"}"#);
        assert_eq!(heard.target, Some(TargetState(None)), "no fight, no target");

        let heard = feed.hear("Char.Status", r#"{"opponent_name": "A Trog"}"#);
        let target = heard.target.unwrap().0.unwrap();
        assert_eq!(target.max_health, None, "a new fight starts clean");
    }

    #[test]
    fn with_no_game_plugin_only_the_shared_fields_are_read() {
        let defaults = crate::defaults::plugin().points.hud.unwrap();
        let fields = GameFields::from_huds([&defaults]);
        let mut feed = GameFeed::default();
        let status =
            r#"{"character_name": "Bren", "level": 3, "str": 9, "opponent_name": "A rat"}"#;
        let heard = feed.read("Char.Status", status, &fields);
        assert_eq!(Vec::from_iter(heard.character.keys()), [CHARACTER_LEVEL]);
        assert_eq!(heard.target, None);

        let heard = feed.read("Char.Name", r#"{"name": "Bren"}"#, &fields);
        let named = heard.character.get(CHARACTER_NAME);
        assert_eq!(named, Some(&Value::Text("Bren".to_owned())));
    }

    #[test]
    fn a_percentage_is_health_out_of_a_hundred() {
        let hud = "[opponent]\nname = \"char.status.enemy\"\npercent = \"char.status.enemypct\"";
        let fields = GameFields::from_huds([&toml::from_str(hud).unwrap()]);
        let mut feed = GameFeed::default();
        let heard = feed.read(
            "char.status",
            r#"{"enemy": "an owl", "enemypct": 93}"#,
            &fields,
        );
        let target = heard.target.unwrap().0.unwrap();
        assert_eq!((target.health, target.max_health), (Some(93), Some(100)));

        let heard = feed.read("char.status", r#"{"enemy": "", "enemypct": 0}"#, &fields);
        assert_eq!(heard.target, Some(TargetState(None)));
    }
}
