//! What each store holds.
//!
//! A store with a small state takes the whole new state as its change. Named
//! values (vitals, what plugins read about a character or its gear) take only
//! the values that changed, as do the two stores that grow large: the
//! terminal and the map.

use super::StoreState;
use crate::defaults::aliases::AliasRule;
use crate::defaults::detect::Suggestion;
use crate::defaults::hud::Vital;
use crate::defaults::triggers::TriggerRule;
use crate::gear::sets::{self, GearNow, GearRules, WearPlan};
use crate::gear::{GearBook, ItemMatcher};
use crate::line::Line;
use crate::map::{
    Derived, JoinFacts, LastJoin, MapChange, MapRoom, Mark, MarkChange, MobNamer, RoomId, WalkStep,
    Whereabouts,
};
use crate::net::DisconnectReason;
use crate::persist::{GearSet, Timestamp};
use crate::plugin::PluginCost;
use crate::plugin::luau::{PluginDisabled, Value};
use crate::repo::AppSettings;
use crate::scrollback::Scrollback;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

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
    derived: Option<Arc<Derived>>,
    joins: Arc<JoinFacts>,
    last_join: Option<LastJoin>,
}

impl MapState {
    pub fn rooms(&self) -> impl ExactSizeIterator<Item = &Arc<MapRoom>> {
        self.rooms.values()
    }

    pub fn room(&self, id: RoomId) -> Option<&Arc<MapRoom>> {
        self.rooms.get(&id)
    }

    /// The room a walk starts from, when the map is sure of one: moves from a
    /// guess would go astray.
    pub fn walk_start(&self) -> Option<RoomId> {
        let whereabouts = &self.whereabouts;
        let sure = !whereabouts.unsure && whereabouts.dark_position.is_none();
        whereabouts.room.filter(|_| sure)
    }

    pub fn whereabouts(&self) -> Whereabouts {
        self.whereabouts
    }

    /// The core map's revision.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Areas and buildings, as of the last change to the rooms.
    pub fn derived(&self) -> Option<&Arc<Derived>> {
        self.derived.as_ref()
    }

    /// What views need to tidy up duplicates.
    pub fn joins(&self) -> &Arc<JoinFacts> {
        &self.joins
    }

    /// How the player's last join or undo went.
    pub fn last_join(&self) -> Option<&LastJoin> {
        self.last_join.as_ref()
    }

    /// The fewest moves between two rooms. A search can cross the whole map,
    /// so a view runs it off the UI thread, on a clone of this state.
    pub fn route(&self, start: RoomId, goal: RoomId) -> Option<Vec<WalkStep>> {
        crate::map::route(|id| self.room(id).map(|room| &**room), start, goal)
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
        if let Some(derived) = &change.derived {
            self.derived = Some(Arc::clone(derived));
        }
        if let Some(joins) = &change.joins {
            self.joins = Arc::clone(joins);
        }
        self.last_join = change.last_join.clone().or(self.last_join.take());
        !change.rooms.is_empty()
            || !change.removed.is_empty()
            || change.whereabouts.is_some()
            || change.joins.is_some()
            || change.last_join.is_some()
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
        earlier.derived = later.derived.or(earlier.derived.take());
        earlier.joins = later.joins.or(earlier.joins.take());
        earlier.last_join = later.last_join.or(earlier.last_join.take());
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
    /// key is `<plugin store>.<key>`. The core adds what GMCP says under
    /// [`CHARACTER_NAME`], [`CHARACTER_LEVEL`], [`CHARACTER_POSITION`] and
    /// [`CHARACTER_AFFECTS`].
    CharactersState(String => Value),
    /// What plugins read about gear, keyed like [`CharactersState`].
    GearState(String => Value),
}

/// What's known about items on this server, and what the character has on
/// them now. Whole on each change, which its shared parts keep cheap.
#[derive(Clone, Debug, Default)]
pub struct ItemsState {
    pub book: Arc<GearBook>,
    /// Finds the book's names in a line. Rebuilt only when they change.
    pub matcher: Arc<ItemMatcher>,
    /// Every row of the game's list, empty slots too.
    pub worn: Arc<[WornItem]>,
    pub carried: Arc<[CarriedItem]>,
    /// The bags among `carried`, and what the game last said is in them.
    pub bags: Arc<[CarriedBag]>,
    /// The plugin's wear slots, in the game's order.
    pub slots: Arc<[String]>,
    /// Who `sets` are the sets of: empty until this connection names them.
    pub character: Arc<str>,
    pub sets: Arc<[GearSet]>,
    pub rules: Arc<GearRules>,
}

impl ItemsState {
    pub fn gear_now(&self) -> GearNow<'_> {
        GearNow {
            worn: &self.worn,
            carried: &self.carried,
            bags: &self.bags,
        }
    }

    /// What putting `set` on takes right now, as Wear would send it.
    pub fn plan(&self, set: &GearSet) -> WearPlan {
        sets::plan(set, self.gear_now(), &self.book, &self.rules)
    }
}

impl StoreState for ItemsState {
    type Change = ItemsState;

    /// The core writes one only when something changed.
    fn apply(&mut self, change: &ItemsState) -> bool {
        *self = change.clone();
        true
    }

    fn coalesce(earlier: &mut ItemsState, later: ItemsState) {
        *earlier = later;
    }
}

/// A row of what the character wears.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WornItem {
    pub slot: String,
    /// As the game shows it, tags included; empty for an empty slot.
    pub item: String,
    /// The game's id for the copy, when it was seen going on.
    pub copy: Option<String>,
}

/// One copy the character carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CarriedItem {
    /// The game's id for it.
    pub id: String,
    /// As the game shows it, tags included.
    pub name: String,
}

/// A bag the character carries, and what's in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CarriedBag {
    /// The game's id for it.
    pub id: String,
    /// As the game shows it, tags included.
    pub name: String,
    pub items: Vec<CarriedItem>,
}

/// The character's name, in [`CharactersState`].
pub const CHARACTER_NAME: &str = "character.name";
/// The character's level, as a number.
pub const CHARACTER_LEVEL: &str = "character.level";
/// What the character is doing as the game words it; empty off the game.
pub const CHARACTER_POSITION: &str = "character.position";

/// What a plugin's HUD point says is affecting the character: under this key,
/// `.count`, then `.<n>.name` and `.<n>.detail` for n from 1. Rows past the
/// count are left from a longer list.
pub const CHARACTER_AFFECTS: &str = "character.affects";
/// Where a plugin's hooks keep the affects they read from the game's text, in
/// the same rows: its store of that name.
const READ_AFFECTS: &str = "affects";

/// What the character carries, from GMCP: under this key, `.count`, then
/// `.<n>.id` and `.<n>.name` for n from 1. Rows past the count are stale.
pub const CHARACTER_INVENTORY: &str = "inventory";

/// Something affecting the character, as the game words it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Affect {
    pub name: String,
    /// How long it lasts; empty if the game doesn't say.
    pub detail: String,
}

impl CharactersState {
    /// The change that makes `affects` the list.
    pub fn listing(affects: &[Affect]) -> CharactersState {
        let mut values = BTreeMap::from([(
            format!("{CHARACTER_AFFECTS}.count"),
            Value::Number(affects.len() as f64),
        )]);
        for (index, affect) in affects.iter().enumerate() {
            let row = format!("{CHARACTER_AFFECTS}.{}", index + 1);
            values.insert(format!("{row}.name"), Value::Text(affect.name.clone()));
            values.insert(format!("{row}.detail"), Value::Text(affect.detail.clone()));
        }
        CharactersState(values)
    }

    /// The change that empties both lists. A saved list stays on the disk, but
    /// it says nothing of a character in a game played since.
    pub fn no_affects() -> CharactersState {
        CharactersState(BTreeMap::from(
            [CHARACTER_AFFECTS, READ_AFFECTS]
                .map(|list| (format!("{list}.count"), Value::Number(0.))),
        ))
    }

    /// What's on the character: what was last read from the game's text, with
    /// what GMCP lists. Each comes once, with GMCP's detail if it gives one.
    pub fn affects(&self) -> Vec<Affect> {
        let mut affects: Vec<Affect> = Vec::new();
        for affect in [READ_AFFECTS, CHARACTER_AFFECTS]
            .map(|list| self.listed(list))
            .concat()
        {
            let same = |known: &&mut Affect| known.name.eq_ignore_ascii_case(&affect.name);
            match affects.iter_mut().find(same) {
                Some(known) if !affect.detail.is_empty() => known.detail = affect.detail,
                Some(_) => {}
                None => affects.push(affect),
            }
        }
        affects
    }

    /// The names of what the character carries.
    pub fn carried(&self) -> Vec<String> {
        self.rows(CHARACTER_INVENTORY)
            .filter_map(|row| self.text(format!("{row}.name")))
            .collect()
    }

    fn listed(&self, list: &str) -> Vec<Affect> {
        self.rows(list)
            .filter_map(|row| {
                Some(Affect {
                    name: self.text(format!("{row}.name"))?,
                    detail: self.text(format!("{row}.detail")).unwrap_or_default(),
                })
            })
            .collect()
    }

    fn text(&self, key: String) -> Option<String> {
        match self.0.get(&key) {
            Some(Value::Text(text)) => Some(text.clone()),
            _ => None,
        }
    }

    /// The keys of the rows `list` counts.
    fn rows(&self, list: &str) -> impl Iterator<Item = String> {
        let count = match self.0.get(&format!("{list}.count")) {
            Some(Value::Number(count)) => *count as usize,
            _ => 0,
        };
        let list = list.to_owned();
        (1..=count).map(move |row| format!("{list}.{row}"))
    }
}

/// One skill or spell on the sheet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SkillRow {
    pub name: String,
    /// The level the character can learn it at.
    pub level: Option<i64>,
    /// How well it's learned, 0–100.
    pub learned: Option<i64>,
    /// Above 0 it's a spell.
    pub mana: Option<i64>,
}

impl SkillRow {
    pub fn named(name: &str) -> SkillRow {
        SkillRow {
            name: name.to_owned(),
            ..SkillRow::default()
        }
    }
}

/// A skill that just got better.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Improved {
    pub skill: String,
    pub learned: i64,
    pub at: Timestamp,
}

/// What this connection's character looks like: their saved rows, then what
/// the game says as it plays. Empty until the connection names them, and
/// never another character's.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SheetState {
    /// By id; empty until this connection names them.
    pub character: String,
    /// By the key after `character.`: `str`, `str_perm`, `gold`.
    pub stats: BTreeMap<String, Value>,
    /// By name.
    pub skills: Vec<SkillRow>,
    /// Sessions left to spend.
    pub practices: Option<i64>,
    /// As last read and saved. On a connection, [`CharactersState::affects`]
    /// is the live list.
    pub affects: Vec<Affect>,
    /// The commands behind the stats and affects.
    pub sources: Vec<String>,
    /// When a table last updated the stats or affects.
    pub character_read: Option<Timestamp>,
    /// When a table last updated the skills.
    pub skills_read: Option<Timestamp>,
    pub improved: Option<Improved>,
    /// A quiet read is on its way or waiting for its moment.
    pub reading: bool,
}

/// The player's room, as the map last placed them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RoomState {
    pub room: Option<Arc<MapRoom>>,
    pub whereabouts: Whereabouts,
    pub contents: RoomContents,
}

/// Who and what the server says is in the player's room now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoomContents {
    pub occupants: Vec<Occupant>,
    pub items: Vec<Item>,
}

/// Someone in the room. `name` is the line the game shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Occupant {
    pub name: String,
    /// One word the game knows them by, made only of letters, so it's safe
    /// to put in a command. `None` when their line gives no word to guess:
    /// they're still here, with nothing to aim a command at.
    pub target: Option<String>,
}

/// Something on the ground, `count` of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    /// As [`Occupant::target`].
    pub target: String,
    pub count: u32,
}

/// Who the character is fighting; `None` out of a fight.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TargetState(pub Option<Target>);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Target {
    pub name: String,
    pub level: Option<i64>,
    pub health: Option<i64>,
    /// What `health` is out of, when the game says.
    pub max_health: Option<i64>,
}

/// The mobs the map remembers: kinds that attack, and mobs the player marked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MobsState {
    pub marks: BTreeSet<(Mark, String)>,
    /// How this game's room lines name a mob, as the marks hold them.
    pub namer: MobNamer,
}

impl MobsState {
    /// The marks that cover a mob, by its room line.
    pub fn marks_on(&self, title: &str) -> Vec<Mark> {
        let covered = |(mark, marked): &&(Mark, String)| mark.covers(marked, title, &self.namer);
        self.marks
            .iter()
            .filter(covered)
            .map(|(mark, _)| *mark)
            .collect()
    }

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
        MobsState {
            marks,
            namer: self.namer.clone(),
        }
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
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SettingsState {
    pub connection: ConnectionStatus,
    /// How long the game takes to answer: the median of the last few
    /// commands. It stands while nothing is sent; `None` until the first
    /// answer and off a connection.
    pub latency: Option<Duration>,
    /// The host of this connection, or the last one.
    pub server: Option<String>,
    /// The game plugin detection suggests for this server, and what named
    /// the game. The player turns it on; this alone runs nothing.
    pub suggestion: Option<Suggestion>,
    /// The server turned echo off: what's typed now is a password.
    pub echo_off: bool,
    /// Things gone wrong that the player should see, newest last: a failed
    /// hook, a failed write. A plugin turned off is in `turned_off` instead.
    pub notices: Vec<String>,
    /// Each plugin's sampled cost per line, in the order added. Refreshed
    /// every few seconds while connected.
    pub plugin_costs: Vec<PluginCost>,
    /// The plugins the core turned off, and why, until
    /// [`crate::session::Session::turn_plugin_on`] brings one back.
    pub turned_off: Vec<PluginDisabled>,
    /// Bumps when map history has changed on the disk, so a view lists it
    /// again. Zero until the database is open.
    pub map_versions: u64,
    /// How the last restore of a map version went.
    pub map_restore: Option<MapRestore>,
}

/// How a restore of a map version went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapRestore {
    /// Counts restores, so two that went the same way are two changes.
    pub attempt: u64,
    pub outcome: Result<i64, String>,
}

/// What the player set up for this session: what Settings edits, and the
/// hotbar.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RulesState {
    /// The host this session plays on.
    pub server: String,
    /// The character this session plays, by id.
    pub character: String,
    /// What the game chains commands with, if it does.
    pub separator: Option<String>,
    pub triggers: Vec<TriggerRule>,
    pub aliases: Vec<AliasRule>,
    /// The hotbar's commands, in order.
    pub hotbar: Vec<String>,
    /// The server's notepad, in plain text.
    pub notepad: String,
}

/// Sounds the triggers asked for: a shell plays each change, once.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CuesState {
    pub sounds: Vec<String>,
}

impl StoreState for CuesState {
    type Change = Vec<String>;

    fn apply(&mut self, change: &Vec<String>) -> bool {
        self.sounds.clone_from(change);
        !change.is_empty()
    }

    fn coalesce(earlier: &mut Vec<String>, later: Vec<String>) {
        for sound in later {
            if !earlier.contains(&sound) {
                earlier.push(sound);
            }
        }
    }
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

/// How sync stands, for Data & sync and the sprocket badge.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SyncState {
    /// The repo in use; `None` keeps everything local.
    pub repo: Option<PathBuf>,
    /// A sync is running now.
    pub syncing: bool,
    /// When a sync last finished.
    pub last_sync: Option<Timestamp>,
    /// What the last sync that changed anything changed, in a line.
    pub changed: Option<String>,
    /// Sessions on this device the remote hasn't got yet.
    pub unsynced: usize,
    /// What's stopping sync, in plain words. Never quotes a secret.
    pub problem: Option<String>,
    /// The settings the app should use: a merge may have brought newer ones.
    pub settings: AppSettings,
    /// Where `secrets.vault` lives now, for the app to open it there.
    pub vault_file: PathBuf,
}

whole_value_stores!(
    RoomState,
    MobsState,
    TargetState,
    SettingsState,
    SyncState,
    RulesState,
    SheetState,
);

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
        assert_eq!(held.get(TERMINAL_CAPACITY - 1).unwrap().plain(), "newest");
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
        let first = walk(
            &mut map,
            r#"{"num": 1, "name": "A road", "exits": {"n": 2}}"#,
        );
        map.sent("north", Timestamp(0));
        let second = walk(
            &mut map,
            r#"{"num": 2, "name": "A gate", "exits": {"s": 1}}"#,
        );
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
    fn the_store_carries_derived_data_and_walks_a_route() {
        let mut map = LiveMap::new(Box::new(PlainRules));
        let mut store = MapState::default();
        store.apply(&walk(
            &mut map,
            r#"{"num": 1, "name": "A road", "zone": "Fields", "exits": {"n": 2}}"#,
        ));
        map.sent("north", Timestamp(0));
        store.apply(&walk(
            &mut map,
            r#"{"num": 2, "name": "A gate", "zone": "Fields", "exits": {"s": 1}}"#,
        ));

        let derived = store.derived().expect("rooms changed");
        assert_eq!(derived.area("Fields").map(|area| area.rooms), Some(2));
        let steps = store.route(2, 1).expect("the rooms are linked");
        assert_eq!(steps.len(), 1);
        assert_eq!(store.route(2, 1), map.route_to(1));
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
