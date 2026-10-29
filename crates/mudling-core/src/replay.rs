//! Replays session logs recorded by the Swift prototype (MUDPad).
//!
//! The prototype's `SessionLogger` writes the server's text as it arrives,
//! and interleaves its own notes on separate lines, each written as
//! `"\n[TAG] …\n"` (only `[connected]` lacks the leading newline). Player
//! input is never logged. Replay keeps the server text and drops the notes,
//! including the blank line a note's leading newline adds after a finished
//! line.
//!
//! Logs hold other players' chat, so they are only ever read at runtime from
//! a path the user gives; none are committed.

use std::io;
use std::path::Path;

/// Notes written by the prototype, not by the server.
const NOTE_PREFIXES: [&str; 5] = ["[GMCP] ", "[telnet] ", "[auto-login] ", "[disconnected] ", "[MAP] "];
const CONNECTED_NOTE: &str = "[connected]";

/// The server text of one session log, one entry per line.
pub fn server_lines(log: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut pending_blanks = 0usize;
    for line in log.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            pending_blanks += 1;
        } else if line == CONNECTED_NOTE {
            continue;
        } else if NOTE_PREFIXES.iter().any(|prefix| line.starts_with(prefix)) {
            pending_blanks = pending_blanks.saturating_sub(1);
        } else {
            lines.extend(std::iter::repeat_n("", pending_blanks));
            pending_blanks = 0;
            lines.push(line);
        }
    }
    lines.extend(std::iter::repeat_n("", pending_blanks));
    lines
}

/// Reads each log and concatenates their server text, in the order given.
/// Bytes that aren't UTF-8 are replaced, as the prototype's decoder did.
pub fn load_logs(paths: &[impl AsRef<Path>]) -> io::Result<Vec<String>> {
    let mut lines = Vec::new();
    for path in paths {
        let bytes = std::fs::read(path)?;
        let log = String::from_utf8_lossy(&bytes);
        lines.extend(server_lines(&log).into_iter().map(str::to_owned));
    }
    Ok(lines)
}

/// Hands out replayed lines in order, starting again from the first line once
/// all have been used, so a stream can outlast its logs.
#[derive(Debug)]
pub struct ReplayFeed {
    lines: Vec<String>,
    next: usize,
}

impl ReplayFeed {
    pub fn new(lines: Vec<String>) -> Self {
        assert!(!lines.is_empty(), "a replay needs at least one line");
        ReplayFeed { lines, next: 0 }
    }

    pub fn source_len(&self) -> usize {
        self.lines.len()
    }

    pub fn next_line(&mut self) -> &str {
        let line = &self.lines[self.next];
        self.next = (self.next + 1) % self.lines.len();
        line
    }
}

/// Deterministic, MUD-shaped lines with ANSI color, for benchmarks and runs
/// on machines without session logs. Entirely made up: no real chat.
pub fn synthetic_lines(count: usize) -> Vec<String> {
    const WORDS: [&str; 24] = [
        "a", "the", "goblin", "sword", "north", "south", "east", "west", "you", "see", "here",
        "torch", "glade", "path", "leads", "quiet", "stone", "river", "guard", "says", "hello",
        "attack", "misses", "hits",
    ];
    const COLORS: [&str; 6] = ["\x1b[31m", "\x1b[1;32m", "\x1b[33m", "\x1b[36m", "\x1b[38;5;208m", "\x1b[0m"];
    // xorshift64: a fixed seed keeps every run identical.
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move |bound: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % bound as u64) as usize
    };
    (0..count)
        .map(|_| {
            let mut line = String::new();
            for word in 0..4 + next(12) {
                if word > 0 {
                    line.push(' ');
                }
                if next(5) == 0 {
                    line.push_str(COLORS[next(COLORS.len())]);
                }
                line.push_str(WORDS[next(WORDS.len())]);
            }
            line
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_server_text_and_drops_notes() {
        // Synthetic: shaped like SessionLogger's output.
        let log = "[connected]\nWelcome to Testland.\r\n\n[telnet] verb 251 option 201\n\
                   By what name are you known? \n[auto-login] answered prompt, now password\n\
                   You stand in a quiet glade.\n\n[GMCP] Room.Info {\"num\":1}\n\n[MAP] move east\n\
                   \nA path leads east.\n<20hp 30mv> \n[GMCP] Char.Vitals {}\n";
        assert_eq!(
            server_lines(log),
            [
                "Welcome to Testland.",
                "By what name are you known? ",
                "You stand in a quiet glade.",
                "",
                "A path leads east.",
                "<20hp 30mv> ",
            ]
        );
    }

    #[test]
    fn feed_starts_over_when_its_lines_run_out() {
        let mut feed = ReplayFeed::new(vec!["a".into(), "b".into()]);
        let taken: Vec<String> = (0..3).map(|_| feed.next_line().to_owned()).collect();
        assert_eq!(taken, ["a", "b", "a"]);
    }
}
