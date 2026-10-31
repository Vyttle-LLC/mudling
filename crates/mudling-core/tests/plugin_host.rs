//! The plugin host end to end, over the synthetic plugins in
//! `tests/fixtures/plugins`.

use mudling_core::ansi::{Color, Span, Style};
use mudling_core::line::{Encoding, Line, LinePipeline};
use mudling_core::plugin::points::{Keywords, LoginReply, RoomIdentity};
use mudling_core::plugin::{
    Layer, MapSignal, PendingKind, Plugin, PluginError, PluginEvent, PluginHost, PluginId, Point,
    Problem, ServerInfo,
};
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Built from components, so paths compare equal on every OS.
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("plugins")
}

fn fixture(name: &str) -> Plugin {
    Plugin::load(fixtures_dir().join(name)).unwrap_or_else(|error| panic!("{error}"))
}

fn defaults() -> Plugin {
    fixture("defaults")
}

fn testland() -> Plugin {
    fixture("testland")
}

fn local_override() -> Plugin {
    fixture("local-override")
}

fn id(id: &str) -> PluginId {
    PluginId::try_from(id.to_owned()).unwrap()
}

/// A plugin from in-memory files.
fn plugin(files: &[(&str, &str)]) -> Result<Plugin, PluginError> {
    Plugin::from_files(|name| {
        files
            .iter()
            .find(|(file, _)| *file == name)
            .map(|(_, text)| text.to_string())
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such file"))
    })
}

const MANIFEST: &str = r#"
id = "tiny"
name = "Tiny"
version = "1.0.0"
api = 1
license = "MIT"
layer = "default"
"#;

fn lines(text: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    LinePipeline::default().push(text.as_bytes(), &mut lines);
    lines
}

fn line(text: &str) -> Line {
    lines(&format!("{text}\n")).remove(0)
}

fn events(host: &mut PluginHost, text: &str) -> Vec<PluginEvent> {
    let mut events = Vec::new();
    host.read_line(&line(text), &mut events);
    events
}

fn names(events: &[PluginEvent]) -> Vec<(&str, &str)> {
    events
        .iter()
        .map(|event| (event.plugin.as_str(), &*event.name))
        .collect()
}

// ---- Manifests ----

#[test]
fn a_plugin_declares_all_nine_points_as_typed_data() {
    let plugin = testland();
    assert_eq!(plugin.id().as_str(), "testland");
    assert_eq!(plugin.layer(), Layer::Game);
    assert_eq!(plugin.manifest.encoding(), Some(Encoding::Latin1));
    let points = &plugin.points;
    let connection = points.connection.as_ref().unwrap();
    assert_eq!(connection.gmcp, ["Char 1", "Room 1"]);
    assert_eq!(connection.logins[1].send, LoginReply::Password);
    let commands = points.commands.as_ref().unwrap();
    assert_eq!(commands.separator.as_deref(), Some(";"));
    assert_eq!(commands.verbs["remove"], "unequip");
    assert_eq!(commands.aliases[0].expansion, "rest;sleep");
    assert_eq!(
        points.hud.as_ref().unwrap().vitals["life"],
        "Char.Vitals.life"
    );
    let map = points.map.as_ref().unwrap();
    assert_eq!(map.room_identity, Some(RoomIdentity::Gmcp));
    assert_eq!(map.failed_move[0].name.as_str(), "blocked");
    let character = points.character.as_ref().unwrap();
    assert_eq!(
        character.readers[0].row[0].as_str(),
        r"^(?<stat>\w+): (?<value>\d+)$"
    );
    let gear = points.gear.as_ref().unwrap();
    assert_eq!(
        gear.equipment
            .as_ref()
            .unwrap()
            .hook
            .as_ref()
            .unwrap()
            .as_str(),
        "read_worn"
    );
    assert_eq!(gear.slots, ["head", "body", "hands"]);
    assert_eq!(
        points.mobs.as_ref().unwrap().room_title[0].name.as_str(),
        "mob.name"
    );
    assert_eq!(points.ui.as_ref().unwrap().panels, ["character", "gear"]);
    let text = points.text.as_ref().unwrap();
    assert_eq!(text.patterns.len(), 2);
    assert_eq!(text.pager.as_ref().unwrap().key, " ");
}

#[test]
fn a_plugin_names_the_servers_it_is_for() {
    let server = |host| ServerInfo {
        host,
        mssp_name: None,
    };
    let game = testland().manifest;
    assert!(game.matches_server(server("mud.testland.example")));
    assert!(game.matches_server(server("EU.Testland.Example")));
    assert!(!game.matches_server(server("testland.example")));
    assert!(!game.matches_server(server("elsewhere.example")));
    assert!(game.matches_server(ServerInfo {
        host: "10.0.0.1",
        mssp_name: Some("testland")
    }));
    assert!(
        defaults()
            .manifest
            .matches_server(server("elsewhere.example"))
    );
}

/// Loads a plugin from `manifest` plus `files` and returns its error.
fn rejection(manifest: &str, files: &[(&str, &str)]) -> PluginError {
    let mut all = vec![("plugin.toml", manifest)];
    all.extend_from_slice(files);
    plugin(&all).expect_err("the plugin should be rejected")
}

fn with(extra: &str) -> String {
    format!("{MANIFEST}{extra}")
}

#[test]
fn a_host_is_exact_or_has_one_leading_wildcard() {
    let game = |hosts: &str| {
        format!(
            "{}[servers]\nhosts = [{hosts}]\n",
            MANIFEST.replace("\"default\"", "\"game\"")
        )
    };
    assert!(
        plugin(&[(
            "plugin.toml",
            &game(r#""mud.example.org", "*.example.org""#)
        )])
        .is_ok()
    );
    for host in [
        "*example.org",
        "mud.*.org",
        "*.*.example.org",
        "mud.example.*",
        "*",
        "*.",
        "",
    ] {
        let error = rejection(&game(&format!("\"{host}\"")), &[]).to_string();
        assert!(
            error.contains(&format!("the host \"{host}\" isn't one")),
            "{error}"
        );
    }
}

#[test]
fn a_manifest_for_another_api_is_rejected_before_anything_else() {
    let manifest = MANIFEST.replace("api = 1", "api = 2\nsomething_new = true");
    let error = rejection(&manifest, &[]);
    assert_eq!(error.problem, Problem::UnsupportedApi("2".into()));
    assert_eq!(
        error.to_string(),
        "plugin.toml: `api = 2` isn't supported; this MUDling reads plugins written for api = 1"
    );
    let error = rejection(&MANIFEST.replace("api = 1", ""), &[]);
    assert_eq!(
        error.to_string(),
        "plugin.toml: `api` is missing; write `api = 1`"
    );
    let error = rejection(&MANIFEST.replace("api = 1", "api = \"1\""), &[]);
    assert_eq!(error.problem, Problem::UnsupportedApi("a string".into()));
}

#[test]
fn manifest_problems_are_readable_errors() {
    let cases: &[(String, &str)] = &[
        (MANIFEST.replace("tiny", "Tiny Plugin"), "isn't a plugin id"),
        (
            MANIFEST.replace("name = ", "nmae = "),
            "unknown field `nmae`",
        ),
        (MANIFEST.replace("1.0.0", "1.0"), "isn't MAJOR.MINOR.PATCH"),
        (MANIFEST.replace("\"MIT\"", "\" \""), "`license` is empty"),
        (
            MANIFEST.replace("\"default\"", "\"core\""),
            "unknown variant `core`",
        ),
        (
            MANIFEST.replace("\"default\"", "\"game\""),
            "must list its `[servers]`",
        ),
        (
            with("[servers]\nhosts = [\"a.example\"]\n"),
            "can't list `[servers]`",
        ),
        (with("encoding = \"ascii\"\n"), "unknown variant `ascii`"),
        (
            with("[points]\ntext = \"../text.toml\"\n"),
            "points.text = \"../text.toml\": the file must be inside",
        ),
        (
            with("[points]\ntext = \"text.txt\"\n"),
            "must be a .toml file",
        ),
        (
            with("[points]\nsound = \"sound.toml\"\n"),
            "unknown field `sound`",
        ),
        (
            with("[points]\ntext = \"text.toml\"\n"),
            "text.toml: couldn't read the file",
        ),
        (
            "id = \"tiny\"\napi = 1\nname = ".into(),
            "plugin.toml: TOML parse error at line 3",
        ),
    ];
    for (manifest, expected) in cases {
        let error = rejection(manifest, &[]).to_string();
        assert!(
            error.contains(expected),
            "expected {expected:?} in {error:?}"
        );
    }
}

#[test]
fn point_file_problems_name_the_file_and_line() {
    let manifest = with("[points]\ntext = \"text.toml\"\ncharacter = \"character.toml\"\n");
    let cases = [
        (
            "text.toml",
            "[[pattern]]\nname = \"x\"\npattern = '''(unclosed'''\n",
            "line 3",
        ),
        (
            "text.toml",
            "[[pattern]]\nname = \"x\"\npattern = '''(unclosed'''\n",
            "the pattern doesn't compile",
        ),
        (
            "text.toml",
            "[[pattern]]\nname = \"\"\npattern = '''x'''\n",
            "isn't an event name",
        ),
        (
            "text.toml",
            "[[pattern]]\nname = \"x\"\npattern = '''x'''\nhook = \"on-x\"\n",
            "isn't a Luau function name",
        ),
        (
            "text.toml",
            "[[pattern]]\nname = \"x\"\npatern = '''x'''\n",
            "unknown field `patern`",
        ),
        (
            "text.toml",
            "[[color]]\npattern = '''x'''\n",
            "sets no fg, bg or bold",
        ),
        (
            "text.toml",
            "[[color]]\npattern = '''(?<a>x)'''\ngroup = \"b\"\nfg = 1\n",
            "names the group `b`",
        ),
        (
            "text.toml",
            "[[color]]\npattern = '''x'''\nfg = 300\n",
            "text.toml: TOML parse error",
        ),
        (
            "character.toml",
            "[[command]]\ncommand = \"stats\"\nreader = \"stats\"\n",
            "isn't declared",
        ),
    ];
    for (file, text, expected) in cases {
        let other = if file == "text.toml" {
            "character.toml"
        } else {
            "text.toml"
        };
        let error = rejection(&manifest, &[(file, text), (other, "")]).to_string();
        assert!(error.starts_with(file), "{error:?} should name {file}");
        assert!(
            error.contains(expected),
            "expected {expected:?} in {error:?}"
        );
    }
}

#[test]
fn loading_from_a_folder_names_the_path() {
    let dir = fixtures_dir().join("missing");
    let error = Plugin::load(&dir).unwrap_err();
    let expected = format!("{}: couldn't read", dir.join("plugin.toml").display());
    assert!(error.to_string().starts_with(&expected), "{error}");
}

#[test]
fn two_plugins_with_one_id_are_rejected() {
    let error = PluginHost::new([testland(), testland()]).err().unwrap();
    assert_eq!(error.problem, Problem::DuplicateId(id("testland")));
    let mut host = PluginHost::new([testland()]).unwrap();
    assert!(host.add(testland()).is_err());
    assert_eq!(host.plugins().count(), 1);
}

// ---- Precedence ----

#[test]
fn the_highest_layer_sets_each_single_value() {
    let host = PluginHost::new([local_override(), defaults(), testland()]).unwrap();
    let settings = host.settings();
    assert_eq!(settings.separator.as_deref(), Some("&&"), "the override's");
    assert_eq!(settings.encoding, Some(Encoding::Utf8), "the override's");
    assert_eq!(settings.pager.as_ref().unwrap().key, "c", "the override's");
    assert_eq!(
        settings.room_identity,
        Some(RoomIdentity::Gmcp),
        "the game's: the override sets none"
    );

    let host = PluginHost::new([testland(), defaults()]).unwrap();
    let settings = host.settings();
    assert_eq!(settings.separator.as_deref(), Some(";"));
    assert_eq!(settings.encoding, Some(Encoding::Latin1));
    assert_eq!(settings.pager.as_ref().unwrap().key, " ");

    let host = PluginHost::new([defaults()]).unwrap();
    let settings = host.settings();
    assert_eq!(
        (settings.separator.as_deref(), settings.encoding),
        (None, None)
    );
    assert_eq!(settings.room_identity, Some(RoomIdentity::Description));
}

#[test]
fn many_value_declarations_come_in_layer_order() {
    let host = PluginHost::new([local_override(), testland(), defaults()]).unwrap();
    let enabled: Vec<&str> = host.enabled().map(|plugin| plugin.id().as_str()).collect();
    assert_eq!(
        enabled,
        ["synthetic-defaults", "testland", "local-override"]
    );
    let maps: Vec<&str> = host
        .declared(|points| points.map.as_ref())
        .map(|(id, _)| id.as_str())
        .collect();
    assert_eq!(maps, ["synthetic-defaults", "testland"]);
}

#[test]
fn every_plugin_matching_a_line_fires_in_layer_order() {
    let mut host = PluginHost::new([local_override(), testland(), defaults()]).unwrap();
    let events = events(&mut host, "Ana says 'well met'");
    assert_eq!(
        names(&events),
        [
            ("synthetic-defaults", "say"),
            ("testland", "speech"),
            ("local-override", "friend.speaks")
        ]
    );
    assert_eq!(events[0].capture("who"), Some("Ana"));
    assert_eq!(events[0].capture("message"), Some("well met"));
    assert_eq!(events[1].capture("speaker"), Some("Ana"));
    assert_eq!(events[1].capture("words"), Some("well met"));
    assert!(events[2].captures.is_empty());
    assert_eq!(events[0].line.plain(), "Ana says 'well met'");
}

#[test]
fn a_pattern_names_its_luau_hook_on_the_event() {
    let mut host = PluginHost::new([testland()]).unwrap();
    let told = events(&mut host, "Bo tells you 'psst'");
    assert_eq!(names(&told), [("testland", "tell")]);
    assert_eq!(
        told[0].hook.as_ref().map(|hook| hook.as_str()),
        Some("on_tell")
    );
    assert_eq!(told[0].capture("message"), Some("psst"));
    assert!(events(&mut host, "Ana says 'hi'")[0].hook.is_none());
}

#[test]
fn lines_no_pattern_matches_send_nothing() {
    let mut host = PluginHost::new([local_override(), testland(), defaults()]).unwrap();
    assert!(events(&mut host, "A breeze stirs the leaves.").is_empty());
    assert!(events(&mut host, "").is_empty());
}

// ---- Coloring ----

fn fg(color: u8, bold: bool) -> Style {
    Style {
        fg: Some(Color(color)),
        bg: None,
        bold,
    }
}

fn coloring(host: &mut PluginHost, text: &str) -> Vec<Span> {
    host.read_line(&line(text), &mut Vec::new())
}

#[test]
fn coloring_rules_paint_plain_lines_in_layer_order() {
    let mut host = PluginHost::new([testland(), defaults()]).unwrap();
    assert_eq!(
        coloring(&mut host, "Ana says 'hi'"),
        [Span {
            start: 0,
            end: 3,
            style: fg(3, true)
        }],
        "only `who`"
    );
    assert_eq!(
        coloring(&mut host, "Exits: north"),
        [Span {
            start: 0,
            end: 12,
            style: fg(6, false)
        }]
    );

    host.add(local_override()).unwrap();
    assert_eq!(
        coloring(&mut host, "Ana says 'hi'"),
        [Span {
            start: 0,
            end: 9,
            style: fg(2, false)
        }],
        "painted over"
    );
}

#[test]
fn lines_the_server_colored_keep_their_colors() {
    let mut host = PluginHost::new([testland(), defaults()]).unwrap();
    let styled = line("\x1b[35mExits: north\x1b[0m");
    let mut events = Vec::new();
    assert!(host.read_line(&styled, &mut events).is_empty());
}

// ---- Pending text ----

#[test]
fn prompt_hints_and_the_pager_read_pending_text() {
    let mut host = PluginHost::new([testland(), defaults()]).unwrap();
    let mut pipeline = LinePipeline::default();
    let mut finished = Vec::new();

    pipeline.push(b"(20 life) ", &mut finished);
    assert_eq!(
        host.read_pending(&pipeline.pending().unwrap()),
        Some(PendingKind::Prompt)
    );
    pipeline.prompt_end();

    pipeline.push(b"<12hp> ", &mut finished);
    assert_eq!(
        host.read_pending(&pipeline.pending().unwrap()),
        Some(PendingKind::Prompt),
        "the defaults' hint"
    );
    pipeline.prompt_end();

    pipeline.push(b"[Space for", &mut finished);
    assert_eq!(
        host.read_pending(&pipeline.pending().unwrap()),
        None,
        "half a pager prompt"
    );
    pipeline.push(b" more]", &mut finished);
    assert_eq!(
        host.read_pending(&pipeline.pending().unwrap()),
        Some(PendingKind::Pager { key: " ".into() })
    );
    pipeline.prompt_end();

    pipeline.push(b"--More--", &mut finished);
    assert_eq!(
        host.read_pending(&pipeline.pending().unwrap()),
        None,
        "the game's pager replaces the defaults'"
    );
    assert!(finished.is_empty());
}

#[test]
fn a_new_pipeline_gets_fresh_answers_from_the_same_host() {
    let mut host = PluginHost::new([testland()]).unwrap();
    let mut first = LinePipeline::default();
    first.push(b"(20 life) ", &mut Vec::new());
    assert_eq!(
        host.read_pending(&first.pending().unwrap()),
        Some(PendingKind::Prompt)
    );

    // A reconnect: line 0 again, and text of the same length.
    let mut second = LinePipeline::default();
    second.push(b"Hello you ", &mut Vec::new());
    let pending = second.pending().unwrap();
    assert_eq!((pending.number, pending.raw.len()), (0, 10));
    assert_eq!(host.read_pending(&pending), None);
}

#[test]
fn with_only_the_defaults_their_pager_applies() {
    let mut host = PluginHost::new([defaults()]).unwrap();
    let mut pipeline = LinePipeline::default();
    pipeline.push(b"--More--", &mut Vec::new());
    assert_eq!(
        host.read_pending(&pipeline.pending().unwrap()),
        Some(PendingKind::Pager { key: "".into() })
    );
}

// ---- Recompiling ----

#[test]
fn enabling_or_disabling_a_plugin_recompiles_and_nothing_else_does() {
    let mut host = PluginHost::new([testland(), defaults()]).unwrap();
    let compiled = host.generation();
    let said = "Ana says 'hi'";
    assert_eq!(events(&mut host, said).len(), 2);
    assert_eq!(
        host.generation(),
        compiled,
        "reading lines doesn't recompile"
    );

    assert_eq!(host.set_enabled(&id("testland"), false), Ok(true));
    assert_eq!(host.generation(), compiled + 1);
    assert_eq!(
        names(&events(&mut host, said)),
        [("synthetic-defaults", "say")]
    );
    assert_eq!(host.settings().separator, None);
    assert_eq!(host.is_enabled(&id("testland")), Some(false));

    assert_eq!(
        host.set_enabled(&id("testland"), false),
        Ok(false),
        "already off"
    );
    assert_eq!(
        host.set_enabled(&id("nowhere"), true),
        Ok(false),
        "not installed"
    );
    assert_eq!(host.generation(), compiled + 1);

    assert_eq!(host.set_enabled(&id("testland"), true), Ok(true));
    assert_eq!(host.generation(), compiled + 2);
    assert_eq!(events(&mut host, said).len(), 2);
    assert_eq!(host.settings().separator.as_deref(), Some(";"));

    host.add(local_override()).unwrap();
    assert_eq!(events(&mut host, said).len(), 3);
    assert!(host.remove(&id("local-override")).unwrap().is_some());
    assert!(host.remove(&id("local-override")).unwrap().is_none());
    assert_eq!(events(&mut host, said).len(), 2);
    assert_eq!(host.generation(), compiled + 4);
}

// ---- Costs and linear time ----

#[test]
fn each_enabled_plugin_s_cost_is_sampled() {
    let mut host = PluginHost::new([testland(), defaults()]).unwrap();
    host.set_cost_sampling(2);
    host.set_enabled(&id("synthetic-defaults"), false).unwrap();
    for _ in 0..10 {
        events(&mut host, "Ana says 'hi'");
    }
    let costs = host.costs();
    assert_eq!(costs[0].plugin, id("testland"));
    assert_eq!(costs[0].sampled_lines, 5);
    assert!(costs[0].per_line.unwrap().max_us > 0.0);
    assert_eq!(
        (costs[1].sampled_lines, costs[1].per_line),
        (0, None),
        "off, so not timed"
    );
}

#[test]
fn a_pathological_pattern_cannot_hang_the_host() {
    let text = "[[pattern]]\nname = \"slow\"\npattern = '''^(a+)+$'''\n\n\
        [[color]]\npattern = '''(x+x+)+y'''\nfg = 1\n";
    let tiny = plugin(&[
        ("plugin.toml", &with("[points]\ntext = \"text.toml\"\n")),
        ("text.toml", text),
    ])
    .unwrap();
    let mut host = PluginHost::new([tiny]).unwrap();
    let near_miss = format!("{}! {} y", "a".repeat(30_000), "x".repeat(30_000));
    let started = Instant::now();
    let mut sent = Vec::new();
    assert!(host.read_line(&line(&near_miss), &mut sent).is_empty());
    assert!(sent.is_empty());
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
}

fn with_points(points: &str) -> String {
    format!("{MANIFEST}\n[points]\n{points}")
}

/// A plugin with a table: `start`, rows, `done` or a prompt to end it.
fn tables() -> PluginHost {
    let plugin = plugin(&[
        (
            "plugin.toml",
            &with_points("text = \"text.toml\"\ncharacter = \"character.toml\"\n"),
        ),
        (
            "text.toml",
            "[[prompt]]\npattern = '''^<\\d+>'''\n\
             [pager]\nprompt = '''--more--$'''\nkey = \"\"\n",
        ),
        (
            "character.toml",
            "[[reader]]\nname = \"stats\"\nstart = ['''^Stats$''', '''^Numbers$''']\n\
             row = ['''^(?<stat>\\w+): (?<value>\\d+)$''', '''^(?<note>\\* .+)$''']\n\
             end = '''^done$'''\n\
             [[reader]]\nname = \"list\"\nstart = '''^(?:<\\d+> ?)?List$'''\nrow = '''^- (?<item>.+)$'''\n",
        ),
    ])
    .unwrap_or_else(|error| panic!("{error}"));
    PluginHost::new([plugin]).unwrap()
}

fn read_all(host: &mut PluginHost, text: &[&str]) -> Vec<PluginEvent> {
    let mut events = Vec::new();
    for text in text {
        host.read_line(&line(text), &mut events);
    }
    events
}

fn event_names(events: &[PluginEvent]) -> Vec<&str> {
    events.iter().map(|event| &*event.name).collect()
}

#[test]
fn every_point_s_rules_run_and_say_which_point() {
    let mut host = PluginHost::new([testland()]).unwrap();
    let points = |host: &mut PluginHost, text: &str| -> Vec<(Point, String)> {
        events(host, text)
            .into_iter()
            .map(|event| (event.point, event.name.to_string()))
            .collect()
    };
    assert_eq!(
        points(&mut host, "You can't go that way."),
        [(Point::Map, "blocked".to_owned())]
    );
    assert_eq!(
        points(&mut host, "Your archery improves."),
        [(Point::Character, "skill.improved".to_owned())]
    );
    assert_eq!(
        points(&mut host, "You put on a cap."),
        [(Point::Gear, "wear".to_owned())]
    );
    let aggro = events(&mut host, "A hedge wolf lunges at you!");
    assert_eq!(aggro[0].point, Point::Mobs);
    assert_eq!(aggro[0].map_signal, Some(MapSignal::Aggressor));
    assert_eq!(aggro[0].capture("name"), Some("A hedge wolf"));
    assert!(
        events(&mut host, "The lair of Bob").is_empty(),
        "a room title rule reads titles, not lines"
    );
}

#[test]
fn a_table_runs_start_rows_and_end_and_rows_need_it_open() {
    let mut host = tables();
    assert!(
        read_all(&mut host, &["str: 10"]).is_empty(),
        "no table open"
    );
    assert!(!host.table_open());

    let events = read_all(
        &mut host,
        &[
            "Stats",
            "Numbers",
            "str: 10",
            "chatter",
            "more chatter",
            "* a note",
            "done",
            "dex: 1",
        ],
    );
    assert_eq!(
        event_names(&events),
        [
            "stats.start",
            "stats.start",
            "stats.row",
            "stats.gap",
            "stats.row",
            "stats.end"
        ],
        "a second start with no rows is the same table, and a gap is told once"
    );
    assert_eq!(events[0].point, Point::Character);
    assert_eq!(events[2].capture("value"), Some("10"));
    assert_eq!(events[4].capture("note"), Some("* a note"));
    assert!(!host.table_open());
}

#[test]
fn a_prompt_or_the_next_table_ends_a_table_and_output_after_a_prompt_is_read() {
    let mut host = tables();
    let events = read_all(&mut host, &["Stats", "str: 1", "<5> List", "- rope", "<5>"]);
    assert_eq!(
        event_names(&events),
        [
            "stats.start",
            "stats.row",
            "stats.end",
            "list.start",
            "list.row",
            "list.end"
        ]
    );

    let events = read_all(&mut host, &["Stats", "str: 1", "List"]);
    assert_eq!(
        event_names(&events)[2..],
        ["stats.end", "list.start"],
        "a plugin reads one table at a time"
    );
    host.cut_tables(&mut Vec::new());
}

#[test]
fn a_line_ended_at_ga_ends_a_table_but_a_pager_prompt_does_not() {
    let mut host = tables();
    let mut pipeline = LinePipeline::default();
    let mut lines = Vec::new();
    pipeline.push(b"Stats\r\nstr: 1\r\n--more--", &mut lines);
    lines.extend(pipeline.prompt_end());
    pipeline.push(b"hp> ", &mut lines);
    lines.extend(pipeline.prompt_end());
    let mut events = Vec::new();
    for line in &lines[..3] {
        host.read_line(line, &mut events);
    }
    host.end_tables_at_signal(&lines[2], &mut events);
    assert!(host.table_open(), "{:?}", event_names(&events));
    host.read_line(&lines[3], &mut events);
    assert!(
        host.table_open(),
        "reading a line ended by quiet ends nothing"
    );
    host.end_tables_at_signal(&lines[3], &mut events);
    assert_eq!(event_names(&events).last(), Some(&"stats.end"));
}

#[test]
fn the_pager_s_own_line_is_no_gap_in_a_table() {
    let mut host = tables();
    let events = read_all(&mut host, &["Stats", "str: 1", "--more--", "chatter"]);
    assert_eq!(
        event_names(&events),
        ["stats.start", "stats.row", "stats.gap"],
        "the gap is the line after the pager's"
    );
    assert_eq!(events[2].line.plain(), "chatter");
}

#[test]
fn a_recompile_cuts_open_tables_and_the_next_read_delivers_it() {
    let mut host = tables();
    read_all(&mut host, &["Stats", "str: 1"]);
    host.add(defaults()).unwrap();
    assert!(!host.table_open());
    let events = read_all(&mut host, &["chatter"]);
    assert_eq!(event_names(&events), ["stats.cut"]);
    assert_eq!(events[0].line.plain(), "Stats");
    assert!(read_all(&mut host, &["chatter"]).is_empty(), "told once");
}

#[test]
fn a_table_left_at_its_pager_is_cut_and_an_answered_one_ends() {
    let pending = |host: &mut PluginHost| {
        let mut pipeline = LinePipeline::default();
        pipeline.push(b"--more--", &mut Vec::new());
        assert!(host.read_pending(&pipeline.pending().unwrap()).is_some());
    };

    let mut host = tables();
    read_all(&mut host, &["Stats", "str: 1"]);
    pending(&mut host);
    let events = read_all(&mut host, &["", "<5>"]);
    assert_eq!(event_names(&events), ["stats.gap", "stats.cut"]);

    let mut host = tables();
    read_all(&mut host, &["Stats", "str: 1"]);
    pending(&mut host);
    host.pager_answered();
    // Asked again about the same pending line, as the next GMCP message does.
    pending(&mut host);
    let events = read_all(&mut host, &["", "<5>"]);
    assert_eq!(event_names(&events), ["stats.gap", "stats.end"]);

    let mut host = tables();
    read_all(&mut host, &["Stats", "str: 1"]);
    let mut events = Vec::new();
    host.cut_tables(&mut events);
    assert_eq!(event_names(&events), ["stats.cut"]);
    assert_eq!(events[0].line.plain(), "Stats");
    assert!(!host.table_open());
}

#[test]
fn a_prompt_shape_must_be_all_of_the_pending_text() {
    let mut host = tables();
    let mut pending = |text: &[u8]| {
        let mut pipeline = LinePipeline::default();
        pipeline.push(text, &mut Vec::new());
        host.read_pending(&pipeline.pending().unwrap())
    };
    assert_eq!(pending(b"<5> "), Some(PendingKind::Prompt));
    assert_eq!(pending(b"<5> You are"), None, "output on its way");
}

#[test]
fn read_line_from_reads_what_follows_and_ranges_stay_the_whole_lines() {
    let mut host = PluginHost::new([testland()]).unwrap();
    let mut events = Vec::new();
    let glued = line("hp> Ana says 'hi'");
    let coloring = host.read_line_from(&glued, 4, &mut events);
    assert_eq!(events[0].capture("speaker"), Some("Ana"));
    assert_eq!(
        &glued.plain()[coloring[0].start as usize..coloring[0].end as usize],
        "Ana"
    );
}

#[test]
fn fragments_are_shared_by_every_point_file() {
    let manifest = format!(
        "{}\n[points]\ntext = \"text.toml\"\nmobs = \"mobs.toml\"\n\
         [fragments]\ntag = '''\\[\\w+\\] |<\\w+> '''\n",
        MANIFEST
    );
    let plugin = plugin(&[
        ("plugin.toml", &manifest),
        (
            "text.toml",
            "[[pattern]]\nname = \"hi\"\npattern = '''^{tag}?hi{2}$'''\n",
        ),
        (
            "mobs.toml",
            "[[aggro]]\nname = \"aggro\"\npattern = '''^{tag}(?<name>\\w+) bites!$'''\n",
        ),
    ])
    .unwrap_or_else(|error| panic!("{error}"));
    let mut host = PluginHost::new([plugin]).unwrap();
    assert_eq!(events(&mut host, "hii").len(), 1);
    assert_eq!(events(&mut host, "<x> hii").len(), 1);
    assert_eq!(
        events(&mut host, "[x] Rat bites!")[0].capture("name"),
        Some("Rat")
    );
}

#[test]
fn a_bad_or_unknown_fragment_is_rejected_at_load() {
    let text = "[[pattern]]\nname = \"x\"\npattern = '''^{nope}$'''\n";
    let manifest = with_points("text = \"text.toml\"\n");
    let error = plugin(&[("plugin.toml", &manifest), ("text.toml", text)]).unwrap_err();
    assert_eq!(error.source, "text.toml", "{error}");
    assert!(error.to_string().contains("doesn't compile"), "{error}");

    let error = rejection(&format!("{MANIFEST}\n[fragments]\nbad = '''('''\n"), &[]);
    assert!(
        error.to_string().contains("`bad` doesn't compile"),
        "{error}"
    );
    let error = rejection(&format!("{MANIFEST}\n[fragments]\n2 = '''a'''\n"), &[]);
    assert!(
        error.to_string().contains("isn't a fragment name"),
        "{error}"
    );
}

#[test]
fn a_background_command_may_name_a_gear_table() {
    let manifest = with_points("character = \"character.toml\"\ngear = \"gear.toml\"\n");
    let plugin = plugin(&[
        ("plugin.toml", &manifest),
        (
            "character.toml",
            "[[command]]\ncommand = \"eq\"\nreader = \"worn\"\n",
        ),
        (
            "gear.toml",
            "[equipment]\nname = \"worn\"\nstart = '''^Worn$'''\nrow = '''^x$'''\n",
        ),
    ]);
    plugin.unwrap_or_else(|error| panic!("{error}"));

    let error = rejection(
        &with_points("character = \"character.toml\"\n"),
        &[(
            "character.toml",
            "[[command]]\ncommand = \"eq\"\nreader = \"worn\"\n",
        )],
    );
    assert_eq!(error.source, "character.toml");
    assert!(error.to_string().contains("isn't declared"), "{error}");
}

#[test]
fn link_rules_find_links_in_a_line_by_request() {
    let plugin = plugin(&[
        ("plugin.toml", &with_points("text = \"text.toml\"\n")),
        (
            "text.toml",
            "[[link]]\npattern = '''^You see (?<item>\\w+)\\.$'''\n\
             action = \"get {item}\"\ngroup = \"item\"\n",
        ),
    ])
    .unwrap_or_else(|error| panic!("{error}"));
    let host = PluginHost::new([plugin]).unwrap();
    let links = host.links(&line("You see gold."));
    assert_eq!((links[0].start, links[0].end), (8, 12));
    assert_eq!(&*links[0].action, "get gold");
    assert!(host.links(&line("Nothing here.")).is_empty());
}

#[test]
fn a_link_naming_a_group_it_does_not_capture_is_rejected() {
    let error = rejection(
        &with_points("text = \"text.toml\"\n"),
        &[(
            "text.toml",
            "[[link]]\npattern = '''x'''\naction = \"y\"\ngroup = \"nope\"\n",
        )],
    );
    assert!(
        error.to_string().contains("names the group `nope`"),
        "{error}"
    );
}

#[test]
fn a_room_title_rule_naming_a_hook_is_rejected_at_load() {
    let error = rejection(
        &with_points("mobs = \"mobs.toml\"\n"),
        &[(
            "mobs.toml",
            "[[room_title]]\nname = \"x\"\npattern = '''x'''\nhook = \"on_title\"\n",
        )],
    );
    assert_eq!(error.source, "mobs.toml");
    assert!(error.to_string().contains("can't name a hook"), "{error}");
}

#[test]
fn a_plugins_area_pattern_reaches_the_settings() {
    let host = PluginHost::new([testland()]).unwrap();
    let area = host.settings().area_name.as_ref().unwrap();
    assert!(area.is_match("[1-2] The Moor"));
}

#[test]
fn an_item_s_keyword_follows_its_rules() {
    let keywords = Keywords {
        skip: vec!["a".to_owned(), "the".to_owned()],
        phrase_ends: vec!["of".to_owned()],
        possessives: false,
    };
    let word = |name: &str| keywords.word_for(name);
    assert_eq!(word("a wooden shield").as_deref(), Some("shield"));
    assert_eq!(word("a bottle of water").as_deref(), Some("bottle"));
    assert_eq!(word("the miller's hat").as_deref(), Some("hat"));
    assert_eq!(word("the miller's").as_deref(), Some("miller"));
    assert_eq!(word("the"), None);
    assert_eq!(
        Keywords::default().word_for("the miller's").as_deref(),
        Some("miller's"),
        "a game that takes possessives"
    );
}
