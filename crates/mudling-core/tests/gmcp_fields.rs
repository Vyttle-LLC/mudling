//! What a plugin says about a game's GMCP, through a session engine: where
//! gear's stats are, where a door that changed lists the exits, which
//! message names an area, what GMCP requests it sends, and which messages
//! are lines of the chat. The game is made
//! up, its packages named unlike any real game's, so nothing here can lean
//! on a name the core knows.

use mudling_core::Line;
use mudling_core::ansi::Color;
use mudling_core::gear::ItemRef;
use mudling_core::map::{Direction, feature};
use mudling_core::persist::Timestamp;
use mudling_core::plugin::Plugin;
use mudling_core::session::{Engine, SessionConfig, Typing};
use mudling_core::store::{ItemsState, MapState, Store, StoreHub, TerminalState, Versioned};
use mudling_core::telnet::command::{IAC, WILL};
use mudling_core::telnet::option::GMCP;
use mudling_core::telnet::{Bytes, gmcp};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

const MANIFEST: &str = "id = \"madeup\"\nname = \"Made Up\"\nversion = \"1.0.0\"\napi = 1\n\
                        license = \"MIT\"\nlayer = \"game\"\n\
                        servers = { hosts = [\"mud.example\"] }\n";

/// A plugin of `plugin.toml`'s `extra` lines and the point files in `files`.
fn plugin(extra: &str, files: &[(&str, &str)]) -> Plugin {
    let files: BTreeMap<&str, &str> = files.iter().copied().collect();
    let points: String = (files.keys())
        .map(|file| format!("{} = \"{file}\"\n", file.trim_end_matches(".toml")))
        .collect();
    let manifest = format!("{MANIFEST}{extra}[points]\n{points}");
    Plugin::from_files(|file| match file {
        "plugin.toml" => Ok(manifest.clone()),
        file => Ok(files[file].to_owned()),
    })
    .unwrap_or_else(|error| panic!("{error}"))
}

struct Driven {
    engine: Engine,
    hub: Arc<StoreHub>,
    _folder: tempfile::TempDir,
}

impl Driven {
    fn with(plugin: Plugin) -> Driven {
        Driven::approving(plugin, false)
    }

    fn approving(plugin: Plugin, approved: bool) -> Driven {
        let folder = tempfile::tempdir().unwrap();
        let hub = Arc::new(StoreHub::new(|| {}));
        let id = plugin.id().clone();
        let config = SessionConfig {
            enabled: vec![id.clone()],
            approved: if approved { vec![id] } else { Vec::new() },
            plugins: vec![plugin],
            ..SessionConfig::new(folder.path(), "mud.example", 4000)
        };
        let typing = Arc::new(Mutex::new(Typing::default()));
        let mut engine = Engine::open(config, Arc::clone(&hub), typing).unwrap();
        engine.start_connection().unwrap();
        Driven {
            engine,
            hub,
            _folder: folder,
        }
    }

    fn say(&mut self, bytes: Vec<u8>) {
        let now = Timestamp(0);
        self.engine.receive(Bytes::from(bytes), now).unwrap();
        self.engine.settle(now);
    }

    fn gmcp(&mut self, package: &str, json: &str) {
        let mut frame = Vec::new();
        gmcp::encode(package, json, &mut frame);
        self.say(frame);
    }

    fn text(&mut self, text: &str) {
        self.say(text.as_bytes().to_vec());
    }

    fn read<S: Store, R>(&self, read: impl FnOnce(&S) -> R) -> R {
        self.hub.read(|store: &Versioned<S>| read(store.value()))
    }
}

#[test]
fn gear_is_measured_from_the_stats_where_the_hud_says() {
    let smith = plugin(
        "",
        &[
            ("hud.toml", "[character]\nstr = \"Body.Stats.might\"\n"),
            (
                "gear.toml",
                "[[wear]]\nname = \"gear.wore\"\npattern = '''^You wear (?<item>.+?) on your \\w+\\.$'''\n",
            ),
        ],
    );
    let mut driven = Driven::with(smith);
    driven.gmcp("Body.Stats", r#"{"might": "15"}"#);
    driven.gmcp(
        "Char.Items.List",
        r#"{"location": "inv", "items": [{"id": "7", "name": "a copper ring"}]}"#,
    );
    driven.text("You wear a copper ring on your finger.\r\n");
    driven.gmcp("Char.Status", r#"{"str": "18"}"#);
    driven.gmcp("Body.Stats", r#"{"might": "16"}"#);

    let ring = ItemRef {
        name: "a copper ring",
        copy: Some("7"),
    };
    let effects = driven.read(|items: &ItemsState| items.book.stats(ring).unwrap().effects());
    assert_eq!(
        effects.get("str"),
        Some(&1),
        "Char.Status's str isn't the stat"
    );
}

#[test]
fn a_door_that_changes_in_place_is_read_where_the_map_says() {
    let keeper = plugin("", &[("map.toml", "room_exits = \"Room.Doors.exits\"\n")]);
    let mut driven = Driven::with(keeper);
    driven.gmcp(
        "Room.Info",
        r#"{"num": 1, "name": "A gatehouse", "exits": {"n": 2}}"#,
    );
    let north_closed = |driven: &Driven| {
        driven.read(|map: &MapState| {
            let room = map.rooms().next().expect("the room");
            room.exits.get(Direction::North).expect("north").closed
        })
    };
    assert!(!north_closed(&driven));
    driven.gmcp("Char.Status", r#"{"room_exits": {"n": "C"}}"#);
    assert!(!north_closed(&driven), "only where the plugin says");
    driven.gmcp("Room.Doors", r#"{"exits": {"n": "C"}}"#);
    assert!(north_closed(&driven));
}

#[test]
fn an_area_is_named_by_the_message_the_map_says() {
    let namer = plugin(
        "",
        &[(
            "map.toml",
            "[area_info]\nzone = \"Zone.About.key\"\nname = \"Zone.About.title\"\n",
        )],
    );
    let mut driven = Driven::with(namer);
    driven.gmcp(
        "Room.Info",
        r#"{"num": 1, "name": "A lane", "zone": "lanes", "exits": {}}"#,
    );
    let area = |driven: &Driven| {
        driven.read(|map: &MapState| map.derived().unwrap().areas[0].name.name.clone())
    };
    assert_eq!(area(&driven), "lanes");
    driven.gmcp(
        "Zone.About",
        r#"{"key": "lanes", "title": "The Green Lanes"}"#,
    );
    assert_eq!(area(&driven), "The Green Lanes");
}

#[test]
fn an_area_info_split_over_two_messages_is_refused() {
    let refused = Plugin::from_files(|file| {
        Ok(match file {
            "plugin.toml" => format!("{MANIFEST}[points]\nmap = \"map.toml\"\n"),
            _ => "[area_info]\nzone = \"Zone.A.key\"\nname = \"Zone.B.title\"\n".to_owned(),
        })
    });
    let error = refused.expect_err("two messages").to_string();
    assert!(error.contains("must be in one message"), "{error}");
}

const ASKS_FOR_THE_ZONE: &str = "[[request]]\nsend = \"Ask.Zone now\"\nwhen = \"Room.Info.zone\"\n";

fn asker() -> Plugin {
    plugin(
        "permissions = [\"send_gmcp\"]\n",
        &[("connection.toml", ASKS_FOR_THE_ZONE)],
    )
}

/// A session with GMCP on, past what the client sends when it starts.
fn with_gmcp(plugin: Plugin, approved: bool) -> Driven {
    let mut driven = Driven::approving(plugin, approved);
    driven.say(vec![IAC, WILL, GMCP]);
    drop(driven.engine.take_output());
    driven
}

fn room(driven: &mut Driven, number: u32, zone: &str) -> Vec<u8> {
    let info = format!(r#"{{"num": {number}, "name": "A lane", "zone": "{zone}", "exits": {{}}}}"#);
    driven.gmcp("Room.Info", &info);
    driven.engine.take_output()
}

/// `IAC SB GMCP Ask.Zone now IAC SE`, and nothing around it.
fn the_request() -> Vec<u8> {
    let mut frame = Vec::new();
    gmcp::encode("Ask.Zone", "now", &mut frame);
    frame
}

#[test]
fn a_request_goes_out_as_gmcp_each_time_its_field_changes() {
    let mut driven = with_gmcp(asker(), true);
    // The first room also asks for the modules and the inventory again.
    assert!(contains(&room(&mut driven, 1, "lanes"), &the_request()));
    assert!(room(&mut driven, 2, "lanes").is_empty(), "the same zone");
    assert_eq!(room(&mut driven, 3, "hills"), the_request());
    assert_eq!(room(&mut driven, 4, "lanes"), the_request());
}

#[test]
fn a_request_waits_for_the_players_approval() {
    let mut driven = with_gmcp(asker(), false);
    assert!(!contains(&room(&mut driven, 1, "lanes"), b"Ask.Zone"));

    driven.engine.set_approved(asker().id().clone(), true);
    assert_eq!(room(&mut driven, 2, "hills"), the_request());
    driven.engine.set_approved(asker().id().clone(), false);
    assert!(room(&mut driven, 3, "lanes").is_empty());
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn a_request_needs_the_send_gmcp_permission() {
    let refused = Plugin::from_files(|file| {
        Ok(match file {
            "plugin.toml" => format!("{MANIFEST}[points]\nconnection = \"connection.toml\"\n"),
            _ => ASKS_FOR_THE_ZONE.to_owned(),
        })
    });
    let error = refused.expect_err("no permission").to_string();
    assert!(
        error.contains("needs the `send_gmcp` permission"),
        "{error}"
    );
}

/// What can't be sent: a line end would end a command on a server that
/// read one, and Core messages run the connection itself.
#[test]
fn a_request_is_a_gmcp_message_and_nothing_else() {
    let load = |send: &str| {
        let connection = format!("[[request]]\nsend = \"{send}\"\nwhen = \"Room.Info.zone\"\n");
        Plugin::from_files(|file| {
            Ok(match file {
                "plugin.toml" => format!(
                    "{MANIFEST}permissions = [\"send_gmcp\"]\n[points]\nconnection = \"connection.toml\"\n"
                ),
                _ => connection.clone(),
            })
        })
    };
    for send in [
        "request area",
        "Char.Skills.Get {\\\"group\\\": \\\"all\\\"}",
    ] {
        load(send).unwrap_or_else(|error| panic!("{send:?}: {error}"));
    }
    for send in [
        "look\\nsay hi",
        "Ask.Zone\\r\\nquit",
        "Ask.Zone now\\u001b[0m",
        "Core.Supports.Remove [\\\"Room 1\\\"]",
        "core.hello {}",
        " Ask.Zone",
        "1Ask",
        "Ask..Zone",
        "",
    ] {
        assert!(load(send).is_err(), "{send:?} loaded");
    }
}

/// A game that sends its channels as `Talk.Said`, and a rule coloring its
/// whispers.
fn talker() -> Plugin {
    plugin(
        "",
        &[(
            "text.toml",
            "[[channel]]\nmessage = \"Talk.Said.line\"\n\
             [[color]]\npattern = '''whispers'''\nfg = 5\n",
        )],
    )
}

fn chat(driven: &Driven) -> Vec<Line> {
    driven.read(|terminal: &TerminalState| terminal.lines().iter().cloned().collect())
}

#[test]
fn a_channel_is_a_line_of_the_chat_read_by_the_text_rules() {
    let mut driven = Driven::with(talker());
    driven.text("A lane\r\n");
    driven.gmcp(
        "Talk.Said",
        r#"{"channel": "gossip", "line": "\u001b[32mMira gossips 'hello'\u001b[0m"}"#,
    );
    driven.gmcp(
        "Talk.Said",
        r#"{"channel": "tell", "line": "Mira whispers 'psst'"}"#,
    );
    driven.gmcp("Talk.Other", r#"{"line": "not a channel"}"#);

    let lines = chat(&driven);
    let plain: Vec<&str> = lines.iter().map(Line::plain).collect();
    assert_eq!(
        plain,
        ["A lane", "Mira gossips 'hello'", "Mira whispers 'psst'"]
    );
    let colors =
        |line: &Line| -> Vec<_> { line.styled().iter().map(|span| span.style.fg).collect() };
    assert_eq!(colors(&lines[1]), [Some(Color(2))], "the game's color");
    assert!(colors(&lines[2]).contains(&Some(Color(5))), "the rule's");
}

#[test]
fn a_channel_shows_after_the_text_the_server_has_not_ended() {
    let mut driven = Driven::with(talker());
    driven.text("Your move? ");
    driven.gmcp("Talk.Said", r#"{"line": "Mira says 'hi'"}"#);
    driven.text("north\r\n");
    let lines = chat(&driven);
    let plain: Vec<&str> = lines.iter().map(Line::plain).collect();
    assert_eq!(plain, ["Your move? ", "Mira says 'hi'", "north"]);
}

/// Another player's words, as a channel, match a map exits rule, a mob's
/// aggression and a wear message. None of them is the game's own word.
const MIMIC: &str =
    "Mira gossips 'Exits: none. A wolf attacks you! You wear a copper ring on your finger.'";

#[test]
fn a_channel_leaves_the_map_and_gear_alone() {
    let mimicked = plugin(
        "",
        &[
            ("text.toml", "[[channel]]\nmessage = \"Talk.Said.line\"\n"),
            (
                "map.toml",
                "[[exits]]\nname = \"map.exits\"\npattern = '''Exits:'''\n",
            ),
            (
                "mobs.toml",
                "[[aggro]]\nname = \"mobs.aggro\"\npattern = '''(?<name>A wolf) attacks you'''\n",
            ),
            (
                "gear.toml",
                "[[wear]]\nname = \"gear.wore\"\npattern = '''You wear (?<item>.+?) on your \\w+\\.'''\n",
            ),
        ],
    );
    let mut driven = Driven::with(mimicked);
    driven.gmcp(
        "Room.Info",
        r#"{"num": 1, "name": "A lane", "exits": {"n": 2}}"#,
    );
    driven.gmcp(
        "Char.Items.List",
        r#"{"location": "inv", "items": [{"id": "7", "name": "a copper ring"}]}"#,
    );
    let aggressive = |driven: &Driven| {
        driven.read(|map: &MapState| {
            let room = map.rooms().next().expect("the room");
            room.features.contains(feature::AGGRESSIVE_MOB)
        })
    };
    let carried = |driven: &Driven| driven.read(|items: &ItemsState| items.carried.len());

    driven.gmcp("Talk.Said", &format!(r#"{{"line": "{MIMIC}"}}"#));
    assert!(
        chat(&driven).iter().any(|line| line.plain() == MIMIC),
        "it shows"
    );
    assert!(!aggressive(&driven), "no mob attacked");
    assert_eq!(carried(&driven), 1, "the ring wasn't put on");

    // The same words from the game itself are read.
    driven.text(&format!("{MIMIC}\r\n"));
    assert!(aggressive(&driven));
    assert_eq!(carried(&driven), 0, "the ring went on");
}
