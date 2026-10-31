//! Gear, identify, mobs and item words on the game's own messages.

use crate::game::Game;
use mudling_core::map::Mark;
use mudling_core::plugin::luau::Value;
use mudling_core::store::{MobsState, Versioned};
use mudling_plugin_avatar::{Transcript, load};

fn read(text: &str) -> Transcript {
    let transcript = Transcript::of(text);
    assert!(
        transcript.problems().is_empty(),
        "{:?}",
        transcript.problems()
    );
    transcript
}

fn items(transcript: &Transcript, event: &str) -> Vec<String> {
    let events = transcript.events(event, &["item"]).into_iter();
    events.map(|mut item| item.remove(0).unwrap()).collect()
}

fn equipment_items(transcript: &Transcript) -> Vec<Value> {
    let equipment = transcript.store("equipment");
    let Some(Value::Number(count)) = equipment.get("count") else {
        panic!("{equipment:?}");
    };
    (1..=*count as usize)
        .map(|row| equipment[&format!("{row}.item")].clone())
        .collect()
}

fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

/// Avatar glues the table's first line to the prompt.
#[test]
fn equipment_glued_to_the_prompt_is_read() {
    let transcript = read(
        "(599hp 1173m 514v 1046tnl)(-/-)You are using:\n\
         <used as light>     (Demonic) (Magical) (Glowing) the eye of the beholder\n\
         <worn on finger>    (Humming) a black hematite ring\n\
         <worn on body>      (Magical) a white leather dress\n\
         <wielded>           (Magical) (Glowing) (Humming) hand of Buddha\n\
         (599hp 1173m 514v 1046tnl)(-/-)",
    );
    let equipment = transcript.store("equipment");
    let slots: Vec<&Value> = (1..=4)
        .map(|row| &equipment[&format!("{row}.slot")])
        .collect();
    assert_eq!(
        slots,
        [
            &text("light"),
            &text("finger"),
            &text("body"),
            &text("wield")
        ]
    );
    assert_eq!(
        equipment["4.item"],
        text("(Magical) (Glowing) (Humming) hand of Buddha")
    );
    assert_eq!(
        transcript.store("gear")["slot.a black hematite ring"],
        text("finger")
    );
}

#[test]
fn gear_and_affect_messages_are_events() {
    let transcript = read(
        "You feel less in tune with the world.\n\
         You stop using an official Cult of Bonk Hat.\n\
         You wear a Tiamat mask on your head.\n\
         You hold a golden feather in your hands.\n\
         You light Nom's Crystal Lantern and hold it.\n\
         You wield a long sword.",
    );
    assert_eq!(transcript.events("character.affects_changed", &[]).len(), 1);
    assert_eq!(
        items(&transcript, "gear.removed"),
        ["an official Cult of Bonk Hat"]
    );
    assert_eq!(
        items(&transcript, "gear.wore"),
        [
            "a Tiamat mask",
            "a golden feather",
            "Nom's Crystal Lantern",
            "a long sword"
        ]
    );
}

/// Divine Awen and sanctuary put "$*" before the prompt; gear read through
/// it went unnoticed, so gear sets planned from stale gear.
#[test]
fn gear_after_a_marked_prompt_is_read() {
    let transcript = read(
        "$*(150h-156m-360v-1397)(-/-)You stop using a golden feather.\n\
         You hold a tattered kite shield in your hands.\n\
         $*(150h-156m-360v-1397)(-/-)You are using:\n\
         <worn about waist>  a scarlet silk sash\n\
         <held>              a tattered kite shield\n\
         $*(150h-156m-360v-1397)(-/-)",
    );
    assert_eq!(items(&transcript, "gear.removed"), ["a golden feather"]);
    assert_eq!(items(&transcript, "gear.wore"), ["a tattered kite shield"]);
    assert_eq!(
        equipment_items(&transcript),
        [text("a scarlet silk sash"), text("a tattered kite shield")]
    );
}

/// The wear message and the `eq` table after it arrive in one piece of text:
/// the table has the sash, once.
#[test]
fn a_wear_followed_by_eq_in_one_read_is_counted_once() {
    let transcript = read(
        "You stop using a rope belt.\n\
         You wear a scarlet silk sash about your waist.\n\
         You are using:\n\
         <worn about waist>  a scarlet silk sash\n\
         (1h-1m-1v-1)(-/-)",
    );
    assert_eq!(equipment_items(&transcript), [text("a scarlet silk sash")]);
}

#[test]
fn a_gear_line_right_after_an_eq_table_still_counts() {
    let transcript = read(
        "You are using:\n\
         <worn about waist>  a rope belt\n\
         You stop using a rope belt.\n\
         (1h-1m-1v-1)(-/-)",
    );
    assert_eq!(items(&transcript, "gear.removed"), ["a rope belt"]);
}

#[test]
fn the_spell_gives_the_words_and_adds_up_what_it_names() {
    let transcript = read(
        "Object 'ring hematite black' type armor, extra flags hum.\n\
         Modifies hit roll by 2\n\
         Modifies damage roll by 3\n\
         Modifies hp by 15\n\
         Modifies saving throw by -1\n\
         (1h-1m-1v-1)(-/-)",
    );
    let identify = transcript.store("identify");
    let value = |key: &str| identify.get(&format!("ring hematite black.{key}")).cloned();
    assert_eq!(value("name"), None);
    assert_eq!(value("base.hitroll"), Some(Value::Number(2.0)));
    assert_eq!(value("base.damroll"), Some(Value::Number(3.0)));
    assert_eq!(value("base.health_max"), Some(Value::Number(15.0)));
}

#[test]
fn an_items_word_is_the_noun_of_its_name() {
    let plugin = load();
    let keywords = plugin.points.commands.as_ref().unwrap().keywords.as_ref();
    let keywords = keywords.unwrap();
    for (name, expected) in [
        ("a pixie crossbow", "crossbow"),
        ("corpse of BONK", "corpse"),
        ("chunk of rabbit meat", "chunk"),
        ("a holy scroll of Bonk", "scroll"),
        ("5 standard bolts", "bolts"),
        ("a magical fountain", "fountain"),
        ("(Glowing) orb of knowledge", "orb"),
        ("belt that says, \"Seven In One.\"", "belt"),
    ] {
        assert_eq!(keywords.word_for(name).as_deref(), Some(expected), "{name}");
    }
}

fn aggressive(game: &Game) -> Vec<String> {
    game.hub.read(|mobs: &Versioned<MobsState>| {
        let marks = mobs.value().marks.iter();
        marks
            .filter(|(mark, _)| *mark == Mark::Aggressive)
            .map(|(_, name)| name.clone())
            .collect()
    })
}

#[test]
fn a_mob_attacking_unprovoked_marks_its_kind() {
    let mut game = Game::new();
    game.gmcp(r#"Room.Info { "name": "A nursery", "zone": "z", "exits": { } }"#);
    for line in [
        "The ugly bear GLARES at you and SNARLS!'",
        "The ugly bear strikes you with nil intensity.",
        "A Raggedy Ann doll GLARES at you and SNARLS!'",
        "The ugly bear GLARES at you and SNARLS!'",
    ] {
        game.line(line);
    }
    let mut marked = aggressive(&game);
    marked.sort();
    assert_eq!(marked, ["bear", "doll"]);
}

#[test]
fn a_mob_helping_a_friend_or_ordinary_combat_marks_nothing() {
    let mut game = Game::new();
    game.gmcp(r#"Room.Info { "name": "A nursery", "zone": "z", "exits": { } }"#);
    for line in [
        "You hit the nanny with pathetic intensity.",
        "The dwarven youth screams and attacks!",
        "A squirrel strikes you with pathetic intensity.",
    ] {
        game.line(line);
    }
    assert_eq!(aggressive(&game), [] as [String; 0]);
}
