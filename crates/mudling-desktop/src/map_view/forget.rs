//! Forgetting a wrongly mapped room: its button under the map, and the one
//! question before it goes. Walking in again maps the room afresh, so there's
//! no Undo.

use super::MapView;
use super::join::small_button;
use crate::theme::Theme;
use gpui::{
    App, Context, Div, FocusHandle, FontWeight, Role, SharedString, Stateful, Window, actions, div,
    prelude::*, px,
};
use mudling_core::map::RoomId;
use mudling_core::session::MapEdit;

/// The key context while focus is in the question, deeper than the dock's Escape.
pub(super) const QUESTION_CONTEXT: &str = "MapForget";
const GOES_WITH_IT: &str =
    "Its paths, note and features go with it. Walk in again to map it fresh.";

actions!(map_view, [KeepRoom]);

pub(super) struct Forgetting {
    /// The room the question is about, while it's asked.
    asking: Option<RoomId>,
    button: FocusHandle,
    cancel: FocusHandle,
    confirm: FocusHandle,
}

impl Forgetting {
    pub(super) fn new(cx: &mut App) -> Forgetting {
        Forgetting {
            asking: None,
            button: cx.focus_handle().tab_stop(true),
            cancel: cx.focus_handle().tab_stop(true),
            confirm: cx.focus_handle().tab_stop(true),
        }
    }
}

impl MapView {
    /// Another room selected, or Review opened, takes the question back.
    pub(super) fn drop_stale_question(&mut self) {
        let selected = self.model.selection().map(|selection| selection.room);
        if self.forget.asking != selected || self.model.reviewing() {
            self.forget.asking = None;
        }
    }

    fn ask_to_forget(&mut self, room: RoomId, window: &mut Window, cx: &mut Context<MapView>) {
        self.forget.asking = Some(room);
        window.focus(&self.forget.cancel, cx);
        cx.notify();
    }

    /// Asks about the room shown, for the accessibility check.
    #[cfg(target_os = "macos")]
    pub fn forget_sample(&mut self, window: &mut Window, cx: &mut Context<MapView>) {
        if let Some(room) = self.model.selection().map(|selection| selection.room) {
            self.ask_to_forget(room, window, cx);
        }
    }

    /// Takes the question back, and focus to the button that asked it.
    pub fn keep_room(&mut self, window: &mut Window, cx: &mut Context<MapView>) {
        self.forget.asking = None;
        window.focus(&self.forget.button, cx);
        cx.notify();
    }

    /// Asks the session to forget the room. Its button goes with the
    /// selection, so the keyboard moves on if it was in the question.
    fn forget(&mut self, window: &mut Window, cx: &mut Context<MapView>) {
        let Some(room) = self.forget.asking.take() else {
            return;
        };
        if self.forget.cancel.is_focused(window) || self.forget.confirm.is_focused(window) {
            self.focus_what_remains(window, cx);
        }
        self.model.await_forget(room);
        cx.emit(MapEdit::Forget { room });
        cx.notify();
    }

    pub(super) fn forget_button(
        &self,
        room: RoomId,
        name: &str,
        theme: &Theme,
        cx: &mut Context<MapView>,
    ) -> Stateful<Div> {
        small_button("map-forget", &format!("Forget room {name}"), theme)
            .track_focus(&self.forget.button)
            .child("Forget room")
            .on_click(cx.listener(move |view, _, window, cx| {
                view.ask_to_forget(room, window, cx);
            }))
    }

    /// The question in the row's place, while it's asked about `room`.
    pub(super) fn forget_question(
        &self,
        room: RoomId,
        name: &str,
        theme: &Theme,
        cx: &mut Context<MapView>,
    ) -> Option<impl IntoElement + use<>> {
        if self.forget.asking != Some(room) {
            return None;
        }
        let title = format!("Forget {name}?");
        Some(
            div()
                .id("map-forget-question")
                .debug_selector(|| "map-forget-question".to_owned())
                .key_context(QUESTION_CONTEXT)
                .on_action(cx.listener(|view, _: &KeepRoom, window, cx| {
                    view.keep_room(window, cx);
                }))
                .role(Role::Alert)
                .aria_label(SharedString::from(format!("{title} {GOES_WITH_IT}")))
                .flex_none()
                .mt_2()
                .p_2p5()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.surface_raised)
                .child(
                    div()
                        .mb_1()
                        .text_size(px(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child(title),
                )
                .child(
                    div()
                        .mb_2()
                        .text_size(px(12.))
                        .text_color(theme.text_muted)
                        .child(GOES_WITH_IT),
                )
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            small_button("map-forget-cancel", &format!("Keep {name}"), theme)
                                .track_focus(&self.forget.cancel)
                                .child("Cancel")
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.keep_room(window, cx);
                                })),
                        )
                        .child(
                            small_button("map-forget-confirm", &format!("Forget {name}"), theme)
                                .track_focus(&self.forget.confirm)
                                .border_color(theme.danger)
                                .text_color(theme.danger)
                                .child("Forget")
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.forget(window, cx);
                                })),
                        ),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map_view::MapLayout;
    use crate::map_view::harness::{grid_map, versioned};
    use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, WindowAppearance};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// Room 12 of a grid with room numbers, so no Pick to join beside Forget.
    const ROOM: RoomId = 12;
    const FORGET_ROOM: &str = "Forget room Square 2, 1";
    const KEEP: &str = "Keep Square 2, 1";
    const FORGET: &str = "Forget Square 2, 1";

    fn selected_view(
        cx: &mut TestAppContext,
    ) -> (
        Entity<MapView>,
        Rc<RefCell<Vec<MapEdit>>>,
        &mut VisualTestContext,
    ) {
        cx.update(|cx| {
            cx.set_global(Theme::for_appearance(WindowAppearance::Dark));
            crate::bind_keys(cx);
        });
        let (view, cx) =
            cx.add_window_view(|_, cx| MapView::new(versioned(grid_map(10)), MapLayout::Panel, cx));
        let heard: Rc<RefCell<Vec<MapEdit>>> = Rc::default();
        let log = heard.clone();
        cx.update(|_, cx| {
            cx.subscribe(&view, move |_, edit: &MapEdit, _| {
                log.borrow_mut().push(edit.clone());
            })
            .detach();
        });
        view.update(cx, |view, cx| view.go_to(ROOM, cx));
        cx.run_until_parked();
        (view, heard, cx)
    }

    fn click_on(name: &'static str, cx: &mut VisualTestContext) {
        let bounds = cx.debug_bounds(name);
        let bounds = bounds.unwrap_or_else(|| panic!("{name} is on show"));
        cx.simulate_click(bounds.center(), Modifiers::none());
        cx.run_until_parked();
    }

    fn asking(cx: &mut VisualTestContext) -> bool {
        cx.debug_bounds("map-forget-question").is_some()
    }

    #[gpui::test]
    fn cancel_keeps_the_room_and_forget_tells_the_session(cx: &mut TestAppContext) {
        let (view, heard, cx) = selected_view(cx);
        assert!(cx.debug_bounds("Pick Square 2, 1 to join").is_none());

        click_on(FORGET_ROOM, cx);
        assert!(asking(cx));
        view.update_in(cx, |view, window, _| {
            assert!(view.forget.cancel.is_focused(window), "Enter can't forget");
        });
        click_on(KEEP, cx);
        assert!(!asking(cx));
        assert!(heard.borrow().is_empty());
        view.read_with(cx, |view, _| {
            assert_eq!(view.selection().map(|s| s.room), Some(ROOM));
            assert!(view.model.room_name(ROOM).is_some());
        });

        click_on(FORGET_ROOM, cx);
        click_on(FORGET, cx);

        assert_eq!(*heard.borrow(), [MapEdit::Forget { room: ROOM }]);
        view.read_with(cx, |view, _| assert!(view.selection().is_none()));
        assert!(
            cx.debug_bounds("Forgot Square 2, 1").is_some(),
            "the pill says so"
        );
        assert!(cx.debug_bounds(FORGET_ROOM).is_none());
    }

    #[gpui::test]
    fn escape_backs_out_of_the_question(cx: &mut TestAppContext) {
        let (view, heard, cx) = selected_view(cx);
        click_on(FORGET_ROOM, cx);

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        assert!(!asking(cx));
        assert!(heard.borrow().is_empty());
        view.update_in(cx, |view, window, _| {
            assert!(view.forget.button.is_focused(window), "back on Forget room");
        });
    }

    #[gpui::test]
    fn selecting_another_room_backs_out_of_the_question(cx: &mut TestAppContext) {
        let (view, _, cx) = selected_view(cx);
        click_on(FORGET_ROOM, cx);

        view.update(cx, |view, cx| view.go_to(ROOM + 1, cx));
        cx.run_until_parked();
        view.update(cx, |view, cx| view.go_to(ROOM, cx));
        cx.run_until_parked();

        assert!(!asking(cx));
    }
}
