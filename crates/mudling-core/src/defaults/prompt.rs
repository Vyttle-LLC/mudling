//! Prompt learning: the game's prompt, learned rather than declared.
//!
//! A game that sends GMCP `Char.Vitals` sends it just before each prompt, so
//! the text after one is a prompt. Its numbers change, so it's kept as a
//! [`Shape`] with a slot for each number, and later prompts match it
//! whatever their numbers. Ported from the prototype's `GamePrompt`.
//!
//! What follows a `Char.Vitals` isn't always a whole, bare prompt:
//!
//! - **The network can split a prompt across reads.** The half that came
//!   first is pending text that later grows with the same line number. It
//!   is never learned: a prompt is learned only once it's known to be
//!   whole, when its line ends, or when a GA or the next GMCP message
//!   shows the server finished writing it.
//! - **Output can be glued onto a prompt** on servers that send no GA.
//!   Such a line is learned whole, but each shape is counted, the most seen
//!   is tried first, and ties go to the shortest, so a bare prompt wins.
//!   When the store is full, the rarest shape goes, ties going to the
//!   longest, so a stream of glued lines can't push out a real prompt.

use crate::line::{Line, LineEnd, Pending};

/// The most shapes kept.
const MOST_SHAPES: usize = 20;

#[derive(Debug, Default)]
pub struct PromptLearner {
    /// Most seen first; ties shortest first.
    shapes: Vec<Shape>,
    awaiting: Awaiting,
}

#[derive(Debug, Default)]
enum Awaiting {
    #[default]
    Nothing,
    /// A `Char.Vitals` arrived: the next text is a prompt.
    Prompt,
    /// The prompt is pending, this much of it so far. It may still grow.
    Pending { text: String },
}

impl PromptLearner {
    /// A learner that already knows `prompts`, each seen once.
    pub fn learning<'a>(prompts: impl IntoIterator<Item = &'a str>) -> PromptLearner {
        let mut learner = PromptLearner::default();
        for prompt in prompts {
            learner.learn(prompt);
        }
        learner
    }

    /// A `Char.Vitals` arrived: the next text is a prompt.
    pub fn vitals_arrived(&mut self) {
        self.gmcp_arrived();
        self.awaiting = Awaiting::Prompt;
    }

    /// Any GMCP message arrived. The server writes in order, so a prompt
    /// still pending is whole.
    pub fn gmcp_arrived(&mut self) {
        if let Awaiting::Pending { text } =std::mem::take(&mut self.awaiting) {
            self.learn(&text);
        }
    }

    /// Text the server hasn't ended. Returns whether it is a prompt already
    /// learned, which the caller ends with `LinePipeline::prompt_end`.
    pub fn read_pending(&mut self, pending: &Pending<'_>) -> bool {
        let awaiting = matches!(self.awaiting, Awaiting::Prompt | Awaiting::Pending { .. });
        if awaiting && !is_blank(pending.plain) {
            self.awaiting = Awaiting::Pending { text: pending.plain.to_owned() };
        }
        self.is_whole_prompt(pending.plain)
    }

    /// A finished line. Learns it if it's the prompt being awaited (a
    /// pending prompt that grew into this line included), and returns how
    /// many bytes of its plain text a prompt takes up at its start.
    pub fn read_line(&mut self, line: &Line) -> Option<usize> {
        let text = line.plain();
        if !is_blank(text) && !matches!(std::mem::take(&mut self.awaiting), Awaiting::Nothing) {
            self.learn_line(text, line.end());
        }
        self.prefix_len(text)
    }

    /// How many bytes a prompt takes up at the start of `text`, if one opens it.
    pub fn prefix_len(&self, text: &str) -> Option<usize> {
        self.shapes.iter().find_map(|shape| shape.prefix_len(text))
    }

    /// The learned shapes, most seen first, each with the times seen. A
    /// shape shows each number as `#`.
    pub fn shapes(&self) -> impl Iterator<Item = (String, u32)> + '_ {
        self.shapes.iter().map(|shape| (shape.pieces.join("#"), shape.seen))
    }

    /// Whether `text` is all prompt. A prompt that ends in a number isn't
    /// trusted here: the network may have cut that number short.
    fn is_whole_prompt(&self, text: &str) -> bool {
        self.shapes
            .iter()
            .any(|shape| shape.pieces.last().is_some_and(|last| !last.is_empty()) && shape.prefix_len(text) == Some(text.len()))
    }

    /// A line ended by GA is a whole prompt. A line ended by a newline may
    /// have output glued on: if a known prompt opens it, that one is seen
    /// again, and otherwise the line is learned whole.
    fn learn_line(&mut self, text: &str, end: LineEnd) {
        let known = (end != LineEnd::PromptEnd).then(|| self.shapes.iter().position(|shape| shape.prefix_len(text).is_some()));
        match known.flatten() {
            Some(index) => self.seen_again(index),
            None => self.learn(text),
        }
    }

    fn learn(&mut self, prompt: &str) {
        let shape = Shape::new(prompt);
        if shape.length == 0 {
            return;
        }
        if let Some(index) = self.shapes.iter().position(|known| known.pieces == shape.pieces) {
            self.seen_again(index);
            return;
        }
        if self.shapes.len() == MOST_SHAPES
            && let Some(rarest) = (0..self.shapes.len())
                .min_by_key(|&index| (self.shapes[index].seen, std::cmp::Reverse(self.shapes[index].length)))
        {
            self.shapes.remove(rarest);
        }
        self.shapes.push(shape);
        self.sort();
    }

    fn seen_again(&mut self, index: usize) {
        self.shapes[index].seen += 1;
        self.sort();
    }

    fn sort(&mut self) {
        self.shapes.sort_by_key(|shape| (std::cmp::Reverse(shape.seen), shape.length));
    }
}

fn is_blank(text: &str) -> bool {
    text.trim().is_empty()
}

/// A prompt with a slot for each number: `(#h-#m-#v-#)`.
#[derive(Debug, PartialEq, Eq)]
struct Shape {
    /// The text around the numbers: one more piece than there are slots.
    pieces: Vec<String>,
    /// Bytes without the numbers. A prompt that's only numbers can't be
    /// told from output.
    length: usize,
    seen: u32,
}

impl Shape {
    fn new(prompt: &str) -> Shape {
        let mut pieces = Vec::new();
        let mut rest = prompt;
        while let Some((start, end)) = find_number(rest) {
            pieces.push(rest[..start].to_owned());
            rest = &rest[end..];
        }
        pieces.push(rest.to_owned());
        let length = pieces.iter().map(String::len).sum();
        Shape { pieces, length, seen: 1 }
    }

    /// Where the prompt ends, if it opens `text`.
    fn prefix_len(&self, text: &str) -> Option<usize> {
        let mut at = 0;
        for (index, piece) in self.pieces.iter().enumerate() {
            if index > 0 {
                at += number_len(&text[at..])?;
            }
            if !text[at..].starts_with(piece.as_str()) {
                return None;
            }
            at += piece.len();
        }
        Some(at)
    }
}

/// The first number in `text`, `-?[0-9]+`, as a byte range.
fn find_number(text: &str) -> Option<(usize, usize)> {
    let digit = text.bytes().position(|byte| byte.is_ascii_digit())?;
    let start = if digit > 0 && text.as_bytes()[digit - 1] == b'-' { digit - 1 } else { digit };
    Some((start, digit + digits_len(&text[digit..])))
}

/// The length of the number that opens `text`, if one does.
fn number_len(text: &str) -> Option<usize> {
    let sign = usize::from(text.starts_with('-'));
    let digits = digits_len(&text[sign..]);
    (digits > 0).then_some(sign + digits)
}

fn digits_len(text: &str) -> usize {
    text.bytes().take_while(u8::is_ascii_digit).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shape_has_a_slot_for_each_number() {
        let shape = Shape::new("(110h-125m-120v-1062)");
        assert_eq!(shape.pieces, ["(", "h", "m", "v", ")"]);
        assert_eq!(shape.prefix_len("(-9h-2m-0v-1)rest"), Some("(-9h-2m-0v-1)".len()));
        assert_eq!(shape.prefix_len("(9h-m-0v-1)"), None);
    }

    #[test]
    fn a_full_store_drops_the_rarest_and_longest() {
        let mut learner = PromptLearner::learning(["<1hp> "]);
        for glued in 0..MOST_SHAPES {
            learner.learn(&format!("<1hp> output {}", "x".repeat(glued)));
        }
        assert_eq!(learner.shapes.len(), MOST_SHAPES);
        assert!(learner.shapes().any(|(shape, _)| shape == "<#hp> "));
    }
}
