//! Words for what's known about an item: "Str +1 · Dam +6 · AC −1 (better)".
//! The Gear panel and the chat's tooltips say it the same way.

use mudling_core::gear::{Comparison, Estimate, GEAR_STATS, ItemStats, shown_name};
use mudling_core::persist::{Identification, StatMap};
use std::collections::BTreeMap;
use std::fmt::Write;
use std::ops::{Range, RangeInclusive};

fn label(stat: &str) -> &str {
    match stat {
        "str" => "Str",
        "int" => "Int",
        "wis" => "Wis",
        "dex" => "Dex",
        "con" => "Con",
        "hitroll" => "Hit",
        "damroll" => "Dam",
        "ac" => "AC",
        "health_max" => "Max HP",
        "mana_max" => "Max mana",
        "movement_max" => "Max moves",
        other => other,
    }
}

/// The real minus sign: the plain one reads as a hyphen.
fn signed(value: i64) -> String {
    if value >= 0 {
        format!("+{value}")
    } else {
        format!("−{}", -value)
    }
}

/// Armor counts down: a lower number is better protection.
fn armor_note(stat: &str, value: i64) -> &'static str {
    match stat {
        "ac" if value < 0 => " (better)",
        "ac" => " (worse)",
        _ => "",
    }
}

/// The game's stats in their usual order, then any others a plugin named.
fn in_order(stats: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut stats: Vec<String> = stats.into_iter().collect();
    let rank = |stat: &String| {
        let known = GEAR_STATS.iter().position(|known| known == stat);
        known.unwrap_or(GEAR_STATS.len())
    };
    stats.sort_by_key(rank);
    stats
}

/// "Str +1 · AC −1 (better)", or nothing when nothing changed.
pub fn stat_changes(effects: &StatMap) -> Option<String> {
    let stats = effects.iter().filter(|(_, value)| **value != 0);
    let parts: Vec<String> = in_order(stats.map(|(stat, _)| stat.clone()))
        .iter()
        .map(|stat| {
            let value = effects[stat];
            format!(
                "{} {}{}",
                label(stat),
                signed(value),
                armor_note(stat, value)
            )
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// Each stat's lowest and highest value.
pub type StatRanges = BTreeMap<String, RangeInclusive<i64>>;

/// "+1", or "+1 to +3".
fn range_value(range: &RangeInclusive<i64>) -> String {
    let (low, high) = (*range.start(), *range.end());
    if low == high {
        signed(low)
    } else {
        format!("{} to {}", signed(low), signed(high))
    }
}

/// "Str +1 to +3 · Hit +2 · AC −1", leaving out what adds up to nothing, with
/// armor's note when `noted`.
fn range_words(ranges: &StatRanges, noted: bool) -> Option<String> {
    let changed = ranges.iter().filter(|(_, range)| **range != (0..=0));
    let parts: Vec<String> = in_order(changed.map(|(stat, _)| stat.clone()))
        .iter()
        .map(|stat| {
            let range = &ranges[stat];
            let note = if noted {
                armor_note(stat, *range.start())
            } else {
                ""
            };
            format!("{} {}{note}", label(stat), range_value(range))
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// "Str +1 to +3 · Hit +2".
fn ranges(estimate: &Estimate) -> Option<String> {
    range_words(&estimate.ranges, true)
}

/// What an item does, stat by stat: one value where it's exact, enchants
/// included, and an estimate's range.
pub fn stat_ranges(stats: &ItemStats) -> StatRanges {
    match stats {
        ItemStats::Estimated(estimate) => estimate.ranges.clone(),
        exact => (exact.effects().into_iter())
            .map(|(stat, value)| (stat, value..=value))
            .collect(),
    }
}

/// What a row's second line says of an item: its stats, without where they're
/// from, which More and the tooltip say.
pub fn brief(stats: Option<&ItemStats>) -> String {
    match stats {
        None => STATS_UNKNOWN.to_owned(),
        Some(stats) => {
            range_words(&stat_ranges(stats), false).unwrap_or_else(|| NO_STAT_CHANGES.to_owned())
        }
    }
}

/// "Worn total · Str +3 · AC −31 · 2 unknown": what's known adds up to, and
/// how many worn items nothing is known about.
pub fn worn_total(total: &StatRanges, unknown: usize) -> String {
    let mut parts = vec![WORN_TOTAL.to_owned()];
    parts.extend(range_words(total, false));
    if unknown > 0 {
        parts.push(format!("{unknown} unknown"));
    } else if parts.len() == 1 {
        parts.push(NO_STAT_CHANGES.to_owned());
    }
    parts.join(" · ")
}

/// Whether a change is for the better: armor counts down.
fn is_better(stat: &str, change: i64) -> bool {
    if stat == "ac" { change < 0 } else { change > 0 }
}

/// Text with some words marked as a gain or a loss.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Marked {
    pub text: String,
    /// Each marked span, and whether it's for the better.
    pub marks: Vec<(Range<usize>, bool)>,
}

impl Marked {
    pub fn push(&mut self, words: &str) {
        self.text.push_str(words);
    }

    fn push_marked(&mut self, words: &str, better: bool) {
        let start = self.text.len();
        self.text.push_str(words);
        self.marks.push((start..self.text.len(), better));
    }

    /// "Wis +3 Int −2": each stat wearing it would change, marked, against
    /// one item worn in its slot.
    pub fn push_comparison(&mut self, compared: &Comparison) {
        let changed = in_order(compared.changes.keys().cloned());
        if changed.is_empty() {
            self.push("no change");
        }
        for (at, stat) in changed.iter().enumerate() {
            if at > 0 {
                self.push(" ");
            }
            let value = compared.changes[stat];
            let words = format!(
                "{} {}{}",
                label(stat),
                signed(value),
                armor_note(stat, value)
            );
            self.push_marked(&words, is_better(stat, value));
        }
        if !compared.known {
            self.push(" (worn stats unknown)");
        }
    }
}

fn added_total(reading: &Identification) -> StatMap {
    let mut total = StatMap::new();
    for effects in reading.added.values() {
        for (stat, amount) in effects {
            *total.entry(stat.clone()).or_default() += amount;
        }
    }
    total
}

/// One line: the stats, and how sure they are.
pub fn line(stats: &ItemStats) -> String {
    match stats {
        ItemStats::Identified(reading) => {
            let base = stat_changes(&reading.base);
            let added = stat_changes(&added_total(reading));
            let added = added.map_or_else(String::new, |added| format!(", enchanted {added}"));
            let base = base.as_deref().unwrap_or("No stat changes");
            format!("{base}{added} (identified)")
        }
        ItemStats::Measured(effects) => {
            let seen = stat_changes(effects);
            format!(
                "{} (measured)",
                seen.as_deref().unwrap_or("No stat changes seen")
            )
        }
        ItemStats::Estimated(estimate) => {
            let noun = if estimate.copies == 1 {
                "copy"
            } else {
                "copies"
            };
            let mut source = format!("estimate from {} {noun}", estimate.copies);
            if estimate.is_measured {
                source.push_str(", measured, may include enchants");
            }
            if estimate.likely_enchanted {
                source.push_str("; likely enchanted, may be better");
            }
            let seen = ranges(estimate);
            format!(
                "{} ({source})",
                seen.as_deref().unwrap_or("No stat changes")
            )
        }
    }
}

/// Everything known, for a tooltip: the stats and where they're from, then an
/// identify's other lines.
pub fn describe(stats: &ItemStats) -> String {
    let mut text = line(stats);
    let ItemStats::Identified(reading) = stats else {
        return text;
    };
    for (spell, effects) in &reading.added {
        let change = stat_changes(effects);
        let change = change.as_deref().unwrap_or("nothing");
        let _ = write!(text, "\nAdded by {spell}: {change}");
    }
    let mut facts = Vec::new();
    facts.extend(reading.level.map(|level| format!("Level {level}")));
    facts.extend(
        reading
            .quality
            .as_ref()
            .map(|quality| format!("Quality {quality}")),
    );
    if !reading.flags.is_empty() {
        facts.push(format!("Flags: {}", reading.flags.join(", ")));
    }
    if !facts.is_empty() {
        let _ = write!(text, "\n{}", facts.join(" · "));
    }
    for detail in &reading.details {
        let _ = write!(text, "\n{detail}");
    }
    text
}

/// "Instead of a small necklace: AC −1 (better) · Hit +2".
pub fn comparison(compared: &Comparison) -> String {
    let unknown = if compared.known {
        ""
    } else {
        " (its stats aren't known)"
    };
    let change = stat_changes(&compared.changes);
    let change = change.as_deref().unwrap_or("no change");
    format!(
        "Instead of {}{unknown}: {change}",
        shown_name(&compared.worn)
    )
}

/// What's said of an item nothing is known about.
pub const NOT_LEARNED: &str = "Not learned yet: identify it, or take it off and put it back on.";
/// The same, in a row's second line.
const STATS_UNKNOWN: &str = "Stats unknown";
const NO_STAT_CHANGES: &str = "No stat changes";
pub const WORN_TOTAL: &str = "Worn total";

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(pairs: &[(&str, i64)]) -> StatMap {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect()
    }

    fn identified() -> Identification {
        Identification {
            id: "1".to_owned(),
            item_name: "a ring".to_owned(),
            copy_id: None,
            keywords: "ring".to_owned(),
            item_type: "armor".to_owned(),
            flags: vec!["magic".to_owned()],
            level: Some(8),
            quality: Some("20 / 20 hps".to_owned()),
            base: stats(&[("ac", -1), ("str", 1)]),
            added: BTreeMap::from([("enchant armor".to_owned(), stats(&[("ac", -4)]))]),
            details: vec!["Weight 3, value 40, level 8.".to_owned()],
            updated: mudling_core::persist::Timestamp(0),
        }
    }

    #[test]
    fn changes_read_in_the_games_order_and_armor_says_which_way_is_better() {
        let changes = stats(&[("ac", -1), ("hitroll", 2), ("str", 1), ("luck", 3)]);
        assert_eq!(
            stat_changes(&changes).unwrap(),
            "Str +1 · Hit +2 · AC −1 (better) · luck +3"
        );
        assert_eq!(stat_changes(&stats(&[("ac", 2)])).unwrap(), "AC +2 (worse)");
        assert_eq!(stat_changes(&StatMap::new()), None);
    }

    #[test]
    fn an_identify_says_everything_it_knows() {
        let text = describe(&ItemStats::Identified(Box::new(identified())));
        assert_eq!(
            text,
            "Str +1 · AC −1 (better), enchanted AC −4 (better) (identified)\n\
             Added by enchant armor: AC −4 (better)\n\
             Level 8 · Quality 20 / 20 hps · Flags: magic\n\
             Weight 3, value 40, level 8."
        );
    }

    #[test]
    fn a_comparison_says_when_the_worn_item_is_a_mystery() {
        let compared = Comparison {
            worn: "(Glowing) a small necklace".to_owned(),
            known: false,
            changes: stats(&[("hitroll", 2)]),
        };
        assert_eq!(
            comparison(&compared),
            "Instead of a small necklace (its stats aren't known): Hit +2"
        );
    }
}
