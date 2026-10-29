//! The plugin host end to end, over the synthetic plugins in
//! `tests/fixtures/plugins`.

use mudling_core::ansi::{Color, Span, Style};
use mudling_core::line::{Encoding, Line, LinePipeline};
use mudling_core::plugin::points::{LoginReply, RoomIdentity};
use mudling_core::plugin::{
    Layer, PendingKind, Plugin, PluginError, PluginEvent, PluginHost, PluginId, Problem, ServerInfo,
};
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn fixture(name: &str) -> Plugin {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugins").join(name);
    Plugin::load(&dir).unwrap_or_else(|error| panic!("{error}"))
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
    events.iter().map(|event| (event.plugin.as_str(), &*event.name)).collect()
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
    assert_eq!(points.hud.as_ref().unwrap().vitals["life"], "Char.Vitals.life");
    let map = points.map.as_ref().unwrap();
    assert_eq!(map.room_identity, Some(RoomIdentity::Gmcp));
    assert_eq!(map.failed_move[0].name.as_str(), "blocked");
    let character = points.character.as_ref().unwrap();
    assert_eq!(character.readers[0].row.as_str(), r"^(?<stat>\w+): (?<value>\d+)$");
    let gear = points.gear.as_ref().unwrap();
    assert_eq!(gear.equipment.as_ref().unwrap().hook.as_ref().unwrap().as_str(), "read_worn");
    assert_eq!(gear.slots, ["head", "body", "hands"]);
    assert_eq!(points.mobs.as_ref().unwrap().room_title[0].name.as_str(), "mob.name");
    assert_eq!(points.ui.as_ref().unwrap().panels, ["character", "gear"]);
    let text = points.text.as_ref().unwrap();
    assert_eq!(text.patterns.len(), 2);
    assert_eq!(text.pager.as_ref().unwrap().key, " ");
}

#[test]
fn a_plugin_names_the_servers_it_is_for() {
    let server = |host| ServerInfo { host, mssp_name: None };
    let game = testland().manifest;
    assert!(game.matches_server(server("mud.testland.example")));
    assert!(game.matches_server(server("EU.Testland.Example")));
    assert!(!game.matches_server(server("testland.example")));
    assert!(!game.matches_server(server("elsewhere.example")));
    assert!(game.matches_server(ServerInfo { host: "10.0.0.1", mssp_name: Some("testland") }));
    assert!(defaults().manifest.matches_server(server("elsewhere.example")));
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
        format!("{}[servers]\nhosts = [{hosts}]\n", MANIFEST.replace("\"default\"", "\"game\""))
    };
    assert!(plugin(&[("plugin.toml", &game(r#""mud.example.org", "*.example.org""#))]).is_ok());
    for host in ["*example.org", "mud.*.org", "*.*.example.org", "mud.example.*", "*", "*.", ""] {
        let error = rejection(&game(&format!("\"{host}\"")), &[]).to_string();
        assert!(error.contains(&format!("the host \"{host}\" isn't one")), "{error}");
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
    assert_eq!(error.to_string(), "plugin.toml: `api` is missing; write `api = 1`");
    let error = rejection(&MANIFEST.replace("api = 1", "api = \"1\""), &[]);
    assert_eq!(error.problem, Problem::UnsupportedApi("a string".into()));
}

#[test]
fn manifest_problems_are_readable_errors() {
    let cases: &[(String, &str)] = &[
        (MANIFEST.replace("tiny", "Tiny Plugin"), "isn't a plugin id"),
        (MANIFEST.replace("name = ", "nmae = "), "unknown field `nmae`"),
        (MANIFEST.replace("1.0.0", "1.0"), "isn't MAJOR.MINOR.PATCH"),
        (MANIFEST.replace("\"MIT\"", "\" \""), "`license` is empty"),
        (MANIFEST.replace("\"default\"", "\"core\""), "unknown variant `core`"),
        (MANIFEST.replace("\"default\"", "\"game\""), "must list its `[servers]`"),
        (with("[servers]\nhosts = [\"a.example\"]\n"), "can't list `[servers]`"),
        (with("encoding = \"ascii\"\n"), "unknown variant `ascii`"),
        (with("[points]\ntext = \"../text.toml\"\n"), "points.text = \"../text.toml\": the file must be inside"),
        (with("[points]\ntext = \"text.txt\"\n"), "must be a .toml file"),
        (with("[points]\nsound = \"sound.toml\"\n"), "unknown field `sound`"),
        (with("[points]\ntext = \"text.toml\"\n"), "text.toml: couldn't read the file"),
        ("id = \"tiny\"\napi = 1\nname = ".into(), "plugin.toml: TOML parse error at line 3"),
    ];
    for (manifest, expected) in cases {
        let error = rejection(manifest, &[]).to_string();
        assert!(error.contains(expected), "expected {expected:?} in {error:?}");
    }
}

#[test]
fn point_file_problems_name_the_file_and_line() {
    let manifest = with("[points]\ntext = \"text.toml\"\ncharacter = \"character.toml\"\n");
    let cases = [
        ("text.toml", "[[pattern]]\nname = \"x\"\npattern = '''(unclosed'''\n", "line 3"),
        ("text.toml", "[[pattern]]\nname = \"x\"\npattern = '''(unclosed'''\n", "the pattern doesn't compile"),
        ("text.toml", "[[pattern]]\nname = \"\"\npattern = '''x'''\n", "isn't an event name"),
        ("text.toml", "[[pattern]]\nname = \"x\"\npattern = '''x'''\nhook = \"on-x\"\n", "isn't a Luau function name"),
        ("text.toml", "[[pattern]]\nname = \"x\"\npatern = '''x'''\n", "unknown field `patern`"),
        ("text.toml", "[[color]]\npattern = '''x'''\n", "sets no fg, bg or bold"),
        ("text.toml", "[[color]]\npattern = '''(?<a>x)'''\ngroup = \"b\"\nfg = 1\n", "names the group `b`"),
        ("text.toml", "[[color]]\npattern = '''x'''\nfg = 300\n", "text.toml: TOML parse error"),
        ("character.toml", "[[command]]\ncommand = \"stats\"\nreader = \"stats\"\n", "isn't declared"),
    ];
    for (file, text, expected) in cases {
        let other = if file == "text.toml" { "character.toml" } else { "text.toml" };
        let error = rejection(&manifest, &[(file, text), (other, "")]).to_string();
        assert!(error.starts_with(file), "{error:?} should name {file}");
        assert!(error.contains(expected), "expected {expected:?} in {error:?}");
    }
}

#[test]
fn loading_from_a_folder_names_the_path() {
    let error = Plugin::load("tests/fixtures/plugins/missing").unwrap_err();
    assert!(error.to_string().starts_with("tests/fixtures/plugins/missing/plugin.toml: couldn't read"), "{error}");
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
    assert_eq!(settings.room_identity, Some(RoomIdentity::Gmcp), "the game's: the override sets none");

    let host = PluginHost::new([testland(), defaults()]).unwrap();
    let settings = host.settings();
    assert_eq!(settings.separator.as_deref(), Some(";"));
    assert_eq!(settings.encoding, Some(Encoding::Latin1));
    assert_eq!(settings.pager.as_ref().unwrap().key, " ");

    let host = PluginHost::new([defaults()]).unwrap();
    let settings = host.settings();
    assert_eq!((settings.separator.as_deref(), settings.encoding), (None, None));
    assert_eq!(settings.room_identity, Some(RoomIdentity::Description));
}

#[test]
fn many_value_declarations_come_in_layer_order() {
    let host = PluginHost::new([local_override(), testland(), defaults()]).unwrap();
    let enabled: Vec<&str> = host.enabled().map(|plugin| plugin.id().as_str()).collect();
    assert_eq!(enabled, ["synthetic-defaults", "testland", "local-override"]);
    let maps: Vec<&str> = host.declared(|points| points.map.as_ref()).map(|(id, _)| id.as_str()).collect();
    assert_eq!(maps, ["synthetic-defaults", "testland"]);
}

#[test]
fn every_plugin_matching_a_line_fires_in_layer_order() {
    let mut host = PluginHost::new([local_override(), testland(), defaults()]).unwrap();
    let events = events(&mut host, "Ana says 'well met'");
    assert_eq!(
        names(&events),
        [("synthetic-defaults", "say"), ("testland", "speech"), ("local-override", "friend.speaks")]
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
    assert_eq!(told[0].hook.as_ref().map(|hook| hook.as_str()), Some("on_tell"));
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
    Style { fg: Some(Color(color)), bg: None, bold }
}

fn coloring(host: &mut PluginHost, text: &str) -> Vec<Span> {
    host.read_line(&line(text), &mut Vec::new())
}

#[test]
fn coloring_rules_paint_plain_lines_in_layer_order() {
    let mut host = PluginHost::new([testland(), defaults()]).unwrap();
    assert_eq!(coloring(&mut host, "Ana says 'hi'"), [Span { start: 0, end: 3, style: fg(3, true) }], "only `who`");
    assert_eq!(coloring(&mut host, "Exits: north"), [Span { start: 0, end: 12, style: fg(6, false) }]);

    host.add(local_override()).unwrap();
    assert_eq!(coloring(&mut host, "Ana says 'hi'"), [Span { start: 0, end: 9, style: fg(2, false) }], "painted over");
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
    assert_eq!(host.read_pending(&pipeline.pending().unwrap()), Some(PendingKind::Prompt));
    pipeline.prompt_end();

    pipeline.push(b"<12hp> ", &mut finished);
    assert_eq!(host.read_pending(&pipeline.pending().unwrap()), Some(PendingKind::Prompt), "the defaults' hint");
    pipeline.prompt_end();

    pipeline.push(b"[Space for", &mut finished);
    assert_eq!(host.read_pending(&pipeline.pending().unwrap()), None, "half a pager prompt");
    pipeline.push(b" more]", &mut finished);
    assert_eq!(host.read_pending(&pipeline.pending().unwrap()), Some(PendingKind::Pager { key: " ".into() }));
    pipeline.prompt_end();

    pipeline.push(b"--More--", &mut finished);
    assert_eq!(host.read_pending(&pipeline.pending().unwrap()), None, "the game's pager replaces the defaults'");
    assert!(finished.is_empty());
}

#[test]
fn a_new_pipeline_gets_fresh_answers_from_the_same_host() {
    let mut host = PluginHost::new([testland()]).unwrap();
    let mut first = LinePipeline::default();
    first.push(b"(20 life) ", &mut Vec::new());
    assert_eq!(host.read_pending(&first.pending().unwrap()), Some(PendingKind::Prompt));

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
    assert_eq!(host.read_pending(&pipeline.pending().unwrap()), Some(PendingKind::Pager { key: "".into() }));
}

// ---- Recompiling ----

#[test]
fn enabling_or_disabling_a_plugin_recompiles_and_nothing_else_does() {
    let mut host = PluginHost::new([testland(), defaults()]).unwrap();
    let compiled = host.generation();
    let said = "Ana says 'hi'";
    assert_eq!(events(&mut host, said).len(), 2);
    assert_eq!(host.generation(), compiled, "reading lines doesn't recompile");

    assert_eq!(host.set_enabled(&id("testland"), false), Ok(true));
    assert_eq!(host.generation(), compiled + 1);
    assert_eq!(names(&events(&mut host, said)), [("synthetic-defaults", "say")]);
    assert_eq!(host.settings().separator, None);
    assert_eq!(host.is_enabled(&id("testland")), Some(false));

    assert_eq!(host.set_enabled(&id("testland"), false), Ok(false), "already off");
    assert_eq!(host.set_enabled(&id("nowhere"), true), Ok(false), "not installed");
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
    assert_eq!((costs[1].sampled_lines, costs[1].per_line), (0, None), "off, so not timed");
}

#[test]
fn a_pathological_pattern_cannot_hang_the_host() {
    let text = "[[pattern]]\nname = \"slow\"\npattern = '''^(a+)+$'''\n\n\
        [[color]]\npattern = '''(x+x+)+y'''\nfg = 1\n";
    let tiny = plugin(&[("plugin.toml", &with("[points]\ntext = \"text.toml\"\n")), ("text.toml", text)]).unwrap();
    let mut host = PluginHost::new([tiny]).unwrap();
    let near_miss = format!("{}! {} y", "a".repeat(30_000), "x".repeat(30_000));
    let started = Instant::now();
    let mut sent = Vec::new();
    assert!(host.read_line(&line(&near_miss), &mut sent).is_empty());
    assert!(sent.is_empty());
    assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
}
