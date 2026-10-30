//! Turns any server, zone or character name into a file or folder name
//! that's safe on Windows, macOS and Linux.
//!
//! The name is a pure function of the text, so two devices agree on it. Of
//! all the ways to write a name in upper and lower case, exactly one is used
//! bare (`Alda`, `The Meadow`, `mud.example.org`). Every other name takes a
//! hash of its exact text (`Hill of Stones-0f3a…`), so names that a
//! case-insensitive file system would mix up never meet. Only ASCII reaches
//! the disk, because some file systems rewrite other text. The files keep
//! the real names inside, and import reads those.

use std::collections::HashSet;

/// Longest name before the hash, well inside every file system's limit.
const MAX_STEM: usize = 48;

/// What the repo's root keeps for itself.
const ROOT_ENTRIES: [&str; 5] = [
    "mudling.json",
    "plugins",
    "secrets.vault",
    "servers.json",
    "settings.json",
];

/// Names Windows keeps for devices, with any extension.
const WINDOWS_DEVICES: [&str; 26] = [
    "con", "prn", "aux", "nul", "com0", "com1", "com2", "com3", "com4", "com5", "com6", "com7",
    "com8", "com9", "lpt0", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    "conin", "conout",
];

/// The names handed out in one folder. Two names in a folder never match,
/// even ignoring case: the rare clash (two characters sharing a name) takes
/// a new hash, so callers must claim names in a fixed order.
#[derive(Default)]
pub(super) struct Names {
    taken: HashSet<String>,
}

impl Names {
    /// A folder in the repo's root, for a server's id.
    pub(super) fn folder(&mut self, id: &str) -> String {
        self.claim(id, is_bare_folder)
    }

    /// A file name without its extension, for a zone or a character.
    pub(super) fn stem(&mut self, name: &str) -> String {
        self.claim(name, is_bare_stem)
    }

    fn claim(&mut self, name: &str, is_bare: fn(&str) -> bool) -> String {
        let mut chosen = if is_bare(name) {
            name.to_owned()
        } else {
            hashed(name)
        };
        let mut seed = name.to_owned();
        while !self.taken.insert(chosen.to_ascii_lowercase()) {
            seed.push('\0');
            chosen = hashed(&seed);
        }
        chosen
    }
}

/// Lower case only, as host names are, so `plugins` and the like are the
/// only ones to rule out.
fn is_bare_folder(id: &str) -> bool {
    let allowed = |c: char| {
        c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, ' ' | '-' | '_' | '.')
    };
    is_safe(id) && id.chars().all(allowed) && !ROOT_ENTRIES.contains(&id)
}

/// Capitalized words, the one spelling of a name that goes without a hash.
fn is_bare_stem(name: &str) -> bool {
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_');
    is_safe(name) && name.chars().all(allowed) && is_capitalized(name)
}

fn is_safe(name: &str) -> bool {
    let device = name.split('.').next().unwrap_or(name).trim_end();
    !name.is_empty()
        && name.len() <= MAX_STEM
        && !name.starts_with([' ', '.'])
        && !name.ends_with([' ', '.'])
        && !WINDOWS_DEVICES.contains(&device.to_ascii_lowercase().as_str())
}

/// Whether each word starts in upper case and goes on in lower case. Only
/// one spelling of any text passes.
fn is_capitalized(name: &str) -> bool {
    let mut word_start = true;
    name.chars().all(|c| {
        let fits = if word_start {
            !c.is_ascii_lowercase()
        } else {
            !c.is_ascii_uppercase()
        };
        word_start = !c.is_ascii_alphanumeric();
        fits
    })
}

/// A readable start, then a hash of the exact text.
fn hashed(name: &str) -> String {
    let readable: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, ' ' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(MAX_STEM)
        .collect();
    let readable = readable.trim_matches([' ', '_', '-']);
    let hash = fnv1a(name.as_bytes());
    if readable.is_empty() {
        format!("{hash:016x}")
    } else {
        format!("{readable}-{hash:016x}")
    }
}

/// FNV-1a: the same on every platform and Rust version, which the standard
/// library's hashers don't promise.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stem(name: &str) -> String {
        Names::default().stem(name)
    }

    fn folder(id: &str) -> String {
        Names::default().folder(id)
    }

    fn assert_safe_everywhere(name: &str) {
        assert!(name.len() <= 80, "{name}");
        assert!(name.is_ascii(), "{name}");
        assert!(
            name.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.')),
            "{name}"
        );
        assert!(!name.starts_with([' ', '.']), "{name}");
        assert!(!name.ends_with([' ', '.']), "{name}");
        let device = name.split('.').next().unwrap().to_ascii_lowercase();
        assert!(!WINDOWS_DEVICES.contains(&device.as_str()), "{name}");
    }

    #[test]
    fn plain_names_stay_readable() {
        assert_eq!(stem("Alda"), "Alda");
        assert_eq!(stem("The Meadow"), "The Meadow");
        assert_eq!(stem("Mid-Gard 2"), "Mid-Gard 2");
        assert_eq!(folder("mud.example.org"), "mud.example.org");
    }

    #[test]
    fn hostile_names_become_safe() {
        for name in [
            "",
            " ",
            ".",
            "..",
            "...",
            ".git",
            "../../etc/passwd",
            "..\\windows",
            "a/b",
            "a\\b",
            "a:b",
            "what?",
            "star*",
            "<tag>",
            "pipe|",
            "quote\"",
            "tab\there",
            "new\nline",
            "nul\0byte",
            "trailing.",
            "trailing ",
            " leading",
            "CON",
            "Con",
            "con",
            "con.txt",
            "Con.Txt",
            "NUL",
            "Com1",
            "LPT9.tar.gz",
            "Conin$",
            "Søren",
            "Ωmega",
            "龍の谷",
            "😀",
            &"very long name ".repeat(40),
        ] {
            assert_safe_everywhere(&stem(name));
            assert_safe_everywhere(&folder(name));
        }
    }

    #[test]
    fn names_the_root_keeps_are_refused() {
        for name in ROOT_ENTRIES.into_iter().chain([".git", ".gitignore"]) {
            let chosen = folder(name);
            assert!(chosen.len() > name.len(), "{chosen}");
        }
        assert!(folder("Plugins").len() > "plugins".len());
    }

    #[test]
    fn spellings_that_differ_only_in_case_get_different_names() {
        let mut names = Names::default();
        let mut seen = HashSet::new();
        for name in [
            "Brightwater",
            "brightwater",
            "BRIGHTWATER",
            "BrightWater",
            "Søren",
            "søren",
            "SØREN",
        ] {
            assert!(seen.insert(names.stem(name).to_lowercase()), "{name}");
        }
    }

    #[test]
    fn a_name_needs_no_context() {
        let mut names = Names::default();
        names.stem("brightwater");
        assert_eq!(names.stem("Brightwater"), stem("Brightwater"));
    }

    #[test]
    fn long_names_with_one_start_stay_apart() {
        let long = "x".repeat(300);
        assert_ne!(stem(&format!("{long}a")), stem(&format!("{long}b")));
    }

    #[test]
    fn the_same_name_claimed_twice_gets_a_second_name() {
        let mut names = Names::default();
        let first = names.stem("Alda");
        let second = names.stem("Alda");
        assert_eq!(first, "Alda");
        assert_ne!(second.to_lowercase(), first.to_lowercase());
        assert_safe_everywhere(&second);
    }

    #[test]
    fn the_hash_never_changes() {
        // A changed hash would rename every file in every player's repo.
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
