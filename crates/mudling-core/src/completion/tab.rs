//! Tab completion: the word before the cursor is completed from the words of
//! recent commands first (newest first), then from the known words
//! (alphabetically). Tab again swaps in the next match, wrapping around.

use super::{History, KnownWords};
use std::collections::HashSet;

/// The command box after a Tab. `cursor` is a byte offset into `line`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    pub line: String,
    pub cursor: usize,
}

#[derive(Debug)]
struct Cycle {
    /// Where the completed word starts in the line.
    start: usize,
    candidates: Vec<String>,
    shown: usize,
    result: Completion,
}

#[derive(Debug, Default)]
pub struct Completer {
    cycle: Option<Cycle>,
}

impl Completer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Tab pressed with the cursor at byte offset `cursor` in `line`.
    /// Returns the new box, or `None` when there is nothing to complete.
    ///
    /// A Tab on exactly what the previous Tab returned continues that cycle;
    /// anything else (the player typed or moved) starts a new one. This is
    /// told from the text itself, so the box needs no change notices.
    pub fn complete(
        &mut self,
        line: &str,
        cursor: usize,
        history: &History,
        known: &KnownWords,
    ) -> Option<Completion> {
        let continues = self
            .cycle
            .as_ref()
            .is_some_and(|cycle| cycle.result.line == line && cycle.result.cursor == cursor);
        if !continues {
            self.cycle = start_cycle(line, cursor, history, known);
        }
        let cycle = self.cycle.as_mut()?;
        if continues {
            cycle.shown = (cycle.shown + 1) % cycle.candidates.len();
        }
        let word = &cycle.candidates[cycle.shown];
        let rest = &line[cursor..];
        let result = Completion { line: format!("{}{word}{rest}", &line[..cycle.start]), cursor: cycle.start + word.len() };
        cycle.result = result.clone();
        Some(result)
    }
}

fn start_cycle(line: &str, cursor: usize, history: &History, known: &KnownWords) -> Option<Cycle> {
    let before = line.get(..cursor)?;
    let start = word_start(before);
    let prefix = &before[start..];
    if prefix.is_empty() {
        return None;
    }
    let prefix = prefix.to_lowercase();
    let mut seen = HashSet::from([prefix.clone()]);
    let mut candidates = Vec::new();
    let from_history = history
        .entries()
        .iter()
        .rev()
        .flat_map(|command| words_of(command))
        .filter(|word| starts_with_ignoring_case(word, &prefix));
    for word in from_history.chain(known.matching(&prefix)) {
        if seen.insert(word.to_lowercase()) {
            candidates.push(word.to_owned());
        }
    }
    if candidates.is_empty() {
        return None;
    }
    Some(Cycle { start, candidates, shown: 0, result: Completion { line: String::new(), cursor: 0 } })
}

/// Whether `word` starts with `lowercase_prefix` once lowercased, without
/// lowercasing all of `word`: a Tab checks every word of 500 commands.
fn starts_with_ignoring_case(word: &str, lowercase_prefix: &str) -> bool {
    let mut lowered = word.chars().flat_map(char::to_lowercase);
    lowercase_prefix.chars().all(|expected| lowered.next() == Some(expected))
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '-' | '_' | '\'')
}

/// Where the word ending `before` starts. A word never starts with a quote,
/// so `cast 'bless` completes `bless`.
fn word_start(before: &str) -> usize {
    let mut start = before.len();
    for (index, c) in before.char_indices().rev() {
        if !is_word_char(c) {
            break;
        }
        start = index;
    }
    let word = &before[start..];
    start + word.len() - word.trim_start_matches('\'').len()
}

/// The words of a command: runs of letters, digits, `-`, `_` and inner
/// apostrophes.
pub fn words_of(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c| !is_word_char(c))
        .map(|word| word.trim_matches('\''))
        .filter(|word| !word.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(commands: &[&str]) -> History {
        let mut history = History::new();
        for command in commands {
            history.record(command, false);
        }
        history
    }

    fn tab(completer: &mut Completer, line: &str, history: &History, known: &KnownWords) -> Option<String> {
        completer.complete(line, line.len(), history, known).map(|c| c.line)
    }

    #[test]
    fn history_words_come_before_known_words() {
        let history = history(&["kill goblin", "look gate", "kill guard"]);
        let mut known = KnownWords::new();
        known.add_words("mob", ["gargoyle", "Guardian"]);
        let mut completer = Completer::new();
        let mut line = String::from("kill g");
        let mut seen = Vec::new();
        for _ in 0..6 {
            line = tab(&mut completer, &line, &history, &known).unwrap();
            seen.push(line.clone());
        }
        assert_eq!(
            seen,
            ["kill guard", "kill gate", "kill goblin", "kill gargoyle", "kill Guardian", "kill guard"]
        );
    }

    #[test]
    fn tab_cycles_in_place_and_wraps() {
        let history = history(&["look east", "look eagle"]);
        let known = KnownWords::new();
        let mut completer = Completer::new();
        assert_eq!(tab(&mut completer, "look e", &history, &known).as_deref(), Some("look eagle"));
        assert_eq!(tab(&mut completer, "look eagle", &history, &known).as_deref(), Some("look east"));
        assert_eq!(tab(&mut completer, "look east", &history, &known).as_deref(), Some("look eagle"));
    }

    #[test]
    fn typing_between_tabs_starts_a_new_cycle() {
        let history = history(&["look east", "look eagle", "look wall"]);
        let known = KnownWords::new();
        let mut completer = Completer::new();
        tab(&mut completer, "look e", &history, &known);
        assert_eq!(tab(&mut completer, "look w", &history, &known).as_deref(), Some("look wall"));
    }

    #[test]
    fn only_the_word_before_the_cursor_changes() {
        let history = history(&["get sword"]);
        let known = KnownWords::new();
        let mut completer = Completer::new();
        let done = completer.complete("get sw from bag", 6, &history, &known).unwrap();
        assert_eq!(done, Completion { line: "get sword from bag".into(), cursor: 9 });
        // The next Tab continues from the new cursor.
        assert_eq!(
            completer.complete(&done.line, done.cursor, &history, &known).unwrap().line,
            "get sword from bag"
        );
    }

    #[test]
    fn matching_ignores_case_and_a_leading_quote() {
        let history = History::new();
        let mut known = KnownWords::new();
        known.add_words("item", ["Bless"]);
        let mut completer = Completer::new();
        assert_eq!(tab(&mut completer, "cast 'BL", &history, &known).as_deref(), Some("cast 'Bless"));
    }

    #[test]
    fn nothing_to_complete() {
        let history = history(&["look"]);
        let known = KnownWords::new();
        let mut completer = Completer::new();
        assert_eq!(tab(&mut completer, "", &history, &known), None);
        assert_eq!(tab(&mut completer, "look ", &history, &known), None);
        assert_eq!(tab(&mut completer, "look", &history, &known), None);
        assert_eq!(tab(&mut completer, "xyz", &history, &known), None);
        assert_eq!(completer.complete("look", 99, &history, &known), None);
    }

    #[test]
    fn words_of_splits_on_punctuation() {
        assert_eq!(words_of("cast 'bless' guard's-axe, now").collect::<Vec<_>>(), ["cast", "bless", "guard's-axe", "now"]);
    }
}
