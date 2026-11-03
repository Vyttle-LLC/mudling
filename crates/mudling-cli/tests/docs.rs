//! Keeps the plugin docs honest: the reference says what the code does, and
//! the cookbook quotes what the Avatar plugin really says.
//!
//! The keys come from the loader, not from reading its source. Every table
//! refuses unknown keys, and says which it knows, so each `<!-- keys: … -->`
//! table in the reference is compared with that answer. What a hook sees comes
//! from a hook.

use mudling_core::plugin::luau::{
    DEFAULT_BUDGET, DEFAULT_COMMANDS_PER_SECOND, DEFAULT_MEMORY_LIMIT, DEFAULT_QUEUE,
    DEFAULT_TIME_LIMIT, HookLimits, HookOutput, HookRunner, OVERRUNS_TO_TURN_OFF, StateView, Value,
};
use mudling_core::plugin::manifest::API_VERSION;
use mudling_core::plugin::{Plugin, PluginError, PluginEvent, PluginHost};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

const UNKNOWN: &str = "not_a_key";
const MANIFEST: &str = "id = \"probe\"\nname = \"Probe\"\nversion = \"1.0.0\"\napi = 1\n\
                        license = \"MIT\"\nlayer = \"game\"\n";
const EVERY_KEY_TABLE: [&str; 26] = [
    "plugin.toml root",
    "plugin.toml [servers]",
    "plugin.toml [points]",
    "plugin.toml [hooks]",
    "connection.toml root",
    "connection.toml [[login]]",
    "commands.toml root",
    "commands.toml [speedwalk]",
    "commands.toml [[alias]]",
    "commands.toml [keywords]",
    "text.toml root",
    "text.toml [pager]",
    "text.toml [[prompt]]",
    "text.toml [[pattern]]",
    "text.toml [[color]]",
    "text.toml [[link]]",
    "hud.toml root",
    "map.toml root",
    "map.toml [area_info]",
    "map.toml [[maze]]",
    "character.toml root",
    "character.toml [[command]]",
    "character.toml [[reader]]",
    "gear.toml root",
    "mobs.toml root",
    "ui.toml root",
];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// With `\n` line endings, so a checkout with CRLF endings reads the same.
fn read(path: &str) -> String {
    fs::read_to_string(repo().join(path))
        .unwrap_or_else(|error| panic!("{path}: {error}"))
        .replace("\r\n", "\n")
}

fn reference() -> String {
    read("docs/plugins/reference.md")
}

/// `manifest`, and `point` declared in a file holding `text`.
fn load(manifest: &str, point: Option<(&str, &str)>) -> Result<Plugin, PluginError> {
    Plugin::from_files(|file| match (file, point) {
        ("plugin.toml", None) => Ok(manifest.to_owned()),
        ("plugin.toml", Some((point, _))) => Ok(format!(
            "{manifest}[servers]\nhosts = [\"probe.example\"]\n[points]\n{point} = \"point.toml\"\n"
        )),
        (_, Some((_, text))) => Ok(text.to_owned()),
        _ => Err(io::ErrorKind::NotFound.into()),
    })
}

/// The words in backticks that a refused value's error lists as expected.
fn expected_in(error: &PluginError, refused: &str) -> BTreeSet<String> {
    let message = error.to_string();
    let (_, list) = message
        .rsplit_once(&format!("`{refused}`, "))
        .unwrap_or_else(|| panic!("no list of what's expected in: {message}"));
    list.split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect()
}

/// The keys the loader knows in `section` of `file`.
fn keys_of(file: &str, section: &str) -> BTreeSet<String> {
    let header = if section == "root" {
        String::new()
    } else {
        format!("{section}\n")
    };
    let probe = format!("{header}{UNKNOWN} = 1\n");
    let error = match file {
        "plugin.toml" => load(&format!("{MANIFEST}{probe}"), None),
        _ => load(MANIFEST, Some((file.trim_end_matches(".toml"), &probe))),
    };
    expected_in(&error.expect_err("an unknown key is refused"), UNKNOWN)
}

/// Each `<!-- keys: <table> -->` and the first-column names of its table.
fn documented_tables(reference: &str) -> Vec<(String, BTreeSet<String>)> {
    let mut tables = Vec::new();
    let mut lines = reference.lines();
    while let Some(line) = lines.next() {
        let Some(name) = line
            .strip_prefix("<!-- keys: ")
            .and_then(|name| name.strip_suffix(" -->"))
        else {
            continue;
        };
        let mut keys = BTreeSet::new();
        for row in lines
            .by_ref()
            .take_while(|row| row.starts_with('|'))
            .skip(2)
        {
            let first_cell = row.split('|').nth(1).expect("a row has cells");
            keys.extend(first_cell.split('`').skip(1).step_by(2).map(str::to_owned));
        }
        tables.push((name.to_owned(), keys));
    }
    tables
}

#[test]
fn every_table_of_keys_lists_exactly_what_the_loader_takes() {
    let reference = reference();
    let tables = documented_tables(&reference);
    let names: Vec<&str> = tables.iter().map(|(name, _)| name.as_str()).collect();
    for table in EVERY_KEY_TABLE {
        assert!(
            names.contains(&table),
            "the reference has no table for {table}"
        );
    }
    for (name, keys) in &tables {
        if let Some((file, section)) = name.split_once(' ')
            && file != "hook"
        {
            assert_eq!(keys, &keys_of(file, section), "{name}");
        }
    }
}

#[test]
fn every_value_with_a_fixed_set_of_names_is_listed() {
    let reference = reference();
    let refused = "not_a_value";
    let probes = [
        load(
            &MANIFEST.replace("\"game\"", &format!("\"{refused}\"")),
            None,
        ),
        load(&format!("{MANIFEST}encoding = \"{refused}\"\n"), None),
        load(
            MANIFEST,
            Some(("map", &format!("room_identity = \"{refused}\"\n"))),
        ),
        load(
            MANIFEST,
            Some((
                "connection",
                &format!("[[login]]\npattern = 'x'\nsend = \"{refused}\"\n"),
            )),
        ),
    ];
    for probe in probes {
        let error = probe.expect_err("an unknown value is refused");
        for value in expected_in(&error, refused) {
            assert!(
                reference.contains(&format!("`\"{value}\"`")),
                "the reference doesn't list \"{value}\""
            );
        }
    }
}

/// What this file's one hook reports of the sandbox it runs in.
const WHAT_A_HOOK_SEES: &str = r#"
function look(event)
    for key in event do mud.store("seen", "event " .. key, true) end
    for key in mud do mud.store("seen", "mud " .. key, true) end
    for key, value in _G do
        if type(value) == "table" and key ~= "mud" and key ~= "_G" then
            mud.store("seen", "library " .. key, true)
        end
    end
    for _, key in { "io", "os", "debug", "require", "loadstring", "print" } do
        if _G[key] ~= nil then mud.store("seen", "library " .. key, true) end
    end
end
"#;

/// Everything `kind`, `event`, `mud` or `library`, that the hook saw.
fn seen_by_a_hook() -> HashMap<String, BTreeSet<String>> {
    let plugin = Plugin::from_files(|file| {
        Ok(match file {
            "plugin.toml" => format!(
                "{MANIFEST}permissions = [\"write_store:seen\"]\n[servers]\n\
                 hosts = [\"probe.example\"]\n[points]\ntext = \"text.toml\"\n\
                 [hooks]\nfiles = [\"look.luau\"]\n"
            ),
            "text.toml" => {
                "[[pattern]]\nname = \"any\"\npattern = 'x'\nhook = \"look\"\n".to_owned()
            }
            _ => WHAT_A_HOOK_SEES.to_owned(),
        })
    })
    .unwrap_or_else(|error| panic!("{error}"));
    let state: Arc<dyn StateView> = Arc::new(HashMap::<String, Value>::new());
    let mut hooks = HookRunner::new(HookLimits::default(), state);
    hooks.add(&plugin).unwrap_or_else(|error| panic!("{error}"));
    let mut host = PluginHost::new([plugin]).unwrap_or_else(|error| panic!("{error}"));

    let mut lines = Vec::new();
    let mut pipeline = mudling_core::line::LinePipeline::default();
    pipeline.push_line(b"x", &mut lines);
    let mut events: Vec<PluginEvent> = Vec::new();
    host.read_line(&lines[0], &mut events);
    let mut outputs = Vec::new();
    hooks.run(&events[0], &mut outputs);

    let mut seen: HashMap<String, BTreeSet<String>> = HashMap::new();
    for output in outputs {
        let HookOutput::StoreWrite(write) = output else {
            panic!("the hook failed: {output:?}");
        };
        let (kind, key) = write.key.split_once(' ').expect("a kind and a key");
        seen.entry(kind.to_owned())
            .or_default()
            .insert(key.to_owned());
    }
    seen
}

#[test]
fn the_hook_api_is_what_a_hook_sees() {
    let reference = reference();
    let tables: HashMap<_, _> = documented_tables(&reference).into_iter().collect();
    let seen = seen_by_a_hook();
    assert_eq!(tables["hook event"], seen["event"]);

    let calls = tables["hook mud"]
        .iter()
        .filter_map(|call| call.strip_prefix("mud."))
        .map(|call| call.split('(').next().expect("a name").to_owned());
    assert_eq!(calls.collect::<BTreeSet<_>>(), seen["mud"]);

    let libraries = ["string", "table", "math", "utf8", "bit32"];
    assert_eq!(seen["library"], libraries.map(str::to_owned).into());
    for library in libraries {
        assert!(reference.contains(&format!("`{library}`")), "{library}");
    }
}

#[test]
fn the_limits_are_the_code_s() {
    let reference = reference();
    let rows = [
        format!("| Interrupt ticks per hook call | {DEFAULT_BUDGET} |"),
        format!(
            "| CPU time per hook call | {} ms |",
            DEFAULT_TIME_LIMIT.as_millis()
        ),
        format!(
            "| Memory per plugin | {} MB |",
            DEFAULT_MEMORY_LIMIT / (1024 * 1024)
        ),
        format!("| Events waiting for the worker | {DEFAULT_QUEUE} |"),
        format!("| Commands per second, per plugin | {DEFAULT_COMMANDS_PER_SECOND} |"),
        format!(
            "| Overruns in a session before the plugin is turned off | {OVERRUNS_TO_TURN_OFF} |"
        ),
        format!("| `api` | yes | The API version: `{API_VERSION}`. |"),
    ];
    for row in rows {
        assert!(reference.contains(&row), "the reference doesn't say: {row}");
    }
}

#[test]
fn the_commands_are_as_the_reference_says() {
    let output = Command::new(env!("CARGO_BIN_EXE_mudling"))
        .output()
        .expect("the binary runs");
    let usage = String::from_utf8_lossy(&output.stderr);
    let reference = reference().replace("\\|", "|");
    for command in usage
        .lines()
        .filter_map(|line| line.trim().strip_prefix("mudling plugin"))
    {
        assert!(
            reference.contains(&format!("| `mudling plugin{command}` |")),
            "the reference doesn't list: {command}"
        );
    }
}

/// Every block with a file after its language, as `` ```lua path ``.
fn quoted_blocks(cookbook: &str) -> Vec<(String, String)> {
    let mut blocks = Vec::new();
    let mut lines = cookbook.lines();
    while let Some(line) = lines.next() {
        let info = line.strip_prefix("```").unwrap_or_default();
        if let Some((_, file)) = info.split_once(' ') {
            let body: Vec<&str> = lines.by_ref().take_while(|line| *line != "```").collect();
            blocks.push((file.to_owned(), body.join("\n")));
        } else if line.starts_with("```") {
            lines.by_ref().take_while(|line| *line != "```").count();
        }
    }
    blocks
}

#[test]
fn the_cookbook_quotes_the_avatar_plugin_as_it_is() {
    let blocks = quoted_blocks(&read("docs/plugins/cookbook.md"));
    assert!(blocks.len() > 10, "found only {}", blocks.len());
    for (file, body) in blocks {
        assert!(file.starts_with("plugins/avatar/"), "{file}");
        assert!(
            read(&file).contains(&body),
            "{file} no longer holds:\n{body}"
        );
    }
}
