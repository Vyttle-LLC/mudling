//! The map view (PRD §8): the left panel's map, with its area, floors and
//! legend, or the Room panel's small map around the player.
//!
//! The view takes the map store and nothing else: a snapshot to start from,
//! then each frame's diff through [`MapView::apply`]. `model` decides what's
//! shown and has no gpui in it; `sheet` holds what's derived from the rooms,
//! cached against store revisions and built off the UI thread; `paint` only
//! reads the finished sheet. The map never animates: motion is for panels
//! opening (PRD §8), so Reduce Motion has nothing to turn off here.

mod camera;
pub mod harness;
mod join;
mod model;
mod paint;
mod sheet;

use crate::assets::HEADING_FONT;
use crate::map_history;
use crate::room_panel::button;
use crate::theme::{MapColors, Theme};
use gpui::{
    AnyView, Bounds, Context, EventEmitter, FocusHandle, FontWeight, Hsla, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, PinchEvent, Pixels,
    Point, Render, ScrollDelta, ScrollEvent, StyleRefinement, Window, canvas, div, point,
    prelude::*, px,
};
pub use join::{PickRoom, bind_keys};
use model::ScanStep;
pub use model::{MapModel, Selection, Walk};
use mudling_core::map::RoomId;
use mudling_core::session::MapEdit;
use mudling_core::store::{Diff, MapState, Versioned};
pub use sheet::Pip;
use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;

/// A press that moves further than this is a drag, not a click.
const CLICK_SLOP: f32 = 4.;
const WHEEL_ZOOM_PER_LINE: f32 = 1.1;
const WHEEL_ZOOM_PER_PIXEL: f32 = 1.005;
/// The Room panel's map opens further out than the left panel's.
const INSET_ZOOM: f32 = 0.6;
/// One press of the inset's + or −.
const ZOOM_STEP: f32 = 1.25;
const ZOOM_KEY_SIZE: Pixels = px(26.);
/// The least height the map keeps in the panel, whatever sits under it.
const MIN_SURFACE: Pixels = px(120.);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapLayout {
    /// The left panel: pan, zoom, floors and the legend.
    Panel,
    /// The Room panel's small map: the player's surroundings, zoomed with its
    /// own + and −, and no more.
    Inset,
}

struct Press {
    last: Point<Pixels>,
    dragged: Pixels,
}

pub struct MapView {
    model: MapModel,
    layout: MapLayout,
    press: Option<Press>,
    /// Where the map was last painted, for turning clicks into squares.
    bounds: Rc<Cell<Bounds<Pixels>>>,
    /// What sits under the map in the panel: map history.
    below: Option<AnyView>,
    /// What's shown of the selected room.
    card: Option<AnyView>,
    review_button: FocusHandle,
    review_first: FocusHandle,
    join_button: FocusHandle,
    undo_button: FocusHandle,
    /// Review was open at the last draw, so a join may just have ended it.
    review_open: bool,
    /// A timer will ask about duplicates again.
    scan_waiting: bool,
}

/// Sent when a room is selected, and again when its walk is known.
impl EventEmitter<Selection> for MapView {}

/// Sent for a shell to hand the session: joins and answers about duplicates.
impl EventEmitter<MapEdit> for MapView {}

impl MapView {
    pub fn new(
        snapshot: Versioned<MapState>,
        layout: MapLayout,
        cx: &mut Context<MapView>,
    ) -> MapView {
        let mut model = MapModel::new(snapshot);
        if layout == MapLayout::Inset {
            model.zoom_by(INSET_ZOOM);
        }
        let mut view = MapView {
            model,
            layout,
            press: None,
            bounds: Rc::default(),
            below: None,
            card: None,
            review_button: cx.focus_handle().tab_stop(true),
            review_first: cx.focus_handle().tab_stop(true),
            join_button: cx.focus_handle().tab_stop(true),
            undo_button: cx.focus_handle().tab_stop(true),
            review_open: false,
            scan_waiting: false,
        };
        view.start_work(cx);
        view
    }

    pub fn mount_below(&mut self, view: AnyView, cx: &mut Context<MapView>) {
        self.below = Some(view);
        cx.notify();
    }

    /// Takes a frame's diff of the map store.
    pub fn apply(&mut self, diff: &Diff<MapState>, cx: &mut Context<MapView>) {
        self.model.apply(diff);
        self.start_work(cx);
        cx.notify();
    }

    /// The selected room and how to walk there.
    pub fn selection(&self) -> Option<&Selection> {
        self.model.selection()
    }

    pub fn model(&self) -> &MapModel {
        &self.model
    }

    /// Runs what the model needs off the UI thread, and draws the result.
    fn start_work(&mut self, cx: &mut Context<MapView>) {
        if let Some(job) = self.model.sheet_job() {
            let built = cx.background_spawn(async move { job.run() });
            cx.spawn(async move |view, cx| {
                let built = built.await;
                view.update(cx, |view, cx| {
                    view.model.finish_sheet(built);
                    view.start_work(cx);
                    cx.notify();
                })
            })
            .detach();
        }
        if let Some(job) = self.model.walk_job() {
            let found = cx.background_spawn(async move { job.run() });
            cx.spawn(async move |view, cx| {
                let found = found.await;
                view.update(cx, |view, cx| {
                    if view.model.finish_walk(found)
                        && let Some(selection) = view.model.selection().cloned()
                    {
                        cx.emit(selection);
                    }
                    view.start_work(cx);
                    cx.notify();
                })
            })
            .detach();
        }
        if self.layout == MapLayout::Panel && !self.scan_waiting {
            self.look_for_duplicates(cx);
        }
    }

    /// Off the UI thread, and not on every change: see [`MapModel::scan_step`].
    fn look_for_duplicates(&mut self, cx: &mut Context<MapView>) {
        match self.model.scan_step(Instant::now()) {
            ScanStep::Nothing => {}
            ScanStep::Run(job) => {
                let found = cx.background_spawn(async move { job.run() });
                cx.spawn(async move |view, cx| {
                    let found = found.await;
                    view.update(cx, |view, cx| {
                        view.model.finish_scan(found, Instant::now());
                        view.start_work(cx);
                        cx.notify();
                    })
                })
                .detach();
            }
            ScanStep::WaitFor(wait) => {
                self.scan_waiting = true;
                let timer = cx.background_executor().timer(wait);
                cx.spawn(async move |view, cx| {
                    timer.await;
                    view.update(cx, |view, cx| {
                        view.scan_waiting = false;
                        view.start_work(cx);
                    })
                })
                .detach();
            }
        }
    }

    fn view_size(&self) -> (f32, f32) {
        let size = self.bounds.get().size;
        (f32::from(size.width), f32::from(size.height))
    }

    fn in_view(&self, position: Point<Pixels>) -> (f32, f32) {
        let local = position - self.bounds.get().origin;
        (f32::from(local.x), f32::from(local.y))
    }

    fn pressed(&mut self, event: &MouseDownEvent, _: &mut Window, _: &mut Context<MapView>) {
        self.press = Some(Press {
            last: event.position,
            dragged: px(0.),
        });
    }

    fn moved(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<MapView>) {
        let Some(press) = self.press.as_mut().filter(|_| event.dragging()) else {
            return;
        };
        let delta = event.position - press.last;
        press.last = event.position;
        press.dragged += delta.x.abs() + delta.y.abs();
        if press.dragged > px(CLICK_SLOP) {
            self.model.pan((f32::from(delta.x), f32::from(delta.y)));
            cx.notify();
        }
    }

    fn released(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<MapView>) {
        let Some(press) = self.press.take() else {
            return;
        };
        if press.dragged > px(CLICK_SLOP) {
            return;
        }
        let point = self.in_view(event.position);
        if event.modifiers.shift && self.model.pick_at(point, self.view_size()) {
            cx.notify();
        } else if self.model.click(point, self.view_size()).is_some() {
            if let Some(selection) = self.model.selection().cloned() {
                cx.emit(selection);
            }
            self.start_work(cx);
            cx.notify();
        }
    }

    /// A trackpad pans, and zooms with ⌘ or Ctrl held. A mouse wheel zooms.
    fn scrolled(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<MapView>) {
        let anchor = self.in_view(event.position);
        let view = self.view_size();
        let zooming = event.modifiers.platform || event.modifiers.control;
        match event.delta {
            ScrollDelta::Pixels(delta) if !zooming => {
                self.model.pan((f32::from(delta.x), f32::from(delta.y)));
            }
            ScrollDelta::Pixels(delta) => {
                let factor = WHEEL_ZOOM_PER_PIXEL.powf(f32::from(delta.y));
                self.model.zoom(factor, anchor, view);
            }
            ScrollDelta::Lines(delta) => {
                self.model
                    .zoom(WHEEL_ZOOM_PER_LINE.powf(delta.y), anchor, view);
            }
        }
        cx.notify();
    }

    fn pinched(&mut self, event: &PinchEvent, _: &mut Window, cx: &mut Context<MapView>) {
        let anchor = self.in_view(event.position);
        let view = self.view_size();
        self.model.zoom(1. + event.delta, anchor, view);
        cx.notify();
    }

    fn map_surface(&self, colors: MapColors, cx: &mut Context<MapView>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let model = &self.model;
        let scene = paint::Scene {
            sheet: model.sheet().clone(),
            camera: model.camera(),
            you: model.you(),
            selected: model
                .selection()
                .and_then(|selection| model.sheet().square_of(selection.room)),
            marked: model.marked().clone(),
            picked: (model.picks().iter())
                .filter_map(|room| model.sheet().square_of(*room))
                .collect(),
            colors,
        };
        let bounds = self.bounds.clone();
        let surface = canvas(
            |_, _, _| {},
            move |painted, _, window, _| {
                bounds.set(painted);
                paint::paint(&scene, painted, window);
            },
        )
        .size_full();
        let frame = div()
            .id("map")
            .relative()
            .size_full()
            .overflow_hidden()
            .rounded(px(12.))
            .border_1()
            .border_color(colors.edge)
            .bg(colors.ground)
            .child(surface);
        match self.layout {
            MapLayout::Inset => frame.child(self.zoom_keys(&theme, cx)),
            MapLayout::Panel => frame
                .on_mouse_down(MouseButton::Left, cx.listener(Self::pressed))
                .on_mouse_move(cx.listener(Self::moved))
                .on_mouse_up(MouseButton::Left, cx.listener(Self::released))
                .on_mouse_up_out(
                    MouseButton::Left,
                    cx.listener(|view, _: &MouseUpEvent, _, _| view.press = None),
                )
                .on_scroll(cx.listener(Self::scrolled))
                .on_pinch(cx.listener(Self::pinched))
                .child(self.legend(colors, cx))
                .children(self.pill(&theme, cx)),
        }
    }

    /// The inset's + over −, in its bottom-right corner.
    fn zoom_keys(&self, theme: &Theme, cx: &mut Context<MapView>) -> impl IntoElement {
        let key = |id: &'static str, name: &'static str, label: &'static str, factor: f32| {
            button(id, name.into(), theme)
                .size(ZOOM_KEY_SIZE)
                .border_0()
                .rounded_none()
                .text_color(theme.text)
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.model.zoom_by(factor);
                    cx.notify();
                }))
                .child(label)
        };
        div()
            .absolute()
            .right_2()
            .bottom_2()
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_raised.opacity(0.92))
            .child(key("map-zoom-in", "Zoom in", "+", ZOOM_STEP))
            .child(
                key("map-zoom-out", "Zoom out", "−", ZOOM_STEP.recip())
                    .border_t_1()
                    .border_color(theme.border),
            )
    }

    fn header(&self, colors: MapColors, cx: &mut Context<MapView>) -> impl IntoElement {
        let model = &self.model;
        let title = model.area_name().unwrap_or("No map yet").to_owned();
        let floor = model.place().map_or(0, |place| place.floor);
        let mut about = vec![floor_name(floor)];
        about.insert(
            0,
            model.building_name().map(str::to_owned).unwrap_or_default(),
        );
        about.retain(|part| !part.is_empty());
        about.push(format!("{} rooms explored", model.rooms_explored()));
        let floor_button = |id: &'static str, up: bool, cx: &mut Context<MapView>| {
            let color = if model.next_floor(up).is_some() {
                colors.heading
            } else {
                colors.edge
            };
            div()
                .id(id)
                .size(px(24.))
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.model.change_floor(up);
                    view.start_work(cx);
                    cx.notify();
                }))
                .child(chevron(up, color))
        };
        div()
            .flex()
            .justify_between()
            .pb_4()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .font_family(HEADING_FONT)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_size(px(22.))
                            .text_color(colors.heading)
                            .child(title),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .text_size(px(14.))
                            .text_color(colors.muted)
                            .child(about.join(" · "))
                            .when(model.building_name().is_some(), |row| {
                                row.child(
                                    div()
                                        .id("map-outdoors")
                                        .text_color(colors.you)
                                        .child("Outside")
                                        .on_click(cx.listener(|view, _, _, cx| {
                                            view.model.outdoors();
                                            view.start_work(cx);
                                            cx.notify();
                                        })),
                                )
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(floor_button("map-floor-up", true, cx))
                    .child(floor_button("map-floor-down", false, cx)),
            )
    }

    /// The canvas's two entries, then what only some sheets have. Clicking
    /// "You" brings the view back to the player.
    fn legend(&self, colors: MapColors, cx: &mut Context<MapView>) -> impl IntoElement {
        let sheet = self.model.sheet();
        let entry = |mark: gpui::Div, label: &'static str| {
            div()
                .flex()
                .items_center()
                .gap_1p5()
                .child(mark)
                .child(label)
        };
        let swatch = |color: Hsla| div().size(px(10.)).rounded(px(1.)).bg(color);
        let ring = |color: Hsla| {
            div()
                .size(px(10.))
                .rounded(px(1.))
                .border_1()
                .border_color(color)
        };
        div()
            .absolute()
            .left_3()
            .right_3()
            .bottom_2()
            .flex()
            .flex_wrap()
            .gap_x_4()
            .text_size(px(12.))
            .text_color(colors.muted)
            .child(
                div()
                    .id("map-you")
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.model.follow();
                        view.start_work(cx);
                        cx.notify();
                    }))
                    .child(entry(swatch(colors.you), "You")),
            )
            .child(entry(
                div().text_color(colors.aggressive).child("×"),
                "Aggressive mob seen",
            ))
            .when(sheet.has_houses(), |legend| {
                legend.child(entry(swatch(colors.building), "Building"))
            })
            .when(sheet.has_mazes(), |legend| {
                legend.child(entry(ring(colors.maze), "Maze"))
            })
            .children(
                sheet
                    .pips()
                    .iter()
                    .map(|pip| entry(swatch(colors.pip(pip)).size(px(6.)), pip.label())),
            )
    }

    /// Selects a room and brings its sheet into view.
    pub fn go_to(&mut self, room: RoomId, cx: &mut Context<MapView>) {
        self.model.go_to(room);
        if let Some(selection) = self.model.selection().cloned() {
            cx.emit(selection);
        }
        self.start_work(cx);
        cx.notify();
    }

    /// Puts the selected room's card under the map.
    pub fn mount_card(&mut self, card: impl Into<AnyView>, cx: &mut Context<MapView>) {
        self.card = Some(card.into());
        cx.notify();
    }
}

/// A floor as the header names it.
fn floor_name(floor: i64) -> String {
    match floor {
        0 => "Ground floor".to_owned(),
        above if above > 0 => format!("Upper floor {above}"),
        below => format!("Lower floor {}", -below),
    }
}

/// The canvas's thin chevron.
fn chevron(up: bool, color: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, _, window, _| {
            let middle = bounds.center();
            let (wide, tall) = (px(5.), if up { px(-2.5) } else { px(2.5) });
            let mut line = PathBuilder::stroke(px(1.5));
            line.move_to(middle + point(-wide, -tall));
            line.line_to(middle + point(px(0.), tall));
            line.line_to(middle + point(wide, -tall));
            if let Ok(path) = line.build() {
                window.paint_path(path, color);
            }
        },
    )
    .size_full()
}

impl Render for MapView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<MapView>) -> impl IntoElement {
        self.keep_focus(window, cx);
        let theme = *cx.global::<Theme>();
        let colors = theme.map;
        let review = self.model.review_pair();
        let selected = self.model.selection().map(|selection| selection.room);
        let card_shown = selected.is_some() && review.is_none();
        match self.layout {
            MapLayout::Inset => div().size_full().child(self.map_surface(colors, cx)),
            MapLayout::Panel => div()
                .size_full()
                .flex()
                .flex_col()
                .child(self.header(colors, cx))
                .children(self.duplicates_line(&theme, cx))
                .child(
                    div()
                        .debug_selector(|| "map-surface".to_owned())
                        .flex_1()
                        .min_h(MIN_SURFACE)
                        .map(|slot| match review {
                            Some(pair) => slot.child(self.review_pane(pair, &theme, cx)),
                            None => slot.child(self.map_surface(colors, cx)),
                        }),
                )
                .children(selected.and_then(|room| self.room_actions(room, &theme, cx)))
                // In a short window this scrolls, and the map keeps its share.
                .child(
                    div()
                        .id("map-below")
                        .debug_selector(|| "map-below".to_owned())
                        .min_h_0()
                        .overflow_y_scroll()
                        .when(card_shown, |below| {
                            below.children(
                                self.card
                                    .clone()
                                    .map(|card| div().flex_none().pt_3().child(card)),
                            )
                        })
                        // Cached: a pan redraws the map, not the list.
                        .children(self.below.clone().map(|view| {
                            view.cached(
                                StyleRefinement::default()
                                    .flex_none()
                                    .w_full()
                                    .h(map_history::HEIGHT),
                            )
                        })),
                ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map_view::harness::{duplicated_map, grid_map, room, store_of, versioned};
    use gpui::{
        Entity, KeyUpEvent, Keystroke, Modifiers, TestAppContext, VisualTestContext,
        WindowAppearance,
    };
    use mudling_core::map::{Building, Direction, JoinDone, LastJoin, MapChange, MapDoor};
    use mudling_core::persist::Timestamp;
    use mudling_core::store::Revision;
    use std::cell::RefCell;

    #[gpui::test]
    fn clicking_a_room_selects_it_and_finds_the_walk(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_global(Theme::for_appearance(WindowAppearance::Dark)));
        let (view, cx) =
            cx.add_window_view(|_, cx| MapView::new(versioned(grid_map(10)), MapLayout::Panel, cx));
        cx.run_until_parked();

        let (bounds, camera) =
            view.read_with(cx, |view, _| (view.bounds.get(), view.model.camera()));
        let size = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        let (x, y) = camera.to_view((2., 1.), size);
        cx.simulate_click(bounds.origin + point(px(x), px(y)), Modifiers::none());
        cx.run_until_parked();

        let selection = view.read_with(cx, |view, _| view.selection().cloned());
        let selection = selection.expect("the click landed on a room");
        assert_eq!(selection.room, 12);
        let Walk::Steps(steps) = selection.walk else {
            panic!("the grid is linked");
        };
        assert_eq!(steps.len(), 3);
    }

    #[gpui::test]
    fn the_inset_opens_zoomed_out_and_its_keys_step_the_zoom(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_global(Theme::for_appearance(WindowAppearance::Dark)));
        let zoom = |view: &Entity<MapView>, cx: &mut VisualTestContext| {
            view.read_with(cx, |view, _| view.model.camera().zoom)
        };
        let (panel, panel_cx) =
            cx.add_window_view(|_, cx| MapView::new(versioned(grid_map(3)), MapLayout::Panel, cx));
        assert_eq!(zoom(&panel, panel_cx), 1.);

        let (inset, cx) =
            cx.add_window_view(|_, cx| MapView::new(versioned(grid_map(3)), MapLayout::Inset, cx));
        cx.run_until_parked();
        assert_eq!(zoom(&inset, cx), INSET_ZOOM);
        let press = |name: &'static str, times: usize, cx: &mut VisualTestContext| {
            for _ in 0..times {
                let key = cx.debug_bounds(name).expect("the inset has zoom keys");
                cx.simulate_click(key.center(), Modifiers::none());
            }
        };
        press("Zoom in", 1, cx);
        assert!((zoom(&inset, cx) - INSET_ZOOM * ZOOM_STEP).abs() < 1e-5);
        press("Zoom out", 2, cx);
        assert!((zoom(&inset, cx) - INSET_ZOOM / ZOOM_STEP).abs() < 1e-5);
        press("Zoom out", 20, cx);
        assert_eq!(zoom(&inset, cx), camera::MIN_ZOOM);
        press("Zoom in", 30, cx);
        assert_eq!(zoom(&inset, cx), camera::MAX_ZOOM);
    }

    fn heard_edits(
        view: &Entity<MapView>,
        cx: &mut VisualTestContext,
    ) -> Rc<RefCell<Vec<MapEdit>>> {
        let heard: Rc<RefCell<Vec<MapEdit>>> = Rc::default();
        let log = heard.clone();
        cx.update(|_, cx| {
            cx.subscribe(view, move |_, edit: &MapEdit, _| {
                log.borrow_mut().push(edit.clone());
            })
            .detach();
        });
        heard
    }

    /// A view of `map`, with the look for duplicates it started still to run.
    fn unscanned_view(
        map: MapState,
        cx: &mut TestAppContext,
    ) -> (Entity<MapView>, &mut VisualTestContext) {
        cx.update(|cx| {
            cx.set_global(Theme::for_appearance(WindowAppearance::Dark));
            crate::bind_keys(cx);
        });
        cx.add_window_view(|_, cx| MapView::new(versioned(map), MapLayout::Panel, cx))
    }

    fn duplicated_view(cx: &mut TestAppContext) -> (Entity<MapView>, &mut VisualTestContext) {
        let (view, cx) = unscanned_view(duplicated_map(9, 3), cx);
        cx.run_until_parked();
        (view, cx)
    }

    /// Enter activates a focused control when it's let go.
    fn press_enter(cx: &mut VisualTestContext) {
        cx.simulate_keystrokes("enter");
        cx.simulate_event(KeyUpEvent {
            keystroke: Keystroke::parse("enter").unwrap(),
        });
        cx.run_until_parked();
    }

    fn click_on(name: &'static str, cx: &mut VisualTestContext) {
        let bounds = cx.debug_bounds(name);
        let bounds = bounds.unwrap_or_else(|| panic!("{name} is on show"));
        cx.simulate_click(bounds.center(), Modifiers::none());
        cx.run_until_parked();
    }

    /// The session's answer, as it comes through the map store.
    fn answer(
        view: &Entity<MapView>,
        attempt: u64,
        outcome: Result<JoinDone, String>,
        cx: &mut VisualTestContext,
    ) {
        let diff = Diff {
            revision: Revision(attempt + 1),
            change: MapChange {
                last_join: Some(LastJoin { attempt, outcome }),
                ..MapChange::default()
            },
        };
        view.update(cx, |view, cx| view.apply(&diff, cx));
        cx.run_until_parked();
    }

    fn joined() -> Result<JoinDone, String> {
        Ok(JoinDone::Joined {
            label: "Joined two copies of a square".to_owned(),
            created: Timestamp(5),
        })
    }

    fn pick_two_by_keyboard(view: &Entity<MapView>, cx: &mut VisualTestContext) {
        let first = view.read_with(cx, |view, _| view.model.first_room());
        for room in [first, first + 1] {
            view.update(cx, |view, cx| view.go_to(room, cx));
            cx.run_until_parked();
            view.update_in(cx, |view, window, cx| view.pick_selected(window, cx));
            cx.run_until_parked();
        }
    }

    #[gpui::test]
    fn the_finder_runs_by_itself_and_never_joins_anything(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_global(Theme::for_appearance(WindowAppearance::Dark)));
        let heard: Rc<RefCell<Vec<MapEdit>>> = Rc::default();
        let log = heard.clone();
        // Listening from before the view exists: its first look starts in `new`.
        let (view, cx) = cx.add_window_view(|_, cx| {
            cx.subscribe_self(move |_, edit: &MapEdit, _| log.borrow_mut().push(edit.clone()))
                .detach();
            MapView::new(versioned(duplicated_map(9, 3)), MapLayout::Panel, cx)
        });

        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert!(!view.model.duplicates().pairs.is_empty());
        });
        assert!(heard.borrow().is_empty());
    }

    #[gpui::test]
    fn shift_clicking_where_no_room_can_be_picked_clicks_as_before(cx: &mut TestAppContext) {
        let mut street = room(1, 0, 0);
        street.links.insert(Direction::East, 20);
        let mut hall = room(20, 7, 7);
        hall.building = Some(20);
        let keep = Building {
            id: 20,
            doors: vec![MapDoor {
                outside: 1,
                direction: Direction::East,
                inside: 20,
            }],
            footprint: None,
        };
        let (view, cx) = unscanned_view(store_of(vec![street, hall], vec![keep]), cx);
        cx.run_until_parked();
        let (bounds, camera) =
            view.read_with(cx, |view, _| (view.bounds.get(), view.model.camera()));
        let size = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        let (x, y) = camera.to_view((1., 0.), size);

        cx.simulate_click(bounds.origin + point(px(x), px(y)), Modifiers::shift());
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert!(view.model.building_name().is_some(), "the house opened");
            assert!(view.model.picks().is_empty());
        });
    }

    #[gpui::test]
    fn shift_clicking_a_second_room_and_join_tells_the_session(cx: &mut TestAppContext) {
        let (view, cx) = duplicated_view(cx);
        let heard = heard_edits(&view, cx);
        let (bounds, camera) =
            view.read_with(cx, |view, _| (view.bounds.get(), view.model.camera()));
        let size = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        let at = |square| {
            let (x, y) = camera.to_view(square, size);
            bounds.origin + point(px(x), px(y))
        };
        cx.simulate_click(at((2., 1.)), Modifiers::none());
        cx.simulate_click(at((3., 1.)), Modifiers::shift());
        cx.run_until_parked();
        let pair = view.read_with(cx, |view, _| view.model.picked_pair());
        let pair = pair.expect("Shift-click picks both rooms");

        let join = cx.debug_bounds("Join the rooms picked");
        let join = join.expect("the pill offers Join");
        cx.simulate_click(join.center(), Modifiers::none());
        cx.run_until_parked();

        assert_eq!(
            *heard.borrow(),
            [MapEdit::Join {
                first: pair.0,
                second: pair.1
            }]
        );
        view.read_with(cx, |view, _| assert!(view.model.picks().is_empty()));
    }

    #[gpui::test]
    fn the_keyboard_opens_review_answers_and_closes_it(cx: &mut TestAppContext) {
        let (view, cx) = duplicated_view(cx);
        let heard = heard_edits(&view, cx);
        let review = cx.debug_bounds("Review duplicates");
        let review = review.expect("the line above the map offers Review");
        cx.simulate_click(review.center(), Modifiers::none());
        cx.run_until_parked();
        view.update_in(cx, |view, window, _| {
            assert!(view.review_first.is_focused(window), "on Skip");
        });

        // On to Not the same, as the dock's Tab goes, and press it.
        cx.update(|window, cx| window.focus_next(cx));
        cx.run_until_parked();
        press_enter(cx);
        assert!(matches!(heard.borrow()[..], [MapEdit::NotSame { .. }]));

        view.update_in(cx, |view, window, cx| {
            window.focus(&view.review_first, cx);
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        view.update_in(cx, |view, window, _| {
            assert!(!view.model.reviewing());
            assert!(view.review_button.is_focused(window), "back on Review");
        });
    }

    #[gpui::test]
    fn the_keyboard_picks_two_rooms_joins_undoes_and_keeps_its_place(cx: &mut TestAppContext) {
        let (view, cx) = duplicated_view(cx);
        let heard = heard_edits(&view, cx);
        pick_two_by_keyboard(&view, cx);
        view.update_in(cx, |view, window, _| {
            assert_eq!(view.model.picks().len(), 2);
            assert!(view.join_button.is_focused(window), "Enter joins from here");
        });

        press_enter(cx);
        answer(&view, 1, joined(), cx);
        view.update_in(cx, |view, window, _| {
            assert!(view.undo_button.is_focused(window), "Join gave way to Undo");
        });

        press_enter(cx);
        assert!(matches!(
            heard.borrow()[..],
            [MapEdit::Join { .. }, MapEdit::UndoJoin { .. }]
        ));
        view.update_in(cx, |view, window, _| {
            assert!(view.review_button.is_focused(window), "Undo has gone");
        });
    }

    #[gpui::test]
    fn picking_by_keyboard_in_review_leaves_focus_off_a_join_that_is_not_there(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = duplicated_view(cx);
        let first = view.read_with(cx, |view, _| view.model.first_room());
        view.update(cx, |view, cx| view.go_to(first, cx));
        cx.run_until_parked();
        view.update_in(cx, |view, window, cx| view.pick_selected(window, cx));
        click_on("Review duplicates", cx);
        view.update(cx, |view, cx| view.go_to(first + 1, cx));
        cx.run_until_parked();

        view.update_in(cx, |view, window, cx| view.pick_selected(window, cx));
        cx.run_until_parked();

        view.update_in(cx, |view, window, _| {
            assert!(view.model.reviewing());
            assert_eq!(view.model.picks().len(), 1, "nothing was picked");
            assert!(!view.join_button.is_focused(window));
            assert!(
                view.review_first.is_focused(window),
                "focus stayed in Review"
            );
        });
    }

    #[gpui::test]
    fn a_refused_join_leaves_the_keyboard_on_a_control_that_is_there(cx: &mut TestAppContext) {
        let (view, cx) = duplicated_view(cx);
        pick_two_by_keyboard(&view, cx);

        press_enter(cx);
        answer(&view, 1, Err("a sync is landing".to_owned()), cx);

        view.update_in(cx, |view, window, _| {
            assert!(view.review_button.is_focused(window));
        });
    }

    #[gpui::test]
    fn a_mouse_join_leaves_the_keyboard_where_it_was(cx: &mut TestAppContext) {
        let (view, cx) = duplicated_view(cx);
        view.update(cx, |view, cx| view.pick_sample(cx));
        cx.run_until_parked();

        click_on("Join the rooms picked", cx);
        answer(&view, 1, joined(), cx);

        view.update_in(cx, |view, window, _| {
            assert!(!view.undo_button.is_focused(window));
        });
    }

    #[gpui::test]
    fn review_says_how_a_join_went_and_offers_undo_there(cx: &mut TestAppContext) {
        let (view, cx) = duplicated_view(cx);
        let heard = heard_edits(&view, cx);
        click_on("Review duplicates", cx);
        let pair = |view: &MapView| view.model.review_pair().map(|shown| shown.pair);
        let asked = view.read_with(cx, |view, _| pair(view));

        click_on("Join these rooms", cx);
        answer(&view, 1, joined(), cx);

        view.read_with(cx, |view, _| {
            assert!(view.model.reviewing(), "more pairs wait");
            assert_ne!(pair(view), asked);
        });
        assert!(cx.debug_bounds("map-review-note").is_some());
        click_on("Undo the join", cx);
        assert!(matches!(
            heard.borrow()[..],
            [MapEdit::Join { .. }, MapEdit::UndoJoin { .. }]
        ));
        assert!(cx.debug_bounds("map-review-note").is_none());
    }

    #[gpui::test]
    fn a_refused_join_in_review_says_why_and_keeps_its_pair(cx: &mut TestAppContext) {
        let (view, cx) = duplicated_view(cx);
        click_on("Review duplicates", cx);
        let pair = |view: &MapView| view.model.review_pair().map(|shown| shown.pair);
        let asked = view.read_with(cx, |view, _| pair(view));

        click_on("Join these rooms", cx);
        answer(&view, 1, Err("a sync is landing".to_owned()), cx);

        view.read_with(cx, |view, _| assert_eq!(pair(view), asked));
        assert!(cx.debug_bounds("map-review-note").is_some());
        assert!(cx.debug_bounds("Undo the join").is_none());
    }

    #[gpui::test]
    fn close_leaves_review_with_nothing_answered(cx: &mut TestAppContext) {
        let (view, cx) = duplicated_view(cx);
        let heard = heard_edits(&view, cx);
        click_on("Review duplicates", cx);

        click_on("Close Review", cx);

        assert!(heard.borrow().is_empty());
        view.update_in(cx, |view, window, _| {
            assert!(!view.model.reviewing());
            assert!(view.review_button.is_focused(window), "back on Review");
        });
    }

    #[gpui::test]
    fn the_last_join_in_review_lands_on_undo(cx: &mut TestAppContext) {
        let copy = |id, x| {
            let mut copy = room(id, x, 0);
            copy.name = "A square".to_owned();
            copy
        };
        let rooms = vec![copy(1 << 40, 0), copy((1 << 40) + 1, 1)];
        let (view, cx) = unscanned_view(store_of(rooms, Vec::new()), cx);
        cx.run_until_parked();
        click_on("Review duplicates", cx);
        click_on("Join these rooms", cx);

        answer(&view, 1, joined(), cx);

        view.update_in(cx, |view, window, _| {
            assert!(!view.model.reviewing(), "nothing's left to review");
            assert!(view.undo_button.is_focused(window));
        });
        assert!(cx.debug_bounds("Undo the join").is_some());
    }

    #[test]
    fn floors_read_as_the_canvas_names_them() {
        assert_eq!(floor_name(0), "Ground floor");
        assert_eq!(floor_name(2), "Upper floor 2");
        assert_eq!(floor_name(-1), "Lower floor 1");
    }
}
