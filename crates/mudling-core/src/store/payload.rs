//! What each store holds.
//!
//! A store with a small state takes the whole new state as its change. Named
//! values (vitals, what plugins read about a character or its gear) take only
//! the values that changed, as do the two stores that grow large: the
//! terminal and the map.

use super::StoreState;
use crate::defaults::detect::Suggestion;
use crate::defaults::hud::Vital;
use crate::line::Line;
use crate::map::{MapChange, MapRoom, Mark, MarkChange, RoomId, Whereabouts};
use crate::net::DisconnectReason;
use crate::plugin::luau::Value;
use crate::scrollback::Scrollback;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// How many lines the terminal store holds (PRD §6: 100,000 lines).
pub const TERMINAL_CAPACITY: usize = 100_000;

/// The chat's lines, oldest first, trimmed to [`TERMINAL_CAPACITY`].
#[derive(Clone, Debug)]
pub struct TerminalState {
    lines: Scrollback,
}

impl TerminalState {
    pub fn lines(&self) -> &Scrollback {
        &self.lines
    }
}

impl Default for TerminalState {
    fn default() -> Self {
        TerminalState {
            lines: Scrollback::with_capacity(TERMINAL_CAPACITY),
        }
    }
}

/// Two terminals are equal when they hold the same lines. How many lines
/// each trimmed differs when a coalesced diff dropped some.
impl PartialEq for TerminalState {
    fn eq(&self, other: &Self) -> bool {
        self.lines.iter().eq(other.lines.iter())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TerminalChange {
    Append(Vec<Line>),
}

impl StoreState for TerminalState {
    type Change = TerminalChange;

    fn apply(&mut self, change: &TerminalChange) -> bool {
        let TerminalChange::Append(lines) = change;
        let skipped = lines.len().saturating_sub(TERMINAL_CAPACITY);
        for line in &lines[skipped..] {
            self.lines.push(line.clone());
        }
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

/// The automap, as views draw it. Rooms are shared with the core's map, and
/// a clone of this state costs next to nothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MapState {
    rooms: imbl::OrdMap<RoomId, Arc<MapRoom>>,
    whereabouts: Whereabouts,
    revision: u64,
}

impl MapState {
    pub fn rooms(&self) -> impl ExactSizeIterator<Item = &Arc<MapRoom>> {
        self.rooms.values()
    }

    pub fn room(&self, id: RoomId) -> Option<&Arc<MapRoom>> {
        self.rooms.get(&id)
    }

    pub fn whereabouts(&self) -> Whereabouts {
        self.whereabouts
    }

    /// The core map's revision: what [`crate::map::Derived`] is computed at.
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

impl StoreState for MapState {
    type Change = MapChange;

    fn apply(&mut self, change: &MapChange) -> bool {
        for room in &change.rooms {
            self.rooms.insert(room.id, Arc::clone(room));
        }
        for id in &change.removed {
            self.rooms.remove(id);
        }
        self.whereabouts = change.whereabouts.unwrap_or(self.whereabouts);
        self.revision = change.revision;
        !change.rooms.is_empty() || !change.removed.is_empty() || change.whereabouts.is_some()
    }

    /// The pending diff can't outgrow the map: a room appears in it once.
    fn coalesce(earlier: &mut MapChange, later: MapChange) {
        let written_later = |id: RoomId| {
            later.rooms.iter().any(|room| room.id == id) || later.removed.contains(&id)
        };
        earlier.rooms.retain(|room| !written_later(room.id));
        earlier.removed.retain(|id| !written_later(*id));
        earlier.rooms.extend(later.rooms);
        earlier.removed.extend(later.removed);
        earlier.whereabouts = later.whereabouts.or(earlier.whereabouts);
        earlier.marks.extend(later.marks);
        earlier.revision = later.revision;
    }
}

/// Declares stores of named values, whose change is the values that changed.
macro_rules! named_value_stores {
    ($($(#[$doc:meta])* $state:ident($key:ty => $value:ty)),+ $(,)?) => {
        $(
            $(#[$doc])*
            #[derive(Clone, Debug, Default, PartialEq)]
            pub struct $state(pub BTreeMap<$key, $value>);

            impl StoreState for $state {
                type Change = $state;

                fn apply(&mut self, change: &$state) -> bool {
                    let mut changed = false;
                    for (key, value) in &change.0 {
                        if self.0.get(key) != Some(value) {
                            self.0.insert(key.clone(), value.clone());
                            changed = true;
                        }
                    }
                    changed
                }

                fn coalesce(earlier: &mut $state, later: $state) {
                    earlier.0.extend(later.0);
                }
            }
        )+
    };
}

named_value_stores! {
    /// The HUD's values, from GMCP.
    VitalsState(Vital => i64),
    /// What plugins read about the character: skills, affects, score. Each
    /// key is `<plugin store>.<key>`.
    CharactersState(String => Value),
    /// What plugins read about gear, keyed like [`CharactersState`].
    GearState(String => Value),
}

/// The player's room, as the map last placed them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RoomState {
    pub room: Option<Arc<MapRoom>>,
    pub whereabouts: Whereabouts,
}

/// The mobs the map remembers: kinds that attack, and mobs the player marked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MobsState {
    pub marks: BTreeSet<(Mark, String)>,
}

impl MobsState {
    /// This state with `changes` made.
    pub fn with_changes(&self, changes: &[MarkChange]) -> MobsState {
        let mut marks = self.marks.clone();
        for change in changes {
            let mark = (change.mark, change.name.clone());
            if change.set {
                marks.insert(mark);
            } else {
                marks.remove(&mark);
            }
        }
        MobsState { marks }
    }
}

/// Where a connection stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ConnectionStatus {
    #[default]
    Idle,
    Connecting,
    Connected,
    Closed(DisconnectReason),
}

/// What a shell shows about the session around the game.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsState {
    pub connection: ConnectionStatus,
    /// The host of this connection, or the last one.
    pub server: Option<String>,
    /// The game plugin on for this server, and what named the game.
    pub game: Option<Suggestion>,
    /// The server turned echo off: what's typed now is a password.
    pub echo_off: bool,
    /// Things gone wrong that the player should see, newest last: a plugin
    /// turned off, a failed write.
    pub notices: Vec<String>,
}

/// Declares stores whose change is the whole new state, so a write that
/// equals the current state is no change.
macro_rules! whole_value_stores {
    ($($state:ident),+ $(,)?) => {
        $(impl StoreState for $state {
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
        })+
    };
}

whole_value_stores!(RoomState, MobsState, SettingsState);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::LiveMap;
    use crate::map::PlainRules;
    use crate::persist::Timestamp;

    /// Lines named "0", "1", … up to `count - 1`.
    fn numbered(count: usize) -> TerminalChange {
        TerminalChange::Append((0..count).map(|n| Line::command(&n.to_string())).collect())
    }

    fn append(text: &str) -> TerminalChange {
        TerminalChange::Append(vec![Line::command(text)])
    }

    #[test]
    fn the_terminal_store_keeps_its_newest_lines() {
        let mut terminal = TerminalState::default();
        terminal.apply(&numbered(TERMINAL_CAPACITY + 3));
        terminal.apply(&append("newest"));

        let held = terminal.lines();
        assert_eq!(held.len(), TERMINAL_CAPACITY);
        assert_eq!(held.get(0).unwrap().plain(), "4");
        assert_eq!(
            held.get(TERMINAL_CAPACITY - 1).unwrap().plain(),
            "newest"
        );
    }

    #[test]
    fn a_coalesced_append_keeps_at_most_the_capacity() {
        let mut pending = numbered(TERMINAL_CAPACITY);
        TerminalState::coalesce(&mut pending, append("newest"));

        let TerminalChange::Append(lines) = &pending;
        assert_eq!(lines.len(), TERMINAL_CAPACITY);
        assert_eq!(lines.first().unwrap().plain(), "1");
        assert_eq!(lines.last().unwrap().plain(), "newest");

        let mut in_order = TerminalState::default();
        in_order.apply(&numbered(TERMINAL_CAPACITY));
        in_order.apply(&append("newest"));
        let mut coalesced = TerminalState::default();
        coalesced.apply(&pending);
        assert_eq!(coalesced, in_order);
    }

    fn walk(map: &mut LiveMap, json: &str) -> MapChange {
        map.room_info(json, Timestamp(0));
        map.commit(Timestamp(0)).expect("the map changed").change
    }

    #[test]
    fn a_coalesced_map_change_equals_the_changes_in_order() {
        let mut map = LiveMap::new(Box::new(PlainRules));
        let first = walk(&mut map, r#"{"num": 1, "name": "A road", "exits": {"n": 2}}"#);
        map.sent("north", Timestamp(0));
        let second = walk(&mut map, r#"{"num": 2, "name": "A gate", "exits": {"s": 1}}"#);
        map.forget(&[1]);
        let third = map.commit(Timestamp(0)).expect("a room left").change;

        let mut in_order = MapState::default();
        let mut coalesced = first.clone();
        in_order.apply(&first);
        for change in [second, third] {
            in_order.apply(&change);
            MapState::coalesce(&mut coalesced, change);
        }
        let mut applied_once = MapState::default();
        applied_once.apply(&coalesced);
        assert_eq!(applied_once, in_order);
        assert_eq!(in_order.rooms().len(), 1);
    }

    #[test]
    fn named_values_change_only_what_differs() {
        let mut vitals = VitalsState(BTreeMap::from([(Vital::Hp, 9)]));
        let same = VitalsState(BTreeMap::from([(Vital::Hp, 9)]));
        assert!(!vitals.apply(&same));
        let more = VitalsState(BTreeMap::from([(Vital::Mana, 3)]));
        assert!(vitals.apply(&more));
        assert_eq!(vitals.0.len(), 2);
    }
}
