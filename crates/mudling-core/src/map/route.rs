//! Walk-to: the moves from one room to another along explored links.

use super::geometry::Direction;
use super::room::{MapRoom, RoomId};
use std::collections::{HashMap, VecDeque};
use std::fmt::Write;

/// The most a speedwalk step can count, in its two digits.
const MOST_IN_A_STEP: u32 = 99;

/// One move of a walk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalkStep {
    pub direction: Direction,
    /// The door that way is known to be closed: open it first.
    pub open_first: bool,
}

/// How a search first came to a room.
#[derive(Clone, Copy)]
struct Reached {
    from: RoomId,
    direction: Direction,
    moves: u32,
}

/// Every room reachable from `start`, by breadth-first search, as far as
/// `goal` when there is one. Routes never cross a maze: the way through
/// changes.
fn spread<'a>(
    room: &impl Fn(RoomId) -> Option<&'a MapRoom>,
    start: RoomId,
    goal: Option<RoomId>,
) -> HashMap<RoomId, Reached> {
    let mut reached: HashMap<RoomId, Reached> = HashMap::new();
    let mut queue = VecDeque::from([start]);
    while let Some(id) = queue.pop_front() {
        if Some(id) == goal {
            break;
        }
        let Some(room) = room(id).filter(|room| !room.maze) else {
            continue;
        };
        let moves = reached.get(&id).map_or(0, |at| at.moves) + 1;
        // A fixed order keeps routes stable between searches.
        for direction in Direction::ALL {
            let Some(next) = room.links.get_copied(direction) else {
                continue;
            };
            if next != start && !reached.contains_key(&next) {
                let from = id;
                reached.insert(
                    next,
                    Reached {
                        from,
                        direction,
                        moves,
                    },
                );
                queue.push_back(next);
            }
        }
    }
    reached
}

/// The fewest moves from `start` to `goal` along explored links; `None` when
/// no known path leads there.
pub fn route<'a>(
    room: impl Fn(RoomId) -> Option<&'a MapRoom>,
    start: RoomId,
    goal: RoomId,
) -> Option<Vec<WalkStep>> {
    let reached = spread(&room, start, Some(goal));
    let mut steps = Vec::new();
    let mut at = goal;
    while at != start {
        let Reached {
            from, direction, ..
        } = *reached.get(&at)?;
        let open_first = room(from)
            .and_then(|room| room.exits.get(direction))
            .is_some_and(|exit| exit.closed);
        steps.push(WalkStep {
            direction,
            open_first,
        });
        at = from;
    }
    steps.reverse();
    Some(steps)
}

/// How many moves away every room reachable from `start` is.
pub fn distances<'a>(
    room: impl Fn(RoomId) -> Option<&'a MapRoom>,
    start: RoomId,
) -> HashMap<RoomId, u32> {
    let reached = spread(&room, start, None);
    reached.iter().map(|(id, at)| (*id, at.moves)).collect()
}

/// A walk in speedwalk shorthand, such as `3n[e]2s`. Every step is counted or
/// bracketed, so even a walk of one move reads as speedwalk.
pub fn speedwalk(steps: &[WalkStep]) -> String {
    let mut text = String::new();
    let mut run: Option<(Direction, u32)> = None;
    let write_run = |text: &mut String, run: Option<(Direction, u32)>| {
        if let Some((direction, count)) = run {
            write!(text, "{count}{}", direction.letter()).expect("a String takes any text");
        }
    };
    for step in steps {
        match &mut run {
            Some((direction, count))
                if *direction == step.direction && !step.open_first && *count < MOST_IN_A_STEP =>
            {
                *count += 1;
            }
            _ => {
                write_run(&mut text, run.take());
                if step.open_first {
                    write!(text, "[{}]", step.direction.letter()).expect("a String takes any text");
                } else {
                    run = Some((step.direction, 1));
                }
            }
        }
    }
    write_run(&mut text, run);
    text
}
