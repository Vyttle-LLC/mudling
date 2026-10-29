//! Commands typed, oldest first, browsed the way zsh's up/down arrows do
//! (history-beginning-search): Up shows the next older command that starts
//! with what's left of the cursor, Down the next newer one, and Down past the
//! newest brings back what the player was typing. Typing anything starts over.

/// How many commands are kept.
pub const HISTORY_LIMIT: usize = 500;

/// While browsing: the entry shown, what it must start with, and what was
/// typed before browsing began.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Browse {
    index: usize,
    prefix: String,
    draft: String,
}

#[derive(Clone, Debug, Default)]
pub struct History {
    entries: Vec<String>,
    browse: Option<Browse>,
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    /// A history holding `entries` (oldest first), kept to the last
    /// [`HISTORY_LIMIT`].
    pub fn from_entries(mut entries: Vec<String>) -> Self {
        let excess = entries.len().saturating_sub(HISTORY_LIMIT);
        entries.drain(..excess);
        History { entries, browse: None }
    }

    /// The commands, oldest first.
    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    /// A command was sent. It becomes the newest entry (once, if it just
    /// was), and browsing starts over. Input typed while the server has echo
    /// off is a password: it is dropped, never stored. Returns whether the
    /// entries changed.
    pub fn record(&mut self, command: &str, echo_off: bool) -> bool {
        self.browse = None;
        let command = command.trim();
        if echo_off || command.is_empty() || self.entries.last().is_some_and(|last| last == command) {
            return false;
        }
        if self.entries.len() == HISTORY_LIMIT {
            self.entries.remove(0);
        }
        self.entries.push(command.to_owned());
        true
    }

    /// Up arrow. `line` is the command box's text and `prefix` the part
    /// before the cursor, which the first press fixes for the rest of the
    /// browse. Returns the command to show, or `None` when there's no older
    /// match.
    pub fn older(&mut self, line: &str, prefix: &str) -> Option<String> {
        if !self.is_showing_a_match(line) {
            self.browse = None;
        }
        let (start, prefix, draft) = match &self.browse {
            Some(browse) => (browse.index, browse.prefix.clone(), browse.draft.clone()),
            None => (self.entries.len(), prefix.to_owned(), line.to_owned()),
        };
        // Skip what's already showing: right after sending, the sent command
        // is still in the box.
        let index = (0..start).rev().find(|&i| self.entries[i].starts_with(&prefix) && self.entries[i] != line)?;
        self.browse = Some(Browse { index, prefix, draft });
        Some(self.entries[index].clone())
    }

    /// Down arrow: the next newer match, or what the player was typing once
    /// past the newest. `None` when not browsing.
    pub fn newer(&mut self, line: &str) -> Option<String> {
        if !self.is_showing_a_match(line) {
            self.browse = None;
            return None;
        }
        let browse = self.browse.take()?;
        let found = (browse.index + 1..self.entries.len())
            .find(|&i| self.entries[i].starts_with(&browse.prefix) && self.entries[i] != line);
        match found {
            Some(index) => {
                let shown = self.entries[index].clone();
                self.browse = Some(Browse { index, ..browse });
                Some(shown)
            }
            None => Some(browse.draft),
        }
    }

    /// Whether a browse is under way and `line` is a history entry starting
    /// with its prefix. Told from the text itself, since a box's change
    /// notices can't tell a command shown from history from one typed.
    fn is_showing_a_match(&self, line: &str) -> bool {
        self.browse
            .as_ref()
            .is_some_and(|browse| line.starts_with(&browse.prefix) && self.entries.iter().any(|entry| entry == line))
    }
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

    #[test]
    fn echo_off_input_is_dropped() {
        let mut history = history(&["look"]);
        assert!(!history.record("hunter2", true));
        assert_eq!(history.entries(), ["look"]);
    }

    #[test]
    fn empty_and_repeated_commands_are_not_stored() {
        let history = history(&["n", "n", "  ", "e", " e ", "n"]);
        assert_eq!(history.entries(), ["n", "e", "n"]);
    }

    #[test]
    fn only_the_last_500_are_kept() {
        let mut history = History::new();
        for i in 0..HISTORY_LIMIT + 25 {
            history.record(&format!("cmd {i}"), false);
        }
        assert_eq!(history.entries().len(), HISTORY_LIMIT);
        assert_eq!(history.entries()[0], "cmd 25");
        assert_eq!(history.entries()[HISTORY_LIMIT - 1], format!("cmd {}", HISTORY_LIMIT + 24));
    }

    #[test]
    fn loaded_entries_are_capped_too() {
        let entries = (0..HISTORY_LIMIT + 3).map(|i| format!("cmd {i}")).collect();
        let history = History::from_entries(entries);
        assert_eq!(history.entries().len(), HISTORY_LIMIT);
        assert_eq!(history.entries()[0], "cmd 3");
    }

    #[test]
    fn up_with_nothing_typed_walks_back_through_everything() {
        let mut history = history(&["look", "kill rabbit", "cast 'bless'"]);
        assert_eq!(history.older("", "").as_deref(), Some("cast 'bless'"));
        assert_eq!(history.older("cast 'bless'", "").as_deref(), Some("kill rabbit"));
        assert_eq!(history.older("kill rabbit", "").as_deref(), Some("look"));
        assert_eq!(history.older("look", ""), None);
    }

    #[test]
    fn up_finds_only_commands_starting_with_what_was_typed() {
        let mut history = history(&["cast 'bless'", "look", "cast 'armor'", "kill rabbit"]);
        assert_eq!(history.older("cast", "cast").as_deref(), Some("cast 'armor'"));
        assert_eq!(history.older("cast 'armor'", "cast 'armor'").as_deref(), Some("cast 'bless'"));
        assert_eq!(history.older("cast 'bless'", ""), None);
    }

    #[test]
    fn browsing_keeps_the_original_prefix_while_the_box_holds_a_match() {
        let mut history = history(&["look", "cast 'bless'", "cast 'armor'"]);
        assert_eq!(history.older("ca", "ca").as_deref(), Some("cast 'armor'"));
        // The cursor is now at the end of the shown command; the prefix is
        // still "ca", so "look" is not offered.
        assert_eq!(history.older("cast 'armor'", "cast 'armor'").as_deref(), Some("cast 'bless'"));
        assert_eq!(history.older("cast 'bless'", "cast 'bless'"), None);
    }

    #[test]
    fn down_goes_back_to_what_was_being_typed() {
        let mut history = history(&["cast 'bless'", "cast 'armor'"]);
        assert_eq!(history.older("cas", "cas").as_deref(), Some("cast 'armor'"));
        assert_eq!(history.older("cast 'armor'", "").as_deref(), Some("cast 'bless'"));
        assert_eq!(history.newer("cast 'bless'").as_deref(), Some("cast 'armor'"));
        assert_eq!(history.newer("cast 'armor'").as_deref(), Some("cas"));
        assert_eq!(history.newer("cas"), None);
    }

    #[test]
    fn the_sent_command_still_in_the_box_is_skipped() {
        let mut history = history(&["look", "kill rabbit"]);
        assert_eq!(history.older("kill rabbit", "").as_deref(), Some("look"));
    }

    #[test]
    fn typing_starts_the_search_over() {
        let mut history = history(&["cast 'bless'", "kill rabbit"]);
        history.older("", "");
        assert_eq!(history.older("cast", "cast").as_deref(), Some("cast 'bless'"));
        assert_eq!(history.newer("cast 'bless' x"), None);
    }

    #[test]
    fn recording_ends_the_browse() {
        let mut history = history(&["look", "kill rabbit"]);
        history.older("", "");
        history.record("north", false);
        assert_eq!(history.newer("kill rabbit"), None);
    }
}
