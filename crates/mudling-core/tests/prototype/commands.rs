//! Commands on their way out: chains, speedwalk, and the spelling of
//! repeated moves.

use crate::driven::Driven;
use mudling_core::defaults::commands::{CommandVariety, WalkSyntax, parse_speedwalk};
use mudling_core::map::Direction;
use mudling_core::plugin::Plugin;
use mudling_core::store::{MapState, Versioned};

/// A game that chains commands with `=`.
fn chaining() -> Plugin {
    let manifest = "id = \"chainer\"\nname = \"Chainer\"\nversion = \"1.0.0\"\napi = 1\n\
         license = \"MIT\"\nlayer = \"game\"\n[servers]\nhosts = [\"mud.example.org\"]\n\
         [points]\ncommands = \"commands.toml\"\n";
    Plugin::from_files(|file| {
        Ok(match file {
            "plugin.toml" => manifest,
            "commands.toml" => "separator = \"=\"\n",
            _ => return Err(std::io::Error::other(file.to_owned())),
        }
        .to_owned())
    })
    .unwrap_or_else(|error| panic!("{error}"))
}

fn room(number: i64, exits: &str) -> String {
    format!(r#"Room.Info {{"num": {number}, "name": "Room {number}", "exits": {{{exits}}}}}"#)
}

#[test]
fn a_typed_chain_is_each_of_its_commands_to_the_map() {
    let mut driven = Driven::with(vec![chaining()], None);
    driven.gmcp(&room(1, r#""e": 2"#));
    driven.sent();
    driven.send("east;west;east");
    assert_eq!(driven.sent(), ["east=west=east"]);
    driven.gmcp(&room(2, r#""w": 1"#));
    driven.gmcp(&room(1, r#""e": 2"#));
    driven.gmcp(&room(2, r#""w": 1"#));
    let links = driven.hub.read(|map: &Versioned<MapState>| {
        let first = map
            .value()
            .room(1)
            .unwrap()
            .links
            .get(Direction::East)
            .copied();
        let second = map
            .value()
            .room(2)
            .unwrap()
            .links
            .get(Direction::West)
            .copied();
        (first, second)
    });
    assert_eq!(links, (Some(2), Some(1)));
}

#[test]
fn a_long_walk_never_repeats_a_line() {
    let mut driven = Driven::new();
    driven.send("30n");
    let sent = driven.sent();
    assert_eq!(sent.len(), 30);
    assert_eq!(sent[..4], ["north", "n", "north", "n"]);
    assert!(sent.windows(2).all(|pair| pair[0] != pair[1]), "{sent:?}");
}

#[test]
fn a_move_typed_again_is_spelled_the_other_way() {
    let mut driven = Driven::new();
    for typed in ["north", "north", "n", "n", "look", "look"] {
        driven.send(typed);
    }
    assert_eq!(
        driven.sent(),
        ["north", "n", "north", "n", "look", "look"],
        "moves alternate; a repeated look goes as typed"
    );
}

/// 25 norths in one chained line got a player disconnected.
#[test]
fn repeated_moves_alternate_their_spelling() {
    let mut variety = CommandVariety::default();
    let walk = [vec!["north"; 30], vec!["open south", "south", "s"]].concat();
    let sent: Vec<String> = walk.iter().map(|command| variety.next(command)).collect();
    assert!(
        sent.windows(2)
            .all(|pair| !pair[0].eq_ignore_ascii_case(&pair[1])),
        "{sent:?}"
    );
    assert_eq!(sent[..4], ["north", "n", "north", "n"]);
    assert_eq!(sent[sent.len() - 3..], ["open south", "south", "s"]);
    assert!(
        sent.iter()
            .all(|command| Direction::parse(command).is_some() || command == "open south")
    );
}

#[test]
fn other_commands_go_as_typed() {
    let mut variety = CommandVariety::default();
    let sent = ["kick", "kick", "look"].map(|command| variety.next(command));
    assert_eq!(sent, ["kick", "kick", "look"]);
}

#[test]
fn what_isnt_a_walk_goes_as_typed() {
    let walk = WalkSyntax::default();
    for typed in [
        "n",
        "d",
        "look",
        "kill goblin",
        "2 goblins",
        "[s",
        "8n, kill rat",
        "",
        "say 2n",
    ] {
        assert_eq!(walk.expand(typed), None, "{typed:?}");
        assert_eq!(parse_speedwalk(typed), None, "{typed:?}");
    }
}
