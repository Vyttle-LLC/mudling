//! Mob memory: which kinds of mob are aggressive, learned from who starts
//! fights and keyed by the mob's noun ("bear") so every one of its kind is
//! flagged; and the mobs the player marked for gear or as people who
//! matter, keyed by the mob itself, since gear comes from one mob, not
//! every one of its kind.

use crate::persist::MobMark;
use std::collections::BTreeSet;

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
    pub fn name(self) -> &'static str {
        match self {
            Mark::Aggressive => "aggressive",
            Mark::Gear => "gear",
            Mark::Important => "important",
        }
    }

    fn parse(name: &str) -> Option<Mark> {
        [Mark::Aggressive, Mark::Gear, Mark::Important]
            .into_iter()
            .find(|mark| mark.name() == name)
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

#[derive(Clone, Debug, Default)]
pub struct MobMemory {
    aggressive: BTreeSet<String>,
    gear: BTreeSet<String>,
    important: BTreeSet<String>,
    changes: Vec<MarkChange>,
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

    /// A mob attacked unprovoked: its kind is aggressive. Returns whether
    /// that's news.
    pub fn learn_aggressor(&mut self, name: &str) -> bool {
        noun(name).is_some_and(|noun| self.set(Mark::Aggressive, noun, true))
    }

    /// Whether a mob, by its room line, is of a kind known to be aggressive.
    pub fn is_aggressive(&self, title: &str) -> bool {
        words(title).any(|word| self.aggressive.contains(&word))
    }

    /// The player marking a kind of mob, by one of its room lines, as
    /// aggressive or not (undoing a wrong guess).
    pub fn set_aggressive(&mut self, title: &str, aggressive: bool) {
        if aggressive {
            if let Some(noun) = noun(&mob_name(title)) {
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
        self.gear.contains(&mob_name(title))
    }

    pub fn set_gear(&mut self, title: &str, gear: bool) {
        let name = mob_name(title);
        if !name.is_empty() {
            self.set(Mark::Gear, name, gear);
        }
    }

    /// Whether a mob, by its room line, was marked as someone who matters.
    /// Their line changes with what they're doing, so a mark made on one
    /// reading covers the others.
    pub fn is_important(&self, title: &str) -> bool {
        let name = mob_name(title);
        self.important.iter().any(|marked| same_mob(marked, &name))
    }

    pub fn set_important(&mut self, title: &str, important: bool) {
        let name = mob_name(title);
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

    fn set(&mut self, mark: Mark, name: String, set: bool) -> bool {
        let names = self.set_of(mark);
        let changed = if set {
            names.insert(name.clone())
        } else {
            names.remove(&name)
        };
        if changed {
            self.changes.push(MarkChange { name, mark, set });
        }
        changed
    }
}

/// "(Tag) A grey wolf is here, howling." → "a grey wolf": the mob without
/// its tags or what it's doing, which change from visit to visit.
pub fn mob_name(title: &str) -> String {
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
    let spaced = untagged.split_whitespace().collect::<Vec<_>>().join(" ");
    let name = [" is ", " are "]
        .iter()
        .filter_map(|verb| spaced.find(verb))
        .min()
        .map_or(spaced.as_str(), |end| &spaced[..end]);
    name.trim_matches([' ', '.', '!', ',']).to_owned()
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

fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphabetic())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
}
