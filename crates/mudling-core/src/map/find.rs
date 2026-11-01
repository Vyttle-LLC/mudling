//! What the Find panel searches (PRD §5 #7): rooms from the map, mobs from
//! sightings and marks, items from what's carried, what's on the ground here
//! and the rooms noted for gear or a fountain.
//!
//! Scanning the stores is the costly part, so [`FindCache`] keeps what it read
//! against their revisions, and a search only walks that list. A shell runs
//! [`FindCache::search`] off the UI thread, on a clone of [`FindStores`].

use super::mobs::Mark;
use super::room::{MapRoom, RoomId, feature};
use super::route::distances;
use crate::persist::Timestamp;
use crate::store::{
    Batch, CharactersState, MapState, Memo, MobsState, Revision, RoomState, StoreHub, Versioned,
};
use memchr::memmem::Finder;
use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap};

/// A short query can match thousands; a panel only needs the nearest.
pub const MOST_RESULTS: usize = 50;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FindKind {
    Room,
    /// A room found by the player's notes on it, which are its name here.
    Note,
    Mob,
    Item,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FindFilter {
    #[default]
    All,
    Rooms,
    Mobs,
    Items,
}

impl FindFilter {
    pub const ALL: [FindFilter; 4] = [
        FindFilter::All,
        FindFilter::Rooms,
        FindFilter::Mobs,
        FindFilter::Items,
    ];

    fn admits(self, kind: FindKind) -> bool {
        match self {
            FindFilter::All => true,
            FindFilter::Rooms => matches!(kind, FindKind::Room | FindKind::Note),
            FindFilter::Mobs => kind == FindKind::Mob,
            FindFilter::Items => kind == FindKind::Item,
        }
    }
}

/// Where a result is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Spot {
    Room(RoomId),
    Carried,
    /// Marked, but never seen in a room of this map.
    Unseen,
}

/// [`Spot`], in words a row can show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FindPlace {
    Room {
        id: RoomId,
        name: String,
        area: String,
    },
    Carried,
    Unseen,
}

/// How far a result is from the player.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    Here,
    Steps(u32),
    /// No explored path leads there.
    NoPath,
    /// The map doesn't know where the player is.
    PlayerLost,
    /// It's in no room.
    NotARoom,
}

impl Reach {
    /// Reachable results first, nearest first.
    fn rank(self) -> u32 {
        match self {
            Reach::Here => 0,
            Reach::Steps(steps) => steps,
            Reach::NoPath | Reach::PlayerLost => u32::MAX - 1,
            Reach::NotARoom => u32::MAX,
        }
    }
}

/// One thing that can be found.
#[derive(Clone, Debug, PartialEq)]
struct FindEntry {
    kind: FindKind,
    name: String,
    spot: Spot,
    marks: Vec<Mark>,
    seen: Option<Timestamp>,
    /// What a search reads, in lowercase.
    text: String,
}

impl FindEntry {
    fn in_room(kind: FindKind, name: &str, room: &MapRoom) -> FindEntry {
        FindEntry {
            kind,
            name: name.to_owned(),
            spot: Spot::Room(room.id),
            marks: Vec::new(),
            seen: None,
            text: name.to_lowercase(),
        }
    }

    /// `word`, which isn't shown, finds this too.
    fn also_found_by(mut self, word: &str) -> FindEntry {
        self.text.push(' ');
        self.text.push_str(word);
        self
    }

    fn held(name: String, spot: Spot) -> FindEntry {
        FindEntry {
            kind: FindKind::Item,
            text: name.to_lowercase(),
            name,
            spot,
            marks: Vec::new(),
            seen: None,
        }
    }
}

/// One row of the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FindRow {
    pub kind: FindKind,
    pub name: String,
    pub place: FindPlace,
    pub reach: Reach,
    pub marks: Vec<Mark>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FindResults {
    /// The nearest [`MOST_RESULTS`].
    pub rows: Vec<FindRow>,
    /// How many matched in all.
    pub matched: usize,
    /// How many the filter lets through, whatever the query.
    pub known: usize,
}

/// The stores Find reads, kept up to date from each frame's batch. A clone
/// costs a mob's name or two: the map's rooms are shared.
#[derive(Clone, Debug, Default)]
pub struct FindStores {
    map: Versioned<MapState>,
    /// The store revision the rooms last changed at, which moves don't touch.
    rooms_changed: Revision,
    mobs: Versioned<MobsState>,
    room: Versioned<RoomState>,
    characters: Versioned<CharactersState>,
}

impl FindStores {
    pub fn subscribed(hub: &StoreHub) -> FindStores {
        let map: Versioned<MapState> = hub.subscribe();
        FindStores {
            rooms_changed: map.revision(),
            map,
            mobs: hub.subscribe(),
            room: hub.subscribe(),
            characters: hub.subscribe(),
        }
    }

    /// Stores that aren't a session's, such as a made-up sample.
    pub fn of(
        map: Versioned<MapState>,
        mobs: Versioned<MobsState>,
        room: Versioned<RoomState>,
        characters: Versioned<CharactersState>,
    ) -> FindStores {
        FindStores {
            rooms_changed: map.revision(),
            map,
            mobs,
            room,
            characters,
        }
    }

    /// Takes a frame's batch; `true` when it changed anything Find reads.
    pub fn apply(&mut self, batch: &Batch) -> bool {
        if let Some(map) = &batch.map {
            self.map.apply_diff(map);
            if !map.change.rooms.is_empty() || !map.change.removed.is_empty() {
                self.rooms_changed = map.revision;
            }
        }
        if let Some(mobs) = &batch.mobs {
            self.mobs.apply_diff(mobs);
        }
        if let Some(room) = &batch.room {
            self.room.apply_diff(room);
        }
        if let Some(characters) = &batch.characters {
            self.characters.apply_diff(characters);
        }
        batch.map.is_some()
            || batch.mobs.is_some()
            || batch.room.is_some()
            || batch.characters.is_some()
    }

    pub fn map(&self) -> &MapState {
        self.map.value()
    }

    /// The room a walk starts from, when the map is sure of one: moves from a
    /// guess would go astray.
    pub fn walk_start(&self) -> Option<RoomId> {
        let whereabouts = self.map().whereabouts();
        let sure = !whereabouts.unsure && whereabouts.dark_position.is_none();
        whereabouts.room.filter(|_| sure)
    }

    fn area_name(&self, room: &MapRoom) -> String {
        let area = self.map().derived().and_then(|d| d.area(&room.zone));
        area.map_or_else(|| room.zone.clone(), |area| area.name.name.clone())
    }

    /// What the map knows of: its rooms, the mobs seen in them and marked,
    /// and the items it noted. Each sighting's marks are worked out once for
    /// all the rooms with that line.
    fn world(&self) -> Vec<FindEntry> {
        let marks = &self.mobs.value().marks;
        let mut marks_of: HashMap<&str, Vec<Mark>> = HashMap::new();
        let mut covered = BTreeSet::new();
        let mut entries = Vec::with_capacity(self.map().rooms().len());
        for room in self.map().rooms() {
            entries.push(FindEntry::in_room(FindKind::Room, &room.name, room));
            if !room.notes.is_empty() {
                entries.push(FindEntry::in_room(FindKind::Note, &room.notes, room));
            }
            if room.features.contains(feature::GEAR) {
                let name = if room.notes.is_empty() {
                    "Important gear"
                } else {
                    &room.notes
                };
                entries.push(
                    FindEntry::in_room(FindKind::Item, name, room).also_found_by("important gear"),
                );
            }
            if room.features.contains(feature::FOUNTAIN) {
                entries.push(FindEntry::in_room(FindKind::Item, "A fountain", room));
            }
            for (title, seen) in &room.sightings {
                let on_title = marks_of.entry(title).or_insert_with(|| {
                    let covering = marks.iter().filter(|(mark, name)| mark.covers(name, title));
                    covering
                        .map(|held| {
                            covered.insert(held);
                            held.0
                        })
                        .collect()
                });
                let mut entry = on_title.iter().fold(
                    FindEntry::in_room(FindKind::Mob, title, room),
                    |entry, mark| entry.also_found_by(mark.name()),
                );
                entry.marks.clone_from(on_title);
                entry.seen = Some(*seen);
                entries.push(entry);
            }
        }
        let unseen = marks.iter().filter(|held| !covered.contains(held));
        entries.extend(unseen.map(|(mark, name)| FindEntry {
            kind: FindKind::Mob,
            name: name.clone(),
            spot: Spot::Unseen,
            marks: vec![*mark],
            seen: None,
            text: format!("{name} {}", mark.name()),
        }));
        entries
    }

    /// What's with the player now, which the map doesn't remember.
    fn at_hand(&self) -> Vec<FindEntry> {
        let room = self.room.value();
        let here = room
            .room
            .as_ref()
            .map_or(Spot::Unseen, |room| Spot::Room(room.id));
        let ground = room.contents.items.iter().map(|item| {
            let name = match item.count {
                0 | 1 => item.name.clone(),
                count => format!("{} ×{count}", item.name),
            };
            FindEntry::held(name, here)
        });
        let carried = self.characters.value().carried();
        ground
            .chain(
                carried
                    .into_iter()
                    .map(|name| FindEntry::held(name, Spot::Carried)),
            )
            .collect()
    }

    fn row(&self, entry: &FindEntry, reach: Reach) -> FindRow {
        let place = match entry.spot {
            Spot::Room(id) => {
                self.map()
                    .room(id)
                    .map_or(FindPlace::Unseen, |room| FindPlace::Room {
                        id,
                        name: room.name.clone(),
                        area: self.area_name(room),
                    })
            }
            Spot::Carried => FindPlace::Carried,
            Spot::Unseen => FindPlace::Unseen,
        };
        FindRow {
            kind: entry.kind,
            name: entry.name.clone(),
            place,
            reach,
            marks: entry.marks.clone(),
        }
    }
}

/// What Find read from the stores, kept until their revisions change.
#[derive(Debug, Default)]
pub struct FindCache {
    world: Memo<(Revision, Revision), Vec<FindEntry>>,
    at_hand: Memo<(Revision, Revision), Vec<FindEntry>>,
    moves_away: Memo<(Revision, Option<RoomId>), HashMap<RoomId, u32>>,
}

impl FindCache {
    /// What `filter` lets through with every word of `query` in it, nearest
    /// first, then the rest by most recently seen. A blank query lists all.
    pub fn search(&mut self, stores: &FindStores, query: &str, filter: FindFilter) -> FindResults {
        let start = stores.walk_start();
        let world_key = (stores.rooms_changed, stores.mobs.revision());
        let hand_key = (stores.room.revision(), stores.characters.revision());
        let world = self.world.get(world_key, || stores.world());
        let at_hand = self.at_hand.get(hand_key, || stores.at_hand());
        let moves_away = self.moves_away.get((stores.rooms_changed, start), || {
            let map = stores.map();
            start.map_or_else(HashMap::new, |start| {
                distances(|id| map.room(id).map(|room| &**room), start)
            })
        });
        let reach = |spot: Spot| match (spot, start) {
            (Spot::Room(room), Some(start)) if room == start => Reach::Here,
            (Spot::Room(room), Some(_)) => moves_away
                .get(&room)
                .map_or(Reach::NoPath, |moves| Reach::Steps(*moves)),
            (Spot::Room(_), None) => Reach::PlayerLost,
            (Spot::Carried | Spot::Unseen, _) => Reach::NotARoom,
        };

        let query = query.to_lowercase();
        let words: Vec<Finder> = query.split_whitespace().map(Finder::new).collect();
        let found = |entry: &&FindEntry| {
            let text = entry.text.as_bytes();
            words.iter().all(|word| word.find(text).is_some())
        };
        let admitted = || {
            let every = world.iter().chain(at_hand);
            every.filter(|entry| filter.admits(entry.kind))
        };
        let mut matched: Vec<(&FindEntry, Reach)> = admitted()
            .filter(found)
            .map(|entry| (entry, reach(entry.spot)))
            .collect();
        matched.sort_by_key(|(entry, reach)| (reach.rank(), Reverse(entry.seen)));
        FindResults {
            rows: matched
                .iter()
                .take(MOST_RESULTS)
                .map(|(entry, reach)| stores.row(entry, *reach))
                .collect(),
            matched: matched.len(),
            known: admitted().count(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::{Direction, LiveMap, MapChange, MarkChange, PlainRules, Whereabouts};
    use crate::plugin::luau::Value;
    use crate::store::{Diff, Item, RoomContents};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    /// A road, a gate north of it, and a hut no link reaches.
    fn walked() -> Vec<Arc<MapRoom>> {
        let mut map = LiveMap::new(Box::new(PlainRules));
        let now = Timestamp(0);
        map.room_info(
            r#"{"num": 1, "name": "A road", "zone": "Fields", "exits": {"n": 2}}"#,
            now,
        );
        map.sent("north", now);
        map.room_info(
            r#"{"num": 2, "name": "A gate", "zone": "Fields", "exits": {"s": 1}}"#,
            now,
        );
        map.note_sightings(&["(Tall) A grey wolf is here.".to_owned()], Timestamp(5));
        map.room_info(
            r#"{"num": 9, "name": "A hut", "zone": "Woods", "exits": {}}"#,
            now,
        );
        let commit = map.commit(now).expect("rooms were mapped");
        commit.change.rooms
    }

    fn stores_at(room: RoomId) -> FindStores {
        stores_with(walked(), Some(room))
    }

    fn stores_with(rooms: Vec<Arc<MapRoom>>, at: Option<RoomId>) -> FindStores {
        let mut map = Versioned::<MapState>::default();
        map.write(&MapChange {
            revision: 1,
            rooms,
            whereabouts: Some(Whereabouts {
                room: at,
                ..Whereabouts::default()
            }),
            ..MapChange::default()
        });
        FindStores::of(
            map,
            Versioned::default(),
            Versioned::default(),
            Versioned::default(),
        )
    }

    fn search(stores: &FindStores, query: &str, filter: FindFilter) -> FindResults {
        FindCache::default().search(stores, query, filter)
    }

    fn names(results: &FindResults) -> Vec<&str> {
        results.rows.iter().map(|row| row.name.as_str()).collect()
    }

    #[test]
    fn every_word_must_match_in_any_case_and_order() {
        let stores = stores_at(1);
        let found = search(&stores, "WOLF grey", FindFilter::All);
        assert_eq!(names(&found), ["(Tall) A grey wolf is here."]);
        assert!(
            search(&stores, "grey bear", FindFilter::All)
                .rows
                .is_empty()
        );
    }

    #[test]
    fn a_blank_query_lists_what_the_filter_lets_through_nearest_first() {
        let stores = stores_at(1);
        let rooms = search(&stores, " ", FindFilter::Rooms);
        assert_eq!(names(&rooms), ["A road", "A gate", "A hut"]);
        let reaches: Vec<Reach> = rooms.rows.iter().map(|row| row.reach).collect();
        assert_eq!(reaches, [Reach::Here, Reach::Steps(1), Reach::NoPath]);
        assert_eq!((rooms.matched, rooms.known), (3, 3));
    }

    #[test]
    fn a_result_says_where_it_is() {
        let found = search(&stores_at(1), "wolf", FindFilter::Mobs);
        let FindPlace::Room { id, name, area } = &found.rows[0].place else {
            panic!("the wolf was seen in a room");
        };
        assert_eq!((*id, name.as_str(), area.as_str()), (2, "A gate", "Fields"));
        assert_eq!(found.rows[0].reach, Reach::Steps(1));
    }

    #[test]
    fn a_lost_player_or_a_guess_gives_no_distances() {
        let lost = stores_with(walked(), None);
        let found = search(&lost, "gate", FindFilter::Rooms);
        assert_eq!(found.rows[0].reach, Reach::PlayerLost);

        let mut unsure = stores_at(1);
        unsure.map.write(&MapChange {
            revision: 2,
            whereabouts: Some(Whereabouts {
                room: Some(1),
                unsure: true,
                ..Whereabouts::default()
            }),
            ..MapChange::default()
        });
        assert_eq!(unsure.walk_start(), None);
    }

    fn marked(stores: &mut FindStores, marks: &[(Mark, &str)]) {
        let changes: Vec<MarkChange> = marks
            .iter()
            .map(|(mark, name)| MarkChange {
                name: (*name).to_owned(),
                mark: *mark,
                set: true,
            })
            .collect();
        let marks = stores.mobs.value().with_changes(&changes);
        stores.mobs.write(&marks);
    }

    #[test]
    fn marks_show_on_sightings_and_find_them_by_their_name() {
        let mut stores = stores_at(1);
        marked(&mut stores, &[(Mark::Important, "a grey wolf")]);
        let found = search(&stores, "important", FindFilter::Mobs);
        assert_eq!(found.rows.len(), 1);
        assert_eq!(found.rows[0].marks, [Mark::Important]);
        assert!(matches!(found.rows[0].place, FindPlace::Room { .. }));
    }

    #[test]
    fn a_mark_never_seen_on_this_map_is_still_listed_and_says_so() {
        let mut stores = stores_at(1);
        marked(
            &mut stores,
            &[(Mark::Gear, "a pale king"), (Mark::Aggressive, "wolf")],
        );
        let found = search(&stores, "", FindFilter::Mobs);
        assert_eq!(
            names(&found),
            ["(Tall) A grey wolf is here.", "a pale king"]
        );
        assert_eq!(found.rows[1].place, FindPlace::Unseen);
        assert_eq!(found.rows[1].reach, Reach::NotARoom);
    }

    #[test]
    fn items_are_whats_at_hand_and_what_the_map_noted() {
        let mut rooms = walked();
        let hut = rooms.iter_mut().find(|room| room.id == 9).expect("mapped");
        let hut = Arc::make_mut(hut);
        hut.features = BTreeSet::from([feature::GEAR.to_owned(), feature::FOUNTAIN.to_owned()]);
        let road = Arc::clone(&rooms[0]);
        let mut stores = stores_with(rooms, Some(road.id));
        stores.room.write(&RoomState {
            room: Some(road),
            contents: RoomContents {
                items: vec![Item {
                    name: "a stone".to_owned(),
                    target: "stone".to_owned(),
                    count: 3,
                }],
                ..RoomContents::default()
            },
            ..RoomState::default()
        });
        stores.characters.write(&CharactersState(BTreeMap::from([
            ("inventory.count".to_owned(), Value::Number(1.)),
            ("inventory.1.name".to_owned(), Value::Text("a lamp".into())),
            ("inventory.2.name".to_owned(), Value::Text("stale".into())),
        ])));

        let found = search(&stores, "", FindFilter::Items);
        assert_eq!(
            names(&found),
            ["a stone ×3", "Important gear", "A fountain", "a lamp"]
        );
        assert_eq!(found.rows[0].reach, Reach::Here);
        assert_eq!(found.rows[3].place, FindPlace::Carried);
    }

    #[test]
    fn a_note_finds_its_room_under_the_rooms_filter() {
        let mut rooms = walked();
        Arc::make_mut(&mut rooms[1]).notes = "Ask the guard for the key".to_owned();
        let stores = stores_with(rooms, Some(1));
        let found = search(&stores, "guard", FindFilter::Rooms);
        assert_eq!(found.rows[0].kind, FindKind::Note);
        assert!(matches!(
            &found.rows[0].place,
            FindPlace::Room { name, .. } if name == "A gate"
        ));
    }

    #[test]
    fn the_list_stops_at_the_nearest_and_counts_the_rest() {
        let rooms: Vec<Arc<MapRoom>> = (0..MOST_RESULTS as i64 + 7)
            .map(|id| {
                let mut room = MapRoom::empty(id);
                room.name = format!("Room {id}");
                if id > 0 {
                    room.links.insert(Direction::West, id - 1);
                }
                Arc::new(room)
            })
            .collect();
        let found = search(&stores_with(rooms, None), "room", FindFilter::All);
        assert_eq!(found.rows.len(), MOST_RESULTS);
        assert_eq!(found.matched, MOST_RESULTS + 7);
    }

    /// Rebuilds show as a new allocation of the list.
    fn world_at(cache: &mut FindCache, stores: &FindStores) -> *const FindEntry {
        cache.search(stores, "", FindFilter::All);
        let key = (stores.rooms_changed, stores.mobs.revision());
        cache.world.get(key, || panic!("just cached")).as_ptr()
    }

    #[test]
    fn a_move_reuses_the_world_and_a_changed_room_or_mark_does_not() {
        let mut stores = stores_at(1);
        let mut cache = FindCache::default();
        let read = world_at(&mut cache, &stores);

        let moved = Batch {
            map: Some(Diff {
                revision: Revision(2),
                change: MapChange {
                    revision: 2,
                    whereabouts: Some(Whereabouts {
                        room: Some(2),
                        ..Whereabouts::default()
                    }),
                    ..MapChange::default()
                },
            }),
            ..Batch::default()
        };
        assert!(stores.apply(&moved));
        assert_eq!(world_at(&mut cache, &stores), read);
        let near = cache.search(&stores, "road", FindFilter::All);
        assert_eq!(near.rows[0].reach, Reach::Steps(1), "distances moved along");

        let renamed = stores.map().room(1).map(|room| {
            let mut room = (**room).clone();
            room.name = "A long road".to_owned();
            Arc::new(room)
        });
        assert!(stores.apply(&Batch {
            map: Some(Diff {
                revision: Revision(3),
                change: MapChange {
                    revision: 3,
                    rooms: renamed.into_iter().collect(),
                    ..MapChange::default()
                },
            }),
            ..Batch::default()
        }));
        let found = cache.search(&stores, "long", FindFilter::All);
        assert_eq!(names(&found), ["A long road"]);

        marked(&mut stores, &[(Mark::Gear, "a pale king")]);
        let found = cache.search(&stores, "pale", FindFilter::All);
        assert_eq!(names(&found), ["a pale king"]);
    }

    #[test]
    fn a_batch_of_nothing_find_reads_changes_nothing() {
        assert!(!stores_at(1).apply(&Batch::default()));
    }
}
