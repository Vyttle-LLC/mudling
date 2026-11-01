//! What's known about items on one server, shared by its characters.
//!
//! [`GearBook`] keeps three kinds of knowledge. A copy is one item in the
//! game, by the game's id for it. An identify reading is what the game said
//! about an item, about one copy when that's known. Facts are what all copies
//! share: the words commands use, the slot, and what wearing one changed when
//! no copy was known. A copy's stats are exact once identified; an item nobody
//! identified gets an estimate from its other copies.
//!
//! What a character wears is theirs, and isn't here: see
//! [`ItemsState`](crate::store::ItemsState).

mod matcher;

pub use matcher::{ItemMatcher, ItemMention, MIN_NAME_LEN};

use crate::persist::{Batch, GearCopy, GearRows, Identification, ItemFacts, StatMap, Timestamp};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::RangeInclusive;
use std::sync::Arc;

/// The GMCP `Char.Status` keys gear can change. They name the protocol's
/// fields, not one game's: a game that sends none of them is measured never.
pub const GEAR_STATS: [&str; 11] = [
    "str",
    "int",
    "wis",
    "dex",
    "con",
    "hitroll",
    "damroll",
    "ac",
    "health_max",
    "mana_max",
    "movement_max",
];

/// The flag an identify gives a magic item, as the game's tag on a copy does.
const MAGIC_FLAG: &str = "magic";
const MAGICAL_TAG: &str = "magical";

/// An item's name without its tags, spaces collapsed, in lowercase: the name
/// other lines give it, and the key everything here is filed by.
pub fn plain_name(name: &str) -> String {
    shown_name(name).to_lowercase()
}

/// An item's name as the game writes it, without its tags.
pub fn shown_name(name: &str) -> String {
    let mut untagged = String::with_capacity(name.len());
    let mut depth = 0usize;
    for c in name.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => untagged.push(c),
            _ => {}
        }
    }
    untagged.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The tags on a copy's name: "(Glowing) (Humming) a ring" gives "Glowing"
/// and "Humming".
pub fn tags(name: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = name;
    while let Some((_, after)) = rest.split_once('(') {
        let Some((tag, tail)) = after.split_once(')') else {
            break;
        };
        found.push(tag.trim().to_owned());
        rest = tail;
    }
    found
}

/// One item as a line or a list names it: a copy when its id is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ItemRef<'a> {
    /// As the game shows it, tags included.
    pub name: &'a str,
    pub copy: Option<&'a str>,
}

/// Everything known about one item.
#[derive(Clone, Debug, PartialEq)]
pub struct KnownItem {
    /// As the game writes it, without tags.
    pub name: String,
    pub facts: ItemFacts,
    /// The word commands name it by, when the game's keywords aren't known.
    pub word: Option<String>,
    pub copies: Vec<GearCopy>,
    /// Every distinct reading, oldest first.
    pub readings: Vec<Identification>,
}

impl KnownItem {
    fn new(key: &str, name: &str) -> KnownItem {
        KnownItem {
            name: name.to_owned(),
            facts: ItemFacts {
                name: key.to_owned(),
                keywords: None,
                slot: None,
                measured: None,
                updated: Timestamp(0),
            },
            word: None,
            copies: Vec::new(),
            readings: Vec::new(),
        }
    }

    /// Whether anything says what it does.
    pub fn has_stats(&self) -> bool {
        !self.readings.is_empty()
            || self.facts.measured.is_some()
            || self.copies.iter().any(|copy| copy.measured.is_some())
    }

    /// The level identifies say it needs: the highest, as a copy that says
    /// more can only be a stricter one.
    pub fn level(&self) -> Option<i64> {
        self.readings.iter().filter_map(|r| r.level).max()
    }

    /// The word to name it by in a command: the game's first keyword, or the
    /// plugin's guess.
    pub fn command_word(&self) -> Option<&str> {
        (self.facts.keywords.as_deref())
            .and_then(|keywords| keywords.split_whitespace().next())
            .or(self.word.as_deref())
    }

    fn latest_reading_of(&self, copy: &str) -> Option<&Identification> {
        (self.readings.iter()).rfind(|reading| reading.copy_id.as_deref() == Some(copy))
    }

    fn estimate(&self) -> Option<Estimate> {
        // A reading seen before and after an enchant is one item.
        let mut seen = BTreeSet::new();
        let distinct: Vec<&Identification> = (self.readings.iter())
            .filter(|reading| seen.insert((&reading.keywords, &reading.base)))
            .collect();
        if !distinct.is_empty() {
            let any_magic = (distinct.iter()).any(|reading| is_magic(reading));
            return Some(Estimate::of(
                distinct.into_iter().map(|reading| &reading.base),
                false,
                any_magic,
            ));
        }
        let measured: Vec<&StatMap> = (self.copies.iter())
            .filter_map(|copy| copy.measured.as_ref())
            .chain(self.facts.measured.as_ref())
            .collect();
        (!measured.is_empty()).then(|| Estimate::of(measured.into_iter(), true, false))
    }
}

fn is_magic(reading: &Identification) -> bool {
    reading.flags.iter().any(|flag| flag == MAGIC_FLAG)
}

/// Everything a reading's copy does, what was added included.
fn effects_of(reading: &Identification) -> StatMap {
    let mut effects = reading.base.clone();
    for added in reading.added.values() {
        for (stat, amount) in added {
            *effects.entry(stat.clone()).or_default() += amount;
        }
    }
    effects.retain(|_, amount| *amount != 0);
    effects
}

/// The best known stats for a copy.
#[derive(Clone, Debug, PartialEq)]
pub enum ItemStats {
    /// This copy, from its identify: exact.
    Identified(Box<Identification>),
    /// This copy, from watching stats change.
    Measured(StatMap),
    /// From other copies of the item.
    Estimated(Estimate),
}

impl ItemStats {
    /// One number per stat, for comparing: an estimate's lowest, so a
    /// comparison never promises more than copies have shown.
    pub fn effects(&self) -> StatMap {
        match self {
            ItemStats::Identified(reading) => effects_of(reading),
            ItemStats::Measured(effects) => effects.clone(),
            ItemStats::Estimated(estimate) => (estimate.ranges.iter())
                .map(|(stat, range)| (stat.clone(), *range.start()))
                .filter(|(_, amount)| *amount != 0)
                .collect(),
        }
    }
}

/// What an item's copies have shown, stat by stat: one value where they agree,
/// a range where they differ.
#[derive(Clone, Debug, PartialEq)]
pub struct Estimate {
    pub ranges: BTreeMap<String, RangeInclusive<i64>>,
    /// How many copies or readings it's from.
    pub copies: usize,
    /// From watching stats change, as no copy was identified: enchants may
    /// be in it, and a stat at its cap missed.
    pub is_measured: bool,
    /// An identified copy was magic, so a magic copy isn't news.
    pub any_magic: bool,
    /// This copy looks magic where identified copies weren't: probably
    /// enchanted, so likely better.
    pub likely_enchanted: bool,
}

impl Estimate {
    fn of<'a>(
        readings: impl ExactSizeIterator<Item = &'a StatMap> + Clone,
        is_measured: bool,
        any_magic: bool,
    ) -> Estimate {
        let copies = readings.len();
        let stats: BTreeSet<&String> = readings.clone().flat_map(BTreeMap::keys).collect();
        let mut ranges = BTreeMap::new();
        for stat in stats {
            // A copy without the stat has it at 0.
            let values = readings.clone().map(|reading| reading.get(stat).copied());
            let values = values.map(Option::unwrap_or_default);
            let low = values.clone().min().unwrap_or(0);
            let high = values.max().unwrap_or(0);
            if low != 0 || high != 0 {
                ranges.insert(stat.clone(), low..=high);
            }
        }
        Estimate {
            ranges,
            copies,
            is_measured,
            any_magic,
            likely_enchanted: false,
        }
    }
}

/// What wearing one item would change against one worn in its slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comparison {
    /// The worn item, as the game shows it.
    pub worn: String,
    /// Whether anything is known about the worn item.
    pub known: bool,
    /// The difference, by stat; nothing where it's none.
    pub changes: StatMap,
}

/// One identify, as a plugin's hooks reported it, before it's filed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reading {
    pub keywords: String,
    pub item_type: String,
    pub flags: Vec<String>,
    pub level: Option<i64>,
    pub quality: Option<String>,
    pub base: StatMap,
    pub added: BTreeMap<String, StatMap>,
    pub details: Vec<String>,
}

impl Reading {
    /// An id that's the same wherever the same words were read, so two
    /// devices' copies of a reading merge into one row.
    fn id(&self, item: &str, copy: Option<&str>) -> String {
        let fields = serde_json::json!([
            item,
            copy,
            self.keywords,
            self.item_type,
            self.flags,
            self.level,
            self.quality,
            self.base,
            self.added,
            self.details,
        ]);
        format!("{:016x}", fnv1a(fields.to_string().as_bytes()))
    }
}

/// FNV-1a, 64 bits: small, and the same on every machine, as std's hasher
/// isn't promised to be.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// What's known about every item on a server, by plain name.
///
/// Items are shared, so the book a store holds is cheap to clone; each change
/// says whether it changed anything, and queues its rows on the batch it's
/// given.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GearBook {
    items: BTreeMap<String, Arc<KnownItem>>,
}

impl GearBook {
    /// The book the database holds.
    pub fn from_rows(rows: GearRows) -> GearBook {
        let mut book = GearBook::default();
        for facts in rows.facts {
            let name = facts.name.clone();
            book.item_mut(&name).facts = facts;
        }
        for copy in rows.copies {
            let key = plain_name(&copy.name);
            let item = book.item_mut(&copy.name);
            item.name = shown_name(&copy.name);
            item.facts.name = key;
            item.copies.push(copy);
        }
        let mut readings = rows.identifications;
        readings.sort_by_key(|reading| reading.updated);
        for reading in readings {
            let name = reading.item_name.clone();
            book.item_mut(&name).readings.push(reading);
        }
        book
    }

    /// Item names something is known about, by plain name.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.items.keys().map(String::as_str)
    }

    pub fn items(&self) -> impl ExactSizeIterator<Item = (&String, &Arc<KnownItem>)> {
        self.items.iter()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn item(&self, name: &str) -> Option<&KnownItem> {
        self.items.get(&plain_name(name)).map(Arc::as_ref)
    }

    fn item_mut(&mut self, name: &str) -> &mut KnownItem {
        let key = plain_name(name);
        let entry = (self.items.entry(key.clone()))
            .or_insert_with(|| Arc::new(KnownItem::new(&key, &shown_name(name))));
        Arc::make_mut(entry)
    }

    /// Files an identify as the truth about `item`: that copy's when its id
    /// is known. Returns whether it was news.
    pub fn learn(
        &mut self,
        reading: Reading,
        item: ItemRef<'_>,
        now: Timestamp,
        batch: &mut Batch,
    ) -> bool {
        let key = plain_name(item.name);
        let id = reading.id(&key, item.copy);
        let known = self.items.get(&key);
        let keywords_known = known.is_some_and(|known| {
            known.facts.keywords.as_deref() == Some(reading.keywords.as_str())
        });
        let read_before =
            known.is_some_and(|known| known.readings.iter().any(|read| read.id == id));
        if keywords_known && read_before {
            return false;
        }
        if let Some(copy) = item.copy {
            self.note_copy(copy, item.name, now, batch);
        }
        let entry = self.item_mut(item.name);
        if !keywords_known {
            entry.facts.keywords = Some(reading.keywords.clone());
            entry.facts.updated = now;
            batch.upsert(entry.facts.clone());
        }
        if !read_before {
            let row = Identification {
                id,
                item_name: key,
                copy_id: item.copy.map(str::to_owned),
                keywords: reading.keywords,
                item_type: reading.item_type,
                flags: reading.flags,
                level: reading.level,
                quality: reading.quality,
                base: reading.base,
                added: reading.added,
                details: reading.details,
                updated: now,
            };
            batch.upsert(row.clone());
            entry.readings.push(row);
        }
        true
    }

    /// What wearing or taking off `item` alone changed. An identify is exact,
    /// so it's never replaced; and "nothing changed" may just be a stat at
    /// its cap, so it never erases effects already seen. Returns whether it
    /// was news.
    pub fn learn_measured(
        &mut self,
        effects: StatMap,
        item: ItemRef<'_>,
        now: Timestamp,
        batch: &mut Batch,
    ) -> bool {
        let known = self.items.get(&plain_name(item.name));
        let keeps = |measured: Option<&StatMap>| {
            measured == Some(&effects)
                || (effects.is_empty() && measured.is_some_and(|m| !m.is_empty()))
        };
        if let Some(copy) = item.copy {
            let found = known.and_then(|known| {
                let copy = known.copies.iter().find(|saved| saved.id == copy)?;
                Some((known, copy))
            });
            let settled = found.is_some_and(|(known, saved)| {
                known.latest_reading_of(&saved.id).is_some() || keeps(saved.measured.as_ref())
            });
            if settled {
                return false;
            }
            self.note_copy(copy, item.name, now, batch);
            let entry = self.item_mut(item.name);
            if let Some(saved) = entry.copies.iter_mut().find(|saved| saved.id == copy) {
                saved.measured = Some(effects);
                saved.updated = now;
                batch.upsert(saved.clone());
            }
        } else {
            if keeps(known.and_then(|known| known.facts.measured.as_ref())) {
                return false;
            }
            let entry = self.item_mut(item.name);
            entry.facts.measured = Some(effects);
            entry.facts.updated = now;
            batch.upsert(entry.facts.clone());
        }
        true
    }

    /// Where the item goes. Returns whether it was news.
    pub fn learn_slot(
        &mut self,
        slot: &str,
        item: &str,
        now: Timestamp,
        batch: &mut Batch,
    ) -> bool {
        let known = self.items.get(&plain_name(item));
        if known.is_some_and(|known| known.facts.slot.as_deref() == Some(slot)) {
            return false;
        }
        let entry = self.item_mut(item);
        entry.facts.slot = Some(slot.to_owned());
        entry.facts.updated = now;
        batch.upsert(entry.facts.clone());
        true
    }

    /// The game's words for the item. Returns whether they were news.
    pub fn learn_keywords(
        &mut self,
        keywords: &str,
        item: &str,
        now: Timestamp,
        batch: &mut Batch,
    ) -> bool {
        let known = self.items.get(&plain_name(item));
        if known.is_some_and(|known| known.facts.keywords.as_deref() == Some(keywords)) {
            return false;
        }
        let entry = self.item_mut(item);
        entry.facts.keywords = Some(keywords.to_owned());
        entry.facts.updated = now;
        batch.upsert(entry.facts.clone());
        true
    }

    /// The word a plugin guessed commands name the item by. Saved by whoever
    /// calls this. Returns whether it was news.
    pub fn note_word(&mut self, word: &str, item: &str) -> bool {
        let known = self.items.get(&plain_name(item));
        if known.is_some_and(|known| known.word.as_deref() == Some(word)) {
            return false;
        }
        self.item_mut(item).word = Some(word.to_owned());
        true
    }

    fn note_copy(&mut self, id: &str, name: &str, now: Timestamp, batch: &mut Batch) {
        let entry = self.item_mut(name);
        if entry.copies.iter().any(|copy| copy.id == id) {
            return;
        }
        let copy = GearCopy {
            id: id.to_owned(),
            name: shown_name(name),
            measured: None,
            updated: now,
        };
        batch.upsert(copy.clone());
        entry.copies.push(copy);
    }

    /// What a command should call `item`: the copy's id, or the word it goes
    /// by. Only an id or a word goes in a command: a name from the server
    /// can't carry one with it.
    pub fn target(&self, item: ItemRef<'_>) -> Option<String> {
        let id = item.copy.filter(|id| id.chars().all(char::is_alphanumeric));
        let word = self.item(item.name).and_then(KnownItem::command_word);
        let is_word = |word: &&str| {
            word.chars()
                .all(|c| c.is_alphabetic() || c == '\'' || c == '-')
        };
        (id.or(word.filter(is_word))).map(str::to_owned)
    }

    /// The best known stats for `item`: a copy's own if learned, else an
    /// estimate from the item's other copies. A `(Magical)` tag on it hints
    /// at an enchant.
    pub fn stats(&self, item: ItemRef<'_>) -> Option<ItemStats> {
        let known = self.item(item.name)?;
        if let Some(copy) = item.copy {
            if let Some(reading) = known.latest_reading_of(copy) {
                return Some(ItemStats::Identified(Box::new(reading.clone())));
            }
            let saved = known.copies.iter().find(|saved| saved.id == copy);
            if let Some(measured) = saved.and_then(|saved| saved.measured.as_ref()) {
                return Some(ItemStats::Measured(measured.clone()));
            }
        }
        let mut estimate = known.estimate()?;
        let tagged_magical =
            (tags(item.name).iter()).any(|tag| tag.eq_ignore_ascii_case(MAGICAL_TAG));
        estimate.likely_enchanted = tagged_magical && !estimate.is_measured && !estimate.any_magic;
        Some(ItemStats::Estimated(estimate))
    }

    /// What wearing `item` would change against each of `worn`, each as what
    /// a line shows and its copy. Nothing for an item with no stats known.
    pub fn compare<'a>(
        &self,
        item: ItemRef<'_>,
        worn: impl IntoIterator<Item = ItemRef<'a>>,
    ) -> Vec<Comparison> {
        let Some(mine) = self.stats(item).map(|stats| stats.effects()) else {
            return Vec::new();
        };
        let key = plain_name(item.name);
        let others = worn.into_iter().filter(|worn| plain_name(worn.name) != key);
        let compare = |worn: ItemRef<'_>| {
            let theirs = self.stats(worn);
            let mut changes = mine.clone();
            for (stat, amount) in theirs.iter().flat_map(ItemStats::effects) {
                *changes.entry(stat).or_default() -= amount;
            }
            changes.retain(|_, amount| *amount != 0);
            Comparison {
                worn: worn.name.to_owned(),
                known: theirs.is_some(),
                changes,
            }
        };
        others.map(compare).collect()
    }
}

#[cfg(test)]
mod tests;
