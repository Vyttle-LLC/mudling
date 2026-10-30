//! What each store holds.
//!
//! Most of these are placeholders until the tickets that own their models
//! land: the line pipeline replaces [`TerminalLine`], and the map, mobs,
//! characters, gear and settings models replace their [`Fields`]. To replace
//! one, give its state type a real body and its own [`StoreState::Change`];
//! nothing else in the store set depends on what's inside.

use super::StoreState;
use std::collections::{BTreeMap, VecDeque};

/// How many lines the terminal store holds (PRD §6: 100,000 lines).
pub const TERMINAL_CAPACITY: usize = 100_000;

/// A placeholder for the line pipeline's `Line`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalLine {
    pub text: String,
}

/// The chat's lines, oldest first, trimmed to [`TERMINAL_CAPACITY`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TerminalState {
    lines: VecDeque<TerminalLine>,
}

impl TerminalState {
    pub fn lines(&self) -> &VecDeque<TerminalLine> {
        &self.lines
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TerminalChange {
    Append(Vec<TerminalLine>),
}

impl StoreState for TerminalState {
    type Change = TerminalChange;

    fn apply(&mut self, change: &TerminalChange) -> bool {
        let TerminalChange::Append(lines) = change;
        let overflow = (self.lines.len() + lines.len()).saturating_sub(TERMINAL_CAPACITY);
        self.lines.drain(..overflow.min(self.lines.len()));
        let skipped = lines.len().saturating_sub(TERMINAL_CAPACITY);
        self.lines.extend(lines[skipped..].iter().cloned());
        !lines.is_empty()
    }

    /// Keeps at most [`TERMINAL_CAPACITY`] lines, so a shell that stops
    /// draining (a minimized window) doesn't grow the pending diff forever.
    /// The dropped lines are ones the terminal would trim anyway.
    fn coalesce(earlier: &mut TerminalChange, later: TerminalChange) {
        let (TerminalChange::Append(earlier), TerminalChange::Append(later)) = (earlier, later);
        earlier.extend(later);
        let overflow = earlier.len().saturating_sub(TERMINAL_CAPACITY);
        earlier.drain(..overflow);
    }
}

/// Placeholder state: named values, replaced as a whole on each change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fields(pub BTreeMap<String, String>);

impl<const N: usize> From<[(&str, &str); N]> for Fields {
    fn from(pairs: [(&str, &str); N]) -> Self {
        Fields(
            pairs
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect(),
        )
    }
}

/// Declares stores whose change is the whole new state, so a write that
/// equals the current state is no change.
macro_rules! whole_value_stores {
    ($($(#[$doc:meta])* $state:ident),+ $(,)?) => {
        $(
            $(#[$doc])*
            #[derive(Clone, Debug, Default, PartialEq, Eq)]
            pub struct $state(pub Fields);

            impl StoreState for $state {
                type Change = $state;

                fn apply(&mut self, change: &$state) -> bool {
                    if self == change {
                        return false;
                    }
                    self.clone_from(change);
                    true
                }

                fn coalesce(earlier: &mut $state, later: $state) {
                    *earlier = later;
                }
            }
        )+
    };
}

whole_value_stores! {
    /// The HUD's values, from `Char.Vitals`.
    VitalsState,
    /// The current room, from `Room.Info`.
    RoomState,
    /// The automap.
    MapState,
    /// Mobs seen, and which are aggressive.
    MobsState,
    /// The player's characters.
    CharactersState,
    /// Gear and gear sets.
    GearState,
    /// Settings for the server and the app.
    SettingsState,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines named "0", "1", … up to `count - 1`.
    fn numbered(count: usize) -> TerminalChange {
        TerminalChange::Append(
            (0..count)
                .map(|n| TerminalLine {
                    text: n.to_string(),
                })
                .collect(),
        )
    }

    fn append(text: &str) -> TerminalChange {
        TerminalChange::Append(vec![TerminalLine {
            text: text.to_owned(),
        }])
    }

    #[test]
    fn the_terminal_store_keeps_its_newest_lines() {
        let mut terminal = TerminalState::default();
        terminal.apply(&numbered(TERMINAL_CAPACITY + 3));
        terminal.apply(&append("newest"));

        let held = terminal.lines();
        assert_eq!(held.len(), TERMINAL_CAPACITY);
        assert_eq!(held.front().unwrap().text, "4");
        assert_eq!(held.back().unwrap().text, "newest");
    }

    #[test]
    fn a_coalesced_append_keeps_at_most_the_capacity() {
        let mut pending = numbered(TERMINAL_CAPACITY);
        TerminalState::coalesce(&mut pending, append("newest"));

        let TerminalChange::Append(lines) = &pending;
        assert_eq!(lines.len(), TERMINAL_CAPACITY);
        assert_eq!(lines.first().unwrap().text, "1");
        assert_eq!(lines.last().unwrap().text, "newest");

        let mut in_order = TerminalState::default();
        in_order.apply(&numbered(TERMINAL_CAPACITY));
        in_order.apply(&append("newest"));
        let mut coalesced = TerminalState::default();
        coalesced.apply(&pending);
        assert_eq!(coalesced, in_order);
    }
}
