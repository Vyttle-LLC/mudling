//! Directions and map coordinates.

use std::fmt;
use std::ops::{Add, Sub};

/// The six ways a move can go.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Direction {
    North,
    East,
    South,
    West,
    Up,
    Down,
}

impl Direction {
    /// Every direction, in the fixed order routes and searches use.
    pub const ALL: [Direction; 6] = [
        Direction::North,
        Direction::East,
        Direction::South,
        Direction::West,
        Direction::Up,
        Direction::Down,
    ];
    /// The directions that stay on one level.
    pub const FLAT: [Direction; 4] = [
        Direction::North,
        Direction::East,
        Direction::South,
        Direction::West,
    ];

    pub fn opposite(self) -> Direction {
        match self {
            Direction::North => Direction::South,
            Direction::South => Direction::North,
            Direction::East => Direction::West,
            Direction::West => Direction::East,
            Direction::Up => Direction::Down,
            Direction::Down => Direction::Up,
        }
    }

    pub fn offset(self) -> GridPoint {
        match self {
            Direction::North => GridPoint::new(0, 1, 0),
            Direction::South => GridPoint::new(0, -1, 0),
            Direction::East => GridPoint::new(1, 0, 0),
            Direction::West => GridPoint::new(-1, 0, 0),
            Direction::Up => GridPoint::new(0, 0, 1),
            Direction::Down => GridPoint::new(0, 0, -1),
        }
    }

    pub fn is_vertical(self) -> bool {
        matches!(self, Direction::Up | Direction::Down)
    }

    /// The word persisted and shown for the direction.
    pub fn name(self) -> &'static str {
        match self {
            Direction::North => "north",
            Direction::East => "east",
            Direction::South => "south",
            Direction::West => "west",
            Direction::Up => "up",
            Direction::Down => "down",
        }
    }

    /// Reads a direction as a player types it or a GMCP exit names it:
    /// "n", "North", "U", any case.
    pub fn parse(word: &str) -> Option<Direction> {
        let word = word.trim();
        Direction::ALL.into_iter().find(|direction| {
            let name = direction.name();
            word.eq_ignore_ascii_case(name) || word.eq_ignore_ascii_case(&name[..1])
        })
    }

    fn bit(self) -> u8 {
        1 << self as u8
    }
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Map coordinates: x grows east, y north, z up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GridPoint {
    pub x: i64,
    pub y: i64,
    pub z: i64,
}

impl GridPoint {
    pub const fn new(x: i64, y: i64, z: i64) -> GridPoint {
        GridPoint { x, y, z }
    }

    /// Steps between two squares, along the grid.
    pub fn distance(self, other: GridPoint) -> i64 {
        (self.x - other.x).abs() + (self.y - other.y).abs() + (self.z - other.z).abs()
    }
}

impl Add for GridPoint {
    type Output = GridPoint;
    fn add(self, other: GridPoint) -> GridPoint {
        GridPoint::new(self.x + other.x, self.y + other.y, self.z + other.z)
    }
}

impl Sub for GridPoint {
    type Output = GridPoint;
    fn sub(self, other: GridPoint) -> GridPoint {
        GridPoint::new(self.x - other.x, self.y - other.y, self.z - other.z)
    }
}

/// A value per direction, stored inline: a room's exits or links.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DirMap<T>([Option<T>; 6]);

impl<T> Default for DirMap<T> {
    fn default() -> DirMap<T> {
        DirMap([None, None, None, None, None, None])
    }
}

impl<T> DirMap<T> {
    pub fn get(&self, direction: Direction) -> Option<&T> {
        self.0[direction as usize].as_ref()
    }

    pub fn insert(&mut self, direction: Direction, value: T) -> Option<T> {
        self.0[direction as usize].replace(value)
    }

    pub fn remove(&mut self, direction: Direction) -> Option<T> {
        self.0[direction as usize].take()
    }

    pub fn contains(&self, direction: Direction) -> bool {
        self.0[direction as usize].is_some()
    }

    /// Entries in [`Direction::ALL`] order.
    pub fn iter(&self) -> impl Iterator<Item = (Direction, &T)> {
        Direction::ALL
            .into_iter()
            .filter_map(|direction| self.get(direction).map(|value| (direction, value)))
    }

    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.0.iter().flatten()
    }

    pub fn len(&self) -> usize {
        self.values().count()
    }

    pub fn is_empty(&self) -> bool {
        self.0.iter().all(Option::is_none)
    }

    pub fn directions(&self) -> DirSet {
        self.iter().map(|(direction, _)| direction).collect()
    }

    pub fn retain(&mut self, mut keep: impl FnMut(Direction, &T) -> bool) {
        for direction in Direction::ALL {
            if let Some(value) = self.get(direction)
                && !keep(direction, value)
            {
                self.remove(direction);
            }
        }
    }
}

impl<T: Copy> DirMap<T> {
    pub fn get_copied(&self, direction: Direction) -> Option<T> {
        self.get(direction).copied()
    }
}

impl<T> FromIterator<(Direction, T)> for DirMap<T> {
    fn from_iter<I: IntoIterator<Item = (Direction, T)>>(entries: I) -> DirMap<T> {
        let mut map = DirMap::default();
        for (direction, value) in entries {
            map.insert(direction, value);
        }
        map
    }
}

/// A set of directions, as bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct DirSet(u8);

impl DirSet {
    pub fn insert(&mut self, direction: Direction) {
        self.0 |= direction.bit();
    }

    pub fn contains(self, direction: Direction) -> bool {
        self.0 & direction.bit() != 0
    }

    pub fn len(self) -> usize {
        self.0.count_ones() as usize
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn union(self, other: DirSet) -> DirSet {
        DirSet(self.0 | other.0)
    }

    pub fn iter(self) -> impl Iterator<Item = Direction> {
        Direction::ALL
            .into_iter()
            .filter(move |direction| self.contains(*direction))
    }
}

impl FromIterator<Direction> for DirSet {
    fn from_iter<I: IntoIterator<Item = Direction>>(directions: I) -> DirSet {
        let mut set = DirSet::default();
        for direction in directions {
            set.insert(direction);
        }
        set
    }
}
