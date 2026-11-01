//! The search for an area's best drawing: every way of choosing up to
//! three fixes, tried in order, keeping the cheapest that lines up.
//!
//! Rooms and passages are numbered once per search, so each try works on
//! plain vectors. And a try that runs into a contradiction its anchor
//! forces says which passages forced it: any later choice that fixes none
//! of them must fail the same way, so it's skipped untried, and so is a
//! choice that only adds fixes a failed try never looked at. The drawing
//! chosen is the one trying every choice would choose.

use super::{Fix, MOST_FIXES, Passage};
use crate::map::geometry::GridPoint;
use crate::map::layout::Layout;
use crate::map::room::RoomId;
use std::cell::Cell;
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
    at: Vec<Vec<Edge>>,
    choices: Vec<Choice>,
    groups: Vec<Option<&'a BTreeSet<RoomId>>>,
    /// The room numbered first by id: the drawing keeps it where it is.
    anchor: usize,
}

/// A passage as seen from one of its rooms.
#[derive(Clone, Copy)]
struct Edge {
    passage: usize,
    next: usize,
    /// The way to `next`.
    offset: GridPoint,
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
                let offset = passage.direction.offset();
                at[from].push(Edge {
                    passage: number,
                    next: to,
                    offset,
                });
                at[to].push(Edge {
                    passage: number,
                    next: from,
                    offset: GridPoint::default() - offset,
                });
                (from, to, offset)
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
        })
    }

    fn run(&self) -> Option<Drawing> {
        let mut best: Option<Tried> = None;
        let mut conflicts: Vec<Conflict> = Vec::new();
        let mut sketch = Sketch::new(self);
        let mut failures = Failures::new(self.choices.len());
        for count in 0..=MOST_FIXES {
            // Each fix costs at least a square: no point trying more.
            if best.as_ref().is_some_and(|best| best.cost <= count as i64) {
                break;
            }
            for picks in combinations(self.choices.len(), count) {
                let choice = &picks[..count];
                let doomed = conflicts
                    .iter()
                    .any(|conflict| !choice.iter().any(|fix| self.fixes(*fix, conflict)));
                if doomed {
                    continue;
                }
                if let Some(unread) = failures.repeated_by(choice) {
                    failures.note(choice, unread);
                    continue;
                }
                sketch.start_try();
                let mut lifted = 0;
                for fix in choice {
                    match &self.choices[*fix] {
                        Choice::Long(passage) => sketch.long[*passage] = true,
                        Choice::Lift(rooms) => {
                            lifted += 1;
                            for room in rooms {
                                sketch.kept[*room] = false;
                            }
                        }
                    }
                }
                let Some(anchor) = (0..self.ids.len())
                    .filter(|room| sketch.kept[*room])
                    .min_by_key(|room| self.ids[*room])
                else {
                    continue;
                };
                if let Err(conflict) = sketch.draw(anchor) {
                    conflicts.extend(conflict);
                    failures.note(choice, sketch.unread_fixes());
                    continue;
                }
                let (kept, long, placed) = (&sketch.kept, &sketch.long, &sketch.placed);
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
                        placed: placed.clone(),
                        cost,
                        lifted,
                        moved,
                        choice: choice.to_vec(),
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

    fn obstacle(&self, square: GridPoint) -> bool {
        self.layout
            .rooms_at(square, self.level.zone, self.level.building)
            .any(|room| !self.level.members.contains(&room.id))
    }
}

/// What's known of a square, kept from try to try.
#[derive(Clone, Copy)]
struct Square {
    /// A room that isn't being redrawn holds it.
    in_the_way: bool,
    /// The try that last placed a room here, and which room.
    placed_in_try: u32,
    room: usize,
}

/// Where a try ran into a contradiction, which only some tries need to work
/// out into a [`Conflict`].
struct Clash {
    room: usize,
    passage: Option<usize>,
    other: Option<usize>,
}

/// The fixes a try that failed never looked at, as bits. A try is fully set
/// by what it reads, so adding those fixes to its choice fails the same way.
struct Failures {
    choices: usize,
    /// By the choice, for every size but the largest.
    unread: Vec<u128>,
}

impl Failures {
    fn new(choices: usize) -> Failures {
        Failures {
            choices,
            unread: vec![0; 1 + choices + choices * choices],
        }
    }

    /// Where a choice of up to two fixes is kept.
    fn slot(&self, choice: impl Iterator<Item = usize>) -> usize {
        choice.fold(0, |slot, fix| slot * self.choices + fix + 1)
    }

    /// The failure this choice only repeats, if any: dropping one of its
    /// fixes leaves a choice that failed without looking at that fix.
    fn repeated_by(&self, choice: &[usize]) -> Option<u128> {
        choice.iter().enumerate().find_map(|(index, fix)| {
            let others = choice.iter().copied();
            let others = others.enumerate().filter(|(other, _)| *other != index);
            let unread = self.unread[self.slot(others.map(|(_, fix)| fix))];
            (*fix < u128::BITS as usize && unread >> *fix & 1 == 1).then_some(unread)
        })
    }

    fn note(&mut self, choice: &[usize], unread: u128) {
        if choice.len() < MOST_FIXES {
            let slot = self.slot(choice.iter().copied());
            self.unread[slot] = unread;
        }
    }
}

/// One try's choices and rooms placed so far, and how each was reached. Its
/// buffers carry over between tries, of which a search makes thousands.
struct Sketch<'a> {
    search: &'a Search<'a>,
    kept: Vec<bool>,
    long: Vec<bool>,
    /// The last try that asked whether each passage is long.
    read_in_try: Vec<Cell<u32>>,
    placed: Vec<Option<GridPoint>>,
    rooms_placed: usize,
    squares: SquareMap<Square>,
    this_try: u32,
    /// The passage each room was placed along, back toward where its group
    /// started.
    reached_by: Vec<Option<usize>>,
    queue: VecDeque<usize>,
    wanted: Vec<GridPoint>,
}

impl<'a> Sketch<'a> {
    fn new(search: &'a Search<'a>) -> Sketch<'a> {
        Sketch {
            search,
            kept: vec![true; search.ids.len()],
            long: vec![false; search.ends.len()],
            read_in_try: vec![Cell::new(0); search.ends.len()],
            placed: vec![None; search.ids.len()],
            rooms_placed: 0,
            squares: SquareMap::default(),
            this_try: 0,
            reached_by: vec![None; search.ids.len()],
            queue: VecDeque::new(),
            wanted: Vec::new(),
        }
    }

    fn start_try(&mut self) {
        self.kept.fill(true);
        self.long.fill(false);
        self.placed.fill(None);
        self.rooms_placed = 0;
        self.this_try += 1;
        self.reached_by.fill(None);
    }

    fn is_long(&self, passage: usize) -> bool {
        self.read_in_try[passage].set(self.this_try);
        self.long[passage]
    }

    /// The fixes this try's outcome doesn't depend on: long passages it
    /// never asked about. A lift always counts, as it changes what's kept.
    fn unread_fixes(&self) -> u128 {
        let choices = self.search.choices.iter().take(u128::BITS as usize);
        choices
            .enumerate()
            .fold(0, |unread, (fix, choice)| match choice {
                Choice::Long(passage) if self.read_in_try[*passage].get() != self.this_try => {
                    unread | 1 << fix
                }
                _ => unread,
            })
    }

    /// Where every kept room goes if all passages but the long ones are one
    /// square. When they disagree or put two rooms in a square, the error
    /// holds the contradiction if the anchor's own passages force it: only
    /// that's the same whatever else is chosen.
    fn draw(&mut self, anchor: usize) -> Result<(), Option<Conflict>> {
        let search = self.search;
        self.put(anchor, search.homes[anchor])
            .map_err(|clash| (anchor == search.anchor).then(|| self.conflict(&clash)))?;
        let total = self.kept.iter().filter(|kept| **kept).count();
        // Rooms joined to the rest only by long passages go to the free
        // square nearest to where those passages point.
        while self.rooms_placed < total {
            let room = self.next_room_on_long_passages().ok_or(None)?;
            let wanted = std::mem::take(&mut self.wanted);
            let spot = nearest_free(&wanted, |square| self.is_taken(square));
            self.wanted = wanted;
            self.put(room, spot.ok_or(None)?).map_err(|_| None)?;
        }
        Ok(())
    }

    /// The first room left that a long passage leads to from a room placed,
    /// with `wanted` set to the squares those passages point at.
    fn next_room_on_long_passages(&mut self) -> Option<usize> {
        let search = self.search;
        for room in 0..search.ids.len() {
            if !self.kept[room] || self.placed[room].is_some() {
                continue;
            }
            self.wanted.clear();
            for edge in &search.at[room] {
                if let Some(square) = self.placed[edge.next]
                    && self.is_long(edge.passage)
                {
                    self.wanted.push(square - edge.offset);
                }
            }
            if !self.wanted.is_empty() {
                return Some(room);
            }
        }
        None
    }

    fn square(&mut self, position: GridPoint) -> &mut Square {
        let search = self.search;
        self.squares.entry(position).or_insert_with(|| Square {
            in_the_way: search.obstacle(position),
            placed_in_try: 0,
            room: 0,
        })
    }

    fn is_taken(&mut self, position: GridPoint) -> bool {
        let this_try = self.this_try;
        let square = self.square(position);
        square.in_the_way || square.placed_in_try == this_try
    }

    /// Places a room if its square is free; otherwise says what's there: a
    /// room of this try, or `None` for one in the way.
    fn claim(&mut self, room: usize, position: GridPoint) -> Result<(), Option<usize>> {
        let this_try = self.this_try;
        let square = self.square(position);
        if square.placed_in_try == this_try {
            return Err(Some(square.room));
        }
        if square.in_the_way {
            return Err(None);
        }
        square.placed_in_try = this_try;
        square.room = room;
        self.placed[room] = Some(position);
        self.rooms_placed += 1;
        Ok(())
    }

    /// Places a room and everything its one-square passages reach.
    fn put(&mut self, room: usize, position: GridPoint) -> Result<(), Clash> {
        self.claim(room, position).map_err(|other| Clash {
            room,
            passage: None,
            other,
        })?;
        let search = self.search;
        self.queue.clear();
        self.queue.push_back(room);
        while let Some(here) = self.queue.pop_front() {
            let square = self.placed[here].expect("placed");
            for &Edge {
                passage,
                next,
                offset,
            } in &search.at[here]
            {
                if !self.kept[next] {
                    continue;
                }
                let position = square + offset;
                // Long or not, a passage that already lines up changes nothing.
                if self.placed[next] == Some(position) || self.is_long(passage) {
                    continue;
                }
                let passage = Some(passage);
                let clash = |other| Clash {
                    room: here,
                    passage,
                    other,
                };
                if self.placed[next].is_some() {
                    return Err(clash(Some(next)));
                }
                self.claim(next, position).map_err(clash)?;
                self.reached_by[next] = passage;
                self.queue.push_back(next);
            }
        }
        Ok(())
    }

    /// The passages that placed a clash's rooms, and the one between them.
    fn conflict(&self, clash: &Clash) -> Conflict {
        let &Clash {
            room,
            passage,
            other,
        } = clash;
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

fn nearest_free(
    wanted: &[GridPoint],
    mut taken: impl FnMut(GridPoint) -> bool,
) -> Option<GridPoint> {
    let center = wanted[0];
    // Nothing is closer to a single square than itself.
    if wanted.len() == 1 && !taken(center) {
        return Some(center);
    }
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

/// Every way to pick `count` of `0..items`, in order, each in the front of
/// its array.
fn combinations(items: usize, count: usize) -> Vec<[usize; MOST_FIXES]> {
    fn pick(
        start: usize,
        items: usize,
        picked: usize,
        count: usize,
        chosen: &mut [usize; MOST_FIXES],
        all: &mut Vec<[usize; MOST_FIXES]>,
    ) {
        if picked == count {
            all.push(*chosen);
            return;
        }
        for index in start..items {
            chosen[picked] = index;
            pick(index + 1, items, picked + 1, count, chosen, all);
        }
    }
    let mut all = Vec::new();
    pick(0, items, 0, count, &mut [0; MOST_FIXES], &mut all);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_choice_repeats_a_failure_only_by_adding_fixes_it_never_read() {
        let mut failures = Failures::new(5);
        let unread_3_and_4 = 1 << 3 | 1 << 4;
        failures.note(&[0, 1], unread_3_and_4);

        assert_eq!(failures.repeated_by(&[0, 1, 3]), Some(unread_3_and_4));
        assert_eq!(failures.repeated_by(&[0, 1, 2]), None, "it read fix 2");
        assert_eq!(failures.repeated_by(&[0, 2, 3]), None, "not that failure");
    }

    #[test]
    fn a_repeat_passes_on_what_the_failure_never_read() {
        let mut failures = Failures::new(5);
        let unread_1_and_2 = 1 << 1 | 1 << 2;
        failures.note(&[0], unread_1_and_2);

        let repeat = failures.repeated_by(&[0, 1]).expect("fix 1 was never read");
        failures.note(&[0, 1], repeat);

        assert_eq!(failures.repeated_by(&[0, 1, 2]), Some(unread_1_and_2));
    }
}
