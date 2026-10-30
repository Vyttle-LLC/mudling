//! One frame of the map: what's in view, drawn from the sheet as it stands.
//!
//! Nothing here allocates per room or looks at a room outside the view.
//! Rooms and links are quads, the cheapest thing gpui draws. Marks that need
//! a diagonal share one path per frame, and a room too small to read them
//! drops them.
//!
//! Each kind of thing paints in a layer of its own. Outside a layer, gpui
//! searches a tree of everything drawn so far for each quad to find its draw
//! order, and with 2,000 rooms in view that search was most of the frame.
//! Inside a layer they share one order and keep the order they're painted in.

use super::camera::{Camera, Region, SQUARE};
use super::sheet::{Link, Sheet, Tile, TileKind};
use crate::theme::MapColors;
use gpui::{
    BorderStyle, Bounds, ContentMask, Corners, Edges, Hsla, Path, PathBuilder, Pixels, Point,
    Window, fill, point, px, quad, size, transparent_black,
};
use mudling_core::map::Direction;
use std::sync::Arc;

/// The room's share of its square at any zoom, as the canvas draws it.
const ROOM_SHARE: f32 = 28. / SQUARE;
const RING_SHARE: f32 = 38. / 28.;
const YOU_SHARE: f32 = 12. / 28.;
const CROSS_SHARE: f32 = 0.25;
const ARROW_SHARE: f32 = 0.16;
/// Below this, in pixels, a room is a plain square.
const DETAILED_ROOM: f32 = 8.;
/// How far past the view to look: rooms overhang their squares, and rings
/// overhang rooms.
const MARGIN: f32 = 1.;

pub struct Scene {
    pub sheet: Arc<Sheet>,
    pub camera: Camera,
    pub you: Option<((i32, i32), bool)>,
    pub selected: Option<(i32, i32)>,
    pub colors: MapColors,
}

struct Frame<'a> {
    scene: &'a Scene,
    origin: Point<Pixels>,
    view: (f32, f32),
    room: f32,
    line: f32,
}

impl Frame<'_> {
    fn at(&self, (x, y): (i32, i32)) -> Point<Pixels> {
        let (x, y) = self.scene.camera.to_view((x as f32, y as f32), self.view);
        self.origin + point(px(x), px(y))
    }

    /// A square `side` pixels wide around a point.
    fn around(&self, middle: Point<Pixels>, side: f32) -> Bounds<Pixels> {
        let half = px(side / 2.);
        Bounds::new(middle - point(half, half), size(px(side), px(side)))
    }
}

pub fn paint(scene: &Scene, bounds: Bounds<Pixels>, window: &mut Window) {
    let square = scene.camera.square_size();
    let frame = Frame {
        scene,
        origin: bounds.origin,
        view: (f32::from(bounds.size.width), f32::from(bounds.size.height)),
        room: square * ROOM_SHARE,
        line: (2. * scene.camera.zoom).clamp(1., 3.),
    };
    let region = scene.camera.region(frame.view);
    let region = Region {
        west: region.west - MARGIN,
        east: region.east + MARGIN,
        south: region.south - MARGIN,
        north: region.north + MARGIN,
    };
    let mask = ContentMask { bounds };
    window.with_content_mask(Some(mask), |window| {
        window.paint_layer(bounds, |window| {
            for link in scene.sheet.links_in(region) {
                paint_link(&frame, link, window);
            }
        });
        window.paint_layer(bounds, |window| {
            for tile in scene.sheet.tiles_in(region) {
                paint_tile(&frame, tile, window);
            }
        });
        if frame.room >= DETAILED_ROOM {
            window.paint_layer(bounds, |window| paint_marks(&frame, region, window));
        }
        window.paint_layer(bounds, |window| {
            if let Some((square, guess)) = scene.you {
                paint_you(&frame, square, guess, window);
            }
            if let Some(square) = scene.selected {
                paint_selection(&frame, square, window);
            }
        });
    });
}

/// A straight link is one quad; a stretched one bends once on the way.
fn paint_link(frame: &Frame, link: &Link, window: &mut Window) {
    let color = frame.scene.colors.link;
    let (from, to) = (frame.at(link.from), frame.at(link.to));
    let bend = point(to.x, from.y);
    for (start, end) in [(from, bend), (bend, to)] {
        if start != end {
            window.paint_quad(fill(segment(start, end, frame.line), color));
        }
    }
}

/// The quad along a horizontal or vertical run.
fn segment(start: Point<Pixels>, end: Point<Pixels>, width: f32) -> Bounds<Pixels> {
    let half = px(width / 2.);
    let corner = |p: Point<Pixels>, q: Point<Pixels>| point(p.x.min(q.x), p.y.min(q.y));
    let far = |p: Point<Pixels>, q: Point<Pixels>| point(p.x.max(q.x), p.y.max(q.y));
    Bounds::from_corners(
        corner(start, end) - point(half, half),
        far(start, end) + point(half, half),
    )
}

fn paint_tile(frame: &Frame, tile: &Tile, window: &mut Window) {
    let colors = &frame.scene.colors;
    let middle = frame.at((tile.x, tile.y));
    let bounds = frame.around(middle, frame.room);
    let detailed = frame.room >= DETAILED_ROOM;
    if detailed {
        for direction in tile.leaves.iter() {
            paint_way_out(frame, middle, direction, window);
        }
    }
    let house = matches!(tile.kind, TileKind::House { .. });
    let background = if house { colors.building } else { colors.room };
    if !detailed {
        let color = if tile.aggressive {
            colors.aggressive
        } else {
            background
        };
        window.paint_quad(fill(bounds, color));
        return;
    }
    let border = match tile.kind {
        TileKind::Room => colors.room_border,
        TileKind::Indoors | TileKind::House { .. } => colors.building,
        TileKind::Maze => colors.maze,
    };
    window.paint_quad(framed(bounds, background, border, 1., corner(frame.room)));
    match tile.kind {
        TileKind::Maze => {
            let inner = frame.around(middle, frame.room * 0.5);
            window.paint_quad(framed(inner, colors.room, border, 1., 0.));
        }
        TileKind::House { .. } => {
            let door = frame.room / 4.;
            let origin = middle + point(px(-door / 2.), px(frame.room / 2. - door * 1.5));
            let bounds = Bounds::new(origin, size(px(door), px(door * 1.5)));
            window.paint_quad(fill(bounds, colors.ground));
        }
        TileKind::Room | TileKind::Indoors => {}
    }
}

/// A short stub off the room's edge: this way leads out of the area.
fn paint_way_out(frame: &Frame, middle: Point<Pixels>, direction: Direction, window: &mut Window) {
    let offset = direction.offset();
    let reach = frame.room / 2. + frame.scene.camera.square_size() * 0.15;
    let end = middle + point(px(offset.x as f32 * reach), px(-offset.y as f32 * reach));
    window.paint_quad(fill(
        segment(middle, end, frame.line),
        frame.scene.colors.muted,
    ));
}

fn corner(room: f32) -> f32 {
    room / 7.
}

fn framed(
    bounds: Bounds<Pixels>,
    background: Hsla,
    border: Hsla,
    width: f32,
    radius: f32,
) -> gpui::PaintQuad {
    quad(
        bounds,
        Corners::all(px(radius)),
        background,
        Edges::all(px(width)),
        border,
        BorderStyle::Solid,
    )
}

/// The crosses over aggressive rooms, and the arrows for floors above and
/// below, each as one path.
fn paint_marks(frame: &Frame, region: Region, window: &mut Window) {
    let reach = frame.room * CROSS_SHARE;
    let arrow = frame.room * ARROW_SHARE;
    let rise = frame.room * 0.5 - arrow * 1.5;
    let mut crosses = PathBuilder::stroke(px(frame.line.max(1.5)));
    let mut arrows = PathBuilder::fill();
    let (mut any_cross, mut any_arrow) = (false, false);
    for tile in frame.scene.sheet.tiles_in(region) {
        let middle = frame.at((tile.x, tile.y));
        if tile.aggressive {
            any_cross = true;
            let (x, y) = (px(reach), px(reach));
            crosses.move_to(middle - point(x, y));
            crosses.line_to(middle + point(x, y));
            crosses.move_to(middle + point(x, -y));
            crosses.line_to(middle + point(-x, y));
        }
        if tile.up {
            any_arrow = true;
            let tip = middle - point(px(0.), px(rise + arrow));
            arrows.add_polygon(
                &[
                    tip,
                    tip + point(px(arrow), px(arrow)),
                    tip + point(px(-arrow), px(arrow)),
                ],
                true,
            );
        }
        if tile.down {
            any_arrow = true;
            let tip = middle + point(px(0.), px(rise + arrow));
            arrows.add_polygon(
                &[
                    tip,
                    tip + point(px(arrow), px(-arrow)),
                    tip + point(px(-arrow), px(-arrow)),
                ],
                true,
            );
        }
    }
    let colors = &frame.scene.colors;
    if any_arrow {
        paint_built(arrows.build(), colors.muted, window);
    }
    if any_cross {
        paint_built(crosses.build(), colors.aggressive, window);
    }
}

fn paint_built<E>(path: Result<Path<Pixels>, E>, color: Hsla, window: &mut Window) {
    if let Ok(path) = path {
        window.paint_path(path, color);
    }
}

/// The ring and square of the canvas; hollow when the square is a guess.
fn paint_you(frame: &Frame, square: (i32, i32), guess: bool, window: &mut Window) {
    let colors = &frame.scene.colors;
    let middle = frame.at(square);
    let ring = frame.around(middle, frame.room * RING_SHARE);
    let radius = corner(frame.room);
    window.paint_quad(framed(ring, colors.room, colors.you, 2., radius));
    let you = frame.around(middle, frame.room * YOU_SHARE);
    let inside = if guess { colors.room } else { colors.you };
    window.paint_quad(framed(you, inside, colors.you, 1.5, radius / 2.));
}

fn paint_selection(frame: &Frame, square: (i32, i32), window: &mut Window) {
    let middle = frame.at(square);
    let side = frame.room * RING_SHARE + 4.;
    let ring = frame.around(middle, side);
    let color = frame.scene.colors.selected;
    let radius = corner(frame.room) + 1.;
    window.paint_quad(framed(ring, transparent_black(), color, 1.5, radius));
}
