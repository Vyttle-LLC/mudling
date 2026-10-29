//! Turns raw server lines into plain text plus style spans, once per line.
//!
//! Only SGR (`ESC [ … m`) is interpreted: the 16 basic colors, the 256-color
//! palette and bold. Every other escape sequence is removed from the text.
//!
//! The parser streams: [`AnsiParser::feed`] takes a line in as many pieces as
//! it arrives in, and an escape sequence may be split between pieces. The
//! line pipeline ([`crate::line`]) feeds it each byte of a line exactly once;
//! [`AnsiParser::parse_line`] is the same pass over a whole line.

/// A palette index: 0–15 are the basic and bright colors, 16–255 the
/// 256-color cube and grays. The shell maps indexes to its theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Color(pub u8);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
}

impl Style {
    pub fn is_plain(&self) -> bool {
        *self == Style::default()
    }
}

/// A byte range of a line's plain text drawn in a non-default style.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    pub style: Style,
}

/// Who a line came from, so the chat can tell them apart by color.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LineKind {
    #[default]
    Server,
    /// A command the player sent, echoed into the chat.
    Command,
}

/// One chat line: its plain text (what readers, find and screen readers
/// see) and the spans that style it. Unstyled text has no spans.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub text: Box<str>,
    pub spans: Box<[Span]>,
    pub kind: LineKind,
}

impl Line {
    pub fn command(text: &str) -> Self {
        Line {
            text: text.into(),
            spans: Box::default(),
            kind: LineKind::Command,
        }
    }
}

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const INTERMEDIATE: std::ops::RangeInclusive<u8> = 0x20..=0x2f;
const ESCAPE_FINAL: std::ops::RangeInclusive<u8> = 0x30..=0x7e;
const CSI_FINAL: std::ops::RangeInclusive<u8> = 0x40..=0x7e;
/// Real SGR parameters are a few dozen bytes; longer ones aren't kept.
const MAX_CSI_PARAMS: usize = 256;

/// Where the parser is between pieces of a line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Escape {
    #[default]
    None,
    /// Just after `ESC`.
    Start,
    /// After `ESC` and intermediate bytes (as in `ESC ( B`), until the
    /// final byte.
    Intermediate,
    /// Inside `ESC [`, collecting parameters until the final byte.
    Csi,
    /// Inside a CSI too long to be real, skipped until its final byte.
    CsiTooLong,
    /// Inside `ESC ]`, until `BEL` or `ESC \`.
    Osc,
    /// An `ESC` inside an OSC: `\` ends it, anything else starts a new escape.
    OscEsc,
}

/// Parses lines in order. The current style carries from one line to the
/// next, as servers expect.
#[derive(Debug, Default)]
pub struct AnsiParser {
    style: Style,
    escape: Escape,
    /// CSI parameters seen so far, when a sequence is split between pieces.
    csi: String,
}

impl AnsiParser {
    pub fn parse_line(&mut self, raw: &str) -> Line {
        let mut text = String::with_capacity(raw.len());
        let mut spans = Vec::new();
        self.feed(raw, &mut text, &mut spans);
        self.end_line();
        Line {
            text: text.into_boxed_str(),
            spans: spans.into_boxed_slice(),
            kind: LineKind::Server,
        }
    }

    /// Parses the next piece of the current line, appending its plain text
    /// to `plain` and its styled runs to `spans` (ranges of `plain`).
    pub fn feed(&mut self, piece: &str, plain: &mut String, spans: &mut Vec<Span>) {
        let mut rest = piece;
        while !rest.is_empty() {
            let bytes = rest.as_bytes();
            rest = match self.escape {
                Escape::None => match memchr::memchr(ESC, bytes) {
                    Some(esc) => {
                        self.push_text(plain, spans, &rest[..esc]);
                        self.escape = Escape::Start;
                        &rest[esc + 1..]
                    }
                    None => {
                        self.push_text(plain, spans, rest);
                        ""
                    }
                },
                Escape::Start => {
                    let next = rest.chars().next().expect("rest isn't empty");
                    self.escape = match next {
                        '[' => Escape::Csi,
                        ']' => Escape::Osc,
                        c if c.is_ascii() && INTERMEDIATE.contains(&(c as u8)) => Escape::Intermediate,
                        _ => Escape::None,
                    };
                    &rest[next.len_utf8()..]
                }
                Escape::Intermediate => match bytes.iter().position(|b| !INTERMEDIATE.contains(b)) {
                    Some(at) => {
                        self.escape = Escape::None;
                        // A byte that can't end the sequence aborts it and stays text.
                        let end = if ESCAPE_FINAL.contains(&bytes[at]) { at + 1 } else { at };
                        &rest[end..]
                    }
                    None => "",
                },
                Escape::Csi => {
                    let end = bytes.iter().position(|b| CSI_FINAL.contains(b));
                    let params = &rest[..end.unwrap_or(rest.len())];
                    if self.csi.len() + params.len() > MAX_CSI_PARAMS {
                        self.csi.clear();
                        self.escape = Escape::CsiTooLong;
                        rest
                    } else if let Some(end) = end {
                        if bytes[end] == b'm' {
                            self.csi.push_str(params);
                            let params = std::mem::take(&mut self.csi);
                            self.apply_sgr(&params);
                            self.csi = params;
                        }
                        self.csi.clear();
                        self.escape = Escape::None;
                        &rest[end + 1..]
                    } else {
                        self.csi.push_str(rest);
                        ""
                    }
                }
                Escape::CsiTooLong => match bytes.iter().position(|b| CSI_FINAL.contains(b)) {
                    Some(end) => {
                        self.escape = Escape::None;
                        &rest[end + 1..]
                    }
                    None => "",
                },
                Escape::Osc => match memchr::memchr2(BEL, ESC, bytes) {
                    Some(end) => {
                        self.escape = if bytes[end] == BEL { Escape::None } else { Escape::OscEsc };
                        &rest[end + 1..]
                    }
                    None => "",
                },
                Escape::OscEsc => {
                    if let Some(after) = rest.strip_prefix('\\') {
                        self.escape = Escape::None;
                        after
                    } else {
                        self.escape = Escape::Start;
                        rest
                    }
                }
            };
        }
    }

    /// Ends the current line. An escape sequence still open is dropped: it
    /// swallows the rest of its line and never the next one.
    pub fn end_line(&mut self) {
        self.escape = Escape::None;
        self.csi.clear();
    }

    fn push_text(&self, text: &mut String, spans: &mut Vec<Span>, chunk: &str) {
        let start = text.len();
        text.extend(chunk.chars().filter(|c| *c == '\t' || !c.is_control()));
        let end = text.len();
        if start == end || self.style.is_plain() {
            return;
        }
        match spans.last_mut() {
            Some(last) if last.end as usize == start && last.style == self.style => {
                last.end = end as u32;
            }
            _ => spans.push(Span {
                start: start as u32,
                end: end as u32,
                style: self.style,
            }),
        }
    }

    fn apply_sgr(&mut self, params: &str) {
        let mut codes = params
            .split([';', ':'])
            .map(|p| if p.is_empty() { Some(0) } else { p.parse::<u16>().ok() });
        while let Some(code) = codes.next() {
            match code {
                Some(0) => self.style = Style::default(),
                Some(1) => self.style.bold = true,
                Some(22) => self.style.bold = false,
                Some(c @ 30..=37) => self.style.fg = Some(Color((c - 30) as u8)),
                Some(39) => self.style.fg = None,
                Some(c @ 40..=47) => self.style.bg = Some(Color((c - 40) as u8)),
                Some(49) => self.style.bg = None,
                Some(c @ 90..=97) => self.style.fg = Some(Color((c - 90 + 8) as u8)),
                Some(c @ 100..=107) => self.style.bg = Some(Color((c - 100 + 8) as u8)),
                Some(c @ (38 | 48)) => {
                    let color = match codes.next() {
                        Some(Some(5)) => codes.next().flatten().map(|i| Color(i.min(255) as u8)),
                        // 24-bit color isn't supported yet; skip its r;g;b.
                        Some(Some(2)) => {
                            codes.by_ref().take(3).for_each(drop);
                            None
                        }
                        _ => None,
                    };
                    if color.is_some() {
                        if c == 38 {
                            self.style.fg = color;
                        } else {
                            self.style.bg = color;
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: &str) -> Line {
        AnsiParser::default().parse_line(raw)
    }

    fn fg(index: u8) -> Style {
        Style {
            fg: Some(Color(index)),
            ..Style::default()
        }
    }

    #[test]
    fn plain_text_has_no_spans() {
        let line = parse("You are standing in a field.");
        assert_eq!(&*line.text, "You are standing in a field.");
        assert!(line.spans.is_empty());
    }

    #[test]
    fn basic_and_bright_colors_and_bold() {
        let line = parse("\x1b[31mred\x1b[0m \x1b[1;92mgreen\x1b[m");
        assert_eq!(&*line.text, "red green");
        assert_eq!(
            &*line.spans,
            &[
                Span { start: 0, end: 3, style: fg(1) },
                Span {
                    start: 4,
                    end: 9,
                    style: Style { fg: Some(Color(10)), bg: None, bold: true },
                },
            ]
        );
    }

    #[test]
    fn palette_colors_and_background() {
        let line = parse("\x1b[38;5;208;48;5;17mx");
        assert_eq!(
            line.spans[0].style,
            Style { fg: Some(Color(208)), bg: Some(Color(17)), bold: false }
        );
    }

    #[test]
    fn truecolor_is_skipped_without_misreading_its_components() {
        // Without skipping, the "1" in r;g;b would turn bold on.
        let line = parse("\x1b[38;2;1;2;3mx");
        assert_eq!(&*line.text, "x");
        assert!(line.spans.is_empty());
    }

    #[test]
    fn style_carries_to_the_next_line() {
        let mut parser = AnsiParser::default();
        parser.parse_line("\x1b[33mstart");
        let next = parser.parse_line("continued");
        assert_eq!(&*next.spans, &[Span { start: 0, end: 9, style: fg(3) }]);
    }

    #[test]
    fn other_sequences_and_controls_are_removed() {
        let line = parse("\x1b[2Ja\x1b]0;title\x07b\x1b]0;t\x1b\\c\rd\x1b[");
        assert_eq!(&*line.text, "abcd");
    }

    #[test]
    fn a_line_fed_in_two_pieces_parses_as_one() {
        let raw = "\x1b[1;31mré\x1b[0md \x1b]0;t\x1b\\x\x1b]0;u\x07y\x1b[38;5;208mz";
        let whole = parse(raw);
        for (split, _) in raw.char_indices().skip(1) {
            let mut parser = AnsiParser::default();
            let (mut text, mut spans) = (String::new(), Vec::new());
            parser.feed(&raw[..split], &mut text, &mut spans);
            parser.feed(&raw[split..], &mut text, &mut spans);
            assert_eq!((&*text, &*spans), (&*whole.text, &*whole.spans), "split at {split}");
        }
    }

    #[test]
    fn character_set_selects_are_removed_whole() {
        assert_eq!(&*parse("a\x1b(Bb\x1b)0c").text, "abc");
        // A byte that can't end the sequence aborts it and is kept.
        assert_eq!(&*parse("a\x1b(éb").text, "aéb");
    }

    #[test]
    fn a_character_set_select_split_at_any_byte_is_removed_whole() {
        let raw = "\x1b[31mred\x1b(B\x1b[m plain\x1b)0 end";
        for split in 1..raw.len() {
            let mut parser = AnsiParser::default();
            let (mut text, mut spans) = (String::new(), Vec::new());
            parser.feed(&raw[..split], &mut text, &mut spans);
            parser.feed(&raw[split..], &mut text, &mut spans);
            assert_eq!(text, "red plain end", "split at {split}");
            assert_eq!(spans, [Span { start: 0, end: 3, style: fg(1) }], "split at {split}");
        }
    }

    #[test]
    fn an_overlong_sequence_is_skipped_without_being_kept() {
        let line = parse(&format!("\x1b[{}31mx", "1;".repeat(MAX_CSI_PARAMS)));
        assert_eq!(&*line.text, "x");
        assert!(line.spans.is_empty());
    }

    #[test]
    fn an_open_escape_ends_with_its_line() {
        let mut parser = AnsiParser::default();
        assert_eq!(&*parser.parse_line("a\x1b[31").text, "a");
        let next = parser.parse_line("b");
        assert_eq!(&*next.text, "b");
        assert!(next.spans.is_empty());
    }

    #[test]
    fn adjacent_runs_of_one_style_merge() {
        let line = parse("\x1b[31ma\x1b[31mb");
        assert_eq!(&*line.spans, &[Span { start: 0, end: 2, style: fg(1) }]);
    }
}
