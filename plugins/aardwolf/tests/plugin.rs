//! The plugin itself: it loads, it's Aardwolf's, it asks for nothing, and it
//! passes the checks a plugin added from a link must.

use mudling_core::git::FolderFiles;
use mudling_core::plugin::check;
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
fn needs_no_hooks_and_no_permissions() {
    let plugin = load();
    assert!(plugin.manifest.permissions.is_empty());
    assert!(plugin.manifest.hooks.files.is_empty());
}

/// What `mudling plugin test` runs, and what the app runs on a plugin added
/// from a link: it loads, its own tests match, and it keeps within budget.
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
    check::own_tests(&load(), &files).unwrap_or_else(|failure| panic!("{failure}"));
}
