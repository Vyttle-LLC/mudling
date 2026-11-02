//! What Aardwolf's GMCP becomes in a session with the plugin on: the map
//! from room.info, and the vitals and level from char.vitals, char.maxstats,
//! char.status and char.base.

use mudling_core::defaults::hud::Vital;
use mudling_core::plugin::luau::Value;
use mudling_core::store::{CHARACTER_LEVEL, CharactersState, MapState, RoomState, VitalsState};
use mudling_plugin_aardwolf::Played;

fn sample() -> Played {
    Played::log(include_str!("sample.txt"))
}

#[test]
fn room_info_places_the_player_and_maps_each_room_by_number() {
    let played = sample();
    let room = played.read(|room: &RoomState| room.room.clone()).unwrap();
    assert_eq!(room.name, "A Gravel Path");
    assert_eq!(room.zone, "hedgerow");
    played.read(|map: &MapState| {
        let names: Vec<&str> = map.rooms().map(|room| room.name.as_str()).collect();
        assert_eq!(names, ["A Quiet Courtyard", "A Gravel Path"]);
        assert_eq!(map.whereabouts().room, Some(room.id));
    });
}

#[test]
fn every_vital_comes_from_where_aardwolf_keeps_it() {
    let vitals = sample().read(|vitals: &VitalsState| vitals.0.clone());
    let expected = [
        (Vital::Hp, 400),
        (Vital::MaxHp, 420),
        (Vital::Mana, 610),
        (Vital::MaxMana, 610),
        (Vital::Moves, 512),
        (Vital::MaxMoves, 530),
        (Vital::ToNextLevel, 980),
        (Vital::MaxToNextLevel, 1500),
    ];
    for (vital, value) in expected {
        assert_eq!(vitals.get(&vital), Some(&value), "{vital:?}");
    }
}

#[test]
fn char_status_gives_the_level() {
    let character = sample().read(|characters: &CharactersState| characters.0.clone());
    assert_eq!(character.get(CHARACTER_LEVEL), Some(&Value::Number(12.0)));
}
