//! Avatar through the whole session: suggested at runtime by its host, and
//! once turned on, its hooks' writes reaching the stores.

use mudling_core::defaults::detect::Source;
use mudling_core::gear::{GearBook, ItemRef};
use mudling_core::net::DisconnectReason;
use mudling_core::persist::{Database, Timestamp};
use mudling_core::plugin::luau::Value;
use mudling_core::session::{Credentials, Engine, Session, SessionConfig, Typing};
use mudling_core::store::{
    CharactersState, ConnectionStatus, GearState, ItemsState, RoomState, SettingsState, StoreHub,
    Versioned,
};
use mudling_core::telnet::Bytes;
use mudling_core::telnet::command::{IAC, SB, SE};
use mudling_core::telnet::option::GMCP;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const HOST: &str = "avatar.outland.org";
const PROMPT: &[u8] = b"(120h-130m-104v-1210)(-/-)";

/// An engine with Avatar on, its connection started, and its stores.
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

#[test]
fn the_login_prompts_are_each_answered_once() {
    let folder = tempfile::tempdir().unwrap();
    let plugin = mudling_plugin_avatar::load();
    let config = SessionConfig {
        enabled: vec![plugin.id().clone()],
        plugins: vec![plugin],
        credentials: Some(Credentials {
            name: "Fen".to_owned(),
            password: "pass-word".to_owned(),
        }),
        ..SessionConfig::new(folder.path(), HOST, 4000)
    };
    let hub = Arc::new(StoreHub::new(|| {}));
    let typing = Arc::new(Mutex::new(Typing::default()));
    let mut engine = Engine::open(config, hub, typing).unwrap();
    engine.start_connection().unwrap();
    let mut answer = |prompt: &'static [u8]| {
        let now = Timestamp(0);
        engine.receive(Bytes::from_static(prompt), now).unwrap();
        engine.quiet(now);
        String::from_utf8(engine.take_output()).unwrap()
    };

    let name = answer(b"What name shall you be known by, adventurer? ");
    assert_eq!(name.trim(), "Fen");
    let password = answer(b"Your Password: ");
    assert_eq!(password.trim(), "pass-word");
    let banner = answer(b"When you have read this, please press <RETURN> to continue ---->");
    assert_eq!(banner.trim(), "");
    assert!(!banner.is_empty(), "the banner wants a blank line");
    assert_eq!(answer(b"Your Password: "), "", "a second ask is a refusal");
}

/// One connection as `name`, reading `text`; what's saved stays in `folder`.
fn play_as(name: &str, folder: &Path, text: &'static [u8]) -> Vec<(String, String)> {
    let plugin = mudling_plugin_avatar::load();
    let config = SessionConfig {
        enabled: vec![plugin.id().clone()],
        plugins: vec![plugin],
        credentials: Some(Credentials {
            name: name.to_owned(),
            password: "pass-word".to_owned(),
        }),
        ..SessionConfig::new(folder, HOST, 4000)
    };
    let hub = Arc::new(StoreHub::new(|| {}));
    let typing = Arc::new(Mutex::new(Typing::default()));
    let mut engine = Engine::open(config, hub, typing).unwrap();
    engine.start_connection().unwrap();
    let now = Timestamp(0);
    engine.receive(Bytes::from_static(text), now).unwrap();
    engine.close_connection(DisconnectReason::ClosedByServer, now);
    engine.finish().unwrap();
    let database = Database::open(&folder.join("servers"), HOST).unwrap();
    let stats = database.reader().unwrap().characters().unwrap().stats;
    stats
        .into_iter()
        .filter(|stat| stat.key.starts_with("worn.") && stat.key.ends_with(".item"))
        .map(|stat| (stat.character_id, stat.value))
        .collect()
}

#[test]
fn each_character_keeps_what_it_wears() {
    let folder = tempfile::tempdir().unwrap();
    play_as(
        "Fen",
        folder.path(),
        b"You are using:\r\n<worn on head>      a red hat\r\n(1h-1m-1v-1)(-/-)\r\n",
    );
    let saved = play_as(
        "Ash",
        folder.path(),
        b"You are using:\r\n<worn on feet>      blue boots\r\n(1h-1m-1v-1)(-/-)\r\n",
    );
    assert_eq!(
        saved,
        [
            ("ash".to_owned(), "\"blue boots\"".to_owned()),
            ("fen".to_owned(), "\"a red hat\"".to_owned()),
        ]
    );
}

#[test]
fn a_spell_identify_is_filed_under_the_one_item_at_hand_it_fits() {
    let folder = tempfile::tempdir().unwrap();
    let (mut engine, hub) = engine(folder.path());
    let now = Timestamp(0);
    let items = r#"{"location":"inv","items":[
        {"id":"1","name":"(Glowing) an amulet of Gorn"},
        {"id":"2","name":"a pink ice ring"}]}"#;
    engine
        .receive(gmcp("Char.Items.List", &items.replace('\n', " ")), now)
        .unwrap();
    engine.settle(now);
    let identify = b"Object 'gorn amulet newbie' type armor, extra flags none.\r\n\
        Weight 3, value 40, level 8.\r\n\
        (119h-130m-104v-1210)(-/-)\r\n";
    engine.receive(Bytes::from_static(identify), now).unwrap();
    engine.close_connection(DisconnectReason::ClosedByServer, now);

    let keywords = hub.read(|gear: &Versioned<GearState>| {
        gear.value()
            .0
            .get("gear.keywords.an amulet of gorn")
            .cloned()
    });
    assert_eq!(keywords, Some(Value::Text("gorn amulet newbie".to_owned())));
}

fn items(hub: &StoreHub) -> ItemsState {
    hub.read(|items: &Versioned<ItemsState>| items.value().clone())
}

/// Settles until `done` says the hooks' writes have landed.
fn settle_until(engine: &mut Engine, hub: &StoreHub, done: impl Fn(&ItemsState) -> bool) {
    let started = Instant::now();
    while !done(&items(hub)) {
        assert!(started.elapsed() < Duration::from_secs(5), "never landed");
        engine.settle(Timestamp(0));
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn carrying(engine: &mut Engine, id: &str, name: &str) {
    let list = format!(r#"{{"location":"inv","items":[{{"id":"{id}","name":"{name}"}}]}}"#);
    let now = Timestamp(0);
    engine.receive(gmcp("Char.Items.List", &list), now).unwrap();
    engine.settle(now);
}

const SHOP_MASK: &str = "Ulric looks closely at your a Tiamat mask.\r\n\
    Object 'mask Tiamat' type armor, extra flags evil.\r\n\
    Weight 3, value 40, level 2.\r\n\
    Armor class is {armor}.\r\n\r\n";

#[test]
fn two_copies_with_one_name_keep_a_reading_each_and_come_back_saved() {
    let folder = tempfile::tempdir().unwrap();
    let (mut engine, hub) = engine(folder.path());
    let now = Timestamp(0);
    for (id, armor, known) in [("1", "1", 1), ("2", "3", 2)] {
        carrying(&mut engine, id, "a Tiamat mask");
        let identify = SHOP_MASK.replace("{armor}", armor);
        engine.receive(Bytes::from(identify), now).unwrap();
        engine.receive(Bytes::from_static(PROMPT), now).unwrap();
        engine.quiet(now);
        settle_until(&mut engine, &hub, |items| {
            let mask = items.book.item("a tiamat mask");
            mask.is_some_and(|mask| mask.readings.len() == known)
        });
    }
    let check = |book: &GearBook| {
        let mask = book.item("a tiamat mask").unwrap();
        assert_eq!(mask.facts.keywords.as_deref(), Some("mask Tiamat"));
        let armor = |copy| {
            let item = ItemRef {
                name: "a Tiamat mask",
                copy: Some(copy),
            };
            book.stats(item).unwrap().effects()["ac"]
        };
        assert_eq!((armor("1"), armor("2")), (-1, -3));
    };
    check(&items(&hub).book);
    engine.finish().unwrap();

    let database = Database::open(&folder.path().join("servers"), HOST).unwrap();
    check(&GearBook::from_rows(
        database.reader().unwrap().gear().unwrap(),
    ));
}

#[test]
fn wearing_one_item_measures_it_and_moves_it_to_the_worn_list() {
    let folder = tempfile::tempdir().unwrap();
    let (mut engine, hub) = engine(folder.path());
    let now = Timestamp(0);
    engine
        .receive(gmcp("Char.Status", r#"{"str":"15","hitroll":"2"}"#), now)
        .unwrap();
    carrying(&mut engine, "7", "(Glowing) a pink ice ring");
    assert_eq!(items(&hub).carried.len(), 1);

    let wore = b"You wear a pink ice ring on your left finger.\r\n";
    engine.receive(Bytes::from_static(wore), now).unwrap();
    engine
        .receive(gmcp("Char.Status", r#"{"str":"16"}"#), now)
        .unwrap();
    settle_until(&mut engine, &hub, |items| {
        items.worn.iter().any(|worn| worn.item.contains("pink ice"))
    });

    let state = items(&hub);
    assert!(state.carried.is_empty(), "it isn't carried now");
    assert_eq!(state.worn[0].slot, "finger");
    assert_eq!(state.worn[0].copy.as_deref(), Some("7"));
    let ring = ItemRef {
        name: "a pink ice ring",
        copy: Some("7"),
    };
    assert_eq!(state.book.stats(ring).unwrap().effects()["str"], 1);

    let removed = b"(1h-1m-1v-1)(-/-)\r\nYou stop using a pink ice ring.\r\n";
    engine.receive(Bytes::from_static(removed), now).unwrap();
    engine.settle(now);
    let state = items(&hub);
    assert_eq!(state.worn[0].item, "", "the slot stays, empty");
    assert_eq!(state.carried[0].id, "7");
}

/// The game's list of two rings of one name, read to its end.
fn list_two_rings(engine: &mut Engine) {
    let now = Timestamp(0);
    let table = b"You are using:\r\n\
        <worn on finger>    a pink ring\r\n\
        <worn on finger>    a pink ring\r\n";
    engine.receive(Bytes::from_static(table), now).unwrap();
    engine.receive(Bytes::from_static(PROMPT), now).unwrap();
    engine.quiet(now);
    engine.close_connection(DisconnectReason::ClosedByServer, now);
}

#[test]
fn two_worn_copies_of_a_name_stay_two_across_a_reconnect() {
    let folder = tempfile::tempdir().unwrap();
    let (mut engine, hub) = engine(folder.path());
    let now = Timestamp(0);
    let list = r#"{"location":"inv","items":[
        {"id":"1","name":"a pink ring"},{"id":"2","name":"a pink ring"}]}"#;
    engine
        .receive(gmcp("Char.Items.List", &list.replace('\n', " ")), now)
        .unwrap();
    engine.settle(now);
    let wore = b"You wear a pink ring on your left finger.\r\n\
        You wear a pink ring on your right finger.\r\n";
    engine.receive(Bytes::from_static(wore), now).unwrap();
    settle_until(&mut engine, &hub, |items| items.worn.len() == 2);
    list_two_rings(&mut engine);

    let copies = |hub: &StoreHub| {
        let state = items(hub);
        let copies = state.worn.iter().map(|worn| worn.copy.clone());
        copies.collect::<Vec<_>>()
    };
    let both = [Some("1".to_owned()), Some("2".to_owned())];
    assert_eq!(copies(&hub), both);

    engine.start_connection().unwrap();
    list_two_rings(&mut engine);
    assert_eq!(copies(&hub), both);
}

#[test]
fn removing_the_second_of_two_rings_by_its_id_takes_that_copy_off() {
    let folder = tempfile::tempdir().unwrap();
    let (mut engine, hub) = engine(folder.path());
    let now = Timestamp(0);
    let list = r#"{"location":"inv","items":[
        {"id":"1","name":"a pink ring"},{"id":"2","name":"a pink ring"}]}"#;
    engine
        .receive(gmcp("Char.Items.List", &list.replace('\n', " ")), now)
        .unwrap();
    engine.settle(now);
    let wore = b"You wear a pink ring on your left finger.\r\n\
        You wear a pink ring on your right finger.\r\n";
    engine.receive(Bytes::from_static(wore), now).unwrap();
    settle_until(&mut engine, &hub, |items| items.worn.len() == 2);

    engine.send_action("remove 2", now);
    let removed = b"You stop using a pink ring.\r\n(1h-1m-1v-1)(-/-)";
    engine.receive(Bytes::from_static(removed), now).unwrap();
    engine.settle(now);

    let state = items(&hub);
    let on: Vec<_> = (state.worn.iter())
        .filter(|worn| !worn.item.is_empty())
        .map(|worn| worn.copy.as_deref())
        .collect();
    assert_eq!(on, [Some("1")], "copy 2 came off, not the first");
    let carried: Vec<_> = state.carried.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(carried, ["2"]);
}

#[test]
fn an_item_keeps_the_games_spelling_live_and_after_a_restart() {
    let folder = tempfile::tempdir().unwrap();
    let hat = "an official Cult of Bonk Hat";
    let name = |hub: &StoreHub| {
        let state = items(hub);
        state.book.item(hat).map(|item| item.name.clone())
    };
    let (mut engine, hub) = self::engine(folder.path());
    let now = Timestamp(0);
    let wore = format!("You wear {hat} on your head.\r\n");
    engine.receive(Bytes::from(wore), now).unwrap();
    settle_until(&mut engine, &hub, |items| items.book.item(hat).is_some());
    assert_eq!(name(&hub).as_deref(), Some(hat));
    engine.finish().unwrap();

    let (engine, hub) = self::engine(folder.path());
    engine.finish().unwrap();
    assert_eq!(name(&hub).as_deref(), Some(hat));
}

#[test]
fn the_matcher_learns_a_new_name_and_keeps_itself_for_anything_else() {
    let folder = tempfile::tempdir().unwrap();
    let (mut engine, hub) = engine(folder.path());
    let now = Timestamp(0);
    let wore = b"You wear a red hat on your head.\r\n";
    engine.receive(Bytes::from_static(wore), now).unwrap();
    settle_until(&mut engine, &hub, |items| !items.matcher.is_empty());
    let line = "You see a red hat.";
    let matcher = items(&hub).matcher;
    assert_eq!(matcher.find(line)[0].item, "a red hat");

    // The same fact again, then what's carried: no new names, no new matcher.
    engine.receive(Bytes::from_static(wore), now).unwrap();
    carrying(&mut engine, "1", "a tin cup");
    settle_until(&mut engine, &hub, |items| !items.carried.is_empty());
    assert!(Arc::ptr_eq(&matcher, &items(&hub).matcher));
}

#[test]
fn a_swap_measures_nothing() {
    let folder = tempfile::tempdir().unwrap();
    let (mut engine, hub) = engine(folder.path());
    let now = Timestamp(0);
    engine
        .receive(gmcp("Char.Status", r#"{"str":"15"}"#), now)
        .unwrap();
    let swap = b"You stop using a tin hat.\r\nYou wear a red hat on your head.\r\n";
    engine.receive(Bytes::from_static(swap), now).unwrap();
    engine
        .receive(gmcp("Char.Status", r#"{"str":"16"}"#), now)
        .unwrap();
    settle_until(&mut engine, &hub, |items| {
        items.book.item("a red hat").is_some()
    });
    assert_eq!(
        items(&hub).book.stats(ItemRef {
            name: "a red hat",
            copy: None
        }),
        None
    );
}

#[test]
fn another_character_on_the_session_sees_no_one_elses_gear() {
    let folder = tempfile::tempdir().unwrap();
    let (mut engine, hub) = engine(folder.path());
    let now = Timestamp(0);
    let name = |who: &str| gmcp("Char.Status", &format!(r#"{{"character_name":"{who}"}}"#));
    engine.receive(name("Fen"), now).unwrap();
    engine.settle(now);
    let eq = b"You are using:\r\n<worn on head>      a red hat\r\n(1h-1m-1v-1)(-/-)\r\n";
    engine.receive(Bytes::from_static(eq), now).unwrap();
    engine.quiet(now);
    settle_until(&mut engine, &hub, |items| !items.worn.is_empty());

    engine.receive(name("Ash"), now).unwrap();
    engine.settle(now);
    assert!(items(&hub).worn.is_empty());
}

#[test]
fn equipment_answering_nothing_empties_the_worn_list_and_its_rows() {
    let folder = tempfile::tempdir().unwrap();
    let (mut engine, hub) = engine(folder.path());
    let now = Timestamp(0);
    let name = gmcp("Char.Status", r#"{"character_name":"Fen"}"#);
    engine.receive(name, now).unwrap();
    engine.settle(now);
    let hat = b"You are using:\r\n<worn on head>      a red hat\r\n(1h-1m-1v-1)(-/-)\r\n";
    engine.receive(Bytes::from_static(hat), now).unwrap();
    engine.quiet(now);
    settle_until(&mut engine, &hub, |items| !items.worn.is_empty());

    let bare = b"You are using:\r\nNothing.\r\n(1h-1m-1v-1)(-/-)\r\n";
    engine.receive(Bytes::from_static(bare), now).unwrap();
    engine.quiet(now);
    settle_until(&mut engine, &hub, |items| items.worn.is_empty());
    engine.close_connection(DisconnectReason::ClosedByServer, now);
    engine.finish().unwrap();

    let database = Database::open(&folder.path().join("servers"), HOST).unwrap();
    let saved = database.reader().unwrap().characters().unwrap().worn;
    assert!(saved.is_empty(), "{saved:?}");
}

#[test]
fn a_stall_at_the_pager_with_vitals_awaited_leaves_the_table_open() {
    let folder = tempfile::tempdir().unwrap();
    let (mut engine, hub) = engine(folder.path());
    let now = Timestamp(0);
    let mut vitals = vec![IAC, SB, GMCP];
    vitals.extend_from_slice(br#"Char.Vitals {"hp":"119"}"#);
    vitals.extend_from_slice(&[IAC, SE]);
    engine.receive(Bytes::from(vitals), now).unwrap();

    let first_page = b"(119h-130m-104v-1210)(-/-)You have 12 practice sessions left.\r\n\
        Name                 % Lvl Sub Mana\r\n\
        dodge               90   4   -    0\r\n\
        [B]ack,[H]elp,[R]efresh, or [C]ontinue: ";
    engine.receive(Bytes::from_static(first_page), now).unwrap();
    engine.quiet(now);
    let last_page = b"\r\nzeal                90   8   -    5\r\n(119h-130m-104v-1210)(-/-)";
    engine.receive(Bytes::from_static(last_page), now).unwrap();
    engine.close_connection(DisconnectReason::ClosedByServer, now);

    let skill = |key: &str| {
        hub.read(|character: &Versioned<CharactersState>| character.value().0.get(key).cloned())
    };
    assert_eq!(skill("skills.practice.read"), Some(Value::Number(2.0)));
    assert_eq!(skill("skills.zeal.learned"), Some(Value::Number(90.0)));
}

/// Char.Status says null for spells only `aff` shows, as the logs do.
#[test]
fn the_hud_has_what_aff_read_and_what_gmcp_lists_each_once() {
    let folder = tempfile::tempdir().unwrap();
    let (mut engine, hub) = engine(folder.path());
    let now = Timestamp(0);
    let status = |engine: &mut Engine, message: &[u8]| {
        let mut bytes = vec![IAC, SB, GMCP];
        bytes.extend_from_slice(message);
        bytes.extend_from_slice(&[IAC, SE]);
        engine.receive(Bytes::from(bytes), now).unwrap();
        engine.settle(now);
    };
    let affects = || {
        hub.read(|character: &Versioned<CharactersState>| {
            let affects = character.value().affects().into_iter();
            affects
                .map(|affect| (affect.name, affect.detail))
                .collect::<Vec<_>>()
        })
    };
    status(&mut engine, br#"Char.Status {"affects": null}"#);

    let mut table = Vec::new();
    for line in include_str!("fixtures/affects.txt").lines() {
        table.extend_from_slice(line.as_bytes());
        table.extend_from_slice(b"\r\n");
    }
    engine.receive(Bytes::from(table), now).unwrap();
    engine.receive(Bytes::from_static(PROMPT), now).unwrap();
    engine.quiet(now);
    let started = Instant::now();
    while affects().is_empty() {
        assert!(started.elapsed() < Duration::from_secs(5), "no affects");
        engine.settle(now);
        std::thread::sleep(Duration::from_millis(2));
    }
    let names: Vec<String> = affects().into_iter().map(|(name, _)| name).collect();
    assert_eq!(names, ["armor", "bless", "sanctuary"]);

    status(
        &mut engine,
        br#"Char.Status {"affects": {"Spell: bless": "for 6 hours", "Racial fatigue": "for 2 hours"}}"#,
    );
    let listed = affects();
    assert_eq!(listed.len(), 4, "{listed:?}");
    assert_eq!(listed[1], ("bless".to_owned(), "for 6 hours".to_owned()));
    assert_eq!(listed[3].0, "Racial fatigue");

    status(&mut engine, br#"Char.Status {"affects": null}"#);
    assert_eq!(affects().len(), 3, "null says nothing of what aff read");

    engine.close_connection(DisconnectReason::ClosedByServer, now);
    assert_eq!(affects(), [], "a dropped connection clears both");
}

#[test]
fn a_room_keeps_the_description_after_its_exits_line() {
    let folder = tempfile::tempdir().unwrap();
    let (mut engine, hub) = engine(folder.path());

    // By lines, so a checkout with CRLF endings reads the same.
    let mut bytes = Vec::new();
    for line in include_str!("fixtures/room.txt").lines() {
        bytes.extend_from_slice(line.as_bytes());
        bytes.extend_from_slice(b"\r\n");
    }
    bytes.extend_from_slice(&[IAC, SB, GMCP]);
    bytes.extend_from_slice(
        br#"Room.Info {"name":"Eastern Meadow","zone":"{ 1   4} Crom    The Meadow","exits":{"n":"O","e":"O"}}"#,
    );
    bytes.extend_from_slice(&[IAC, SE]);
    engine.receive(Bytes::from(bytes), Timestamp(0)).unwrap();
    engine.settle(Timestamp(0));

    let room = hub.read(|room: &Versioned<RoomState>| room.value().room.clone());
    assert_eq!(
        room.expect("the room").description,
        "You stand hip deep in thick red wildflowers.  Small gnats and aphids spring"
    );
}

#[test]
fn the_session_suggests_avatar_by_its_host_and_fills_its_stores_when_on() {
    let folder = tempfile::tempdir().unwrap();
    let plugin = mudling_plugin_avatar::load();
    let session = Session::open(
        SessionConfig {
            enabled: vec![plugin.id().clone()],
            plugins: vec![plugin],
            ..SessionConfig::new(folder.path(), HOST, 4000)
        },
        || {},
    )
    .unwrap();

    let score = include_bytes!("fixtures/score.txt");
    session.replay(vec![
        Bytes::from_static(score),
        Bytes::from_static(b"\r\n"),
        Bytes::from_static(PROMPT),
    ]);
    let settings = |session: &Session| {
        session
            .stores()
            .read(|settings: &Versioned<SettingsState>| settings.value().clone())
    };
    let started = Instant::now();
    while settings(&session).connection
        != ConnectionStatus::Closed(DisconnectReason::ClosedByServer)
    {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the replay hung"
        );
        std::thread::sleep(Duration::from_millis(2));
    }

    let game = settings(&session)
        .suggestion
        .expect("the host names the game");
    assert_eq!(
        (game.plugin.as_str(), game.source),
        ("avatar", Source::Host)
    );
    let level = session
        .stores()
        .read(|character: &Versioned<CharactersState>| {
            character.value().0.get("character.level").cloned()
        });
    assert_eq!(level, Some(Value::Number(12.0)));
    session.quit().wait().unwrap();
}
