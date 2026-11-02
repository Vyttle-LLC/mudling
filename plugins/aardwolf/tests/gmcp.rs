//! What Aardwolf's GMCP becomes in a session with the plugin on: the map
//! from room.info; the vitals from char.vitals, char.maxstats, char.status
//! and char.base; the sheet from char.base, char.stats, char.status and
//! char.worth; and the opponent from char.status.

use mudling_core::defaults::hud::Vital;
use mudling_core::plugin::luau::Value;
use mudling_core::store::{
    CHARACTER_LEVEL, CHARACTER_NAME, CHARACTER_POSITION, CharactersState, MapState, RoomState,
    Target, TargetState, VitalsState,
};
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
fn the_sheet_comes_from_char_base_stats_status_and_worth() {
    let character = sample().read(|characters: &CharactersState| characters.0.clone());
    let sheet = |key: &str| character.get(key).cloned();
    let text = |text: &str| Some(Value::Text(text.to_owned()));
    let number = |number: f64| Some(Value::Number(number));
    assert_eq!(sheet(CHARACTER_NAME), text("Wren"));
    assert_eq!(sheet(CHARACTER_LEVEL), number(12.0));
    assert_eq!(sheet(CHARACTER_POSITION), text("Fighting"));
    assert_eq!(sheet("character.class"), text("Mage"));
    assert_eq!(sheet("character.str"), number(18.0));
    assert_eq!(sheet("character.hitroll"), number(30.0));
    assert_eq!(sheet("character.alignment"), number(850.0));
    assert_eq!(sheet("character.gold"), number(2450.0));
    assert_eq!(sheet("character.practice"), number(7.0));
}

#[test]
fn the_opponent_is_named_with_its_health_as_a_percentage() {
    let target = sample().read(|target: &TargetState| target.0.clone());
    assert_eq!(
        target,
        Some(Target {
            name: "a bramble sprite".to_owned(),
            level: None,
            health: Some(64),
            max_health: Some(100),
        })
    );
}
