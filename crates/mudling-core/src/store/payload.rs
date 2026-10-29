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

    fn coalesce(earlier: &mut TerminalChange, later: TerminalChange) {
        let (TerminalChange::Append(earlier), TerminalChange::Append(later)) = (earlier, later);
        earlier.extend(later);
    }
}

/// Placeholder state: named values, replaced as a whole on each change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fields(pub BTreeMap<String, String>);

impl<const N: usize> From<[(&str, &str); N]> for Fields {
    fn from(pairs: [(&str, &str); N]) -> Self {
        Fields(pairs.into_iter().map(|(name, value)| (name.to_owned(), value.to_owned())).collect())
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
