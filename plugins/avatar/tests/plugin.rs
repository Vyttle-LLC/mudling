//! The plugin itself: it loads, its patterns compile, and the declared
//! points say what the README says.

use mudling_core::git::FolderFiles;
use mudling_core::line::LinePipeline;
use mudling_core::plugin::check;
use mudling_core::plugin::luau::{HookLimits, HookOutput, LuauHooks, StateView, Value};
use mudling_core::plugin::{Layer, PluginHost, ServerInfo};
use mudling_plugin_avatar::{load, plugin_dir};
use std::collections::HashMap;
use std::fs;
use std::sync::{Arc, mpsc};

/// What `mudling plugin test` runs: it replays its own test log to what's
/// expected, and its hooks don't fail. Its cost a line is the CLI's timing
/// test, `timing_the_avatar_plugin_passes_the_command_registry_ci_runs`.
#[test]
fn passes_its_own_tests() {
    let mut files = FolderFiles::new();
    for folder in [plugin_dir(), plugin_dir().join("tests")] {
        for entry in fs::read_dir(&folder).unwrap().flatten() {
            let path = entry.path();
            if path.is_file() {
                let name = path.strip_prefix(plugin_dir()).unwrap();
                let name = name.to_str().unwrap().replace('\\', "/");
                files.insert(name, fs::read(&path).unwrap());
            }
        }
    }
    check::own_tests_except_cost(&load(), &files).unwrap_or_else(|failure| panic!("{failure}"));
}

#[test]
fn loads_with_every_pattern_compiled_and_every_hook_defined() {
    let plugin = load();
    assert_eq!(plugin.id().as_str(), "avatar");
    assert_eq!(plugin.layer(), Layer::Game);
    assert!(plugin.manifest.matches_server(ServerInfo {
        host: "avatar.outland.org",
        mssp_name: None
    }));
    assert!(plugin.manifest.matches_server(ServerInfo {
        host: "other.example",
        mssp_name: Some("Avatar")
    }));
    assert!(!plugin.manifest.matches_server(ServerInfo {
        host: "other.example",
        mssp_name: None
    }));
    // A host compiles every pattern together; a session loads every hook.
    PluginHost::new([plugin]).unwrap_or_else(|error| panic!("{error}"));
    PluginHost::new([mudling_plugin_avatar::bundled()]).unwrap_or_else(|error| panic!("{error}"));
    mudling_plugin_avatar::Session::new();
}

#[test]
fn the_clients_hook_worker_loads_the_hooks_and_runs_them() {
    let plugin = load();
    let state: Arc<dyn StateView> = Arc::new(HashMap::<String, Value>::new());
    let (outputs, received) = mpsc::channel();
    let mut hooks = LuauHooks::start([&plugin], HookLimits::default(), state, outputs).unwrap();
    assert!(
        hooks.failed_to_load().is_empty(),
        "{:?}",
        hooks.failed_to_load()
    );

    let mut host = PluginHost::new([plugin]).unwrap();
    let mut lines = Vec::new();
    LinePipeline::default().push(
        b"You are using:\n<worn on head>      a Tiamat mask\n(1h-1m-1v-1)(-/-)\n",
        &mut lines,
    );
    for line in &lines {
        host.read_line(line, &mut hooks);
    }
    hooks.wait_until_idle();
    let count = received.try_iter().find_map(|output| match output {
        HookOutput::StoreWrite(write) if write.store == "worn" && write.key == "count" => {
            Some(write.value)
        }
        _ => None,
    });
    assert_eq!(count, Some(Value::Number(1.0)));
}

#[test]
fn every_background_command_has_a_table_the_prompt_ends() {
    let plugin = load();
    let character = plugin.points.character.as_ref().unwrap();
    assert!(character.readers.iter().all(|reader| reader.end.is_none()));
    let commands: Vec<(&str, &str)> = character
        .commands
        .iter()
        .map(|command| (command.command.as_str(), command.reader.as_str()))
        .collect();
    assert_eq!(
        commands,
        [
            ("score", "score"),
            ("practice", "practice"),
            ("slist", "slist"),
            ("aff", "affects"),
            ("equipment", "equipment")
        ]
    );
}
