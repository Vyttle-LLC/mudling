//! Words the game has shown (room, mob and item names), offered by Tab.
//!
//! Each word belongs to one or more named sources, so a room's words can go
//! when the player leaves it without dropping a word a mob there shares.
//! Words match case-insensitively and keep the case they were added with.

use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Debug)]
struct Entry {
    word: String,
    sources: usize,
}

#[derive(Debug, Default)]
pub struct KnownWords {
    /// Lowercased words held by each source.
    by_source: HashMap<String, HashSet<String>>,
    /// Every known word by its lowercase form, sorted so a prefix is a range.
    index: BTreeMap<String, Entry>,
}

impl KnownWords {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_words<S: AsRef<str>>(&mut self, source: &str, words: impl IntoIterator<Item = S>) {
        let held = self.by_source.entry(source.to_owned()).or_default();
        for word in words {
            let word = word.as_ref();
            if word.is_empty() {
                continue;
            }
            let key = word.to_lowercase();
            if held.insert(key.clone()) {
                self.index.entry(key).or_insert_with(|| Entry { word: word.to_owned(), sources: 0 }).sources += 1;
            }
        }
    }

    pub fn remove_words<S: AsRef<str>>(&mut self, source: &str, words: impl IntoIterator<Item = S>) {
        let Some(held) = self.by_source.get_mut(source) else { return };
        for word in words {
            let key = word.as_ref().to_lowercase();
            if held.remove(&key)
                && let Some(entry) = self.index.get_mut(&key)
            {
                entry.sources -= 1;
                if entry.sources == 0 {
                    self.index.remove(&key);
                }
            }
        }
    }

    /// Known words that start with `prefix` (ignoring case) and are longer
    /// than it, alphabetically.
    pub fn matching<'a>(&'a self, prefix: &str) -> impl Iterator<Item = &'a str> + use<'a> {
        let prefix = prefix.to_lowercase();
        let prefix_len = prefix.len();
        self.index
            .range(prefix.clone()..)
            .take_while(move |(key, _)| key.starts_with(&prefix))
            .filter(move |(key, _)| key.len() > prefix_len)
            .map(|(_, entry)| entry.word.as_str())
    }

    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matching(words: &KnownWords, prefix: &str) -> Vec<String> {
        words.matching(prefix).map(str::to_owned).collect()
    }

    #[test]
    fn matches_ignore_case_and_keep_the_added_case() {
        let mut words = KnownWords::new();
        words.add_words("room", ["Temple", "temperature"]);
        assert_eq!(matching(&words, "TEM"), ["temperature", "Temple"]);
        assert_eq!(matching(&words, "temple"), Vec::<String>::new());
    }

    #[test]
    fn a_word_stays_while_any_source_holds_it() {
        let mut words = KnownWords::new();
        words.add_words("room", ["guard", "gate"]);
        words.add_words("mob", ["Guard"]);
        words.remove_words("room", ["guard", "gate"]);
        assert_eq!(matching(&words, "gu"), ["guard"]);
        assert_eq!(words.len(), 1);
        words.remove_words("mob", ["GUARD"]);
        assert!(words.is_empty());
    }

    #[test]
    fn adding_twice_or_removing_what_isnt_there_changes_nothing() {
        let mut words = KnownWords::new();
        words.add_words("room", ["gate", "gate"]);
        words.remove_words("room", ["door"]);
        words.remove_words("nobody", ["gate"]);
        words.remove_words("room", ["gate"]);
        assert!(words.is_empty());
    }
}
