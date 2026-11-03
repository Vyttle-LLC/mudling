//! What a plugin says about a game's GMCP, through a session engine: where
//! gear's stats are, where a door that changed lists the exits, and which
//! message names an area. The game is made up, its packages named unlike
//! any real game's, so nothing here can lean on a name the core knows.

use mudling_core::gear::ItemRef;
use mudling_core::map::Direction;
use mudling_core::persist::Timestamp;
use mudling_core::plugin::Plugin;
use mudling_core::session::{Engine, SessionConfig, Typing};
use mudling_core::store::{ItemsState, MapState, Store, StoreHub, Versioned};
use mudling_core::telnet::{Bytes, gmcp};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

const MANIFEST: &str = "id = \"madeup\"\nname = \"Made Up\"\nversion = \"1.0.0\"\napi = 1\n\
                        license = \"MIT\"\nlayer = \"game\"\n\
                        [servers]\nhosts = [\"mud.example\"]\n";

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
        let folder = tempfile::tempdir().unwrap();
        let hub = Arc::new(StoreHub::new(|| {}));
        let config = SessionConfig {
            enabled: vec![plugin.id().clone()],
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
