//! `mudling plugin new`: a small plugin that shows each way of writing one,
//! with a test of its own. The files are in `scaffold/`.

use mudling_core::plugin::PluginId;
use std::fs;
use std::path::{Path, PathBuf};

const MANIFEST: &str = include_str!("scaffold/plugin.toml");
const FILES: [(&str, &str); 4] = [
    ("text.toml", include_str!("scaffold/text.toml")),
    ("hooks.luau", include_str!("scaffold/hooks.luau")),
    ("tests/sample.txt", include_str!("scaffold/sample.txt")),
    (
        "tests/sample.expected",
        include_str!("scaffold/sample.expected"),
    ),
];

/// Makes `<parent>/<id>/`. It must not exist: nothing gets overwritten.
pub fn create(id: &str, parent: &Path) -> Result<PathBuf, String> {
    let id = PluginId::try_from(id.to_owned())?;
    let folder = parent.join(id.as_str());
    let could_not = |error| format!("couldn't create {}: {error}", folder.display());
    fs::create_dir_all(parent).map_err(could_not)?;
    fs::create_dir(&folder).map_err(could_not)?;
    fs::create_dir(folder.join("tests")).map_err(could_not)?;
    let manifest = MANIFEST.replace("{id}", id.as_str());
    fs::write(folder.join("plugin.toml"), manifest).map_err(could_not)?;
    for (file, text) in FILES {
        fs::write(folder.join(file), text).map_err(could_not)?;
    }
    Ok(folder)
}
