//! The plugin itself: it loads, its patterns compile, and the declared
//! points say what the README says.

use mudling_core::line::LinePipeline;
use mudling_core::plugin::luau::{HookLimits, HookOutput, LuauHooks, StateView, Value};
use mudling_core::plugin::points::TableReader;
use mudling_core::plugin::{Layer, PluginHost, ServerInfo};
use mudling_plugin_avatar::load;
use std::collections::HashMap;
use std::sync::{Arc, mpsc};

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
        HookOutput::StoreWrite(write) if write.store == "equipment" && write.key == "count" => {
            Some(write.value)
        }
        _ => None,
    });
    assert_eq!(count, Some(Value::Number(1.0)));
}

#[test]
fn each_background_reader_is_the_text_patterns_of_its_name() {
    let plugin = load();
    let text = plugin.points.text.as_ref().unwrap();
    let character = plugin.points.character.as_ref().unwrap();
    let pattern = |name: &str| {
        let rule = text.patterns.iter().find(|rule| rule.name.as_str() == name);
        rule.unwrap_or_else(|| panic!("text.toml has no `{name}`"))
            .clone()
    };
    for TableReader {
        name,
        start,
        row,
        end,
        hook,
    } in &character.readers
    {
        let events = if name == "equipment" {
            "gear.equipment".to_owned()
        } else {
            format!("character.{name}")
        };
        let (start_rule, row_rule) = (
            pattern(&format!("{events}.start")),
            pattern(&format!("{events}.row")),
        );
        assert_eq!(start.as_str(), start_rule.pattern.as_str(), "{name}: start");
        assert_eq!(row.as_str(), row_rule.pattern.as_str(), "{name}: row");
        assert_eq!(hook.as_ref(), start_rule.hook.as_ref(), "{name}: hook");
        assert!(end.is_none(), "{name}: the prompt ends it");
    }
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
