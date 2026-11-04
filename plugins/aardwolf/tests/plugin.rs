//! The plugin itself: it loads, it's Aardwolf's, it asks only to send GMCP,
//! and it passes the checks a plugin added from a link must.

use mudling_core::git::FolderFiles;
use mudling_core::plugin::check;
use mudling_core::plugin::manifest::Permission;
use mudling_core::plugin::{Layer, PluginHost, ServerInfo};
use mudling_plugin_aardwolf::{load, plugin_dir};
use std::fs;

#[test]
fn loads_as_aardwolfs_game_plugin() {
    let plugin = load();
    assert_eq!(plugin.id().as_str(), "aardwolf");
    assert_eq!(plugin.layer(), Layer::Game);
    let matches =
        |host, mssp_name| (plugin.manifest).matches_server(ServerInfo { host, mssp_name });
    assert!(matches("aardmud.org", None));
    assert!(matches("AardMUD.org", None), "case doesn't matter");
    assert!(matches("other.example", Some("Aardwolf")));
    assert!(!matches("other.example", None));
    PluginHost::new([plugin]).unwrap_or_else(|error| panic!("{error}"));
}

#[test]
fn needs_no_hooks_and_asks_only_to_send_gmcp() {
    let plugin = load();
    assert_eq!(plugin.manifest.permissions, [Permission::SendGmcp]);
    assert!(plugin.manifest.hooks.files.is_empty());
}

/// Most of what `mudling plugin test` and the app run on a plugin added from
/// a link: it loads, its own tests match, and its hooks don't fail. Its cost a
/// line is `timing_keeps_within_its_line_budget`, below.
#[test]
fn passes_its_own_tests() {
    check::own_tests_except_cost(&load(), &own_files())
        .unwrap_or_else(|failure| panic!("{failure}"));
}

/// The same, and it keeps within its cost a line, which depends on how fast
/// the machine is.
#[test]
#[ignore = "timing: runs in the profile"]
fn timing_keeps_within_its_line_budget() {
    check::own_tests(&load(), &own_files()).unwrap_or_else(|failure| panic!("{failure}"));
}

fn own_files() -> FolderFiles {
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
    files
}
