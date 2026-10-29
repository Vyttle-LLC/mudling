//! The core knows no game (PRD §11): fails if any file in mudling-core holds
//! wording from the prototype's Avatar support. Game text belongs in
//! `plugins/`. This file is the one exception.

use std::path::{Path, PathBuf};

/// Taken from the prototype's Avatar wording; compared ignoring case.
const DENYLIST: &[&str] = &[
    "avatar",
    "outland",
    "Spells and skills learnable by your class",
    "skills are at your disposal",
    "practice sessions left",
    "Available to practice",
    "You are affected by:",
    "You are not affected by any",
    "You are not under the affects of any",
    "Exp/TNL",
    // The tail of the prompt.
    "(-/-)",
    "<worn on finger>",
    "<worn about body>",
    "Welcome back to the",
];

#[test]
fn mudling_core_holds_no_game_strings() {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let this_file = crate_dir.join(file!().trim_start_matches("crates/mudling-core/"));
    let mut files = Vec::new();
    collect(&crate_dir, &mut files);
    assert!(files.len() > 5, "found only {files:?}");

    let mut found = Vec::new();
    for file in files.iter().filter(|file| **file != this_file) {
        let Ok(text) = std::fs::read_to_string(file) else { continue };
        let text = text.to_lowercase();
        for word in DENYLIST.iter().filter(|word| text.contains(&word.to_lowercase())) {
            found.push(format!("{}: {word:?}", file.strip_prefix(&crate_dir).unwrap().display()));
        }
    }
    assert!(found.is_empty(), "game strings in mudling-core:\n{}", found.join("\n"));
}

fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name != "target") {
                collect(&path, files);
            }
        } else {
            files.push(path);
        }
    }
}
