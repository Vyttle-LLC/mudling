//! Gear sets through the whole session with Avatar on: whose they are, what
//! Wear sends, and what happens as the game answers.

use mudling_core::persist::{Database, GearPiece, GearSet, Timestamp};
use mudling_core::session::{Engine, GearEdit, SessionConfig, Typing};
use mudling_core::store::{ItemsState, SettingsState, StoreHub, Versioned};
use mudling_core::telnet::Bytes;
use mudling_core::telnet::command::{IAC, SB, SE, WILL, WONT};
use mudling_core::telnet::option::{ECHO, GMCP};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const HOST: &str = "avatar.outland.org";
const NOW: Timestamp = Timestamp(0);
const SATCHEL_LIST: &str = r#"{"location":"inv","items":[
    {"id":"30","name":"a satchel","type":"container"},
    {"id":"1","name":"a linen robe","type":"armor"},
    {"id":"2","name":"an amulet of Gorn","type":"armor"},
    {"id":"3","name":"an amulet of Gorn","type":"armor"}]}"#;

fn engine(folder: &Path) -> (Engine, Arc<StoreHub>) {
    let plugin = mudling_plugin_avatar::load();
    let config = SessionConfig {
        enabled: vec![plugin.id().clone()],
        plugins: vec![plugin],
        ..SessionConfig::new(folder, HOST, 4000)
    };
    let hub = Arc::new(StoreHub::new(|| {}));
    let typing = Arc::new(Mutex::new(Typing::default()));
    let mut engine = Engine::open(config, Arc::clone(&hub), typing).unwrap();
    engine.start_connection().unwrap();
    (engine, hub)
}

fn gmcp(package: &str, json: &str) -> Bytes {
    let mut frame = vec![IAC, SB, GMCP];
    frame.extend_from_slice(format!("{package} {json}").as_bytes());
    frame.extend_from_slice(&[IAC, SE]);
    Bytes::from(frame)
}

fn items(hub: &StoreHub) -> ItemsState {
    hub.read(|items: &Versioned<ItemsState>| items.value().clone())
}

/// Settles until `done` says the hooks' writes have landed.
fn settle_until(engine: &mut Engine, hub: &StoreHub, done: impl Fn(&ItemsState) -> bool) {
    let started = Instant::now();
    while !done(&items(hub)) {
        assert!(started.elapsed() < Duration::from_secs(5), "never landed");
        engine.settle(NOW);
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn say(engine: &mut Engine, text: impl Into<Vec<u8>>, now: Timestamp) {
    engine.receive(Bytes::from(text.into()), now).unwrap();
    engine.quiet(now);
}

fn named(engine: &mut Engine, who: &str) {
    let status = format!(r#"{{"character_name":"{who}"}}"#);
    engine.receive(gmcp("Char.Status", &status), NOW).unwrap();
    engine.settle(NOW);
}

fn sent(engine: &mut Engine) -> String {
    let output = engine.take_output();
    String::from_utf8_lossy(&output).into_owned()
}

fn sent_lines(engine: &mut Engine) -> Vec<String> {
    (sent(engine).split("\r\n").map(str::trim))
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

fn piece(item: &str, slot: &str, copy: Option<&str>) -> GearPiece {
    GearPiece {
        item: item.to_owned(),
        slot: Some(slot.to_owned()),
        copy_id: copy.map(str::to_owned),
    }
}

fn set(character: &str, name: &str, pieces: Vec<GearPiece>, container: Option<&str>) -> GearSet {
    GearSet {
        id: String::new(),
        character_id: character.to_owned(),
        name: name.to_owned(),
        container: container.map(str::to_owned),
        pieces,
        updated: NOW,
    }
}

fn saved(engine: &mut Engine, hub: &StoreHub, set: GearSet) -> String {
    let name = set.name.clone();
    engine.edit_gear(GearEdit::SaveSet(set), NOW);
    settle_until(engine, hub, |items| {
        items.sets.iter().any(|set| set.name == name)
    });
    let sets = items(hub).sets;
    let found = sets.iter().find(|set| set.name == name).unwrap();
    found.id.clone()
}

fn notices(hub: &StoreHub) -> Vec<String> {
    hub.read(|settings: &Versioned<SettingsState>| settings.value().notices.clone())
}

/// Fen, on a connection, wearing a hat, with a bag and its contents.
fn ready(engine: &mut Engine, hub: &StoreHub) {
    engine
        .receive(Bytes::from(vec![IAC, WILL, GMCP]), NOW)
        .unwrap();
    named(engine, "Fen");
    let list = gmcp("Char.Items.List", SATCHEL_LIST);
    engine.receive(list, NOW).unwrap();
    let inside = r#"{"location":"30","items":[{"id":"4","name":"a Tiamat mask","type":"armor"}]}"#;
    engine.receive(gmcp("Char.Items.List", inside), NOW).unwrap();
    let eq = "You are using:\r\n<worn on head>      a red hat\r\n(1h-1m-1v-1)(-/-)\r\n";
    say(engine, eq, NOW);
    settle_until(engine, hub, |items| {
        !items.worn.is_empty() && !items.bags.is_empty() && items.rules.capacity("neck") == 2
    });
    engine.take_output();
}

#[test]
fn the_plugin_says_how_its_slots_are_filled() {
    let folder = tempfile::tempdir().unwrap();
    let (mut engine, hub) = engine(folder.path());
    ready(&mut engine, &hub);
    let rules = items(&hub).rules;
    assert_eq!(rules.capacity("finger"), 2);
    assert_eq!(rules.capacity("wrist"), 2);
    assert_eq!(rules.capacity("head"), 1);
}

#[test]
fn a_bag_is_asked_about_and_its_contents_reach_the_store() {
    let folder = tempfile::tempdir().unwrap();
    let (mut engine, hub) = engine(folder.path());
    engine
        .receive(Bytes::from(vec![IAC, WILL, GMCP]), NOW)
        .unwrap();
    engine.take_output();
    engine
        .receive(gmcp("Char.Items.List", SATCHEL_LIST), NOW)
        .unwrap();
    assert!(sent(&mut engine).contains("Char.Items.Contents 30"));
    engine
        .receive(gmcp("Char.Items.List", SATCHEL_LIST), NOW)
        .unwrap();
    assert!(!sent(&mut engine).contains("Char.Items.Contents"));

    let inside = r#"{"location":"30","items":[{"id":"4","name":"a Tiamat mask"}]}"#;
    engine.receive(gmcp("Char.Items.List", inside), NOW).unwrap();
    settle_until(&mut engine, &hub, |items| {
        items.bags.first().is_some_and(|bag| bag.items.len() == 1)
    });
}

#[test]
fn sets_are_their_characters_and_come_back_saved() {
    let folder = tempfile::tempdir().unwrap();
    {
        let (mut engine, hub) = engine(folder.path());
        named(&mut engine, "Fen");
        let tank = set("fen", "Tank", vec![piece("a red hat", "head", None)], None);
        saved(&mut engine, &hub, tank);

        named(&mut engine, "Ash");
        let state = items(&hub);
        assert!(state.sets.is_empty(), "Fen's sets aren't Ash's");
        assert_eq!(&*state.character, "ash");
        engine.finish().unwrap();
    }
    let (mut engine, hub) = engine(folder.path());
    assert!(items(&hub).sets.is_empty(), "nobody is named yet");
    named(&mut engine, "Fen");
    settle_until(&mut engine, &hub, |items| items.sets.len() == 1);
    let state = items(&hub);
    assert_eq!(state.sets[0].name, "Tank");
    assert_eq!(state.sets[0].pieces[0].item, "a red hat");
}

#[test]
fn a_change_for_someone_no_longer_playing_goes_nowhere() {
    let folder = tempfile::tempdir().unwrap();
    let (mut engine, hub) = engine(folder.path());
    named(&mut engine, "Fen");
    let id = saved(&mut engine, &hub, set("fen", "Tank", Vec::new(), None));
    named(&mut engine, "Ash");
    engine.settle(NOW);

    engine.edit_gear(GearEdit::SaveSet(set("fen", "Stale", Vec::new(), None)), NOW);
    let remove = GearEdit::RemoveSet {
        character: "fen".to_owned(),
        id: id.clone(),
    };
    engine.edit_gear(remove, NOW);
    engine.wear_set("fen", &id, NOW);
    engine.settle(NOW);
    assert!(items(&hub).sets.is_empty());
    engine.finish().unwrap();

    let database = Database::open(&folder.path().join("servers"), HOST).unwrap();
    let rows = database.reader().unwrap().characters().unwrap().gear_sets;
    let names: Vec<&str> = rows.iter().map(|set| set.name.as_str()).collect();
    assert_eq!(names, ["Tank"]);
}

#[test]
fn nobody_has_sets_until_this_connection_names_them() {
    let folder = tempfile::tempdir().unwrap();
    let (mut engine, hub) = engine(folder.path());
    engine.edit_gear(GearEdit::SaveSet(set("", "Lost", Vec::new(), None)), NOW);
    engine.settle(NOW);
    assert!(items(&hub).sets.is_empty());
}

#[test]
fn a_set_deleted_stays_deleted() {
    let folder = tempfile::tempdir().unwrap();
    {
        let (mut engine, hub) = engine(folder.path());
        named(&mut engine, "Fen");
        let id = saved(&mut engine, &hub, set("fen", "Tank", Vec::new(), None));
        let remove = GearEdit::RemoveSet {
            character: "fen".to_owned(),
            id,
        };
        engine.edit_gear(remove, NOW);
        settle_until(&mut engine, &hub, |items| items.sets.is_empty());
        engine.finish().unwrap();
    }
    let (mut engine, hub) = engine(folder.path());
    named(&mut engine, "Fen");
    engine.settle(NOW);
    assert!(items(&hub).sets.is_empty());
}

#[test]
fn saving_a_known_set_changes_it_in_place() {
    let folder = tempfile::tempdir().unwrap();
    let (mut engine, hub) = engine(folder.path());
    named(&mut engine, "Fen");
    let id = saved(&mut engine, &hub, set("fen", "Tank", Vec::new(), None));
    let renamed = GearSet {
        id: id.clone(),
        ..set("fen", "Hit", Vec::new(), None)
    };
    saved(&mut engine, &hub, renamed);
    let sets = items(&hub).sets;
    assert_eq!(sets.len(), 1);
    assert_eq!((sets[0].id.as_str(), sets[0].name.as_str()), (id.as_str(), "Hit"));
}

#[test]
fn wear_sends_the_plan_with_the_games_ids_and_the_chat_shows_it() {
    let folder = tempfile::tempdir().unwrap();
    let (mut engine, hub) = engine(folder.path());
    ready(&mut engine, &hub);
    let pieces = vec![
        piece("a Tiamat mask", "head", None),
        piece("an amulet of Gorn", "neck", Some("3")),
    ];
    let id = saved(&mut engine, &hub, set("fen", "Tank", pieces, Some("a satchel")));
    let preview = items(&hub).plan(&items(&hub).sets[0]);

    engine.wear_set("fen", &id, NOW);
    assert_eq!(
        sent_lines(&mut engine),
        ["remove hat", "get 4 30", "put hat 30", "wear 4", "wear 3"]
    );
    assert_eq!(preview.commands[1], "get 4 30", "what the editor shows");
}

#[test]
fn wear_is_refused_while_echo_is_off() {
    let folder = tempfile::tempdir().unwrap();
    let (mut engine, hub) = engine(folder.path());
    ready(&mut engine, &hub);
    let pieces = vec![piece("a linen robe", "about", None)];
    let id = saved(&mut engine, &hub, set("fen", "Robe", pieces, None));

    engine.receive(Bytes::from(vec![IAC, WILL, ECHO]), NOW).unwrap();
    engine.take_output();
    engine.wear_set("fen", &id, NOW);
    assert_eq!(sent(&mut engine), "");

    engine.receive(Bytes::from(vec![IAC, WONT, ECHO]), NOW).unwrap();
    engine.take_output();
    engine.wear_set("fen", &id, NOW);
    assert_eq!(sent_lines(&mut engine), ["wear 1"]);
}

#[test]
fn a_put_away_after_echo_went_off_is_refused_too() {
    let folder = tempfile::tempdir().unwrap();
    let (mut engine, hub) = engine(folder.path());
    ready(&mut engine, &hub);
    let pieces = vec![piece("a linen robe", "about", None)];
    let id = saved(&mut engine, &hub, set("fen", "Robe", pieces, Some("a satchel")));
    engine.wear_set("fen", &id, NOW);
    engine.take_output();

    engine.receive(Bytes::from(vec![IAC, WILL, ECHO]), NOW).unwrap();
    engine.take_output();
    say(&mut engine, "You stop using a cloak.\r\n", NOW);
    assert_eq!(sent(&mut engine), "");
}

#[test]
fn what_a_piece_of_unknown_slot_swaps_out_goes_in_the_container() {
    let folder = tempfile::tempdir().unwrap();
    let (mut engine, hub) = engine(folder.path());
    ready(&mut engine, &hub);
    let mut robe = piece("a linen robe", "about", None);
    robe.slot = None;
    let id = saved(&mut engine, &hub, set("fen", "Robe", vec![robe], Some("a satchel")));
    engine.wear_set("fen", &id, NOW);
    assert_eq!(sent_lines(&mut engine), ["wear 1"]);

    say(&mut engine, "You stop using a cloak.\r\n", NOW);
    assert_eq!(sent_lines(&mut engine), ["put cloak 30"]);

    say(&mut engine, "You stop using a linen robe.\r\n", NOW);
    assert_eq!(sent_lines(&mut engine), Vec::<String>::new(), "its own piece");

    let later = Timestamp(NOW.0 + 60_000);
    say(&mut engine, "You stop using a sash.\r\n", later);
    assert_eq!(sent_lines(&mut engine), Vec::<String>::new(), "long over");
}

#[test]
fn a_full_container_is_said_to_the_player() {
    let folder = tempfile::tempdir().unwrap();
    let (mut engine, hub) = engine(folder.path());
    ready(&mut engine, &hub);
    let pieces = vec![piece("a Tiamat mask", "head", None)];
    let id = saved(&mut engine, &hub, set("fen", "Mask", pieces, Some("a satchel")));
    engine.wear_set("fen", &id, NOW);
    engine.take_output();
    say(&mut engine, "You stop using a red hat.\r\n", NOW);
    assert_eq!(sent_lines(&mut engine), Vec::<String>::new(), "already on its way");

    say(&mut engine, "It won't fit.\r\n", NOW);
    let said = notices(&hub);
    assert!(
        said.iter()
            .any(|notice| notice == "a red hat didn't fit in a satchel, so it's still carried."),
        "{said:?}"
    );
}

#[test]
fn the_copy_a_plan_named_is_the_copy_the_worn_list_keeps() {
    let folder = tempfile::tempdir().unwrap();
    let (mut engine, hub) = engine(folder.path());
    ready(&mut engine, &hub);
    let pieces = vec![piece("an amulet of Gorn", "neck", Some("3"))];
    let id = saved(&mut engine, &hub, set("fen", "Amulet", pieces, None));
    engine.wear_set("fen", &id, NOW);
    say(&mut engine, "You wear an amulet of Gorn around your neck.\r\n", NOW);
    settle_until(&mut engine, &hub, |items| {
        items.worn.iter().any(|worn| worn.slot == "neck")
    });
    let state = items(&hub);
    let neck = state.worn.iter().find(|worn| worn.slot == "neck").unwrap();
    assert_eq!(neck.copy.as_deref(), Some("3"));
    let left: Vec<&str> = state.carried.iter().map(|item| item.id.as_str()).collect();
    assert!(left.contains(&"2") && !left.contains(&"3"), "{left:?}");
}

#[test]
fn a_piece_not_seen_is_said_and_the_rest_still_go_on() {
    let folder = tempfile::tempdir().unwrap();
    let (mut engine, hub) = engine(folder.path());
    ready(&mut engine, &hub);
    let pieces = vec![
        piece("a linen robe", "about", None),
        piece("an alligator tooth", "held", None),
    ];
    let id = saved(&mut engine, &hub, set("fen", "Hit", pieces, None));
    engine.wear_set("fen", &id, NOW);
    assert_eq!(sent_lines(&mut engine), ["wear 1"]);
    let said = notices(&hub);
    assert!(said.iter().any(|notice| notice.contains("an alligator tooth")));
}

#[test]
fn a_set_all_on_says_so_and_sends_nothing() {
    let folder = tempfile::tempdir().unwrap();
    let (mut engine, hub) = engine(folder.path());
    ready(&mut engine, &hub);
    let pieces = vec![piece("a red hat", "head", None)];
    let id = saved(&mut engine, &hub, set("fen", "Hat", pieces, None));
    engine.wear_set("fen", &id, NOW);
    assert_eq!(sent(&mut engine), "");
    assert!(notices(&hub).iter().any(|notice| notice == "Hat is already on."));
}
