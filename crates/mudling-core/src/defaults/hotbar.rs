//! The hotbar: commands the player keeps above the command bar, one list per
//! server, in the order they put them.

use super::commands::CHAIN_SEPARATOR;
use crate::persist::{Meta, Timestamp};

/// The `meta` key a server's chips are kept under, as a JSON list.
pub(crate) const HOTBAR_META: &str = "hotbar.chips";

/// Adds `typed` as a chip, each line a chained command. Returns whether it
/// went on: not when blank, or when it's there already.
pub fn pin(chips: &mut Vec<String>, typed: &str) -> bool {
    let separator = CHAIN_SEPARATOR.to_string();
    let lines: Vec<&str> = typed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let command = lines.join(&separator);
    if command.is_empty() || chips.contains(&command) {
        return false;
    }
    chips.push(command);
    true
}

pub(crate) fn from_meta(meta: &[Meta]) -> Vec<String> {
    let saved = meta.iter().find(|meta| meta.key == HOTBAR_META);
    saved
        .and_then(|meta| serde_json::from_str(&meta.value).ok())
        .unwrap_or_default()
}

pub(crate) fn to_meta(chips: &[String], updated: Timestamp) -> Meta {
    Meta {
        key: HOTBAR_META.to_owned(),
        value: serde_json::Value::from(chips).to_string(),
        updated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinning_trims_chains_lines_and_refuses_what_is_there() {
        let mut chips = Vec::new();
        assert!(pin(&mut chips, "  look "));
        assert!(pin(&mut chips, "get all\nwear all\n"));
        assert!(!pin(&mut chips, "look"));
        assert!(!pin(&mut chips, " \n "));
        assert_eq!(chips, ["look", "get all;wear all"]);
    }

    #[test]
    fn chips_come_back_in_their_order() {
        let chips = ["look".to_owned(), "score".to_owned()];
        assert_eq!(from_meta(&[to_meta(&chips, Timestamp(1))]), chips);
        assert!(from_meta(&[]).is_empty());
    }
}
