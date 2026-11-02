//! The commands as an author runs them: the built binary, on plugins in
//! temporary folders. Every log here is made-up text; real session logs hold
//! other players' chat and never go in the repo.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn mudling(args: &[&str]) -> Run {
    let output = Command::new(env!("CARGO_BIN_EXE_mudling"))
        .args(args)
        .output()
        .expect("the binary runs");
    Run {
        code: output.status.code().expect("it exits by itself"),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn text(path: &Path) -> &str {
    path.to_str().expect("a temporary path is text")
}

/// A new plugin in a temporary folder.
fn scaffolded() -> (TempDir, PathBuf) {
    let parent = TempDir::new().unwrap();
    let run = mudling(&["plugin", "new", "demo", "--dir", text(parent.path())]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    let plugin = parent.path().join("demo");
    (parent, plugin)
}

fn test_plugin(plugin: &Path) -> Run {
    mudling(&["plugin", "test", text(plugin)])
}

fn test_log(plugin: &Path, log: &str) -> Run {
    let path = plugin.join("chatter.txt");
    fs::write(&path, log).unwrap();
    mudling(&["plugin", "test", text(plugin), "--log", text(&path)])
}

/// The words the reference says a line of output starts with.
const LINE_STARTS: [&str; 8] = [
    "plugin ", "log ", "caught ", "  ", "skipped ", "cost ", "updated ", "fail ",
];

/// It failed, with `reason` on a `fail` line, as registry CI reads them.
fn assert_fails(run: &Run, reason: &str) {
    assert_eq!(run.code, 1, "{}{}", run.stdout, run.stderr);
    let (last, lines) = run
        .stdout
        .lines()
        .collect::<Vec<_>>()
        .split_last()
        .map_or(("", Vec::new()), |(last, lines)| (*last, lines.to_vec()));
    assert_eq!(last, "failed", "{}", run.stdout);
    for line in &lines {
        assert!(
            LINE_STARTS.iter().any(|start| line.starts_with(start)),
            "an unmarked line: {line}"
        );
    }
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("fail ") && line.contains(reason)),
        "{}",
        run.stdout
    );
}

#[test]
fn a_new_plugin_loads_and_passes_its_own_test() {
    let (_parent, plugin) = scaffolded();
    let run = test_plugin(&plugin);
    assert_eq!(run.code, 0, "{}{}", run.stdout, run.stderr);
    assert!(
        run.stdout.starts_with("plugin demo 0.1.0\n"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout
            .contains("caught 1 text.pattern#1 character.welcome\n"),
        "{}",
        run.stdout
    );
    assert!(run.stdout.ends_with("passed\n"), "{}", run.stdout);
}

#[test]
fn a_log_shows_what_each_pattern_caught_and_what_it_missed() {
    let (_parent, plugin) = scaffolded();
    let run = test_log(
        &plugin,
        "[connected]\nIt starts to snow.\nIt starts to hail.\nBo waves.\n",
    );
    assert_eq!(run.code, 0, "{}{}", run.stdout, run.stderr);
    let expected = "caught 2 text.pattern#2 weather\n  \
                    It starts to snow.  weather=\"snow\"\n  \
                    It starts to hail.  weather=\"hail\"\n";
    assert!(run.stdout.contains(expected), "{}", run.stdout);
    assert!(
        run.stdout
            .contains("caught 0 text.pattern#1 character.welcome\n"),
        "{}",
        run.stdout
    );
}

#[test]
fn a_plugin_that_does_not_load_fails() {
    let (_parent, plugin) = scaffolded();
    let manifest = plugin.join("plugin.toml");
    let declared = fs::read_to_string(&manifest).unwrap();
    fs::write(manifest, declared.replace("api = 1", "api = 2")).unwrap();
    let run = test_log(&plugin, "Bo waves.\n");
    assert_fails(&run, "fail load: ");
    assert_fails(&run, "`api = 2` isn't supported");
}

#[test]
fn a_pattern_that_does_not_compile_fails() {
    let (_parent, plugin) = scaffolded();
    fs::write(
        plugin.join("text.toml"),
        "[[pattern]]\nname = \"broken\"\npattern = '''(unclosed'''\n",
    )
    .unwrap();
    let run = test_log(&plugin, "Bo waves.\n");
    assert_fails(&run, "fail load: ");
    assert_fails(&run, "the pattern doesn't compile");
    assert_fails(&run, "unclosed group");
}

#[test]
fn a_hook_no_file_defines_fails() {
    let (_parent, plugin) = scaffolded();
    fs::write(plugin.join("hooks.luau"), "-- on_welcome is gone\n").unwrap();
    assert_fails(&test_log(&plugin, "Bo waves.\n"), "fail load: ");
}

#[test]
fn a_hook_past_its_time_box_is_over_budget() {
    let (_parent, plugin) = scaffolded();
    fs::write(
        plugin.join("hooks.luau"),
        "function on_welcome(event)\n\twhile true do end\nend\n",
    )
    .unwrap();
    let run = test_log(&plugin, "Welcome back, Ana.\n");
    assert_fails(&run, "fail budget: the hook on_welcome overran 1 times");
}

#[test]
fn a_hook_that_raises_an_error_fails() {
    let (_parent, plugin) = scaffolded();
    fs::write(
        plugin.join("hooks.luau"),
        "function on_welcome(event)\n\tmud.store(\"elsewhere\", \"key\", 1)\nend\n",
    )
    .unwrap();
    let run = test_log(&plugin, "Welcome back, Ana.\n");
    assert_fails(&run, "fail hook: on_welcome failed 1 times");
    assert_fails(&run, "write_store:elsewhere");
}

#[test]
fn a_plugin_over_its_line_budget_fails() {
    let (_parent, plugin) = scaffolded();
    let rules: String = (0..100)
        .map(|rule| format!("[[pattern]]\nname = \"slow-{rule}\"\npattern = '''(?<all>.*)'''\n\n"))
        .collect();
    fs::write(plugin.join("text.toml"), rules).unwrap();
    fs::write(
        plugin.join("hooks.luau"),
        "function on_welcome(event) end\n",
    )
    .unwrap();
    let run = test_log(&plugin, "Bo waves to the crowd in the square.\n");
    assert_fails(&run, "fail budget: +");
}

#[test]
fn a_test_fails_when_its_transcript_changes_until_it_is_updated() {
    let (_parent, plugin) = scaffolded();
    let log = plugin.join("tests/sample.txt");
    let sample = fs::read_to_string(&log).unwrap();
    fs::write(&log, sample.replace("rain", "snow")).unwrap();
    assert_fails(
        &test_plugin(&plugin),
        "line 3: wanted event text weather weather=\"rain\", got event text weather weather=\"snow\"",
    );

    let update = mudling(&["plugin", "test", text(&plugin), "--update"]);
    assert_eq!(update.code, 0, "{}", update.stdout);
    let run = test_plugin(&plugin);
    assert_eq!(run.code, 0, "{}", run.stdout);
}

#[test]
fn a_test_that_should_catch_nothing_passes_once_updated() {
    let (_parent, plugin) = scaffolded();
    fs::write(plugin.join("tests/quiet.txt"), "Bo waves.\n").unwrap();
    let update = mudling(&["plugin", "test", text(&plugin), "--update"]);
    assert_eq!(update.code, 0, "{}", update.stdout);
    let expected = fs::read_to_string(plugin.join("tests/quiet.expected")).unwrap();
    assert_eq!(expected, "");
    let run = test_plugin(&plugin);
    assert_eq!(run.code, 0, "{}", run.stdout);
}

#[test]
fn a_plugin_with_no_tests_fails() {
    let (_parent, plugin) = scaffolded();
    fs::remove_dir_all(plugin.join("tests")).unwrap();
    assert_fails(&test_plugin(&plugin), "fail tests: ");
}

#[test]
fn the_avatar_plugin_passes_the_command_registry_ci_runs() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let run = test_plugin(&root.join("plugins/avatar"));
    assert_eq!(run.code, 0, "{}{}", run.stdout, run.stderr);
    assert!(run.stdout.ends_with("passed\n"), "{}", run.stdout);
}

#[test]
fn the_avatar_plugin_reads_its_own_fixture() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let avatar = root.join("plugins/avatar");
    let fixture = avatar.join("tests/fixtures/score.txt");
    let run = mudling(&["plugin", "test", text(&avatar), "--log", text(&fixture)]);
    assert_eq!(run.code, 0, "{}{}", run.stdout, run.stderr);
    // The cookbook quotes these two.
    for caught in [
        "caught 1 character.reader.score.start#1 score.start\n",
        "caught 19 character.reader.score.row#1 score.row\n",
    ] {
        assert!(run.stdout.contains(caught), "{}", run.stdout);
    }
    assert!(
        run.stdout.contains("skipped mobs.room_title#1: "),
        "{}",
        run.stdout
    );
}

#[test]
fn what_cannot_run_exits_2_and_leaves_things_alone() {
    let (parent, plugin) = scaffolded();
    let manifest = fs::read_to_string(plugin.join("plugin.toml")).unwrap();

    let again = mudling(&["plugin", "new", "demo", "--dir", text(parent.path())]);
    assert_eq!(again.code, 2, "{}", again.stdout);
    assert_eq!(
        fs::read_to_string(plugin.join("plugin.toml")).unwrap(),
        manifest
    );

    let bad_id = mudling(&["plugin", "new", "Not/An/Id", "--dir", text(parent.path())]);
    assert_eq!(bad_id.code, 2, "{}", bad_id.stdout);

    let no_log = plugin.join("missing.txt");
    let run = mudling(&["plugin", "test", text(&plugin), "--log", text(&no_log)]);
    assert_eq!(run.code, 2, "{}", run.stdout);
    assert_eq!(test_log(&plugin, "[connected]\n").code, 2);

    assert_eq!(mudling(&["plugin", "remove"]).code, 2);
    assert_eq!(mudling(&[]).code, 2);
}
