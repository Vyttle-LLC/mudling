//! The automap on synthetic GMCP and text. Room names and messages are
//! made up: no game's wording belongs in the core.

use super::*;
use crate::persist::{Batch, Database, Timestamp};
use std::collections::BTreeSet;

const FIELDS: &str = "The Fields";
const CAVES: &str = "The Caves";

/// Rules standing in for a game plugin, with made-up messages.
struct TestRules {
    identity: RoomIdentity,
}

impl MapRules for TestRules {
    fn identity(&self) -> RoomIdentity {
        self.identity
    }

    fn area_name(&self, zone: &str) -> AreaName {
        match zone.split_once(" | ") {
            Some((levels, name)) => AreaName {
                name: name.to_owned(),
                levels: Some(levels.to_owned()),
            },
            None => AreaName {
                name: zone.to_owned(),
                levels: None,
            },
        }
    }

    fn failed_move(&self, line: &str) -> bool {
        line == "You can't go that way."
    }

    fn fled(&self, line: &str) -> Option<Direction> {
        Direction::parse(line.strip_prefix("You run away ")?.trim_end_matches('.'))
    }

    fn dark_room(&self, info: &RoomInfo) -> bool {
        info.details.iter().any(|detail| detail == "dark")
    }

    fn darkness(&self, line: &str) -> bool {
        line == "Too dark to see."
    }

    fn maze(&self, line: &str) -> bool {
        line == "You are lost."
    }

    fn aggressor(&self, line: &str) -> Option<String> {
        line.strip_suffix(" leaps at you!").map(str::to_owned)
    }

    fn arrival(&self, line: &str) -> Option<String> {
        line.strip_suffix(" wanders in.").map(str::to_owned)
    }

    fn exits_line(&self, line: &str) -> bool {
        line.starts_with("Ways out:")
    }
}

fn text_map() -> LiveMap {
    LiveMap::new(Box::new(TestRules {
        identity: RoomIdentity::Text,
    }))
}

fn numbered_map() -> LiveMap {
    LiveMap::new(Box::new(TestRules {
        identity: RoomIdentity::Number,
    }))
}

/// A Room.Info without a room number; exits are (direction, state).
fn info(name: &str, zone: &str, environment: &str, exits: &[(&str, &str)]) -> String {
    let exits: Vec<String> = exits
        .iter()
        .map(|(direction, state)| format!("\"{direction}\": \"{state}\""))
        .collect();
    format!(
        r#"{{"name": "{name}", "zone": "{zone}", "environment": "{environment}", "exits": {{{}}}, "details": []}}"#,
        exits.join(", ")
    )
}

fn open(directions: &[&'static str]) -> Vec<(&'static str, &'static str)> {
    directions
        .iter()
        .map(|direction| (*direction, "O"))
        .collect()
}

/// A walk through a map, one second per room unless told otherwise.
struct Walk {
    map: LiveMap,
    clock: i64,
}

impl Walk {
    fn new(map: LiveMap) -> Walk {
        Walk { map, clock: 0 }
    }

    fn now(&self) -> Timestamp {
        Timestamp(self.clock * 1000)
    }

    fn tick(&mut self) {
        self.clock += 1;
    }

    /// Arrives without a move: the first room, a recall, a summons.
    fn arrive(&mut self, name: &str, exits: &[&'static str]) -> Arrived {
        self.arrive_in(name, FIELDS, exits, "")
    }

    fn arrive_in(
        &mut self,
        name: &str,
        zone: &str,
        exits: &[&'static str],
        description: &str,
    ) -> Arrived {
        self.tick();
        self.describe(name, description);
        let now = self.now();
        self.map
            .room_info(&info(name, zone, "field", &open(exits)), now)
            .expect("the Room.Info reads")
    }

    /// The room text a game prints before its Room.Info.
    fn describe(&mut self, name: &str, description: &str) {
        if description.is_empty() {
            return;
        }
        let now = self.now();
        for line in [name, "Ways out: somewhere", description] {
            self.map.line(line, now);
        }
    }

    fn go(&mut self, direction: &str, name: &str, exits: &[&'static str]) -> Arrived {
        let now = self.now();
        self.map.sent(direction, now);
        self.arrive(name, exits)
    }

    fn go_described(
        &mut self,
        direction: &str,
        name: &str,
        exits: &[&'static str],
        description: &str,
    ) -> Arrived {
        let now = self.now();
        self.map.sent(direction, now);
        self.arrive_in(name, FIELDS, exits, description)
    }

    fn current(&self) -> &MapRoom {
        self.map.current_room().expect("the player is somewhere")
    }

    fn position(&self) -> GridPoint {
        self.current().position
    }

    fn id_of(&self, name: &str) -> RoomId {
        self.map
            .rooms()
            .find(|room| room.name == name)
            .map(|room| room.id)
            .expect("the room is mapped")
    }

    fn count(&self) -> usize {
        self.map.room_count()
    }

    fn tangled(&self) -> usize {
        self.map.rooms().filter(|room| room.tangled).count()
    }
}

const CROSSROADS: [&str; 4] = ["n", "e", "s", "w"];

// MARK: Rooms and identity

#[test]
fn the_first_room_starts_the_map_and_announces_its_area() {
    let mut walk = Walk::new(text_map());
    let arrived = walk.arrive("A meadow", &["e"]);
    assert!(arrived.new_room);
    assert_eq!(arrived.entered_zone.as_deref(), Some(FIELDS));
    assert_eq!(walk.position(), GridPoint::new(0, 0, 0));
}

#[test]
fn moving_places_the_next_room_and_links_both_ways() {
    let mut walk = Walk::new(text_map());
    walk.arrive("A meadow", &["e"]);
    walk.go("east", "A barn", &["w"]);
    let barn = walk.current().clone();
    assert_eq!(barn.position, GridPoint::new(1, 0, 0));
    let meadow = barn.links.get_copied(Direction::West).expect("linked back");
    assert_eq!(
        walk.map
            .room(meadow)
            .unwrap()
            .links
            .get_copied(Direction::East),
        Some(barn.id)
    );
}

#[test]
fn walking_back_and_looking_reuse_rooms() {
    let mut walk = Walk::new(text_map());
    walk.arrive("A meadow", &["e"]);
    walk.go("e", "A barn", &["w"]);
    let arrived = walk.go("w", "A meadow", &["e"]);
    assert!(!arrived.new_room);
    walk.go("look", "A meadow", &["e"]);
    walk.arrive("A meadow", &["e"]);
    assert_eq!(walk.count(), 2);
}

#[test]
fn a_recall_jumps_to_a_known_room() {
    let mut walk = Walk::new(text_map());
    walk.arrive("A meadow", &["e"]);
    walk.go("e", "A barn", &["w", "d"]);
    walk.go("d", "A cellar", &["u"]);
    walk.arrive("A meadow", &["e"]);
    assert_eq!(walk.position(), GridPoint::new(0, 0, 0));
    assert_eq!(walk.count(), 3);
}

#[test]
fn a_room_number_is_the_rooms_identity() {
    let mut map = numbered_map();
    let now = Timestamp(0);
    let room = |number: i64, name: &str, exits: &str| {
        format!(
            r#"{{"num": {number}, "name": "{name}", "zone": "{FIELDS}", "exits": {{{exits}}}}}"#
        )
    };
    map.room_info(&room(10, "A road", r#""n": 11"#), now);
    map.sent("n", now);
    map.room_info(&room(11, "A road", r#""s": 10, "n": 12"#), now);
    map.sent("n", now);
    // Renamed by the game, but the same number: the same room.
    map.room_info(&room(12, "A road", r#""s": 11"#), now);
    map.sent("s", now);
    map.room_info(&room(11, "The road, rebuilt", r#""s": 10, "n": 12"#), now);

    assert_eq!(map.room_count(), 3);
    let middle = map.room(11).expect("kept by its number");
    assert_eq!(middle.position, GridPoint::new(0, 1, 0));
    assert_eq!(middle.links.get_copied(Direction::North), Some(12));
    assert_eq!(middle.links.get_copied(Direction::South), Some(10));
    assert!(map.rooms().all(|room| !room.tangled));
}

#[test]
fn a_numbered_room_seen_without_a_move_is_found_by_number() {
    let mut map = numbered_map();
    let now = Timestamp(0);
    map.room_info(
        r#"{"num": 5, "name": "Home", "zone": "Town", "exits": {"e": 6}}"#,
        now,
    );
    map.sent("e", now);
    map.room_info(
        r#"{"num": 6, "name": "Square", "zone": "Town", "exits": {"w": 5}}"#,
        now,
    );
    // Recalled home, with no move asked: the number says where.
    map.room_info(
        r#"{"num": 5, "name": "Home", "zone": "Town", "exits": {"e": 6}}"#,
        now,
    );
    assert_eq!(map.whereabouts().room, Some(5));
    assert_eq!(map.room_count(), 2);
}

#[test]
fn rooms_without_numbers_are_told_apart_by_description() {
    let mut walk = Walk::new(text_map());
    walk.arrive_in("In the grass", FIELDS, &CROSSROADS, "Tall grass sways.");
    let start = walk.current().id;
    walk.go_described("e", "In the grass", &CROSSROADS, "A rabbit hole.");
    // Summoned, with no move: the description names the room.
    walk.arrive_in("In the grass", FIELDS, &CROSSROADS, "Tall grass sways.");
    assert_eq!(walk.current().id, start);
    assert_eq!(walk.count(), 2);
}

#[test]
fn a_rooms_description_comes_from_the_text_before_its_room_info() {
    let mut walk = Walk::new(text_map());
    walk.arrive_in("A pond", FIELDS, &["n"], "Lilies float here.");
    assert_eq!(walk.current().description, "Lilies float here.");
}

#[test]
fn links_stay_when_a_room_later_lists_fewer_exits() {
    let mut walk = Walk::new(text_map());
    walk.arrive("A hall", &["e", "w"]);
    walk.go("e", "A study", &["w"]);
    walk.go("w", "A hall", &["e", "w"]);
    walk.go("w", "A yard", &["e"]);
    // Back in: the way east is gone from the listing now (shut and hidden).
    walk.go("e", "A hall", &["w"]);
    assert_eq!(walk.count(), 3);
    let hall = walk.current();
    assert!(!hall.exits.contains(Direction::East));
    assert_eq!(
        hall.links.get_copied(Direction::East),
        Some(walk.id_of("A study"))
    );
}

#[test]
fn a_revisit_with_different_exits_is_flagged() {
    let mut walk = Walk::new(text_map());
    walk.arrive("A hall", &["e"]);
    walk.go("e", "A study", &["w"]);
    assert!(walk.go("w", "A hall", &["e", "n"]).exits_changed);
}

#[test]
fn crossing_into_another_area_is_announced() {
    let mut walk = Walk::new(text_map());
    walk.arrive("A hill", &["d"]);
    let now = walk.now();
    walk.map.sent("d", now);
    let arrived = walk.arrive_in("A hole", CAVES, &["u"], "");
    assert_eq!(arrived.entered_zone.as_deref(), Some(CAVES));
    assert_eq!(walk.position(), GridPoint::new(0, 0, -1));
}

#[test]
fn closed_doors_are_remembered_as_doorways() {
    let mut map = text_map();
    map.room_info(
        &info("A gatehouse", FIELDS, "field", &[("n", "C"), ("s", "O")]),
        Timestamp(0),
    );
    let room = map.current_room().unwrap();
    assert!(room.exits.get(Direction::North).unwrap().closed);
    assert!(room.doorways.contains(Direction::North));
    assert!(!room.doorways.contains(Direction::South));
}

#[test]
fn control_codes_inside_json_strings_still_read() {
    let mut map = text_map();
    let json = "{\"name\": \"A \u{1b}[31mred\u{1b}[0m room\", \"zone\": \"Z\", \"exits\": {}}";
    assert!(map.room_info(json, Timestamp(0)).is_some());
    assert!(map.current_room().unwrap().name.contains("red"));
}

// MARK: Moves that fail, wait or flee

#[test]
fn a_failed_move_does_not_drift_the_map() {
    let mut walk = Walk::new(text_map());
    walk.arrive("A meadow", &["n", "e"]);
    let now = walk.now();
    walk.map.sent("n", now);
    walk.map.line("You can't go that way.", now);
    walk.arrive("A meadow", &["n", "e"]);
    assert_eq!(walk.count(), 1);
}

#[test]
fn a_refused_move_drops_only_itself() {
    let mut walk = Walk::new(text_map());
    walk.arrive("A meadow", &["n", "e"]);
    let now = walk.now();
    walk.map.sent("n", now);
    walk.map.sent("e", now);
    walk.map.gmcp("Room.WrongDir", "\"n\"", now);
    walk.arrive("A barn", &["w"]);
    assert_eq!(walk.position(), GridPoint::new(1, 0, 0));
}

#[test]
fn a_move_the_room_could_not_make_is_ignored() {
    let mut walk = Walk::new(text_map());
    walk.arrive("A dead end", &["e"]);
    walk.go("w", "A dead end", &["e"]);
    assert_eq!(walk.count(), 1);
}

#[test]
fn stale_moves_expire_but_slow_answers_count() {
    let mut walk = Walk::new(text_map());
    walk.arrive("A meadow", &["e"]);
    let now = walk.now();
    walk.map.sent("e", now);
    walk.clock += 8;
    walk.arrive("A barn", &["w"]);
    assert_eq!(walk.position(), GridPoint::new(1, 0, 0));

    let now = walk.now();
    walk.map.sent("w", now);
    walk.clock += 30;
    walk.arrive("A barn", &["w"]);
    assert_eq!(walk.count(), 2);
}

#[test]
fn a_forty_move_speedwalk_answered_once_a_second_stays_connected() {
    let mut walk = Walk::new(text_map());
    walk.arrive("Road 0", &["n"]);
    let sent = walk.now();
    for _ in 0..40 {
        walk.map.sent("n", sent);
    }
    for step in 1..=40 {
        walk.arrive(&format!("Road {step}"), &["n", "s"]);
    }
    assert_eq!(walk.position(), GridPoint::new(0, 40, 0));
    assert!(walk.map.rooms().all(|room| room.position.x == 0));
    for step in 1..=40 {
        let road = walk.map.room(walk.id_of(&format!("Road {step}"))).unwrap();
        assert!(
            road.links.get_copied(Direction::South).is_some(),
            "Road {step} is linked"
        );
    }
}

#[test]
fn queued_moves_map_in_order() {
    let mut walk = Walk::new(text_map());
    walk.arrive("A meadow", &["e"]);
    let now = walk.now();
    walk.map.sent("e", now);
    walk.map.sent("d", now);
    walk.arrive("A barn", &["w", "d"]);
    walk.arrive("A cellar", &["u"]);
    assert_eq!(walk.position(), GridPoint::new(1, 0, -1));
}

#[test]
fn a_flee_the_game_names_is_a_move() {
    let mut walk = Walk::new(text_map());
    walk.arrive("A lair", &["s"]);
    let now = walk.now();
    walk.map.line("You run away south.", now);
    walk.arrive("A ledge", &["n"]);
    assert_eq!(walk.position(), GridPoint::new(0, -1, 0));
    assert_eq!(
        walk.current().links.get_copied(Direction::North),
        Some(walk.id_of("A lair"))
    );
}

#[test]
fn other_commands_do_not_disturb_pending_moves() {
    let mut walk = Walk::new(text_map());
    walk.arrive("A meadow", &["e"]);
    let now = walk.now();
    walk.map.sent("e", now);
    walk.map.sent("score", now);
    walk.arrive("A barn", &["w"]);
    assert_eq!(walk.position(), GridPoint::new(1, 0, 0));
}

// MARK: Dark rooms

fn dark_info(exits: &[(&str, &str)]) -> String {
    let exits: Vec<String> = exits
        .iter()
        .map(|(d, s)| format!("\"{d}\": \"{s}\""))
        .collect();
    format!(
        r#"{{"name": "Darkness", "zone": "{FIELDS}", "environment": "", "exits": {{{}}}, "details": ["dark"]}}"#,
        exits.join(", ")
    )
}

#[test]
fn a_dark_room_is_never_mapped_and_moves_are_followed() {
    let mut walk = Walk::new(text_map());
    walk.arrive("A cave mouth", &["e"]);
    let now = walk.now();
    walk.map.sent("e", now);
    let arrived = walk
        .map
        .room_info(&dark_info(&open(&["w", "e"])), now)
        .unwrap();
    assert!(arrived.entered_dark);
    walk.map.sent("e", now);
    walk.map.room_info(&dark_info(&open(&["w", "e"])), now);
    assert_eq!(walk.count(), 1);
    let whereabouts = walk.map.whereabouts();
    assert!(whereabouts.dark);
    assert_eq!(whereabouts.dark_position, Some(GridPoint::new(2, 0, 0)));

    // Light again: placed where the moves lead, linked to nothing unseen.
    walk.go("e", "A lit grotto", &["w"]);
    assert_eq!(walk.position(), GridPoint::new(3, 0, 0));
    assert!(!walk.map.whereabouts().dark);
    assert_eq!(walk.count(), 2);
}

#[test]
fn a_dark_room_follows_known_links() {
    let mut walk = Walk::new(text_map());
    walk.arrive("A cave mouth", &["e"]);
    walk.go("e", "A tunnel", &["w"]);
    walk.go("w", "A cave mouth", &["e"]);
    // The torch goes out: the same tunnel, but too dark to see.
    let now = walk.now();
    walk.map.line("Too dark to see.", now);
    walk.map.sent("e", now);
    walk.map
        .room_info(&info("A tunnel", FIELDS, "", &open(&["w"])), now);
    assert_eq!(walk.map.whereabouts().room, Some(walk.id_of("A tunnel")));
    assert!(walk.map.whereabouts().dark);
    assert_eq!(walk.count(), 2);
}

#[test]
fn sightings_in_the_dark_are_not_noted() {
    let mut walk = Walk::new(text_map());
    walk.arrive("A cave mouth", &["e"]);
    let now = walk.now();
    walk.map.sent("e", now);
    walk.map.room_info(&dark_info(&open(&["w"])), now);
    walk.map.note_sightings(&["A bat".to_owned()], now);
    assert!(walk.map.rooms().all(|room| room.sightings.is_empty()));
}

// MARK: Mazes and tangles

fn woods(walk: &mut Walk, direction: &str) -> Arrived {
    walk.go_described(
        direction,
        "Lost in the woods",
        &CROSSROADS,
        "Trees in every direction.",
    )
}

/// East into the woods three times, then west lands in the clearing again:
/// the woods' paths don't add up.
fn lost_in_the_woods() -> Walk {
    let mut walk = Walk::new(text_map());
    walk.arrive("A clearing", &["e", "n"]);
    for _ in 0..3 {
        woods(&mut walk, "e");
    }
    walk.go("w", "A clearing", &["e", "n"]);
    walk
}

#[test]
fn look_alike_crossroads_leading_crookedly_into_each_other_are_one_maze() {
    let mut walk = lost_in_the_woods();
    let mazes: Vec<&MapRoom> = walk.map.rooms().filter(|room| room.maze).collect();
    assert_eq!(mazes.len(), 1, "one maze square");
    let maze = mazes[0].clone();
    assert_eq!(walk.count(), 2);
    let clearing = walk.current().clone();
    assert_eq!(clearing.links.get_copied(Direction::East), Some(maze.id));
    assert_eq!(
        walk.tangled(),
        0,
        "the rooms around a maze were only tangled by it"
    );

    // Moving about inside it stays on the one square.
    woods(&mut walk, "e");
    woods(&mut walk, "e");
    woods(&mut walk, "s");
    assert_eq!(walk.current().id, maze.id);
    assert_eq!(walk.count(), 2);
    // Leaving it lands on the side the last move faced, a square out.
    walk.go("n", "A misty hill", &["s"]);
    assert_eq!(walk.position(), maze.position + GridPoint::new(0, 2, 0));
    assert_eq!(
        walk.map.route_to(clearing.id),
        None,
        "no route crosses a maze"
    );
}

#[test]
fn hand_laid_crossroads_that_lead_crookedly_collapse() {
    let mut layout = layout::Layout::new(1);
    // East twice, then north leads back to the first: that can't be a grid.
    let links = [
        (1, Direction::East, 2),
        (2, Direction::East, 3),
        (3, Direction::North, 1),
    ];
    for (id, x) in [(1, 0), (2, 1), (3, 2)] {
        layout.insert(crossroads(id, x));
    }
    for (from, direction, to) in links {
        layout.update(from, |room| room.links.insert(direction, to));
    }
    layout.current = Some(3);
    assert!(layout.collapse_maze(3));
    assert_eq!(layout.len(), 1);
    assert!(layout.current_room().unwrap().maze);
}

fn crossroads(id: RoomId, x: i64) -> MapRoom {
    MapRoom {
        name: "A crossroads".into(),
        zone: "z".into(),
        description: "Roads meet.".into(),
        exits: CROSSROADS
            .iter()
            .map(|d| {
                (
                    Direction::parse(d).unwrap(),
                    ExitState {
                        state: "O".into(),
                        closed: false,
                    },
                )
            })
            .collect(),
        position: GridPoint::new(x, 0, 0),
        ..MapRoom::empty(id)
    }
}

#[test]
fn copies_of_one_crossroads_that_never_meet_are_not_a_maze() {
    let mut layout = layout::Layout::new(1);
    layout.insert(MapRoom {
        tangled: true,
        ..crossroads(1, 0)
    });
    layout.insert(crossroads(2, 10));
    layout.insert(crossroads(3, 20));
    assert!(!layout.collapse_maze(1));
    assert_eq!(layout.len(), 3);
}

#[test]
fn a_maze_message_makes_the_room_one_square() {
    let mut walk = Walk::new(text_map());
    walk.arrive("A hedge gate", &["n"]);
    let now = walk.now();
    walk.map.line("You are lost.", now);
    walk.go("n", "Among hedges", &CROSSROADS);
    assert!(walk.current().maze);
    walk.go("e", "Among hedges", &CROSSROADS);
    walk.go("n", "Among hedges", &CROSSROADS);
    assert_eq!(walk.count(), 2);
}

#[test]
fn a_maze_message_while_unsure_survives_the_next_room() {
    let mut walk = mapped_meadow();
    walk.arrive("The temple", &["e"]);
    let now = walk.now();
    walk.map.line("You are lost.", now);
    walk.go("e", "In shade", &CROSSROADS);
    assert!(walk.map.whereabouts().unsure);
    walk.go("n", "A field", &["w", "s", "e"]);
    assert_eq!(walk.map.rooms().filter(|room| room.maze).count(), 1);
}

#[test]
fn a_way_back_that_leads_elsewhere_is_tangled() {
    let mut walk = Walk::new(text_map());
    walk.arrive("A twisty passage", &["e", "w"]);
    walk.go("e", "A dim passage", &["e", "w"]);
    assert!(!walk.current().tangled);
    let arrived = walk.go("w", "A narrow passage", &["e"]);
    assert!(arrived.entered_tangle);
    assert!(walk.tangled() >= 2);
}

#[test]
fn a_crowded_square_is_a_stretch_not_a_maze() {
    let mut walk = Walk::new(text_map());
    walk.arrive("A", &["e", "n"]);
    walk.go("e", "B", &["w"]);
    walk.go("w", "A", &["e", "n"]);
    walk.go("n", "C", &["s", "e"]);
    walk.go("e", "D", &["w", "s"]);
    // South of D is where B is drawn, but E is a different room further on.
    walk.go("s", "E", &["n"]);
    assert_eq!(walk.position(), GridPoint::new(1, -1, 0));
    assert_eq!(walk.tangled(), 0);
}

// MARK: Look-alikes and changing its mind

/// Two identical shaded rooms that only their surroundings tell apart:
///
/// ```text
///     Pond ── Path ── Field ── Barn
///      │                │
///     Shade₁          Shade₂
/// ```
fn mapped_meadow() -> Walk {
    let mut walk = Walk::new(text_map());
    walk.arrive("In shade", &CROSSROADS);
    walk.go("n", "A pond", &["s", "e"]);
    walk.go("e", "A path", &["w", "e"]);
    walk.go("e", "A field", &["w", "s", "e"]);
    walk.go("s", "In shade", &CROSSROADS);
    walk.go("n", "A field", &["w", "s", "e"]);
    walk.go("e", "A barn", &["w"]);
    walk
}

fn shade_south_of(walk: &Walk, name: &str) -> Option<RoomId> {
    walk.map
        .room(walk.id_of(name))?
        .links
        .get_copied(Direction::South)
}

#[test]
fn identical_rooms_map_apart() {
    let walk = mapped_meadow();
    assert_eq!(walk.count(), 6);
    assert!(!walk.map.whereabouts().unsure);
    assert_ne!(
        shade_south_of(&walk, "A pond"),
        shade_south_of(&walk, "A field")
    );
    assert_eq!(walk.tangled(), 0);
}

#[test]
fn lost_among_look_alikes_the_next_room_decides() {
    let mut walk = mapped_meadow();
    walk.arrive("In shade", &CROSSROADS);
    assert!(walk.map.whereabouts().unsure);
    let arrived = walk.go("n", "A field", &["w", "s", "e"]);
    assert!(!walk.map.whereabouts().unsure);
    assert_eq!(walk.current().id, walk.id_of("A field"));
    assert_eq!(walk.count(), 6);
    assert_eq!(arrived.rethought_rooms, 1);
}

#[test]
fn a_guess_is_rethought_two_rooms_later_and_written_once_sure() {
    let mut walk = mapped_meadow();
    walk.map.commit(walk.now());
    walk.arrive("The temple", &["e"]);
    walk.go("e", "In shade", &CROSSROADS);
    assert!(walk.map.whereabouts().unsure);
    assert_eq!(walk.count(), 8);
    let current = walk.current().id;
    assert!(walk.map.unconfirmed_rooms().contains(&current));

    let rethink = walk.go("n", "A field", &["w", "s", "e"]);
    assert_eq!(rethink.rethought_rooms, 1);
    assert!(walk.map.whereabouts().unsure);

    walk.go("e", "A barn", &["w"]);
    assert!(!walk.map.whereabouts().unsure);
    assert_eq!(walk.count(), 7, "no third shade, field or barn");
    let temple = walk.map.room(walk.id_of("The temple")).unwrap().clone();
    assert_eq!(
        temple.links.get_copied(Direction::East),
        shade_south_of(&walk, "A field")
    );
    assert!(walk.map.unconfirmed_rooms().is_empty());

    // The rows written are the temple and the links that changed, not the
    // rooms guessed and given up.
    let commit = walk.map.commit(walk.now()).unwrap();
    let ids: BTreeSet<RoomId> = commit.change.rooms.iter().map(|room| room.id).collect();
    assert!(ids.contains(&temple.id));
    assert!(
        ids.len() <= 3,
        "only the temple, the shade it leads to and the barn: {ids:?}"
    );
}

#[test]
fn mobs_seen_while_unsure_stay_with_the_right_room() {
    let mut walk = mapped_meadow();
    walk.arrive("The temple", &["e"]);
    walk.go("e", "In shade", &CROSSROADS);
    let now = walk.now();
    walk.map.note_sightings(&["A rabbit".to_owned()], now);
    walk.go("n", "A field", &["w", "s", "e"]);
    walk.go("e", "A barn", &["w"]);
    let shade = shade_south_of(&walk, "A field").unwrap();
    assert!(
        walk.map
            .room(shade)
            .unwrap()
            .sightings
            .contains_key("A rabbit")
    );
}

#[test]
fn editing_the_map_settles_on_what_it_shows() {
    let mut walk = mapped_meadow();
    walk.arrive("In shade", &CROSSROADS);
    assert!(walk.map.whereabouts().unsure);
    let current = walk.current().id;
    walk.map.set_notes(current, "shady");
    assert!(!walk.map.whereabouts().unsure);
}

#[test]
fn an_off_grid_return_reuses_the_described_room() {
    let mut walk = Walk::new(text_map());
    walk.arrive_in("In shade", FIELDS, &CROSSROADS, "The trunk is north.");
    let start = walk.current().id;
    for (direction, description) in [
        ("n", "Shade covers the grass."),
        ("n", "A breeze in the leaves."),
        ("e", "Roots break the soil."),
        ("s", "Thick green grass."),
        ("w", "The trunk is north."),
    ] {
        walk.go_described(direction, "In shade", &CROSSROADS, description);
    }
    assert_eq!(walk.count(), 5);
    assert_eq!(walk.current().id, start);
    assert_eq!(walk.tangled(), 0);
}

// MARK: Buildings and straightening

#[test]
fn a_building_walked_through_between_neighbors_is_lifted_out() {
    let mut walk = Walk::new(text_map());
    let described = |walk: &mut Walk, direction: &str, name: &str, exits: &[&'static str]| {
        walk.go_described(direction, name, exits, name);
    };
    walk.arrive_in(
        "West of the tower",
        FIELDS,
        &["e", "n", "s"],
        "West of the tower",
    );
    described(&mut walk, "n", "Northwest", &["s", "e"]);
    described(&mut walk, "e", "Northeast", &["s", "w"]);
    described(&mut walk, "s", "East of the tower", &["w", "n", "s"]);
    described(&mut walk, "s", "Southeast", &["n", "w"]);
    described(&mut walk, "w", "Southwest", &["n", "e"]);
    described(&mut walk, "n", "West of the tower", &["e", "n", "s"]);
    let now = walk.now();
    walk.map.sent("e", now);
    walk.tick();
    walk.describe("Inside the tower", "Inside the tower");
    let now = walk.now();
    walk.map.room_info(
        &info(
            "Inside the tower",
            FIELDS,
            "inside",
            &open(&["e", "w", "u"]),
        ),
        now,
    );
    described(&mut walk, "e", "East of the tower", &["w", "n", "s"]);

    let tower = walk
        .map
        .room(walk.id_of("Inside the tower"))
        .unwrap()
        .clone();
    assert_eq!(tower.building, Some(tower.id));
    let east = walk.map.room(walk.id_of("East of the tower")).unwrap();
    assert_eq!(
        east.position,
        GridPoint::new(1, 0, 0),
        "right beside the west side"
    );
    assert_eq!(walk.tangled(), 0);

    walk.map.commit(walk.now());
    let derived = walk.map.derived().clone();
    let building = derived
        .buildings
        .iter()
        .find(|building| building.id == tower.id)
        .expect("the tower is a building");
    assert_eq!(building.footprint, None, "bigger inside than out");
    let ways: BTreeSet<Direction> = building.doors.iter().map(|door| door.direction).collect();
    assert_eq!(ways, BTreeSet::from([Direction::East, Direction::West]));
}

#[test]
fn indoor_rooms_that_fit_are_ordinary_squares() {
    let mut walk = Walk::new(text_map());
    walk.arrive("A field", &["s"]);
    let now = walk.now();
    walk.map.sent("s", now);
    walk.map
        .room_info(&info("A wagon", FIELDS, "inside", &open(&["n", "e"])), now);
    walk.go("e", "Another field", &["w"]);
    walk.map.commit(walk.now());
    assert!(walk.map.derived().buildings.is_empty());
    assert_eq!(
        walk.map.room(walk.id_of("A wagon")).unwrap().position,
        GridPoint::new(0, -1, 0)
    );
}

#[test]
fn each_area_has_its_own_grid() {
    let mut walk = Walk::new(text_map());
    walk.arrive("Trail", &["e", "s"]);
    walk.go("s", "Below", &["n"]);
    walk.go("n", "Trail", &["e", "s"]);
    walk.go("e", "Ridge", &["w", "s"]);
    walk.go("s", "Cliff", &["n", "w"]);
    let now = walk.now();
    walk.map.sent("w", now);
    walk.arrive_in("Hole", CAVES, &["e"], "");
    assert_eq!(walk.position(), GridPoint::new(0, -1, 0));
    assert_eq!(walk.tangled(), 0);
}

// MARK: Mob memory

#[test]
fn an_attacker_marks_its_whole_kind_and_the_room() {
    let mut walk = Walk::new(text_map());
    walk.arrive("A den", &["n"]);
    let now = walk.now();
    walk.map.line("The grey wolf leaps at you!", now);
    assert!(
        walk.map
            .mobs()
            .is_aggressive("A big wolf is here, growling.")
    );
    assert!(!walk.map.mobs().is_aggressive("A rabbit is here."));
    assert!(walk.current().features.contains(feature::AGGRESSIVE_MOB));

    let commit = walk.map.commit(walk.now()).unwrap();
    let rows = written(&commit.batch);
    assert!(rows.contains(r#"Upsert(MobMark(MobMark { name: "wolf", mark: "aggressive""#));
    assert!(
        rows.contains(r#"Upsert(RoomFeature(RoomFeature { room_id: 1, feature: "aggressive-mob""#)
    );
}

#[test]
fn unmarking_undoes_a_learned_guess() {
    let mut mobs = MobMemory::default();
    mobs.learn_aggressor("The old goat");
    let title = "A goat chews on a boot.";
    assert!(mobs.is_aggressive(title));
    mobs.set_aggressive(title, false);
    assert!(!mobs.is_aggressive(title));
}

#[test]
fn a_gear_mob_is_that_mob_whatever_its_doing() {
    let mut mobs = MobMemory::default();
    mobs.set_gear("(Glowing) A cave troll is here, snoring.", true);
    assert!(mobs.is_gear("A cave troll is here, fighting you!"));
    assert!(!mobs.is_gear("A river troll is here."));
    mobs.set_gear("A cave troll is resting here.", false);
    assert!(!mobs.is_gear("A cave troll is here."));
}

#[test]
fn an_important_person_is_them_whatever_theyre_doing() {
    let mut mobs = MobMemory::default();
    mobs.set_important("Mara the healer sits here, humming.", true);
    assert!(mobs.is_important("Mara the healer is sleeping here."));
    assert!(!mobs.is_important("A villager is here."));
    mobs.set_important("Mara the healer is resting here.", false);
    assert!(mobs.marks(Mark::Important).is_empty());
}

#[test]
fn arrivals_are_sightings_in_the_players_room() {
    let mut walk = Walk::new(text_map());
    walk.arrive("A road", &["n"]);
    let now = walk.now();
    walk.map.line("A tinker wanders in.", now);
    assert_eq!(walk.current().sightings.get("A tinker"), Some(&now));
}

// MARK: Walk-to

#[test]
fn a_route_follows_explored_links_and_opens_closed_doors() {
    let mut map = text_map();
    let now = Timestamp(0);
    map.room_info(&info("A tunnel", CAVES, "", &open(&["n", "s"])), now);
    map.sent("n", now);
    map.room_info(&info("A guardpost", CAVES, "", &open(&["s", "e"])), now);
    map.sent("e", now);
    map.room_info(&info("An armory", CAVES, "", &open(&["w"])), now);
    map.sent("w", now);
    map.room_info(&info("A guardpost", CAVES, "", &open(&["s", "e"])), now);
    map.sent("s", now);
    // Someone shut the door north behind us.
    map.room_info(&info("A tunnel", CAVES, "", &[("n", "C"), ("s", "O")]), now);

    let armory = map
        .rooms()
        .find(|room| room.name == "An armory")
        .unwrap()
        .id;
    let route = map.route_to(armory).expect("a known path");
    assert_eq!(
        route,
        vec![
            WalkStep {
                direction: Direction::North,
                open_first: true
            },
            WalkStep {
                direction: Direction::East,
                open_first: false
            },
        ]
    );
    let here = map.whereabouts().room.unwrap();
    assert_eq!(map.route_to(here), Some(vec![]));
}

#[test]
fn no_route_crosses_a_maze_or_reaches_an_island() {
    let mut walk = Walk::new(text_map());
    walk.arrive("A meadow", &["e"]);
    walk.arrive("Far away", &["n"]);
    let meadow = walk.id_of("A meadow");
    assert_eq!(walk.map.route_to(meadow), None);
}

#[test]
fn routes_on_a_walked_grid_are_shortest() {
    const SIDE: i64 = 20;
    let mut map = text_map();
    let now = Timestamp(0);
    let visit = |map: &mut LiveMap, direction: &str, x: i64, y: i64| {
        map.sent(direction, now);
        map.room_info(
            &info(&format!("Plain {x} {y}"), FIELDS, "", &open(&CROSSROADS)),
            now,
        );
    };
    // Every row, then every column, in a snake: each square's paths walked.
    map.room_info(&info("Plain 0 0", FIELDS, "", &open(&CROSSROADS)), now);
    for y in 0..SIDE {
        let east = y % 2 == 0;
        for step in 1..SIDE {
            let x = if east { step } else { SIDE - 1 - step };
            visit(&mut map, if east { "e" } else { "w" }, x, y);
        }
        if y + 1 < SIDE {
            visit(&mut map, "n", if east { SIDE - 1 } else { 0 }, y + 1);
        }
    }
    // Ending at (0, top): walk the columns down and up.
    for x in 0..SIDE {
        let south = x % 2 == 0;
        for step in 1..SIDE {
            let y = if south { SIDE - 1 - step } else { step };
            visit(&mut map, if south { "s" } else { "n" }, x, y);
        }
        if x + 1 < SIDE {
            visit(&mut map, "e", x + 1, if south { 0 } else { SIDE - 1 });
        }
    }
    assert_eq!(map.room_count(), (SIDE * SIDE) as usize);
    assert!(map.rooms().all(|room| !room.tangled));
    // From the far corner (top right) to (0, 0): the grid distance.
    let corner = map
        .rooms()
        .find(|room| room.name == "Plain 0 0")
        .unwrap()
        .id;
    let route = map.route_to(corner).unwrap();
    assert_eq!(route.len() as i64, 2 * (SIDE - 1));
}

// MARK: Changes, rows and revisions

/// A batch's changes, one per line, as the writer would apply them.
/// (Batches keep their changes to the persistence module; their debug form
/// shows them.)
fn written(batch: &Batch) -> String {
    format!("{batch:?}")
        .replace(", Upsert(", "\nUpsert(")
        .replace(", Delete(", "\nDelete(")
}

fn count(rows: &str, prefix: &str) -> usize {
    rows.lines()
        .filter(|line| {
            line.trim_start_matches("Batch { changes: [")
                .starts_with(prefix)
        })
        .count()
}

#[test]
fn a_move_writes_only_the_rows_it_changed() {
    let mut walk = Walk::new(text_map());
    walk.arrive("A meadow", &["e"]);
    let first = walk.map.commit(walk.now()).unwrap();
    assert_eq!(first.change.rooms.len(), 1);

    walk.go("e", "A barn", &["w"]);
    let commit = walk.map.commit(walk.now()).unwrap();
    let rows = written(&commit.batch);
    assert_eq!(
        count(&rows, "Upsert(Room("),
        1,
        "the meadow's own row is unchanged:\n{rows}"
    );
    assert!(rows.contains("Upsert(Room(Room { id: 2, name: \"A barn\""));
    assert_eq!(count(&rows, "Upsert(Link("), 2, "the link each way");
    assert_eq!(count(&rows, "Upsert(Exit("), 1, "the barn's exit");
    assert_eq!(commit.change.rooms.len(), 2);
}

#[test]
fn the_revision_bumps_only_on_a_real_change() {
    let mut walk = Walk::new(text_map());
    walk.arrive("A meadow", &["e"]);
    let first = walk.map.commit(walk.now()).unwrap().change.revision;
    assert!(walk.map.commit(walk.now()).is_none());
    // A look that shows the room unchanged changes nothing.
    walk.go("look", "A meadow", &["e"]);
    assert!(walk.map.commit(walk.now()).is_none());
    assert_eq!(walk.map.revision(), first);
    walk.go("e", "A barn", &["w"]);
    assert_eq!(
        walk.map.commit(walk.now()).unwrap().change.revision,
        first + 1
    );
}

#[test]
fn derived_data_is_computed_once_per_revision() {
    let mut walk = Walk::new(text_map());
    let zone = "1-5 | The Fields";
    walk.arrive_in("A meadow", zone, &["e"], "");
    let now = walk.now();
    walk.map.sent("e", now);
    walk.arrive_in("A barn", zone, &["w"], "");
    walk.map.commit(walk.now());
    let derived = walk.map.derived().clone();
    assert_eq!(derived.areas.len(), 1);
    assert_eq!(derived.areas[0].rooms, 2);
    assert_eq!(derived.areas[0].name.name, "The Fields");
    assert_eq!(derived.areas[0].name.levels.as_deref(), Some("1-5"));
    assert_eq!(walk.map.derived().revision, derived.revision);
}

#[test]
fn forgetting_rooms_removes_them_and_links_to_them() {
    let mut walk = Walk::new(text_map());
    walk.arrive("A meadow", &["e"]);
    walk.go("e", "A barn", &["w", "d"]);
    walk.go("d", "A cellar", &["u"]);
    walk.map.commit(walk.now());
    let barn = walk.id_of("A barn");
    walk.map.forget(&[barn]);
    assert_eq!(walk.count(), 2);
    assert!(
        walk.map
            .rooms()
            .all(|room| room.links.values().all(|to| *to != barn))
    );
    let commit = walk.map.commit(walk.now()).unwrap();
    assert_eq!(commit.change.removed, vec![barn]);
}

#[test]
fn forgetting_your_room_rebuilds_from_the_same_spot() {
    let mut walk = Walk::new(text_map());
    walk.arrive("A meadow", &["e"]);
    walk.go("e", "A barn", &["w"]);
    let spot = walk.position();
    let barn = walk.current().id;
    walk.map.forget(&[barn]);
    walk.arrive("A barn", &["w"]);
    assert_eq!(walk.position(), spot);
}

#[test]
fn im_here_removes_the_stray_room_the_map_invented() {
    let mut walk = Walk::new(text_map());
    walk.arrive("A meadow", &["e"]);
    walk.go("e", "A barn", &["w"]);
    walk.arrive("A field of flowers", &["n"]);
    assert_eq!(walk.count(), 3);
    let meadow = walk.id_of("A meadow");
    walk.map.relocate(meadow);
    assert_eq!(walk.count(), 2);
    assert_eq!(walk.map.whereabouts().room, Some(meadow));
}

#[test]
fn the_map_survives_the_database_and_carries_on() {
    let folder = tempfile::tempdir().unwrap();
    let database = Database::open(folder.path(), "test.example").unwrap();
    let writer = database.writer().unwrap();
    let mut walk = Walk::new(text_map());
    walk.arrive_in("A meadow", FIELDS, &["e"], "Grass all around.");
    walk.go("e", "A barn", &["w", "n"]);
    let now = walk.now();
    walk.map.note_sightings(&["A cow".to_owned()], now);
    let barn = walk.current().id;
    walk.map.set_notes(barn, "Milk here");
    walk.map.set_feature(barn, "shop", true);
    walk.map.mobs_mut().set_gear("A cow is here.", true);
    for _ in 0..2 {
        if let Some(commit) = walk.map.commit(walk.now()) {
            writer.submit(commit.batch).unwrap();
        }
    }
    writer
        .submit(walk.map.save_version("named", "Barn found", walk.now()))
        .unwrap();
    writer.flush().unwrap();

    let reader = database.reader().unwrap();
    let loaded = LiveMap::load(
        Box::new(TestRules {
            identity: RoomIdentity::Text,
        }),
        &reader.map().unwrap(),
        &reader.mob_marks().unwrap(),
        &reader.meta().unwrap(),
    );
    let mut rooms: Vec<MapRoom> = walk.map.rooms().cloned().collect();
    let mut again: Vec<MapRoom> = loaded.rooms().cloned().collect();
    rooms.sort_by_key(|room| room.id);
    again.sort_by_key(|room| room.id);
    assert_eq!(rooms, again);
    assert_eq!(loaded.whereabouts().room, Some(barn));
    assert!(loaded.mobs().is_gear("A cow is here, mooing."));

    let versions = reader.map_versions().unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].room_count, 2);
    assert_eq!(versions[0].areas.get(FIELDS), Some(&2));

    // A loaded map goes on mapping where it left off.
    let mut walk = Walk {
        map: loaded,
        clock: walk.clock,
    };
    walk.go("n", "A loft", &["s"]);
    assert_eq!(walk.count(), 3);
    let commit = walk.map.commit(walk.now()).unwrap();
    writer.submit(commit.batch).unwrap();
    writer.flush().unwrap();
    assert_eq!(reader.map().unwrap().rooms.len(), 3);
}

#[test]
fn a_maze_collapse_writes_rows_the_database_accepts() {
    let folder = tempfile::tempdir().unwrap();
    let database = Database::open(folder.path(), "test.example").unwrap();
    let writer = database.writer().unwrap();
    let mut walk = Walk::new(text_map());
    walk.arrive("A clearing", &["e", "n"]);
    let steps = ["e", "e", "e", "w"];
    for (index, direction) in steps.into_iter().enumerate() {
        if index + 1 < steps.len() {
            woods(&mut walk, direction);
        } else {
            walk.go(direction, "A clearing", &["e", "n"]);
        }
        if let Some(commit) = walk.map.commit(walk.now()) {
            writer.submit(commit.batch).unwrap();
        }
    }
    writer.flush().unwrap();
    let rows = database.reader().unwrap().map().unwrap();
    assert_eq!(rows.rooms.len(), walk.count());
    assert_eq!(rows.rooms.iter().filter(|room| room.maze).count(), 1);
}

#[test]
fn a_cleared_note_is_deleted() {
    let mut walk = Walk::new(text_map());
    walk.arrive("A meadow", &["e"]);
    let meadow = walk.current().id;
    walk.map.set_notes(meadow, "nice");
    walk.map.commit(walk.now());
    walk.map.set_notes(meadow, "");
    let commit = walk.map.commit(walk.now()).unwrap();
    assert_eq!(
        written(&commit.batch).trim_end_matches("] }"),
        "Batch { changes: [Delete(RoomNote(1))"
    );
}
