//! Quiet reads through the whole engine, on Avatar's own tables: the commands
//! go out, the answers land in the typed tables, and the chat and the
//! triggers never hear of them.

use mudling_core::Line;
use mudling_core::defaults::triggers::TriggerRule;
use mudling_core::net::DisconnectReason;
use mudling_core::persist::{CharacterRows, Database, SERVERS_FOLDER, Timestamp};
use mudling_core::session::{Engine, RuleEdit, SessionConfig, Typing};
use mudling_core::store::{
    CharactersState, CuesState, ItemsState, SettingsState, SheetState, StoreHub, TerminalState,
    Versioned,
};
use mudling_core::telnet::Bytes;
use mudling_core::telnet::command::{IAC, SB, SE, WILL, WONT};
use mudling_core::telnet::option::{ECHO, GMCP};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const HOST: &str = "avatar.outland.org";
const PROMPT: &str = "(120h-130m-104v-1210)(-/-)";
/// Past every wait a read has.
const LATER: i64 = 40_000;
const PRACTICE: &str = "You have 2 practice sessions left.\n\
    Name                 % Lvl Sub Mana  Name                 % Lvl Sub Mana\n\
    dodge               95   4   -    0  kick                33   7   -    0";

struct Game {
    engine: Engine,
    hub: Arc<StoreHub>,
    now: i64,
}

impl Game {
    fn open(folder: &Path) -> Game {
        let plugin = mudling_plugin_avatar::load();
        let config = SessionConfig {
            enabled: vec![plugin.id().clone()],
            plugins: vec![plugin],
            ..SessionConfig::new(folder, HOST, 4000)
        };
        let hub = Arc::new(StoreHub::new(|| {}));
        hub.subscribe::<CuesState>();
        let typing = Arc::new(Mutex::new(Typing::default()));
        let mut engine = Engine::open(config, Arc::clone(&hub), typing).unwrap();
        engine.start_connection().unwrap();
        Game {
            engine,
            hub,
            now: 1_000_000,
        }
    }

    /// In the game as `name`, standing, with nothing due but what's asked.
    fn enter_as(folder: &Path, name: &str) -> Game {
        let mut game = Game::open(folder);
        game.gmcp("Char.Status", &format!(r#"{{"character_name": "{name}"}}"#));
        game.gmcp("Char.Vitals", r#"{"position": "Stand"}"#);
        game.gmcp("Room.Info", r#"{"num": 1, "name": "A hall", "exits": {}}"#);
        game
    }

    fn at(&self) -> Timestamp {
        Timestamp(self.now)
    }

    fn receive(&mut self, bytes: Vec<u8>) {
        let now = self.at();
        self.engine.receive(Bytes::from(bytes), now).unwrap();
    }

    fn gmcp(&mut self, package: &str, json: &str) {
        let mut frame = vec![IAC, SB, GMCP];
        frame.extend_from_slice(format!("{package} {json}").as_bytes());
        frame.extend_from_slice(&[IAC, SE]);
        self.receive(frame);
        self.engine.take_output();
    }

    /// The server's text, a line at a time, with the prompt after it.
    fn say(&mut self, text: &str) {
        let mut bytes = Vec::new();
        for line in text.lines() {
            bytes.extend_from_slice(line.as_bytes());
            bytes.extend_from_slice(b"\r\n");
        }
        bytes.extend_from_slice(PROMPT.as_bytes());
        self.receive(bytes);
        let now = self.at();
        self.engine.quiet(now);
    }

    /// What goes out after `wait` ms, if anything.
    fn tick(&mut self, wait: i64) -> String {
        self.now += wait;
        let now = self.at();
        self.engine.tick(now);
        self.engine.settle(now);
        let output = self.engine.take_output();
        String::from_utf8_lossy(&output).trim_end().to_owned()
    }

    fn chat(&mut self) -> Vec<String> {
        let now = self.at();
        self.engine.settle(now);
        self.hub.read(|terminal: &Versioned<TerminalState>| {
            let lines = terminal.value().lines().iter();
            let shown = lines.filter_map(Line::without_prompt);
            shown.map(|line| line.plain().to_owned()).collect()
        })
    }

    fn sheet(&self) -> SheetState {
        self.hub
            .read(|sheet: &Versioned<SheetState>| sheet.value().clone())
    }

    /// Waits for the hooks to act on what was read.
    fn wait_until(&mut self, what: &str, done: impl Fn(&SheetState) -> bool) {
        let started = Instant::now();
        while !done(&self.sheet()) {
            assert!(started.elapsed() < Duration::from_secs(5), "{what}");
            let now = self.at();
            self.engine.settle(now);
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Sends what's next, which must be `command`.
    fn expect_read(&mut self, command: &str) {
        assert_eq!(self.tick(LATER), command);
    }

    /// Sends the entry's reads up to and including `command`, answering the
    /// others with nothing, as a server with lag would.
    fn read_up_to(&mut self, command: &str) {
        for _ in 0..5 {
            if self.tick(LATER) == command {
                return;
            }
        }
        panic!("{command} never went out");
    }
}

fn saved(folder: &Path) -> CharacterRows {
    let database = Database::open(&folder.join(SERVERS_FOLDER), HOST).unwrap();
    database.reader().unwrap().characters().unwrap()
}

#[test]
fn entering_the_game_reads_the_tables_in_order_and_hides_their_answers() {
    let folder = tempfile::tempdir().unwrap();
    let mut game = Game::enter_as(folder.path(), "Tamsin");
    let shown = game.chat().len();
    let trigger = TriggerRule {
        sound: Some("ding".to_owned()),
        ..TriggerRule::new("Str")
    };
    game.engine
        .edit(RuleEdit::SaveTrigger(trigger), Timestamp(0));

    game.expect_read("score");
    game.say(include_str!("fixtures/score.txt"));
    game.wait_until("score", |sheet| {
        sheet.stats.contains_key("str") && sheet.character_read.is_some()
    });
    assert_eq!(game.chat().len(), shown, "{:?}", game.chat());
    assert_eq!(
        game.hub
            .read(|cues: &Versioned<CuesState>| cues.value().sounds.len()),
        0,
        "the trigger stayed quiet"
    );
    let sheet = game.sheet();
    assert_eq!(sheet.sources, ["score"]);

    game.expect_read("practice");
    game.say(PRACTICE);
    game.expect_read("slist");
    game.say(
        "Spells and skills learnable by your class at the lower levels.\n  3:       fast healing",
    );
    game.expect_read("aff");
    game.say("You are not under the affects of any spells.");
    game.expect_read("equipment");
    game.say("You are using:\n<worn on finger>    a pink ice ring");
    assert_eq!(game.tick(LATER), "", "nothing's left");
    assert_eq!(game.chat().len(), shown, "{:?}", game.chat());

    game.say("Bren says 'hello'");
    assert_eq!(game.chat().last().unwrap(), "Bren says 'hello'");
}

#[test]
fn a_level_up_reads_the_score_and_both_skill_tables_again() {
    let folder = tempfile::tempdir().unwrap();
    let mut game = Game::enter_as(folder.path(), "Tamsin");
    game.expect_read("score");
    game.say(include_str!("fixtures/score.txt"));
    game.expect_read("practice");
    game.say(PRACTICE);
    game.expect_read("slist");
    game.say(
        "Spells and skills learnable by your class at the lower levels.\n  3:       fast healing",
    );
    game.expect_read("aff");
    game.say("You are not under the affects of any spells.");
    game.expect_read("equipment");
    game.say("You are using:\nNothing.");
    assert_eq!(game.tick(LATER), "", "nothing's left");

    game.say("You raise a level!! Your gain is: 13/86 hp, 2/122 m, 10/150 mv 5/16 prac.");
    game.expect_read("score");
    game.say(include_str!("fixtures/score.txt"));
    game.expect_read("practice");
    game.say(PRACTICE);
    game.expect_read("slist");
}

#[test]
fn a_table_the_player_asks_for_shows_and_counts_as_read() {
    let folder = tempfile::tempdir().unwrap();
    let mut game = Game::enter_as(folder.path(), "Tamsin");
    game.read_up_to("equipment");
    game.say("You are using:\n<worn on finger>    a pink ice ring");
    game.tick(LATER);

    game.say("You feel a chill.");
    let now = game.at();
    game.engine.send("aff", now);
    game.engine.take_output();
    game.say("You are affected by:\nSpell: 'armor'.");
    assert!(
        game.chat()
            .iter()
            .any(|line| line == "You are affected by:")
    );
    assert_eq!(game.tick(LATER), "", "the player's table answered it");
}

#[test]
fn a_spell_wearing_off_leaves_the_hud_by_itself() {
    let folder = tempfile::tempdir().unwrap();
    let mut game = Game::enter_as(folder.path(), "Tamsin");
    game.read_up_to("aff");
    game.say("You are affected by:\nSpell: 'armor'.\nSpell: 'bless'.");
    game.wait_until("two affects", |sheet| sheet.affects.len() == 2);
    game.tick(LATER);
    let hud = |game: &Game| {
        game.hub
            .read(|character: &Versioned<CharactersState>| character.value().affects().len())
    };
    assert_eq!(hud(&game), 2);

    game.say("Your bless spell wears off.");
    game.expect_read("aff");
    game.say("You are affected by:\nSpell: 'armor'.");
    game.wait_until("one affect", |sheet| sheet.affects.len() == 1);
    assert_eq!(hud(&game), 1);

    game.say("Your armor spell wears off.");
    game.expect_read("aff");
    game.say("You are not under the affects of any spells.");
    game.wait_until("none", |sheet| sheet.affects.is_empty());
    assert_eq!(hud(&game), 0);
    assert_eq!(
        game.chat()
            .iter()
            .filter(|line| line.contains("armor"))
            .count(),
        1,
        "only the message the game sent: {:?}",
        game.chat()
    );

    game.engine.flush();
    assert!(
        saved(folder.path()).affects.is_empty(),
        "gone from disk too"
    );
}

#[test]
fn a_paged_table_is_answered_and_stays_hidden() {
    let folder = tempfile::tempdir().unwrap();
    let mut game = Game::enter_as(folder.path(), "Tamsin");
    let shown = game.chat().len();
    game.read_up_to("practice");
    let table = include_str!("fixtures/practice-long.txt");
    let (_earlier_prompt, table) = table.split_once('\n').unwrap();
    let (first, rest) = table.split_once("[B]ack").unwrap();
    let mut bytes = first.replace('\n', "\r\n").into_bytes();
    bytes.extend_from_slice(b"[B]ack,[H]elp,[R]efresh, or [C]ontinue: ");
    game.receive(bytes);
    assert_eq!(game.tick(0), "c", "the pager's answered");
    let (_, rest) = rest.split_once('\n').unwrap();
    game.say(rest);
    game.wait_until("the second page", |sheet| {
        sheet.skills.iter().any(|skill| skill.name == "kick")
    });
    assert_eq!(game.chat().len(), shown, "{:?}", game.chat());
}

#[test]
fn typing_a_fight_and_a_password_stop_the_reads_and_the_pager() {
    let folder = tempfile::tempdir().unwrap();
    let mut game = Game::enter_as(folder.path(), "Tamsin");

    game.engine.set_typing(true);
    assert_eq!(game.tick(LATER), "", "typing");
    game.engine.set_typing(false);

    game.gmcp("Char.Vitals", r#"{"position": "Fight"}"#);
    assert_eq!(game.tick(LATER), "", "a fight");
    game.gmcp("Char.Vitals", r#"{"position": "Stand"}"#);

    let mut echo_off = vec![IAC, WILL, ECHO];
    game.receive(std::mem::take(&mut echo_off));
    game.engine.take_output();
    assert_eq!(game.tick(LATER), "", "a password");
    game.receive(vec![IAC, WONT, ECHO]);
    game.engine.take_output();

    let now = game.at();
    game.engine.send("look", now);
    assert_eq!(game.tick(0), "look");
    assert_eq!(game.tick(1_000), "", "the player's busy");

    assert_eq!(game.tick(LATER), "score", "it all waited");
}

#[test]
fn a_greeting_for_someone_else_in_the_game_renames_nobody() {
    let folder = tempfile::tempdir().unwrap();
    let mut game = Game::enter_as(folder.path(), "Benjamina");
    game.read_up_to("aff");
    game.say("Welcome back to the AVATAR System, Lady Eiri.");
    // Hooks run in order: once this table lands, the greeting's hook has run.
    game.say("You are affected by:\nSpell: 'armor'.");
    game.wait_until("the read", |sheet| !sheet.affects.is_empty());
    assert_eq!(game.sheet().character, "benjamina");
}

#[test]
fn a_greeting_at_login_names_the_character() {
    let folder = tempfile::tempdir().unwrap();
    let mut game = Game::open(folder.path());
    game.say("Welcome back to the AVATAR System, Lady Eiri.");
    game.wait_until("the name", |sheet| sheet.character == "eiri");
}

#[test]
fn a_practice_lowers_the_practices_left_with_the_live_count() {
    let folder = tempfile::tempdir().unwrap();
    let mut game = Game::enter_as(folder.path(), "Tamsin");
    game.read_up_to("practice");
    game.say(PRACTICE);
    game.wait_until("the list", |sheet| sheet.practices == Some(2));
    game.tick(LATER);
    game.say("You practice kick with Ulric. Your current skill level is now 45.");
    game.gmcp("Char.Status", r#"{"practice": "1"}"#);
    let now = game.at();
    game.engine.settle(now);
    assert_eq!(game.sheet().practices, Some(1));
    assert_eq!(
        game.tick(LATER),
        "practice",
        "and read again for a game that won't say"
    );
}

#[test]
fn every_line_of_the_games_own_tables_is_a_row_and_a_tell_inside_one_shows() {
    let folder = tempfile::tempdir().unwrap();
    let mut game = Game::enter_as(folder.path(), "Tamsin");
    let trigger = TriggerRule {
        sound: Some("ding".to_owned()),
        ..TriggerRule::new("tells you")
    };
    let now = game.at();
    game.engine.edit(RuleEdit::SaveTrigger(trigger), now);
    let shown = game.chat().len();
    game.read_up_to("practice");

    // Between the rows, then between the pages.
    let first_page = "You have 2 practice sessions left.\r\n\
        Name                 % Lvl Sub Mana  Name                 % Lvl Sub Mana\r\n\
        -----------------------------------  -----------------------------------\r\n\
        dodge               95   4   -    0  kick                33   7   -    0\r\n\
        Bren tells you 'one'\r\n\
        bash                50   9   -    0  trip                20  12   -    0\r\n\
        [B]ack,[H]elp,[R]efresh, or [C]ontinue: ";
    game.receive(first_page.as_bytes().to_vec());
    assert_eq!(game.tick(0), "c", "the pager's answered");
    game.say(
        "\nMira tells you 'two'\n\
        hide                95  11   -    0  sneak               90  12   -    0\n\
        \n\
        'It is still possible for you to train the following :'\n\
        Statistic       Current    Max Trainable    Cost\n\
        ------------    -------    -------------    ----\n\
        Strength            14             18         4\n\
        HP                  99         999999        12",
    );
    game.wait_until("the last page", |sheet| {
        sheet.skills.iter().any(|skill| skill.name == "sneak")
    });
    let chat = game.chat();
    assert_eq!(
        chat[shown..],
        ["Bren tells you 'one'", "Mira tells you 'two'"],
        "only what wasn't the table"
    );
    let sounds = game
        .hub
        .read(|cues: &Versioned<CuesState>| cues.value().sounds.clone());
    assert_eq!(sounds, ["ding"], "the trigger heard them");
    let notices = game
        .hub
        .read(|settings: &Versioned<SettingsState>| settings.value().notices.clone());
    assert_eq!(notices, [""; 0], "no row ran a hook out of its budget");
}

#[test]
fn a_quiet_equipment_read_fills_the_worn_list_and_a_tell_inside_it_shows() {
    let folder = tempfile::tempdir().unwrap();
    let mut game = Game::enter_as(folder.path(), "Tamsin");
    let shown = game.chat().len();
    game.read_up_to("equipment");
    game.say(
        "You are using:\n<worn on finger>    a pink ice ring\n\
        Bren tells you 'hi'\n<worn on head>      a red hat",
    );
    let started = Instant::now();
    let worn = loop {
        let now = game.at();
        game.engine.settle(now);
        let worn = game
            .hub
            .read(|items: &Versioned<ItemsState>| items.value().worn.clone());
        if worn.len() == 2 {
            break worn;
        }
        assert!(started.elapsed() < Duration::from_secs(5), "never landed");
        std::thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(worn[0].item, "a pink ice ring");
    assert_eq!(worn[1].item, "a red hat");
    assert_eq!(game.chat()[shown..], ["Bren tells you 'hi'"]);
}

#[test]
fn a_character_wearing_nothing_hears_nothing_of_the_read() {
    let folder = tempfile::tempdir().unwrap();
    let mut game = Game::enter_as(folder.path(), "Tamsin");
    let shown = game.chat().len();
    game.read_up_to("equipment");
    game.say("You are using:\nNothing.");
    assert_eq!(game.chat()[shown..], [""; 0]);
}

#[test]
fn two_late_answers_glued_at_a_prompt_both_stay_hidden() {
    let folder = tempfile::tempdir().unwrap();
    let mut game = Game::enter_as(folder.path(), "Tamsin");
    let shown = game.chat().len();
    game.read_up_to("aff");
    game.expect_read("equipment");
    game.say(&format!(
        "You are affected by:\nSpell: 'armor'.\n{PROMPT}You are using:\n\
         <worn on finger>    a pink ice ring\n<worn on body>      a robe"
    ));
    game.wait_until("the first answer", |sheet| !sheet.affects.is_empty());
    assert_eq!(game.chat()[shown..], [""; 0]);
}

#[test]
fn a_late_answer_stays_out_of_the_chat_and_fires_no_trigger() {
    let folder = tempfile::tempdir().unwrap();
    let mut game = Game::enter_as(folder.path(), "Tamsin");
    let shown = game.chat().len();
    game.expect_read("score");
    game.now += 20_000;
    game.say(include_str!("fixtures/score.txt"));
    game.wait_until("score", |sheet| sheet.stats.contains_key("str"));
    assert_eq!(game.chat().len(), shown, "{:?}", game.chat());
}

#[test]
fn a_help_page_at_its_pager_holds_the_reads() {
    let folder = tempfile::tempdir().unwrap();
    let mut game = Game::enter_as(folder.path(), "Tamsin");
    game.receive(b"A long help page.\r\n[B]ack,[H]elp,[R]efresh, or [C]ontinue: ".to_vec());
    assert_eq!(game.tick(LATER), "", "the pager is the player's");
    let now = game.at();
    game.engine.send("c", now);
    game.engine.take_output();
    game.say("The rest of the page.");
    assert_eq!(game.tick(LATER), "score");
}

#[test]
fn a_pressed_button_cant_reach_a_password_box() {
    let folder = tempfile::tempdir().unwrap();
    let mut game = Game::enter_as(folder.path(), "Tamsin");
    game.receive(vec![IAC, WILL, ECHO]);
    game.engine.take_output();
    let now = game.at();
    game.engine.send_action("practice dodge", now);
    assert_eq!(game.engine.take_output(), b"");
}

#[test]
fn each_character_keeps_their_own_sheet() {
    let folder = tempfile::tempdir().unwrap();
    let mut tamsin = Game::enter_as(folder.path(), "Tamsin");
    tamsin.read_up_to("practice");
    tamsin.say(PRACTICE);
    tamsin.wait_until("skills", |sheet| !sheet.skills.is_empty());
    let now = tamsin.at();
    tamsin
        .engine
        .close_connection(DisconnectReason::ClosedByServer, now);
    tamsin.engine.finish().unwrap();
    let rows = saved(folder.path());
    assert!(
        rows.skills
            .iter()
            .all(|skill| skill.character_id == "tamsin")
    );
    assert!(!rows.skills.is_empty(), "in the typed table");
    assert!(!rows.stats.iter().any(|stat| stat.key.ends_with(".learned")));

    let bren = Game::open(folder.path());
    assert_eq!(bren.sheet(), SheetState::default(), "nobody's named yet");
    drop(bren);
    let bren = Game::enter_as(folder.path(), "Bren");
    let sheet = bren.sheet();
    assert_eq!(sheet.character, "bren");
    assert!(sheet.skills.is_empty(), "Tamsin's aren't Bren's");
    drop(bren);

    let tamsin = Game::enter_as(folder.path(), "Tamsin");
    assert!(!tamsin.sheet().skills.is_empty(), "back with their own");
}

#[test]
fn a_practice_that_raises_a_skill_is_the_improvement_the_panel_shows() {
    let folder = tempfile::tempdir().unwrap();
    let mut game = Game::enter_as(folder.path(), "Tamsin");
    game.read_up_to("practice");
    game.say(PRACTICE);
    game.wait_until("the list", |sheet| sheet.practices == Some(2));

    game.say("You practice kick with Ulric. Your current skill level is now 45.");
    game.wait_until("the rise", |sheet| sheet.improved.is_some());
    let improved = game.sheet().improved.unwrap();
    assert_eq!((&*improved.skill, improved.learned), ("kick", 45));
}
