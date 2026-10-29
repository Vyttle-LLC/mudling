//! The chat's scrollback: a fixed-capacity ring buffer of parsed lines.
//!
//! The buffer is allocated once at its capacity, so an append never
//! reallocates or moves other lines. Once full, each append drops the oldest
//! line. A view addresses lines by index, so trimming never re-lays anything
//! out; [`Scrollback::evicted`] lets a scrolled-up view keep its place.

use crate::ansi::Line;
use aho_corasick::AhoCorasick;
use std::collections::VecDeque;

#[derive(Debug)]
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

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// The line at `index`, where 0 is the oldest line still held.
    pub fn get(&self, index: usize) -> Option<&Line> {
        self.lines.get(index)
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Line> {
        self.lines.iter()
    }

    /// How many lines have been trimmed from the front since creation.
    pub fn evicted(&self) -> u64 {
        self.evicted
    }

    /// Indexes of the lines containing `needle`, ignoring ASCII case.
    pub fn find(&self, needle: &str) -> Vec<usize> {
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
            .filter(|(_, line)| matcher.is_match(&*line.text))
            .map(|(index, _)| index)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ansi::AnsiParser;

    fn scrollback_of(capacity: usize, texts: &[&str]) -> Scrollback {
        let mut parser = AnsiParser::default();
        let mut scrollback = Scrollback::with_capacity(capacity);
        for text in texts {
            scrollback.push(parser.parse_line(text));
        }
        scrollback
    }

    fn texts(scrollback: &Scrollback) -> Vec<&str> {
        scrollback.iter().map(|line| &*line.text).collect()
    }

    #[test]
    fn keeps_the_newest_lines_once_full() {
        let scrollback = scrollback_of(3, &["a", "b", "c", "d", "e"]);
        assert_eq!(texts(&scrollback), ["c", "d", "e"]);
        assert_eq!(scrollback.evicted(), 2);
        assert_eq!(scrollback.get(0).map(|l| &*l.text), Some("c"));
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
        assert_eq!(scrollback.find("GOBLIN"), [0, 1]);
        assert!(scrollback.find("").is_empty());
    }
}
