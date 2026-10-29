//! The line pipeline: server text bytes in, finished [`Line`]s out.
//!
//! Every reader (the scrollback, each plugin's extension point) gets the same
//! [`Line`], so the work is done once, in one streaming pass:
//!
//! - Each byte is looked at once to find line ends (`memchr`).
//! - The bytes between them are decoded once (UTF-8, or Latin-1 when a game
//!   sets it) and appended to the line in progress.
//! - Only the newly decoded text is fed to the [`AnsiParser`], which strips
//!   and styles it as it arrives. Nothing is parsed a second time: the
//!   pending partial line is a view of these same buffers, and finishing a
//!   line only copies them into the shared [`Line`].
//!
//! A line ends at `\n`, or at GA/EOR ([`LinePipeline::prompt_end`]). Every
//! `\r` is dropped, as the prototype did, so `\r\n`, `\n\r` and `\r\r\n` each
//! end one line and `a\rb` stays one line. Text with no ending yet is the
//! pending partial line, often a prompt; see [`LinePipeline::pending`]. It
//! can't grow past [`MAX_LINE_BYTES`].

use crate::ansi::{AnsiParser, Span};
use std::sync::Arc;

/// The most server bytes one line may hold. A server that sends more
/// without a newline gets its line cut, so it can't grow memory without
/// limit.
pub const MAX_LINE_BYTES: usize = 64 * 1024;

/// How server bytes become text. A game plugin picks it per connection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Encoding {
    /// UTF-8; bytes that aren't valid become U+FFFD.
    #[default]
    Utf8,
    /// ISO 8859-1: every byte is the code point of the same value.
    Latin1,
}

/// What finished a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineEnd {
    Newline,
    /// GA or EOR from the server, or a reader that recognized a prompt.
    PromptEnd,
    /// The line reached [`MAX_LINE_BYTES`]; its text continues on the next.
    Cut,
}

/// A link or action over a byte range of a line's plain text. Nothing
/// produces these yet; MXP and plugins will.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub start: u32,
    pub end: u32,
    /// The command to send, or the URL to open.
    pub action: Box<str>,
}

/// One finished server line. Cloning shares it, so the scrollback, plugins
/// and other threads all hold the same allocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line(Arc<LineData>);

#[derive(Debug, PartialEq, Eq)]
struct LineData {
    /// The raw text, then the plain text; just one copy when they're equal.
    text: Box<str>,
    raw_end: u32,
    plain_start: u32,
    styled: Box<[Span]>,
    links: Box<[Link]>,
    end: LineEnd,
}

impl Line {
    fn new(raw: &str, plain: &str, styled: &[Span], end: LineEnd) -> Self {
        let (text, plain_start) = if raw == plain {
            (Box::from(raw), 0)
        } else {
            let mut text = String::with_capacity(raw.len() + plain.len());
            text.push_str(raw);
            text.push_str(plain);
            (text.into_boxed_str(), raw.len())
        };
        Line(Arc::new(LineData {
            text,
            raw_end: raw.len() as u32,
            plain_start: plain_start as u32,
            styled: styled.into(),
            links: Box::default(),
            end,
        }))
    }

    /// The decoded text as the server sent it, ANSI codes included, without
    /// the line ending.
    pub fn raw(&self) -> &str {
        &self.0.text[..self.0.raw_end as usize]
    }

    /// The text without ANSI codes or control characters: what readers,
    /// patterns, find and screen readers see.
    pub fn plain(&self) -> &str {
        &self.0.text[self.0.plain_start as usize..]
    }

    /// Styled runs, as byte ranges of [`Line::plain`]. Unstyled text has none.
    pub fn styled(&self) -> &[Span] {
        &self.0.styled
    }

    pub fn links(&self) -> &[Link] {
        &self.0.links
    }

    pub fn end(&self) -> LineEnd {
        self.0.end
    }
}

/// The line still arriving: everything received since the last line ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pending<'a> {
    pub raw: &'a str,
    pub plain: &'a str,
    pub styled: &'a [Span],
    /// Which line this is: the count of lines finished before it. A pending
    /// line keeps its number while it grows, so a reader can tell half a
    /// prompt split by the network from a new one.
    pub number: u64,
}

/// Turns one connection's server text into lines. Feed it the bytes of each
/// telnet data event in order, and call [`LinePipeline::prompt_end`] for
/// GA/EOR.
#[derive(Debug, Default)]
pub struct LinePipeline {
    encoding: Encoding,
    ansi: AnsiParser,
    raw: String,
    plain: String,
    styled: Vec<Span>,
    /// The start of a UTF-8 code point split from its end by a chunk boundary.
    utf8_tail: Vec<u8>,
    /// Server bytes in the line in progress, `\r` aside.
    line_bytes: usize,
    /// A prompt end already ended the line the next `\n` would end.
    prompt_ended_line: bool,
    finished: u64,
}

impl LinePipeline {
    pub fn new(encoding: Encoding) -> Self {
        LinePipeline { encoding, ..LinePipeline::default() }
    }

    /// Changes the encoding from the next byte on. A UTF-8 code point still
    /// waiting for its end becomes U+FFFD where it stands.
    pub fn set_encoding(&mut self, encoding: Encoding) {
        if encoding != Encoding::Utf8 {
            self.flush_utf8_tail();
        }
        self.encoding = encoding;
    }

    /// Reads the next server bytes, appending each line they finish to
    /// `lines`. Bytes may be cut anywhere, even inside an escape sequence or
    /// a UTF-8 code point.
    pub fn push(&mut self, bytes: &[u8], lines: &mut Vec<Line>) {
        let mut rest = bytes;
        while let Some(at) = memchr::memchr2(b'\n', b'\r', rest) {
            self.append(&rest[..at], lines);
            if rest[at] == b'\n' && !std::mem::take(&mut self.prompt_ended_line) {
                lines.push(self.finish(LineEnd::Newline));
            }
            rest = &rest[at + 1..];
        }
        self.append(rest, lines);
    }

    /// The server marked a prompt (GA or EOR): the pending text is a whole
    /// line. A reader that recognizes the pending text as a prompt (servers
    /// that send no GA) calls this too. Returns `None` if nothing is pending.
    pub fn prompt_end(&mut self) -> Option<Line> {
        if self.raw.is_empty() && self.utf8_tail.is_empty() {
            return None;
        }
        let line = self.finish(LineEnd::PromptEnd);
        self.prompt_ended_line = true;
        Some(line)
    }

    /// The text received since the last line ended, if any. It may be a
    /// prompt, or only the first part of a line or prompt the network split:
    /// it can still grow, and it isn't a [`Line`] until it ends.
    pub fn pending(&self) -> Option<Pending<'_>> {
        (!self.raw.is_empty()).then_some(Pending {
            raw: &self.raw,
            plain: &self.plain,
            styled: &self.styled,
            number: self.finished,
        })
    }

    /// Adds text bytes (no line ends) to the line in progress, cutting it
    /// into `lines` each time it reaches [`MAX_LINE_BYTES`].
    fn append(&mut self, mut bytes: &[u8], lines: &mut Vec<Line>) {
        if bytes.is_empty() {
            return;
        }
        self.prompt_ended_line = false;
        while self.line_bytes + bytes.len() > MAX_LINE_BYTES {
            match self.cut_point(bytes, MAX_LINE_BYTES - self.line_bytes) {
                Some(cut) => {
                    self.decode(&bytes[..cut]);
                    lines.push(self.finish(LineEnd::Cut));
                    bytes = &bytes[cut..];
                }
                None => {
                    let held = std::mem::take(&mut self.utf8_tail);
                    lines.push(self.finish(LineEnd::Cut));
                    self.line_bytes = held.len();
                    self.utf8_tail = held;
                }
            }
        }
        self.decode(bytes);
    }

    /// Where to cut `bytes` to fit `room`: before the code point that would
    /// cross it, so a cut never splits one. `None` cuts before the code point
    /// whose start is held from the last chunk, which then opens the next line.
    fn cut_point(&self, bytes: &[u8], room: usize) -> Option<usize> {
        let is_continuation = |byte: u8| byte & 0xC0 == 0x80;
        match self.encoding {
            Encoding::Latin1 => Some(room),
            Encoding::Utf8 => (room.saturating_sub(3)..=room)
                .rev()
                .find(|&at| !is_continuation(bytes[at]))
                .or((self.utf8_tail.is_empty() || room >= 3).then_some(room)),
        }
    }

    /// Decodes `bytes` onto the line in progress and parses only that text.
    fn decode(&mut self, bytes: &[u8]) {
        self.line_bytes += bytes.len();
        let start = self.raw.len();
        match self.encoding {
            Encoding::Utf8 if self.utf8_tail.is_empty() => self.decode_utf8(bytes),
            Encoding::Utf8 => {
                let mut joined = std::mem::take(&mut self.utf8_tail);
                joined.extend_from_slice(bytes);
                self.decode_utf8(&joined);
            }
            Encoding::Latin1 => self.raw.extend(bytes.iter().map(|&byte| char::from(byte))),
        }
        self.ansi.feed(&self.raw[start..], &mut self.plain, &mut self.styled);
    }

    /// Decodes as `String::from_utf8_lossy` does, except that a code point
    /// cut off at the end is held until the next bytes arrive.
    fn decode_utf8(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.utf8_chunks().peekable();
        while let Some(chunk) = chunks.next() {
            self.raw.push_str(chunk.valid());
            let invalid = chunk.invalid();
            if invalid.is_empty() {
                continue;
            }
            let cut_off = chunks.peek().is_none()
                && std::str::from_utf8(invalid).is_err_and(|error| error.error_len().is_none());
            if cut_off {
                self.utf8_tail.extend_from_slice(invalid);
            } else {
                self.raw.push(char::REPLACEMENT_CHARACTER);
            }
        }
    }

    /// Replaces a held, never-finished UTF-8 code point with U+FFFD.
    fn flush_utf8_tail(&mut self) {
        if self.utf8_tail.is_empty() {
            return;
        }
        self.utf8_tail.clear();
        let start = self.raw.len();
        self.raw.push(char::REPLACEMENT_CHARACTER);
        self.ansi.feed(&self.raw[start..], &mut self.plain, &mut self.styled);
    }

    fn finish(&mut self, end: LineEnd) -> Line {
        self.flush_utf8_tail();
        self.ansi.end_line();
        let line = Line::new(&self.raw, &self.plain, &self.styled, end);
        self.raw.clear();
        self.plain.clear();
        self.styled.clear();
        self.line_bytes = 0;
        self.finished += 1;
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ansi::{Color, Style};

    /// Feeds each chunk in turn; returns the finished lines' plain text and
    /// the pending plain text.
    fn run(pipeline: &mut LinePipeline, chunks: &[&[u8]]) -> (Vec<String>, Option<String>) {
        let mut lines = Vec::new();
        for chunk in chunks {
            pipeline.push(chunk, &mut lines);
        }
        let plain = lines.iter().map(|line| line.plain().to_owned()).collect();
        (plain, pipeline.pending().map(|pending| pending.plain.to_owned()))
    }

    fn plain_lines(chunks: &[&[u8]]) -> (Vec<String>, Option<String>) {
        run(&mut LinePipeline::default(), chunks)
    }

    #[test]
    fn every_line_ending_variant() {
        let (lines, pending) = plain_lines(&[b"a\r\nb\n\rc\nd\r\r\ne\r\n\r\nf\n\r\n\rg"]);
        assert_eq!(lines, ["a", "b", "c", "d", "e", "", "f", ""]);
        assert_eq!(pending.as_deref(), Some("g"));
    }

    #[test]
    fn a_lone_carriage_return_is_dropped() {
        let (lines, _) = lines_and_pending(&[b"a\rb\r\n"]);
        assert_eq!((lines.len(), lines[0].raw(), lines[0].plain()), (1, "ab", "ab"));
    }

    #[test]
    fn any_run_of_carriage_returns_before_a_newline_ends_one_line() {
        assert_eq!(plain_lines(&[b"a\r\r\nb\r\r\r\r\nc\n"]).0, ["a", "b", "c"]);
        assert_eq!(plain_lines(&[b"a\r", b"\r", b"\n"]).0, ["a"]);
    }

    #[test]
    fn a_pair_split_between_chunks_is_still_one_ending() {
        assert_eq!(plain_lines(&[b"a\n", b"\rb\r", b"\nc"]).0, ["a", "b"]);
    }

    /// Synthetic text mixing every ending, styles, an OSC title, multi-byte
    /// UTF-8 and an invalid byte.
    const STREAM: &[u8] = b"\x1b[1;31mWelcome\x1b[0m to Testland.\n\r\
        A caf\xc3\xa9 \xe4\xb8\xad sign \xf0\x9f\x8f\xb0 glows.\r\n\
        \x1b]0;title\x07Bad \xff byte\r\r\n\
        \x1b[38;5;208morange\x1b[m\r line\n\
        \n\r\
        (10h 20m) \x1b[32mprompt";

    /// The finished lines, then the pending text finished as a prompt.
    fn lines_and_pending(chunks: &[&[u8]]) -> (Vec<Line>, Option<Line>) {
        let mut pipeline = LinePipeline::default();
        let mut lines = Vec::new();
        for chunk in chunks {
            pipeline.push(chunk, &mut lines);
        }
        (lines, pipeline.prompt_end())
    }

    #[test]
    fn the_whole_stream_at_once() {
        let (lines, pending) = lines_and_pending(&[STREAM]);
        let plain: Vec<&str> = lines.iter().map(Line::plain).collect();
        assert_eq!(
            plain,
            [
                "Welcome to Testland.",
                "A café 中 sign 🏰 glows.",
                "Bad \u{FFFD} byte",
                "orange line",
                "",
            ]
        );
        assert_eq!(lines[0].raw(), "\x1b[1;31mWelcome\x1b[0m to Testland.");
        assert_eq!(pending.unwrap().plain(), "(10h 20m) prompt");
    }

    #[test]
    fn a_split_at_any_byte_changes_nothing() {
        let whole = lines_and_pending(&[STREAM]);
        for split in 0..=STREAM.len() {
            let (head, tail) = STREAM.split_at(split);
            assert_eq!(lines_and_pending(&[head, tail]), whole, "split at byte {split}");
        }
        let bytes: Vec<&[u8]> = STREAM.chunks(1).collect();
        assert_eq!(lines_and_pending(&bytes), whole, "one byte at a time");
    }

    #[test]
    fn styles_are_spans_of_the_plain_text() {
        let (lines, pending) = lines_and_pending(&[STREAM]);
        let bold_red = Style { fg: Some(Color(1)), bg: None, bold: true };
        assert_eq!(lines[0].styled(), [Span { start: 0, end: 7, style: bold_red }]);
        assert!(lines[1].styled().is_empty());
        assert!(lines[0].links().is_empty());
        let green = Style { fg: Some(Color(2)), ..Style::default() };
        assert_eq!(pending.unwrap().styled(), [Span { start: 10, end: 16, style: green }]);
    }

    #[test]
    fn unstyled_lines_keep_one_copy_of_their_text() {
        let (lines, _) = lines_and_pending(&[b"plain\n\x1b[1mbold\n"]);
        assert_eq!(lines[0].plain().as_ptr(), lines[0].raw().as_ptr());
        assert_eq!((lines[1].raw(), lines[1].plain()), ("\x1b[1mbold", "bold"));
    }

    #[test]
    fn a_prompt_without_a_newline_waits_for_more_text() {
        let mut pipeline = LinePipeline::default();
        let (lines, pending) = run(&mut pipeline, &[b"(10h 20m) "]);
        assert!(lines.is_empty());
        assert_eq!(pending.as_deref(), Some("(10h 20m) "));
        let (lines, pending) = run(&mut pipeline, &[b"You see a lantern.\r\n"]);
        assert_eq!(lines, ["(10h 20m) You see a lantern."]);
        assert_eq!(pending, None);
    }

    #[test]
    fn half_a_prompt_split_by_the_network_stays_one_growing_line() {
        let mut pipeline = LinePipeline::default();
        let mut lines = Vec::new();
        pipeline.push(b"<100hp 5", &mut lines);
        let half = pipeline.pending().unwrap();
        assert_eq!((half.plain, half.number), ("<100hp 5", 0));
        pipeline.push(b"0mp> ", &mut lines);
        let whole = pipeline.pending().unwrap();
        assert_eq!((whole.plain, whole.number), ("<100hp 50mp> ", 0));
        assert!(lines.is_empty());
    }

    #[test]
    fn prompt_end_finishes_the_pending_line() {
        let mut pipeline = LinePipeline::default();
        let mut lines = Vec::new();
        pipeline.push(b"\x1b[32m(10h)\x1b[0m ", &mut lines);
        let prompt = pipeline.prompt_end().unwrap();
        assert_eq!((prompt.plain(), prompt.end()), ("(10h) ", LineEnd::PromptEnd));
        assert_eq!(pipeline.pending(), None);
        assert_eq!(pipeline.prompt_end(), None);
    }

    #[test]
    fn the_newline_after_a_prompt_end_is_not_a_blank_line() {
        let mut pipeline = LinePipeline::default();
        pipeline.push(b"(10h) ", &mut Vec::new());
        pipeline.prompt_end();
        assert_eq!(run(&mut pipeline, &[b"\n", b"\rText\n"]).0, ["Text"]);

        pipeline.push(b"(10h) ", &mut Vec::new());
        pipeline.prompt_end();
        assert_eq!(run(&mut pipeline, &[b"\r\n\r\nText\n"]).0, ["", "Text"]);
    }

    #[test]
    fn a_prompt_end_with_nothing_pending_changes_nothing() {
        let mut pipeline = LinePipeline::default();
        let (lines, _) = run(&mut pipeline, &[b"(10h)\n"]);
        assert_eq!(lines, ["(10h)"]);
        assert_eq!(pipeline.prompt_end(), None);
        assert_eq!(run(&mut pipeline, &[b"\nText\n"]).0, ["", "Text"]);
    }

    #[test]
    fn latin1_maps_each_byte_to_one_character() {
        let bytes: &[u8] = b"Caf\xe9 \xa9 \xc3\xa9\n";
        assert_eq!(run(&mut LinePipeline::new(Encoding::Latin1), &[bytes]).0, ["Café © Ã©"]);
        assert_eq!(run(&mut LinePipeline::new(Encoding::Utf8), &[bytes]).0, ["Caf\u{FFFD} \u{FFFD} é"]);
    }

    #[test]
    fn the_encoding_can_change_between_chunks() {
        let mut pipeline = LinePipeline::default();
        let mut lines = Vec::new();
        pipeline.push(b"\xc3\xa9 ", &mut lines);
        pipeline.set_encoding(Encoding::Latin1);
        pipeline.push(b"\xe9\n", &mut lines);
        assert_eq!(lines[0].plain(), "é é");
    }

    #[test]
    fn a_code_point_cut_off_by_a_line_end_is_replaced() {
        assert_eq!(plain_lines(&[b"a\xe4\xb8", b"\nb"]).0, ["a\u{FFFD}"]);
    }

    #[test]
    fn a_line_without_end_is_cut_at_the_cap() {
        let mut pipeline = LinePipeline::default();
        let mut lines = Vec::new();
        for _ in 0..200 {
            pipeline.push(&[b'x'; 1000], &mut lines);
            assert!(pipeline.pending().is_none_or(|p| p.raw.len() <= MAX_LINE_BYTES));
        }
        pipeline.push(b"\n", &mut lines);
        let lengths: Vec<(usize, LineEnd)> = lines.iter().map(|l| (l.raw().len(), l.end())).collect();
        let full = (MAX_LINE_BYTES, LineEnd::Cut);
        assert_eq!(lengths, [full, full, full, (200_000 - 3 * MAX_LINE_BYTES, LineEnd::Newline)]);
    }

    #[test]
    fn a_cut_never_splits_a_code_point() {
        // "a" then two-byte "é"s: the cap falls inside an "é".
        let mut bytes = b"a".to_vec();
        bytes.extend("é".repeat(MAX_LINE_BYTES).as_bytes());
        let (lines, pending) = lines_and_pending(&[&bytes]);
        assert_eq!(lines[0].raw().len(), MAX_LINE_BYTES - 1);
        assert!(lines.iter().chain(&pending).all(|line| !line.raw().contains('\u{FFFD}')));
        let text: String = lines.iter().chain(&pending).map(Line::raw).collect();
        assert_eq!(text.as_bytes(), bytes);
    }

    #[test]
    fn a_cut_never_splits_a_code_point_that_straddles_two_chunks() {
        for character in ["é", "中", "🏰"] {
            let encoded = character.as_bytes();
            // The code point's first byte fills the line; the rest arrives next.
            let mut head = vec![b'a'; MAX_LINE_BYTES - 1];
            head.push(encoded[0]);
            let mut tail = encoded[1..].to_vec();
            tail.push(b'b');
            let (lines, pending) = lines_and_pending(&[&head, &tail]);
            assert_eq!(lines.len(), 1, "{character}");
            assert_eq!(lines[0].raw(), "a".repeat(MAX_LINE_BYTES - 1), "{character}");
            assert_eq!(pending.unwrap().raw(), format!("{character}b"));
        }
    }

    #[test]
    fn switching_encoding_mid_code_point_replaces_it_in_place() {
        let mut pipeline = LinePipeline::default();
        let mut lines = Vec::new();
        pipeline.push(b"a\xc3", &mut lines);
        pipeline.set_encoding(Encoding::Latin1);
        pipeline.push(b"b\n", &mut lines);
        assert_eq!(lines[0].plain(), "a\u{FFFD}b");
    }

    #[test]
    fn lines_are_numbered_in_order() {
        let mut pipeline = LinePipeline::default();
        pipeline.push(b"one\ntwo\nthr", &mut Vec::new());
        assert_eq!(pipeline.pending().map(|p| p.number), Some(2));
    }

    #[test]
    fn lines_can_cross_threads_and_clones_share_one_copy() {
        fn shareable<T: Send + Sync>() {}
        shareable::<Line>();
        let (lines, _) = lines_and_pending(&[b"shared\n"]);
        let copy = lines[0].clone();
        assert_eq!(copy.plain().as_ptr(), lines[0].plain().as_ptr());
    }
}
