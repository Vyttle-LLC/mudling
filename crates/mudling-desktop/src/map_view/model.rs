//! The view model: what the map view shows, with no gpui in it.
//!
//! It holds a copy of the map store, fed by store diffs. Anything that scans
//! the map (a sheet, a walk) is cached against the store revision the rooms
//! last changed at, and handed out as a job for a background thread. Panning,
//! zooming and walking known rooms ask for no work at all.

use super::camera::Camera;
use super::sheet::{Hit, Place, Sheet};
use mudling_core::map::{GridPoint, RoomId, WalkStep};
use mudling_core::store::{Diff, MapState, Revision, Versioned};
use std::sync::Arc;

type SheetKey = (Revision, Place);

/// A sheet to build, on a cheap clone of the map.
pub struct SheetJob {
    key: SheetKey,
    map: MapState,
    /// Where the view was looking when the player went to this sheet.
    arriving_from: Option<(f32, f32)>,
}

impl SheetJob {
    pub fn run(self) -> BuiltSheet {
        let sheet = Sheet::build(&self.map, &self.key.1);
        let arrival = self.arriving_from.and_then(|from| sheet.nearest(from));
        BuiltSheet {
            key: self.key,
            sheet,
            arrival,
        }
    }
}

pub struct BuiltSheet {
    key: SheetKey,
    sheet: Sheet,
    /// The square to look at on arriving here.
    arrival: Option<(i32, i32)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalkKey {
    revision: Revision,
    start: RoomId,
    goal: RoomId,
}

/// A walk to work out, on a cheap clone of the map.
pub struct WalkJob {
    key: WalkKey,
    map: MapState,
}

impl WalkJob {
    pub fn run(self) -> (WalkKey, Option<Vec<WalkStep>>) {
        let steps = self.map.route(self.key.start, self.key.goal);
        (self.key, steps)
    }
}

/// How to get to the selected room.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Walk {
    /// Worked out in the background; or the player's room isn't known yet.
    Unknown,
    /// The moves there, from `map::route`.
    Steps(Vec<WalkStep>),
    /// No explored path leads there.
    NoPath,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub room: RoomId,
    pub walk: Walk,
}

/// The player's marker.
#[derive(Clone, Debug, PartialEq, Eq)]
struct You {
    place: Place,
    square: GridPoint,
    /// In the dark or unsure of the room: the square is the map's guess.
    guess: bool,
}

#[derive(Default)]
pub struct MapModel {
    map: Versioned<MapState>,
    /// The store revision the rooms last changed at.
    rooms_changed: Revision,
    place: Option<Place>,
    you: Option<You>,
    /// The view keeps the player in the middle until the user looks away.
    following: bool,
    camera: Camera,
    sheet: Arc<Sheet>,
    built: Option<SheetKey>,
    building: Option<SheetKey>,
    /// The next sheet was asked for by going to it, so it should come into view.
    arriving: bool,
    selection: Option<Selection>,
    walk_for: Option<WalkKey>,
    walking: Option<WalkKey>,
}

impl MapModel {
    /// Starts from a snapshot of the store.
    pub fn new(snapshot: Versioned<MapState>) -> MapModel {
        let mut model = MapModel {
            rooms_changed: snapshot.revision(),
            map: snapshot,
            following: true,
            ..MapModel::default()
        };
        model.track_player();
        model
    }

    /// Takes a frame's diff of the map store.
    pub fn apply(&mut self, diff: &Diff<MapState>) {
        self.map.apply_diff(diff);
        if !diff.change.rooms.is_empty() || !diff.change.removed.is_empty() {
            self.rooms_changed = diff.revision;
        }
        self.track_player();
    }

    fn track_player(&mut self) {
        let map = self.map.value();
        let whereabouts = map.whereabouts();
        let you = whereabouts.room.and_then(|id| map.room(id)).map(|room| {
            let (place, square) = Place::of(room, map.derived().map(|derived| &**derived));
            let offset = square - room.position;
            You {
                place,
                square: whereabouts
                    .dark_position
                    .map_or(square, |dark| dark + offset),
                guess: whereabouts.dark || whereabouts.unsure,
            }
        });
        let moved_on =
            you.as_ref().map(|you| &you.place) != self.you.as_ref().map(|you| &you.place);
        self.you = you;
        self.following |= moved_on;
        if self.following {
            self.look_at_player();
        }
    }

    fn look_at_player(&mut self) {
        if let Some(you) = &self.you {
            self.place = Some(you.place.clone());
            self.camera.center = (you.square.x as f32, you.square.y as f32);
        }
    }

    /// Goes back to following the player.
    pub fn follow(&mut self) {
        self.following = true;
        self.look_at_player();
    }

    // MARK: Looking around

    pub fn camera(&self) -> Camera {
        self.camera
    }

    pub fn pan(&mut self, delta: (f32, f32)) {
        self.following = false;
        self.camera.pan_pixels(delta);
    }

    pub fn zoom(&mut self, factor: f32, anchor: (f32, f32), size: (f32, f32)) {
        self.camera.zoom_at(factor, anchor, size);
    }

    /// Fits the sheet in a view of `size` pixels.
    pub fn fit(&mut self, size: (f32, f32)) {
        if let Some(extent) = self.sheet.extent() {
            self.following = false;
            self.camera.fit(extent, size);
        }
    }

    pub fn place(&self) -> Option<&Place> {
        self.place.as_ref()
    }

    /// Goes to a sheet. Its nearest room comes into view once it's built.
    pub fn show(&mut self, place: Place) {
        self.following = false;
        self.arriving = true;
        self.place = Some(place);
    }

    /// Leaves a building for the outdoors, by its door when it has one.
    pub fn outdoors(&mut self) {
        let Some(place) = self.place.clone() else {
            return;
        };
        let map = self.map.value();
        let door = place
            .building
            .and_then(|id| map.derived()?.building(id)?.doors.first())
            .and_then(|door| map.room(door.outside));
        let floor = door.map_or(0, |room| room.position.z);
        if let Some(door) = door {
            self.camera.center = (door.position.x as f32, door.position.y as f32);
        }
        self.show(Place {
            zone: place.zone,
            building: None,
            floor,
        });
    }

    /// The nearest floor with rooms above or below this one.
    pub fn next_floor(&self, up: bool) -> Option<i64> {
        let floor = self.place.as_ref()?.floor;
        let floors = self.sheet.floors();
        if up {
            floors.iter().copied().find(|other| *other > floor)
        } else {
            floors.iter().rev().copied().find(|other| *other < floor)
        }
    }

    pub fn change_floor(&mut self, up: bool) {
        if let (Some(floor), Some(place)) = (self.next_floor(up), &self.place) {
            let place = Place {
                floor,
                ..place.clone()
            };
            self.show(place);
        }
    }

    // MARK: What's shown

    pub fn sheet(&self) -> &Arc<Sheet> {
        &self.sheet
    }

    pub fn area_name(&self) -> Option<&str> {
        let place = self.place.as_ref()?;
        let area = self.map.value().derived()?.area(&place.zone)?;
        Some(&area.name.name)
    }

    pub fn rooms_explored(&self) -> usize {
        let derived = self.map.value().derived();
        self.place
            .as_ref()
            .and_then(|place| derived?.area(&place.zone))
            .map_or(0, |area| area.rooms)
    }

    /// The name of the building being shown, by its first room.
    pub fn building_name(&self) -> Option<&str> {
        let id = self.place.as_ref()?.building?;
        Some(&self.map.value().room(id)?.name)
    }

    /// The player's square and whether it's a guess, when on this sheet.
    pub fn you(&self) -> Option<((i32, i32), bool)> {
        let you = self.you.as_ref()?;
        (Some(&you.place) == self.place.as_ref())
            .then_some(((you.square.x as i32, you.square.y as i32), you.guess))
    }

    // MARK: Background work

    /// The sheet the view needs built, if it isn't built or being built.
    pub fn sheet_job(&mut self) -> Option<SheetJob> {
        let key = (self.rooms_changed, self.place.clone()?);
        let arriving_from = std::mem::take(&mut self.arriving).then_some(self.camera.center);
        if self.built.as_ref() == Some(&key) || self.building.as_ref() == Some(&key) {
            return None;
        }
        self.building = Some(key.clone());
        Some(SheetJob {
            key,
            map: self.map.value().clone(),
            arriving_from,
        })
    }

    pub fn finish_sheet(&mut self, built: BuiltSheet) {
        let BuiltSheet {
            key,
            sheet,
            arrival,
        } = built;
        if self.building.as_ref() == Some(&key) {
            self.building = None;
        }
        if let Some((x, y)) = arrival.filter(|_| self.place.as_ref() == Some(&key.1)) {
            self.camera.center = (x as f32, y as f32);
        }
        self.sheet = Arc::new(sheet);
        self.built = Some(key);
    }

    /// The walk the selection needs worked out, if any.
    pub fn walk_job(&mut self) -> Option<WalkJob> {
        let selection = self.selection.as_mut()?;
        let Some(start) = self.map.value().whereabouts().room else {
            selection.walk = Walk::Unknown;
            self.walk_for = None;
            return None;
        };
        let key = WalkKey {
            revision: self.rooms_changed,
            start,
            goal: selection.room,
        };
        if self.walk_for == Some(key) || self.walking == Some(key) {
            return None;
        }
        selection.walk = Walk::Unknown;
        self.walking = Some(key);
        Some(WalkJob {
            key,
            map: self.map.value().clone(),
        })
    }

    /// Takes a finished walk; `true` when it's still the one wanted.
    pub fn finish_walk(&mut self, (key, steps): (WalkKey, Option<Vec<WalkStep>>)) -> bool {
        if self.walking == Some(key) {
            self.walking = None;
        }
        let Some(selection) = self.selection.as_mut().filter(|s| s.room == key.goal) else {
            return false;
        };
        self.walk_for = Some(key);
        selection.walk = steps.map_or(Walk::NoPath, Walk::Steps);
        true
    }

    // MARK: Clicking

    pub fn selection(&self) -> Option<&Selection> {
        self.selection.as_ref()
    }

    pub fn room_name(&self, room: RoomId) -> Option<&str> {
        Some(&self.map.value().room(room)?.name)
    }

    /// Acts on a click at a point of a view of `size` pixels: a room is
    /// selected, a house opens its building's map.
    pub fn click(&mut self, point: (f32, f32), size: (f32, f32)) -> Option<Hit> {
        let hit = self.sheet.hit(self.camera.to_grid(point, size))?;
        match hit {
            // The selected room keeps its walk; another room needs its own.
            Hit::Room(room) if self.selection.as_ref().is_none_or(|s| s.room != room) => {
                self.selection = Some(Selection {
                    room,
                    walk: Walk::Unknown,
                });
                self.walk_for = None;
            }
            Hit::Room(_) => {}
            Hit::Building(building) => {
                let zone = self.place.as_ref()?.zone.clone();
                let floor = self.map.value().room(building)?.position.z;
                self.show(Place {
                    zone,
                    building: Some(building),
                    floor,
                });
            }
        }
        Some(hit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map_view::harness::{grid_map, room, store_of, versioned};
    use mudling_core::map::{Building, Direction, MapChange, MapDoor, Whereabouts};

    const VIEW: (f32, f32) = (400., 300.);

    fn build_sheet(model: &mut MapModel) -> bool {
        let job = model.sheet_job();
        let built = job.is_some();
        if let Some(job) = job {
            model.finish_sheet(job.run());
        }
        built
    }

    fn player_at(room: RoomId) -> Whereabouts {
        Whereabouts {
            room: Some(room),
            ..Whereabouts::default()
        }
    }

    fn move_to(model: &mut MapModel, room: RoomId, revision: u64) {
        model.apply(&Diff {
            revision: Revision(revision),
            change: MapChange {
                revision,
                whereabouts: Some(player_at(room)),
                ..MapChange::default()
            },
        });
    }

    fn grid_model() -> MapModel {
        let mut model = MapModel::new(versioned(grid_map(10)));
        build_sheet(&mut model);
        model
    }

    #[test]
    fn a_sheet_is_built_once_and_not_for_panning_zooming_or_moves() {
        let mut model = grid_model();
        assert_eq!(model.sheet().room_count(), 100);
        let sheet = Arc::as_ptr(model.sheet());

        model.pan((30., 10.));
        model.zoom(1.4, (10., 10.), VIEW);
        move_to(&mut model, 3, 2);
        move_to(&mut model, 4, 3);

        assert!(!build_sheet(&mut model), "nothing asked for a new sheet");
        assert_eq!(Arc::as_ptr(model.sheet()), sheet);
    }

    #[test]
    fn a_changed_room_builds_one_new_sheet() {
        let mut model = grid_model();
        model.apply(&Diff {
            revision: Revision(2),
            change: MapChange {
                revision: 2,
                rooms: vec![Arc::new(room(1_000, 20, 20))],
                ..MapChange::default()
            },
        });
        assert!(build_sheet(&mut model));
        assert_eq!(model.sheet().room_count(), 101);
        assert!(!build_sheet(&mut model));
    }

    #[test]
    fn a_sheet_being_built_is_not_asked_for_twice() {
        let mut model = MapModel::new(versioned(grid_map(3)));
        let first = model.sheet_job().expect("nothing is built yet");
        assert!(model.sheet_job().is_none());
        model.finish_sheet(first.run());
        assert_eq!(model.sheet().room_count(), 9);
    }

    #[test]
    fn the_view_follows_the_player_until_panned() {
        let mut model = grid_model();
        move_to(&mut model, 5, 2);
        let (square, _) = model.you().expect("the player is on this sheet");
        let on_player = (square.0 as f32, square.1 as f32);
        assert_eq!(model.camera().center, on_player);

        model.pan((100., 0.));
        let panned = model.camera().center;
        move_to(&mut model, 6, 3);
        assert_eq!(model.camera().center, panned);

        model.follow();
        assert_ne!(model.camera().center, panned);
    }

    #[test]
    fn walking_to_another_floor_brings_the_view_along() {
        let mut ground = room(1, 0, 0);
        ground.links.insert(Direction::Up, 2);
        let mut upstairs = room(2, 0, 0);
        upstairs.position.z = 1;
        let mut model = MapModel::new(versioned(store_of(vec![ground, upstairs], Vec::new())));
        move_to(&mut model, 1, 2);
        build_sheet(&mut model);
        model.pan((80., 0.));

        move_to(&mut model, 2, 3);
        assert_eq!(model.place().map(|place| place.floor), Some(1));
        assert!(build_sheet(&mut model));
        assert_eq!(model.next_floor(false), Some(0));
        assert_eq!(model.next_floor(true), None);
    }

    #[test]
    fn the_dark_shows_the_guessed_square() {
        let mut model = grid_model();
        model.apply(&Diff {
            revision: Revision(2),
            change: MapChange {
                revision: 2,
                whereabouts: Some(Whereabouts {
                    room: Some(0),
                    dark: true,
                    dark_position: Some(GridPoint::new(-1, -1, 0)),
                    ..Whereabouts::default()
                }),
                ..MapChange::default()
            },
        });
        assert_eq!(model.you(), Some(((-1, -1), true)));
    }

    #[test]
    fn a_click_selects_a_room_and_its_walk_comes_from_a_job() {
        let mut model = grid_model();
        move_to(&mut model, 0, 2);
        let point = model.camera().to_view((2., 1.), VIEW);

        assert_eq!(model.click(point, VIEW), Some(Hit::Room(12)));
        let selection = model.selection().expect("a room was clicked");
        assert_eq!(selection.walk, Walk::Unknown);

        let job = model.walk_job().expect("the walk needs working out");
        assert!(model.walk_job().is_none());
        assert!(model.finish_walk(job.run()));
        let Some(Walk::Steps(steps)) = model.selection().map(|s| s.walk.clone()) else {
            panic!("the grid is linked");
        };
        assert_eq!(steps.len(), 3);

        assert!(model.walk_job().is_none(), "nothing moved");
        move_to(&mut model, 1, 3);
        assert!(model.walk_job().is_some(), "the walk starts somewhere new");
    }

    #[test]
    fn a_house_opens_its_buildings_map_and_outdoors_comes_back() {
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
        let map = store_of(vec![street, hall], vec![keep]);
        let mut model = MapModel::new(versioned(map));
        build_sheet(&mut model);

        let house = model.camera().to_view((1., 0.), VIEW);
        assert_eq!(model.click(house, VIEW), Some(Hit::Building(20)));
        assert!(model.selection().is_none());
        assert!(build_sheet(&mut model));
        assert_eq!(model.sheet().square_of(20), Some((7, 7)));
        assert_eq!(model.building_name(), Some("Square 7, 7"));

        assert_eq!(model.camera().center, (7., 7.), "the hall comes into view");

        model.outdoors();
        assert!(build_sheet(&mut model));
        assert_eq!(model.sheet().square_of(1), Some((0, 0)));
        assert_eq!(model.camera().center, (0., 0.), "back out by the door");
    }

    #[test]
    fn a_floor_with_rooms_elsewhere_comes_into_view() {
        let mut ground = room(1, 0, 0);
        ground.links.insert(Direction::Up, 2);
        let mut upstairs = room(2, 9, 9);
        upstairs.position.z = 1;
        let mut model = MapModel::new(versioned(store_of(vec![ground, upstairs], Vec::new())));
        build_sheet(&mut model);

        model.change_floor(true);
        assert!(build_sheet(&mut model));
        assert_eq!(model.camera().center, (9., 9.));
    }

    #[test]
    fn a_sheet_left_behind_does_not_move_the_view() {
        let mut model = grid_model();
        model.change_floor(true);
        let place = model.place().cloned().expect("a place is shown");
        model.show(Place {
            floor: place.floor + 1,
            ..place.clone()
        });
        let stale = model.sheet_job().expect("the new floor needs a sheet");
        model.show(place);
        model.camera.center = (5., 5.);

        model.finish_sheet(stale.run());
        assert_eq!(model.camera().center, (5., 5.));
    }

    fn walk_of(model: &MapModel) -> Option<Walk> {
        model.selection().map(|selection| selection.walk.clone())
    }

    fn finish_walk(model: &mut MapModel) {
        let job = model.walk_job().expect("the walk needs working out");
        assert!(model.finish_walk(job.run()));
    }

    #[test]
    fn clicking_the_selected_room_again_keeps_its_walk() {
        let mut model = grid_model();
        let point = model.camera().to_view((2., 1.), VIEW);
        model.click(point, VIEW);
        finish_walk(&mut model);
        let walk = walk_of(&model);
        assert!(matches!(walk, Some(Walk::Steps(_))));

        model.click(point, VIEW);
        assert_eq!(walk_of(&model), walk);
        assert!(model.walk_job().is_none());
    }

    #[test]
    fn coming_back_to_a_room_works_its_walk_out_again() {
        let mut model = grid_model();
        let first = model.camera().to_view((2., 1.), VIEW);
        let second = model.camera().to_view((3., 1.), VIEW);
        model.click(first, VIEW);
        finish_walk(&mut model);

        model.click(second, VIEW);
        let abandoned = model.walk_job().expect("a new room needs a walk");
        model.click(first, VIEW);
        assert!(!model.finish_walk(abandoned.run()));

        finish_walk(&mut model);
        assert!(matches!(walk_of(&model), Some(Walk::Steps(_))));
    }

    #[test]
    fn a_walk_comes_back_when_the_player_is_found_again() {
        let mut model = grid_model();
        model.click(model.camera().to_view((2., 1.), VIEW), VIEW);
        finish_walk(&mut model);

        let lost = Whereabouts::default();
        model.apply(&Diff {
            revision: Revision(2),
            change: MapChange {
                revision: 2,
                whereabouts: Some(lost),
                ..MapChange::default()
            },
        });
        assert!(model.walk_job().is_none());
        assert_eq!(walk_of(&model), Some(Walk::Unknown));

        move_to(&mut model, 0, 3);
        finish_walk(&mut model);
        assert!(matches!(walk_of(&model), Some(Walk::Steps(_))));
    }

    #[test]
    fn an_empty_map_asks_for_nothing() {
        let mut model = MapModel::new(Versioned::default());
        assert!(model.sheet_job().is_none());
        assert_eq!(model.click((1., 1.), VIEW), None);
        assert!(model.you().is_none());
    }
}
