//! The prompt and pager, the map's messages, mobs, and how Avatar takes
//! commands.

use mudling_core::line::LinePipeline;
use mudling_core::plugin::{PendingKind, PluginHost};
use mudling_plugin_avatar::{Transcript, load};
use std::collections::BTreeMap;

fn read(fixture: &str) -> Transcript {
    let transcript = Transcript::of(fixture);
    assert!(transcript.problems().is_empty(), "{:?}", transcript.problems());
    transcript
}

/// What the host makes of `text` sent without a line end.
fn pending(host: &mut PluginHost, text: &str) -> Option<PendingKind> {
    let mut pipeline = LinePipeline::default();
    pipeline.push(text.as_bytes(), &mut Vec::new());
    host.read_pending(&pipeline.pending().unwrap())
}

#[test]
fn a_whole_prompt_is_recognized_and_half_of_one_is_not() {
    let mut host = PluginHost::new([load()]).unwrap();
    for prompt in [
        "(110h-125m-120v-1062)(-/-)",
        "(599hp 1173m 514v 1046tnl)(-/-) ",
        "$*(110h-125m-120v-1062)(-/-)",
        "(AFK) (110h-125m-120v-1062)(-/-)",
        "<46/46hp><116/116m><116/116mv>",
    ] {
        assert_eq!(pending(&mut host, prompt), Some(PendingKind::Prompt), "{prompt}");
    }
    // A network read can split a prompt; the half isn't one.
    for half in ["(271h-17", "(271h-170m-90v-1000)", "<46/46hp>", "(Magical) a ring", "<worn on finger>"] {
        assert_eq!(pending(&mut host, half), None, "{half}");
    }
}

#[test]
fn the_pager_continues_with_c() {
    let mut host = PluginHost::new([load()]).unwrap();
    let pager = pending(&mut host, "[B]ack,[H]elp,[R]efresh, or [C]ontinue: ");
    assert_eq!(pager, Some(PendingKind::Pager { key: "c".into() }));
    assert_eq!(host.settings().pager.as_ref().unwrap().key, "c");
}

#[test]
fn area_names_drop_the_level_range_and_builder() {
    let transcript = read(include_str!("fixtures/map.txt"));
    let areas = transcript.events("map.area", &["low", "high", "name"]);
    let area = |low: &str, high: &str, name: &str| vec![Some(low.to_owned()), Some(high.to_owned()), Some(name.to_owned())];
    assert_eq!(
        areas,
        [
            area("10", "15", "Sarphyre's Court"),
            // One space after the builder: no telling it from the name.
            area("51", "51", "Izanagi The Deepways"),
            area("1", "4", "The Meadow"),
        ]
    );
}

#[test]
fn failed_moves_darkness_and_flees_are_map_events() {
    let transcript = read(include_str!("fixtures/map.txt"));
    assert_eq!(transcript.events("map.flee", &["dir"]), [[Some("north".to_owned())]]);
    assert_eq!(transcript.events("map.failed_move", &[]).len(), 2, "still fighting, and too exhausted");
    assert_eq!(transcript.events("map.darkness", &[]).len(), 1);
}

#[test]
fn a_mob_attacking_on_sight_is_named_by_its_noun() {
    let transcript = read(include_str!("fixtures/mobs.txt"));
    let owned = |name: &str, noun: &str| vec![Some(name.to_owned()), Some(noun.to_owned())];
    assert_eq!(
        transcript.events("mobs.aggro", &["name", "noun"]),
        [owned("The nanny", "nanny"), owned("An old doll", "doll")],
        "helping a friend in a fight isn't aggression"
    );
}

#[test]
fn a_room_title_gives_the_mob_name() {
    let plugin = load();
    let rule = &plugin.points.mobs.as_ref().unwrap().room_title[0];
    let name = |title: &str| {
        let captures = rule.pattern.captures(title).unwrap_or_else(|| panic!("{title}"));
        let capture = captures.iter().find(|capture| &*capture.name == "name").unwrap();
        title[capture.start as usize..capture.end as usize].to_owned()
    };
    assert_eq!(name("(White Aura) A myconoid shaman is here, chanting."), "A myconoid shaman");
    assert_eq!(name("(Flying) A pitch black wasp buzzes through the air, seeking new victims."), "A pitch black wasp buzzes through the air, seeking new victims");
    assert_eq!(name("Sol the Healer is sleeping here."), "Sol the Healer");
    assert_eq!(name("Two guards are standing here."), "Two guards");
}

#[test]
fn commands_go_one_per_line_with_the_games_verbs_and_doors() {
    let host = PluginHost::new([load()]).unwrap();
    assert_eq!(host.settings().separator, None, "no separator: one command a line");
    let (_, commands) = host.declared(|points| points.commands.as_ref()).next().unwrap();
    let verbs: BTreeMap<&str, &str> = commands.verbs.iter().map(|(verb, word)| (verb.as_str(), word.as_str())).collect();
    for verb in ["wear", "wield", "hold", "remove", "get", "put", "look", "examine", "kill", "flee", "cast"] {
        assert_eq!(verbs.get(verb), Some(&verb), "{verb}");
    }
    assert_eq!(commands.doors["open"], "open {dir}");
    assert_eq!(commands.doors["unlock"], "unlock {dir}");
    assert!(commands.speedwalk.is_none(), "the game has none; the client expands shorthand");
}

#[test]
fn the_map_knows_rooms_by_description_and_its_maze() {
    let plugin = load();
    let map = plugin.points.map.as_ref().unwrap();
    assert_eq!(map.room_identity, Some(mudling_core::plugin::points::RoomIdentity::Description));
    assert_eq!(map.mazes, ["Brothers Grimm"]);
}
