//! One matcher for many patterns: `RegexSet`s behind an Aho-Corasick filter.
//!
//! At build time each pattern is searched for literals that every match must
//! contain (` tells you ` in `^(?<who>\w+) tells you '(?<what>.*)'$`). The
//! literals of all patterns go into one Aho-Corasick automaton. A line that
//! contains none of them can't match any of those patterns, so it costs one
//! literal scan, however many plugins are on. The patterns are split into
//! chunks of a few dozen, each one `RegexSet`; the same scan says which
//! chunks' literals the line holds, and only those chunks run, each in one
//! pass. Patterns with no such literal sit in an unfiltered set that every
//! line runs.

use aho_corasick::AhoCorasick;
use regex::{RegexSet, RegexSetBuilder};
use regex_syntax::hir::literal::{Extractor, Seq};
use regex_syntax::hir::{Hir, HirKind};
use std::collections::BTreeMap;

/// Room for hundreds of patterns, Unicode classes included, in one set.
const SET_SIZE_LIMIT: usize = 64 << 20;
/// The [`DfaCache::Large`] limit. Chunks of [`CHUNK_PATTERNS`] settle at
/// 3–4 MiB on the replay benchmark, so this is headroom, not what they take.
const LARGE_DFA_CACHE: usize = 8 << 20;

/// How much the lazy DFA of each set may cache. The cache grows only as
/// lines need it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DfaCache {
    /// The `regex` crate's default (2 MiB), for matchers off the hot path.
    Default,
    /// For the merged matcher every line runs. At the default, a chunk keeps
    /// clearing its cache and falls back to the slow engine: 157 µs p99 per
    /// line on the replay benchmark with 300 patterns, against 4 µs.
    Large,
}

/// The most patterns in one filtered set. A lazy DFA's states are sets of its
/// patterns' NFA states, so a bigger set needs far more of them per line.
/// With 1,000 patterns in one set its cache keeps clearing (661 µs p99 per
/// line); in chunks this size, 14 µs.
const CHUNK_PATTERNS: usize = 64;
/// Which chunks hit is a bit mask; past this many chunks they grow instead.
const MAX_CHUNKS: usize = u64::BITS as usize;

#[derive(Clone, Debug)]
pub(crate) struct Matcher {
    /// The patterns with required literals, in id order.
    chunks: Vec<Group>,
    unfiltered: Group,
    prefilter: Option<AhoCorasick>,
    /// For each prefilter literal, the chunks with a pattern that needs it.
    literal_chunks: Vec<u64>,
}

/// Patterns run as one set, and the matcher index of each.
#[derive(Clone, Debug)]
struct Group {
    set: RegexSet,
    ids: Vec<usize>,
}

impl Group {
    fn new(sources: &[&str], ids: Vec<usize>, cache: DfaCache) -> Result<Group, String> {
        let mut builder = RegexSetBuilder::new(ids.iter().map(|&id| sources[id]));
        builder.size_limit(SET_SIZE_LIMIT);
        if cache == DfaCache::Large {
            builder.dfa_size_limit(LARGE_DFA_CACHE);
        }
        let set = builder.build().map_err(|error| error.to_string())?;
        Ok(Group { set, ids })
    }

    fn matching(&self, text: &str, out: &mut Vec<usize>) {
        if self.ids.is_empty() {
            return;
        }
        out.extend(self.set.matches(text).iter().map(|index| self.ids[index]));
    }
}

impl Matcher {
    /// Builds one matcher over the patterns in `sources`; their indexes are
    /// the ids [`Matcher::matching`] reports.
    pub(crate) fn new(sources: &[&str], cache: DfaCache) -> Result<Matcher, String> {
        let mut filtered = Vec::new();
        let mut unfiltered = Vec::new();
        for (id, source) in sources.iter().enumerate() {
            match required_literals(source) {
                Some(literals) => filtered.push((id, literals)),
                None => unfiltered.push(id),
            }
        }

        let chunk_size = CHUNK_PATTERNS.max(filtered.len().div_ceil(MAX_CHUNKS));
        let mut literal_chunks: BTreeMap<Vec<u8>, u64> = BTreeMap::new();
        let mut chunks = Vec::new();
        for (chunk, patterns) in filtered.chunks(chunk_size).enumerate() {
            for literal in patterns.iter().flat_map(|(_, literals)| literals) {
                *literal_chunks.entry(literal.clone()).or_default() |= 1 << chunk;
            }
            chunks.push(Group::new(sources, patterns.iter().map(|(id, _)| *id).collect(), cache)?);
        }
        let prefilter = if literal_chunks.is_empty() {
            None
        } else {
            Some(AhoCorasick::new(literal_chunks.keys()).map_err(|error| error.to_string())?)
        };
        Ok(Matcher {
            chunks,
            unfiltered: Group::new(sources, unfiltered, cache)?,
            prefilter,
            literal_chunks: literal_chunks.into_values().collect(),
        })
    }

    /// Replaces `out` with the ids of the patterns that match `text`, in
    /// ascending order.
    pub(crate) fn matching(&self, text: &str, out: &mut Vec<usize>) {
        out.clear();
        let hit = self.chunks_hit(text);
        for (index, chunk) in self.chunks.iter().enumerate() {
            if hit & (1 << index) != 0 {
                chunk.matching(text, out);
            }
        }
        if !self.unfiltered.ids.is_empty() {
            let filtered = out.len();
            self.unfiltered.matching(text, out);
            if filtered > 0 && out.len() > filtered {
                out.sort_unstable();
            }
        }
    }

    /// The chunks with a pattern whose literal `text` contains, in one scan.
    fn chunks_hit(&self, text: &str) -> u64 {
        let Some(prefilter) = &self.prefilter else {
            return 0;
        };
        let all = u64::MAX >> (u64::BITS as usize - self.chunks.len());
        let mut hit = 0;
        for found in prefilter.find_overlapping_iter(text) {
            hit |= self.literal_chunks[found.pattern().as_usize()];
            if hit == all {
                break;
            }
        }
        hit
    }

    #[cfg(test)]
    fn filtered_count(&self) -> usize {
        self.chunks.iter().map(|chunk| chunk.ids.len()).sum()
    }
}

/// Literals of which every match of `pattern` contains at least one, or
/// `None` if the pattern has no such set (`^\d+$`).
///
/// Every part of a concatenation (and the inside of a group, or of a
/// repetition that must happen at least once) is part of every match, so
/// the prefix literals of any such part will do. The part whose shortest
/// literal is longest is the most selective, and wins.
fn required_literals(pattern: &str) -> Option<Vec<Vec<u8>>> {
    let hir = regex_syntax::parse(pattern).ok()?;
    let mut best: Option<(usize, Seq)> = None;
    consider(&hir, &mut best);
    let (_, seq) = best?;
    Some(seq.literals()?.iter().map(|literal| literal.as_bytes().to_vec()).collect())
}

fn consider(hir: &Hir, best: &mut Option<(usize, Seq)>) {
    let seq = Extractor::new().extract(hir);
    if let Some(shortest) = seq.min_literal_len()
        && shortest > 0
        && best.as_ref().is_none_or(|(length, _)| shortest > *length)
    {
        *best = Some((shortest, seq));
    }
    match hir.kind() {
        HirKind::Concat(parts) => parts.iter().for_each(|part| consider(part, best)),
        HirKind::Capture(capture) => consider(&capture.sub, best),
        HirKind::Repetition(repetition) if repetition.min > 0 => consider(&repetition.sub, best),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn matcher(patterns: &[&str]) -> Matcher {
        Matcher::new(patterns, DfaCache::Default).unwrap()
    }

    fn matching(matcher: &Matcher, text: &str) -> Vec<usize> {
        let mut out = Vec::new();
        matcher.matching(text, &mut out);
        out
    }

    fn literals(pattern: &str) -> Option<Vec<String>> {
        required_literals(pattern)
            .map(|found| found.into_iter().map(|l| String::from_utf8(l).unwrap()).collect())
    }

    #[test]
    fn the_longest_required_literal_is_chosen() {
        assert_eq!(literals(r"^(?<who>\w+) tells you '(?<what>.*)'$"), Some(vec![" tells you '".into()]));
        assert_eq!(literals(r"^You (hit|miss) (?<target>.+)\.$").unwrap().len(), 2);
        assert_eq!(literals(r"(?:north|south)+ gatehouse"), Some(vec![" gatehouse".into()]));
    }

    #[test]
    fn patterns_without_a_required_literal_are_unfiltered() {
        assert_eq!(literals(r"^\d+$"), None);
        assert_eq!(literals(r"^(a|)$"), None, "a branch that can match nothing");
        assert_eq!(literals(r"(?:abc)?\d"), None);
        assert_eq!(matcher(&[r"^\d+$", "gate"]).filtered_count(), 1);
    }

    #[test]
    fn filtered_and_unfiltered_matches_come_back_in_order() {
        let m = matcher(&[r"^\d", "gate", r"\d$", "north"]);
        assert_eq!(matching(&m, "1 north gate 2"), [0, 1, 2, 3]);
        assert_eq!(matching(&m, "north"), [3]);
        assert_eq!(matching(&m, "4"), [0, 2]);
        assert!(matching(&m, "nothing here").is_empty());
    }

    #[test]
    fn patterns_in_different_chunks_match_in_order() {
        let words: Vec<String> = (0..CHUNK_PATTERNS * 3).map(|n| format!("word{n:04}")).collect();
        let mut patterns: Vec<&str> = words.iter().map(String::as_str).collect();
        patterns.push(r"^\d");
        let m = matcher(&patterns);
        assert_eq!(m.chunks.len(), 3);
        let last = CHUNK_PATTERNS * 3 - 1;
        let text = format!("1 word{last:04} word0000 word{:04}", CHUNK_PATTERNS + 1);
        assert_eq!(matching(&m, &text), [0, CHUNK_PATTERNS + 1, last, last + 1]);
        assert_eq!(matching(&m, "word0005"), [5]);
        assert!(matching(&m, "no words").is_empty());
    }

    #[test]
    fn a_literal_hit_that_is_not_a_match_matches_nothing() {
        let m = matcher(&[r"^(?<who>\w+) tells you '(?<what>.*)'$"]);
        assert!(matching(&m, "nobody tells you anything").is_empty());
        assert_eq!(matching(&m, "Ana tells you 'hi'"), [0]);
    }

    #[test]
    fn a_pathological_pattern_runs_in_linear_time() {
        // Catastrophic for a backtracking engine: 2^n paths on a near miss.
        let m = matcher(&[r"^(a+)+$", r"(x+x+)+y", r"^(a|aa)+$"]);
        let time = |n: usize| {
            let text = format!("{}!{} y", "a".repeat(n / 2), "x".repeat(n / 2));
            let started = Instant::now();
            for _ in 0..10 {
                assert!(matching(&m, &text).is_empty());
            }
            started.elapsed()
        };
        time(1_000);
        let (short, long) = (time(10_000), time(100_000));
        // Ten times the text may cost about ten times the time, never 2^n.
        assert!(long < short * 40 + Duration::from_millis(5), "{short:?} → {long:?}");
    }
}
