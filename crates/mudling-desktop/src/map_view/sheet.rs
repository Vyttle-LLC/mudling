//! A sheet: one floor of one map, ready to draw.
//!
//! An area's outdoors and each building have a map of their own, and each
//! map has floors. Building a sheet walks every room, so it happens off the
//! UI thread and only when the rooms change (see `model`). Everything a draw
//! asks of a finished sheet is a binary search: its tiles and links are
//! sorted west to east, so culling never touches what's out of view.

use super::camera::Region;
use mudling_core::map::{Derived, DirSet, Direction, GridPoint, MapRoom, RoomId, feature};
use mudling_core::store::MapState;
use std::collections::{BTreeSet, HashMap, HashSet};

/// Which sheet to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub zone: String,
    /// `None` for the area's outdoors.
    pub building: Option<RoomId>,
    pub floor: i64,
}

impl Place {
    /// The sheet a room is drawn on, and its square there. A building that
    /// fits the outdoor map is drawn on it, shifted to its doors.
    pub fn of(room: &MapRoom, derived: Option<&Derived>) -> (Place, GridPoint) {
        let on_outdoors = room
            .building
            .and_then(|id| derived?.building(id)?.footprint);
        let building = room.building.filter(|_| on_outdoors.is_none());
        let square = room.position + on_outdoors.unwrap_or_default();
        let place = Place {
            zone: room.zone.clone(),
            building,
            floor: square.z,
        };
        (place, square)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileKind {
    Room,
    /// Inside a building that's drawn on the outdoor map.
    Indoors,
    Maze,
    /// A building with a map of its own, drawn as a house at its door.
    House {
        building: RoomId,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tile {
    pub room: RoomId,
    pub x: i32,
    pub y: i32,
    pub kind: TileKind,
    pub aggressive: bool,
    /// Links to the floors above and below.
    pub up: bool,
    pub down: bool,
    /// Ways out of the area.
    pub leaves: DirSet,
}

/// A path between two tiles, drawn once for both ways.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Link {
    pub from: (i32, i32),
    pub to: (i32, i32),
}

impl Link {
    fn west(&self) -> i32 {
        self.from.0.min(self.to.0)
    }

    fn east(&self) -> i32 {
        self.from.0.max(self.to.0)
    }

    fn crosses(&self, region: &Region) -> bool {
        let (south, north) = (self.from.1.min(self.to.1), self.from.1.max(self.to.1));
        self.east() as f32 >= region.west
            && north as f32 >= region.south
            && south as f32 <= region.north
    }
}

/// What a click on the sheet lands on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Room(RoomId),
    Building(RoomId),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sheet {
    /// By x, then y, then id.
    tiles: Vec<Tile>,
    /// By their west end.
    links: Vec<Link>,
    /// The most squares a link spans east to west: how far west of the view
    /// a link crossing it can start.
    widest_link: i32,
    squares: HashMap<RoomId, (i32, i32)>,
    /// Every floor of this map, lowest first, with rooms on it or not here.
    floors: Vec<i64>,
    has_houses: bool,
    has_mazes: bool,
}

impl Sheet {
    pub fn build(map: &MapState, place: &Place) -> Sheet {
        let derived = map.derived().map(|derived| &**derived);
        let mut sheet = Sheet::default();
        let mut floors = BTreeSet::new();
        let mut on_sheet = Vec::new();
        for room in map.rooms().filter(|room| room.zone == place.zone) {
            let (room_place, square) = Place::of(room, derived);
            if room_place.building != place.building {
                continue;
            }
            floors.insert(room_place.floor);
            if room_place.floor == place.floor {
                let square = (square.x as i32, square.y as i32);
                sheet.squares.insert(room.id, square);
                on_sheet.push((&**room, square));
            }
        }
        sheet.floors = floors.into_iter().collect();

        let taken: HashSet<(i32, i32)> = sheet.squares.values().copied().collect();
        sheet.tiles = on_sheet
            .iter()
            .map(|(room, square)| tile(map, room, *square, place))
            .collect();
        sheet.links = sheet.links_between(&on_sheet);
        if place.building.is_none() {
            sheet.add_houses(map, derived, place, taken);
        }
        sheet.tiles.sort_by_key(|tile| (tile.x, tile.y, tile.room));
        sheet.links.sort_by_key(Link::west);
        sheet.widest_link = sheet
            .links
            .iter()
            .map(|link| link.east() - link.west())
            .max()
            .unwrap_or(0);
        sheet.has_mazes = sheet.tiles.iter().any(|tile| tile.kind == TileKind::Maze);
        sheet
    }

    fn links_between(&self, rooms: &[(&MapRoom, (i32, i32))]) -> Vec<Link> {
        let mut seen = HashSet::new();
        let mut links = Vec::new();
        for (room, square) in rooms {
            for direction in Direction::FLAT {
                let Some(to) = room.links.get_copied(direction) else {
                    continue;
                };
                let Some(other) = self.squares.get(&to) else {
                    continue;
                };
                if to != room.id && seen.insert((room.id.min(to), room.id.max(to))) {
                    links.push(Link {
                        from: *square,
                        to: *other,
                    });
                }
            }
        }
        links
    }

    /// A house for each door of a building with a map of its own, on the
    /// first free square past the door. Up or down isn't a square, so a
    /// door that way goes east.
    fn add_houses(
        &mut self,
        map: &MapState,
        derived: Option<&Derived>,
        place: &Place,
        mut taken: HashSet<(i32, i32)>,
    ) {
        let buildings = derived.map_or(&[][..], |derived| &derived.buildings);
        for building in buildings
            .iter()
            .filter(|building| building.footprint.is_none())
        {
            for door in &building.doors {
                let Some(square) = self.squares.get(&door.outside).copied() else {
                    continue;
                };
                let inside = map.room(door.inside);
                if inside.is_none_or(|room| room.zone != place.zone) {
                    continue;
                }
                let heading = match door.direction {
                    Direction::Up | Direction::Down => Direction::East,
                    flat => flat,
                }
                .offset();
                let mut at = square;
                loop {
                    at = (at.0 + heading.x as i32, at.1 + heading.y as i32);
                    if taken.insert(at) {
                        break;
                    }
                }
                self.links.push(Link {
                    from: square,
                    to: at,
                });
                self.tiles.push(Tile {
                    room: door.inside,
                    x: at.0,
                    y: at.1,
                    kind: TileKind::House {
                        building: building.id,
                    },
                    aggressive: false,
                    up: false,
                    down: false,
                    leaves: DirSet::default(),
                });
                self.has_houses = true;
            }
        }
    }

    pub fn floors(&self) -> &[i64] {
        &self.floors
    }

    pub fn has_houses(&self) -> bool {
        self.has_houses
    }

    pub fn has_mazes(&self) -> bool {
        self.has_mazes
    }

    pub fn room_count(&self) -> usize {
        self.tiles.len()
    }

    /// Every square with a tile; `None` on an empty sheet.
    pub fn extent(&self) -> Option<Region> {
        let (first, last) = (self.tiles.first()?, self.tiles.last()?);
        let ys = || self.tiles.iter().map(|tile| tile.y as f32);
        Some(Region {
            west: first.x as f32,
            east: last.x as f32,
            south: ys().fold(f32::MAX, f32::min),
            north: ys().fold(f32::MIN, f32::max),
        })
    }

    /// The room's square here, if it's on this sheet.
    pub fn square_of(&self, room: RoomId) -> Option<(i32, i32)> {
        self.squares.get(&room).copied()
    }

    /// The tiles in a region.
    pub fn tiles_in(&self, region: Region) -> impl Iterator<Item = &Tile> {
        let start = self
            .tiles
            .partition_point(|tile| (tile.x as f32) < region.west);
        self.tiles[start..]
            .iter()
            .take_while(move |tile| tile.x as f32 <= region.east)
            .filter(move |tile| (region.south..=region.north).contains(&(tile.y as f32)))
    }

    /// The links crossing a region.
    pub fn links_in(&self, region: Region) -> impl Iterator<Item = &Link> {
        let reach = region.west - self.widest_link as f32;
        let start = self
            .links
            .partition_point(|link| (link.west() as f32) < reach);
        self.links[start..]
            .iter()
            .take_while(move |link| link.west() as f32 <= region.east)
            .filter(move |link| link.crosses(&region))
    }

    /// What's on the square under a point of the grid.
    pub fn hit(&self, (x, y): (f32, f32)) -> Option<Hit> {
        let square = (x.round() as i32, y.round() as i32);
        let first = self.tiles.partition_point(|tile| (tile.x, tile.y) < square);
        let tile = self.tiles.get(first)?;
        ((tile.x, tile.y) == square).then_some(match tile.kind {
            TileKind::House { building } => Hit::Building(building),
            _ => Hit::Room(tile.room),
        })
    }
}

fn tile(map: &MapState, room: &MapRoom, square: (i32, i32), place: &Place) -> Tile {
    let kind = if room.maze {
        TileKind::Maze
    } else if room.building.is_some() && place.building.is_none() {
        TileKind::Indoors
    } else {
        TileKind::Room
    };
    let mut leaves = DirSet::default();
    for direction in Direction::FLAT {
        let elsewhere = room
            .links
            .get_copied(direction)
            .and_then(|to| map.room(to))
            .is_some_and(|to| to.zone != room.zone);
        if elsewhere {
            leaves.insert(direction);
        }
    }
    Tile {
        room: room.id,
        x: square.0,
        y: square.1,
        kind,
        aggressive: room.features.contains(feature::AGGRESSIVE_MOB),
        up: room.links.contains(Direction::Up),
        down: room.links.contains(Direction::Down),
        leaves,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map_view::harness::{ZONE, grid_map, room, store_of};
    use mudling_core::map::{Building, MapDoor};
    use std::sync::Arc;

    fn outdoors(floor: i64) -> Place {
        Place {
            zone: ZONE.into(),
            building: None,
            floor,
        }
    }

    fn region(west: f32, east: f32, south: f32, north: f32) -> Region {
        Region {
            west,
            east,
            south,
            north,
        }
    }

    #[test]
    fn culling_keeps_only_what_a_region_shows() {
        let sheet = Sheet::build(&grid_map(50), &outdoors(0));
        assert_eq!(sheet.room_count(), 2_500);

        let view = region(10., 12., 20., 21.);
        let mut tiles: Vec<(i32, i32)> =
            sheet.tiles_in(view).map(|tile| (tile.x, tile.y)).collect();
        tiles.sort_unstable();
        assert_eq!(
            tiles,
            [(10, 20), (10, 21), (11, 20), (11, 21), (12, 20), (12, 21)]
        );
        // Two rows of four links east and west, three columns of three.
        assert_eq!(sheet.links_in(view).count(), 2 * 4 + 3 * 3);
        assert_eq!(sheet.tiles_in(region(900., 910., 0., 9.)).count(), 0);
        assert_eq!(sheet.links_in(region(900., 910., 0., 9.)).count(), 0);
    }

    #[test]
    fn a_long_link_is_kept_when_neither_end_is_in_view() {
        let mut far_west = room(1, 0, 0);
        let far_east = room(2, 100, 0);
        far_west.links.insert(Direction::East, 2);
        let map = store_of(vec![far_west, far_east], Vec::new());

        let sheet = Sheet::build(&map, &outdoors(0));
        let view = region(50., 51., -1., 1.);
        assert_eq!(sheet.tiles_in(view).count(), 0);
        assert_eq!(sheet.links_in(view).count(), 1);
        assert_eq!(sheet.links_in(region(50., 51., 5., 6.)).count(), 0);
    }

    #[test]
    fn floors_split_a_map_and_mark_the_way_up() {
        let mut ground = room(1, 0, 0);
        ground.links.insert(Direction::Up, 2);
        let mut upstairs = room(2, 0, 0);
        upstairs.position.z = 1;
        upstairs.links.insert(Direction::Down, 1);
        let map = store_of(vec![ground, upstairs], Vec::new());

        let sheet = Sheet::build(&map, &outdoors(0));
        assert_eq!(sheet.floors(), [0, 1]);
        assert_eq!(sheet.room_count(), 1);
        let tile = sheet.tiles_in(region(-1., 1., -1., 1.)).next().unwrap();
        assert!(tile.up && !tile.down);
        assert_eq!(Sheet::build(&map, &outdoors(1)).room_count(), 1);
    }

    #[test]
    fn a_building_that_fits_joins_the_outdoors_and_one_that_doesnt_is_a_house() {
        let mut street = room(1, 0, 0);
        street.links.insert(Direction::East, 10);
        street.links.insert(Direction::West, 20);
        let mut shop = room(10, 5, 5);
        shop.building = Some(10);
        let mut keep = room(20, 0, 0);
        keep.building = Some(20);
        let mut cellar = room(21, 1, 0);
        cellar.building = Some(20);
        let door = |inside, direction| MapDoor {
            outside: 1,
            direction,
            inside,
        };
        let map = store_of(
            vec![street, shop, keep, cellar],
            vec![
                Building {
                    id: 10,
                    doors: vec![door(10, Direction::East)],
                    footprint: Some(GridPoint::new(-4, -5, 0)),
                },
                Building {
                    id: 20,
                    doors: vec![door(20, Direction::West)],
                    footprint: None,
                },
            ],
        );

        let sheet = Sheet::build(&map, &outdoors(0));
        assert_eq!(sheet.hit((1., 0.)), Some(Hit::Room(10)));
        assert_eq!(sheet.hit((-1., 0.)), Some(Hit::Building(20)));
        assert!(sheet.has_houses());

        let inside = Place {
            building: Some(20),
            ..outdoors(0)
        };
        assert_eq!(Sheet::build(&map, &inside).room_count(), 2);
    }

    #[test]
    fn a_way_out_of_the_area_is_marked_on_the_room() {
        let mut gate = room(1, 0, 0);
        gate.links.insert(Direction::North, 2);
        let mut road = room(2, 0, 1);
        road.zone = "Elsewhere".into();
        let map = store_of(vec![gate, road], Vec::new());

        let sheet = Sheet::build(&map, &outdoors(0));
        assert_eq!(sheet.room_count(), 1);
        let tile = sheet.tiles_in(region(-1., 1., -1., 1.)).next().unwrap();
        assert!(tile.leaves.contains(Direction::North));
        assert_eq!(sheet.links_in(region(-1., 1., -1., 1.)).count(), 0);
    }

    #[test]
    fn a_click_misses_an_empty_square() {
        let map = store_of(vec![room(7, 3, 3)], Vec::new());
        let sheet = Arc::new(Sheet::build(&map, &outdoors(0)));
        assert_eq!(sheet.hit((3.2, 2.8)), Some(Hit::Room(7)));
        assert_eq!(sheet.hit((5., 5.)), None);
    }
}
