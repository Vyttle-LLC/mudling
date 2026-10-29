//! Turns raw server lines into plain text plus style spans, once per line.
//!
//! Only SGR (`ESC [ … m`) is interpreted: the 16 basic colors, the 256-color
//! palette and bold. Every other escape sequence is removed from the text.

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

/// A byte range of [`Line::text`] drawn in a non-default style.
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

const ESC: char = '\x1b';

/// Parses lines in order. The current style carries from one line to the
/// next, as servers expect.
#[derive(Debug, Default)]
pub struct AnsiParser {
    style: Style,
}

impl AnsiParser {
    pub fn parse_line(&mut self, raw: &str) -> Line {
        let mut text = String::with_capacity(raw.len());
        let mut spans = Vec::new();
        let mut rest = raw;
        while let Some(esc) = rest.find(ESC) {
            self.push_text(&mut text, &mut spans, &rest[..esc]);
            rest = self.consume_escape(&rest[esc + 1..]);
        }
        self.push_text(&mut text, &mut spans, rest);
        Line {
            text: text.into_boxed_str(),
            spans: spans.into_boxed_slice(),
            kind: LineKind::Server,
        }
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

    /// Consumes one escape sequence (the text just after `ESC`) and returns
    /// what follows it. An unterminated sequence swallows the rest of the line.
    fn consume_escape<'a>(&mut self, after_esc: &'a str) -> &'a str {
        let mut chars = after_esc.char_indices();
        match chars.next() {
            Some((_, '[')) => {
                let body = &after_esc[1..];
                match body.find(|c: char| ('\x40'..='\x7e').contains(&c)) {
                    Some(end) => {
                        if body.as_bytes()[end] == b'm' {
                            self.apply_sgr(&body[..end]);
                        }
                        &body[end + 1..]
                    }
                    None => "",
                }
            }
            Some((_, ']')) => {
                let body = &after_esc[1..];
                match body.find(['\x07', ESC]) {
                    Some(end) if body.as_bytes()[end] == 0x07 => &body[end + 1..],
                    Some(end) => body[end + 1..].strip_prefix('\\').unwrap_or(&body[end..]),
                    None => "",
                }
            }
            Some((_, c)) => &after_esc[c.len_utf8()..],
            None => "",
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
    fn adjacent_runs_of_one_style_merge() {
        let line = parse("\x1b[31ma\x1b[31mb");
        assert_eq!(&*line.spans, &[Span { start: 0, end: 2, style: fg(1) }]);
    }
}
