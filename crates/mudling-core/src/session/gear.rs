//! Gear as a session sees it: what hooks report and what the game does to
//! the character, filed in the [`GearBook`] and published as
//! [`ItemsState`].
//!
//! Hooks write a table's facts one key at a time, the last key closing it.
//! `gear` keys are one fact each. `identify` and `worn` keys are drafts until
//! their closing key: each `identify` batch is one reading, kept as a reading
//! of its own, however many others have the same words.
//!
//! The rules a plugin declares for wearing and removing say when gear moved,
//! and their `item` capture what moved. What an item does is measured from
//! `Char.Status` around that moment, as hooks can't see it in time.

use super::inventory::Inventory;
use super::values::from_row;
use crate::defaults::hud::Vital;
use crate::gear::{GEAR_STATS, GearBook, ItemMatcher, ItemRef, Reading, plain_name};
use crate::persist::{Batch, GearRows, Key, Meta, StatMap, Timestamp, Worn};
use crate::plugin::luau::Value;
use crate::plugin::{PluginEvent, PluginHost, Point};
use crate::store::{
    CHARACTER_NAME, CarriedItem, CharactersState, GearState, ItemsState, StoreHub, Versioned,
    VitalsState, WornItem,
};
use crate::telnet::gmcp;
use serde_json::Value as Json;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

/// The plugin store whose batches are one reading each, closed by [`READ`].
const IDENTIFY_STORE: &str = "identify";
/// The plugin store of facts about items, by plain name.
const GEAR_STORE: &str = "gear";
const READ: &str = "read";
/// The plugin store that lists what's worn, closed by [`COUNT`].
const WORN_STORE: &str = "worn";
const COUNT: &str = "count";
/// What a `gear` key names, before the item's plain name.
const SLOT_PREFIX: &str = "slot.";
const KEYWORDS_PREFIX: &str = "keywords.";
const WORD_PREFIX: &str = "keyword.";
/// Where the database kept what's in typed rows now.
const OLD_GEAR_META: &str = "plugin.gear.";
const OLD_IDENTIFY_META: &str = "plugin.identify.";
/// The fields an old `identify` key could end in.
const OLD_FIELDS: [&str; 7] = [
    ".name", ".type", ".flags", ".level", ".quality", ".base.", ".added.",
];

/// A wear or remove rule's capture naming the item.
const ITEM_CAPTURE: &str = "item";
const STATUS: &str = "Char.Status";
const VITALS: &str = "Char.Vitals";

fn is(package: &str, name: &str) -> bool {
    package.eq_ignore_ascii_case(name)
}

fn number(value: &Json) -> Option<i64> {
    match value {
        Json::Number(number) => number.as_i64(),
        Json::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn text(value: &Value) -> Option<&str> {
    match value {
        Value::Text(text) => Some(text),
        _ => None,
    }
}

fn whole(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => Some(*number as i64),
        _ => None,
    }
}

/// One moment gear went on or came off: a swap lands in one.
#[derive(Debug)]
struct GearChange {
    worn: Vec<Held>,
    removed: Vec<Held>,
    before: StatMap,
    after: StatMap,
    saw_prompt: bool,
}

#[derive(Clone, Debug)]
struct Held {
    name: String,
    copy: Option<String>,
}

impl GearChange {
    /// What one item does, when it went on or came off alone and the game
    /// reported the stats after it; a swap can't be told apart.
    fn measured(self) -> Option<(Held, StatMap)> {
        if self.after.is_empty() {
            return None;
        }
        let before = &self.before;
        let mut deltas: StatMap = (self.after.iter())
            .filter_map(|(stat, value)| {
                let old = before.get(stat)?;
                (old != value).then(|| (stat.clone(), value - old))
            })
            .collect();
        match (self.worn.len(), self.removed.len()) {
            (1, 0) => self.worn.into_iter().next().map(|held| (held, deltas)),
            (0, 1) => {
                deltas.values_mut().for_each(|delta| *delta = -*delta);
                self.removed.into_iter().next().map(|held| (held, deltas))
            }
            _ => None,
        }
    }
}

/// What a connection has seen of the character's gear.
#[derive(Debug, Default)]
pub(super) struct GearWatch {
    stats: StatMap,
    change: Option<GearChange>,
    /// Copies put on this connection, by plain name, oldest first.
    worn_copies: Vec<(String, String)>,
}

impl GearWatch {
    /// The game's prompt is back, so a new message is a new command.
    pub(super) fn prompt_seen(&mut self) {
        if let Some(change) = &mut self.change {
            change.saw_prompt = true;
        }
    }

    /// Records stats reported since a change began; later reports win.
    /// Returns whether any gear stat was among them.
    fn note_stats(&mut self, values: impl IntoIterator<Item = (String, i64)>) -> bool {
        let mut noted = false;
        for (stat, value) in values {
            if let Some(change) = &mut self.change {
                change.after.insert(stat.clone(), value);
            }
            self.stats.insert(stat, value);
            noted = true;
        }
        noted
    }

    fn copy_worn_as(&self, key: &str, taken: &[usize]) -> Option<usize> {
        (self.worn_copies.iter().enumerate())
            .find(|(at, (worn, _))| worn == key && !taken.contains(at))
            .map(|(at, _)| at)
    }
}

/// What's left for the session to do with a hook's write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Filing {
    /// Nothing: it's part of a reading.
    Draft,
    /// Show it to hooks. It's saved in a typed row.
    Filed,
    /// Show it to hooks and save it as a plain value.
    Plain,
}

/// When something happened, and the batch its rows join.
pub(super) struct Saving<'a> {
    pub(super) now: Timestamp,
    pub(super) batch: &'a mut Batch,
}

/// A gear rule that says gear moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Moved {
    On,
    Off,
}

fn moved(plugins: &PluginHost, event: &PluginEvent) -> Option<Moved> {
    if event.point != Point::Gear {
        return None;
    }
    let named = |rules: &[crate::plugin::points::EventRule]| {
        rules.iter().any(|rule| &*event.name == rule.name.as_str())
    };
    (plugins.declared(|points| points.gear.as_ref())).find_map(|(_, gear)| {
        if named(&gear.wear) {
            Some(Moved::On)
        } else if named(&gear.remove) {
            Some(Moved::Off)
        } else {
            None
        }
    })
}

/// A session's gear, kept on the core thread. A shell gets its own snapshot
/// from [`GearKeeper::publish`].
#[derive(Default)]
pub(super) struct GearKeeper {
    book: GearBook,
    /// `book` as last published.
    shared_book: Arc<GearBook>,
    book_changed: bool,
    matcher: Arc<ItemMatcher>,
    /// The book's names may have moved on from the matcher's.
    names_changed: bool,
    /// Hears when the matcher being rebuilt is done.
    matcher_built: Option<Receiver<ItemMatcher>>,
    worn: Vec<WornItem>,
    carried: Vec<CarriedItem>,
    /// The record's saved rows, for ids a table can't give.
    saved_worn: Vec<Worn>,
    /// What went on, until its hook says which slot it's in.
    going_on: Vec<Held>,
    /// Who `worn` is the list of, in lowercase, once anything has said.
    worn_by: String,
    playing: String,
    reading: BTreeMap<String, Value>,
    worn_rows: BTreeMap<String, Value>,
    /// The plugin set `slots` came from.
    slots_of: u64,
    slots: Arc<[String]>,
    changed: bool,
}

impl GearKeeper {
    /// Takes what the database holds, folding in what an older MUDling kept
    /// under `meta`, whose rows go.
    pub(super) fn load(
        &mut self,
        rows: GearRows,
        worn: Vec<Worn>,
        character: &str,
        meta: &[Meta],
        hub: &StoreHub,
        saving: &mut Saving<'_>,
    ) {
        self.book = GearBook::from_rows(rows);
        self.saved_worn = worn;
        (self.saved_worn).retain(|row| row.character_id == character);
        self.saved_worn.sort_by_key(|row| row.position);
        self.reading.clear();
        self.worn_rows.clear();
        adopt_old_meta(&mut self.book, meta, saving.now, saving.batch);
        let mut flat = BTreeMap::new();
        for (key, item) in self.book.items() {
            if let Some(slot) = &item.facts.slot {
                flat.insert(
                    format!("gear.{SLOT_PREFIX}{key}"),
                    Value::Text(slot.clone()),
                );
            }
            if let Some(words) = &item.facts.keywords {
                flat.insert(
                    format!("gear.{KEYWORDS_PREFIX}{key}"),
                    Value::Text(words.clone()),
                );
            }
        }
        for entry in meta {
            let Some(key) = entry.key.strip_prefix(OLD_GEAR_META) else {
                continue;
            };
            let filed = key.starts_with(SLOT_PREFIX) || key.starts_with(KEYWORDS_PREFIX);
            let Some(value) = from_row(&entry.value).filter(|_| !filed) else {
                continue;
            };
            if let (Some(item), Value::Text(word)) = (key.strip_prefix(WORD_PREFIX), &value) {
                self.book.note_word(word, item);
            }
            flat.insert(format!("gear.{key}"), value);
        }
        hub.write::<GearState>(GearState(flat));
        self.worn_by = hub.read(|characters: &Versioned<CharactersState>| {
            match characters.value().0.get(CHARACTER_NAME) {
                Some(Value::Text(name)) => name.to_lowercase(),
                _ => String::new(),
            }
        });
        self.worn = (self.saved_worn.iter())
            .map(|row| WornItem {
                slot: row.slot.clone(),
                item: row.item.clone(),
                copy: row.copy_id.clone(),
            })
            .collect();
        self.changed = true;
        self.note_book(true);
    }

    fn note_book(&mut self, changed: bool) {
        self.book_changed |= changed;
        self.names_changed |= changed;
    }

    /// `playing` is who's on now. Characters can share a session's record, so
    /// someone else's list isn't theirs: it stays empty until the game lists
    /// what they wear.
    pub(super) fn follow(&mut self, playing: &str) {
        self.playing = playing.to_owned();
        if playing.is_empty() || self.worn_by == playing {
            return;
        }
        if !self.worn_by.is_empty() {
            self.worn.clear();
            for row in &mut self.saved_worn {
                row.item.clear();
                row.copy_id = None;
            }
            self.changed = true;
        }
        self.worn_by = playing.to_owned();
    }

    /// A hook wrote to a store. Says what's left for the session to do.
    pub(super) fn hook_wrote(
        &mut self,
        store: &str,
        key: &str,
        value: &Value,
        character: &str,
        watch: Option<&GearWatch>,
        saving: &mut Saving<'_>,
    ) -> Filing {
        match store {
            IDENTIFY_STORE => {
                if key == READ {
                    self.file_reading(saving.now, saving.batch);
                } else {
                    self.reading.insert(key.to_owned(), value.clone());
                }
                Filing::Draft
            }
            WORN_STORE => {
                if key == COUNT {
                    let count = whole(value).unwrap_or(0);
                    self.file_worn(count, character, watch, saving.now, saving.batch);
                } else {
                    self.worn_rows.insert(key.to_owned(), value.clone());
                }
                Filing::Plain
            }
            GEAR_STORE => {
                self.gear_fact(key, value, saving.now, saving.batch);
                if key.starts_with(SLOT_PREFIX) || key.starts_with(KEYWORDS_PREFIX) {
                    Filing::Filed
                } else {
                    Filing::Plain
                }
            }
            _ => Filing::Plain,
        }
    }

    fn gear_fact(&mut self, key: &str, value: &Value, now: Timestamp, batch: &mut Batch) {
        let Some(value) = text(value) else { return };
        let changed = if let Some(item) = key.strip_prefix(SLOT_PREFIX) {
            let at = (self.going_on.iter()).position(|held| plain_name(&held.name) == item);
            if let Some(held) = at.map(|at| self.going_on.remove(at)) {
                self.put_on(held, value);
            }
            self.book.learn_slot(value, item, now, batch)
        } else if let Some(item) = key.strip_prefix(KEYWORDS_PREFIX) {
            self.book.learn_keywords(value, item, now, batch)
        } else if let Some(item) = key.strip_prefix(WORD_PREFIX) {
            self.book.note_word(value, item)
        } else {
            false
        };
        self.note_book(changed);
    }

    /// Files the drafted reading. One no item could be named for can't be
    /// told from any other, so it goes with the draft.
    fn file_reading(&mut self, now: Timestamp, batch: &mut Batch) {
        let draft = std::mem::take(&mut self.reading);
        let Some(reading) = reading_from(&draft) else {
            return;
        };
        let Some(name) = draft.get("name").and_then(text) else {
            return;
        };
        let copy = self.only_copy_at_hand(name);
        let item = ItemRef {
            name,
            copy: copy.as_deref(),
        };
        let learned = self.book.learn(reading, item, now, batch);
        self.note_book(learned);
    }

    /// The copy of `name` at hand, when there's just one: a copy on the
    /// character counts, known by id or not.
    fn only_copy_at_hand(&self, name: &str) -> Option<String> {
        let key = plain_name(name);
        let carried = (self.carried.iter())
            .filter(|carried| plain_name(&carried.name) == key)
            .map(|carried| Some(carried.id.clone()));
        let worn = (self.worn.iter())
            .filter(|worn| plain_name(&worn.item) == key)
            .map(|worn| worn.copy.clone());
        let mut copies = carried.chain(worn);
        match (copies.next(), copies.next()) {
            (Some(only), None) => only,
            _ => None,
        }
    }

    fn file_worn(
        &mut self,
        count: i64,
        character: &str,
        watch: Option<&GearWatch>,
        now: Timestamp,
        batch: &mut Batch,
    ) {
        let draft = std::mem::take(&mut self.worn_rows);
        let mut taken = Vec::new();
        let mut worn = Vec::new();
        for position in 1..=count {
            let field = |name: &str| {
                let value = draft.get(&format!("{position}.{name}"));
                value.and_then(text).unwrap_or_default().to_owned()
            };
            let (slot, item) = (field("slot"), field("item"));
            let key = plain_name(&item);
            let copy = watch.and_then(|watch| {
                let at = watch.copy_worn_as(&key, &taken)?;
                taken.push(at);
                Some(watch.worn_copies[at].1.clone())
            });
            let was_saved = |row: &&Worn| !item.is_empty() && row.item == item;
            let copy = copy.or_else(|| {
                let saved = self.saved_worn.iter().find(was_saved);
                saved.and_then(|row| row.copy_id.clone())
            });
            worn.push(WornItem { slot, item, copy });
        }
        for row in self.saved_worn.drain(..) {
            if row.position > count {
                batch.delete(Key::Worn {
                    character_id: character.to_owned(),
                    position: row.position,
                });
            }
        }
        if !self.playing.is_empty() {
            self.worn_by.clone_from(&self.playing);
        }
        for (position, worn) in (1..).zip(&worn) {
            let row = Worn {
                character_id: character.to_owned(),
                position,
                slot: worn.slot.clone(),
                item: worn.item.clone(),
                copy_id: worn.copy.clone(),
                updated: now,
            };
            batch.upsert(row.clone());
            self.saved_worn.push(row);
        }
        self.worn = worn;
        self.changed = true;
    }

    /// The carried list changed. A copy carried isn't worn: one taken off by
    /// words may have been another of its name than the worn list thought.
    pub(super) fn carried_changed(&mut self, inventory: &Inventory, watch: &mut GearWatch) {
        self.carried = (inventory.items())
            .map(|item| CarriedItem {
                id: item.id.clone(),
                name: item.name.clone(),
            })
            .collect();
        let carried = &self.carried;
        (watch.worn_copies).retain(|(_, id)| !carried.iter().any(|item| item.id == *id));
        self.changed = true;
    }

    /// Notes gear going on or coming off, from a plugin's event.
    pub(super) fn event(
        &mut self,
        event: &PluginEvent,
        plugins: &PluginHost,
        inventory: &mut Inventory,
        watch: &mut GearWatch,
        hub: &StoreHub,
        saving: &mut Saving<'_>,
    ) {
        let Some(moved) = moved(plugins, event) else {
            return;
        };
        let Some(name) = event.capture(ITEM_CAPTURE) else {
            return;
        };
        if watch
            .change
            .as_ref()
            .is_some_and(|change| change.saw_prompt)
        {
            self.finish_change(watch, saving);
        }
        let change = watch.change.get_or_insert_with(|| GearChange {
            worn: Vec::new(),
            removed: Vec::new(),
            before: stats_now(&watch.stats, hub),
            after: StatMap::new(),
            saw_prompt: false,
        });
        let key = plain_name(name);
        let copy = match moved {
            Moved::On => {
                let copy = inventory.wore(&key);
                let held = Held {
                    name: name.to_owned(),
                    copy: copy.clone(),
                };
                change.worn.push(held.clone());
                self.going_on.push(held);
                if let Some(copy) = &copy {
                    watch.worn_copies.push((key, copy.clone()));
                }
                copy
            }
            Moved::Off => {
                let at = watch.worn_copies.iter().position(|(worn, _)| *worn == key);
                let noted = at.map(|at| watch.worn_copies.remove(at).1);
                let listed = self.take_off(&key);
                let copy = noted.or(listed);
                let held = Held {
                    name: name.to_owned(),
                    copy: copy.clone(),
                };
                change.removed.push(held);
                if let Some(copy) = &copy {
                    inventory.took_off(copy.clone(), name.to_owned());
                }
                copy
            }
        };
        if copy.is_some() {
            self.carried_changed(inventory, watch);
            hub.write::<CharactersState>(CharactersState(inventory.entries()));
        }
    }

    /// Empties the first worn row of the plain name `key`, returning its copy.
    fn take_off(&mut self, key: &str) -> Option<String> {
        let at = (self.worn.iter()).position(|worn| plain_name(&worn.item) == key)?;
        self.worn[at].item.clear();
        self.changed = true;
        self.worn[at].copy.take()
    }

    /// An item that went on has a slot now: into the first empty row for it,
    /// or a new row after the slot's last.
    fn put_on(&mut self, held: Held, slot: &str) {
        let rows = || self.worn.iter();
        let empty = rows().position(|worn| worn.slot == slot && worn.item.is_empty());
        let row = WornItem {
            slot: slot.to_owned(),
            item: held.name,
            copy: held.copy,
        };
        match empty {
            Some(at) => self.worn[at] = row,
            None => {
                let after = rows().rposition(|worn| worn.slot == slot);
                let at = after.map_or(self.worn.len(), |at| at + 1);
                self.worn.insert(at, row);
            }
        }
        self.changed = true;
    }

    /// Reads a GMCP message for what gear changed. The game answers a gear
    /// command with one `Char.Status`, so the first with a gear stat settles
    /// the change: waiting longer would catch a spell wearing off.
    pub(super) fn gmcp(
        &mut self,
        package: &str,
        json: &str,
        watch: &mut GearWatch,
        hub: &StoreHub,
        saving: &mut Saving<'_>,
    ) {
        if is(package, VITALS) {
            watch.note_stats(vital_maximums(hub));
        } else if is(package, STATUS)
            && let Some(values) = gmcp::from_json::<BTreeMap<String, Json>>(json)
        {
            let stats = (values.iter())
                .filter(|(key, _)| GEAR_STATS.contains(&key.as_str()))
                .filter_map(|(key, value)| Some((key.clone(), number(value)?)));
            if watch.note_stats(stats) {
                self.finish_change(watch, saving);
            }
        }
    }

    /// Files what the change in hand measured.
    pub(super) fn finish_change(&mut self, watch: &mut GearWatch, saving: &mut Saving<'_>) {
        let Some((held, effects)) = watch.change.take().and_then(GearChange::measured) else {
            return;
        };
        let item = ItemRef {
            name: &held.name,
            copy: held.copy.as_deref(),
        };
        let learned = (self.book).learn_measured(effects, item, saving.now, saving.batch);
        self.note_book(learned);
    }

    /// Hands a shell what changed since the last call. Only what did is
    /// copied: most changes are to what's carried, and leave the book alone.
    pub(super) fn publish(&mut self, plugins: &PluginHost, hub: &StoreHub) {
        if self.slots_of != plugins.generation() {
            self.slots_of = plugins.generation();
            let declared = plugins.declared(|points| points.gear.as_ref());
            self.slots = declared.flat_map(|(_, gear)| gear.slots.clone()).collect();
            self.changed = true;
        }
        if std::mem::take(&mut self.book_changed) {
            self.shared_book = Arc::new(self.book.clone());
            self.changed = true;
        }
        self.keep_matcher_current();
        if !std::mem::take(&mut self.changed) {
            return;
        }
        hub.write::<ItemsState>(ItemsState {
            book: Arc::clone(&self.shared_book),
            matcher: Arc::clone(&self.matcher),
            worn: self.worn.as_slice().into(),
            carried: self.carried.as_slice().into(),
            slots: Arc::clone(&self.slots),
        });
    }

    /// Takes a finished matcher, and rebuilds one if the names moved on. A
    /// rebuild over a full book takes tens of milliseconds, so it has a thread
    /// of its own: no line waits for it, and the old matcher serves until then.
    fn keep_matcher_current(&mut self) {
        if let Some(built) = self.matcher_built.as_ref().map(Receiver::try_recv) {
            match built {
                Ok(matcher) => {
                    self.matcher = Arc::new(matcher);
                    self.matcher_built = None;
                    self.changed = true;
                    self.names_changed = true;
                }
                Err(TryRecvError::Disconnected) => self.matcher_built = None,
                Err(TryRecvError::Empty) => return,
            }
        }
        if !std::mem::take(&mut self.names_changed) || self.matcher.is_over(self.book.names()) {
            return;
        }
        let names: Vec<String> = self.book.names().map(str::to_owned).collect();
        let (done, built) = mpsc::channel();
        let spawned = thread::Builder::new()
            .name("mudling-item-matcher".to_owned())
            .spawn(move || {
                // A session that's gone doesn't need it.
                let _ = done.send(ItemMatcher::new(names.iter().map(String::as_str)));
            });
        self.matcher_built = spawned.ok().map(|_| built);
    }
}

fn vital_maximums(hub: &StoreHub) -> Vec<(String, i64)> {
    hub.read(|vitals: &Versioned<VitalsState>| {
        let vitals = &vitals.value().0;
        let keys = [
            (Vital::MaxHp, "health_max"),
            (Vital::MaxMana, "mana_max"),
            (Vital::MaxMoves, "movement_max"),
        ];
        if vitals.get(&Vital::MaxHp).is_none_or(|max| *max <= 0) {
            return Vec::new();
        }
        (keys.iter())
            .filter_map(|(vital, key)| Some(((*key).to_owned(), *vitals.get(vital)?)))
            .collect()
    })
}

/// The stats as last reported, or as `score` last read them.
fn stats_now(reported: &StatMap, hub: &StoreHub) -> StatMap {
    let read = hub.read(|characters: &Versioned<CharactersState>| {
        let characters = &characters.value().0;
        let known = |key: &&str| characters.get(&format!("character.{key}")).and_then(whole);
        let found = GEAR_STATS.iter().filter_map(|key| {
            let value = known(key)?;
            Some(((*key).to_owned(), value))
        });
        found.collect::<StatMap>()
    });
    let mut stats = read;
    stats.extend(reported.iter().map(|(key, value)| (key.clone(), *value)));
    stats
}

/// A reading from its keys: `keywords`, `type`, `flags`, `level`, `quality`,
/// `base.<stat>`, `added.<spell>.<stat>` and `detail.<n>`.
fn reading_from(draft: &BTreeMap<String, Value>) -> Option<Reading> {
    let mut reading = Reading {
        keywords: draft.get("keywords").and_then(text)?.to_owned(),
        ..Reading::default()
    };
    let mut details = BTreeMap::new();
    for (key, value) in draft {
        reading_field(&mut reading, key, value, &mut details);
    }
    reading.details = details.into_values().collect();
    Some(reading)
}

fn reading_field(
    reading: &mut Reading,
    key: &str,
    value: &Value,
    details: &mut BTreeMap<i64, String>,
) {
    let (head, tail) = key.split_once('.').unwrap_or((key, ""));
    match (head, value) {
        ("type", Value::Text(kind)) => reading.item_type.clone_from(kind),
        ("flags", Value::Text(flags)) => {
            reading.flags = flags.split_whitespace().map(str::to_owned).collect();
        }
        ("level", Value::Number(level)) => reading.level = Some(*level as i64),
        ("quality", Value::Text(quality)) => reading.quality = Some(quality.clone()),
        ("base", Value::Number(amount)) => {
            reading.base.insert(tail.to_owned(), *amount as i64);
        }
        ("added", Value::Number(amount)) => {
            if let Some((spell, stat)) = tail.rsplit_once('.') {
                let totals = reading.added.entry(spell.to_owned()).or_default();
                totals.insert(stat.to_owned(), *amount as i64);
            }
        }
        ("detail", Value::Text(line)) => {
            details.insert(tail.parse().unwrap_or_default(), line.clone());
        }
        _ => {}
    }
}

/// Moves slots, keywords and readings an older MUDling kept under `meta` to
/// their rows. An old reading was filed by the item's words, one per words.
fn adopt_old_meta(book: &mut GearBook, meta: &[Meta], now: Timestamp, batch: &mut Batch) {
    let mut readings: BTreeMap<&str, BTreeMap<String, Value>> = BTreeMap::new();
    for entry in meta {
        let value = from_row(&entry.value);
        let gone = if let Some(key) = entry.key.strip_prefix(OLD_GEAR_META) {
            if let (Some(item), Some(Value::Text(slot))) = (key.strip_prefix(SLOT_PREFIX), &value) {
                book.learn_slot(slot, item, now, batch);
                true
            } else if let (Some(item), Some(Value::Text(words))) =
                (key.strip_prefix(KEYWORDS_PREFIX), &value)
            {
                book.learn_keywords(words, item, now, batch);
                true
            } else {
                false
            }
        } else if let Some(key) = entry.key.strip_prefix(OLD_IDENTIFY_META) {
            if let Some(at) = OLD_FIELDS.iter().filter_map(|field| key.rfind(field)).min()
                && let Some(value) = value
            {
                let (words, field) = key.split_at(at);
                readings
                    .entry(words)
                    .or_default()
                    .insert(field[1..].to_owned(), value);
            }
            true
        } else {
            false
        };
        if gone {
            batch.delete(Key::Meta(entry.key.clone()));
        }
    }
    for (words, mut draft) in readings {
        draft.insert("keywords".to_owned(), Value::Text(words.to_owned()));
        let name = draft.get("name").and_then(text).map(str::to_owned);
        if let (Some(name), Some(reading)) = (name, reading_from(&draft)) {
            let item = ItemRef {
                name: &name,
                copy: None,
            };
            book.learn(reading, item, now, batch);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(text: &str) -> Value {
        Value::Text(text.to_owned())
    }

    fn change(worn: usize, removed: usize, before: i64, after: i64) -> GearChange {
        let held = || Held {
            name: "a ring".to_owned(),
            copy: None,
        };
        GearChange {
            worn: vec![held(); worn],
            removed: vec![held(); removed],
            before: StatMap::from([("str".to_owned(), before)]),
            after: StatMap::from([("str".to_owned(), after)]),
            saw_prompt: false,
        }
    }

    #[test]
    fn a_change_measures_one_item_either_way_and_never_a_swap() {
        let effects = |change: GearChange| change.measured().map(|(_, effects)| effects);
        let plus_one = StatMap::from([("str".to_owned(), 1)]);
        assert_eq!(effects(change(1, 0, 15, 16)), Some(plus_one.clone()));
        assert_eq!(effects(change(0, 1, 16, 15)), Some(plus_one));
        assert_eq!(effects(change(1, 1, 15, 16)), None);
        assert_eq!(effects(change(1, 0, 15, 15)), Some(StatMap::new()));
    }

    #[test]
    fn a_reading_keeps_a_spell_with_a_dot_in_its_name_and_details_in_order() {
        let draft = BTreeMap::from([
            ("keywords".to_owned(), value("black bear bracelet")),
            ("flags".to_owned(), value("glow magic")),
            ("base.str".to_owned(), Value::Number(1.0)),
            ("added.st. elmo.ac".to_owned(), Value::Number(-4.0)),
            ("detail.2".to_owned(), value("Second.")),
            ("detail.1".to_owned(), value("First.")),
        ]);
        let reading = reading_from(&draft).unwrap();
        assert_eq!(reading.flags, ["glow", "magic"]);
        assert_eq!(reading.base["str"], 1);
        assert_eq!(reading.added["st. elmo"]["ac"], -4);
        assert_eq!(reading.details, ["First.", "Second."]);
    }

    #[test]
    fn a_reading_with_no_keywords_is_no_reading() {
        assert_eq!(reading_from(&BTreeMap::new()), None);
    }

    #[test]
    fn an_older_database_moves_to_rows_and_loses_its_meta() {
        let meta = |key: &str, value: &str| Meta {
            key: key.to_owned(),
            value: value.to_owned(),
            updated: Timestamp(1),
        };
        let rows = [
            meta("plugin.gear.slot.a tiamat mask", "\"head\""),
            meta("plugin.gear.keyword.a tiamat mask", "\"mask\""),
            meta("plugin.identify.mask tiamat.name", "\"a Tiamat mask\""),
            meta("plugin.identify.mask tiamat.base.wis", "2"),
            meta("plugin.identify.read", "\"mask tiamat\""),
        ];
        let mut book = GearBook::default();
        let mut batch = Batch::new();
        adopt_old_meta(&mut book, &rows, Timestamp(2), &mut batch);
        let item = book.item("a tiamat mask").unwrap();
        assert_eq!(item.facts.slot.as_deref(), Some("head"));
        assert_eq!(item.readings.len(), 1);
        assert_eq!(item.readings[0].base["wis"], 2);
        assert_eq!(item.readings[0].keywords, "mask tiamat");
    }
}
