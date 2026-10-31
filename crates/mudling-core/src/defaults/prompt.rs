//! Prompt learning: the game's prompt, learned rather than declared.
//!
//! A game that sends GMCP `Char.Vitals` sends it just before each prompt, so
//! the text after one is a prompt. Its numbers change, so it's kept as a
//! [`Shape`] with a slot for each number, and later prompts match it
//! whatever their numbers. Ported from the prototype's `GamePrompt`.
//!
//! A prompt is learned only at a **prompt signal**, which shows the server
//! finished writing it: its line ended by GA or EOR, by the connection going
//! quiet, or the next GMCP message arriving. So:
//!
//! - **The network can split a prompt across reads.** The half that came
//!   first is only pending text, and it's never learned.
//! - **Output can be glued onto a prompt** on servers that send no GA, with
//!   no signal between them. That line ends at a newline, so it's never
//!   learned: the learner waits for the next signal.
//! - **Text already pending when `Char.Vitals` arrives** came before it, so
//!   only what follows it on that line is the prompt.
//!
//! A game with no GMCP sends no `Char.Vitals`. Until one comes, a line ended
//! at a signal is learned if it holds a number and its shape comes twice
//! running, so a line that merely stalled isn't taken for a prompt.
//!
//! Each shape is counted. When the store is full, the rarest shape goes, ties
//! going to the longest.
//!
//! How a line ended says how much of it is prompt, whatever the counts. No
//! reading of the words could: `Tnl 500> ` and `You have 5 gold.` look alike.
//!
//! - **A line ended at a prompt signal** that a learned shape matches whole
//!   is all prompt, so a prompt that grew is never cut at its old shape. The
//!   accepted cost: output that stalled unended on a prompt's line, and was
//!   learned at a signal, is hidden with it.
//! - **A line ended by a newline** has its prompt found by
//!   [`PromptLearner::prefix_len`], so output glued after a prompt is shown.

use crate::line::{Line, LineEnd, Pending};

/// The most shapes kept.
const MOST_SHAPES: usize = 20;

#[derive(Debug, Default)]
pub struct PromptLearner {
    /// Most seen first; ties shortest first.
    shapes: Vec<Shape>,
    /// Set from a `Char.Vitals` until the prompt after it is learned.
    awaited: Option<AwaitedPrompt>,
    /// The pending text's length, as of the last read.
    pending_len: usize,
    /// The shape of the last line ended at a signal with no `Char.Vitals`
    /// before it.
    last_signaled: Option<Vec<String>>,
    /// A `Char.Vitals` has come, so only it teaches prompts from here on.
    vitals_seen: bool,
}

/// What's known of the prompt a `Char.Vitals` announced.
#[derive(Debug)]
struct AwaitedPrompt {
    /// Bytes of the line in progress that came before the `Char.Vitals`.
    before: usize,
    /// What's pending after them, so far. It may still grow.
    text: String,
}

impl PromptLearner {
    /// A learner that already knows `prompts`, each seen once.
    pub fn learning<'a>(prompts: impl IntoIterator<Item = &'a str>) -> PromptLearner {
        let mut learner = PromptLearner {
            vitals_seen: true,
            ..PromptLearner::default()
        };
        for prompt in prompts {
            learner.learn(prompt);
        }
        learner
    }

    /// A `Char.Vitals` arrived: the next text is a prompt. The first one
    /// shows the game has GMCP, so what repetition taught before it goes:
    /// that was a login menu or the like, not a prompt.
    pub fn vitals_arrived(&mut self) {
        if !std::mem::replace(&mut self.vitals_seen, true) {
            self.shapes.clear();
        }
        self.gmcp_arrived();
        self.awaited = Some(AwaitedPrompt {
            before: self.pending_len,
            text: String::new(),
        });
    }

    /// Any GMCP message arrived. The server writes in order, so a prompt
    /// still pending is whole.
    pub fn gmcp_arrived(&mut self) {
        if self
            .awaited
            .as_ref()
            .is_some_and(|awaited| !awaited.text.is_empty())
            && let Some(awaited) = self.awaited.take()
        {
            self.learn(&awaited.text);
        }
    }

    /// Text the server hasn't ended; read it after every read. Returns
    /// whether it is a prompt already learned, which the caller ends with
    /// `LinePipeline::prompt_end`.
    pub fn read_pending(&mut self, pending: &Pending<'_>) -> bool {
        self.pending_len = pending.plain.len();
        if let Some(awaited) = &mut self.awaited
            && let Some(prompt) = pending.plain.get(awaited.before..)
            && !is_blank(prompt)
        {
            prompt.clone_into(&mut awaited.text);
        }
        self.is_whole_prompt(pending.plain)
    }

    /// The pending text is the pager's prompt, which is never learned: the
    /// prompt a `Char.Vitals` announced is still to come.
    pub fn pager_pending(&mut self, pending: &Pending<'_>) {
        self.pending_len = pending.plain.len();
        if let Some(awaited) = &mut self.awaited {
            awaited.text.clear();
        }
    }

    /// A line ended at a prompt signal is the pager's prompt. Read it in
    /// place of [`PromptLearner::read_line`], for the same reason.
    pub fn pager_line(&mut self) {
        self.pending_len = 0;
        if let Some(awaited) = &mut self.awaited {
            awaited.before = 0;
            awaited.text.clear();
        }
    }

    /// A finished line; read every one, in order. Learns it if it ended at
    /// a prompt signal, and returns how many bytes of its plain text a
    /// prompt takes up at its start.
    pub fn read_line(&mut self, line: &Line) -> Option<usize> {
        self.pending_len = 0;
        let text = line.plain();
        let at_signal = line.end() == LineEnd::PromptEnd;
        match self.awaited.take() {
            Some(awaited) => {
                let prompt = text.get(awaited.before..).unwrap_or_default();
                if at_signal && !is_blank(prompt) {
                    self.learn(prompt);
                } else {
                    self.awaited = Some(AwaitedPrompt {
                        before: 0,
                        text: String::new(),
                    });
                }
            }
            None if at_signal && !self.vitals_seen => self.learn_if_repeated(text),
            None => {}
        }
        if at_signal
            && self
                .shapes
                .iter()
                .any(|shape| shape.prefix_len(text) == Some(text.len()))
        {
            return Some(text.len());
        }
        self.prefix_len(text)
    }

    /// How many bytes a prompt takes up at the start of `text`, a line with
    /// no prompt signal to say where its prompt stops. A longer matching
    /// shape wins over a shorter one only if what it adds reads as prompt, so
    /// a prompt that grew isn't cut at its old shape and output learned glued
    /// to a prompt is still shown.
    pub fn prefix_len(&self, text: &str) -> Option<usize> {
        let mut ends: Vec<usize> = self
            .shapes
            .iter()
            .filter_map(|shape| shape.prefix_len(text))
            .collect();
        ends.sort_unstable();
        let (&shortest, longer) = ends.split_first()?;
        Some(longer.iter().fold(shortest, |end, &next| {
            if reads_as_prompt(&text[end..next]) {
                next
            } else {
                end
            }
        }))
    }

    /// The learned shapes, most seen first, each with the times seen. A
    /// shape shows each number as `#`.
    pub fn shapes(&self) -> impl Iterator<Item = (String, u32)> + '_ {
        self.shapes
            .iter()
            .map(|shape| (shape.pieces.join("#"), shape.seen))
    }

    /// Whether `text` is all prompt. A prompt that ends in a number isn't
    /// trusted here: the network may have cut that number short. Nor is a
    /// shorter prompt while a longer one could still grow from `text`.
    fn is_whole_prompt(&self, text: &str) -> bool {
        self.shapes.iter().any(|shape| {
            shape.pieces.last().is_some_and(|last| !last.is_empty())
                && shape.prefix_len(text) == Some(text.len())
        }) && !self.shapes.iter().any(|shape| shape.could_grow_from(text))
    }

    /// Learns a line that ended at a signal on a server sending no
    /// `Char.Vitals`, once its shape comes twice running. It must hold a
    /// number: "Password: " is no prompt to learn.
    fn learn_if_repeated(&mut self, text: &str) {
        let pieces = Shape::new(text).pieces;
        if pieces.len() < 2 {
            return;
        }
        let known = self.shapes.iter().any(|shape| shape.pieces == pieces);
        if known || self.last_signaled.as_ref() == Some(&pieces) {
            self.learn(text);
        }
        self.last_signaled = Some(pieces);
    }

    fn learn(&mut self, prompt: &str) {
        let shape = Shape::new(prompt);
        if shape.length == 0 {
            return;
        }
        if let Some(index) = self
            .shapes
            .iter()
            .position(|known| known.pieces == shape.pieces)
        {
            self.seen_again(index);
            return;
        }
        if self.shapes.len() == MOST_SHAPES
            && let Some(rarest) = (0..self.shapes.len()).min_by_key(|&index| {
                (
                    self.shapes[index].seen,
                    std::cmp::Reverse(self.shapes[index].length),
                )
            })
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
        self.shapes
            .sort_by_key(|shape| (std::cmp::Reverse(shape.seen), shape.length));
    }
}

fn is_blank(text: &str) -> bool {
    text.trim().is_empty()
}

/// Whether text after a prompt is more prompt, not output: output opens with
/// a word, as `You rest.` does.
fn reads_as_prompt(text: &str) -> bool {
    !text.trim_start().starts_with(char::is_alphabetic)
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
        Shape {
            pieces,
            length,
            seen: 1,
        }
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

    /// Whether `text` is this prompt's start, with more of it to come.
    fn could_grow_from(&self, text: &str) -> bool {
        let mut rest = text;
        for (index, piece) in self.pieces.iter().enumerate() {
            if index > 0 {
                rest = &rest[number_len(rest).unwrap_or(0)..];
                if rest.is_empty() {
                    return true;
                }
            }
            match rest.strip_prefix(piece.as_str()) {
                Some("") => return index + 1 < self.pieces.len(),
                Some(after) => rest = after,
                None => return piece.starts_with(rest),
            }
        }
        false
    }
}

/// The first number in `text`, `-?[0-9]+`, as a byte range.
fn find_number(text: &str) -> Option<(usize, usize)> {
    let digit = text.bytes().position(|byte| byte.is_ascii_digit())?;
    let start = if digit > 0 && text.as_bytes()[digit - 1] == b'-' {
        digit - 1
    } else {
        digit
    };
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
        let shape = Shape::new("(20h-30m-40v-500)");
        assert_eq!(shape.pieces, ["(", "h", "m", "v", ")"]);
        assert_eq!(
            shape.prefix_len("(-9h-2m-0v-1)rest"),
            Some("(-9h-2m-0v-1)".len())
        );
        assert_eq!(shape.prefix_len("(9h-m-0v-1)"), None);
    }

    #[test]
    fn a_shape_can_tell_what_could_still_grow_into_it() {
        let shape = Shape::new("(1h) (2m) ");
        for start in [
            "", "(", "(1", "(1h", "(1h) ", "(1h) (", "(1h) (2", "(1h) (2m",
        ] {
            assert!(shape.could_grow_from(start), "{start:?}");
        }
        for other in ["(1h) (2m) ", "(1h) (2m) x", "(1x", "x"] {
            assert!(!shape.could_grow_from(other), "{other:?}");
        }
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
