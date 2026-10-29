//! The default plugins over synthetic lines, through the line pipeline and
//! the plugin host as a shell drives them.

use mudling_core::defaults::commands::{WalkSyntax, chain_lines, split_chain};
use mudling_core::defaults::detect::{ServerSignals, Source, Suggestion, suggest};
use mudling_core::defaults::hud::{Field, HudFields, Vital, values};
use mudling_core::defaults::prompt::PromptLearner;
use mudling_core::defaults::{self, Defaults, PendingAction};
use mudling_core::line::{Line, LinePipeline};
use mudling_core::plugin::points::Speedwalk;
use mudling_core::plugin::{Plugin, PluginHost, PluginId};
use serde_json::{Map, Value};
use std::path::PathBuf;

fn fixture(name: &str) -> Plugin {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugins").join(name);
    Plugin::load(&dir).unwrap_or_else(|error| panic!("{error}"))
}

/// A game plugin from a manifest tail and its point files.
fn game(id: &str, servers: &str, points: &[(&str, &str, &str)]) -> Plugin {
    let mut manifest = format!(
        "id = \"{id}\"\nname = \"{id}\"\nversion = \"1.0.0\"\napi = 1\nlicense = \"MIT\"\nlayer = \"game\"\n\
         [servers]\n{servers}\n[points]\n"
    );
    for (point, file, _) in points {
        manifest.push_str(&format!("{point} = \"{file}\"\n"));
    }
    Plugin::from_files(|name| {
        if name == "plugin.toml" {
            return Ok(manifest.clone());
        }
        Ok(points.iter().find(|(_, file, _)| *file == name).expect("a declared file").2.to_owned())
    })
    .unwrap_or_else(|error| panic!("{error}"))
}

/// Drives a pipeline, the host and the defaults the way the shell will.
struct Session {
    pipeline: LinePipeline,
    host: PluginHost,
    defaults: Defaults,
    lines: Vec<Line>,
    /// Each finished line, and the prompt length the defaults found at its start.
    read: Vec<(String, Option<usize>)>,
    sent: Vec<String>,
}

impl Session {
    fn new(plugins: Vec<Plugin>) -> Session {
        let host = PluginHost::new(std::iter::once(defaults::plugin()).chain(plugins)).expect("compiles");
        Session {
            pipeline: LinePipeline::default(),
            host,
            defaults: Defaults::default(),
            lines: Vec::new(),
            read: Vec::new(),
            sent: Vec::new(),
        }
    }

    fn push(&mut self, text: &str) {
        self.pipeline.push(text.as_bytes(), &mut self.lines);
        self.read_lines();
        let Some(pending) = self.pipeline.pending() else { return };
        let kind = self.host.read_pending(&pending);
        match self.defaults.read_pending(&pending, kind) {
            PendingAction::Wait => {}
            PendingAction::Send(key) => self.sent.push(key.to_string()),
            PendingAction::EndPrompt => {
                let line = self.pipeline.prompt_end().expect("text is pending");
                self.lines.push(line);
                self.read_lines();
            }
        }
    }

    fn vitals(&mut self) {
        self.defaults.gmcp("Char.Vitals", r#"{"hp": "20", "maxhp": "30"}"#);
    }

    /// Another GMCP message, after which the shell looks at the pending
    /// text again: a prompt learned from it ends.
    fn gmcp(&mut self, package: &str) {
        self.defaults.gmcp(package, "{}");
        self.push("");
    }

    fn read_lines(&mut self) {
        for line in self.lines.drain(..) {
            self.host.read_line(&line, &mut Vec::new());
            let prompt = self.defaults.read_line(&line);
            self.read.push((line.plain().to_owned(), prompt));
        }
    }

    fn shapes(&self) -> Vec<String> {
        self.defaults.prompt.shapes().map(|(shape, _)| shape).collect()
    }
}

// ---- Prompt learning ----

#[test]
fn the_line_after_vitals_is_learned_as_the_prompt() {
    let mut session = Session::new(Vec::new());
    session.push("You stand in a glade.\n\r");
    session.vitals();
    session.push("<20hp 30mv> ");
    session.gmcp("Room.Info");
    assert_eq!(session.shapes(), ["<#hp #mv> "]);

    // Learned, a prompt ends itself and is found under output glued after it.
    session.vitals();
    session.push("<9hp 0mv> ");
    assert_eq!(session.read.last().unwrap(), &("<9hp 0mv> ".to_owned(), Some(10)));
    session.push("You rest.\n\r<9hp 1mv> You stand.\n\r");
    assert_eq!(session.read.last().unwrap(), &("<9hp 1mv> You stand.".to_owned(), Some(10)));
}

#[test]
fn only_text_after_vitals_is_learned() {
    let mut session = Session::new(Vec::new());
    session.push("<20hp 30mv> ");
    session.gmcp("Room.Info");
    session.push("You rest.\n\r");
    assert!(session.shapes().is_empty());
}

/// The network splits a prompt at every possible offset; the half that
/// arrived first is never learned, however the rest arrives.
#[test]
fn a_prompt_split_across_reads_never_learns_the_half() {
    let prompt = "(110h 125m 120v 1062)> ";
    for at in 1..prompt.len() {
        let (first, rest) = prompt.split_at(at);
        let endings: [&[&str]; 4] = [
            // The rest, then a GMCP message.
            &[rest, ""],
            // The rest, then output glued on.
            &[rest, "You rest.\n\r"],
            // The rest and output in one read.
            &[&format!("{rest}You rest.\n\r")],
            // The rest, then the next line.
            &[&format!("{rest}\n\r")],
        ];
        for ending in endings {
            let mut session = Session::new(Vec::new());
            for _ in 0..3 {
                session.vitals();
                session.push("\n\r");
                session.push(first);
                for read in ending {
                    session.push(read);
                }
                session.gmcp("Char.Status");
            }
            let shapes = session.shapes();
            assert!(!shapes.is_empty(), "split at {at}, {ending:?}: nothing learned");
            for shape in &shapes {
                assert!(shape.starts_with("(#h #m #v #)> "),"split at {at}, {ending:?}: learned {shape:?}");
            }
        }
    }
}

/// From the prototype: learned once with output glued on, the prompt still
/// leaves that output to be read once the bare prompt is seen.
#[test]
fn a_bare_prompt_wins_over_one_learned_with_output_glued() {
    let mut session = Session::new(Vec::new());
    session.defaults.prompt = PromptLearner::learning(["(110h-125m-170v-568)> You are carrying:"]);
    session.vitals();
    session.push("(110h-125m-170v-568)> ");
    session.gmcp("Char.Status");
    assert_eq!(session.defaults.prompt.prefix_len("(9h-1m-1v-1)> You are carrying:"), Some("(9h-1m-1v-1)> ".len()));
}

#[test]
fn leaves_other_text_alone() {
    let learner = PromptLearner::learning(["(110h-125m-120v-1062)> ", "(AFK) (110h-125m-170v-568)> "]);
    for line in ["(Humming) a wooden staff", "(Glowing) a ring", "You are carrying:"] {
        assert_eq!(learner.prefix_len(line), None, "{line}");
    }
}

#[test]
fn prompt_lines_are_hidden_by_default() {
    assert!(Defaults::default().hide_prompts);
}

// ---- HUD fields ----

fn object(json: &str) -> Map<String, Value> {
    serde_json::from_str(json).unwrap()
}

#[test]
fn hud_fields_come_from_the_first_vitals_by_common_names() {
    let spellings = [
        (r#"{"hp": 1, "maxhp": 2, "mana": 3, "maxmana": 4, "moves": 5, "maxmoves": 6}"#, ["hp", "maxhp", "mana", "maxmana", "moves", "maxmoves"]),
        (r#"{"HP": 1, "max_hp": 2, "mp": 3, "max_mp": 4, "mv": 5, "maxmv": 6}"#, ["HP", "max_hp", "mp", "max_mp", "mv", "maxmv"]),
        (r#"{"health": 1, "maxHealth": 2, "sp": 3, "maxsp": 4, "movement": 5, "max-moves": 6}"#, ["health", "maxHealth", "sp", "maxsp", "movement", "max-moves"]),
    ];
    let host = PluginHost::new([defaults::plugin()]).unwrap();
    let vitals = [Vital::Hp, Vital::MaxHp, Vital::Mana, Vital::MaxMana, Vital::Moves, Vital::MaxMoves];
    for (json, keys) in spellings {
        let mut hud = HudFields::default();
        hud.vitals_arrived(&object(json));
        hud.vitals_arrived(&object(r#"{"hits": 9}"#));
        let fields = hud.fields(&host);
        for (vital, key) in vitals.iter().zip(keys) {
            assert_eq!(fields.get(vital), Some(&Field { package: "Char.Vitals".into(), key: key.into() }), "{json}");
        }
        assert_eq!(fields.len(), 6, "{json}: unknown keys are left out");
    }
}

#[test]
fn a_plugins_hud_mapping_wins() {
    let hud = "[vitals]\nhp = \"Char.Vitals.life\"\nrage = \"Char.Status.rage\"\n";
    let host = PluginHost::new([defaults::plugin(), game("rager", "hosts = [\"r.example\"]", &[("hud", "hud.toml", hud)])]).unwrap();
    let mut learned = HudFields::default();
    learned.vitals_arrived(&object(r#"{"hp": "5", "life": "7", "maxhp": "9"}"#));
    let fields = learned.fields(&host);
    assert_eq!(fields[&Vital::Hp], Field { package: "Char.Vitals".into(), key: "life".into() });
    assert_eq!(fields[&Vital::MaxHp].key, "maxhp");
    assert_eq!(fields[&Vital::Other("rage".into())].package, "Char.Status");

    // Numeric strings count, as games send them.
    let read = values(&fields, "Char.Vitals", &object(r#"{"life": "7", "maxhp": 9}"#));
    assert_eq!(read, [(Vital::Hp, 7), (Vital::MaxHp, 9)]);
    // GMCP package names ignore case.
    assert_eq!(values(&fields, "char.vitals", &object(r#"{"life": 7}"#)), [(Vital::Hp, 7)]);
}

// ---- Chaining and speedwalk ----

#[test]
fn a_chain_without_a_separator_goes_as_separate_lines() {
    let commands = split_chain("n; n ;;open s");
    assert_eq!(commands, ["n", "n", "open s"]);
    assert_eq!(chain_lines(&commands, None), ["n", "n", "open s"]);
}

#[test]
fn a_chain_with_a_separator_is_packed_into_it() {
    let host = PluginHost::new([defaults::plugin(), fixture("testland")]).unwrap();
    let separator = host.settings().separator.as_deref();
    assert_eq!(chain_lines(&split_chain("n;n;open s"), separator), ["n;n;open s"]);
    assert_eq!(chain_lines(&["north", "south"], Some("=")), ["north=south"]);

    let long = chain_lines(&vec!["north"; 100], Some("="));
    assert!(long.len() > 1);
    assert!(long.iter().all(|line| line.len() <= 200));
    assert_eq!(long.iter().flat_map(|line| line.split('=')).count(), 100);
}

#[test]
fn speedwalk_expands_to_moves() {
    let walk = WalkSyntax::default();
    let moves = |direction: &str, count| vec![direction.to_owned(); count];
    assert_eq!(walk.expand("3n2e"), Some([moves("north", 3), moves("east", 2)].concat()));
    assert_eq!(
        walk.expand("8n, 7w, [s], d, 2n, w"),
        Some([moves("north", 8), moves("west", 7), vec!["open south".into(), "south".into(), "down".into()], moves("north", 2), moves("west", 1)].concat())
    );
    assert_eq!(walk.expand("3E 2[U]"), Some([moves("east", 3), vec!["open up".into()], moves("up", 2)].concat()));
    for typed in ["n", "d", "ne", "news", "look", "kill goblin", "2 goblins", "[s", "8n, kill rat", "", "say 2n", "100n"] {
        assert_eq!(walk.expand(typed), None, "{typed:?}");
    }
}

#[test]
fn speedwalk_uses_the_games_syntax_when_declared() {
    let host = PluginHost::new([defaults::plugin(), fixture("testland")]).unwrap();
    let walk = WalkSyntax::of(&host);
    assert_eq!(walk.speedwalk, Some(Speedwalk { step: "{count}{dir}".into(), join: " ".into() }));
    assert_eq!(walk.expand("3n2e"), Some(vec!["3n 2e".to_owned()]));
    assert_eq!(walk.expand("3n [e] 2e"), Some(vec!["3n".to_owned(), "open door east".into(), "3e".into()]));
}

// ---- Pagers ----

#[test]
fn the_pager_is_answered_only_during_a_table_read() {
    for pager in ["[B]ack, [R]efresh, or [C]ontinue: ", "--More--", "-- More --", "[Press RETURN to continue]"] {
        let mut session = Session::new(Vec::new());
        session.push(&format!("A help page.\n\r{pager}"));
        assert!(session.sent.is_empty(), "{pager}: answered outside a table read");

        session.defaults.pager.begin_table_read();
        session.push(&format!("\n\rRow one\n\r{pager}"));
        session.push("");
        assert_eq!(session.sent, [""], "{pager}: answered once with a blank line");
        session.defaults.pager.end_table_read();
        session.push(&format!("\n\rRow two\n\r{pager}"));
        assert_eq!(session.sent, [""], "{pager}: answered after the read");
    }
}

#[test]
fn a_game_plugins_pager_wins() {
    let mut session = Session::new(vec![fixture("testland")]);
    session.defaults.pager.begin_table_read();
    session.push("Row\n\r--More--");
    assert!(session.sent.is_empty());
    session.push("\n\r[Space for more]");
    assert_eq!(session.sent, [" "]);
}

// ---- Game detection ----

#[test]
fn a_game_is_suggested_from_each_source() {
    let plugins = [
        defaults::plugin(),
        fixture("testland"),
        game("elsewhere", "hosts = [\"mud.elsewhere.example\"]\nmssp_names = [\"Elsewhere\"]", &[]),
    ];
    let suggestion = |signals| suggest(&plugins, signals);
    let elsewhere = |source| Some(Suggestion { plugin: PluginId::try_from("elsewhere".to_owned()).unwrap(), source });
    let testland = |source| Some(Suggestion { plugin: PluginId::try_from("testland".to_owned()).unwrap(), source });

    assert_eq!(suggestion(ServerSignals { host: "unknown.example", mssp_name: Some("elsewhere"), gmcp_hello: None }), elsewhere(Source::Mssp));
    assert_eq!(suggestion(ServerSignals { host: "unknown.example", mssp_name: None, gmcp_hello: Some("Testland") }), testland(Source::GmcpHello));
    assert_eq!(suggestion(ServerSignals { host: "eu.testland.example", ..ServerSignals::default() }), testland(Source::Host));
    // The server's own name for its game beats its host.
    assert_eq!(suggestion(ServerSignals { host: "mud.testland.example", mssp_name: Some("Elsewhere"), gmcp_hello: None }), elsewhere(Source::Mssp));
    assert_eq!(suggestion(ServerSignals { host: "unknown.example", mssp_name: Some("Nowhere"), gmcp_hello: None }), None);
}

#[test]
fn the_gmcp_hello_names_the_game() {
    let mut defaults = Defaults::default();
    defaults.gmcp("Core.Hello", r#"{"name": "Testland", "version": "2"}"#);
    assert_eq!(defaults.gmcp_hello(), Some("Testland"));
}

// ---- Text ----

#[test]
fn the_default_text_layer_passes_text_through_uncolored() {
    let mut host = PluginHost::new([defaults::plugin()]).unwrap();
    let mut lines = Vec::new();
    LinePipeline::default().push(b"Exits: north and south.\n", &mut lines);
    let mut events = Vec::new();
    assert!(host.read_line(&lines[0], &mut events).is_empty());
    assert!(events.is_empty());
}
