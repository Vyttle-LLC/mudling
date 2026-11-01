//! Tidying up duplicates: the line above the map, Review, and picking rooms by
//! hand. The model decides what's shown; this draws it and tells the session.
//! MUDling only ever suggests: a join starts with the player pressing Join.

use super::{MapView, model::JoinNote, model::ReviewPair, model::RoomCopy};
use crate::room_panel::button;
use crate::shell::FocusCommandBar;
use crate::theme::Theme;
use gpui::{
    App, Context, Div, FontWeight, KeyBinding, MouseButton, Role, SharedString, Stateful, Window,
    actions, div, prelude::*, px,
};
use mudling_core::map::{RoomId, RoomPair};
use mudling_core::session::MapEdit;

/// The key context while focus is in Review, deeper than the dock's Escape.
const REVIEW_CONTEXT: &str = "MapReview";
const KEEPS_BOTH: &str = "Joining keeps both copies' links, notes, features and sightings.";
const READ_THE_SAME: &str = "Same name, zone, description and exits";

actions!(map_view, [PickRoom, CloseReview]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new(secondary!("shift-j"), PickRoom, None),
        KeyBinding::new("escape", CloseReview, Some(REVIEW_CONTEXT)),
    ]);
}

impl MapView {
    /// Picks the room shown, as the keyboard's way to Shift-click. With a
    /// second room picked, focus goes to Join.
    pub fn pick_selected(&mut self, window: &mut Window, cx: &mut Context<MapView>) {
        let Some(room) = self.model.selection().map(|selection| selection.room) else {
            return;
        };
        self.model.pick(room);
        if self.model.picked_pair().is_some() {
            window.focus(&self.join_button, cx);
        }
        cx.notify();
    }

    /// Looks for duplicates here and now, as the images and checks need.
    #[cfg(any(test, target_os = "macos"))]
    pub fn find_duplicates_now(&mut self, cx: &mut Context<MapView>) {
        self.model.scan_now();
        cx.notify();
    }

    #[cfg(any(test, target_os = "macos"))]
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

    fn open_review(&mut self, window: &mut Window, cx: &mut Context<MapView>) {
        self.model.open_review();
        window.focus(&self.review_first, cx);
        cx.notify();
    }

    /// Closes Review, back on the button that opened it if there's still
    /// anything to review.
    pub fn close_review(&mut self, window: &mut Window, cx: &mut Context<MapView>) {
        self.model.close_review();
        if self.model.duplicates().pairs.is_empty() {
            window.dispatch_action(Box::new(FocusCommandBar), cx);
        } else {
            window.focus(&self.review_button, cx);
        }
        cx.notify();
    }

    fn skip_pair(&mut self, cx: &mut Context<MapView>) {
        self.model.skip_pair();
        cx.notify();
    }

    fn not_the_same(&mut self, pair: RoomPair, window: &mut Window, cx: &mut Context<MapView>) {
        self.model.answer_pair(pair, None);
        cx.emit(MapEdit::NotSame {
            first: pair.0,
            second: pair.1,
        });
        self.after_answer(window, cx);
    }

    fn join_reviewed(&mut self, pair: RoomPair, window: &mut Window, cx: &mut Context<MapView>) {
        self.model.answer_pair(pair, Some(pair.1));
        self.join(pair, cx);
        self.after_answer(window, cx);
    }

    fn after_answer(&mut self, window: &mut Window, cx: &mut Context<MapView>) {
        if self.model.review_pair().is_none() {
            self.close_review(window, cx);
        }
        cx.notify();
    }

    fn join(&mut self, (first, second): RoomPair, cx: &mut Context<MapView>) {
        self.model.clear_picks();
        cx.emit(MapEdit::Join { first, second });
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

    /// Both copies side by side, in the map's place.
    pub(super) fn review_pane(
        &self,
        pair: ReviewPair,
        theme: &Theme,
        cx: &mut Context<MapView>,
    ) -> impl IntoElement + use<> {
        let title = format!("Review duplicates · {} of {}", pair.number, pair.of);
        let rooms = pair.pair;
        let [first, second] = &pair.copies;
        let first = copy_card(first, "First copy", theme);
        let second = copy_card(second, "Second copy", theme);
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
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_size(px(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(title),
            )
            .child(div().flex().gap_3().child(first).child(second))
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(theme.text_muted)
                    .child(KEEPS_BOTH),
            )
            .children(pair.warning.map(|warning| {
                div()
                    .id("map-review-warning")
                    .role(Role::Status)
                    .aria_label(SharedString::from(warning.clone()))
                    .text_size(px(13.))
                    .text_color(theme.map.maze)
                    .child(warning)
            }))
            .child(
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
                                view.join_reviewed(rooms, window, cx);
                            })),
                    ),
            )
    }

    /// The pill over the map: what's picked, or how the last join went.
    pub(super) fn pill(
        &self,
        theme: &Theme,
        cx: &mut Context<MapView>,
    ) -> Option<impl IntoElement + use<>> {
        let picks = self.model.picks().len();
        let (text, hint): (String, Option<&str>) = match (picks, self.model.join_note()) {
            (0, Some(JoinNote::Joined { label, .. })) => (label.clone(), None),
            (0, Some(JoinNote::Failed(error))) => (format!("The map didn't change: {error}"), None),
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
                    .on_click(cx.listener(move |view, _, _, cx| view.join(pair, cx)))
            });
            Some(clear).into_iter().chain(join).collect::<Vec<_>>()
        } else {
            let joined = matches!(self.model.join_note(), Some(JoinNote::Joined { .. }));
            joined
                .then(|| {
                    small_button("map-join-undo", "Undo the join", theme)
                        .child("Undo")
                        .on_click(cx.listener(|view, _, _, cx| view.undo(cx)))
                })
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
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.text)
                                .child(text),
                        )
                        .children(hint)
                        .children(actions),
                ),
        )
    }

    /// What can be done with the room shown: the keyboard's way in, and the
    /// way for anyone who'd rather press a button than hold Shift.
    pub(super) fn room_actions(
        &self,
        room: RoomId,
        theme: &Theme,
        cx: &mut Context<MapView>,
    ) -> Option<impl IntoElement + use<>> {
        if !self.model.can_pick(room) || self.model.reviewing() {
            return None;
        }
        let picked = self.model.picks().contains(&room);
        let name = self.model.room_name(room).unwrap_or("this room");
        let label = format!("Pick {name} to join");
        Some(
            div().flex().flex_none().justify_end().pt_2().child(
                small_button("map-pick", &label, theme)
                    .aria_toggled(picked.into())
                    .child(if picked { "Picked" } else { "Pick to join" })
                    .on_click(cx.listener(|view, _, window, cx| {
                        view.pick_selected(window, cx);
                    })),
            ),
        )
    }
}

fn small_button(id: &'static str, name: &str, theme: &Theme) -> Stateful<Div> {
    button(id, SharedString::from(name.to_owned()), theme)
        .px_2p5()
        .py_1()
        .text_size(px(13.))
        .text_color(theme.text)
}

/// One copy, as the mockup's card lists it.
fn copy_card(copy: &RoomCopy, side: &'static str, theme: &Theme) -> impl IntoElement + use<> {
    let line = |text: String| div().child(text);
    let name = format!("{side}: {}", copy.name);
    div()
        .id(side)
        .role(Role::Group)
        .aria_label(SharedString::from(name))
        .flex_1()
        .min_w_0()
        .p_3()
        .flex()
        .flex_col()
        .gap_0p5()
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .bg(theme.surface_raised)
        .text_size(px(12.))
        .text_color(theme.text_muted)
        .child(
            div()
                .mb_1()
                .text_size(px(13.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(copy.name.clone()),
        )
        .child(div().text_color(theme.accent).child(READ_THE_SAME))
        .child(line(copy.mapped.to_owned()))
        .child(line(copy.held.clone()))
        .child(line(copy.sightings.clone()))
}
