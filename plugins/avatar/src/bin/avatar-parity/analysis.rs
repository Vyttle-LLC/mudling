//! Questions about one map that a comparison doesn't answer: which rooms
//! share an identity, and whether they're really one place.

use crate::model::{Room, World};
use std::collections::{BTreeMap, HashSet};

/// How far a pair of rooms is walked looking for a difference.
const DEEPEST_WALK: usize = 12;

type Identity = (String, String, String, String);

/// Rooms that share a name, zone, description and exits: an identity the
/// room merge (VYT-227) won't unite, as it needs one room on each side.
#[derive(Debug, Default)]
pub struct LookAlikes {
    pub identities: usize,
    pub shared: usize,
    pub rooms_in_shared: usize,
    /// Shared identities whose rooms the map's own paths all tell apart.
    pub true_look_alikes: usize,
    /// Rooms in identities holding two that nothing tells apart.
    pub maybe_copies: Vec<String>,
    pub maybe_copy_rooms: usize,
    pub largest: Vec<(usize, String)>,
}

fn groups(world: &World) -> BTreeMap<Identity, Vec<&Room>> {
    let mut groups: BTreeMap<Identity, Vec<&Room>> = BTreeMap::new();
    for room in world.rooms.values() {
        groups.entry(room.identity()).or_default().push(room);
    }
    groups
}

/// Whether a walk the same way from each room reaches rooms that differ, or
/// reaches one room from the other: then they're two places.
fn told_apart(world: &World, a: &Room, b: &Room) -> bool {
    let mut seen = HashSet::new();
    let mut walks = vec![(a.id, b.id, 0)];
    while let Some((one, other, depth)) = walks.pop() {
        if !seen.insert((one, other)) || depth == DEEPEST_WALK {
            continue;
        }
        let (Some(one), Some(other)) = (world.rooms.get(&one), world.rooms.get(&other)) else {
            continue;
        };
        if one.identity() != other.identity() {
            return true;
        }
        for (direction, to) in &one.links {
            if let Some(there) = other.links.get(direction)
                && to != there
            {
                walks.push((*to, *there, depth + 1));
            }
        }
    }
    false
}

pub fn look_alikes(world: &World) -> LookAlikes {
    let groups = groups(world);
    let mut found = LookAlikes {
        identities: groups.len(),
        ..LookAlikes::default()
    };
    for (identity, rooms) in &groups {
        if rooms.len() < 2 {
            continue;
        }
        found.shared += 1;
        found.rooms_in_shared += rooms.len();
        found.largest.push((rooms.len(), identity.1.clone()));
        let every_pair = rooms
            .iter()
            .enumerate()
            .flat_map(|(index, a)| rooms[index + 1..].iter().map(move |b| (*a, *b)));
        let tied: HashSet<i64> = every_pair
            .filter(|(a, b)| !told_apart(world, a, b))
            .flat_map(|(a, b)| [a.id, b.id])
            .collect();
        if tied.is_empty() {
            found.true_look_alikes += 1;
        } else {
            found.maybe_copy_rooms += tied.len();
            found
                .maybe_copies
                .push(format!("{} × {} ({})", tied.len(), identity.1, identity.0));
        }
    }
    found.largest.sort_by(|a, b| b.cmp(a));
    found
}

pub fn maze_squares(world: &World) -> Vec<String> {
    let mazes = world.rooms.values().filter(|room| room.maze);
    mazes.map(Room::label).collect()
}

pub fn rooms_without_a_description(world: &World) -> Vec<String> {
    let rooms = world.rooms.values();
    let empty = rooms.filter(|room| room.description.is_empty());
    empty
        .map(|room| format!("{:?} ({})", room.name, room.zone))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room(id: i64, name: &str, links: &[(&str, i64)]) -> Room {
        Room {
            id,
            name: name.to_owned(),
            links: links.iter().map(|(d, to)| ((*d).to_owned(), *to)).collect(),
            ..Room::default()
        }
    }

    fn world(rooms: Vec<Room>) -> World {
        World {
            rooms: rooms.into_iter().map(|room| (room.id, room)).collect(),
            ..World::default()
        }
    }

    #[test]
    fn rooms_leading_to_different_places_are_look_alikes() {
        let found = look_alikes(&world(vec![
            room(1, "Trail", &[("east", 3)]),
            room(2, "Trail", &[("east", 4)]),
            room(3, "Gate", &[]),
            room(4, "Well", &[]),
        ]));
        assert_eq!(found.shared, 1);
        assert_eq!(found.true_look_alikes, 1);
        assert_eq!(found.maybe_copy_rooms, 0);
    }

    #[test]
    fn rooms_nothing_tells_apart_may_be_one_room() {
        let found = look_alikes(&world(vec![
            room(1, "Trail", &[("east", 3)]),
            room(2, "Trail", &[("east", 3)]),
            room(3, "Gate", &[]),
        ]));
        assert_eq!(found.true_look_alikes, 0);
        assert_eq!(found.maybe_copy_rooms, 2);
    }
}
