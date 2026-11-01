//! Walk-to: the moves from one room to another along explored links.

use super::geometry::Direction;
use super::room::{MapRoom, RoomId};
use std::collections::{HashMap, VecDeque};

/// One move of a walk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalkStep {
    pub direction: Direction,
    /// The door that way is known to be closed: open it first.
    pub open_first: bool,
}

/// A walk as one line of speedwalk, as the player would type it. It's made
/// only of direction letters, whatever the rooms are called.
pub fn walk_command(steps: &[WalkStep]) -> String {
    let moves: Vec<String> = steps
        .iter()
        .map(|step| match step.open_first {
            true => format!("[{}]", step.direction.letter()),
            false => step.direction.letter().to_owned(),
        })
        .collect();
    moves.join(", ")
}

/// The fewest moves from `start` to `goal` along explored links, by
/// breadth-first search; `None` when no known path leads there. Routes
/// never cross a maze: the way through changes.
pub fn route<'a>(
    room: impl Fn(RoomId) -> Option<&'a MapRoom>,
    start: RoomId,
    goal: RoomId,
) -> Option<Vec<WalkStep>> {
    let mut came_from: HashMap<RoomId, (RoomId, Direction)> = HashMap::new();
    let mut queue = VecDeque::from([start]);
    while let Some(id) = queue.pop_front() {
        if id == goal {
            break;
        }
        let Some(room) = room(id).filter(|room| !room.maze) else {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defaults::commands::parse_speedwalk;

    #[test]
    fn a_walk_reads_back_as_speedwalk_door_and_all() {
        let step = |direction, open_first| WalkStep {
            direction,
            open_first,
        };
        let walk = walk_command(&[
            step(Direction::North, false),
            step(Direction::East, true),
            step(Direction::Down, false),
        ]);
        assert_eq!(walk, "n, [e], d");
        assert_eq!(parse_speedwalk(&walk).map(|steps| steps.len()), Some(3));
    }
}
