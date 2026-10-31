//! What the game says about the room, the player and the screen, through
//! the engine.

use crate::game::Game;
use mudling_core::defaults::hud::Vital;
use mudling_core::map::Direction;
use mudling_core::store::{Versioned, VitalsState};

const SHADE: &str = "In Shade of Tree of Knowledge";

fn info(name: &str, exits: &str) -> String {
    format!(
        r#"Room.Info {{ "name": "{name}", "zone": "{{ 1   4}} Crom    The Meadow", "environment": "field", "exits": {{ {exits} }}, "details": [ ] }}"#
    )
}

/// The description the map took for a room after `lines`.
fn description(lines: &[&str], name: &str) -> String {
    let mut game = Game::new();
    for line in lines {
        game.line(line);
    }
    game.gmcp(&info(name, r#""North": "O""#));
    game.current().description.clone()
}

#[test]
fn the_description_is_the_first_line_after_the_exits() {
    let description = description(
        &[
            "(110h-125m-120v-1062)(-/-)In Shade of Tree of Knowledge",
            "[Exits: north east south west]",
            "   The trunk of the tree stands to the North and bends overhead to form a",
            "canopy of leaves.",
        ],
        SHADE,
    );
    assert_eq!(
        description,
        "The trunk of the tree stands to the North and bends overhead to form a"
    );
}

#[test]
fn the_name_is_read_when_the_prompt_had_its_own_line() {
    let description = description(
        &[
            "(110h-125m-120v-1062)(-/-)",
            "",
            "\u{1b}[1;36mWest Side of Tree of Knowledge\u{1b}[0m",
            "[Exits: north east south west]",
            "   You stand on the west side of a huge tree.",
        ],
        "West Side of Tree of Knowledge",
    );
    assert_eq!(description, "You stand on the west side of a huge tree.");
}

#[test]
fn another_rooms_text_gives_no_description() {
    let description = description(
        &[
            "A mountain trail",
            "[Exits: south west up]",
            "   You look around and you can barely see a dark crack in the cliff to the",
        ],
        SHADE,
    );
    assert_eq!(description, "");
}

#[test]
fn a_description_is_taken_once() {
    let mut game = Game::new();
    game.line("A mountain trail");
    game.line("[Exits: south west up]");
    game.line("   You look around.");
    game.gmcp(&info("A mountain trail", r#""South": "O""#));
    game.send("south");
    game.gmcp(&info("A mountain trail", r#""North": "O""#));
    assert_eq!(game.rooms().len(), 2);
    assert_eq!(game.current().description, "");
}

#[test]
fn the_description_is_read_when_the_name_carries_a_tag() {
    let description = description(
        &[
            "(599hp 648m 343v 1046tnl)(-/-)",
            "An underground lake (Water)",
            "[Exits: north east west]",
            "You are floating on a cold, jet-black underground lake fed by",
        ],
        "An underground lake",
    );
    assert_eq!(
        description,
        "You are floating on a cold, jet-black underground lake fed by"
    );
}

#[test]
fn room_info_sets_the_room_and_its_exits() {
    let mut game = Game::new();
    game.gmcp(
        r#"Room.Info { "name": "A turn in the dirt tunnel", "zone": "{ 2   4} Crom    The Cult of Bonk",
           "environment": "inside", "exits": { "North": "C", "East": "O", "South": "O" }, "details": [ ] }"#,
    );
    let room = game.current();
    assert_eq!(room.name, "A turn in the dirt tunnel");
    assert_eq!(room.environment, "inside");
    let closed = |direction| room.exits.get(direction).map(|exit| exit.closed);
    assert_eq!(closed(Direction::North), Some(true));
    assert_eq!(closed(Direction::East), Some(false));
    assert_eq!(closed(Direction::South), Some(false));
    assert_eq!(closed(Direction::West), None);
}

#[test]
fn vitals_read_numbers_sent_as_strings() {
    let mut game = Game::new();
    game.gmcp(
        r#"Char.Vitals { "hp": "30", "maxhp": "39", "mp": "110", "maxmp": "111", "mv": "96", "maxmv": "120",
           "wgt": "141", "tnl": "1342", "maxtnl": "1515", "maxwgt": "180", "items": "29",
           "lag": "5", "position": "Fight", "monhp": null, "string": "HP:30/39" }"#,
    );
    let vital = |game: &Game, vital: Vital| {
        game.hub
            .read(|vitals: &Versioned<VitalsState>| vitals.value().0.get(&vital).copied())
    };
    assert_eq!(vital(&game, Vital::Hp), Some(30));
    assert_eq!(vital(&game, Vital::MaxHp), Some(39));
    assert_eq!(vital(&game, Vital::Mana), Some(110));
    assert_eq!(vital(&game, Vital::MaxMoves), Some(120));
}

/// Whatever the player's style: the line after a Char.Vitals is the prompt,
/// and later prompts of its shape are hidden whatever their numbers.
#[test]
fn any_style_of_prompt_is_learned() {
    for (prompt, later) in [
        ("(110h-125m-120v-1062)(-/-)", "(207h-147m-90v-15)(-/-)"),
        ("(599hp 1173m 514v 1046tnl)(-/-)", "(1hp 3m 514v 9tnl)(-/-)"),
        ("$*(150h-156m-360v-1397)(-/-)", "$*(2h-156m-360v-1397)(-/-)"),
        (
            "<46/46hp><116/116m><116/116mv>",
            "<12/46hp><1/116m><0/116mv>",
        ),
    ] {
        let mut game = Game::new();
        game.gmcp(r#"Char.Vitals { "hp": "30" }"#);
        game.say(prompt.as_bytes());
        game.gmcp(r#"Char.Vitals { "hp": "31" }"#);
        game.say(later.as_bytes());
        game.line("You are using:");
        assert_eq!(game.terminal(), ["You are using:"], "{prompt}");
    }
}

/// The network can split a prompt. Half a prompt mustn't hide the whole one.
#[test]
fn half_a_prompt_does_not_hide_the_whole_one() {
    let mut game = Game::new();
    for _ in 0..3 {
        game.gmcp(r#"Char.Vitals { "hp": "271" }"#);
        game.say(b"(271h-171m-310v-4)(-/-)");
    }
    game.gmcp(r#"Char.Vitals { "hp": "271" }"#);
    game.say(b"(271h-17");
    game.say(b"1m-310v-4)(-/-)");
    game.gmcp(r#"Char.Vitals { "hp": "271" }"#);
    game.say(b"(271h-0m-0v-4)(-/-)You stop using midnight boots.\r\n");
    assert_eq!(game.terminal(), ["You stop using midnight boots."]);
}

/// A prompt, a command, the reply and the next prompt: the reply comes
/// after the command it answers.
#[test]
fn replies_follow_the_command_they_answer() {
    let mut game = Game::new();
    game.gmcp(r#"Char.Vitals { "hp": "125" }"#);
    game.say(b"(125h-129m-180v-1166)(-/-)");
    game.send("autoloot");
    game.say(b"Ok, your autoloot is now set to OFF.");
    game.say(b"\n\r(125h-129m-180v-1166)(-/-)");
    assert_eq!(
        game.terminal(),
        ["autoloot", "Ok, your autoloot is now set to OFF."]
    );
}
