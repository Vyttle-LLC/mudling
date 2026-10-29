//! The search for an area's best drawing: every way of choosing up to
//! three fixes, tried in order, keeping the cheapest that lines up.
//!
//! Rooms and passages are numbered once per search, so each try works on
//! plain vectors. And a try that runs into a contradiction its anchor
//! forces says which passages forced it: any later choice that fixes none
//! of them must fail the same way, so it's skipped untried. The drawing
//! chosen is the one trying every choice would choose.

use super::{Fix, MOST_FIXES, Passage};
use crate::map::geometry::GridPoint;
use crate::map::layout::Layout;
use crate::map::room::RoomId;
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::hash::{BuildHasherDefault, Hasher};

/// The map being redrawn: its area's rooms are free to move; every other
/// room on it is in the way.
pub(super) struct Level<'a> {
    pub zone: &'a str,
    pub building: Option<RoomId>,
    pub members: &'a HashSet<RoomId>,
}

/// The drawing chosen: where the rooms that stay go, and the groups lifted
/// into buildings.
pub(super) struct Drawing {
    pub positions: Vec<(RoomId, GridPoint)>,
    pub lifted: Vec<BTreeSet<RoomId>>,
}

/// The best drawing of `area` (in its search order, anchor first among
/// equals by id) with `fixes`, or `None` when no choice of a few lines up.
pub(super) fn best_drawing(
    layout: &Layout,
    area: &[RoomId],
    passages: &[Passage],
    fixes: &[Fix],
    level: &Level<'_>,
) -> Option<Drawing> {
    let search = Search::new(layout, area, passages, fixes, level)?;
    search.run()
}

/// A fix, by the numbers of what it touches.
enum Choice {
    Long(usize),
    Lift(Vec<usize>),
}

struct Search<'a> {
    layout: &'a Layout,
    level: &'a Level<'a>,
    ids: Vec<RoomId>,
    homes: Vec<GridPoint>,
    /// Each passage's rooms (going north or east) and its offset.
    ends: Vec<(usize, usize, GridPoint)>,
    /// The passages at each room.
    at: Vec<Vec<usize>>,
    choices: Vec<Choice>,
    groups: Vec<Option<&'a BTreeSet<RoomId>>>,
    /// The room numbered first by id: the drawing keeps it where it is.
    anchor: usize,
    obstacles: RefCell<SquareMap<bool>>,
}

/// A contradiction a try ran into: one-square passages that can't all hold.
struct Conflict {
    passages: Vec<bool>,
    rooms: Vec<bool>,
}

struct Tried {
    placed: Vec<Option<GridPoint>>,
    cost: i64,
    lifted: usize,
    moved: i64,
    choice: Vec<usize>,
}

impl<'a> Search<'a> {
    fn new(
        layout: &'a Layout,
        area: &[RoomId],
        passages: &[Passage],
        fixes: &'a [Fix],
        level: &'a Level<'a>,
    ) -> Option<Search<'a>> {
        let index: HashMap<RoomId, usize> = area
            .iter()
            .enumerate()
            .map(|(index, id)| (*id, index))
            .collect();
        let homes = area
            .iter()
            .map(|id| layout.room(*id).map(|room| room.position))
            .collect::<Option<Vec<_>>>()?;
        let mut at = vec![Vec::new(); area.len()];
        let ends: Vec<(usize, usize, GridPoint)> = passages
            .iter()
            .enumerate()
            .map(|(number, passage)| {
                let (from, to) = (index[&passage.from], index[&passage.to]);
                at[from].push(number);
                at[to].push(number);
                (from, to, passage.direction.offset())
            })
            .collect();
        let numbers: HashMap<Passage, usize> = passages
            .iter()
            .enumerate()
            .map(|(number, passage)| (*passage, number))
            .collect();
        let choices = fixes
            .iter()
            .map(|fix| match fix {
                Fix::Long(passage) => Choice::Long(numbers[passage]),
                Fix::Lift(group) => Choice::Lift(group.iter().map(|id| index[id]).collect()),
            })
            .collect();
        let groups = fixes
            .iter()
            .map(|fix| match fix {
                Fix::Long(_) => None,
                Fix::Lift(group) => Some(group),
            })
            .collect();
        let anchor = (0..area.len()).min_by_key(|room| area[*room])?;
        Some(Search {
            layout,
            level,
            ids: area.to_vec(),
            homes,
            ends,
            at,
            choices,
            groups,
            anchor,
            obstacles: RefCell::default(),
        })
    }

    fn run(&self) -> Option<Drawing> {
        let mut best: Option<Tried> = None;
        let mut conflicts: Vec<Conflict> = Vec::new();
        let mut long = vec![false; self.ends.len()];
        let mut kept = vec![true; self.ids.len()];
        for count in 0..=MOST_FIXES {
            // Each fix costs at least a square: no point trying more.
            if best.as_ref().is_some_and(|best| best.cost <= count as i64) {
                break;
            }
            for choice in combinations(self.choices.len(), count) {
                let doomed = conflicts
                    .iter()
                    .any(|conflict| !choice.iter().any(|fix| self.fixes(*fix, conflict)));
                if doomed {
                    continue;
                }
                long.fill(false);
                kept.fill(true);
                let mut lifted = 0;
                for fix in &choice {
                    match &self.choices[*fix] {
                        Choice::Long(passage) => long[*passage] = true,
                        Choice::Lift(rooms) => {
                            lifted += 1;
                            for room in rooms {
                                kept[*room] = false;
                            }
                        }
                    }
                }
                let Some(anchor) = (0..self.ids.len())
                    .filter(|room| kept[*room])
                    .min_by_key(|room| self.ids[*room])
                else {
                    continue;
                };
                let placed = match self.positions(&kept, &long, anchor) {
                    Ok(placed) => placed,
                    Err(conflict) => {
                        // Only a contradiction the anchor forces is the
                        // same whatever else is chosen.
                        if anchor == self.anchor {
                            conflicts.extend(conflict);
                        }
                        continue;
                    }
                };
                let at = |room: usize| placed[room].expect("every kept room is placed");
                let stretch: i64 = (0..self.ends.len())
                    .filter(|passage| long[*passage])
                    .map(|passage| {
                        let (from, to, offset) = self.ends[passage];
                        if kept[from] && kept[to] {
                            at(to).distance(at(from) + offset)
                        } else {
                            0
                        }
                    })
                    .sum();
                let cost = lifted as i64 + stretch;
                let moved = (0..self.ids.len())
                    .filter(|room| kept[*room])
                    .map(|room| at(room).distance(self.homes[room]))
                    .sum();
                let better = best.as_ref().is_none_or(|best| {
                    (cost, lifted, moved) < (best.cost, best.lifted, best.moved)
                });
                if better {
                    best = Some(Tried {
                        placed,
                        cost,
                        lifted,
                        moved,
                        choice,
                    });
                }
            }
        }
        let best = best?;
        Some(Drawing {
            positions: best
                .placed
                .iter()
                .enumerate()
                .filter_map(|(room, position)| Some((self.ids[room], (*position)?)))
                .collect(),
            lifted: best
                .choice
                .iter()
                .filter_map(|fix| self.groups[*fix].cloned())
                .collect(),
        })
    }

    /// Whether a fix avoids a contradiction: it draws one of its passages
    /// long, or lifts out one of its rooms (or the anchor, which redraws
    /// everything from another room).
    fn fixes(&self, fix: usize, conflict: &Conflict) -> bool {
        match &self.choices[fix] {
            Choice::Long(passage) => conflict.passages[*passage],
            Choice::Lift(rooms) => rooms
                .iter()
                .any(|room| *room == self.anchor || conflict.rooms[*room]),
        }
    }

    /// Where every kept room goes if all passages but the long ones are one
    /// square. When they disagree or put two rooms in a square, the error
    /// holds the contradiction if the anchor's own passages force it.
    fn positions(
        &self,
        kept: &[bool],
        long: &[bool],
        anchor: usize,
    ) -> Result<Vec<Option<GridPoint>>, Option<Conflict>> {
        let mut sketch = Sketch {
            search: self,
            kept,
            long,
            placed: vec![None; self.ids.len()],
            occupants: SquareMap::default(),
            reached_by: vec![None; self.ids.len()],
        };
        sketch.put(anchor, self.homes[anchor]).map_err(Some)?;
        let total = kept.iter().filter(|kept| **kept).count();
        let mut placed = sketch.occupants.len();
        // Rooms joined to the rest only by long passages go to the free
        // square nearest to where those passages point.
        while placed < total {
            let (room, wanted) = (0..self.ids.len())
                .filter(|room| kept[*room] && sketch.placed[*room].is_none())
                .find_map(|room| {
                    let wanted: Vec<GridPoint> = self.at[room]
                        .iter()
                        .filter(|passage| long[**passage])
                        .filter_map(|passage| {
                            let (from, to, offset) = self.ends[*passage];
                            if from == room {
                                sketch.placed[to].map(|to| to - offset)
                            } else {
                                sketch.placed[from].map(|from| from + offset)
                            }
                        })
                        .collect();
                    (!wanted.is_empty()).then_some((room, wanted))
                })
                .ok_or(None)?;
            let spot = nearest_free(&wanted, |square| sketch.is_taken(square)).ok_or(None)?;
            sketch.put(room, spot).map_err(|_| None)?;
            placed = sketch.occupants.len();
        }
        Ok(sketch.placed)
    }

    fn obstacle(&self, square: GridPoint) -> bool {
        *self
            .obstacles
            .borrow_mut()
            .entry(square)
            .or_insert_with(|| {
                self.layout
                    .rooms_at(square, self.level.zone, self.level.building)
                    .any(|room| !self.level.members.contains(&room.id))
            })
    }
}

/// One try's rooms placed so far, and how each was reached.
struct Sketch<'a> {
    search: &'a Search<'a>,
    kept: &'a [bool],
    long: &'a [bool],
    placed: Vec<Option<GridPoint>>,
    occupants: SquareMap<usize>,
    /// The passage each room was placed along, back toward where its group
    /// started.
    reached_by: Vec<Option<usize>>,
}

impl Sketch<'_> {
    fn is_taken(&self, square: GridPoint) -> bool {
        self.occupants.contains_key(&square) || self.search.obstacle(square)
    }

    /// Places a room and everything its one-square passages reach. On a
    /// contradiction, returns the passages that forced it.
    fn put(&mut self, room: usize, position: GridPoint) -> Result<(), Conflict> {
        if self.is_taken(position) {
            let occupant = self.occupants.get(&position).copied();
            return Err(self.conflict(room, None, occupant));
        }
        self.place(room, position);
        let mut queue = VecDeque::from([room]);
        while let Some(here) = queue.pop_front() {
            for &passage in &self.search.at[here] {
                let (from, to, offset) = self.search.ends[passage];
                if self.long[passage] || !self.kept[from] || !self.kept[to] {
                    continue;
                }
                let (next, position) = if from == here {
                    (to, self.placed[here].expect("placed") + offset)
                } else {
                    (from, self.placed[here].expect("placed") - offset)
                };
                if let Some(existing) = self.placed[next] {
                    if existing != position {
                        return Err(self.conflict(here, Some(passage), Some(next)));
                    }
                    continue;
                }
                if self.is_taken(position) {
                    let occupant = self.occupants.get(&position).copied();
                    return Err(self.conflict(here, Some(passage), occupant));
                }
                self.place(next, position);
                self.reached_by[next] = Some(passage);
                queue.push_back(next);
            }
        }
        Ok(())
    }

    fn place(&mut self, room: usize, position: GridPoint) {
        self.placed[room] = Some(position);
        self.occupants.insert(position, room);
    }

    /// The passages that placed `room` and `other`, and the one between them.
    fn conflict(&self, room: usize, passage: Option<usize>, other: Option<usize>) -> Conflict {
        let mut conflict = Conflict {
            passages: vec![false; self.search.ends.len()],
            rooms: vec![false; self.search.ids.len()],
        };
        if let Some(passage) = passage {
            conflict.passages[passage] = true;
        }
        for start in [Some(room), other].into_iter().flatten() {
            let mut at = start;
            conflict.rooms[at] = true;
            while let Some(passage) = self.reached_by[at] {
                conflict.passages[passage] = true;
                let (from, to, _) = self.search.ends[passage];
                at = if from == at { to } else { from };
                conflict.rooms[at] = true;
            }
        }
        for (passage, &(from, to, _)) in self.search.ends.iter().enumerate() {
            if conflict.passages[passage] {
                conflict.rooms[from] = true;
                conflict.rooms[to] = true;
            }
        }
        conflict
    }
}

fn nearest_free(wanted: &[GridPoint], taken: impl Fn(GridPoint) -> bool) -> Option<GridPoint> {
    let center = wanted[0];
    let mut best: Option<(GridPoint, i64)> = None;
    for dx in -3..=3 {
        for dy in -3..=3 {
            let spot = GridPoint::new(center.x + dx, center.y + dy, center.z);
            if taken(spot) {
                continue;
            }
            let cost: i64 = wanted.iter().map(|want| spot.distance(*want)).sum();
            if best.is_none_or(|(_, best)| cost < best) {
                best = Some((spot, cost));
            }
        }
    }
    best.map(|(spot, _)| spot)
}

/// Every way to pick `count` of `0..items`, in order.
fn combinations(items: usize, count: usize) -> Vec<Vec<usize>> {
    fn pick(
        start: usize,
        items: usize,
        count: usize,
        chosen: &mut Vec<usize>,
        all: &mut Vec<Vec<usize>>,
    ) {
        if chosen.len() == count {
            all.push(chosen.clone());
            return;
        }
        for index in start..items {
            chosen.push(index);
            pick(index + 1, items, count, chosen, all);
            chosen.pop();
        }
    }
    let mut all = Vec::new();
    pick(0, items, count, &mut Vec::new(), &mut all);
    all
}

/// Squares hash often and are trusted, local keys: a multiply-and-rotate
/// hash is plenty, and far cheaper than the default.
#[derive(Default)]
struct SquareHasher(u64);

impl Hasher for SquareHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.write_u64(u64::from(*byte));
        }
    }

    fn write_u64(&mut self, value: u64) {
        self.0 = (self.0.rotate_left(5) ^ value).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }

    fn write_i64(&mut self, value: i64) {
        self.write_u64(value as u64);
    }
}

type SquareMap<V> = HashMap<GridPoint, V, BuildHasherDefault<SquareHasher>>;
