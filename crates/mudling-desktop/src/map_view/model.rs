//! The view model: what the map view shows, with no gpui in it.
//!
//! It holds a copy of the map store, fed by store diffs. Anything that scans
//! the map (a sheet, a walk) is cached against the store revision the rooms
//! last changed at, and handed out as a job for a background thread. Panning,
//! zooming and walking known rooms ask for no work at all.

use super::camera::Camera;
use super::sheet::{Hit, Place, Sheet};
use mudling_core::map::{
    Direction, Duplicates, GridPoint, JoinDone, MapRoom, RoomId, RoomPair, WalkStep,
    likely_duplicates, mapper_of, room_pair,
};
use mudling_core::persist::Timestamp;
use mudling_core::store::{Diff, MapState, Revision, Versioned};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

type SheetKey = (Revision, Place);
/// The rooms as of a revision, and how many pairs the player had answered.
type ScanKey = (Revision, usize);

/// The least time between looks for duplicates: a walk through new rooms
/// changes the map on every move, and nothing here needs to keep up with it.
const SCAN_GAP: Duration = Duration::from_secs(2);

/// A look for duplicates, on a cheap clone of the map.
pub struct DuplicatesJob {
    key: ScanKey,
    map: MapState,
}

impl DuplicatesJob {
    pub fn run(self) -> (ScanKey, Duplicates) {
        let rooms = self.map.rooms().map(|room| &**room);
        let found = likely_duplicates(rooms, &self.map.joins().not_same);
        (self.key, found)
    }
}

/// What the view does about duplicates next.
pub enum ScanStep {
    Nothing,
    Run(DuplicatesJob),
    /// Too soon after the last look: ask again after this long.
    WaitFor(Duration),
}

/// The note over the map after a join.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JoinNote {
    Joined { label: String, created: Timestamp },
    Failed(String),
}

/// One room of a pair, as Review words it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomCopy {
    pub name: String,
    pub mapped: &'static str,
    pub held: String,
    pub sightings: String,
}

impl RoomCopy {
    fn of(room: &MapRoom, this_device: i64) -> RoomCopy {
        let notes = room.notes.lines().filter(|line| !line.trim().is_empty());
        let features: Vec<&str> = room.features.iter().map(String::as_str).collect();
        let features = match features.len() {
            0 => "no features".to_owned(),
            count => format!("{} ({})", counted(count, "feature"), features.join(", ")),
        };
        let held = match notes.count() {
            0 => format!("No notes · {features}"),
            count => format!("{} · {features}", counted(count, "note")),
        };
        RoomCopy {
            name: room.name.clone(),
            mapped: if mapper_of(room.id) == this_device {
                "Mapped on this device"
            } else {
                "Mapped on your other device"
            },
            held,
            sightings: match room.sightings.len() {
                0 => "No sightings".to_owned(),
                count => counted(count, "sighting"),
            },
        }
    }
}

fn counted(count: usize, thing: &str) -> String {
    match count {
        1 => format!("1 {thing}"),
        count => format!("{count} {thing}s"),
    }
}

/// The ways two rooms lead to different places. Joined, the newer link stays.
fn ways_that_differ(first: &MapRoom, second: &MapRoom) -> Vec<Direction> {
    Direction::ALL
        .into_iter()
        .filter(|way| {
            let (a, b) = (first.links.get_copied(*way), second.links.get_copied(*way));
            a.is_some() && b.is_some() && a != b
        })
        .collect()
}

/// Two copies side by side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewPair {
    pub pair: RoomPair,
    pub copies: [RoomCopy; 2],
    /// Which pair this is, from one.
    pub number: usize,
    pub of: usize,
    /// What a join can't keep both of, in the player's words.
    pub warning: Option<String>,
}

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
    duplicates: Arc<Duplicates>,
    /// `duplicates.rooms`, for a tile's color without a tree walk per frame.
    marked: Arc<HashSet<RoomId>>,
    scanned: Option<ScanKey>,
    scanning: Option<ScanKey>,
    scan_finished: Option<Instant>,
    /// Rooms picked to join, at most two.
    picks: Vec<RoomId>,
    /// Which pair Review shows; `None` when it's closed.
    reviewing: Option<usize>,
    /// The pair sent to the session, until it answers. It stays in Review
    /// until then: a join can be refused.
    joining: Option<RoomPair>,
    join_note: Option<JoinNote>,
    /// The last answer taken from the store.
    answered: u64,
}

impl MapModel {
    /// Starts from a snapshot of the store.
    pub fn new(snapshot: Versioned<MapState>) -> MapModel {
        let mut model = MapModel {
            rooms_changed: snapshot.revision(),
            answered: snapshot.value().last_join().map_or(0, |last| last.attempt),
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
        self.take_join_answer();
        let map = self.map.value();
        self.picks.retain(|room| map.room(*room).is_some());
        self.track_player();
    }

    fn take_join_answer(&mut self) {
        let Some(last) = self.map.value().last_join().cloned() else {
            return;
        };
        if last.attempt == self.answered {
            return;
        }
        self.answered = last.attempt;
        let joining = self.joining.take();
        self.join_note = match last.outcome {
            Ok(JoinDone::Joined { label, created, .. }) => {
                if let Some(pair) = joining {
                    self.answer_pair(pair, Some(pair.1));
                }
                Some(JoinNote::Joined { label, created })
            }
            Ok(JoinDone::Undone) => None,
            Err(error) => Some(JoinNote::Failed(error)),
        };
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

    /// Whether to look for duplicates now. A look costs a walk of the map off
    /// the UI thread, and a line from the game costs nothing here: only a
    /// change to the rooms or to the player's answers asks for one.
    pub fn scan_step(&mut self, now: Instant) -> ScanStep {
        let key = (self.rooms_changed, self.map.value().joins().not_same.len());
        if self.scanned == Some(key) || self.scanning.is_some() {
            return ScanStep::Nothing;
        }
        if let Some(wait) = self
            .scan_finished
            .and_then(|last| SCAN_GAP.checked_sub(now - last))
        {
            return ScanStep::WaitFor(wait);
        }
        self.scanning = Some(key);
        ScanStep::Run(DuplicatesJob {
            key,
            map: self.map.value().clone(),
        })
    }

    /// Looks on this thread, whatever a look under way would find. For the
    /// images and checks, which can't wait for one.
    #[cfg(any(test, target_os = "macos"))]
    pub fn scan_now(&mut self) {
        let job = DuplicatesJob {
            key: (self.rooms_changed, self.map.value().joins().not_same.len()),
            map: self.map.value().clone(),
        };
        self.finish_scan(job.run(), Instant::now());
    }

    pub fn finish_scan(&mut self, (key, found): (ScanKey, Duplicates), now: Instant) {
        self.scanning = None;
        self.scan_finished = Some(now);
        self.scanned = Some(key);
        self.set_duplicates(found);
    }

    fn set_duplicates(&mut self, found: Duplicates) {
        self.marked = Arc::new(found.rooms.iter().copied().collect());
        self.duplicates = Arc::new(found);
        if let Some(index) = self.reviewing {
            let last = self.duplicates.pairs.len().checked_sub(1);
            self.reviewing = last.map(|last| index.min(last));
        }
    }

    pub fn duplicates(&self) -> &Duplicates {
        &self.duplicates
    }

    /// The lowest id on the map, for a measured walk.
    pub fn first_room(&self) -> RoomId {
        let first = self.map.value().rooms().next();
        first.map_or(0, |room| room.id)
    }

    pub fn marked(&self) -> &Arc<HashSet<RoomId>> {
        &self.marked
    }

    // MARK: Review

    pub fn open_review(&mut self) {
        self.reviewing = (!self.duplicates.pairs.is_empty()).then_some(0);
    }

    pub fn close_review(&mut self) {
        self.reviewing = None;
    }

    pub fn reviewing(&self) -> bool {
        self.reviewing.is_some()
    }

    /// The pair after this one, or the first again after the last.
    pub fn skip_pair(&mut self) {
        self.join_note = None;
        let pairs = self.duplicates.pairs.len();
        self.reviewing = self.reviewing.map(|index| (index + 1) % pairs.max(1));
    }

    /// Stops asking about `pair`, ahead of the next look at the map. Each pair
    /// that held a room `retired` by joining it goes too.
    pub fn answer_pair(&mut self, pair: RoomPair, retired: Option<RoomId>) {
        let mut found = (*self.duplicates).clone();
        found.pairs.retain(|answered| {
            *answered != pair && retired.is_none_or(|id| answered.0 != id && answered.1 != id)
        });
        found.rooms = found.pairs.iter().flat_map(|(a, b)| [*a, *b]).collect();
        self.set_duplicates(found);
    }

    pub fn review_pair(&self) -> Option<ReviewPair> {
        let index = self.reviewing?;
        let pair = *self.duplicates.pairs.get(index)?;
        let map = self.map.value();
        let (first, second) = (&**map.room(pair.0)?, &**map.room(pair.1)?);
        let this_device = map.joins().this_device;
        let ways = ways_that_differ(first, second);
        let warning = (!ways.is_empty()).then(|| {
            let names: Vec<&str> = ways.iter().map(|way| way.name()).collect();
            format!(
                "Their links differ: {}. The newer one stays for each.",
                names.join(", ")
            )
        });
        Some(ReviewPair {
            pair,
            copies: [
                RoomCopy::of(first, this_device),
                RoomCopy::of(second, this_device),
            ],
            number: index + 1,
            of: self.duplicates.pairs.len(),
            warning,
        })
    }

    // MARK: Picking rooms to join

    pub fn picks(&self) -> &[RoomId] {
        &self.picks
    }

    pub fn clear_picks(&mut self) {
        self.picks.clear();
    }

    /// Whether the map can join `room` at all.
    pub fn can_pick(&self, room: RoomId) -> bool {
        self.duplicates.joinable && self.map.value().room(room).is_some()
    }

    /// Adds `room`, or takes it back off. A third room takes the oldest's place.
    pub fn pick(&mut self, room: RoomId) {
        if !self.can_pick(room) {
            return;
        }
        if let Some(picked) = self.picks.iter().position(|id| *id == room) {
            self.picks.remove(picked);
            return;
        }
        if self.picks.len() == 2 {
            self.picks.remove(0);
        }
        self.picks.push(room);
    }

    /// A Shift-click: picks the selected room as well, if nothing is picked.
    /// Returns whether a room was there to pick: if not, the click is an
    /// ordinary one.
    pub fn pick_at(&mut self, point: (f32, f32), size: (f32, f32)) -> bool {
        let Some(Hit::Room(room)) = self.sheet.hit(self.camera.to_grid(point, size)) else {
            return false;
        };
        if self.picks.is_empty()
            && let Some(selected) = self.selection.as_ref().map(|selection| selection.room)
            && selected != room
        {
            self.pick(selected);
        }
        self.pick(room);
        true
    }

    /// The two rooms to join, once two are picked.
    pub fn picked_pair(&self) -> Option<RoomPair> {
        match self.picks[..] {
            [first, second] => Some(room_pair(first, second)),
            _ => None,
        }
    }

    /// The session has been asked to join `pair`.
    pub fn await_join(&mut self, pair: RoomPair) {
        self.joining = Some(pair);
        self.join_note = None;
        self.picks.clear();
    }

    pub fn joining(&self) -> Option<RoomPair> {
        self.joining
    }

    pub fn join_note(&self) -> Option<&JoinNote> {
        self.join_note.as_ref()
    }

    pub fn clear_join_note(&mut self) {
        self.join_note = None;
    }

    // MARK: Clicking

    pub fn selection(&self) -> Option<&Selection> {
        self.selection.as_ref()
    }

    pub fn room_name(&self, room: RoomId) -> Option<&str> {
        Some(&self.map.value().room(room)?.name)
    }

    /// Selects a room and goes to its sheet, looking at it.
    pub fn go_to(&mut self, room: RoomId) {
        let map = self.map.value();
        let Some(found) = map.room(room) else {
            return;
        };
        let (place, square) = Place::of(found, map.derived().map(|derived| &**derived));
        self.camera.center = (square.x as f32, square.y as f32);
        self.selection = Some(Selection {
            room,
            walk: Walk::Unknown,
        });
        self.walk_for = None;
        self.show(place);
    }

    /// Acts on a click at a point of a view of `size` pixels: a room is
    /// selected, a house opens its building's map.
    pub fn click(&mut self, point: (f32, f32), size: (f32, f32)) -> Option<Hit> {
        let hit = self.sheet.hit(self.camera.to_grid(point, size))?;
        self.picks.clear();
        self.join_note = None;
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
    use crate::map_view::harness::{duplicated_map, grid_map, room, store_of, versioned};
    use mudling_core::map::{Building, LastJoin, MapChange, MapDoor, Whereabouts};

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

    fn duplicated_model() -> MapModel {
        let mut model = MapModel::new(versioned(duplicated_map(9, 3)));
        build_sheet(&mut model);
        scan(&mut model);
        model
    }

    fn scan(model: &mut MapModel) {
        let ScanStep::Run(job) = model.scan_step(Instant::now()) else {
            panic!("a look was due");
        };
        // Long enough ago that the next change can look at once.
        model.finish_scan(job.run(), Instant::now() - SCAN_GAP);
    }

    #[test]
    fn rooms_that_read_alike_are_marked_and_a_numbered_map_never_is() {
        let model = duplicated_model();
        assert!(!model.duplicates().pairs.is_empty());
        assert_eq!(model.marked().len(), model.duplicates().rooms.len());

        let mut numbered = grid_model();
        scan(&mut numbered);
        assert!(numbered.duplicates().rooms.is_empty());
        let room = numbered.first_room();
        numbered.pick(room);
        assert!(numbered.picks().is_empty(), "nothing to join on this game");
    }

    #[test]
    fn a_move_asks_for_no_look_and_a_changed_room_waits_its_turn() {
        let mut model = MapModel::new(versioned(duplicated_map(4, 2)));
        let ScanStep::Run(job) = model.scan_step(Instant::now()) else {
            panic!("the first look is due");
        };
        model.finish_scan(job.run(), Instant::now());

        let first = model.first_room();
        move_to(&mut model, first, 2);
        assert!(matches!(model.scan_step(Instant::now()), ScanStep::Nothing));

        model.apply(&Diff {
            revision: Revision(3),
            change: MapChange {
                revision: 3,
                rooms: vec![Arc::new(room(first + 1_000, 20, 20))],
                ..MapChange::default()
            },
        });
        assert!(matches!(
            model.scan_step(Instant::now()),
            ScanStep::WaitFor(_)
        ));
        let later = Instant::now() + SCAN_GAP * 2;
        assert!(matches!(model.scan_step(later), ScanStep::Run(_)));
    }

    #[test]
    fn a_shift_click_picks_the_selected_room_too_and_a_click_lets_go() {
        let mut model = duplicated_model();
        let first = model.camera().to_view((2., 1.), VIEW);
        let second = model.camera().to_view((3., 1.), VIEW);
        model.click(first, VIEW);

        model.pick_at(second, VIEW);

        assert_eq!(model.picks().len(), 2);
        let pair = model.picked_pair().expect("two rooms are picked");
        assert!(pair.0 < pair.1, "lower id first");

        model.click(first, VIEW);
        assert!(model.picks().is_empty());
    }

    #[test]
    fn picking_a_room_again_takes_it_back_and_a_third_replaces_the_oldest() {
        let mut model = duplicated_model();
        let base = model.first_room();
        model.pick(base);
        model.pick(base);
        assert!(model.picks().is_empty());

        model.pick(base);
        model.pick(base + 1);
        model.pick(base + 2);
        assert_eq!(model.picks(), [base + 1, base + 2]);
    }

    #[test]
    fn review_walks_the_pairs_and_an_answer_stops_the_question() {
        let mut model = duplicated_model();
        model.open_review();
        let first = model.review_pair().expect("there are pairs");
        assert_eq!(first.number, 1);
        assert_eq!(first.copies[0].mapped, "Mapped on your other device");

        model.skip_pair();
        let second = model.review_pair().expect("there are more");
        assert_eq!(second.number, 2);
        assert_ne!(second.pair, first.pair);

        let total = second.of;
        model.answer_pair(second.pair, None);
        let next = model.review_pair().expect("there are more");
        assert_eq!(next.of, total - 1);
        assert_ne!(next.pair, second.pair);

        model.answer_pair(next.pair, Some(next.pair.1));
        let retired = next.pair.1;
        assert!(
            (model.duplicates().pairs.iter()).all(|pair| pair.0 != retired && pair.1 != retired),
            "a retired room is asked about no more"
        );
    }

    #[test]
    fn review_says_what_each_copy_holds() {
        let mut held = room(1 << 40, 0, 0);
        held.notes = "Shop by the well\nBeggars at night".to_owned();
        held.features.insert("shop".to_owned());
        held.sightings.insert("a pilgrim".to_owned(), Timestamp(1));

        let copy = RoomCopy::of(&held, 0);
        assert_eq!(copy.held, "2 notes · 1 feature (shop)");
        assert_eq!(copy.sightings, "1 sighting");

        let bare = RoomCopy::of(&room(1 << 40, 1, 0), 0);
        assert_eq!(bare.held, "No notes · no features");
        assert_eq!(bare.sightings, "No sightings");
    }

    #[test]
    fn review_warns_of_a_way_that_leads_two_places() {
        let (mut first, mut second) = (room(1 << 40, 0, 0), room((1 << 40) + 1, 0, 0));
        first.links.insert(Direction::North, 5);
        second.links.insert(Direction::North, 6);
        second.links.insert(Direction::East, 7);
        assert_eq!(ways_that_differ(&first, &second), [Direction::North]);
    }

    fn answered(attempt: u64, outcome: Result<JoinDone, String>) -> Diff<MapState> {
        Diff {
            revision: Revision(attempt + 1),
            change: MapChange {
                last_join: Some(LastJoin { attempt, outcome }),
                ..MapChange::default()
            },
        }
    }

    #[test]
    fn the_note_over_the_map_follows_the_sessions_answers() {
        let mut model = duplicated_model();
        let created = Timestamp(5);
        model.apply(&answered(
            1,
            Ok(JoinDone::Joined {
                label: "Joined two copies of a square".to_owned(),
                created,
            }),
        ));
        assert!(matches!(
            model.join_note(),
            Some(JoinNote::Joined { created: at, .. }) if *at == created
        ));

        model.apply(&answered(2, Ok(JoinDone::Undone)));
        assert_eq!(model.join_note(), None);

        model.apply(&answered(3, Err("no".to_owned())));
        assert_eq!(model.join_note(), Some(&JoinNote::Failed("no".to_owned())));
    }

    #[test]
    fn a_pair_goes_only_once_the_session_has_joined_it() {
        let mut model = duplicated_model();
        model.open_review();
        let pair = model.review_pair().expect("there are pairs").pair;

        model.await_join(pair);
        assert_eq!(model.review_pair().map(|shown| shown.pair), Some(pair));

        model.apply(&answered(
            1,
            Ok(JoinDone::Joined {
                label: "Joined".to_owned(),
                created: Timestamp(5),
            }),
        ));
        assert_ne!(model.review_pair().map(|shown| shown.pair), Some(pair));
        assert_eq!(model.joining(), None);
    }

    #[test]
    fn a_refused_join_keeps_its_pair_and_says_why() {
        let mut model = duplicated_model();
        model.open_review();
        let pair = model.review_pair().expect("there are pairs").pair;

        model.await_join(pair);
        model.apply(&answered(1, Err("a sync is landing".to_owned())));

        assert_eq!(model.review_pair().map(|shown| shown.pair), Some(pair));
        assert_eq!(
            model.join_note(),
            Some(&JoinNote::Failed("a sync is landing".to_owned()))
        );
        assert_eq!(model.joining(), None);
    }

    #[test]
    fn an_empty_map_asks_for_nothing() {
        let mut model = MapModel::new(Versioned::default());
        assert!(model.sheet_job().is_none());
        assert_eq!(model.click((1., 1.), VIEW), None);
        assert!(model.you().is_none());
    }
}
