//! The character's tables on the text the prototype tested them with.

use crate::game::Game;
use mudling_core::plugin::luau::Value;
use mudling_core::store::{CharactersState, Versioned};
use mudling_plugin_avatar::Transcript;
use std::collections::HashMap;

const SLIST: &str = "\
(110h-125m-99v-1471)(-/-)Spells and skills learnable by your class at the lower levels.

Lv:               Name
  1:          open hand
  3:       fast healing        meditation
  7:               kick
 10:             prayer     inner balance            embalm
 30:          iron monk
[B]ack,[H]elp,[R]efresh, or [C]ontinue:
B     - Scroll back one page.
Enter - Continue Scrolling.

[B]ack,[H]elp,[R]efresh, or [C]ontinue:
 35:        move hidden serpent technique       weapon lore        invigorate
 50: mongoose technique            gurney             mercy        rejuvenate
            demonstrate      transmogrify


(110h-125m-103v-1471)(-/-)1 CONCENTRATE
";

const PRACTICE: &str = "\
'You have 0 practice sessions left.'
'The following skills are at your disposal :'

Name                 % Lvl Sub Mana  Name                 % Lvl Sub Mana
-----------------------------------  -----------------------------------
armor lore           0   5   -    0  concentrate          0   8   -    5
dodge               90   4   -    0  enlightenment       90   8   -    0
fast healing         0   3   -    0  kick                33   7   -    0
open hand           90   1   -    0

(110h-125m-106v-1471)(-/-)West Side of Tree of Knowledge
";

fn read(text: &str) -> Transcript {
    let transcript = Transcript::of(text);
    assert!(
        transcript.problems().is_empty(),
        "{:?}",
        transcript.problems()
    );
    transcript
}

fn number(value: f64) -> Option<Value> {
    Some(Value::Number(value))
}

fn get(store: &HashMap<String, Value>, key: &str) -> Option<Value> {
    store.get(key).cloned()
}

/// What the engine's hooks wrote under `key`.
fn stored(game: &Game, key: &str) -> Option<Value> {
    game.hub
        .read(|stores: &Versioned<CharactersState>| stores.value().0.get(key).cloned())
}

#[test]
fn the_skill_list_leaves_out_the_pager_and_what_follows_the_prompt() {
    let skills = read(SLIST).store("skills");
    assert_eq!(get(&skills, "transmogrify.level"), number(50.0));
    assert_eq!(get(&skills, "demonstrate.level"), number(50.0));
    assert!(
        skills
            .keys()
            .all(|key| !key.contains("Scroll") && !key.contains("Continue")),
        "{skills:?}"
    );
    assert!(
        !skills
            .keys()
            .any(|key| key.to_lowercase().contains("concentrate")),
        "{skills:?}"
    );
}

/// Room text after a list was once read as skills ("(Glowing) Are you").
#[test]
fn text_after_a_list_is_no_skill() {
    let text = SLIST.replace(
        "(110h-125m-103v-1471)(-/-)1 CONCENTRATE",
        "     (Glowing) Are you lost?\n(110h-125m-103v-1471)(-/-)",
    );
    let skills = read(&text).store("skills");
    assert!(
        !skills.keys().any(|key| key.contains("Glowing")),
        "{skills:?}"
    );
    assert_eq!(get(&skills, "demonstrate.level"), number(50.0));
}

#[test]
fn the_practice_table_reads_every_cell() {
    let skills = read(PRACTICE).store("skills");
    assert_eq!(get(&skills, "practice.read"), number(7.0));
    assert_eq!(get(&skills, "kick.learned"), number(33.0));
    assert_eq!(get(&skills, "kick.level"), number(7.0));
    assert_eq!(get(&skills, "kick.mana"), number(0.0));
    assert_eq!(get(&skills, "concentrate.mana"), number(5.0));
    assert_eq!(get(&skills, "armor lore.learned"), number(0.0));
}

#[test]
fn the_skill_list_and_practice_merge() {
    let skills = read(&format!("{SLIST}{PRACTICE}")).store("skills");
    assert_eq!(get(&skills, "kick.learned"), number(33.0));
    assert_eq!(get(&skills, "iron monk.learned"), None);
    assert_eq!(get(&skills, "iron monk.level"), number(30.0));
    assert_eq!(get(&skills, "dodge.learned"), number(90.0));
}

/// A command sent between two reads shows in the chat; the game's text is
/// read as the game sent it.
#[test]
fn a_command_mid_line_leaves_the_line_whole() {
    let mut game = Game::new();
    game.say(b"You are");
    game.send("look");
    game.say(
        b" using:\r\n<used as light>     a small lantern\r\n\
          <worn on finger>    a pink ice ring\r\n(1h-1m-1v-1)(-/-)",
    );
    game.close();
    assert_eq!(game.terminal()[..3], ["You are", "look", " using:"]);
    assert_eq!(stored(&game, "worn.count"), number(2.0));
}

#[test]
fn a_command_at_any_point_of_a_table_changes_nothing_read() {
    let text = PRACTICE.replace('\n', "\r\n");
    for split in (1..text.len()).step_by(41) {
        let mut game = Game::new();
        game.say(&text.as_bytes()[..split]);
        game.send("look");
        game.say(&text.as_bytes()[split..]);
        game.close();
        assert_eq!(
            stored(&game, "skills.practice.read"),
            number(7.0),
            "split at {split}"
        );
    }
}

#[test]
fn a_table_split_at_any_point_still_reads() {
    let text = PRACTICE.replace('\n', "\r\n");
    for split in (1..text.len()).step_by(37) {
        let mut game = Game::new();
        game.say(&text.as_bytes()[..split]);
        game.say(&text.as_bytes()[split..]);
        game.close();
        assert_eq!(
            stored(&game, "skills.practice.read"),
            number(7.0),
            "split at {split}"
        );
    }
}

/// With no read of its own under way, the player's table shows and is read.
#[test]
fn a_table_the_player_asked_for_shows_and_is_read() {
    let mut game = Game::new();
    let table = [
        "You are using:",
        "<used as light>     (Glowing) Nom's Crystal Lantern",
        "<worn on finger>    a pink ice ring",
    ];
    for line in table {
        game.line(line);
    }
    game.line("(1h-1m-1v-1)(-/-)");
    game.close();
    assert_eq!(game.terminal()[..3], table);
    assert_eq!(stored(&game, "worn.count"), number(2.0));
}

#[test]
fn ordinary_text_changes_nothing() {
    let transcript = read("You hit A squirrel with pathetic intensity.\n10: not a skill");
    assert_eq!(transcript.writes().count(), 0);
}

#[test]
fn a_trainers_message_is_ordinary_output() {
    let mut game = Game::new();
    let message = "You practice kick with a trainer. Your current skill level is now 57.";
    game.line(message);
    assert_eq!(game.terminal(), [message]);
    let transcript = read(message);
    assert_eq!(transcript.events("practice.start", &[]).len(), 0);
    assert_eq!(
        get(&transcript.store("skills"), "kick.learned"),
        number(57.0)
    );
}

/// A hero's `practice` runs past a page, so its pager is continued too, and
/// the rest is read.
#[test]
fn a_long_practice_table_pages_through() {
    let mut game = Game::new();
    game.say(
        b"You have 18 practice sessions left.\r\n\r\n\
          Name                 % Lvl Sub Mana  Name                 % Lvl Sub Mana\r\n\
          -----------------------------------  -----------------------------------\r\n\
          gurney               0  50   -  100  haggle               0  51   1    0\r\n\
          [B]ack,[H]elp,[R]efresh, or [C]ontinue: ",
    );
    assert_eq!(game.sent(), "c\r\n");
    game.say(b"\r\nhide                90  11   -    0  kick                95   7   -    0\r\n");
    game.say(b"\r\n(1h-1m-1v-1)(-/-)");
    game.close();
    assert_eq!(stored(&game, "skills.kick.learned"), number(95.0));
}

#[test]
fn the_skill_list_pager_is_answered_too() {
    let mut game = Game::new();
    game.say(b"Spells and skills learnable by your class at the lower levels.\r\n");
    game.say(b"[B]ack,[H]elp,[R]efresh, or [C]ontinue: ");
    assert_eq!(game.sent(), "c\r\n");
}
