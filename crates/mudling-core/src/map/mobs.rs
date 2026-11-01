//! Mob memory: which kinds of mob are aggressive, learned from who starts
//! fights and keyed by the mob's noun ("bear") so every one of its kind is
//! flagged; and the mobs the player marked for gear or as people who
//! matter, keyed by the mob itself, since gear comes from one mob, not
//! every one of its kind.

use crate::persist::MobMark;
use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

/// A mark on a mob, as persisted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Mark {
    /// By noun: every mob of the kind.
    Aggressive,
    /// By name: this mob drops important gear.
    Gear,
    /// By name: someone who matters (a healer, a quest giver).
    Important,
}

impl Mark {
    pub const ALL: [Mark; 3] = [Mark::Aggressive, Mark::Gear, Mark::Important];

    pub fn name(self) -> &'static str {
        match self {
            Mark::Aggressive => "aggressive",
            Mark::Gear => "gear",
            Mark::Important => "important",
        }
    }

    fn parse(name: &str) -> Option<Mark> {
        Mark::ALL.into_iter().find(|mark| mark.name() == name)
    }

    /// Whether this mark, held under `marked`, covers the mob on a room line,
    /// which `namer` names.
    pub fn covers(self, marked: &str, title: &str, namer: &MobNamer) -> bool {
        match self {
            Mark::Aggressive => has_word(title, marked),
            Mark::Gear | Mark::Important => self.covers_name(marked, &namer.name(title)),
        }
    }

    /// [`Mark::covers`], for a mob already named.
    fn covers_name(self, marked: &str, name: &str) -> bool {
        match self {
            Mark::Aggressive => has_word(name, marked),
            Mark::Gear => marked == name,
            // Their line changes with what they're doing, so a mark made on
            // one reading covers the others.
            Mark::Important => same_mob(marked, name),
        }
    }
}

/// A mark set or cleared, for persisting and for views.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkChange {
    /// The noun for [`Mark::Aggressive`], the mob's name otherwise.
    pub name: String,
    pub mark: Mark,
    pub set: bool,
}

/// A game's reading of a room title, if it has one for that title.
type TitleRule = dyn Fn(&str) -> Option<String> + Send + Sync;

/// Reads a game's room titles as mob names. A title the game's rule doesn't
/// name, or a game with no rule, goes by [`mob_name`].
#[derive(Clone, Default)]
pub struct MobNamer(Option<Arc<TitleRule>>);

impl MobNamer {
    pub fn new(rule: impl Fn(&str) -> Option<String> + Send + Sync + 'static) -> MobNamer {
        MobNamer(Some(Arc::new(rule)))
    }

    /// The mob's name, lowercase with its spaces collapsed, as marks hold it.
    pub fn name(&self, title: &str) -> String {
        let named = self.0.as_ref().and_then(|rule| rule(title));
        named.map_or_else(
            || mob_name(title),
            |name| {
                let words = name.to_lowercase();
                words.split_whitespace().collect::<Vec<_>>().join(" ")
            },
        )
    }
}

impl PartialEq for MobNamer {
    fn eq(&self, other: &MobNamer) -> bool {
        match (&self.0, &other.0) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (a, b) => a.is_none() && b.is_none(),
        }
    }
}

impl Eq for MobNamer {}

impl fmt::Debug for MobNamer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MobNamer")
    }
}

#[derive(Clone, Debug, Default)]
pub struct MobMemory {
    aggressive: BTreeSet<String>,
    gear: BTreeSet<String>,
    important: BTreeSet<String>,
    changes: Vec<MarkChange>,
    namer: MobNamer,
}

impl MobMemory {
    pub fn from_marks(marks: &[MobMark]) -> MobMemory {
        let mut memory = MobMemory::default();
        for mark in marks {
            if let Some(kind) = Mark::parse(&mark.mark) {
                memory.set_of(kind).insert(mark.name.clone());
            }
        }
        memory
    }

    pub(super) fn named_by(self, namer: MobNamer) -> MobMemory {
        MobMemory { namer, ..self }
    }

    /// The mob a room title shows, by name.
    pub fn name(&self, title: &str) -> String {
        self.namer.name(title)
    }

    pub fn namer(&self) -> &MobNamer {
        &self.namer
    }

    /// A mob attacked unprovoked: its kind is aggressive.
    pub fn learn_aggressor(&mut self, name: &str) {
        if let Some(noun) = noun(name) {
            self.set(Mark::Aggressive, noun, true);
        }
    }

    /// Whether a mob, by its room line, is of a kind known to be aggressive.
    pub fn is_aggressive(&self, title: &str) -> bool {
        words(title)
            .filter_map(|word| self.aggressive.get(&word))
            .any(|marked| Mark::Aggressive.covers(marked, title, &self.namer))
    }

    /// The player marking a kind of mob, by one of its room lines, as
    /// aggressive or not (undoing a wrong guess).
    pub fn set_aggressive(&mut self, title: &str, aggressive: bool) {
        if aggressive {
            if let Some(noun) = noun_of_title(title) {
                self.set(Mark::Aggressive, noun, true);
            }
        } else {
            // Unmark every noun this line would have matched.
            for word in words(title).collect::<Vec<_>>() {
                self.set(Mark::Aggressive, word, false);
            }
        }
    }

    /// Whether a mob, by its room line, was marked for important gear.
    pub fn is_gear(&self, title: &str) -> bool {
        self.gear.contains(&self.name(title))
    }

    pub fn set_gear(&mut self, title: &str, gear: bool) {
        let name = self.name(title);
        if !name.is_empty() {
            self.set(Mark::Gear, name, gear);
        }
    }

    /// Whether a mob, by its room line, was marked as someone who matters.
    pub fn is_important(&self, title: &str) -> bool {
        let name = self.name(title);
        let covers = |marked: &String| Mark::Important.covers_name(marked, &name);
        self.important.iter().any(covers)
    }

    pub fn set_important(&mut self, title: &str, important: bool) {
        let name = self.name(title);
        if name.is_empty() {
            return;
        }
        if important {
            self.set(Mark::Important, name, true);
        } else {
            let marked: Vec<String> = self
                .important
                .iter()
                .filter(|marked| same_mob(marked, &name))
                .cloned()
                .collect();
            for marked in marked {
                self.set(Mark::Important, marked, false);
            }
        }
    }

    /// Whether `mark` covers a mob, by its room line.
    pub fn is(&self, mark: Mark, title: &str) -> bool {
        match mark {
            Mark::Aggressive => self.is_aggressive(title),
            Mark::Gear => self.is_gear(title),
            Mark::Important => self.is_important(title),
        }
    }

    /// The player setting or clearing `mark` on a mob, by its room line.
    pub fn set_mark(&mut self, mark: Mark, title: &str, set: bool) {
        match mark {
            Mark::Aggressive => self.set_aggressive(title, set),
            Mark::Gear => self.set_gear(title, set),
            Mark::Important => self.set_important(title, set),
        }
    }

    pub fn marks(&self, mark: Mark) -> &BTreeSet<String> {
        match mark {
            Mark::Aggressive => &self.aggressive,
            Mark::Gear => &self.gear,
            Mark::Important => &self.important,
        }
    }

    pub(super) fn take_changes(&mut self) -> Vec<MarkChange> {
        std::mem::take(&mut self.changes)
    }

    fn set_of(&mut self, mark: Mark) -> &mut BTreeSet<String> {
        match mark {
            Mark::Aggressive => &mut self.aggressive,
            Mark::Gear => &mut self.gear,
            Mark::Important => &mut self.important,
        }
    }

    fn set(&mut self, mark: Mark, name: String, set: bool) {
        let names = self.set_of(mark);
        let changed = if set {
            names.insert(name.clone())
        } else {
            names.remove(&name)
        };
        if changed {
            self.changes.push(MarkChange { name, mark, set });
        }
    }
}

/// "(Tag) A grey wolf is here, howling." → "a grey wolf": the mob without
/// its tags or what it's doing, which change from visit to visit.
pub fn mob_name(title: &str) -> String {
    let untagged = without_tags(title);
    let spaced = untagged.split_whitespace().collect::<Vec<_>>().join(" ");
    let name = [" is ", " are "]
        .iter()
        .filter_map(|verb| spaced.find(verb))
        .min()
        .map_or(spaced.as_str(), |end| &spaced[..end]);
    name.trim_matches([' ', '.', '!', ',']).to_owned()
}

/// A title in lowercase, without its tags.
fn without_tags(title: &str) -> String {
    let mut untagged = String::with_capacity(title.len());
    let mut depth = 0usize;
    for c in title.chars() {
        match c {
            '(' => depth += 1,
            ')' if depth > 0 => depth -= 1,
            _ if depth == 0 => untagged.extend(c.to_lowercase()),
            _ => {}
        }
    }
    untagged
}

/// One name is the other, or starts it: a mob's name and its whole line.
fn same_mob(a: &str, b: &str) -> bool {
    a == b
        || a.strip_prefix(b).is_some_and(|rest| rest.starts_with(' '))
        || b.strip_prefix(a).is_some_and(|rest| rest.starts_with(' '))
}

/// "The grey wolf" → "wolf".
fn noun(name: &str) -> Option<String> {
    words(name).last()
}

/// The noun of a room title: the last word of its opening noun phrase, which
/// ends at the first verb, "of", or a comma that closes more than adjectives.
/// "A myconoid shaman sways here, spores drifting." → "shaman".
fn noun_of_title(title: &str) -> Option<String> {
    const ARTICLES: [&str; 4] = ["a", "an", "the", "some"];
    const PHRASE_ENDS: [&str; 7] = ["is", "are", "of", "here", "who", "that", "with"];
    // A present-tense verb; plural nouns rarely open a mob's room line.
    let verb = |word: &str| word.len() > 3 && word.ends_with('s') && !word.ends_with("ss");

    let untagged = without_tags(title);
    let mut phrase: Vec<&str> = Vec::new();
    for clause in untagged.split(',') {
        let mut ended = false;
        for word in clause
            .split(|c: char| !c.is_alphabetic() && c != '\'')
            .filter(|word| !word.is_empty())
        {
            if PHRASE_ENDS.contains(&word) || (!phrase.is_empty() && verb(word)) {
                ended = true;
                break;
            }
            phrase.push(word);
        }
        // "An old, wrinkled nanny": a comma after only an adjective doesn't
        // end the phrase.
        let content = phrase.iter().filter(|word| !ARTICLES.contains(word));
        if ended || content.count() > 1 {
            break;
        }
    }
    let noun = phrase.iter().rev().find(|word| !ARTICLES.contains(word))?;
    Some((*noun).to_owned())
}

fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphabetic())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
}

/// Whether `lowercase`, a whole word, is in `text`, in any case.
fn has_word(text: &str, lowercase: &str) -> bool {
    text.split(|c: char| !c.is_alphabetic()).any(|word| {
        word.chars()
            .flat_map(char::to_lowercase)
            .eq(lowercase.chars())
    })
}
