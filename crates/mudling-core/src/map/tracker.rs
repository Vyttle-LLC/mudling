//! Following the player: which room each Room.Info is, and where a new one
//! goes.
//!
//! Positions come from dead reckoning on the player's moves. A room without
//! a game number is recognized by its name, area, exits and description;
//! when it could be one of several, the tracker keeps every explanation and
//! lets the next rooms decide. Each area has a grid of its own, and so does
//! each building within an area.

use super::geometry::{Direction, GridPoint};
use super::layout::{Arrival, Landing, Layout, Step, refresh};
use super::room::{MapRoom, RoomId};
use crate::persist::Timestamp;
use std::collections::{HashSet, VecDeque};

/// A command left waiting this long is taken to have failed silently.
/// Generous, because games delay commands during combat lag.
const EXPECTATION_TIMEOUT_MS: i64 = 20_000;
/// How far ahead the best explanation must be to be believed.
const CONVINCING_LEAD: i32 = 3;
/// After this many unsure rooms, the best explanation is taken as is.
const LONGEST_DOUBT: usize = 8;
/// Explanations kept at once, best first.
const MOST_GUESSES: usize = 12;
/// Areas known to be drawn as well as they can be, remembered up to this
/// many before the memory starts over.
const SETTLED_AREAS: usize = 4_096;

/// What entering a room revealed, for the client to announce.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Arrived {
    /// Set when this room is in a different area than the last one.
    pub entered_zone: Option<String>,
    /// A revisited room whose set of exits differs from last time.
    pub exits_changed: bool,
    /// Stepped from mappable ground into rooms that don't map cleanly.
    pub entered_tangle: bool,
    pub new_room: bool,
    /// Earlier rooms of the walk the map has changed its mind about.
    pub rethought_rooms: usize,
    /// Stepped into a room too dark to see.
    pub entered_dark: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expectation {
    /// A move brings a new room.
    Move(Direction),
    /// A look shows the same one.
    Stay,
}

#[derive(Clone, Copy, Debug)]
struct Pending {
    expectation: Expectation,
    /// When it began waiting for its answer: when it was sent, or for the
    /// oldest, when the one before it was answered, since the game answers
    /// in order.
    since: Timestamp,
}

#[derive(Clone, Debug)]
pub(super) struct Tracker {
    /// The map as drawn. While unsure, this is the best guess's version.
    pub layout: Layout,
    /// Set while the map can't yet tell which known rooms the player walked
    /// through: every explanation of the walk so far that still fits.
    doubt: Option<Doubt>,
    /// What each pending command should bring back in a Room.Info, oldest
    /// first. Commands that bring no Room.Info aren't listed.
    expected: VecDeque<Pending>,
    /// Set while the player walks rooms too dark to see.
    dark: Option<DarkWalk>,
    /// Fingerprints of areas straightened, or found impossible to, since
    /// they last changed.
    settled_areas: HashSet<u64>,
}

/// One explanation of an unsure walk, with the map as it would be.
#[derive(Clone, Debug)]
struct Guess {
    layout: Layout,
    /// The room each step of the walk ended in, to tell when the best
    /// explanation changes its mind.
    path: Vec<RoomId>,
    score: i32,
    arrival: Arrival,
}

impl Guess {
    fn new(layout: Layout) -> Guess {
        Guess {
            layout,
            path: Vec::new(),
            score: 0,
            arrival: Arrival::default(),
        }
    }
}

/// The walk since the map was last sure where the player was.
#[derive(Clone, Debug)]
struct Doubt {
    /// The map when the doubt began: rooms it lacks were added since.
    origin: Layout,
    /// Best first.
    guesses: Vec<Guess>,
    steps: usize,
}

/// A walk through rooms too dark to see, since the last room seen.
#[derive(Clone, Debug)]
struct DarkWalk {
    /// The area the game says the player is in, to announce a new one once.
    zone: String,
    /// Where the moves lead, on the map of the last room seen; `None` with
    /// nothing mapped yet.
    position: Option<GridPoint>,
    heading: Option<Direction>,
    /// A move went where the map has no exit, so the player is off the map.
    off_map: bool,
}

impl DarkWalk {
    /// Where the player is after `step`, the move that brought light.
    fn landing(&self, step: Option<Direction>) -> Option<Landing> {
        let position = self.position?;
        let heading = step.or(self.heading)?;
        Some(Landing {
            position: step.map_or(position, |step| position + step.offset()),
            heading,
            building: None,
        })
    }
}

impl Tracker {
    pub fn new(layout: Layout) -> Tracker {
        Tracker {
            layout,
            doubt: None,
            expected: VecDeque::new(),
            dark: None,
            settled_areas: HashSet::new(),
        }
    }

    pub fn is_unsure(&self) -> bool {
        self.doubt.is_some()
    }

    pub fn is_in_dark(&self) -> bool {
        self.dark.is_some()
    }

    /// In the dark past the edge of the map: the square the moves lead to,
    /// on the current room's map.
    pub fn dark_position(&self) -> Option<GridPoint> {
        self.dark
            .as_ref()
            .filter(|walk| walk.off_map)
            .and_then(|walk| walk.position)
    }

    /// Rooms placed on a guess the map hasn't confirmed: those added during
    /// the unsure walk, and where it thinks the player is.
    pub fn unconfirmed_rooms(&self) -> Vec<RoomId> {
        if self.is_in_dark() {
            return self.layout.current.into_iter().collect();
        }
        let Some(doubt) = &self.doubt else {
            return Vec::new();
        };
        let mut ids: Vec<RoomId> = self
            .layout
            .dirty()
            .iter()
            .copied()
            .filter(|id| self.layout.contains(*id) && !doubt.origin.contains(*id))
            .chain(self.layout.current)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    pub fn expect_move(&mut self, direction: Direction, now: Timestamp) {
        self.expected.push_back(Pending {
            expectation: Expectation::Move(direction),
            since: now,
        });
    }

    /// A look: the next Room.Info shows the same room again.
    pub fn expect_stay(&mut self, now: Timestamp) {
        self.expected.push_back(Pending {
            expectation: Expectation::Stay,
            since: now,
        });
    }

    /// The game refused a move: only the oldest pending move failed.
    pub fn move_failed(&mut self, now: Timestamp) {
        let Some(index) = self
            .expected
            .iter()
            .position(|pending| pending.expectation != Expectation::Stay)
        else {
            return;
        };
        self.expected.remove(index);
        if index == 0 {
            self.start_waiting(now);
        }
    }

    /// Takes the oldest pending expectation, passing over any that waited
    /// too long.
    fn next_expectation(&mut self, now: Timestamp) -> Option<Expectation> {
        while let Some(oldest) = self.expected.front().copied()
            && now.0 - oldest.since.0 > EXPECTATION_TIMEOUT_MS
        {
            self.expected.pop_front();
            self.start_waiting(Timestamp(oldest.since.0 + EXPECTATION_TIMEOUT_MS));
        }
        let next = self.expected.pop_front()?.expectation;
        self.start_waiting(now);
        Some(next)
    }

    /// The oldest pending command waits from `time` on: a speedwalk is sent
    /// at once, but each move is only answered after the one before it.
    fn start_waiting(&mut self, time: Timestamp) {
        if let Some(oldest) = self.expected.front_mut() {
            oldest.since = oldest.since.max(time);
        }
    }

    /// Records arriving in the step's room; call on every Room.Info.
    ///
    /// When the room could be one of several known rooms (or a new one),
    /// every explanation is kept, the map shows the best, and later rooms
    /// rule the others out.
    pub fn enter(&mut self, mut step: Step, dark: bool, now: Timestamp) -> Arrived {
        let previous = self.layout.current_room().cloned();
        let next = self.next_expectation(now);
        if dark {
            return self.walk_in_dark(next, &step.zone);
        }
        let dark_walk = self.dark.take();
        let mut arrived = Arrived::default();
        let last_zone = dark_walk
            .as_ref()
            .map(|walk| walk.zone.as_str())
            .or(previous.as_ref().map(|room| room.zone.as_str()));
        if last_zone != Some(step.zone.as_str()) {
            arrived.entered_zone = Some(step.zone.clone());
        }

        let step_taken = match next {
            Some(Expectation::Move(direction)) => Some(direction),
            _ => None,
        };
        step.landing = dark_walk.as_ref().and_then(|walk| walk.landing(step_taken));
        let mut maze_exit = None;
        if let (Some(maze), Some(direction)) =
            (previous.as_ref().filter(|room| room.maze), step_taken)
        {
            // Lost in it still: the maze is one square.
            if self.layout.matches_identity(maze, &step) {
                return arrived;
            }
            // Out of it: which way round the maze went is unknown, so the
            // room is placed on the side the last move faced, a square out.
            step.landing = Some(Landing {
                position: maze.position + direction.offset() + direction.offset(),
                heading: direction,
                building: maze.building,
            });
            maze_exit = Some((maze.id, direction));
        } else if dark_walk.as_ref().is_some_and(|walk| walk.off_map) {
            // The rooms walked in the dark were never seen, so nothing
            // links to this one: it's placed where the moves lead.
        } else if step_taken.is_some() {
            step.direction = step_taken;
        } else if let Some(previous) = &previous
            && self.layout.matches_exactly(previous, &step)
        {
            // A look, or a Room.Info nobody asked for, showing the same room.
            self.update_current_rooms(|room| refresh(room, &step));
            return arrived;
        }

        let mut doubt = self.doubt.take().unwrap_or_else(|| Doubt {
            origin: self.layout.clone(),
            guesses: vec![Guess::new(self.layout.clone())],
            steps: 0,
        });
        let mut guesses: Vec<Guess> = doubt
            .guesses
            .iter()
            .flat_map(|guess| branches(guess, &step))
            .collect();
        if guesses.is_empty() {
            // No explanation has an exit that way: the move didn't happen as
            // asked (or the map was wrong). Treat it as an unexplained arrival.
            step.direction = None;
            guesses = doubt
                .guesses
                .iter()
                .flat_map(|guess| branches(guess, &step))
                .collect();
        }
        let earlier_path = std::mem::take(&mut doubt.guesses[0].path);
        doubt.guesses = strongest(guesses, &doubt.origin);
        doubt.steps += 1;

        let best = &doubt.guesses[0];
        let best_arrival = best.arrival;
        arrived.rethought_rooms = earlier_path
            .iter()
            .zip(&best.path)
            .filter(|(a, b)| a != b)
            .count();
        self.layout = best.layout.clone();
        if let (Some((maze, direction)), Some(current)) = (maze_exit, self.layout.current) {
            self.layout
                .update(maze, |room| room.links.insert(direction, current));
        }
        if let Some(current) = self.layout.current {
            let collapsed = self.layout.collapse_maze(current)
                || previous
                    .as_ref()
                    .is_some_and(|room| self.layout.collapse_maze(room.id));
            if collapsed {
                // The rooms it couldn't tell apart are one now: nothing left to doubt.
                doubt.guesses.truncate(1);
            }
        }
        let at_hub = previous.as_ref().is_some_and(MapRoom::is_hub)
            || self.layout.current_room().is_some_and(MapRoom::is_hub);
        if at_hub {
            self.layout.assign_wings();
        }
        if let Some(current) = self.layout.current {
            self.straighten_around(current);
        }
        doubt.guesses[0].layout = self.layout.clone();
        arrived.new_room = best_arrival.new_room;
        arrived.exits_changed = best_arrival.exits_changed;
        arrived.entered_tangle = self.layout.current_room().is_some_and(|room| room.tangled)
            && !previous.as_ref().is_some_and(|room| room.tangled);

        let convinced = doubt.guesses.len() == 1
            || doubt.guesses[0].score - doubt.guesses[1].score >= CONVINCING_LEAD
            || doubt.steps >= LONGEST_DOUBT;
        self.doubt = (!convinced).then_some(doubt);
        arrived
    }

    /// Redraws the area around the room if its paths don't line up, unless
    /// it's been tried since the area last changed.
    fn straighten_around(&mut self, id: RoomId) {
        let Some(area) = self.layout.misdrawn_area(id) else {
            return;
        };
        if self.settled_areas.contains(&area.fingerprint) {
            return;
        }
        self.layout.straighten(&area);
        if let Some(after) = self.layout.misdrawn_area(id) {
            if self.settled_areas.len() >= SETTLED_AREAS {
                self.settled_areas.clear();
            }
            self.settled_areas.insert(after.fingerprint);
        }
    }

    /// A room too dark to see looks like every other, so it's never mapped.
    /// Moves follow the mapped links while there are any, then are counted
    /// from where the map ran out, until a room can be seen again.
    fn walk_in_dark(&mut self, next: Option<Expectation>, zone: &str) -> Arrived {
        self.settle();
        let mut arrived = Arrived::default();
        let current = self.layout.current_room();
        let mut walk = self.dark.take().unwrap_or_else(|| {
            arrived.entered_dark = true;
            DarkWalk {
                zone: current.map_or(zone, |room| room.zone.as_str()).to_owned(),
                position: current.map(|room| room.position),
                heading: None,
                off_map: false,
            }
        });
        if walk.zone != zone {
            arrived.entered_zone = Some(zone.to_owned());
            walk.zone = zone.to_owned();
        }
        if let Some(Expectation::Move(direction)) = next {
            walk.heading = Some(direction);
            let linked = self
                .layout
                .current_room()
                .and_then(|room| room.links.get_copied(direction))
                .and_then(|id| self.layout.room(id))
                .map(|room| (room.id, room.position));
            match linked {
                Some((id, position)) if !walk.off_map => {
                    self.layout.current = Some(id);
                    walk.position = Some(position);
                }
                _ => {
                    walk.off_map = true;
                    walk.position = walk.position.map(|position| position + direction.offset());
                }
            }
        }
        self.dark = Some(walk);
        arrived
    }

    /// Takes the best explanation as the truth. The player editing the map
    /// means it's what they see that counts.
    pub fn settle(&mut self) {
        self.doubt = None;
    }

    /// Forgets every pending command and the dark walk: the player said
    /// where they are.
    pub fn lose_track(&mut self) {
        self.expected.clear();
        self.dark = None;
    }

    /// Applies something seen in the player's room to every explanation of
    /// where that is, so it survives whichever one wins. Not in the dark:
    /// the game still sends the dark room's exits and occupants, and the
    /// current room is only a guess.
    pub fn update_current_rooms(&mut self, change: impl Fn(&mut MapRoom)) {
        if self.dark.is_some() {
            return;
        }
        self.layout.update_current(&change);
        if let Some(doubt) = &mut self.doubt {
            for guess in &mut doubt.guesses {
                guess.layout.update_current(&change);
            }
        }
    }
}

/// Every way `guess` could explain arriving in the step's room, each with
/// its own copy of the map updated and its evidence scored. Empty when the
/// guess's room has no exit the way the player moved.
///
/// Evidence: following a known path into the room it leads to counts for
/// (+2), less when other rooms look the same (+1); closing a loop where the
/// grid expects counts a little (+1), and so does the very same description
/// (+1); a new room that looks exactly like the one known room of its kind
/// is a small coincidence (-1); a known room somewhere the grid doesn't
/// expect it, a stretched path, is a bigger one (-2); an exit that led
/// somewhere else before, or a way back that leads somewhere else, counts
/// strongly against (-3), since only a maze or a wrong guess does that.
fn branches(guess: &Guess, step: &Step) -> Vec<Guess> {
    let layout = &guess.layout;
    let mut known = layout.candidates(step);
    let certain = layout.described_match(step);
    let here = layout.current_room();
    let choices: Vec<(Option<RoomId>, i32)> = if let Some(number) = step.number {
        // The game names the room: nothing to weigh.
        if let (Some(direction), Some(here)) = (step.direction, here)
            && !here.exits.contains(direction)
        {
            return Vec::new();
        }
        vec![(layout.contains(number).then_some(number), 2)]
    } else if let Some(maze) = known
        .iter()
        .copied()
        .find(|id| layout.room(*id).is_some_and(|room| room.maze))
    {
        // Every room of a maze is its one square.
        vec![(Some(maze), 2)]
    } else if let (Some(direction), Some(here)) = (step.direction, here) {
        if !here.exits.contains(direction) {
            return Vec::new();
        }
        // A move leads somewhere else.
        known.retain(|id| *id != here.id);
        let linked = here
            .links
            .get_copied(direction)
            .and_then(|id| layout.room(id));
        if let Some(linked) = linked {
            if layout.matches_identity(linked, step) {
                let look_alikes = known.iter().any(|id| *id != linked.id);
                vec![(Some(linked.id), if look_alikes { 1 } else { 2 })]
            } else {
                known
                    .iter()
                    .map(|id| (Some(*id), -3))
                    .chain([(None, -3)])
                    .collect()
            }
        } else {
            let ahead = here.position + direction.offset();
            let evidence = |id: RoomId| {
                let Some(room) = layout.room(id) else {
                    return -5;
                };
                let on_grid = room.position == ahead
                    && room.zone == here.zone
                    && room.building == here.building;
                let way_back_elsewhere = room
                    .links
                    .get_copied(direction.opposite())
                    .is_some_and(|back| back != here.id);
                (if on_grid { 1 } else { -2 })
                    + i32::from(Some(id) == certain)
                    + if way_back_elsewhere { -3 } else { 0 }
            };
            if known.len() == 1 && evidence(known[0]) > 0 {
                vec![(Some(known[0]), evidence(known[0]))]
            } else {
                // One room like this known: a second is a coincidence.
                // Several (a corridor): another is no surprise.
                let new_room = if known.len() == 1 { -1 } else { 0 };
                known
                    .iter()
                    .map(|id| (Some(*id), evidence(*id)))
                    .chain([(None, new_room)])
                    .collect()
            }
        }
    } else if let Some(certain) = certain {
        vec![(Some(certain), 0)]
    } else if known.len() == 1 {
        vec![(Some(known[0]), 0)]
    } else {
        // Followed, summoned, recalled: a known room nearby is likeliest; a
        // room new to the map starts a far-off island, so it's least. Out
        // of the dark, so is the room the moves lead to.
        let landing = step.landing.map(|landing| landing.position);
        let near = |id: RoomId| {
            let neighbor = here.is_some_and(|here| here.links.values().any(|to| *to == id));
            let landed = layout
                .room(id)
                .is_some_and(|room| Some(room.position) == landing);
            i32::from(neighbor) + i32::from(landed)
        };
        known
            .iter()
            .map(|id| (Some(*id), near(*id)))
            .chain([(None, -1)])
            .collect()
    };

    choices
        .into_iter()
        .map(|(identity, evidence)| {
            let mut next = guess.clone();
            next.arrival = next.layout.arrive(step, identity);
            next.path.push(next.layout.current.unwrap_or(0));
            next.score += evidence;
            next
        })
        .collect()
}

/// The best explanations, best first: per known room the player might be
/// in, its strongest history. Histories that tie both stay, since their
/// maps differ.
fn strongest(guesses: Vec<Guess>, origin: &Layout) -> Vec<Guess> {
    let mut kept: Vec<Guess> = Vec::new();
    for guess in guesses {
        let rival = guess
            .layout
            .current
            .filter(|current| origin.contains(*current))
            .and_then(|current| {
                kept.iter()
                    .position(|other| other.layout.current == Some(current))
            })
            .filter(|index| kept[*index].score != guess.score);
        match rival {
            Some(index) => {
                if guess.score > kept[index].score {
                    kept[index] = guess;
                }
            }
            None => kept.push(guess),
        }
    }
    // Stable, so ties keep the order the choices were made in.
    kept.sort_by_key(|guess| std::cmp::Reverse(guess.score));
    kept.truncate(MOST_GUESSES);
    kept
}
