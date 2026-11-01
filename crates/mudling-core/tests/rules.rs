//! What the player sets up, through a session: triggers, aliases and the
//! hotbar, chaining and walks, the password prompt, and what's kept.

use mudling_core::defaults::aliases::AliasRule;
use mudling_core::defaults::triggers::{Highlight, Scope, TriggerRule};
use mudling_core::map::{MapRoom, Mark, feature};
use mudling_core::persist::{Database, SERVERS_FOLDER, Timestamp};
use mudling_core::plugin::Plugin;
use mudling_core::session::{Credentials, Engine, MapEdit, RuleEdit, SessionConfig, Typing};
use mudling_core::store::{
    CuesState, MapState, MobsState, RulesState, StoreHub, TerminalState, Versioned,
};
use mudling_core::telnet::Bytes;
use mudling_core::telnet::command::{IAC, WILL, WONT};
use mudling_core::telnet::gmcp;
use mudling_core::telnet::option::ECHO;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const HOST: &str = "mud.example";
const NOW: Timestamp = Timestamp(0);

fn fixture(name: &str) -> Plugin {
    let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("plugins")
        .join(name);
    Plugin::load(folder).unwrap_or_else(|error| panic!("{error}"))
}

struct Played {
    engine: Engine,
    hub: Arc<StoreHub>,
    typing: Arc<Mutex<Typing>>,
}

impl Played {
    fn on(folder: &Path, host: &str, adjust: impl FnOnce(&mut SessionConfig)) -> Played {
        let hub = Arc::new(StoreHub::new(|| {}));
        hub.subscribe::<CuesState>();
        let mut config = SessionConfig::new(folder, host, 4000);
        adjust(&mut config);
        let typing = Arc::new(Mutex::new(Typing::default()));
        let mut engine = Engine::open(config, Arc::clone(&hub), Arc::clone(&typing)).unwrap();
        engine.start_connection().unwrap();
        engine.take_output();
        Played {
            engine,
            hub,
            typing,
        }
    }

    fn with_plugins(folder: &Path, plugins: Vec<Plugin>) -> Played {
        Played::on(folder, HOST, |config| {
            config.enabled = plugins.iter().map(|plugin| plugin.id().clone()).collect();
            config.plugins = plugins;
        })
    }

    fn plain(folder: &Path) -> Played {
        Played::with_plugins(folder, Vec::new())
    }

    fn say(&mut self, bytes: &[u8]) {
        self.engine
            .receive(Bytes::copy_from_slice(bytes), NOW)
            .unwrap();
        self.engine.settle(NOW);
    }

    /// The game says who is playing, as Char.Status does.
    fn named(&mut self, name: &str) {
        let mut frame = Vec::new();
        let status = format!(r#"{{"character_name": "{name}"}}"#);
        gmcp::encode("Char.Status", &status, &mut frame);
        self.say(&frame);
    }

    fn in_a_room(&mut self) {
        let mut frame = Vec::new();
        let room = r#"{"num": 1, "name": "A road", "exits": {"n": 2}}"#;
        gmcp::encode("Room.Info", room, &mut frame);
        self.say(&frame);
        self.engine.take_output();
    }

    fn room(&self) -> Option<Arc<MapRoom>> {
        self.hub
            .read(|map: &Versioned<MapState>| map.value().room(1).cloned())
    }

    fn echo_off(&mut self) {
        self.say(&[IAC, WILL, ECHO]);
        self.engine.take_output();
    }

    fn echo_on(&mut self) {
        self.say(&[IAC, WONT, ECHO]);
        self.engine.take_output();
    }

    fn edit(&mut self, edit: RuleEdit) {
        self.engine.edit(edit, NOW);
        self.engine.settle(NOW);
    }

    fn trigger(&mut self, rule: TriggerRule) {
        self.edit(RuleEdit::SaveTrigger(rule));
    }

    fn alias(&mut self, name: &str, expansion: &str) {
        self.edit(RuleEdit::SaveAlias(AliasRule::new(name, expansion)));
    }

    /// What goes to the server for a typed line, one entry per wire line.
    fn sent_for(&mut self, typed: &str) -> Vec<String> {
        self.engine.send(typed, NOW);
        self.sent()
    }

    fn sent(&mut self) -> Vec<String> {
        String::from_utf8(self.engine.take_output())
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn chat(&mut self) -> Vec<String> {
        self.engine.settle(NOW);
        self.hub.read(|terminal: &Versioned<TerminalState>| {
            let lines = terminal.value().lines().iter();
            lines.map(|line| line.plain().to_owned()).collect()
        })
    }

    fn rules(&self) -> RulesState {
        self.hub
            .read(|rules: &Versioned<RulesState>| rules.value().clone())
    }

    /// The sounds asked for since the last drain.
    fn sounds(&self) -> Vec<String> {
        let batch = self.hub.drain();
        let cues = batch.and_then(|batch| batch.cues);
        cues.map(|cues| cues.change).unwrap_or_default()
    }

    fn finish(self) {
        self.engine.finish().unwrap();
    }
}

fn hiding(pattern: &str) -> TriggerRule {
    TriggerRule {
        hides: true,
        ..TriggerRule::new(pattern)
    }
}

fn sounding(pattern: &str, sound: &str) -> TriggerRule {
    TriggerRule {
        sound: Some(sound.to_owned()),
        ..TriggerRule::new(pattern)
    }
}

#[test]
fn plain_text_matches_anywhere_in_any_case() {
    let folder = tempfile::tempdir().unwrap();
    let mut played = Played::plain(folder.path());
    played.trigger(hiding("tells you"));
    played.say(b"Grimm TELLS YOU 'hi'\r\nGrimm says 'hi'\r\n");
    assert_eq!(played.chat(), ["Grimm says 'hi'"]);
}

#[test]
fn a_regular_expression_matches_as_written() {
    let folder = tempfile::tempdir().unwrap();
    let mut played = Played::plain(folder.path());
    played.trigger(TriggerRule {
        regex: true,
        ..hiding(r"^\w+ is DEAD!!$")
    });
    played.say(b"Grimm is DEAD!!\r\nHa, Grimm is DEAD!!\r\n");
    assert_eq!(played.chat(), ["Ha, Grimm is DEAD!!"]);
}

#[test]
fn bad_regexes_and_triggers_that_are_off_never_match() {
    let folder = tempfile::tempdir().unwrap();
    let mut played = Played::plain(folder.path());
    played.trigger(TriggerRule {
        enabled: false,
        ..hiding("rat")
    });
    played.trigger(TriggerRule {
        regex: true,
        ..hiding("rat(")
    });
    played.say(b"A rat is here.\r\n");
    assert_eq!(played.chat(), ["A rat is here."]);
}

#[test]
fn hidden_lines_never_reach_the_chat_and_their_sound_still_plays() {
    let folder = tempfile::tempdir().unwrap();
    let mut played = Played::plain(folder.path());
    played.trigger(TriggerRule {
        hides: true,
        ..sounding("flaps its wings", "pop")
    });
    played.sounds();
    played.say(b"A bat flaps its wings.\r\nA bat is here.\r\n");
    assert_eq!(played.chat(), ["A bat is here."]);
    assert_eq!(played.sounds(), ["pop"]);
    assert!(played.sounds().is_empty(), "each sound plays once");
}

#[test]
fn a_highlight_paints_the_whole_line_over_the_servers_colors() {
    let folder = tempfile::tempdir().unwrap();
    let mut played = Played::plain(folder.path());
    played.trigger(TriggerRule {
        color: Some(Highlight::Purple),
        ..TriggerRule::new("tells you")
    });
    played.say(b"\x1b[32mGrimm tells you\x1b[0m 'hi'\r\n");
    played.engine.settle(NOW);
    let (start, end, style) = played.hub.read(|terminal: &Versioned<TerminalState>| {
        let lines = terminal.value().lines();
        let line = lines.iter().next().expect("the line");
        let [span] = line.styled() else {
            panic!("one span, not {:?}", line.styled())
        };
        (span.start, span.end, span.style)
    });
    assert_eq!((start, end), (0, "Grimm tells you 'hi'".len() as u32));
    assert_eq!(style, Highlight::Purple.style());
}

#[test]
fn what_the_chat_echoes_is_not_matched() {
    let folder = tempfile::tempdir().unwrap();
    let mut played = Played::plain(folder.path());
    played.trigger(hiding("kill"));
    played.engine.send("kill rat", NOW);
    assert_eq!(played.chat(), ["kill rat"]);
}

#[test]
fn a_trigger_applies_from_the_next_line_and_stops_when_removed() {
    let folder = tempfile::tempdir().unwrap();
    let mut played = Played::plain(folder.path());
    played.say(b"one bat\r\n");
    played.trigger(hiding("bat"));
    played.say(b"two bat\r\n");
    let id = played.rules().triggers[0].id.clone();
    played.edit(RuleEdit::RemoveTrigger(id));
    played.say(b"three bat\r\n");
    assert_eq!(played.chat(), ["one bat", "three bat"]);
}

/// The desktop opens a session with no credentials, so who is playing is only
/// what the game says. A rule for one character applies to that one, from the
/// moment the game names them, and to nobody before.
#[test]
fn a_rule_for_one_character_follows_the_name_the_game_gives() {
    let folder = tempfile::tempdir().unwrap();
    let mut played = Played::plain(folder.path());
    assert_eq!(played.rules().character, "", "nobody is named yet");
    played.trigger(TriggerRule {
        scope: Scope::Character("bren".to_owned()),
        ..hiding("bat")
    });
    played.edit(RuleEdit::SaveAlias(AliasRule {
        scope: Scope::Character("bren".to_owned()),
        ..AliasRule::new("gb", "get bread")
    }));
    played.say(b"before: a bat\r\n");
    assert_eq!(played.sent_for("gb"), ["gb"], "an alias nobody can use yet");

    played.named("Bren");
    assert_eq!(played.rules().character, "bren");
    played.say(b"as Bren: a bat\r\n");
    assert_eq!(played.sent_for("gb"), ["get bread"]);

    played.named("Ana");
    assert_eq!(played.rules().character, "ana");
    played.say(b"as Ana: a bat\r\n");
    assert_eq!(played.sent_for("gb"), ["gb"]);

    // The three typed `gb` echo as typed; Bren's trigger hid only the line
    // Bren saw.
    assert_eq!(
        played.chat(),
        ["before: a bat", "gb", "gb", "as Ana: a bat", "gb"]
    );
}

/// Two characters share a server and, on the desktop, a session id, so what
/// the last one's name left saved says nothing about who plays now.
#[test]
fn a_new_session_does_not_borrow_the_last_characters_rules() {
    let folder = tempfile::tempdir().unwrap();
    let mut first = Played::plain(folder.path());
    first.trigger(TriggerRule {
        scope: Scope::Character("bren".to_owned()),
        ..hiding("bat")
    });
    first.edit(RuleEdit::SaveAlias(AliasRule {
        scope: Scope::Character("bren".to_owned()),
        ..AliasRule::new("gb", "get bread")
    }));
    first.named("Bren");
    assert_eq!(first.rules().character, "bren");
    first.finish();

    let mut second = Played::plain(folder.path());
    assert_eq!(second.rules().character, "", "nobody has been named yet");
    second.say(b"a bat\r\n");
    assert_eq!(second.sent_for("gb"), ["gb"]);
    second.named("Ana");
    second.say(b"another bat\r\n");
    assert_eq!(second.rules().character, "ana");
    second.named("Bren");
    assert_eq!(second.sent_for("gb"), ["get bread"]);
    assert_eq!(second.chat(), ["a bat", "gb", "another bat", "gb"]);
}

/// Connecting again reuses the session, so who played before must not linger
/// until the game says who plays now.
#[test]
fn connecting_again_forgets_who_was_playing() {
    let folder = tempfile::tempdir().unwrap();
    let mut played = Played::plain(folder.path());
    played.trigger(TriggerRule {
        scope: Scope::Character("bren".to_owned()),
        ..hiding("bat")
    });
    played.named("Bren");
    played.say(b"seen by Bren: a bat\r\n");

    played.engine.start_connection().unwrap();
    played.engine.take_output();
    assert_eq!(played.rules().character, "", "the new login isn't named");
    played.say(b"before the name: a bat\r\n");
    played.named("Ana");
    played.say(b"as Ana: a bat\r\n");
    assert_eq!(
        played.chat(),
        ["before the name: a bat", "as Ana: a bat"][..],
        "only the line Bren saw was hidden"
    );
}

#[test]
fn a_trigger_for_one_character_passes_the_others_by() {
    let folder = tempfile::tempdir().unwrap();
    let scoped = |character: &str| TriggerRule {
        scope: Scope::Character(character.to_owned()),
        ..hiding("bat")
    };
    let ana = |config: &mut SessionConfig| {
        config.credentials = Some(Credentials {
            name: "Ana".to_owned(),
            password: String::new(),
        });
    };
    let mut played = Played::on(folder.path(), HOST, ana);
    played.trigger(scoped("bo"));
    played.trigger(TriggerRule {
        pattern: "rat".to_owned(),
        ..scoped("ana")
    });
    played.say(b"a bat\r\na rat\r\n");
    assert_eq!(played.chat(), ["a bat"]);
}

#[test]
fn everything_comes_back_with_the_next_session() {
    let folder = tempfile::tempdir().unwrap();
    let mut played = Played::plain(folder.path());
    played.trigger(TriggerRule {
        scope: Scope::All,
        color: Some(Highlight::Teal),
        ..sounding("bat", "chime")
    });
    played.alias("k", "kill $1");
    played.edit(RuleEdit::SaveAlias(AliasRule {
        enabled: false,
        ..AliasRule::new("off", "quit")
    }));
    played.edit(RuleEdit::SetHotbar(vec![
        "look".to_owned(),
        "score".to_owned(),
    ]));
    let saved = played.rules();
    played.finish();

    let mut again = Played::plain(folder.path());
    assert_eq!(again.rules(), saved);
    assert_eq!(again.sent_for("k rat"), ["kill rat"]);
    assert_eq!(again.sent_for("off"), ["off"]);
    again.finish();
}

#[test]
fn a_rule_for_everyone_reaches_every_server_and_saves_where_it_lives() {
    let folder = tempfile::tempdir().unwrap();
    let mut home = Played::plain(folder.path());
    home.trigger(TriggerRule {
        scope: Scope::All,
        ..hiding("bat")
    });
    home.trigger(hiding("rat"));
    home.edit(RuleEdit::SaveAlias(AliasRule {
        scope: Scope::All,
        ..AliasRule::new("k", "kill")
    }));
    home.finish();

    let mut elsewhere = Played::on(folder.path(), "other.example", |_| {});
    elsewhere.say(b"a bat\r\na rat\r\n");
    assert_eq!(
        elsewhere.chat(),
        ["a rat"],
        "the server's own rule stays home"
    );
    assert_eq!(elsewhere.sent_for("k"), ["kill"]);

    let mut edited = elsewhere.rules().triggers[0].clone();
    assert_eq!(edited.home, HOST);
    edited.pattern = "wolf".to_owned();
    elsewhere.trigger(edited);
    elsewhere.finish();

    let database = Database::open(&folder.path().join(SERVERS_FOLDER), HOST).unwrap();
    let rows = database.reader().unwrap().triggers().unwrap();
    assert!(rows.iter().any(|row| row.matches == "wolf"));
    let other = Database::open(&folder.path().join(SERVERS_FOLDER), "other.example").unwrap();
    assert!(other.reader().unwrap().triggers().unwrap().is_empty());
}

#[test]
fn a_rule_narrowed_from_everyone_moves_to_the_server_it_was_narrowed_on() {
    let folder = tempfile::tempdir().unwrap();
    let mut home = Played::plain(folder.path());
    home.trigger(TriggerRule {
        scope: Scope::All,
        ..hiding("bat")
    });
    home.finish();

    let mut elsewhere = Played::on(folder.path(), "other.example", |_| {});
    let mut narrowed = elsewhere.rules().triggers[0].clone();
    narrowed.scope = Scope::Server;
    elsewhere.trigger(narrowed);
    elsewhere.finish();

    let rows = |host: &str| {
        let database = Database::open(&folder.path().join(SERVERS_FOLDER), host).unwrap();
        database.reader().unwrap().triggers().unwrap().len()
    };
    assert_eq!((rows(HOST), rows("other.example")), (0, 1));
}

#[test]
fn aliases_expand_with_their_words_and_chains() {
    let folder = tempfile::tempdir().unwrap();
    let mut played = Played::plain(folder.path());
    played.alias("k", "kill $1;kick $1");
    assert_eq!(played.sent_for("k rat"), ["kill rat", "kick rat"]);
    assert_eq!(
        played.sent_for("K rat;look"),
        ["kill rat", "kick rat", "look"]
    );
    assert_eq!(played.sent_for("kick k"), ["kick k"]);
    assert_eq!(
        played.chat().last().unwrap(),
        "kick k",
        "the chat echoes what was typed"
    );
}

#[test]
fn an_alias_can_be_a_walk() {
    let folder = tempfile::tempdir().unwrap();
    let mut played = Played::plain(folder.path());
    played.alias("gate", "2n, e;open gate");
    assert_eq!(played.sent_for("gate"), ["north", "n", "east", "open gate"]);
}

#[test]
fn a_plugins_aliases_expand_and_the_players_win_a_name() {
    let folder = tempfile::tempdir().unwrap();
    let mut played = Played::with_plugins(folder.path(), vec![fixture("testland")]);
    assert_eq!(
        played.sent_for("rr"),
        ["rest;sleep"],
        "joined by its separator"
    );
    played.alias("RR", "stand");
    assert_eq!(played.sent_for("rr"), ["stand"]);
}

#[test]
fn a_pressed_command_expands_as_a_typed_one_does() {
    let folder = tempfile::tempdir().unwrap();
    let mut played = Played::plain(folder.path());
    played.alias("wave", "bow");
    played.engine.press("wave", NOW);
    assert_eq!(played.sent(), ["bow"]);
    assert_eq!(played.chat(), ["wave"]);
}

#[test]
fn the_preview_says_what_an_alias_would_send() {
    let folder = tempfile::tempdir().unwrap();
    let mut played = Played::plain(folder.path());
    played.alias("k", "kill $1;kick $1");
    let preview = |typed: &str| {
        let typing = played.typing.lock().unwrap();
        typing.alias_preview(typed)
    };
    assert_eq!(preview("k rat").as_deref(), Some("kill rat; kick rat"));
    assert_eq!(preview("look"), None);
}

#[test]
fn chaining_uses_the_plugins_separator_and_none_means_a_command_a_line() {
    let folder = tempfile::tempdir().unwrap();
    let mut chained = Played::with_plugins(folder.path(), vec![fixture("testland")]);
    assert_eq!(chained.sent_for("get all;wear all"), ["get all;wear all"]);
    let folder = tempfile::tempdir().unwrap();
    let mut plain = Played::plain(folder.path());
    assert_eq!(plain.sent_for("get all;wear all"), ["get all", "wear all"]);
}

#[test]
fn a_long_walk_never_sends_one_line_twice_running() {
    let folder = tempfile::tempdir().unwrap();
    let mut played = Played::plain(folder.path());
    let sent = played.sent_for("30n");
    assert_eq!(sent.len(), 30);
    assert!(sent.windows(2).all(|pair| pair[0] != pair[1]), "{sent:?}");
}

#[test]
fn with_echo_off_no_trigger_acts_and_no_alias_expands() {
    let folder = tempfile::tempdir().unwrap();
    let mut played = Played::plain(folder.path());
    played.trigger(TriggerRule {
        hides: true,
        ..sounding("secret", "chime")
    });
    played.alias("secret", "say my password is $*");
    played.sounds();

    played.echo_off();
    assert_eq!(played.sent_for("secret"), ["secret"]);
    played.say(b"secret line\r\n");
    assert_eq!(played.chat(), ["secret line"], "nothing hid it");
    assert!(played.sounds().is_empty());
    assert_eq!(played.chat().len(), 1, "a password isn't echoed");

    played.echo_on();
    played.say(b"secret again\r\n");
    assert_eq!(played.chat(), ["secret line"]);
}

#[test]
fn a_press_does_nothing_while_the_server_asks_for_a_password() {
    let folder = tempfile::tempdir().unwrap();
    let mut played = Played::plain(folder.path());
    played.echo_off();
    played.engine.press("look", NOW);
    assert!(played.sent().is_empty());
    assert!(played.chat().is_empty());

    played.echo_on();
    played.engine.press("look", NOW);
    assert_eq!(played.sent(), ["look"]);
    assert_eq!(played.chat(), ["look"]);
}

/// Words a note could hold that would do harm as commands.
const COMMANDS: &str = "quit\ndrop all;kill everyone\n3n2e";

#[test]
fn notes_stay_notes_and_come_back_in_the_next_session() {
    let folder = tempfile::tempdir().unwrap();
    let mut played = Played::plain(folder.path());
    played.in_a_room();
    played.engine.edit_map(
        MapEdit::Note {
            room: 1,
            text: COMMANDS.to_owned(),
        },
        NOW,
    );
    played.engine.edit_map(
        MapEdit::Feature {
            room: 1,
            feature: feature::SHOP.to_owned(),
            present: true,
        },
        NOW,
    );
    played.engine.edit_map(
        MapEdit::Mark {
            title: "A grey wolf is here.".to_owned(),
            mark: Mark::Gear,
            set: true,
        },
        NOW,
    );
    played.edit(RuleEdit::SetNotepad(COMMANDS.to_owned()));

    assert!(played.sent().is_empty(), "nothing reaches the server");
    assert!(played.chat().is_empty(), "nothing runs");
    played.finish();

    let again = Played::plain(folder.path());
    assert_eq!(again.rules().notepad, COMMANDS);
    let room = again.room().expect("the room was saved");
    assert_eq!(room.notes, COMMANDS);
    assert!(room.features.contains(feature::SHOP));
    assert!(room.features.contains(feature::GEAR_MOB));
    let marks = again.hub.read(|mobs: &Versioned<MobsState>| {
        mobs.value().marks_on("A grey wolf is here, sleeping.")
    });
    assert_eq!(marks, [Mark::Gear]);
    again.finish();
}
