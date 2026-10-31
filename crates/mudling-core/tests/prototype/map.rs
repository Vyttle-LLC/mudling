//! The map through its public face, on made-up rooms: rooms told apart by
//! text, as a game with no room numbers is.

use mudling_core::map::{
    Arrived, Direction, GridPoint, LiveMap, MapRoom, MapRules, Mark, MobMemory, RoomId,
    RoomIdentity, feature,
};
use mudling_core::persist::{Batch, Database, Timestamp};

const FIELDS: &str = "The Fields";
const CAVES: &str = "The Caves";
const EXITS_LINE: &str = "Exits:";
const SHADE: &str = "In Shade";
const CROSSROADS: &[&str] = &["north", "east", "south", "west"];
const CAMP: &str = "A camp";
const INSIDE: &str = "Inside a tree";
const UNDER: &str = "Under a tree";

struct TextRules;

impl MapRules for TextRules {
    fn identity(&self) -> RoomIdentity {
        RoomIdentity::Text
    }

    fn exits_line(&self, line: &str) -> bool {
        line.starts_with(EXITS_LINE)
    }
}

struct Walk {
    map: LiveMap,
    clock: i64,
    /// What each commit would have written.
    saved: Vec<Batch>,
}

impl Walk {
    fn new() -> Walk {
        Walk {
            map: LiveMap::new(Box::new(TextRules)),
            clock: 0,
            saved: Vec::new(),
        }
    }

    fn now(&mut self) -> Timestamp {
        self.clock += 1_000;
        Timestamp(self.clock)
    }

    fn info(name: &str, zone: &str, environment: &str, exits: &[&str]) -> String {
        let exits: Vec<String> = exits
            .iter()
            .map(|exit| format!("\"{exit}\":\"O\""))
            .collect();
        format!(
            r#"{{"name":"{name}","zone":"{zone}","environment":"{environment}","exits":{{{}}}}}"#,
            exits.join(",")
        )
    }

    /// The player is in this room, shown with `description` if there is one.
    fn arrive_in(
        &mut self,
        zone: &str,
        environment: &str,
        name: &str,
        exits: &[&str],
        description: &str,
    ) -> Arrived {
        let now = self.now();
        if !description.is_empty() {
            self.map.line(name, now);
            self.map.line(EXITS_LINE, now);
            self.map.exits_line();
            self.map.line(description, now);
        }
        let info = Walk::info(name, zone, environment, exits);
        let arrived = self.map.room_info(&info, now).expect("it reads");
        self.saved
            .extend(self.map.commit(now).map(|commit| commit.batch));
        arrived
    }

    fn arrive(&mut self, name: &str, exits: &[&str]) -> Arrived {
        self.arrive_in(FIELDS, "field", name, exits, "")
    }

    fn describe(&mut self, name: &str, exits: &[&str], description: &str) -> Arrived {
        self.arrive_in(FIELDS, "field", name, exits, description)
    }

    fn head(&mut self, direction: &str) {
        let now = self.now();
        self.map.sent(direction, now);
    }

    fn go(&mut self, direction: &str, name: &str, exits: &[&str]) -> Arrived {
        self.head(direction);
        self.arrive(name, exits)
    }

    fn current(&self) -> &MapRoom {
        self.map.current_room().expect("the player is on the map")
    }

    fn id(&self, name: &str) -> RoomId {
        let mut rooms = self.map.rooms();
        rooms.find(|room| room.name == name).expect("mapped").id
    }

    fn tangled(&self) -> usize {
        self.map.rooms().filter(|room| room.tangled).count()
    }

    /// The same room, four sides of a square.
    fn square() -> Walk {
        let mut walk = Walk::new();
        walk.arrive(SHADE, CROSSROADS);
        for direction in ["east", "north", "west"] {
            walk.go(direction, SHADE, CROSSROADS);
        }
        walk
    }

    /// Camp -e-> Inside -d-> Under -d-> Hole, the last in another area.
    fn down_the_tree() -> Walk {
        let mut walk = Walk::new();
        walk.arrive(CAMP, CROSSROADS);
        walk.go("east", INSIDE, &["east", "west", "up", "down"]);
        walk.go("down", UNDER, &["east", "up", "down"]);
        walk
    }
}

#[test]
fn up_and_down_change_level() {
    let mut walk = Walk::new();
    walk.arrive(INSIDE, &["down"]);
    walk.go("down", UNDER, &["up"]);
    assert_eq!(walk.current().position.z, -1);
}

#[test]
fn a_taken_square_pushes_a_new_room_further_out() {
    let mut walk = Walk::new();
    walk.arrive(CAMP, CROSSROADS);
    walk.go("east", INSIDE, &["east", "west"]);
    walk.go("west", CAMP, CROSSROADS);
    walk.go("east", "A shortcut", &["west"]);
    assert_eq!(walk.current().position, GridPoint::new(2, 0, 0));
    assert!(walk.current().tangled);
}

#[test]
fn a_rooms_features_are_noted() {
    let mut walk = Walk::new();
    walk.arrive(CAMP, CROSSROADS);
    walk.map.note_feature("fountain");
    assert!(walk.current().features.contains("fountain"));
}

#[test]
fn notes_and_marks_can_be_set_on_any_room() {
    let mut walk = Walk::down_the_tree();
    let camp = walk.id(CAMP);
    walk.map.set_notes(camp, "Heals here for free");
    walk.map.set_feature(camp, "trainer", true);
    walk.map.set_feature(camp, feature::AGGRESSIVE_MOB, true);
    walk.map.set_feature(camp, feature::AGGRESSIVE_MOB, false);
    let room = walk.map.room(camp).unwrap();
    assert_eq!(room.notes, "Heals here for free");
    assert_eq!(
        room.features.iter().collect::<Vec<_>>(),
        ["trainer"],
        "the other came off"
    );
}

#[test]
fn a_square_of_identical_rooms_maps_cleanly() {
    let mut walk = Walk::square();
    walk.go("south", SHADE, CROSSROADS);
    assert_eq!(walk.map.room_count(), 4);
    assert_eq!(walk.tangled(), 0);
    assert_eq!(walk.current().position, GridPoint::new(0, 0, 0));
}

#[test]
fn a_look_among_look_alikes_stays_put() {
    let mut walk = Walk::square();
    let here = walk.map.whereabouts().room;
    let now = walk.now();
    walk.map.sent("look", now);
    walk.arrive(SHADE, CROSSROADS);
    assert_eq!(walk.map.whereabouts().room, here);
    assert_eq!(walk.map.room_count(), 4);
}

/// A background command used to wipe the pending move, so the next room
/// arrived unexplained and became a stray island.
#[test]
fn an_unexplained_room_among_look_alikes_stays_put() {
    let mut walk = Walk::square();
    let here = walk.map.whereabouts().room;
    walk.arrive(SHADE, CROSSROADS);
    assert_eq!(walk.map.whereabouts().room, here);
    assert_eq!(walk.map.room_count(), 4);
}

#[test]
fn im_here_moves_the_player() {
    let mut walk = Walk::down_the_tree();
    let camp = walk.id(CAMP);
    walk.map.relocate(camp);
    walk.go("east", INSIDE, &["east", "west", "up", "down"]);
    assert_eq!(walk.map.room_count(), 3);
    assert_eq!(walk.current().name, INSIDE);
}

#[test]
fn not_a_maze_clears_the_mark() {
    let mut walk = Walk::new();
    walk.arrive(CAMP, CROSSROADS);
    walk.go("east", INSIDE, &["east", "west"]);
    walk.go("west", CAMP, CROSSROADS);
    walk.go("east", "A shortcut", &["west"]);
    let tangled: Vec<RoomId> = walk
        .map
        .rooms()
        .filter(|room| room.tangled)
        .map(|room| room.id)
        .collect();
    assert!(!tangled.is_empty());
    walk.map.clear_tangles(&tangled);
    assert_eq!(walk.tangled(), 0);
}

#[test]
fn marking_a_mob_flags_its_whole_kind() {
    let mut memory = MobMemory::default();
    memory.set_aggressive("A myconoid shaman is here, spores drifting.", true);
    assert!(memory.is_aggressive("A myconoid shaman chants quietly."));
    assert!(!memory.is_aggressive("A field mouse is burrowing around under the grass."));
}

#[test]
fn a_mob_learned_from_its_attack_flags_its_kind_only() {
    let mut memory = MobMemory::default();
    memory.learn_aggressor("The ugly bear");
    memory.learn_aggressor("The ugly bear");
    assert!(memory.is_aggressive("An ugly stuffed bear stands here."));
    assert!(!memory.is_aggressive("A toy soldier is standing here."));
    assert_eq!(memory.marks(Mark::Aggressive).len(), 1);
}

#[test]
fn moves_read_in_any_spelling() {
    for (command, direction) in [
        ("n", Direction::North),
        ("north", Direction::North),
        ("S", Direction::South),
        ("e", Direction::East),
        ("west", Direction::West),
        ("u", Direction::Up),
        ("down", Direction::Down),
    ] {
        assert_eq!(Direction::parse(command), Some(direction), "{command}");
    }
    assert_eq!(Direction::parse("look"), None);
    assert_eq!(Direction::parse("open n"), None);
}

#[test]
fn an_unexplained_arrival_finds_the_described_room() {
    let mut walk = Walk::new();
    let (first, second) = ("The trunk of the tree", "Shade covers the grass");
    walk.describe(SHADE, CROSSROADS, first);
    let start = walk.map.whereabouts().room;
    walk.head("east");
    walk.describe(SHADE, CROSSROADS, second);
    // Summoned: no move asked.
    walk.describe(SHADE, CROSSROADS, first);
    assert_eq!(walk.map.whereabouts().room, start);
    assert_eq!(walk.map.room_count(), 2);
}

#[test]
fn rooms_mapped_before_their_description_learn_it() {
    let mut walk = Walk::new();
    walk.arrive(CAMP, CROSSROADS);
    walk.go("east", INSIDE, &["east", "west"]);
    walk.head("west");
    walk.describe(CAMP, CROSSROADS, "You stand on the west side");
    assert_eq!(walk.map.room_count(), 2);
    assert_eq!(walk.current().description, "You stand on the west side");
}

#[test]
fn im_here_keeps_a_stray_with_notes() {
    let mut walk = Walk::new();
    walk.arrive(CAMP, CROSSROADS);
    walk.arrive("A field of flowers", &["north"]);
    let field = walk.id("A field of flowers");
    walk.map.set_notes(field, "herbs here");
    walk.map.relocate(walk.id(CAMP));
    assert!(walk.map.room(field).is_some());
}

/// Saved while the map is unsure, the map as shown comes back, and sure.
#[test]
fn a_map_saved_while_unsure_keeps_its_best_guess() {
    let mut walk = Walk::new();
    walk.arrive(SHADE, CROSSROADS);
    walk.go("north", "A pond", &["south", "east"]);
    walk.go("east", "A meadow", &["west", "east"]);
    walk.go("east", "A field", &["west", "south", "east"]);
    walk.go("south", SHADE, CROSSROADS);
    walk.go("north", "A field", &["west", "south", "east"]);
    walk.go("east", "A barn", &["west"]);
    walk.arrive(SHADE, CROSSROADS);
    assert!(walk.map.whereabouts().unsure);

    let folder = tempfile::tempdir().unwrap();
    let database = Database::open(folder.path(), "mud.example.org").unwrap();
    let writer = database.writer().unwrap();
    for batch in walk.saved.drain(..) {
        writer.submit(batch).unwrap();
    }
    writer.flush().unwrap();
    let reader = database.reader().unwrap();
    let (rows, marks, meta) = (
        reader.map().unwrap(),
        reader.mob_marks().unwrap(),
        reader.meta().unwrap(),
    );
    let loaded = LiveMap::load(Box::new(TextRules), &rows, &marks, &meta);

    let places = |map: &LiveMap| {
        let mut rooms: Vec<(RoomId, GridPoint)> =
            map.rooms().map(|room| (room.id, room.position)).collect();
        rooms.sort_by_key(|room| room.0);
        rooms
    };
    assert_eq!(places(&loaded), places(&walk.map));
    assert!(!loaded.whereabouts().unsure);
}

#[test]
fn map_versions_saved_the_same_moment_are_both_kept() {
    let mut walk = Walk::new();
    walk.arrive(CAMP, CROSSROADS);
    let folder = tempfile::tempdir().unwrap();
    let database = Database::open(folder.path(), "mud.example.org").unwrap();
    let writer = database.writer().unwrap();
    for batch in walk.saved.drain(..) {
        writer.submit(batch).unwrap();
    }
    for label in ["First", "Second"] {
        writer
            .submit(LiveMap::save_version("safety", label, Timestamp(1)))
            .unwrap();
    }
    writer.flush().unwrap();
    let versions = database.reader().unwrap().map_versions().unwrap();
    let mut labels: Vec<&str> = versions.iter().map(|v| v.label.as_str()).collect();
    labels.sort_unstable();
    assert_eq!(labels, ["First", "Second"]);
}

#[test]
fn floors_belong_to_the_building() {
    const WEST: &[&str] = &["east", "north", "south"];
    const EAST: &[&str] = &["west", "north", "south"];
    const THROUGH: &[&str] = &["east", "west", "up", "down"];
    let mut walk = Walk::new();
    let (camp, east_side) = ("West side", "East side");
    let lit = |walk: &mut Walk, direction: &str, name: &str, exits: &[&str]| {
        walk.head(direction);
        walk.describe(name, exits, name);
    };
    walk.describe(camp, WEST, camp);
    lit(&mut walk, "north", "North-west", &["south", "east"]);
    lit(&mut walk, "east", "North-east", &["south", "west"]);
    lit(&mut walk, "south", east_side, EAST);
    lit(&mut walk, "south", "South-east", &["north", "west"]);
    lit(&mut walk, "west", "South-west", &["north", "east"]);
    lit(&mut walk, "north", camp, WEST);
    let indoors = |walk: &mut Walk, direction: &str, name: &str, exits: &[&str]| {
        walk.head(direction);
        walk.arrive_in(FIELDS, "inside", name, exits, name);
    };
    indoors(&mut walk, "east", INSIDE, THROUGH);
    lit(&mut walk, "east", east_side, EAST);
    indoors(&mut walk, "west", INSIDE, THROUGH);
    indoors(&mut walk, "up", "The canopy", &["down"]);
    indoors(&mut walk, "down", INSIDE, THROUGH);
    lit(&mut walk, "west", camp, WEST);

    let tree = walk.id(INSIDE);
    assert_eq!(walk.map.room(tree).unwrap().building, Some(tree));
    let canopy = walk.id("The canopy");
    assert_eq!(walk.map.room(canopy).unwrap().building, Some(tree));
    assert_eq!(walk.current().building, None);
}

#[test]
fn a_tunnel_into_another_area_is_no_house() {
    let mut walk = Walk::new();
    walk.arrive("A mountain trail", &["south"]);
    walk.head("south");
    walk.arrive_in(
        FIELDS,
        "inside",
        "Inside a mountain",
        &["north", "south"],
        "",
    );
    walk.head("south");
    walk.arrive_in(CAVES, "inside", "A drop-off", &["north"], "");
    assert!(walk.map.rooms().all(|room| room.building.is_none()));
}
