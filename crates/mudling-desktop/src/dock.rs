//! The side panels. A [`Dock`] sits on each side of the chat and holds the
//! [`Slot`]s of that side. It can be hidden, collapsed to a rail of icons, or
//! open at a width the player drags, and it slides between those.
//!
//! Later tickets fill a slot through [`crate::shell::Shell::mount`]; a dock
//! only knows the slot's name, its icon and the view to show.
//!
//! [`fit_docks`] shares the window between the two docks and the chat column
//! on every frame, so neither a drag nor a resize can take the column below
//! 80 columns of chat text. When even the narrowest panels would leave it too
//! little, open panels slide over the chat instead of pushing it aside. The
//! rail stays beside the chat: it's too slim to matter.

use crate::assets::{HEADING_FONT, Icon, UI_FONT};
use crate::chat::min_column_width;
use crate::theme::Theme;
use gpui::{
    AnyView, App, Context, Div, DragMoveEvent, Entity, FontWeight, Pixels, Role, SharedString,
    Stateful, StyleRefinement, Window, deferred, div, prelude::*, px,
};
use std::time::{Duration, Instant};

/// How long a panel takes to open or close.
const SLIDE: Duration = Duration::from_millis(180);
const RAIL_WIDTH: Pixels = px(44.);
const DEFAULT_WIDTH: Pixels = px(360.);
const MIN_WIDTH: Pixels = px(280.);
/// A panel never takes more than this share of the window.
const MAX_WINDOW_SHARE: f32 = 0.6;
const RESIZE_HANDLE_WIDTH: Pixels = px(6.);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// A place in a panel that a later ticket fills.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Map,
    Find,
    Room,
    Character,
    Gear,
    Skills,
    Notes,
}

impl Slot {
    fn on(side: Side) -> &'static [Slot] {
        match side {
            Side::Left => &[Slot::Map, Slot::Find],
            Side::Right => &[
                Slot::Room,
                Slot::Character,
                Slot::Gear,
                Slot::Skills,
                Slot::Notes,
            ],
        }
    }

    pub fn side(self) -> Side {
        if Slot::on(Side::Left).contains(&self) {
            Side::Left
        } else {
            Side::Right
        }
    }

    fn title(self) -> &'static str {
        match self {
            Slot::Map => "Map",
            Slot::Find => "Find",
            Slot::Room => "Room",
            Slot::Character => "Character",
            Slot::Gear => "Gear",
            Slot::Skills => "Skills",
            Slot::Notes => "Notes",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Slot::Map => Icon::Map,
            Slot::Find => Icon::Find,
            Slot::Room => Icon::Room,
            Slot::Character => Icon::Character,
            Slot::Gear => Icon::Gear,
            Slot::Skills => Icon::Skills,
            Slot::Notes => Icon::Notes,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockState {
    Hidden,
    Rail,
    Open,
}

/// A slide under way: the width it started from, when, and what it's showing
/// while it moves, which is the wider of the two ends.
#[derive(Clone, Copy)]
struct Slide {
    from: Pixels,
    started: Instant,
    showing: DockState,
}

/// Eases out, so a panel arrives gently. `progress` runs from 0 to 1.
fn slide_width(from: Pixels, to: Pixels, progress: f32) -> Pixels {
    let eased = 1. - (1. - progress).powi(3);
    from + (to - from) * eased
}

/// The widths a resize can reach in a window this wide.
fn clamp_width(width: Pixels, window_width: Pixels) -> Pixels {
    let widest = (window_width * MAX_WINDOW_SHARE).max(MIN_WIDTH);
    width.clamp(MIN_WIDTH, widest)
}

/// Narrows two panels to fit in `room`, the wider one first. A dock that isn't
/// open comes in at 0.
fn shared_widths(left: Pixels, right: Pixels, room: Pixels) -> (Pixels, Pixels) {
    let excess = (left + right - room).max(px(0.));
    let from_wider = excess.min((left - right).abs());
    let from_both = (excess - from_wider) * 0.5;
    if left > right {
        (left - from_wider - from_both, right - from_both)
    } else {
        (left - from_both, right - from_wider - from_both)
    }
}

/// Shares `window_width` between the two docks and the chat column. The shell
/// runs this before the docks draw, so it follows every drag and resize. A
/// dragged width is kept, and comes back when the window has room for it.
pub fn fit_docks(left: &Entity<Dock>, right: &Entity<Dock>, window_width: Pixels, cx: &mut App) {
    let now = cx.background_executor().now();
    for dock in [left, right] {
        dock.update(cx, |dock, _| dock.end_finished_slide(now));
    }
    let (left_wanted, right_wanted) = (left.read(cx).open_width(), right.read(cx).open_width());
    let rails = left.read(cx).rail_width() + right.read(cx).rail_width();
    let room = window_width - min_column_width(cx) - rails;
    let (left_fitted, right_fitted) = shared_widths(left_wanted, right_wanted, room);
    let squeezed = |wanted: Pixels, fitted: Pixels| wanted > px(0.) && fitted < MIN_WIDTH;
    let overlaying = squeezed(left_wanted, left_fitted) || squeezed(right_wanted, right_fitted);
    for (dock, fitted) in [(left, left_fitted), (right, right_fitted)] {
        dock.update(cx, |dock, _| {
            dock.overlaying = overlaying;
            dock.width_limit = if overlaying {
                clamp_width(dock.width, window_width)
            } else if dock.open_width() > px(0.) {
                fitted
            } else {
                Pixels::MAX
            };
        });
    }
}

/// Marks the drag of a resize handle.
#[derive(Clone)]
struct Resizing(Side);

impl Render for Resizing {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

pub struct Dock {
    side: Side,
    state: DockState,
    /// The width the player chose; [`Dock::width_limit`] may hold it back.
    width: Pixels,
    /// What [`fit_docks`] allows this frame.
    width_limit: Pixels,
    overlaying: bool,
    selected: Slot,
    slide: Option<Slide>,
    mounted: Vec<(Slot, AnyView)>,
}

impl Dock {
    pub fn new(side: Side) -> Dock {
        Dock {
            side,
            state: DockState::Hidden,
            width: DEFAULT_WIDTH,
            width_limit: Pixels::MAX,
            overlaying: false,
            selected: Slot::on(side)[0],
            slide: None,
            mounted: Vec::new(),
        }
    }

    pub fn mount(&mut self, slot: Slot, view: AnyView, cx: &mut Context<Dock>) {
        self.mounted.retain(|(mounted, _)| *mounted != slot);
        self.mounted.push((slot, view));
        cx.notify();
    }

    /// Opens the dock, or hides it if it's open: what a shortcut or the title
    /// bar's toggle does.
    pub fn toggle(&mut self, cx: &mut Context<Dock>) {
        self.move_to(
            if self.state == DockState::Open {
                DockState::Hidden
            } else {
                DockState::Open
            },
            cx,
        );
    }

    /// Goes to `state`, sliding there unless the player asked for no motion.
    pub fn move_to(&mut self, state: DockState, cx: &mut Context<Dock>) {
        if state == self.state {
            return;
        }
        #[cfg(not(test))]
        crate::motion::follow_os(cx);
        let now = cx.background_executor().now();
        let showing = if self.width_of(state) > self.width_of(self.state) {
            state
        } else {
            self.state
        };
        self.slide = (!cx.reduce_motion()).then_some(Slide {
            from: self.shown_width(now),
            started: now,
            showing,
        });
        self.state = state;
        cx.notify();
    }

    pub fn state(&self) -> DockState {
        self.state
    }

    /// Drags the panel's chat-side edge to `x`, in window coordinates.
    pub fn resize_to(&mut self, x: Pixels, window_width: Pixels, cx: &mut Context<Dock>) {
        let width = match self.side {
            Side::Left => x,
            Side::Right => window_width - x,
        };
        self.width = clamp_width(width, window_width);
        cx.notify();
    }

    fn select(&mut self, slot: Slot, cx: &mut Context<Dock>) {
        self.selected = slot;
        self.move_to(DockState::Open, cx);
        cx.notify();
    }

    fn width_of(&self, state: DockState) -> Pixels {
        match state {
            DockState::Hidden => px(0.),
            DockState::Rail => RAIL_WIDTH,
            DockState::Open => self.width.min(self.width_limit),
        }
    }

    /// What the dock shows: while it slides, the wider of the two ends.
    fn showing(&self) -> DockState {
        self.slide.map_or(self.state, |slide| slide.showing)
    }

    fn open_width(&self) -> Pixels {
        match self.showing() {
            DockState::Open => self.width,
            _ => px(0.),
        }
    }

    fn rail_width(&self) -> Pixels {
        match self.showing() {
            DockState::Rail => RAIL_WIDTH,
            _ => px(0.),
        }
    }

    fn shown_width(&self, now: Instant) -> Pixels {
        let to = self.width_of(self.state);
        match self.slide {
            Some(slide) => {
                let progress = now.saturating_duration_since(slide.started).as_secs_f32()
                    / SLIDE.as_secs_f32();
                slide_width(slide.from, to, progress.min(1.))
            }
            None => to,
        }
    }

    fn end_finished_slide(&mut self, now: Instant) {
        self.slide = self
            .slide
            .filter(|slide| now.saturating_duration_since(slide.started) < SLIDE);
    }

    /// Ends a slide that has run its time; one still running asks for the
    /// next frame.
    fn advance_slide(&mut self, now: Instant, window: &mut Window) {
        self.end_finished_slide(now);
        if self.slide.is_some() {
            window.request_animation_frame();
        }
    }

    fn render_button(
        &self,
        icon: Icon,
        label: SharedString,
        theme: &Theme,
        on_click: impl Fn(&mut Dock, &mut Context<Dock>) + 'static,
        cx: &mut Context<Dock>,
    ) -> Stateful<Div> {
        div()
            .id(label.clone())
            .debug_selector({
                let label = label.clone();
                move || format!("rail-{label}")
            })
            .role(Role::Button)
            .aria_label(label)
            .size(px(28.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .hover(|style| style.bg(theme.hover))
            .on_click(cx.listener(move |dock, _, _, cx| on_click(dock, cx)))
            .child(icon.svg().size(px(16.)).text_color(theme.text_muted))
    }

    fn render_rail(&self, theme: &Theme, cx: &mut Context<Dock>) -> impl IntoElement {
        let mut slots = Vec::new();
        for &slot in Slot::on(self.side) {
            slots.push(self.render_button(
                slot.icon(),
                slot.title().into(),
                theme,
                move |dock, cx| dock.select(slot, cx),
                cx,
            ));
        }
        div()
            .w(RAIL_WIDTH)
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .items_center()
            .gap_2()
            .pt_3()
            .children(slots)
    }

    fn render_tabs(&self, theme: &Theme, cx: &mut Context<Dock>) -> impl IntoElement {
        let tabs: Vec<_> = Slot::on(self.side)
            .iter()
            .map(|&slot| {
                let selected = slot == self.selected;
                div()
                    .id(slot.title())
                    .role(Role::Tab)
                    .aria_label(slot.title())
                    .aria_selected(selected)
                    .flex_grow(1.)
                    .px(px(2.))
                    .h(px(30.))
                    .text_size(px(12.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .text_color(theme.text_muted)
                    .when(selected, |tab| {
                        tab.bg(theme.selected)
                            .text_color(theme.text)
                            .font_weight(FontWeight::MEDIUM)
                    })
                    .on_click(cx.listener(move |dock, _, _, cx| dock.select(slot, cx)))
                    .child(slot.title())
            })
            .collect();
        let (collapse, label) = match self.side {
            Side::Left => (Icon::CollapseLeft, "Collapse the left panel"),
            Side::Right => (Icon::CollapseRight, "Collapse the right panel"),
        };
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(
                div()
                    .id("tabs")
                    .role(Role::TabList)
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .p(px(3.))
                    .rounded_lg()
                    .bg(theme.segment)
                    .border_1()
                    .border_color(theme.border)
                    .children(tabs),
            )
            .child(self.render_button(
                collapse,
                label.into(),
                theme,
                |dock, cx| dock.move_to(DockState::Rail, cx),
                cx,
            ))
    }

    /// The panel at its full width, whatever the slide has uncovered so far,
    /// so its content slides rather than reflows.
    fn render_panel(
        &self,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Dock>,
    ) -> impl IntoElement {
        let view = self
            .mounted
            .iter()
            .find(|(slot, _)| *slot == self.selected)
            .map(|(_, view)| view.clone());
        let content = match view {
            // A cached view leaves its nodes out of the accessibility tree,
            // so a screen reader always gets the panel drawn afresh.
            Some(view) if window.is_a11y_active() => view.into_any_element(),
            // The chat draws every frame, and a panel only when it changes.
            Some(view) => view
                .cached(StyleRefinement::default().size_full())
                .into_any_element(),
            None => div()
                .font_family(HEADING_FONT)
                .font_weight(FontWeight::SEMIBOLD)
                .text_size(px(18.))
                .text_color(theme.text)
                .child(self.selected.title())
                .into_any_element(),
        };
        div()
            .w(self.width_of(DockState::Open))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .child(self.render_tabs(theme, cx))
            .child(div().flex_1().min_h_0().child(content))
    }

    fn render_resize_handle(&self, theme: &Theme, cx: &mut Context<Dock>) -> impl IntoElement {
        let side = self.side;
        let handle = div()
            .id("resize-handle")
            .debug_selector(|| "resize-handle".to_owned())
            .absolute()
            .top_0()
            .bottom_0()
            .w(RESIZE_HANDLE_WIDTH)
            .cursor_col_resize()
            .hover(|style| style.bg(theme.hover))
            .on_drag(Resizing(side), |resizing, _, _, cx| {
                cx.stop_propagation();
                cx.new(|_| resizing.clone())
            })
            .on_drag_move(
                cx.listener(|dock, event: &DragMoveEvent<Resizing>, window, cx| {
                    if event.drag(cx).0 == dock.side {
                        dock.resize_to(event.event.position.x, window.viewport_size().width, cx);
                    }
                }),
            );
        match side {
            Side::Left => handle.right_0(),
            Side::Right => handle.left_0(),
        }
    }
}

impl Render for Dock {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Dock>) -> impl IntoElement {
        let now = cx.background_executor().now();
        self.advance_slide(now, window);
        let showing = self.showing();
        if showing == DockState::Hidden {
            return div().into_any_element();
        }
        let theme = *cx.global::<Theme>();
        let shown_width = self.shown_width(now);
        let (id, label) = match self.side {
            Side::Left => ("left-dock", "Left panel"),
            Side::Right => ("right-dock", "Right panel"),
        };
        let dock = div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .role(Role::Group)
            .aria_label(label)
            .relative()
            .h_full()
            .w(shown_width)
            .flex_none()
            .flex()
            .overflow_hidden()
            .font_family(UI_FONT)
            .text_size(px(13.))
            .bg(theme.surface)
            .border_color(theme.divider)
            .when(self.side == Side::Left, |dock| {
                dock.border_r_1().justify_end()
            })
            .when(self.side == Side::Right, |dock| dock.border_l_1())
            .map(|dock| match showing {
                DockState::Open => dock
                    .child(self.render_panel(&theme, window, cx))
                    .when(self.state == DockState::Open, |dock| {
                        dock.child(self.render_resize_handle(&theme, cx))
                    }),
                _ => dock.child(self.render_rail(&theme, cx)),
            });
        if showing == DockState::Open && self.overlaying {
            // Occluded, so a click on the panel can't reach the Send button
            // or the chat under it.
            let dock = dock.occlude().shadow(theme.floating_shadow());
            let overlay = div().absolute().top_0().bottom_0().child(dock);
            // Deferred, so it paints above the chat.
            deferred(match self.side {
                Side::Left => overlay.left_0(),
                Side::Right => overlay.right_0(),
            })
            .with_priority(1)
            .into_any_element()
        } else {
            dock.into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{
        Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext, WindowAppearance, point,
    };

    fn dock_window(cx: &mut TestAppContext, side: Side) -> (Entity<Dock>, &mut VisualTestContext) {
        cx.update(|cx| cx.set_global(Theme::for_appearance(WindowAppearance::Dark)));
        cx.add_window_view(|_, _| Dock::new(side))
    }

    fn shown_width(dock: &Entity<Dock>, cx: &mut VisualTestContext) -> Pixels {
        dock.read_with(cx, |dock, cx| {
            dock.shown_width(cx.background_executor().now())
        })
    }

    #[gpui::test]
    fn a_panel_slides_open_within_the_motion_budget(cx: &mut TestAppContext) {
        let (dock, cx) = dock_window(cx, Side::Right);
        dock.update(cx, |dock, cx| dock.toggle(cx));
        assert_eq!(shown_width(&dock, cx), px(0.));

        cx.executor().advance_clock(Duration::from_millis(90));
        let halfway = shown_width(&dock, cx);
        assert!(px(0.) < halfway && halfway < DEFAULT_WIDTH, "{halfway:?}");

        cx.executor().advance_clock(Duration::from_millis(90));
        assert_eq!(shown_width(&dock, cx), DEFAULT_WIDTH);
        assert!((150..=200).contains(&SLIDE.as_millis()));
    }

    #[gpui::test]
    fn reduce_motion_opens_and_closes_at_once(cx: &mut TestAppContext) {
        let (dock, cx) = dock_window(cx, Side::Left);
        cx.update(|_, cx| cx.set_reduce_motion(true));
        dock.update(cx, |dock, cx| dock.toggle(cx));
        assert_eq!(shown_width(&dock, cx), DEFAULT_WIDTH);
        dock.update(cx, |dock, cx| dock.toggle(cx));
        assert_eq!(shown_width(&dock, cx), px(0.));
    }

    #[gpui::test]
    fn a_closing_panel_keeps_its_content_until_the_slide_ends(cx: &mut TestAppContext) {
        let (dock, cx) = dock_window(cx, Side::Left);
        cx.update(|_, cx| cx.set_reduce_motion(true));
        dock.update(cx, |dock, cx| dock.toggle(cx));
        cx.update(|_, cx| cx.set_reduce_motion(false));
        dock.update(cx, |dock, cx| dock.move_to(DockState::Rail, cx));
        let showing = |dock: &Entity<Dock>, cx: &mut VisualTestContext| {
            dock.read_with(cx, |dock, _| dock.slide.map(|slide| slide.showing))
        };
        assert_eq!(showing(&dock, cx), Some(DockState::Open));

        cx.executor().advance_clock(SLIDE);
        cx.run_until_parked();
        dock.update_in(cx, |dock, window, cx| {
            dock.advance_slide(cx.background_executor().now(), window);
        });
        assert_eq!(showing(&dock, cx), None);
        assert_eq!(shown_width(&dock, cx), RAIL_WIDTH);
    }

    #[gpui::test]
    fn the_rail_opens_on_the_slot_clicked(cx: &mut TestAppContext) {
        let (dock, cx) = dock_window(cx, Side::Right);
        cx.update(|_, cx| cx.set_reduce_motion(true));
        dock.update(cx, |dock, cx| dock.move_to(DockState::Rail, cx));
        cx.run_until_parked();

        let gear = cx.debug_bounds("rail-Gear").unwrap();
        cx.simulate_click(gear.center(), Modifiers::none());

        dock.read_with(cx, |dock, _| {
            assert_eq!(dock.state, DockState::Open);
            assert_eq!(dock.selected, Slot::Gear);
        });
    }

    #[gpui::test]
    fn dragging_the_handle_resizes_within_its_limits(cx: &mut TestAppContext) {
        let (dock, cx) = dock_window(cx, Side::Right);
        cx.update(|_, cx| cx.set_reduce_motion(true));
        dock.update(cx, |dock, cx| dock.toggle(cx));
        cx.run_until_parked();

        let handle = cx.debug_bounds("resize-handle").unwrap().center();
        let window_width = cx.update(|window, _| window.viewport_size().width);
        cx.simulate_mouse_down(handle, MouseButton::Left, Modifiers::none());
        // The first move past gpui's threshold only starts the drag.
        for _ in 0..2 {
            cx.simulate_mouse_move(
                point(window_width - px(300.), handle.y),
                MouseButton::Left,
                Modifiers::none(),
            );
        }
        dock.read_with(cx, |dock, _| assert_eq!(dock.width, px(300.)));

        cx.simulate_mouse_move(
            point(window_width - px(100.), handle.y),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.simulate_mouse_up(handle, MouseButton::Left, Modifiers::none());
        dock.read_with(cx, |dock, _| assert_eq!(dock.width, MIN_WIDTH));
        assert_eq!(clamp_width(px(5_000.), px(1_000.)), px(600.));
        assert_eq!(clamp_width(px(5_000.), px(300.)), MIN_WIDTH);
    }

    #[test]
    fn panels_give_way_wider_first_then_equally() {
        let shared = |left: f32, right: f32, room: f32| {
            let (left, right) = shared_widths(px(left), px(right), px(room));
            (f32::from(left), f32::from(right))
        };
        assert_eq!(shared(360., 360., 800.), (360., 360.), "they fit");
        assert_eq!(shared(500., 300., 700.), (400., 300.));
        assert_eq!(shared(840., 840., 900.), (450., 450.));
        assert_eq!(shared(600., 300., 500.), (250., 250.));
        assert_eq!(shared(0., 840., 450.), (0., 450.), "one panel open");
    }

    #[test]
    fn a_slot_lives_on_one_side() {
        assert_eq!(Slot::Find.side(), Side::Left);
        assert_eq!(Slot::Notes.side(), Side::Right);
    }
}
