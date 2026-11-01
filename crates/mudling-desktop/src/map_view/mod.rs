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
mod model;
mod paint;
mod sheet;

use crate::assets::HEADING_FONT;
use crate::theme::{MapColors, Theme};
use gpui::{
    AnyView, Bounds, Context, EventEmitter, FontWeight, Hsla, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, PinchEvent, Pixels, Point, Render,
    ScrollDelta, ScrollEvent, Window, canvas, div, point, prelude::*, px,
};
pub use model::{MapModel, Selection, Walk};
use mudling_core::map::RoomId;
use mudling_core::store::{Diff, MapState, Versioned};
pub use sheet::Pip;
use std::cell::Cell;
use std::rc::Rc;

/// A press that moves further than this is a drag, not a click.
const CLICK_SLOP: f32 = 4.;
const WHEEL_ZOOM_PER_LINE: f32 = 1.1;
const WHEEL_ZOOM_PER_PIXEL: f32 = 1.005;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapLayout {
    /// The left panel: pan, zoom, floors and the legend.
    Panel,
    /// The Room panel's small map: the player's surroundings, and no more.
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
    /// What's shown of the selected room.
    card: Option<AnyView>,
}

/// Sent when a room is selected, and again when its walk is known.
impl EventEmitter<Selection> for MapView {}

impl MapView {
    pub fn new(
        snapshot: Versioned<MapState>,
        layout: MapLayout,
        cx: &mut Context<MapView>,
    ) -> MapView {
        let mut view = MapView {
            model: MapModel::new(snapshot),
            layout,
            press: None,
            bounds: Rc::default(),
            card: None,
        };
        view.start_work(cx);
        view
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
        if self.model.click(point, self.view_size()).is_some() {
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
        let model = &self.model;
        let scene = paint::Scene {
            sheet: model.sheet().clone(),
            camera: model.camera(),
            you: model.you(),
            selected: model
                .selection()
                .and_then(|selection| model.sheet().square_of(selection.room)),
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
            MapLayout::Inset => frame,
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
                .child(self.legend(colors, cx)),
        }
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<MapView>) -> impl IntoElement {
        let colors = cx.global::<Theme>().map;
        match self.layout {
            MapLayout::Inset => div().size_full().child(self.map_surface(colors, cx)),
            MapLayout::Panel => div()
                .size_full()
                .flex()
                .flex_col()
                .child(self.header(colors, cx))
                .child(div().flex_1().min_h_0().child(self.map_surface(colors, cx)))
                .when(self.model.selection().is_some(), |view| {
                    view.children(self.card.clone().map(|card| div().pt_3().child(card)))
                }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map_view::harness::{grid_map, versioned};
    use gpui::{Modifiers, TestAppContext, WindowAppearance};

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

    #[test]
    fn floors_read_as_the_canvas_names_them() {
        assert_eq!(floor_name(0), "Ground floor");
        assert_eq!(floor_name(2), "Upper floor 2");
        assert_eq!(floor_name(-1), "Lower floor 1");
    }
}
