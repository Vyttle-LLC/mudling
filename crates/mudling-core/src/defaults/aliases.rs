//! Aliases: a short word typed for longer commands. With `k` for
//! `kill $1;kick $1`, typing `k rat` sends `kill rat` and `kick rat`.
//!
//! The player's aliases and the `[[alias]]` entries of the Commands point run
//! the same way; the player's win a name, then the highest layer's.

use super::commands::{CHAIN_SEPARATOR, split_chain};
use super::triggers::Scope;
use crate::persist::{self, Timestamp};

/// Marks a switched-off alias's scope: the `aliases` table has no column for
/// it, and an alias that's off applies to no one.
const OFF_PREFIX: &str = "off:";
/// How the preview joins the commands an alias stands for.
const PREVIEW_JOIN: &str = "; ";

/// One alias, as the player edits it.
#[derive(Clone, Debug, PartialEq)]
pub struct AliasRule {
    /// Empty until the session first saves it.
    pub id: String,
    pub scope: Scope,
    /// The first word typed, in any case.
    pub name: String,
    /// Commands separated by `;` or new lines. `$1`, `$2`, ... are the words
    /// typed after the name and `$*` all of them; with none of these, the
    /// words typed go on the end.
    pub expansion: String,
    pub enabled: bool,
    /// The server whose database keeps it.
    pub home: String,
}

impl AliasRule {
    pub fn new(name: impl Into<String>, expansion: impl Into<String>) -> AliasRule {
        AliasRule {
            id: String::new(),
            scope: Scope::default(),
            name: name.into(),
            expansion: expansion.into(),
            enabled: true,
            home: String::new(),
        }
    }

    /// Why it can't be used, if it can't.
    pub fn problem(&self) -> Option<&'static str> {
        if self.name.is_empty() || self.name.contains(char::is_whitespace) {
            return Some("The name is one word, like k.");
        }
        if self.expansion.trim().is_empty() {
            return Some("Type the commands it stands for.");
        }
        None
    }

    pub(crate) fn from_row(row: persist::Alias, home: &str) -> AliasRule {
        let off = row.scope.strip_prefix(OFF_PREFIX);
        AliasRule {
            scope: Scope::from_row(off.unwrap_or(&row.scope)),
            enabled: off.is_none(),
            home: home.to_owned(),
            name: row.name,
            expansion: row.expansion,
            id: row.id,
        }
    }

    pub(crate) fn to_row(&self, updated: Timestamp) -> persist::Alias {
        let scope = self.scope.to_row();
        persist::Alias {
            id: self.id.clone(),
            scope: if self.enabled {
                scope
            } else {
                format!("{OFF_PREFIX}{scope}")
            },
            name: self.name.clone(),
            expansion: self.expansion.clone(),
            updated,
        }
    }
}

/// The commands `expansion` stands for, with `words` in their places.
fn expand(expansion: &str, words: &[&str]) -> Vec<String> {
    let mut text = String::with_capacity(expansion.len());
    let mut placed = false;
    let mut chars = expansion.chars().peekable();
    while let Some(c) = chars.next() {
        let next = chars.peek().copied();
        match next.filter(|_| c == '$') {
            Some('*') => text.push_str(&words.join(" ")),
            Some(digit @ '1'..='9') => {
                let number = digit as usize - '1' as usize;
                text.push_str(words.get(number).unwrap_or(&""));
            }
            _ => {
                text.push(c);
                continue;
            }
        }
        placed = true;
        chars.next();
    }
    if !placed && !words.is_empty() {
        text.push(' ');
        text.push_str(&words.join(" "));
    }
    text.split([CHAIN_SEPARATOR, '\n'])
        .map(|command| command.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|command| !command.is_empty())
        .collect()
}

/// The aliases a session runs, matched against the first word of a command.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AliasSet {
    /// Lowercase name and expansion. The first of a name wins.
    aliases: Vec<(String, String)>,
}

impl AliasSet {
    /// `aliases` come first to last in the order they win.
    pub fn new<'a>(aliases: impl IntoIterator<Item = (&'a str, &'a str)>) -> AliasSet {
        AliasSet {
            aliases: aliases
                .into_iter()
                .map(|(name, expansion)| (name.to_lowercase(), expansion.to_owned()))
                .collect(),
        }
    }

    /// The commands one command stands for, or `None` when it starts with no
    /// alias.
    pub fn commands(&self, command: &str) -> Option<Vec<String>> {
        let mut words = command.split_whitespace();
        let first = words.next()?.to_lowercase();
        let (_, expansion) = self.aliases.iter().find(|(name, _)| *name == first)?;
        Some(expand(expansion, &words.collect::<Vec<_>>()))
    }

    /// What a typed chain sends, or `None` when no part of it is an alias.
    pub fn expand_chain(&self, typed: &str) -> Option<Vec<String>> {
        let commands = split_chain(typed);
        commands
            .iter()
            .any(|command| self.commands(command).is_some())
            .then(|| {
                commands
                    .iter()
                    .flat_map(|command| {
                        self.commands(command)
                            .unwrap_or_else(|| vec![(*command).to_owned()])
                    })
                    .collect()
            })
    }

    /// What the command bar shows beside a typed chain.
    pub fn preview(&self, typed: &str) -> Option<String> {
        let commands = self.expand_chain(typed)?;
        Some(commands.join(PREVIEW_JOIN))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aliases() -> AliasSet {
        AliasSet::new([
            ("k", "kill $1;kick $1"),
            ("gg", "get $2 $1\nwear $2"),
            ("say2", "say $*"),
            ("c", "cast 'cure light'"),
        ])
    }

    #[test]
    fn arguments_fill_their_places() {
        let aliases = aliases();
        assert_eq!(aliases.commands("k rat").unwrap(), ["kill rat", "kick rat"]);
        assert_eq!(
            aliases.commands("GG satchel ring").unwrap(),
            ["get ring satchel", "wear ring"]
        );
        assert_eq!(
            aliases.commands("say2 hello there").unwrap(),
            ["say hello there"]
        );
    }

    #[test]
    fn without_places_arguments_go_on_the_end() {
        assert_eq!(
            aliases().commands("c grimm").unwrap(),
            ["cast 'cure light' grimm"]
        );
    }

    #[test]
    fn missing_arguments_leave_no_gaps() {
        assert_eq!(
            aliases().commands("gg satchel").unwrap(),
            ["get satchel", "wear"]
        );
    }

    #[test]
    fn only_the_first_word_counts() {
        assert_eq!(aliases().commands("kick k"), None);
        assert_eq!(aliases().commands(""), None);
    }

    #[test]
    fn a_chain_expands_each_part_and_keeps_the_rest() {
        let aliases = aliases();
        assert_eq!(
            aliases.expand_chain("k rat;look").unwrap(),
            ["kill rat", "kick rat", "look"]
        );
        assert_eq!(aliases.expand_chain("look;n"), None);
        assert_eq!(
            aliases.preview("k rat").as_deref(),
            Some("kill rat; kick rat")
        );
    }

    #[test]
    fn the_first_of_a_name_wins() {
        let aliases = AliasSet::new([("k", "kick"), ("K", "kill")]);
        assert_eq!(aliases.commands("k").unwrap(), ["kick"]);
    }

    #[test]
    fn off_or_unfinished_aliases_have_a_row_and_a_problem() {
        let off = AliasRule {
            enabled: false,
            home: "mud.example".to_owned(),
            ..AliasRule::new("k", "kill $1")
        };
        let row = off.to_row(Timestamp(1));
        assert_eq!(row.scope, "off:server");
        assert_eq!(AliasRule::from_row(row, "mud.example"), off);
        assert!(AliasRule::new("two words", "x").problem().is_some());
        assert!(AliasRule::new("k", " ").problem().is_some());
        assert!(off.problem().is_none());
    }
}
