//! The chat's scrollback: a fixed-capacity ring buffer of finished lines.
//!
//! The buffer is allocated once at its capacity, so an append never
//! reallocates or moves other lines. Once full, each append drops the oldest
//! line. A view addresses lines by index, so trimming never re-lays anything
//! out; [`Scrollback::evicted`] lets a scrolled-up view keep its place.

use crate::line::Line;
use aho_corasick::AhoCorasick;
use std::collections::VecDeque;

#[derive(Clone, Debug)]
pub struct Scrollback {
    lines: VecDeque<Line>,
    capacity: usize,
    evicted: u64,
}

impl Scrollback {
    pub fn with_capacity(capacity: usize) -> Self {
        assert!(capacity > 0, "a scrollback must hold at least one line");
        Scrollback {
            lines: VecDeque::with_capacity(capacity),
            capacity,
            evicted: 0,
        }
    }

    pub fn push(&mut self, line: Line) {
        if self.lines.len() == self.capacity {
            self.lines.pop_front();
            self.evicted += 1;
        }
        self.lines.push_back(line);
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The line at `index`, where 0 is the oldest line still held.
    pub fn get(&self, index: usize) -> Option<&Line> {
        self.lines.get(index)
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &Line> + ExactSizeIterator {
        self.lines.iter()
    }

    /// How many lines have been trimmed from the front since creation.
    pub fn evicted(&self) -> u64 {
        self.evicted
    }

    /// Indexes of up to `limit` lines containing `needle`, ignoring ASCII
    /// case, newest first. The search reads each line's plain text in place
    /// and stops at `limit`, so the first page of results comes quickly.
    /// Unless `prompts_shown`, the prompts the HUD repeats aren't read: only
    /// what shows of a line can be found.
    pub fn find(&self, needle: &str, limit: usize, prompts_shown: bool) -> Vec<usize> {
        if needle.is_empty() {
            return Vec::new();
        }
        let matcher = AhoCorasick::builder()
            .ascii_case_insensitive(true)
            .build([needle])
            .expect("a single literal pattern always builds");
        self.lines
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, line)| matcher.is_match(line.shown_text(prompts_shown)))
            .map(|(index, _)| index)
            .take(limit)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::line::LinePipeline;

    fn scrollback_of(capacity: usize, texts: &[&str]) -> Scrollback {
        let mut pipeline = LinePipeline::default();
        let mut lines = Vec::new();
        let mut scrollback = Scrollback::with_capacity(capacity);
        for text in texts {
            pipeline.push_line(text.as_bytes(), &mut lines);
        }
        lines.into_iter().for_each(|line| scrollback.push(line));
        scrollback
    }

    fn texts(scrollback: &Scrollback) -> Vec<&str> {
        scrollback.iter().map(Line::plain).collect()
    }

    #[test]
    fn keeps_the_newest_lines_once_full() {
        let scrollback = scrollback_of(3, &["a", "b", "c", "d", "e"]);
        assert_eq!(texts(&scrollback), ["c", "d", "e"]);
        assert_eq!(scrollback.evicted(), 2);
        assert_eq!(scrollback.get(0).map(Line::plain), Some("c"));
    }

    #[test]
    fn never_grows_past_its_capacity() {
        let mut scrollback = scrollback_of(2, &[]);
        let allocated = scrollback.lines.capacity();
        for _ in 0..10 {
            scrollback.push(Line::command("x"));
        }
        assert_eq!(scrollback.len(), 2);
        assert_eq!(scrollback.lines.capacity(), allocated);
    }

    #[test]
    fn find_ignores_case_and_matches_plain_text_only() {
        let scrollback = scrollback_of(10, &["A Goblin waits", "\x1b[31mgob\x1b[0mlin", "nothing"]);
        assert_eq!(scrollback.find("GOBLIN", 10, true), [1, 0]);
        assert!(scrollback.find("", 10, true).is_empty());
    }

    #[test]
    fn find_returns_the_newest_matches_up_to_the_limit() {
        let scrollback = scrollback_of(10, &["hit 1", "miss", "hit 2", "hit 3"]);
        assert_eq!(scrollback.find("hit", 2, true), [3, 2]);
    }

    #[test]
    fn find_reads_a_hidden_prompt_only_once_prompts_show() {
        let mut scrollback = scrollback_of(10, &[]);
        let plain = scrollback_of(10, &["<9hp> ", "<9hp> You rest.", "a 9hp potion"]);
        let lines: Vec<&Line> = plain.iter().collect();
        scrollback.push(lines[0].with_prompt(6));
        scrollback.push(lines[1].with_prompt(6));
        scrollback.push(lines[2].clone());

        assert_eq!(scrollback.find("9hp", 10, true), [2, 1, 0]);
        assert_eq!(scrollback.find("9hp", 10, false), [2]);
        assert_eq!(scrollback.find("rest", 10, false), [1]);
    }
}
