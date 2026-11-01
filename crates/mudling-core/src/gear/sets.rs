//! Gear sets: what putting a set on takes right now, worked out from where
//! each piece is.
//!
//! [`plan`] is one pure function of a set and what's at hand, so what the
//! editor previews is what Wear sends. Pieces already on are left alone.
//! First, whatever is in the way comes off: in each slot a piece is going to,
//! enough of what's there that isn't in the set. The pieces come out of their
//! bags, what came off goes into the set's container (in that order, so a full
//! bag has room), and the pieces go on. Wearing over a full slot isn't relied
//! on: a game may swap out a piece of its own choosing. Anything a piece of
//! unknown slot swaps out is put away as the game reports it: see [`PutAway`].
//!
//! Commands name a copy by the game's id, so only that copy answers, and
//! otherwise an item by its words. A piece pinned to a copy never goes on as
//! another copy of its name: the plan says it's missing instead.

use super::{GearBook, plain_name, shown_name};
use crate::persist::{GearPiece, GearSet, Timestamp};
use crate::plugin::points::Keywords;
use crate::store::{CarriedBag, CarriedItem, WornItem};
use std::collections::{BTreeMap, BTreeSet};

/// Verbs, as a plugin's `[verbs]` table names them.
const WEAR: &str = "wear";
const REMOVE: &str = "remove";
const GET: &str = "get";
const PUT: &str = "put";

/// How long a change of gear lasts after the last thing came off, so a slow
/// game still has all it took off put away.
pub const CHANGE_WINDOW_MS: i64 = 15_000;

/// How one game takes gear: the plugin's words, and what its hooks wrote about
/// its slots.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GearRules {
    verbs: BTreeMap<String, String>,
    keywords: Keywords,
    /// How many items a slot holds, where it's more than one.
    holds: BTreeMap<String, usize>,
    /// The verb that fills a slot, where it isn't `wear`.
    slot_verbs: BTreeMap<String, String>,
}

impl GearRules {
    /// A game's words: `verbs` as the plugins name them and `keywords` for
    /// guessing an item's word. What's known about slots stays.
    pub fn use_words(&mut self, verbs: BTreeMap<String, String>, keywords: Keywords) {
        self.verbs = verbs;
        self.keywords = keywords;
    }

    /// Notes how many items `slot` holds. Returns whether it was news.
    pub fn note_holds(&mut self, slot: &str, holds: usize) -> bool {
        self.holds.insert(slot.to_owned(), holds) != Some(holds)
    }

    /// Notes the verb that fills `slot`. Returns whether it was news.
    pub fn note_slot_verb(&mut self, slot: &str, verb: &str) -> bool {
        let new = self.slot_verbs.get(slot).is_none_or(|known| known != verb);
        if new {
            self.slot_verbs.insert(slot.to_owned(), verb.to_owned());
        }
        new
    }

    /// How many items `slot` holds.
    pub fn capacity(&self, slot: &str) -> usize {
        self.holds.get(slot).copied().unwrap_or(1)
    }

    fn verb<'a>(&'a self, verb: &'a str) -> &'a str {
        self.verbs.get(verb).map_or(verb, String::as_str)
    }

    fn filling(&self, slot: Option<&str>) -> &str {
        let verb = slot.and_then(|slot| self.slot_verbs.get(slot));
        self.verb(verb.map_or(WEAR, String::as_str))
    }

    /// The command that puts `item` into `container`, each as commands name it.
    pub fn put_away(&self, item: &str, container: &str) -> String {
        format!("{} {item} {container}", self.verb(PUT))
    }
}

/// Where gear is right now. `carried` includes the bags themselves, as the
/// game lists them.
#[derive(Clone, Copy, Debug)]
pub struct GearNow<'a> {
    pub worn: &'a [WornItem],
    pub carried: &'a [CarriedItem],
    pub bags: &'a [CarriedBag],
}

impl<'a> GearNow<'a> {
    fn worn_pieces(self) -> impl Iterator<Item = &'a WornItem> {
        self.worn.iter().filter(|worn| !worn.item.is_empty())
    }

    fn loose(self) -> impl Iterator<Item = &'a CarriedItem> {
        (self.carried.iter()).filter(move |item| !self.bags.iter().any(|bag| bag.id == item.id))
    }

    /// Everything nearby by name, in the order a game would count it.
    fn names(self) -> Vec<&'a str> {
        let worn = self.worn_pieces().map(|worn| worn.item.as_str());
        let loose = self.loose().map(|item| item.name.as_str());
        let bagged = self.bags.iter().flat_map(|bag| {
            let inside = bag.items.iter().map(|item| item.name.as_str());
            std::iter::once(bag.name.as_str()).chain(inside)
        });
        worn.chain(loose).chain(bagged).collect()
    }

    /// The id of the bag carried by this name, if there's one.
    pub fn bag_id(self, name: &str) -> Option<&'a str> {
        let key = plain_name(name);
        let bag = self.bags.iter().find(|bag| plain_name(&bag.name) == key);
        bag.map(|bag| bag.id.as_str())
    }
}

/// An item as the plan moves it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Moved {
    pub name: String,
    pub copy: Option<String>,
}

/// What putting a set on takes right now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WearPlan {
    pub commands: Vec<String>,
    /// Pieces not worn, carried or in a bag, and pinned copies not seen.
    pub missing: Vec<String>,
    /// Pieces whose chosen copy isn't seen, while another of the name is on.
    pub stand_ins: Vec<String>,
    /// How many of the set's pieces are on already.
    pub left_alone: usize,
    /// What it takes off, in order.
    pub removed: Vec<Moved>,
    /// What it puts on, in order.
    pub wearing: Vec<Moved>,
}

/// How commands name what's nearby.
struct Naming<'a> {
    names: Vec<&'a str>,
    book: &'a GearBook,
    rules: &'a GearRules,
}

impl Naming<'_> {
    /// A copy's id, else the item's words. Ids and words only, as anything
    /// else from the server could carry a second command.
    fn of(&self, name: &str, copy: Option<&str>) -> String {
        match copy.filter(|id| is_id(id)) {
            Some(id) => id.to_owned(),
            None => word_for(name, self.book, self.rules, &self.names),
        }
    }
}

/// How a command names an item right now: the copy's id, or its words.
pub fn target(
    name: &str,
    copy: Option<&str>,
    now: GearNow<'_>,
    book: &GearBook,
    rules: &GearRules,
) -> String {
    let naming = Naming {
        names: now.names(),
        book,
        rules,
    };
    naming.of(name, copy)
}

fn is_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(char::is_alphanumeric)
}

/// How a command names `name` by its words: its known keywords, quoted when
/// there are several, else a likely word among `names` nearby.
pub fn word_for(name: &str, book: &GearBook, rules: &GearRules, names: &[&str]) -> String {
    let known = book
        .item(name)
        .and_then(|item| item.facts.keywords.as_ref());
    match known.and_then(|known| quoted(known)) {
        Some(keywords) => keywords,
        None => likely_word(name, &rules.keywords, names),
    }
}

/// `keywords` as a command says them, if they're safe to send: an apostrophe
/// inside quotes would end them.
fn quoted(keywords: &str) -> Option<String> {
    let words: Vec<&str> = keywords.split_whitespace().collect();
    let safe = |word: &&str| word.chars().all(|c| c.is_alphanumeric() || c == '-');
    match words.as_slice() {
        [] => None,
        [word] if word.chars().all(|c| c.is_alphabetic() || "'-".contains(c)) => {
            Some((*word).to_owned())
        }
        [_] => None,
        several if several.iter().all(safe) => Some(format!("'{}'", several.join(" "))),
        _ => None,
    }
}

/// Words a game might accept for an item. Games match the start of a word, so
/// a hyphenated one gives its first part: "red" for "red-eyed", never "eyed".
fn item_words(name: &str, keywords: &Keywords) -> Vec<String> {
    let plain = plain_name(name);
    let is_filler = |word: &str| {
        keywords.skip.iter().any(|skip| skip == word)
            || keywords.phrase_ends.iter().any(|end| end == word)
    };
    plain
        .split(|c: char| !c.is_alphabetic() && c != '\'' && c != '-')
        .filter_map(|word| word.split('-').next())
        .filter(|word| !word.is_empty() && !is_filler(word))
        .map(str::to_owned)
        .collect()
}

/// A likely word for `name` among the others nearby: its usual word, else one
/// none of them has, else the usual word counted ("2.ring").
fn likely_word(name: &str, keywords: &Keywords, names: &[&str]) -> String {
    let key = plain_name(name);
    let others: Vec<Vec<String>> = (names.iter())
        .filter(|other| plain_name(other) != key)
        .map(|other| item_words(other, keywords))
        .collect();
    let mut words = item_words(name, keywords);
    let usual = keywords.word_for(&key).or_else(|| words.last().cloned());
    let usual = usual.unwrap_or_default();
    words.retain(|word| *word != usual);
    words.sort_by_key(|word| std::cmp::Reverse(word.len()));
    let plain_words = words.iter().filter(|word| !word.contains('\''));
    let possessives = words.iter().filter(|word| word.contains('\''));
    let candidates = std::iter::once(&usual)
        .chain(plain_words)
        .chain(possessives);
    let is_distinct = |word: &&String| !others.iter().any(|other| other.contains(word));
    if let Some(distinct) = candidates.into_iter().find(is_distinct) {
        return distinct.clone();
    }
    let before = (names.iter())
        .take_while(|other| plain_name(other) != key)
        .filter(|other| item_words(other, keywords).contains(&usual))
        .count();
    match before {
        0 => usual,
        before => format!("{}.{usual}", before + 1),
    }
}

/// A set's pieces as they can be worn together: where more are chosen for a
/// slot than it holds, the ones later in the set win.
pub fn fitting<'a>(
    pieces: &'a [GearPiece],
    book: &GearBook,
    rules: &GearRules,
) -> Vec<&'a GearPiece> {
    let mut taken: BTreeMap<String, usize> = BTreeMap::new();
    let mut fits: Vec<&GearPiece> = (pieces.iter().rev())
        .filter(|piece| {
            let Some(slot) = slot_of(piece, book) else {
                return true;
            };
            let holds = rules.capacity(&slot);
            let count = taken.entry(slot).or_default();
            *count += 1;
            *count <= holds
        })
        .collect();
    fits.reverse();
    fits
}

/// Where a piece goes: as the set says, or as the book knows.
fn slot_of(piece: &GearPiece, book: &GearBook) -> Option<String> {
    let known = || book.item(&piece.item)?.facts.slot.clone();
    piece.slot.clone().or_else(known)
}

/// What's carried and in bags, as the plan takes things out.
struct AtHand<'a> {
    loose: Vec<&'a CarriedItem>,
    bagged: Vec<(&'a CarriedItem, &'a CarriedBag)>,
}

impl<'a> AtHand<'a> {
    fn has(&self, copy: &str) -> bool {
        self.loose.iter().any(|item| item.id == copy)
            || self.bagged.iter().any(|(item, _)| item.id == copy)
    }

    /// Takes out a pinned piece's own copy, or any of an unpinned piece's name.
    fn take(&mut self, piece: &GearPiece) -> Option<(&'a CarriedItem, Option<&'a CarriedBag>)> {
        let key = plain_name(&piece.item);
        let matches = |item: &CarriedItem| match &piece.copy_id {
            Some(copy) => item.id == *copy && is_id(copy),
            None => plain_name(&item.name) == key,
        };
        if let Some(at) = self.loose.iter().position(|item| matches(item)) {
            return Some((self.loose.remove(at), None));
        }
        let at = self.bagged.iter().position(|(item, _)| matches(item))?;
        let (item, bag) = self.bagged.remove(at);
        Some((item, Some(bag)))
    }
}

/// A piece on its way on.
struct Going<'a> {
    item: &'a CarriedItem,
    bag: Option<&'a CarriedBag>,
    slot: Option<String>,
}

/// What putting `set` on takes right now.
pub fn plan(set: &GearSet, now: GearNow<'_>, book: &GearBook, rules: &GearRules) -> WearPlan {
    let mut plan = WearPlan::default();
    let naming = Naming {
        names: now.names(),
        book,
        rules,
    };
    let worn: Vec<&WornItem> = now.worn_pieces().collect();
    let (going, staying, replaced) = sort_out(set, now, &worn, book, rules, &mut plan);
    let takes_off = make_room(&going, &worn, &staying, &replaced, rules);

    for worn in takes_off.iter().map(|&at| worn[at]) {
        let target = naming.of(&worn.item, worn.copy.as_deref());
        plan.commands
            .push(format!("{} {target}", rules.verb(REMOVE)));
        plan.removed.push(Moved {
            name: worn.item.clone(),
            copy: worn.copy.clone(),
        });
    }
    // Out of the bags before anything goes in, so a full bag has room.
    for piece in &going {
        if let Some(bag) = piece.bag {
            let target = naming.of(&piece.item.name, Some(&piece.item.id));
            let from = naming.of(&bag.name, Some(&bag.id));
            plan.commands
                .push(format!("{} {target} {from}", rules.verb(GET)));
        }
    }
    if let Some(container) = &set.container {
        let into = naming.of(container, now.bag_id(container));
        for removed in &plan.removed {
            let item = naming.of(&removed.name, removed.copy.as_deref());
            plan.commands.push(rules.put_away(&item, &into));
        }
    }
    for piece in &going {
        let target = naming.of(&piece.item.name, Some(&piece.item.id));
        let verb = rules.filling(piece.slot.as_deref());
        plan.commands.push(format!("{verb} {target}"));
        plan.wearing.push(Moved {
            name: piece.item.name.clone(),
            copy: Some(piece.item.id.clone()),
        });
    }
    plan
}

/// Sorts a set's pieces into what stays on and what goes on, and says which
/// worn items a chosen copy replaces. A worn copy stays when it's the chosen
/// one, or may be; another of the name stays too unless the chosen copy is at
/// hand to change into.
fn sort_out<'a>(
    set: &GearSet,
    now: GearNow<'a>,
    worn: &[&WornItem],
    book: &GearBook,
    rules: &GearRules,
    plan: &mut WearPlan,
) -> (Vec<Going<'a>>, BTreeSet<usize>, BTreeSet<usize>) {
    let mut at_hand = AtHand {
        loose: now.loose().collect(),
        bagged: (now.bags.iter())
            .flat_map(|bag| bag.items.iter().map(move |item| (item, bag)))
            .collect(),
    };
    let (mut staying, mut replaced) = (BTreeSet::new(), BTreeSet::new());
    let mut going = Vec::new();
    for piece in fitting(&set.pieces, book, rules) {
        let key = plain_name(&piece.item);
        let same_name = (0..worn.len()).filter(|at| {
            !staying.contains(at) && !replaced.contains(at) && plain_name(&worn[*at].item) == key
        });
        let same_name: Vec<usize> = same_name.collect();
        let chosen = piece.copy_id.as_deref();
        let exact =
            (same_name.iter()).find(|at| chosen.is_some() && worn[**at].copy.as_deref() == chosen);
        if let Some(&exact) = exact {
            staying.insert(exact);
            plan.left_alone += 1;
            continue;
        }
        if let Some(&other) = same_name.first() {
            if !chosen.is_some_and(|copy| at_hand.has(copy)) {
                staying.insert(other);
                plan.left_alone += 1;
                if chosen.is_some() && worn[other].copy.is_some() {
                    plan.stand_ins.push(piece.item.clone());
                }
                continue;
            }
            replaced.insert(other);
        }
        match at_hand.take(piece) {
            Some((item, bag)) => going.push(Going {
                item,
                bag,
                slot: slot_of(piece, book),
            }),
            None => plan.missing.push(piece.item.clone()),
        }
    }
    (going, staying, replaced)
}

/// The worn items to take off, by index: in each slot something's going to,
/// what's replaced, then whatever else isn't staying, as much as the slot needs.
fn make_room(
    going: &[Going<'_>],
    worn: &[&WornItem],
    staying: &BTreeSet<usize>,
    replaced: &BTreeSet<usize>,
    rules: &GearRules,
) -> Vec<usize> {
    let mut takes_off = Vec::new();
    let mut cleared = BTreeSet::new();
    for slot in going.iter().filter_map(|piece| piece.slot.as_deref()) {
        if !cleared.insert(slot) {
            continue;
        }
        let there: Vec<usize> = (0..worn.len())
            .filter(|at| worn[*at].slot == slot)
            .collect();
        let leaving: Vec<usize> = there
            .iter()
            .copied()
            .filter(|at| replaced.contains(at))
            .collect();
        let coming = (going.iter())
            .filter(|piece| piece.slot.as_deref() == Some(slot))
            .count();
        let extra = (there.len() - leaving.len() + coming).saturating_sub(rules.capacity(slot));
        let others = (there.iter().copied())
            .filter(|at| !staying.contains(at) && !replaced.contains(at))
            .take(extra);
        takes_off.extend(leaving.iter().copied().chain(others));
    }
    takes_off
}

/// An item a set could be made of, and where it is now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// As the game writes it, without tags.
    pub name: String,
    /// The game's id for this copy, when known.
    pub copy: Option<String>,
    /// Where it's worn, if it is.
    pub worn_in: Option<String>,
    /// The bag it's in, if it is.
    pub bag: Option<String>,
}

/// Everything at hand, worn first, then carried, then bag by bag: a copy each.
pub fn candidates(now: GearNow<'_>) -> Vec<Candidate> {
    let worn = now.worn_pieces().map(|worn| Candidate {
        name: shown_name(&worn.item),
        copy: worn.copy.clone(),
        worn_in: Some(worn.slot.clone()),
        bag: None,
    });
    let loose = now.loose().map(|item| Candidate {
        name: shown_name(&item.name),
        copy: Some(item.id.clone()),
        worn_in: None,
        bag: None,
    });
    let bagged = now.bags.iter().flat_map(|bag| {
        let name = shown_name(&bag.name);
        bag.items.iter().map(move |item| Candidate {
            name: shown_name(&item.name),
            copy: Some(item.id.clone()),
            worn_in: None,
            bag: Some(name.clone()),
        })
    });
    worn.chain(loose).chain(bagged).collect()
}

/// Which candidate each piece is: each piece's own copy first, then any of its
/// name, one candidate a piece. `None` is a piece not seen right now.
pub fn match_pieces(pieces: &[GearPiece], candidates: &[Candidate]) -> Vec<Option<usize>> {
    let mut free: Vec<bool> = vec![true; candidates.len()];
    let mut found = vec![None; pieces.len()];
    for (at, piece) in pieces.iter().enumerate() {
        let own = |(index, candidate): (usize, &Candidate)| {
            (free[index] && candidate.copy.is_some() && candidate.copy == piece.copy_id)
                .then_some(index)
        };
        found[at] = candidates.iter().enumerate().find_map(own);
        if let Some(index) = found[at] {
            free[index] = false;
        }
    }
    for (at, piece) in pieces.iter().enumerate() {
        if found[at].is_some() {
            continue;
        }
        let key = plain_name(&piece.item);
        // A pinned piece can only be a copy whose id isn't known yet.
        let named = |(index, candidate): (usize, &Candidate)| {
            let may_be = piece.copy_id.is_none() || candidate.copy.is_none();
            (free[index] && may_be && plain_name(&candidate.name) == key).then_some(index)
        };
        found[at] = candidates.iter().enumerate().find_map(named);
        if let Some(index) = found[at] {
            free[index] = false;
        }
    }
    found
}

/// While a set goes on, what comes off goes into its container, and the game's
/// answers say how that went. Wearing a piece of unknown slot swaps something
/// out without the plan knowing, so what the game says came off is put away
/// too. The window stays open while pieces keep coming off.
#[derive(Clone, Debug)]
pub struct PutAway {
    container: String,
    /// The container as the player knows it.
    container_name: String,
    /// The pieces that go on: they stay out.
    pieces: BTreeSet<String>,
    /// Plain names a put has been sent for and not answered, oldest first.
    expected: Vec<String>,
    until: Timestamp,
}

impl PutAway {
    /// - `container`: how commands name the set's container.
    /// - `sent`: the items the plan's own commands put away.
    pub fn new(
        container: String,
        container_name: String,
        pieces: impl IntoIterator<Item = String>,
        sent: impl IntoIterator<Item = String>,
        now: Timestamp,
    ) -> PutAway {
        PutAway {
            container,
            container_name,
            pieces: pieces.into_iter().map(|item| plain_name(&item)).collect(),
            expected: sent.into_iter().map(|item| plain_name(&item)).collect(),
            until: Timestamp(now.0 + CHANGE_WINDOW_MS),
        }
    }

    /// `name` came off. Says the container's command word if it should be put
    /// away: not one of the set's pieces, not already on its way, and not once
    /// the change is over.
    pub fn came_off(&mut self, name: &str, now: Timestamp) -> Option<&str> {
        let key = plain_name(name);
        if now >= self.until || self.pieces.contains(&key) || self.expected.contains(&key) {
            return None;
        }
        self.until = Timestamp(now.0 + CHANGE_WINDOW_MS);
        self.expected.push(key);
        Some(&self.container)
    }

    /// The game put `name` away.
    pub fn went_in(&mut self, name: &str) {
        let key = plain_name(name);
        if let Some(at) = self.expected.iter().position(|item| *item == key) {
            self.expected.remove(at);
        }
    }

    /// The game had nothing to put: a put for what hadn't come off yet.
    pub fn was_not_carried(&mut self) {
        if !self.expected.is_empty() {
            self.expected.remove(0);
        }
    }

    /// The container refused the oldest put: says what stays out, and where
    /// it would have gone.
    pub fn did_not_fit(&mut self) -> Option<(String, &str)> {
        if self.expected.is_empty() {
            return None;
        }
        Some((self.expected.remove(0), &self.container_name))
    }
}

#[cfg(test)]
mod tests;
