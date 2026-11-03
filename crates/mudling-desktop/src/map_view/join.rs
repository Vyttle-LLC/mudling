//! Tidying up duplicates: the line above the map, Review, and picking rooms by
//! hand. The model decides what's shown; this draws it and tells the session.
//! MUDling only ever suggests: a join starts with the player pressing Join.

use super::forget::{self, KeepRoom};
use super::model::{Description, JoinNote, ReviewPair};
use super::{MapView, REVIEW_MAP_HEIGHT};
use crate::room_panel::button;
use crate::shell::FocusCommandBar;
use crate::theme::Theme;
use gpui::{
    AnyElement, App, Context, Div, FontWeight, HighlightStyle, KeyBinding, MouseButton, Pixels,
    Role, SharedString, Stateful, StyledText, Window, actions, div, prelude::*, px,
};
use mudling_core::map::{RoomId, RoomPair};
use mudling_core::session::MapEdit;

/// The key context while focus is in Review, deeper than the dock's Escape.
const REVIEW_CONTEXT: &str = "MapReview";
/// Review's columns, in the order the map numbers its copies.
const COPY_SIDES: [&str; 2] = ["First copy", "Second copy"];
const COMPARE_LABEL_WIDTH: Pixels = px(92.);

actions!(map_view, [PickRoom, CloseReview]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new(secondary!("shift-j"), PickRoom, None),
        KeyBinding::new("escape", CloseReview, Some(REVIEW_CONTEXT)),
        KeyBinding::new("escape", KeepRoom, Some(forget::QUESTION_CONTEXT)),
    ]);
}

impl MapView {
    /// Picks the room shown, as the keyboard's way to Shift-click. With a
    /// second room picked, focus goes to Join. Review's map is for looking,
    /// with no Join to go to, so it does nothing there.
    pub fn pick_selected(&mut self, window: &mut Window, cx: &mut Context<MapView>) {
        let Some(room) = self.model.selection().map(|selection| selection.room) else {
            return;
        };
        if self.model.reviewing() {
            return;
        }
        self.model.pick(room);
        if self.model.picked_pair().is_some() {
            window.focus(&self.join_button, cx);
        }
        cx.notify();
    }

    /// Looks for duplicates here and now, as the images, checks and harness need.
    pub fn find_duplicates_now(&mut self, cx: &mut Context<MapView>) {
        self.model.scan_now();
        cx.notify();
    }

    pub fn review_sample(&mut self, window: &mut Window, cx: &mut Context<MapView>) {
        self.find_duplicates_now(cx);
        self.open_review(window, cx);
    }

    /// Picks the first two rooms that read alike.
    #[cfg(any(test, target_os = "macos"))]
    pub fn pick_sample(&mut self, cx: &mut Context<MapView>) {
        self.find_duplicates_now(cx);
        if let Some((first, second)) = self.model.duplicates().pairs.first().copied() {
            self.model.pick(first);
            self.model.pick(second);
        }
        cx.notify();
    }

    /// Shows the core's answer to a join that went in, as after Join, which
    /// takes the picks with it, for `--ax-check`.
    #[cfg(target_os = "macos")]
    pub fn show_sample_join(&mut self, cx: &mut Context<MapView>) {
        self.model.clear_picks();
        use mudling_core::map::{JoinDone, LastJoin, MapChange};
        use mudling_core::persist::Timestamp;
        use mudling_core::store::{Diff, Revision};
        let joined = JoinDone::Joined {
            label: "Joined two copies of Square 1, 1".to_owned(),
            created: Timestamp::now(),
        };
        let diff = Diff {
            revision: Revision(2),
            change: MapChange {
                last_join: Some(LastJoin {
                    attempt: 1,
                    outcome: Ok(joined),
                }),
                ..MapChange::default()
            },
        };
        self.apply(&diff, cx);
    }

    fn open_review(&mut self, window: &mut Window, cx: &mut Context<MapView>) {
        self.model.open_review();
        self.review_open = true;
        window.focus(&self.review_first, cx);
        cx.notify();
    }

    /// Closes Review, back on the button that opened it if there's still
    /// anything to review.
    pub fn close_review(&mut self, window: &mut Window, cx: &mut Context<MapView>) {
        self.model.close_review();
        self.review_open = false;
        self.start_work(cx);
        self.focus_what_remains(window, cx);
        cx.notify();
    }

    /// Takes focus to a control that's still there.
    pub(super) fn focus_what_remains(&mut self, window: &mut Window, cx: &mut Context<MapView>) {
        if self.model.reviewing() {
            window.focus(&self.review_first, cx);
        } else if self.model.duplicates().pairs.is_empty() {
            window.dispatch_action(Box::new(FocusCommandBar), cx);
        } else {
            window.focus(&self.review_button, cx);
        }
    }

    /// Looks after the keyboard when the session's answer takes a control
    /// away: a join can end Review, and Undo goes with its note. Focus moves
    /// only if it was there, so a mouse player's typing stays where it is.
    pub(super) fn keep_focus(&mut self, window: &mut Window, cx: &mut Context<MapView>) {
        let joined = matches!(self.model.join_note(), Some(JoinNote::Joined { .. }));
        if self.review_open && !self.model.reviewing() {
            self.review_open = false;
            cx.defer_in(window, move |view, window, cx| {
                if joined {
                    window.focus(&view.undo_button, cx);
                } else {
                    view.focus_what_remains(window, cx);
                }
            });
        } else if self.undo_button.is_focused(window) && !joined && self.model.joining().is_none() {
            cx.defer_in(window, Self::focus_what_remains);
        }
    }

    fn skip_pair(&mut self, cx: &mut Context<MapView>) {
        self.model.skip_pair();
        cx.notify();
    }

    fn not_the_same(&mut self, pair: RoomPair, window: &mut Window, cx: &mut Context<MapView>) {
        self.model.clear_join_note();
        self.model.answer_pair(pair, None);
        cx.emit(MapEdit::NotSame {
            first: pair.0,
            second: pair.1,
        });
        if self.model.review_pair().is_none() {
            self.close_review(window, cx);
        }
        cx.notify();
    }

    /// Asks the session to join `pair`. Its Join goes as the picks clear, so
    /// the keyboard waits where Undo will come.
    fn join(&mut self, pair: RoomPair, window: &mut Window, cx: &mut Context<MapView>) {
        if self.model.joining() == Some(pair) {
            return;
        }
        if self.join_button.is_focused(window) {
            window.focus(&self.undo_button, cx);
        }
        self.model.await_join(pair);
        cx.emit(MapEdit::Join {
            first: pair.0,
            second: pair.1,
        });
        cx.notify();
    }

    fn undo(&mut self, cx: &mut Context<MapView>) {
        if let Some(JoinNote::Joined { created, .. }) = self.model.join_note() {
            cx.emit(MapEdit::UndoJoin { created: *created });
        }
        self.model.clear_join_note();
        cx.notify();
    }

    /// The quiet line above the map, with its way into Review.
    pub(super) fn duplicates_line(
        &self,
        theme: &Theme,
        cx: &mut Context<MapView>,
    ) -> Option<impl IntoElement + use<>> {
        let rooms = self.model.duplicates().rooms.len();
        if rooms == 0 || self.model.reviewing() {
            return None;
        }
        let count = format!("{rooms} rooms");
        let text = format!("{count} look like duplicates");
        Some(
            div()
                .id("map-duplicates")
                .debug_selector(|| "map-duplicates".to_owned())
                .role(Role::Status)
                .aria_label(SharedString::from(text))
                .flex()
                .flex_none()
                .items_center()
                .gap_2()
                .mb_2()
                .px_3()
                .py_1p5()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .bg(theme.surface_raised)
                .text_size(px(12.))
                .text_color(theme.text_muted)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .gap_1()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.text)
                                .child(count),
                        )
                        .child("look like duplicates"),
                )
                .child(
                    small_button("map-review", "Review duplicates", theme)
                        .track_focus(&self.review_button)
                        .child("Review")
                        .on_click(cx.listener(|view, _, window, cx| view.open_review(window, cx))),
                ),
        )
    }

    /// The map framed on both copies, and what tells them apart.
    pub(super) fn review_pane<M: IntoElement>(
        &self,
        pair: ReviewPair,
        map: M,
        theme: &Theme,
        cx: &mut Context<MapView>,
    ) -> impl IntoElement + use<M> {
        let title = format!("Review duplicates · {} of {}", pair.number, pair.of);
        let rooms = pair.pair;
        div()
            .id("map-review-pane")
            .debug_selector(|| "map-review".to_owned())
            .key_context(REVIEW_CONTEXT)
            .on_action(cx.listener(|view, _: &CloseReview, window, cx| {
                view.close_review(window, cx);
            }))
            .role(Role::Group)
            .aria_label(SharedString::from(title.clone()))
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_size(px(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(title)
                    .child(
                        small_button("map-review-close", "Close Review", theme)
                            .border_color(gpui::transparent_black())
                            .text_color(theme.text_muted)
                            .child("Close")
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.close_review(window, cx);
                            })),
                    ),
            )
            // In a short window the map gives way, and the actions stay.
            .child(div().h(REVIEW_MAP_HEIGHT).min_h_0().child(map))
            // Only the comparison scrolls: the map and actions stay in view.
            .child(
                div()
                    .id("map-review-details")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(verdict(&pair, theme))
                    .child(comparison(&pair, theme))
                    .child(description(&pair.description, theme))
                    .child(
                        div()
                            .id("map-review-join-note")
                            .role(Role::Label)
                            .aria_label(SharedString::from(pair.join_note.clone()))
                            .text_size(px(12.))
                            .text_color(theme.text_muted)
                            .child(pair.join_note),
                    ),
            )
            .child(self.review_actions(rooms, theme, cx))
            .children(self.review_note(theme, cx))
    }

    fn review_actions(
        &self,
        rooms: RoomPair,
        theme: &Theme,
        cx: &mut Context<MapView>,
    ) -> impl IntoElement + use<> {
        div()
            .flex()
            .flex_wrap()
            .gap_2()
            .child(
                small_button("map-review-skip", "Skip this pair", theme)
                    .track_focus(&self.review_first)
                    .child("Skip")
                    .on_click(cx.listener(|view, _, _, cx| view.skip_pair(cx))),
            )
            .child(
                small_button("map-review-different", "Not the same", theme)
                    .child("Not the same")
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.not_the_same(rooms, window, cx);
                    })),
            )
            .child(
                small_button("map-review-join", "Join these rooms", theme)
                    .border_color(theme.accent)
                    .text_color(theme.accent)
                    .child("Join these rooms")
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.join(rooms, window, cx);
                    })),
            )
    }

    /// How the last join went, in Review, with Undo right there.
    fn review_note(
        &self,
        theme: &Theme,
        cx: &mut Context<MapView>,
    ) -> Option<impl IntoElement + use<>> {
        let note = self.model.join_note()?;
        let (text, color, undo) = match note {
            JoinNote::Joined { label, .. } => (label.clone(), theme.text, true),
            JoinNote::Failed(error) => (failed(error), theme.error, false),
            JoinNote::Forgot(_) => return None,
        };
        Some(
            div()
                .id("map-review-note")
                .debug_selector(|| "map-review-note".to_owned())
                .role(Role::Status)
                .aria_label(SharedString::from(text.clone()))
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .text_size(px(13.))
                .text_color(color)
                .child(div().min_w_0().child(text))
                .children(undo.then(|| self.undo_control(theme, cx))),
        )
    }

    fn undo_control(&self, theme: &Theme, cx: &mut Context<MapView>) -> Stateful<Div> {
        small_button("map-join-undo", "Undo the join", theme)
            .track_focus(&self.undo_button)
            .child("Undo")
            .on_click(cx.listener(|view, _, _, cx| view.undo(cx)))
    }

    /// The pill over the map: what's picked, how the last join went, or
    /// which room was forgotten.
    pub(super) fn pill(
        &self,
        theme: &Theme,
        cx: &mut Context<MapView>,
    ) -> Option<impl IntoElement + use<>> {
        let picks = self.model.picks().len();
        let (text, hint): (String, Option<&str>) = match (picks, self.model.join_note()) {
            (0, Some(JoinNote::Joined { label, .. })) => (label.clone(), None),
            (0, Some(JoinNote::Failed(error))) => (failed(error), None),
            (0, Some(JoinNote::Forgot(name))) => {
                (format!("Forgot {name}"), Some("walk in to map it again"))
            }
            (0, None) => return None,
            (1, _) => ("1 room picked".to_owned(), Some("Pick one more")),
            (count, _) => (format!("{count} rooms picked"), None),
        };
        let spoken = hint.map_or_else(|| text.clone(), |hint| format!("{text}. {hint}"));
        let actions = if picks > 0 {
            let clear = small_button("map-picks-clear", "Clear the rooms picked", theme)
                .child("Clear")
                .on_click(cx.listener(|view, _, _, cx| {
                    view.model.clear_picks();
                    cx.notify();
                }));
            let pair = self.model.picked_pair();
            let join = pair.map(|pair| {
                small_button("map-picks-join", "Join the rooms picked", theme)
                    .track_focus(&self.join_button)
                    .border_color(theme.accent)
                    .text_color(theme.accent)
                    .child("Join")
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.join(pair, window, cx);
                    }))
            });
            Some(clear).into_iter().chain(join).collect::<Vec<_>>()
        } else {
            let joined = matches!(self.model.join_note(), Some(JoinNote::Joined { .. }));
            joined
                .then(|| self.undo_control(theme, cx))
                .into_iter()
                .collect()
        };
        Some(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom_8()
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("map-pill")
                        .debug_selector(|| "map-pill".to_owned())
                        .role(Role::Status)
                        .aria_label(SharedString::from(spoken))
                        // As on a room card: typing stays with the bar.
                        .capture_any_mouse_down(|_, window, _| window.prevent_default())
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .flex()
                        .items_center()
                        .gap_2p5()
                        .px_2p5()
                        .py_1p5()
                        .rounded_lg()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.surface_raised)
                        .text_size(px(12.))
                        .text_color(theme.text_muted)
                        .child(
                            div()
                                .debug_selector({
                                    let text = text.clone();
                                    move || text
                                })
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.text)
                                .child(text),
                        )
                        .children(hint)
                        .children(actions),
                ),
        )
    }

    /// What can be done with the room shown: pick it to join, for anyone
    /// who'd rather press a button than hold Shift, or forget it.
    pub(super) fn room_actions(
        &self,
        room: RoomId,
        theme: &Theme,
        cx: &mut Context<MapView>,
    ) -> Option<AnyElement> {
        if self.model.reviewing() {
            return None;
        }
        let name = self.model.room_name(room)?;
        if let Some(question) = self.forget_question(room, name, theme, cx) {
            return Some(question.into_any_element());
        }
        let pick = self.model.can_pick(room).then(|| {
            let picked = self.model.picks().contains(&room);
            small_button("map-pick", &format!("Pick {name} to join"), theme)
                .aria_toggled(picked.into())
                .child(if picked { "Picked" } else { "Pick to join" })
                .on_click(cx.listener(|view, _, window, cx| {
                    view.pick_selected(window, cx);
                }))
        });
        let forget = self.forget_button(room, name, theme, cx);
        Some(
            div()
                .flex()
                .flex_none()
                .justify_end()
                .gap_2()
                .pt_2()
                .children(pick)
                .child(forget)
                .into_any_element(),
        )
    }
}

/// A join or undo the session refused, in the player's words.
fn failed(error: &str) -> String {
    format!("The map didn't change: {error}")
}

pub(super) fn small_button(id: &'static str, name: &str, theme: &Theme) -> Stateful<Div> {
    button(id, SharedString::from(name.to_owned()), theme)
        .px_2p5()
        .py_1()
        .text_size(px(13.))
        .text_color(theme.text)
}

/// The copy's number in its own color, as the map marks it.
fn copy_mark(index: usize, theme: &Theme) -> Div {
    div()
        .flex_none()
        .size(px(16.))
        .rounded(px(4.))
        .flex()
        .items_center()
        .justify_center()
        .bg(theme.map.copies[index])
        .text_size(px(11.))
        .font_weight(FontWeight::BOLD)
        .text_color(theme.map.on_copy)
        .child((index + 1).to_string())
}

/// Where the copies are from each other, its lead in bold.
fn verdict(pair: &ReviewPair, theme: &Theme) -> impl IntoElement + use<> {
    let text = SharedString::from(pair.verdict.text());
    let bold = HighlightStyle {
        font_weight: Some(FontWeight::SEMIBOLD),
        color: Some(theme.text),
        ..HighlightStyle::default()
    };
    div()
        .id("map-review-verdict")
        .role(Role::Label)
        .aria_label(text.clone())
        .text_size(px(13.))
        .text_color(theme.text_muted)
        .child(StyledText::new(text).with_highlights([(0..pair.verdict.lead.len(), bold)]))
}

/// A column per copy and a row per fact. A row that differs is in the
/// warning color, marked ≠ and said to differ; the rest are dimmed.
fn comparison(pair: &ReviewPair, theme: &Theme) -> impl IntoElement + use<> {
    let label_cell = || div().flex_none().w(COMPARE_LABEL_WIDTH);
    let value_cell = || div().flex_1().min_w_0();
    let heads = COPY_SIDES.iter().enumerate().map(|(index, side)| {
        let name = format!("{side}: {}", pair.names[index]);
        value_cell()
            .id(*side)
            .role(Role::Group)
            .aria_label(SharedString::from(name))
            .flex()
            .items_center()
            .gap_1p5()
            .child(copy_mark(index, theme))
            .child(*side)
    });
    let rows = pair.rows.iter().enumerate().map(|(index, row)| {
        let differs = row.differs();
        let [first, second] = &row.values;
        let spoken = if differs {
            format!("{} differs: {first}, {second}", row.label)
        } else {
            format!("{}, same in both: {first}", row.label)
        };
        let color = if differs {
            theme.warning
        } else {
            theme.text_muted
        };
        let label = label_cell()
            .text_color(theme.text_muted)
            .child(row.label.clone())
            .when(differs, |label| {
                label
                    .flex()
                    .gap_1()
                    .child(div().text_color(theme.warning).child("≠"))
            });
        div()
            .id(("map-review-row", index))
            .role(Role::Group)
            .aria_label(SharedString::from(spoken))
            .flex()
            .gap_2()
            .py_1p5()
            .border_t_1()
            .border_color(theme.border)
            .text_color(color)
            .child(label)
            .children(
                row.values
                    .iter()
                    .map(|value| value_cell().child(value.clone())),
            )
    });
    div()
        .flex()
        .flex_col()
        .text_size(px(13.))
        .child(
            div()
                .flex()
                .gap_2()
                .pb_1p5()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(label_cell())
                .children(heads),
        )
        .children(rows)
}

/// The description once when the copies share it, or each copy's.
fn description(description: &Description, theme: &Theme) -> impl IntoElement + use<> {
    let part = |label: String, text: &str| {
        let text = if text.is_empty() { "Not read" } else { text };
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(theme.text_muted)
                    .child(label),
            )
            .child(text.to_owned())
    };
    let parts: Vec<Div> = match description {
        Description::Same(text) if text.is_empty() => Vec::new(),
        Description::Same(text) => vec![part("DESCRIPTION · SAME IN BOTH".to_owned(), text)],
        Description::Different(texts) => (COPY_SIDES.iter().zip(texts))
            .map(|(side, text)| part(format!("DESCRIPTION · {}", side.to_uppercase()), text))
            .collect(),
    };
    div()
        .when(!parts.is_empty(), |shown| {
            shown
                .flex()
                .flex_col()
                .gap_2()
                .px_3()
                .py_2p5()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.surface_raised)
                .text_size(px(13.))
                .text_color(theme.text)
        })
        .children(parts)
}
