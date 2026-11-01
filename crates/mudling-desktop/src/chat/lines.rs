//! The chat's virtualized lines, which wrap and so differ in height.
//!
//! No height is kept for the scrollback. A view is the line at its top, by
//! absolute number, and how far past that line's top it sits; each frame walks
//! outward from there, measuring only the lines it passes. So a new column
//! width costs what any frame costs, and trimming the oldest lines moves
//! nothing. The price is that nothing here knows the scrollback's total
//! height, which a scrollbar would have to estimate from line numbers.

use super::Chat;
use gpui::{
    AnyElement, App, Bounds, ContentMask, Element, ElementId, Entity, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, Size, Style, Window, px, relative,
};
use std::ops::Range;

/// The line at the top of a view.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Anchor {
    /// Its number since the session began, which trimming doesn't change.
    pub line: u64,
    /// How much of it is scrolled out above the view. Out of range after a
    /// scroll, until the next frame finds the line that really is on top.
    pub past_top: Pixels,
}

/// Where a view sits in the scrollback.
#[derive(Clone, Debug)]
pub struct LinePosition {
    following: bool,
    /// Whether the last scroll since the last frame went down. Only that can
    /// reach the newest line: a wider column or a taller view that brings it
    /// into view leaves a reader where they are.
    scrolled_down: bool,
    top: Anchor,
    top_height: Pixels,
    width: Pixels,
    height: Pixels,
}

impl Default for LinePosition {
    fn default() -> LinePosition {
        LinePosition {
            following: true,
            scrolled_down: false,
            top: Anchor::default(),
            top_height: px(0.),
            width: px(0.),
            height: px(0.),
        }
    }
}

/// The lines a frame draws, as indexes into the scrollback.
#[derive(Debug, PartialEq)]
pub struct Placement {
    pub lines: Range<usize>,
    /// Where the first of them starts, relative to the view's top.
    pub top: Pixels,
}

impl LinePosition {
    /// A view with the line numbered `line` at its top.
    pub fn reading_from(line: u64) -> LinePosition {
        LinePosition {
            following: false,
            top: Anchor {
                line,
                past_top: px(0.),
            },
            ..LinePosition::default()
        }
    }

    /// Whether the view stays on the newest line as lines arrive.
    pub fn is_following(&self) -> bool {
        self.following
    }

    #[cfg(test)]
    pub fn top(&self) -> Anchor {
        self.top
    }

    #[cfg(test)]
    pub fn top_height(&self) -> Pixels {
        self.top_height
    }

    /// How tall the view last drew.
    pub fn height(&self) -> Pixels {
        self.height
    }

    /// Whether any of the scrollback lies above the view. Lines trimmed since
    /// `evicted` was the oldest number don't count.
    pub fn has_lines_above(&self, evicted: u64) -> bool {
        self.top.line > evicted || self.top.past_top > px(0.)
    }

    /// Moves the view `down`, or up if negative. Scrolling up stops following;
    /// reaching the newest line again follows it again.
    pub fn scroll(&mut self, down: Pixels) {
        if self.following && down >= px(0.) {
            return;
        }
        self.following = false;
        self.scrolled_down = down > px(0.);
        self.top.past_top += down;
    }

    /// Settles the view for this frame. `height` measures a scrollback line
    /// at the current width; only lines in view, or scrolled past since the
    /// last frame, are asked for.
    pub fn place(
        &mut self,
        evicted: u64,
        len: usize,
        view: Size<Pixels>,
        mut height: impl FnMut(usize) -> Pixels,
    ) -> Placement {
        if len == 0 {
            self.following = true;
            return Placement {
                lines: 0..0,
                top: px(0.),
            };
        }
        let placement = (!self.following)
            .then(|| self.place_from_anchor(evicted, len, view, &mut height))
            .flatten();
        let placement = placement.unwrap_or_else(|| {
            self.following = true;
            place_from_newest(len, view.height, &mut height)
        });
        self.top = Anchor {
            line: evicted + placement.lines.start as u64,
            past_top: -placement.top,
        };
        self.top_height = height(placement.lines.start);
        self.width = view.width;
        self.height = view.height;
        self.scrolled_down = false;
        placement
    }

    /// `None` once scrolling down brings the newest line into view, where the
    /// view follows it.
    fn place_from_anchor(
        &self,
        evicted: u64,
        len: usize,
        view: Size<Pixels>,
        height: &mut impl FnMut(usize) -> Pixels,
    ) -> Option<Placement> {
        let (mut first, mut past_top) = match self.top.line.checked_sub(evicted) {
            Some(index) => (index as usize, self.top.past_top),
            None => (0, px(0.)),
        };
        if view.width != self.width && self.top_height > px(0.) {
            past_top *= height(first) / self.top_height;
        }
        while past_top < px(0.) {
            if first == 0 {
                past_top = px(0.);
                break;
            }
            first -= 1;
            past_top += height(first);
        }
        while past_top >= height(first) {
            past_top -= height(first);
            first += 1;
            if first == len {
                return None;
            }
        }
        let mut end = first;
        let mut bottom = -past_top;
        while bottom < view.height && end < len {
            bottom += height(end);
            end += 1;
        }
        let reached_newest = self.scrolled_down && end == len && bottom <= view.height;
        (!reached_newest).then_some(Placement {
            lines: first..end,
            top: -past_top,
        })
    }
}

/// Stacks lines up from the view's bottom. Lines too few to fill the view
/// start at its top instead.
fn place_from_newest(
    len: usize,
    view_height: Pixels,
    height: &mut impl FnMut(usize) -> Pixels,
) -> Placement {
    let mut first = len - 1;
    let mut filled = height(first);
    while filled < view_height && first > 0 {
        first -= 1;
        filled += height(first);
    }
    Placement {
        lines: first..len,
        top: (view_height - filled).min(px(0.)),
    }
}

/// Which of the chat's views a [`LineList`] draws.
#[derive(Clone, Copy, PartialEq)]
pub enum Pane {
    History,
    Live,
}

/// Draws one pane's lines. They live on [`Chat`], which measures them against
/// the width this element is given.
pub struct LineList {
    chat: Entity<Chat>,
    pane: Pane,
}

impl LineList {
    pub fn new(chat: Entity<Chat>, pane: Pane) -> LineList {
        LineList { chat, pane }
    }
}

impl Element for LineList {
    type RequestLayoutState = ();
    type PrepaintState = Vec<AnyElement>;

    fn id(&self) -> Option<ElementId> {
        Some(match self.pane {
            Pane::History => "history-lines".into(),
            Pane::Live => "live-lines".into(),
        })
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Vec<AnyElement> {
        let pane = self.pane;
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            self.chat
                .update(cx, |chat, cx| chat.place_lines(pane, bounds, window, cx))
        })
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        lines: &mut Vec<AnyElement>,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for line in lines {
                line.paint(window, cx);
            }
        });
    }
}

impl IntoElement for LineList {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::super::LINE_HEIGHT;
    use super::*;
    use gpui::size;
    use std::cell::Cell;

    const VIEW: Size<Pixels> = size(px(300.), px(100.));

    /// Every third line wraps to three rows.
    fn mixed_height(number: u64) -> Pixels {
        LINE_HEIGHT * if number.is_multiple_of(3) { 3. } else { 1. }
    }

    fn place(position: &mut LinePosition, evicted: u64, len: usize, width: Pixels) -> Placement {
        position.place(evicted, len, size(width, VIEW.height), |index| {
            mixed_height(evicted + index as u64)
        })
    }

    fn reading(line: u64, past_top: Pixels) -> LinePosition {
        LinePosition {
            top: Anchor { line, past_top },
            top_height: mixed_height(line),
            width: VIEW.width,
            ..LinePosition::reading_from(line)
        }
    }

    #[test]
    fn a_followed_view_ends_on_the_newest_line() {
        let mut position = LinePosition::default();
        let placement = place(&mut position, 0, 1_000, VIEW.width);
        assert_eq!(placement.lines.end, 1_000);
        // Lines 997 to 999 hold the view's 5 rows exactly.
        assert_eq!(placement.lines.start, 997);
        assert_eq!(placement.top, px(0.));
        assert!(position.is_following());
    }

    #[test]
    fn lines_too_few_to_fill_the_view_start_at_its_top() {
        let mut position = LinePosition::default();
        let placement = place(&mut position, 0, 2, VIEW.width);
        assert_eq!(
            placement,
            Placement {
                lines: 0..2,
                top: px(0.)
            }
        );
    }

    #[test]
    fn scrolling_crosses_lines_of_different_heights() {
        let mut position = reading(300, px(0.));
        position.scroll(LINE_HEIGHT * 4.);
        let placement = place(&mut position, 0, 1_000, VIEW.width);
        assert_eq!(position.top().line, 302);
        assert_eq!(position.top().past_top, px(0.));
        assert_eq!(placement.top, px(0.));

        position.scroll(-LINE_HEIGHT * 3.);
        place(&mut position, 0, 1_000, VIEW.width);
        assert_eq!(
            position.top(),
            Anchor {
                line: 300,
                past_top: LINE_HEIGHT
            }
        );
    }

    #[test]
    fn scrolling_up_past_the_oldest_line_stops_there() {
        let mut position = reading(10, px(0.));
        position.scroll(px(-100_000.));
        place(&mut position, 8, 1_000, VIEW.width);
        assert_eq!(
            position.top(),
            Anchor {
                line: 8,
                past_top: px(0.)
            }
        );
    }

    #[test]
    fn scrolling_up_stops_following_and_the_newest_line_resumes_it() {
        let mut position = LinePosition::default();
        place(&mut position, 0, 1_000, VIEW.width);
        position.scroll(px(10.));
        assert!(position.is_following(), "there's nothing below");

        position.scroll(px(-10.));
        assert!(!position.is_following());
        position.scroll(px(10.));
        place(&mut position, 0, 1_000, VIEW.width);
        assert!(position.is_following());
    }

    #[test]
    fn trimming_lines_leaves_the_top_line_in_place() {
        let mut position = reading(501, LINE_HEIGHT);
        place(&mut position, 0, 1_000, VIEW.width);
        let top = position.top();

        let placement = place(&mut position, 200, 800, VIEW.width);
        assert_eq!(position.top(), top);
        assert_eq!(placement.lines.start, 301);
        assert!(!position.is_following());
    }

    #[test]
    fn a_top_line_that_is_trimmed_gives_way_to_the_oldest() {
        let mut position = reading(100, LINE_HEIGHT);
        place(&mut position, 500, 1_000, VIEW.width);
        assert_eq!(
            position.top(),
            Anchor {
                line: 500,
                past_top: px(0.)
            }
        );
    }

    #[test]
    fn a_new_width_keeps_the_top_line_and_the_share_of_it_scrolled_out() {
        let mut position = reading(300, LINE_HEIGHT);
        place(&mut position, 0, 1_000, VIEW.width);

        // Twice as narrow: line 300 now takes six rows.
        position.place(0, 1_000, size(px(150.), VIEW.height), |index| {
            mixed_height(index as u64) * 2.
        });
        assert_eq!(
            position.top(),
            Anchor {
                line: 300,
                past_top: LINE_HEIGHT * 2.
            }
        );
    }

    #[test]
    fn a_wider_column_that_brings_in_the_newest_line_keeps_the_reader_in_place() {
        // Line 990 and the lines after it overflow the view, until they halve.
        let mut position = reading(990, LINE_HEIGHT);
        place(&mut position, 0, 1_000, VIEW.width);

        let halved = |index: usize| mixed_height(index as u64) * 0.5;
        position.place(0, 1_000, size(px(600.), VIEW.height * 3.), halved);
        assert!(!position.is_following());
        assert_eq!(position.top().line, 990);
    }

    #[test]
    fn a_taller_view_reaches_the_newest_line_only_once_scrolled_down() {
        let mut position = reading(995, px(0.));
        let tall = size(VIEW.width, VIEW.height * 5.);
        position.place(0, 1_000, tall, |index| mixed_height(index as u64));
        assert!(!position.is_following());

        position.scroll(px(-1.));
        position.place(0, 1_000, tall, |index| mixed_height(index as u64));
        assert!(!position.is_following(), "that went up");

        position.scroll(px(1.));
        position.place(0, 1_000, tall, |index| mixed_height(index as u64));
        assert!(position.is_following());
    }

    #[test]
    fn a_new_width_measures_only_the_lines_in_view() {
        let mut position = reading(50_000, px(0.));
        place(&mut position, 0, 100_000, VIEW.width);

        let measured = Cell::new(0);
        position.place(0, 100_000, size(px(200.), VIEW.height), |index| {
            measured.set(measured.get() + 1);
            mixed_height(index as u64)
        });
        assert!(measured.get() < 40, "measured {}", measured.get());
    }

    #[test]
    fn a_line_taller_than_the_view_can_be_read_through() {
        let mut position = reading(0, px(0.));
        let tall = |index: usize| if index == 0 { px(500.) } else { LINE_HEIGHT };
        position.scroll(px(200.));
        let placement = position.place(0, 100, VIEW, tall);
        assert_eq!(placement.lines, 0..1);
        assert_eq!(placement.top, px(-200.));
    }

    #[test]
    fn lines_with_no_height_are_walked_past_up_and_down_the_scrollback() {
        let hidden_every_second = |index: usize| px(if index.is_multiple_of(2) { 0. } else { 20. });
        let view = size(px(400.), px(60.));
        let mut position = LinePosition::default();
        let placed = position.place(0, 100, view, hidden_every_second);
        assert_eq!(placed.lines, 95..100, "three rows, among hidden lines");

        position.scroll(px(-30.));
        let placed = position.place(0, 100, view, hidden_every_second);
        assert_eq!((placed.lines.start, placed.top), (91, px(-10.)));

        let mut at_a_hidden_line = LinePosition::reading_from(50);
        let placed = at_a_hidden_line.place(0, 100, view, hidden_every_second);
        assert_eq!(
            placed.lines.start, 51,
            "the first line that shows is on top"
        );
    }

    #[test]
    fn an_empty_scrollback_places_nothing() {
        let mut position = LinePosition::default();
        assert_eq!(place(&mut position, 0, 0, VIEW.width).lines, 0..0);
        assert!(position.is_following());
    }
}
