//! Gear: the eq table in slot order, wear, remove and put, identify (base
//! against enchant) and examine, from scrubbed fixtures.

use mudling_core::plugin::luau::Value;
use mudling_plugin_avatar::{Transcript, load};
use std::collections::HashMap;

fn read(fixture: &str) -> Transcript {
    let transcript = Transcript::of(fixture);
    assert!(transcript.problems().is_empty(), "{:?}", transcript.problems());
    transcript
}

fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

fn get<'a>(store: &'a HashMap<String, Value>, key: &str) -> &'a Value {
    store.get(key).unwrap_or_else(|| panic!("no `{key}` in {store:?}"))
}

#[test]
fn the_slots_are_the_games_fifteen_in_its_order() {
    let plugin = load();
    let slots = &plugin.points.gear.as_ref().unwrap().slots;
    assert_eq!(
        slots,
        &[
            "light", "finger", "neck", "body", "head", "legs", "feet", "hands", "arms", "offhand", "about", "waist",
            "wrist", "wield", "held"
        ]
    );
}

#[test]
fn equipment_lists_every_row_in_order_empty_slots_included() {
    let transcript = read(include_str!("fixtures/equipment.txt"));
    let equipment = transcript.store("equipment");
    assert_eq!(get(&equipment, "count"), &Value::Number(17.0));
    let rows: Vec<(String, String)> = (1..=17)
        .map(|row| {
            let field = |name: &str| match get(&equipment, &format!("{row}.{name}")) {
                Value::Text(value) => value.clone(),
                other => panic!("{other:?}"),
            };
            (field("slot"), field("item"))
        })
        .collect();
    let slots: Vec<&str> = rows.iter().map(|(slot, _)| slot.as_str()).collect();
    assert_eq!(
        slots,
        [
            "light", "finger", "neck", "neck", "body", "head", "legs", "feet", "hands", "arms", "offhand", "about",
            "waist", "wrist", "wrist", "wield", "held"
        ]
    );
    assert_eq!(rows[0].1, "", "an empty slot");
    assert_eq!(rows[3].1, "(Glowing) a small necklace", "the item as the game shows it");
    assert_eq!(rows[10].1, "");

    // Each worn item's slot and command word, by its plain name.
    let gear = transcript.store("gear");
    assert_eq!(get(&gear, "slot.a small necklace"), &text("neck"));
    assert_eq!(get(&gear, "slot.the red-eyed jewel"), &text("held"));
    assert_eq!(get(&gear, "keyword.an amulet of gorn"), &text("amulet"));
    assert_eq!(get(&gear, "keyword.the red-eyed jewel"), &text("jewel"));
    assert_eq!(get(&gear, "keyword.the captain's silver sash"), &text("sash"), "never the possessive");

    // A gear message glued after the table's prompt is still read.
    assert_eq!(transcript.events("gear.removed", &["item"]), [[Some("a pink ice ring".to_owned())]]);
}

#[test]
fn a_possessive_is_the_command_word_only_when_nothing_else_is() {
    let transcript = read("You are using:\n<worn about waist>  the captain's\n<held>  Nom's Crystal Lantern\n(1h-1m-1v-1)(-/-)");
    let gear = transcript.store("gear");
    assert_eq!(get(&gear, "keyword.the captain's"), &text("captain"));
    assert_eq!(get(&gear, "keyword.nom's crystal lantern"), &text("lantern"));
}

#[test]
fn wear_remove_and_put_are_events_and_wearing_notes_the_slot() {
    let transcript = read(include_str!("fixtures/gear-messages.txt"));
    let worn: Vec<String> =
        transcript.events("gear.wore", &["item"]).into_iter().map(|captures| captures[0].clone().unwrap()).collect();
    assert_eq!(
        worn,
        [
            "a Tiamat mask",
            "the mantle of Bonk",
            "a copper bracelet",
            "a linen robe",
            "a scarlet silk sash",
            "a golden feather",
            "Nom's Crystal Lantern",
            "a long sword"
        ]
    );
    let gear = transcript.store("gear");
    for (item, slot) in [
        ("a tiamat mask", "head"),
        ("the mantle of bonk", "neck"),
        ("a copper bracelet", "wrist"),
        ("a linen robe", "about"),
        ("a scarlet silk sash", "waist"),
        ("a golden feather", "held"),
        ("nom's crystal lantern", "light"),
        ("a long sword", "wield"),
    ] {
        assert_eq!(get(&gear, &format!("slot.{item}")), &text(slot), "{item}");
    }
    assert_eq!(transcript.events("gear.removed", &["item"]), [[Some("a round shield".to_owned())]]);
    assert_eq!(
        transcript.events("gear.put", &["item", "container"]),
        [[Some("a rope belt".to_owned()), Some("a satchel".to_owned())]]
    );
    assert_eq!(transcript.events("gear.did_not_fit", &[]).len(), 1);
    assert_eq!(transcript.events("gear.not_carried", &[]).len(), 1);
}

#[test]
fn identify_splits_base_from_enchant() {
    let transcript = read(include_str!("fixtures/identify.txt"));
    let identify = transcript.store("identify");
    let reads: Vec<&str> = transcript
        .writes()
        .filter(|write| write.store == "identify" && write.key == "read")
        .map(|write| match &write.value {
            Value::Text(keywords) => keywords.as_str(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(reads, ["black bear bracelet", "gorn amulet newbie"]);

    // A shop's identify names the item.
    let bracelet = |key: &str| get(&identify, &format!("black bear bracelet.{key}")).clone();
    assert_eq!(bracelet("name"), text("bear skin bracelet"));
    assert_eq!(bracelet("type"), text("armor"));
    assert_eq!(bracelet("flags"), text(""), "\"none\" is no flags");
    assert_eq!(bracelet("level"), Value::Number(8.0));
    assert_eq!(bracelet("quality"), text("100 / 100 hps"));
    assert_eq!(bracelet("base.ac"), Value::Number(-2.0));
    assert_eq!(bracelet("base.con"), Value::Number(1.0));
    assert_eq!(bracelet("base.str"), Value::Number(1.0));
    assert_eq!(get(&transcript.store("gear"), "keywords.bear skin bracelet"), &text("black bear bracelet"));

    // The spell's, glued to a prompt, doesn't; its enchant is apart.
    let amulet = |key: &str| identify.get(&format!("gorn amulet newbie.{key}")).cloned();
    assert_eq!(amulet("name"), None);
    assert_eq!(amulet("flags"), Some(text("magic anti-evil")));
    assert_eq!(amulet("level"), Some(Value::Number(2.0)));
    assert_eq!(amulet("base.ac"), Some(Value::Number(-1.0)));
    assert_eq!(amulet("base.hitroll"), Some(Value::Number(2.0)));
    assert_eq!(amulet("added.enchant armor.ac"), Some(Value::Number(-4.0)));
}

#[test]
fn examine_notes_the_slot_an_item_goes_in() {
    let gear = read(include_str!("fixtures/examine.txt")).store("gear");
    assert_eq!(
        gear,
        HashMap::from([
            ("slot.a pair of steel bracers".to_owned(), text("arms")),
            ("slot.a golden feather".to_owned(), text("held")),
            ("slot.a linen robe".to_owned(), text("about")),
        ])
    );
}
