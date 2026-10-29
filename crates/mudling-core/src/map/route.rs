//! Walk-to: the moves from one room to another along explored links.

use super::geometry::Direction;
use super::layout::Layout;
use super::room::RoomId;
use std::collections::{HashMap, VecDeque};

/// One move of a walk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalkStep {
    pub direction: Direction,
    /// The door that way is known to be closed: open it first.
    pub open_first: bool,
}

/// The fewest moves from `start` to `goal` along explored links, by
/// breadth-first search; `None` when no known path leads there. Routes
/// never cross a maze: the way through changes.
pub(super) fn route(layout: &Layout, start: RoomId, goal: RoomId) -> Option<Vec<WalkStep>> {
    let mut came_from: HashMap<RoomId, (RoomId, Direction)> = HashMap::new();
    let mut queue = VecDeque::from([start]);
    while let Some(id) = queue.pop_front() {
        if id == goal {
            break;
        }
        let Some(room) = layout.room(id).filter(|room| !room.maze) else {
            continue;
        };
        // A fixed order keeps routes stable between searches.
        for direction in Direction::ALL {
            let Some(next) = room.links.get_copied(direction) else {
                continue;
            };
            if next != start && !came_from.contains_key(&next) {
                came_from.insert(next, (id, direction));
                queue.push_back(next);
            }
        }
    }
    let mut steps = Vec::new();
    let mut at = goal;
    while at != start {
        let (from, direction) = *came_from.get(&at)?;
        let open_first = layout
            .room(from)
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
