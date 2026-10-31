//! Walks replayed through the whole engine, as the prototype replayed its
//! session logs through the map. Each fixture is the shortest stretch of a
//! real walk its test needs, with rooms, zones and text renamed.

use crate::game::Game;
use mudling_core::map::{Direction, GridPoint, MapRoom};
use std::collections::BTreeSet;
use std::sync::Arc;

const SHADE_EXITS: &[&str] = &["north", "east", "south", "west"];
const SHADE: &str = "In Shade of Tree of Knowledge";
const PROMPT: &str = "(110h-125m-120v-1062)(-/-)";

fn capitalized(word: &str) -> String {
    word[..1].to_uppercase() + &word[1..]
}

fn gmcp_exits(exits: &[&str]) -> String {
    let exits: Vec<String> = exits
        .iter()
        .map(|exit| format!("\"{}\": \"O\"", capitalized(exit)))
        .collect();
    exits.join(", ")
}

fn room_info(name: &str, environment: &str, exits: &[&str], details: &str) -> String {
    format!(
        "[GMCP] Room.Info {{ \"name\": \"{name}\", \"zone\": \"{{ 1   4}} Crom    The Meadow\", \
         \"environment\": \"{environment}\", \"exits\": {{ {} }}, \"details\": [ {details} ] }}",
        gmcp_exits(exits)
    )
}

/// The text and Room.Info Avatar sends on arriving in a room, then the
/// prompt, which Char.Vitals comes just before.
fn arrival(name: &str, exits: &[&str], environment: &str, description: &str) -> String {
    format!(
        "{name}\n[Exits: {}]\n   {description}\nand the description goes on.\n\n{}\n\
         [GMCP] Char.Vitals {{ }}\n{PROMPT}\n",
        exits.join(" "),
        room_info(name, environment, exits, "")
    )
}

fn field(name: &str, description: &str) -> String {
    arrival(name, SHADE_EXITS, "field", description)
}

/// What Avatar sends for a room too dark to see, whichever room it is.
fn dark_arrival() -> String {
    format!(
        "It is pitch black...\n\n{}\n[GMCP] Char.Vitals {{ }}\n{PROMPT}\n",
        room_info("It is pitch black...", "forest", SHADE_EXITS, "\"dark\"")
    )
}

fn moved(direction: &str) -> String {
    format!("[MAP] move {direction}\n")
}

fn played(log: &str) -> Game {
    let mut game = Game::new();
    game.play(log);
    game
}

fn untangled(rooms: &[Arc<MapRoom>]) {
    let tangled: Vec<&str> = rooms
        .iter()
        .filter(|room| room.tangled)
        .map(|room| room.name.as_str())
        .collect();
    assert!(tangled.is_empty(), "tangled: {tangled:?}");
}

/// Every flat link between `rooms` that `same_grid` puts on one grid is one
/// square long.
fn links_are_one_square(rooms: &[Arc<MapRoom>], same_grid: impl Fn(&MapRoom, &MapRoom) -> bool) {
    for room in rooms {
        for (direction, next) in room.links.iter() {
            let next = rooms.iter().find(|other| other.id == *next);
            let Some(next) = next.filter(|next| same_grid(room, next)) else {
                continue;
            };
            if !direction.is_vertical() {
                assert_eq!(
                    next.position,
                    room.position + direction.offset(),
                    "{} {direction}",
                    room.name
                );
            }
        }
    }
}

#[test]
fn a_stretched_loop_of_look_alikes_closes_on_the_start() {
    let trunk = "The trunk of the tree stands to the North and bends overhead to form a";
    let breeze = "Although you feel a strong breeze against your cheek";
    let log = [
        field(SHADE, trunk),
        moved("north"),
        field(SHADE, breeze),
        moved("north"),
        field(SHADE, "Giant branches overhead give you a sense of the age"),
        moved("east"),
        field(SHADE, "To the east is a green and red house on wheels."),
        moved("south"),
        field(SHADE, "You stumble on a small rock as you walk around here"),
        moved("west"),
        field(SHADE, trunk),
        // The same description is strong evidence, not proof; walking on
        // along the known path north confirms it.
        moved("north"),
        field(SHADE, breeze),
    ]
    .concat();

    let game = played(&log);
    assert_eq!(game.rooms().len(), 5);
    assert_eq!(game.current().description, breeze);
    untangled(&game.rooms());
    assert!(!game.whereabouts().unsure);
}

#[test]
fn dark_rooms_are_not_mapped_and_the_next_room_seen_goes_where_the_moves_lead() {
    let log = [
        field("Southern Meadow", "Deer and rabbits abound."),
        moved("east"),
        dark_arrival(),
        moved("east"),
        dark_arrival(),
    ]
    .concat();
    let game = played(&log);
    assert_eq!(game.rooms().len(), 1);
    assert!(game.whereabouts().dark);
    assert_eq!(
        game.whereabouts().dark_position,
        Some(GridPoint::new(2, 0, 0))
    );

    let seen = moved("north") + &field("The Meadow", "You stand waist deep.");
    let game = played(&(log + &seen));
    assert_eq!(game.rooms().len(), 2);
    let whereabouts = game.whereabouts();
    assert!(!whereabouts.dark && !whereabouts.unsure);
    assert_eq!(game.current().name, "The Meadow");
    assert_eq!(game.current().position, GridPoint::new(2, 1, 0));
    // The rooms in between were never seen, so nothing links to it.
    assert!(game.rooms().iter().all(|room| room.links.is_empty()));
}

#[test]
fn in_the_dark_the_map_follows_exits_it_already_knows() {
    let east = [
        field("Southern Meadow", "Deer and rabbits abound."),
        moved("east"),
        field("Eastern Meadow", "The grass grows tall here."),
        moved("east"),
        field("Far Meadow", "A fence runs along the edge."),
        moved("west"),
        field("Eastern Meadow", "The grass grows tall here."),
        moved("west"),
        field("Southern Meadow", "Deer and rabbits abound."),
    ]
    .concat();
    let dark = [moved("east"), dark_arrival(), moved("east"), dark_arrival()].concat();

    let game = played(&(east.clone() + &dark));
    let far = game.room("Far Meadow");
    assert_eq!(game.whereabouts().room, Some(far.id));
    assert_eq!(game.whereabouts().dark_position, None);

    let end = moved("east") + &field("Meadow's End", "The meadow stops.");
    let game = played(&(east + &dark + &end));
    assert_eq!(game.rooms().len(), 4);
    assert_eq!(game.current().position, GridPoint::new(3, 0, 0));
    assert_eq!(
        game.room("Far Meadow").links.get(Direction::East),
        Some(&game.current().id)
    );
}

#[test]
fn a_refused_move_is_dropped() {
    let log = [
        field("Southern Meadow", "Deer and rabbits abound."),
        moved("west"),
        "Alas, you cannot go that way.\n[GMCP] Room.WrongDir \"west\"\n".to_owned(),
        moved("north"),
        field("The Meadow", "You stand waist deep in thick grasses."),
    ]
    .concat();
    let game = played(&log);
    assert_eq!(game.current().name, "The Meadow");
    assert_eq!(game.current().position, GridPoint::new(0, 1, 0));
}

#[test]
fn a_flee_is_followed_from_the_text() {
    let log = [
        field("Southern Meadow", "Deer and rabbits abound."),
        "You flee east!\n".to_owned(),
        field("Eastern Meadow", "Small birds chirp at their leisure here."),
    ]
    .concat();
    assert_eq!(played(&log).current().position, GridPoint::new(1, 0, 0));
}

/// The first walk around the Tree of Knowledge. Outside, the meadow is an
/// exact grid; Nom's camp (W) and the east side (E) are neighbors, and
/// walking between them goes through the inside of the tree (I): a building,
/// bigger inside than out, with its own map and a door on each side.
///
/// ```text
/// M2  T2  T1  M1  NM
/// S6  W 🏠 E  S4  EM2
/// S5  S1  S2  S3  EM1
/// WM1/WM2 west of S6/S5
/// ```
#[test]
fn the_tree_of_knowledge_stands_in_the_meadow_like_a_building() {
    type Spot = (&'static str, &'static str);
    const SHADE_OF: &str = "In Shade of the Tree of Knowledge";
    let west: Spot = (
        "West Side of Tree of Knowledge",
        "You enter the campsite of an old man named Nom.",
    );
    let inside: Spot = (
        "Inside the Tree of Knowledge",
        "You stand in the center of the most ancient tree.",
    );
    let east: Spot = (
        "East Side of Tree of Knowledge",
        "You stand on the east side of the Tree of Knowledge.",
    );
    let s1 = (
        SHADE,
        "The trunk of the tree stands to the North and bends overhead to form a",
    );
    let s2 = (
        SHADE,
        "You stumble on a small rock as you walk around here.",
    );
    let s3 = (
        SHADE,
        "Although you feel a strong breeze against your cheek",
    );
    let s4 = (
        SHADE,
        "The cool shade of the tree is a nice break from the sun.",
    );
    let s5 = (
        SHADE,
        "Giant branches overhead give you a sense of the age.",
    );
    let s6 = (
        SHADE,
        "You walk under the branches of this ancient oak tree.",
    );
    let t1 = (
        SHADE_OF,
        "The cool breeze makes this a pleasant place to rest.",
    );
    let t2 = (
        SHADE_OF,
        "You walk underneath the canopy of a very large tree.",
    );
    let em1 = ("Eastern Meadow", "Small briars cling to your garments.");
    let em2 = ("Eastern Meadow", "You stand hip deep in red wildflowers.");
    let nm = (
        "Northern Meadow",
        "A young cleric runs by chasing a rabbit.",
    );
    let m1 = ("The Meadow", "Small rodents abound.");
    let m2 = ("The Meadow", "Butterflies flutter above waist high grass.");
    let wm1 = ("Western Meadow", "Small birds chirp at their leisure here.");
    let wm2 = ("Western Meadow", "Butterflies flutter above briars.");

    let steps = [
        ("south", s1),
        ("east", s2),
        ("east", s3),
        ("east", em1),
        ("north", em2),
        ("north", nm),
        ("west", m1),
        ("west", t1),
        ("west", t2),
        ("south", west),
        ("north", t2),
        ("east", t1),
        ("south", east),
        ("east", s4),
        ("west", east),
        ("west", inside),
        ("west", west),
        ("east", inside),
        ("east", east),
        ("south", s2),
        ("west", s1),
        ("west", s5),
        ("north", s6),
        ("east", west),
        ("north", t2),
        ("west", m2),
        ("south", s6),
        ("west", wm1),
        ("south", wm2),
        ("east", s5),
        ("north", s6),
    ];
    let visit = |room: Spot| {
        if room == inside {
            let exits = &["east", "west", "up", "down"];
            arrival(room.0, exits, "inside", room.1)
        } else {
            field(room.0, room.1)
        }
    };
    let mut log = visit(west);
    for (direction, room) in steps {
        log += &moved(direction);
        log += &visit(room);
    }
    let game = played(&log);
    let rooms = game.rooms();

    let at = |room: Spot| {
        rooms
            .iter()
            .find(|found| found.name == room.0 && found.description == room.1)
            .unwrap_or_else(|| panic!("{}", room.1))
    };
    let start = at(west).position;
    let expected = [
        (m2, -1, 1),
        (t2, 0, 1),
        (t1, 1, 1),
        (m1, 2, 1),
        (nm, 3, 1),
        (s6, -1, 0),
        (east, 1, 0),
        (s4, 2, 0),
        (em2, 3, 0),
        (s5, -1, -1),
        (s1, 0, -1),
        (s2, 1, -1),
        (s3, 2, -1),
        (em1, 3, -1),
        (wm1, -2, 0),
        (wm2, -2, -1),
    ];
    for (room, x, y) in expected {
        let spot = at(room).position - start;
        assert_eq!((spot.x, spot.y), (x, y), "{}", room.1);
    }
    let tree = at(inside);
    assert_eq!(tree.building, Some(tree.id));
    assert_eq!(rooms.len(), 18);
    untangled(&rooms);
    assert!(!game.whereabouts().unsure);

    let nom = at(west).id;
    let (tree, derived) = (tree.id, game.derived());
    let building = derived.buildings.iter().find(|built| built.id == tree);
    let building = building.expect("the tree is a building");
    assert_eq!(
        building.footprint, None,
        "no square for it between neighbors"
    );
    let doors: BTreeSet<&str> = building
        .doors
        .iter()
        .map(|door| door.direction.name())
        .collect();
    assert_eq!(doors, BTreeSet::from(["east", "west"]));
    assert!(
        building
            .doors
            .iter()
            .any(|door| door.outside == nom && door.direction == Direction::East)
    );
}

/// The stretch of a 61-move walk that shows it: around a tree, through it,
/// and on to a wagon south of it, whose loop shows the row south of the tree
/// has no gap. Outdoors every path is one square; the tree is lifted out
/// into a building, and the wagon is an ordinary square.
#[test]
fn the_meadow_is_an_exact_grid_with_the_tree_on_a_path() {
    let game = played(include_str!("../fixtures/walks/meadow-walk.txt"));
    let rooms = game.rooms();
    assert_eq!(rooms.len(), 9);
    assert!(!game.whereabouts().unsure);
    untangled(&rooms);

    let tree = game.room("Inside the Oak");
    let camp = game.room("West of the Oak");
    let wagon = game.room("The Wagon");
    assert_eq!(camp.links.get(Direction::East), Some(&tree.id));
    assert_eq!(tree.building, Some(tree.id));
    assert_eq!(wagon.building, None);
    assert!(wagon.links.len() >= 2, "the wagon is on a loop");

    let outdoors: Vec<Arc<MapRoom>> = rooms
        .iter()
        .filter(|room| room.building.is_none())
        .cloned()
        .collect();
    assert_eq!(outdoors.len(), rooms.len() - 1);
    links_are_one_square(&outdoors, |_, _| true);
    let squares: BTreeSet<(i64, i64, i64)> = outdoors
        .iter()
        .map(|room| (room.position.x, room.position.y, room.position.z))
        .collect();
    assert_eq!(squares.len(), outdoors.len());

    let derived = game.derived();
    let buildings: Vec<i64> = derived.buildings.iter().map(|built| built.id).collect();
    assert_eq!(buildings, [tree.id]);
    assert_eq!(derived.buildings[0].footprint, None);
}

/// The two ends of a 44-move walk: a canopy whose portal leads into another
/// area, and an indoor tunnel from a mountainside into a third. Nothing
/// needs lifting out, so the tunnel is an ordinary square.
#[test]
fn portals_lead_to_other_areas_and_tunnels_arent_houses() {
    let game = played(include_str!("../fixtures/walks/tree-portals-walk.txt"));
    let rooms = game.rooms();
    assert_eq!(rooms.len(), 5);
    assert!(rooms.iter().all(|room| room.building.is_none()));

    let portal = game.room("The Portal");
    let canopy = rooms
        .iter()
        .find(|room| room.links.get(Direction::West) == Some(&portal.id))
        .expect("a room west to the portal");
    assert_eq!(canopy.name, "The Canopy");
    assert_ne!(portal.zone, canopy.zone);
    let tunnel = game.room("The Tunnel");
    let leaves_its_area = tunnel.links.iter().any(|(_, next)| {
        let next = rooms.iter().find(|room| room.id == *next);
        next.is_some_and(|next| next.zone != tunnel.zone)
    });
    assert!(leaves_its_area, "the tunnel leads into another area");
    links_are_one_square(&rooms, |room, next| {
        next.zone == room.zone && next.building == room.building
    });
}

/// Avatar reuses room descriptions: three rooms of one name open with the
/// same line, told apart only by their exits. Taking one for another used
/// to tangle the area into mazes. The stretch of a 97-move walk that visits
/// all three.
#[test]
fn look_alikes_with_the_same_description_are_told_apart_by_their_exits() {
    let game = played(include_str!("../fixtures/walks/meadow-lookalikes-walk.txt"));
    let rooms = game.rooms();
    let meadow: Vec<Arc<MapRoom>> = rooms
        .iter()
        .filter(|room| room.zone.ends_with("Greenfield"))
        .cloned()
        .collect();
    untangled(&meadow);
    assert!(rooms.iter().all(|room| room.building.is_none()));
    let wheat = meadow
        .iter()
        .filter(|room| room.name == "Wheat Field" && room.description == "Wheat waves.");
    let exits: BTreeSet<Vec<Direction>> = wheat
        .map(|room| room.exit_directions().iter().collect())
        .collect();
    assert_eq!(exits.len(), 3);
    links_are_one_square(&meadow, |room, next| next.zone == room.zone);
}

/// Four rooms in a row with the same name, exits and description, walked
/// into twice. Each is a room of its own, in a straight line, with a road
/// beyond; not one room, and not a loop.
#[test]
fn a_corridor_of_identical_rooms_is_a_row_not_one_room() {
    let game = played(include_str!("../fixtures/walks/narrow-trail-walk.txt"));
    let rooms = game.rooms();
    let mut trail: Vec<&Arc<MapRoom>> =
        rooms.iter().filter(|room| room.name == "A Trail").collect();
    trail.sort_by_key(|room| -room.position.x);
    assert_eq!(trail.len(), 4);
    assert!(
        trail
            .iter()
            .all(|room| room.position.y == trail[0].position.y)
    );
    for pair in trail.windows(2) {
        assert_eq!(pair[0].links.get(Direction::West), Some(&pair[1].id));
        assert_eq!(pair[1].position.x, pair[0].position.x - 1);
    }
    let beyond = trail[3].links.get(Direction::West).expect("a way on");
    let beyond = rooms.iter().find(|room| room.id == *beyond).unwrap();
    assert_eq!(beyond.name, "The Road");
    untangled(&rooms);
    assert!(!game.whereabouts().unsure);
}

/// A home with a door every way, each opening on a story, one of them
/// through a maze, and the way in from the west. The area and the maze keep
/// their names, which the plugin's maze rule reads.
#[test]
fn each_story_of_the_brothers_grimm_is_a_map_of_its_own() {
    let game = played(include_str!("../fixtures/walks/brothers-grimm-walk.txt"));
    let rooms = game.rooms();
    let grimm: Vec<&Arc<MapRoom>> = rooms
        .iter()
        .filter(|room| room.zone.ends_with("Brothers Grimm"))
        .collect();
    let home = game.room("The Home");
    let through = |direction| {
        let next = home.links.get(direction).expect("a door");
        rooms.iter().find(|room| room.id == *next).unwrap()
    };

    // The home and the way in stay on the area's map.
    assert_eq!(home.building, None);
    assert_eq!(through(Direction::West).building, None);
    // Every other door opens on a different story.
    let stories: Vec<i64> = [
        Direction::North,
        Direction::South,
        Direction::East,
        Direction::Up,
        Direction::Down,
    ]
    .into_iter()
    .map(|direction| through(direction).building.expect("a story"))
    .collect();
    assert_eq!(stories.iter().collect::<BTreeSet<_>>().len(), 5);

    // The maze is one square in the east story, and nothing is mapped twice.
    let mazes: Vec<&&Arc<MapRoom>> = grimm.iter().filter(|room| room.maze).collect();
    assert_eq!(mazes.len(), 1);
    assert_eq!(mazes[0].name, "Lost in the Dark Woods");
    assert_eq!(mazes[0].building, Some(stories[2]));
    assert_eq!(grimm.len(), 9);
    assert!(grimm.iter().all(|room| !room.tangled || room.maze));
}

/// The keep's halls ring the sanctuary, all with one description.
#[test]
fn identical_halls_around_a_sanctuary_stay_apart() {
    const HALL: &str = "Midgaard Keep Main Hall";
    const SANCTUARY: &str = "Aelmon's Sanctuary";
    const WORDS: &str = "This high ceiling used to clamor with a thousand men";
    let hall = |exits: &[&str]| arrival(HALL, exits, "inside", WORDS);
    let sanctuary = arrival(SANCTUARY, SHADE_EXITS, "inside", "A quiet place.");
    let keep = arrival(
        "Inside Midgaard Keep",
        &["north", "south"],
        "inside",
        "The keep's entry.",
    );
    let log = [
        sanctuary.clone(),
        moved("north"),
        hall(SHADE_EXITS),
        moved("north"),
        keep,
        moved("south"),
        hall(SHADE_EXITS),
        moved("south"),
        sanctuary.clone(),
        moved("east"),
        hall(SHADE_EXITS),
        // Back and forth, which used to settle it as the north hall.
        moved("west"),
        sanctuary.clone(),
        moved("east"),
        hall(SHADE_EXITS),
        moved("north"),
        hall(&["south", "west"]),
        moved("west"),
        hall(SHADE_EXITS),
        moved("south"),
        sanctuary,
    ]
    .concat();

    let game = played(&log);
    let rooms = game.rooms();
    assert_eq!(rooms.len(), 5);
    untangled(&rooms);
    let east = game.current().links.get(Direction::East).copied().unwrap();
    let east = rooms.iter().find(|room| room.id == east).unwrap();
    assert_eq!(
        east.position - game.current().position,
        GridPoint::new(1, 0, 0)
    );
}
