//! One Aho-Corasick matcher over the known item names (PRD §5 #4). The core
//! rebuilds it on a thread of its own when the names change, and a shell only
//! asks it about the lines it shows.

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use std::ops::Range;

/// A shorter name would light up in ordinary words.
pub const MIN_NAME_LEN: usize = 4;

/// An item a line names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemMention<'a> {
    /// Where, by bytes.
    pub range: Range<usize>,
    /// Its plain name.
    pub item: &'a str,
}

/// Finds known item names in a line, whole words only, ignoring ASCII case.
#[derive(Clone, Debug, Default)]
pub struct ItemMatcher {
    automaton: Option<AhoCorasick>,
    names: Vec<String>,
}

impl PartialEq for ItemMatcher {
    fn eq(&self, other: &ItemMatcher) -> bool {
        self.names == other.names
    }
}

impl ItemMatcher {
    /// A matcher over `names`, which are plain names.
    pub fn new<'a>(names: impl IntoIterator<Item = &'a str>) -> ItemMatcher {
        let names: Vec<String> = (names.into_iter())
            .filter(|name| name.len() >= MIN_NAME_LEN)
            .map(str::to_owned)
            .collect();
        let automaton = AhoCorasickBuilder::new()
            .ascii_case_insensitive(true)
            .match_kind(MatchKind::LeftmostLongest)
            .build(&names)
            .ok()
            .filter(|_| !names.is_empty());
        ItemMatcher { automaton, names }
    }

    /// Whether `names` are the names this was built over.
    pub fn is_over<'a>(&self, names: impl IntoIterator<Item = &'a str>) -> bool {
        let wanted = names.into_iter().filter(|name| name.len() >= MIN_NAME_LEN);
        self.names.iter().map(String::as_str).eq(wanted)
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// The items `line` names, in order.
    pub fn find(&self, line: &str) -> Vec<ItemMention<'_>> {
        let Some(automaton) = &self.automaton else {
            return Vec::new();
        };
        (automaton.find_iter(line))
            .filter(|found| is_whole_word(line, found.range()))
            .map(|found| ItemMention {
                range: found.range(),
                item: &self.names[found.pattern().as_usize()],
            })
            .collect()
    }
}

fn is_whole_word(line: &str, range: Range<usize>) -> bool {
    let joins = |c: char| c.is_alphanumeric();
    !line[..range.start].chars().next_back().is_some_and(joins)
        && !line[range.end..].chars().next().is_some_and(joins)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_finds_whole_names_ignoring_case_and_prefers_the_longest() {
        let matcher = ItemMatcher::new(["a tiamat mask", "a tiamat mask of woe", "ring"]);
        let line = "You wear A Tiamat mask of woe. Your earrings shine; the ring too.";
        let found = matcher.find(line);
        assert_eq!(
            found.iter().map(|m| m.item).collect::<Vec<_>>(),
            ["a tiamat mask of woe", "ring"]
        );
        assert_eq!(&line[found[0].range.clone()], "A Tiamat mask of woe");
    }

    #[test]
    fn names_too_short_to_tell_from_words_are_left_out() {
        let matcher = ItemMatcher::new(["bag", "a bag"]);
        assert_eq!(matcher.len(), 1);
        assert!(matcher.find("a bag").len() == 1);
        assert!(matcher.is_over(["bag", "a bag"]));
        assert!(!matcher.is_over(["a bag", "a box"]));
    }

    #[test]
    fn an_empty_matcher_finds_nothing() {
        assert!(ItemMatcher::default().find("a tiamat mask").is_empty());
        assert!(ItemMatcher::new([]).find("a tiamat mask").is_empty());
    }
}
