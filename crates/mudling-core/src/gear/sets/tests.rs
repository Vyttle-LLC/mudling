use super::*;
use crate::persist::Batch;

const NOW: Timestamp = Timestamp(100);

fn rules() -> GearRules {
    let mut rules = GearRules::default();
    let words = |list: &[&str]| list.iter().map(|word| (*word).to_owned()).collect();
    rules.use_words(
        BTreeMap::new(),
        Keywords {
            skip: words(&["a", "an", "the", "some"]),
            phrase_ends: words(&["of", "that", "with"]),
            possessives: false,
        },
    );
    for slot in ["finger", "neck", "wrist"] {
        rules.note_holds(slot, 2);
    }
    rules.note_slot_verb("held", "hold");
    rules.note_slot_verb("wield", "wield");
    rules
}

fn book() -> GearBook {
    let mut book = GearBook::default();
    let mut batch = Batch::new();
    for (name, slot) in [
        ("a mushroom-shaped amulet", "neck"),
        ("an amulet of Gorn", "neck"),
        ("a small necklace", "neck"),
        ("a linen robe", "about"),
        ("a white healer's baldric", "about"),
        ("the red-eyed jewel", "held"),
        ("a tattered kite shield", "held"),
        ("the Orb of Faerie Magic", "light"),
    ] {
        book.learn_slot(slot, name, NOW, &mut batch);
    }
    for (name, keywords) in [
        ("a mushroom-shaped amulet", "amulet mushroom"),
        ("a linen robe", "robe linen"),
        ("the red-eyed jewel", "jewel red-eyed gem"),
    ] {
        book.learn_keywords(keywords, name, NOW, &mut batch);
    }
    book
}

fn piece(item: &str) -> GearPiece {
    GearPiece {
        item: item.to_owned(),
        slot: None,
        copy_id: None,
    }
}

fn in_slot(item: &str, slot: &str) -> GearPiece {
    GearPiece {
        slot: Some(slot.to_owned()),
        ..piece(item)
    }
}

fn pinned(item: &str, slot: &str, copy: &str) -> GearPiece {
    GearPiece {
        copy_id: Some(copy.to_owned()),
        ..in_slot(item, slot)
    }
}

fn set(name: &str, pieces: Vec<GearPiece>, container: Option<&str>) -> GearSet {
    GearSet {
        id: "1".to_owned(),
        character_id: "tamsin".to_owned(),
        name: name.to_owned(),
        container: container.map(str::to_owned),
        pieces,
        updated: NOW,
    }
}

fn leveling() -> GearSet {
    let pieces = [
        "a mushroom-shaped amulet",
        "a linen robe",
        "the red-eyed jewel",
    ];
    set("Leveling", pieces.map(piece).to_vec(), Some("a satchel"))
}

fn hit() -> GearSet {
    let pieces = [
        "the Orb of Faerie Magic",
        "an amulet of Gorn",
        "a small necklace",
        "a white healer's baldric",
        "a tattered kite shield",
    ];
    set("Hit", pieces.map(piece).to_vec(), Some("a satchel"))
}

fn worn(slot: &str, item: &str) -> WornItem {
    WornItem {
        slot: slot.to_owned(),
        item: item.to_owned(),
        copy: None,
    }
}

fn worn_copy(slot: &str, item: &str, copy: &str) -> WornItem {
    WornItem {
        copy: Some(copy.to_owned()),
        ..worn(slot, item)
    }
}

/// An item by name only: its id can't name it.
fn named(name: &str) -> CarriedItem {
    copy(name, "")
}

fn copy(name: &str, id: &str) -> CarriedItem {
    CarriedItem {
        id: id.to_owned(),
        name: name.to_owned(),
    }
}

fn bag(name: &str, id: &str, items: Vec<CarriedItem>) -> CarriedBag {
    CarriedBag {
        id: id.to_owned(),
        name: name.to_owned(),
        items,
    }
}

/// What's at hand, for a plan.
#[derive(Default)]
struct Gear {
    worn: Vec<WornItem>,
    carried: Vec<CarriedItem>,
    bags: Vec<CarriedBag>,
}

impl Gear {
    fn now(&self) -> GearNow<'_> {
        GearNow {
            worn: &self.worn,
            carried: &self.carried,
            bags: &self.bags,
        }
    }

    fn plan(&self, set: &GearSet) -> WearPlan {
        plan(set, self.now(), &book(), &rules())
    }

    fn plan_with(&self, set: &GearSet, book: &GearBook) -> WearPlan {
        plan(set, self.now(), book, &rules())
    }
}

fn wearing_hit() -> Gear {
    let in_satchel = [
        "a mushroom-shaped amulet",
        "a linen robe",
        "the red-eyed jewel",
    ];
    Gear {
        worn: vec![
            worn("light", "the Orb of Faerie Magic"),
            worn("neck", "an amulet of Gorn"),
            worn("neck", "a small necklace"),
            worn("about", "a white healer's baldric"),
            worn("held", "a tattered kite shield"),
        ],
        carried: Vec::new(),
        bags: vec![bag("a satchel", "", in_satchel.map(named).to_vec())],
    }
}

/// A set saved from what was worn knows each slot, even one never seen.
#[test]
fn a_piece_saved_with_its_slot_makes_room_there() {
    let gear = Gear {
        worn: vec![
            worn("neck", "an amulet of Gorn"),
            worn("neck", "a small necklace"),
        ],
        carried: vec![named("the mantle of Bonk")],
        ..Gear::default()
    };
    let wanted = set(
        "Leveling",
        vec![in_slot("the mantle of Bonk", "neck")],
        None,
    );
    let plan = gear.plan_with(&wanted, &GearBook::default());
    assert_eq!(plan.commands, ["remove amulet", "wear mantle"]);
}

/// What's in the way comes off first: one of the two at the neck, and what
/// fills the body and hands. The pieces come out of the satchel, what came off
/// goes in, and the pieces go on.
#[test]
fn pieces_in_a_bag_come_out_and_go_on() {
    assert_eq!(
        wearing_hit().plan(&leveling()).commands,
        [
            "remove gorn",
            "remove baldric",
            "remove shield",
            "get 'amulet mushroom' satchel",
            "get 'robe linen' satchel",
            "get 'jewel red-eyed gem' satchel",
            "put gorn satchel",
            "put baldric satchel",
            "put shield satchel",
            "wear 'amulet mushroom'",
            "wear 'robe linen'",
            "hold 'jewel red-eyed gem'",
        ]
    );
}

#[test]
fn pieces_already_worn_are_left_alone() {
    let plan = wearing_hit().plan(&hit());
    assert_eq!(plan.left_alone, 5);
    assert_eq!(
        plan,
        WearPlan {
            left_alone: 5,
            ..WearPlan::default()
        }
    );
}

#[test]
fn carried_pieces_go_straight_on() {
    let gear = Gear {
        carried: vec![named("a linen robe")],
        ..Gear::default()
    };
    let robe = set("Robe", vec![piece("a linen robe")], None);
    assert_eq!(gear.plan(&robe).commands, ["wear 'robe linen'"]);
}

#[test]
fn pieces_found_nowhere_are_reported() {
    let robe = set("Robe", vec![piece("a linen robe")], None);
    let plan = Gear::default().plan(&robe);
    assert!(plan.commands.is_empty());
    assert_eq!(plan.missing, ["a linen robe"]);
}

/// Two manes: one is on, so only the second comes out of the bag.
#[test]
fn copies_count_separately() {
    let gear = Gear {
        worn: vec![worn("wrist", "a lion's mane")],
        bags: vec![bag("a satchel", "", vec![named("a lion's mane")])],
        ..Gear::default()
    };
    let manes = set(
        "Manes",
        vec![piece("a lion's mane"), piece("a lion's mane")],
        None,
    );
    assert_eq!(
        gear.plan(&manes).commands,
        ["get mane satchel", "wear mane"]
    );
}

/// The satchel also holds a belt that says "Seven In One.".
#[test]
fn a_guessed_word_picks_out_its_item_among_whats_nearby() {
    let items = vec![
        named("a rope belt"),
        named(r#"belt that says, "Seven In One.""#),
    ];
    let gear = Gear {
        bags: vec![bag("a satchel", "", items)],
        ..Gear::default()
    };
    let belt = set("Hit", vec![piece("a rope belt")], None);
    assert_eq!(gear.plan(&belt).commands, ["get rope satchel", "wear rope"]);
}

/// A set with both a kite shield and a golden feather can hold only one: the
/// feather, chosen later, wins, so the shield goes into the satchel like
/// anything else.
#[test]
fn when_a_set_overfills_a_slot_the_later_piece_wins() {
    let gear = Gear {
        worn: vec![worn("held", "a tattered kite shield")],
        bags: vec![bag("a satchel", "", vec![named("a golden feather")])],
        ..Gear::default()
    };
    let pieces = vec![
        in_slot("a tattered kite shield", "held"),
        in_slot("a golden feather", "held"),
    ];
    let wanted = set("Leveling", pieces, Some("a satchel"));
    assert_eq!(
        gear.plan_with(&wanted, &GearBook::default()).commands,
        [
            "remove shield",
            "get feather satchel",
            "put shield satchel",
            "hold feather",
        ]
    );
}

/// A game may refuse a possessive for a command's word.
#[test]
fn avoids_words_the_game_wont_take() {
    let keywords = rules().keywords;
    let names = ["the sergeant's silver sash", "a scarlet silk sash"];
    assert_eq!(likely_word(names[0], &keywords, &names), "silver");
}

#[test]
fn counts_when_no_word_tells_items_apart() {
    let keywords = rules().keywords;
    let word = |name, names: &[&str]| likely_word(name, &keywords, names);
    assert_eq!(
        word("a gold ring", &["a silver ring", "a gold ring"]),
        "gold"
    );
    assert_eq!(word("a ring", &["a ring of iron", "a ring"]), "2.ring");
    let two = ["a sprite ribcage", "a sprite ribcage"];
    assert_eq!(word("a sprite ribcage", &two), "ribcage");
}

/// Games match the start of "red-eyed", so "eyed" fails.
#[test]
fn uses_the_start_of_a_hyphenated_word() {
    let keywords = rules().keywords;
    let names = ["the red-eyed jewel", "the Tiger Jewel"];
    assert_eq!(likely_word(names[0], &keywords, &names), "red");
}

/// A bare word for the amulet also named a belt whose hidden name is the same.
#[test]
fn words_given_to_an_item_name_it_in_every_command() {
    let mut book = GearBook::default();
    book.set_keywords(
        "  amulet   gorn ",
        "an amulet of Gorn",
        NOW,
        &mut Batch::new(),
    );
    let gear = Gear {
        worn: vec![
            worn("neck", "an amulet of Gorn"),
            worn("neck", "a small necklace"),
        ],
        carried: vec![named("the mantle of Bonk")],
        ..Gear::default()
    };
    let wanted = set(
        "Leveling",
        vec![in_slot("the mantle of Bonk", "neck")],
        Some("a satchel"),
    );
    assert_eq!(
        gear.plan_with(&wanted, &book).commands,
        [
            "remove 'amulet gorn'",
            "put 'amulet gorn' satchel",
            "wear mantle"
        ]
    );
    book.set_keywords("", "an amulet of Gorn", NOW, &mut Batch::new());
    let item = book.item("an amulet of Gorn").unwrap();
    assert_eq!(item.facts.keywords, None);
}

#[test]
fn keywords_that_could_carry_a_command_are_never_sent() {
    let mut book = GearBook::default();
    let mut batch = Batch::new();
    book.learn_keywords("robe;quit now", "a linen robe", NOW, &mut batch);
    let gear = Gear {
        carried: vec![named("a linen robe")],
        ..Gear::default()
    };
    let robe = set("Robe", vec![piece("a linen robe")], None);
    assert_eq!(gear.plan_with(&robe, &book).commands, ["wear robe"]);
}

/// Both neck pieces in the set: both that aren't come off first.
#[test]
fn two_neck_pieces_each_make_room() {
    let gear = Gear {
        worn: vec![
            worn("neck", "an amulet of Gorn"),
            worn("neck", "(Glowing) a small necklace"),
        ],
        bags: vec![bag(
            "a satchel",
            "",
            vec![
                named("the mantle of Bonk"),
                named("a mushroom-shaped amulet"),
            ],
        )],
        ..Gear::default()
    };
    let pieces = vec![
        in_slot("the mantle of Bonk", "neck"),
        in_slot("a mushroom-shaped amulet", "neck"),
    ];
    let plan = gear.plan_with(&set("Leveling", pieces, None), &GearBook::default());
    assert_eq!(
        plan.commands,
        [
            "remove gorn",
            "remove necklace",
            "get mantle satchel",
            "get mushroom satchel",
            "wear mantle",
            "wear mushroom",
        ]
    );
}

const SATCHEL: &str = "30988637278";
const ENCHANTED: &str = "30989400863";
const OWN: &str = "30988637148";

fn satchel_with(items: Vec<CarriedItem>) -> CarriedBag {
    bag("a satchel", SATCHEL, items)
}

fn the_enchanted_amulet(container: Option<&str>) -> GearSet {
    let pieces = vec![pinned("an amulet of Gorn", "neck", ENCHANTED)];
    set("Hit", pieces, container)
}

#[test]
fn copies_with_ids_are_named_by_them() {
    let gear = Gear {
        worn: vec![
            worn_copy("neck", "a small necklace", "5"),
            worn_copy("neck", "a mushroom-shaped amulet", "6"),
        ],
        carried: vec![copy("a satchel", SATCHEL)],
        bags: vec![satchel_with(vec![copy("an amulet of Gorn", ENCHANTED)])],
    };
    let wanted = the_enchanted_amulet(Some("a satchel"));
    assert_eq!(
        gear.plan_with(&wanted, &GearBook::default()).commands,
        [
            "remove 5",
            "get 30989400863 30988637278",
            "put 5 30988637278",
            "wear 30989400863",
        ]
    );
}

/// The set has the enchanted amulet; a plain one is worn and the enchanted one
/// is in the satchel: they change places.
#[test]
fn the_chosen_copy_replaces_another_of_its_name() {
    let gear = Gear {
        worn: vec![worn_copy("neck", "an amulet of Gorn", OWN)],
        bags: vec![satchel_with(vec![copy("an amulet of Gorn", ENCHANTED)])],
        ..Gear::default()
    };
    let plan = gear.plan_with(
        &the_enchanted_amulet(Some("a satchel")),
        &GearBook::default(),
    );
    let moved = |id: &str| Moved {
        name: "an amulet of Gorn".to_owned(),
        copy: Some(id.to_owned()),
    };
    assert_eq!(plan.removed, [moved(OWN)]);
    assert_eq!(plan.wearing, [moved(ENCHANTED)]);
    assert!(plan.stand_ins.is_empty());
}

/// The prototype wore another of the name here; MUDling never does.
#[test]
fn a_chosen_copy_thats_gone_is_missing_and_another_of_the_name_stays_out() {
    let gear = Gear {
        carried: vec![copy("an amulet of Gorn", OWN)],
        ..Gear::default()
    };
    let plan = gear.plan_with(&the_enchanted_amulet(None), &GearBook::default());
    assert!(plan.commands.is_empty());
    assert_eq!(plan.missing, ["an amulet of Gorn"]);
}

#[test]
fn a_chosen_copy_isnt_found_in_a_bag_of_others_of_its_name() {
    let gear = Gear {
        bags: vec![satchel_with(vec![copy("an amulet of Gorn", OWN)])],
        ..Gear::default()
    };
    let plan = gear.plan_with(&the_enchanted_amulet(None), &GearBook::default());
    assert!(plan.commands.is_empty());
    assert_eq!(plan.missing, ["an amulet of Gorn"]);
}

/// It's on and it stays on, as nothing else could replace it, but the player
/// hears it's not the one they chose.
#[test]
fn another_of_the_name_already_on_stays_on_and_says_so() {
    let gear = Gear {
        worn: vec![worn_copy("neck", "an amulet of Gorn", OWN)],
        ..Gear::default()
    };
    let plan = gear.plan_with(&the_enchanted_amulet(None), &GearBook::default());
    assert!(plan.commands.is_empty());
    assert_eq!(plan.stand_ins, ["an amulet of Gorn"]);
    assert_eq!(plan.left_alone, 1);
}

const RING: &str = "a pink ring";

fn pinned_ring(id: &str) -> GearPiece {
    pinned(RING, "finger", id)
}

fn any_ring() -> GearPiece {
    in_slot(RING, "finger")
}

fn plan_rings(worn_ids: &[&str], carried: &[&str], pieces: Vec<GearPiece>) -> WearPlan {
    let gear = Gear {
        worn: (worn_ids.iter())
            .map(|id| worn_copy("finger", RING, id))
            .collect(),
        carried: carried.iter().map(|id| copy(RING, id)).collect(),
        ..Gear::default()
    };
    gear.plan_with(&set("Rings", pieces, None), &GearBook::default())
}

/// The order a set lists its pieces in decides nothing.
#[test]
fn a_piece_already_on_stays_on_wherever_the_set_lists_it() {
    for pieces in [
        vec![pinned_ring("1"), pinned_ring("2")],
        vec![pinned_ring("2"), pinned_ring("1")],
    ] {
        let plan = plan_rings(&["2"], &["1"], pieces);
        assert_eq!(plan.commands, ["wear 1"]);
        assert_eq!(plan.left_alone, 1);
        assert!(plan.missing.is_empty() && plan.stand_ins.is_empty());
    }
}

#[test]
fn a_chosen_copy_stays_on_and_the_unchosen_one_comes_off() {
    let plan = plan_rings(
        &["2", "3"],
        &["1"],
        vec![pinned_ring("1"), pinned_ring("2")],
    );
    assert_eq!(plan.commands, ["remove 3", "wear 1"]);
    assert!(plan.stand_ins.is_empty());
}

/// One ring can't be two pieces, but the chosen one is the one that's on.
#[test]
fn a_chosen_copy_on_is_never_claimed_by_a_piece_of_any_copy() {
    let plan = plan_rings(&["2"], &[], vec![any_ring(), pinned_ring("2")]);
    assert_eq!(plan.left_alone, 1);
    assert_eq!(plan.missing, [RING], "the piece of any copy");
    assert!(plan.commands.is_empty());
}

#[test]
fn a_piece_of_any_copy_stays_on_while_the_chosen_copy_joins_it() {
    let plan = plan_rings(&["2"], &["1"], vec![any_ring(), pinned_ring("1")]);
    assert_eq!(plan.commands, ["wear 1"]);
    assert_eq!(plan.left_alone, 1);
    assert!(plan.missing.is_empty());
}

#[test]
fn a_piece_of_any_copy_leaves_the_chosen_copy_for_its_own_piece() {
    let plan = plan_rings(&[], &["1", "3"], vec![any_ring(), pinned_ring("3")]);
    assert_eq!(plan.commands, ["wear 1", "wear 3"]);

    let plan = plan_rings(&[], &["3"], vec![any_ring(), pinned_ring("3")]);
    assert_eq!(plan.commands, ["wear 3"]);
    assert_eq!(plan.missing, [RING], "the piece of any copy");
}

/// A worn copy whose id isn't known may well be the chosen one.
#[test]
fn a_worn_copy_of_unknown_id_may_be_the_chosen_one() {
    let gear = Gear {
        worn: vec![worn("neck", "an amulet of Gorn")],
        ..Gear::default()
    };
    let plan = gear.plan_with(&the_enchanted_amulet(None), &GearBook::default());
    assert!(plan.commands.is_empty());
    assert!(plan.stand_ins.is_empty());
    assert_eq!(plan.left_alone, 1);
}

/// An id that isn't one word can't go in a command, and words would answer as
/// any copy of the name.
#[test]
fn a_chosen_copy_that_cant_be_named_exactly_stays_off() {
    let copy_id = "1;quit";
    let gear = Gear {
        carried: vec![copy("an amulet of Gorn", copy_id)],
        ..Gear::default()
    };
    let wanted = set(
        "Hit",
        vec![pinned("an amulet of Gorn", "neck", copy_id)],
        None,
    );
    let plan = gear.plan_with(&wanted, &GearBook::default());
    assert!(plan.commands.is_empty());
    assert_eq!(plan.missing, ["an amulet of Gorn"]);
}

#[test]
fn a_bag_carried_is_not_a_piece_to_wear() {
    let gear = Gear {
        carried: vec![copy("a satchel", SATCHEL)],
        bags: vec![satchel_with(Vec::new())],
        ..Gear::default()
    };
    let wanted = set("Odd", vec![piece("a satchel")], None);
    assert_eq!(gear.plan(&wanted).missing, ["a satchel"]);
}

#[test]
fn the_plugin_names_the_verbs_and_the_slots_they_fill() {
    let mut rules = rules();
    let verbs = [("wear", "don"), ("hold", "grasp"), ("remove", "doff")];
    let verbs = verbs.map(|(verb, word)| (verb.to_owned(), word.to_owned()));
    rules.use_words(BTreeMap::from(verbs), Keywords::default());
    let gear = Gear {
        worn: vec![worn("held", "a kite")],
        carried: vec![copy("a golden feather", "7"), copy("a linen robe", "8")],
        ..Gear::default()
    };
    let pieces = vec![
        in_slot("a golden feather", "held"),
        in_slot("a linen robe", "about"),
    ];
    let plan = plan(
        &set("Both", pieces, None),
        gear.now(),
        &GearBook::default(),
        &rules,
    );
    assert_eq!(plan.commands, ["doff kite", "grasp 7", "don 8"]);
}

#[test]
fn a_game_that_says_nothing_of_its_slots_holds_one_and_wears() {
    let rules = GearRules::default();
    assert_eq!(rules.capacity("neck"), 1);
    assert_eq!(rules.filling(Some("held")), "wear");
}

#[test]
fn candidates_are_each_copy_where_it_is_now() {
    let gear = Gear {
        worn: vec![worn("neck", "(Glowing) a small necklace"), worn("legs", "")],
        carried: vec![copy("a satchel", SATCHEL), copy("a linen robe", "1")],
        bags: vec![satchel_with(vec![copy("a linen robe", "2")])],
    };
    let found = candidates(gear.now());
    let names: Vec<(&str, Option<&str>)> = (found.iter())
        .map(|found| (found.name.as_str(), found.copy.as_deref()))
        .collect();
    assert_eq!(
        names,
        [
            ("a small necklace", None),
            ("a linen robe", Some("1")),
            ("a linen robe", Some("2")),
        ]
    );
    assert_eq!(found[0].worn_in.as_deref(), Some("neck"));
    assert_eq!(found[2].bag.as_deref(), Some("a satchel"));
}

/// Two manes tick two; a pinned copy only ticks itself; what's left isn't seen.
#[test]
fn pieces_match_their_own_copy_first_then_their_name() {
    let candidate = |name: &str, copy: Option<&str>| Candidate {
        name: name.to_owned(),
        copy: copy.map(str::to_owned),
        worn_in: None,
        bag: None,
    };
    let found = [
        candidate("a lion's mane", Some("1")),
        candidate("a lion's mane", Some("2")),
        candidate("an amulet of Gorn", Some(OWN)),
        candidate("a small necklace", None),
    ];
    let pieces = [
        piece("a lion's mane"),
        pinned("a lion's mane", "wrist", "2"),
        pinned("an amulet of Gorn", "neck", ENCHANTED),
        pinned("a small necklace", "neck", "9"),
        piece("a linen robe"),
    ];
    assert_eq!(
        match_pieces(&pieces, &found),
        [Some(0), Some(1), None, Some(3), None]
    );
}

#[test]
fn what_comes_off_goes_into_the_container_while_the_set_goes_on() {
    let pieces = leveling().pieces.into_iter().map(|piece| piece.item);
    let mut away = PutAway::new(
        SATCHEL.to_owned(),
        "a satchel".to_owned(),
        pieces,
        Vec::new(),
        NOW,
    );
    let later = |ms| Timestamp(NOW.0 + ms);
    assert_eq!(
        away.came_off("a white healer's baldric", later(2_000)),
        Some(SATCHEL)
    );
    assert_eq!(away.came_off("(Glowing) a linen robe", later(3_000)), None);
    assert_eq!(
        away.came_off("a tattered kite shield", later(60_000)),
        None,
        "long over"
    );
}

#[test]
fn the_window_stays_open_while_pieces_keep_coming_off() {
    let mut away = PutAway::new("bag".to_owned(), "a bag".to_owned(), [], [], NOW);
    let later = |ms| Timestamp(NOW.0 + ms);
    assert!(away.came_off("a rope", later(14_000)).is_some());
    assert!(away.came_off("a lamp", later(28_000)).is_some());
}

/// What the plan put away isn't put away twice as it comes off, and a put that
/// fails because the item hadn't come off yet leaves it to its message.
#[test]
fn a_put_the_plan_sent_is_only_sent_again_if_it_failed() {
    let sent = ["a rope".to_owned()];
    let mut away = PutAway::new("bag".to_owned(), "a bag".to_owned(), [], sent, NOW);
    assert_eq!(away.came_off("a rope", NOW), None);
    away.was_not_carried();
    assert_eq!(away.came_off("a rope", NOW), Some("bag"));
}

#[test]
fn a_full_container_says_what_stayed_out() {
    let sent = ["a rope".to_owned(), "a lamp".to_owned()];
    let mut away = PutAway::new("bag".to_owned(), "a bag".to_owned(), [], sent, NOW);
    away.went_in("a rope");
    assert_eq!(away.did_not_fit(), Some(("a lamp".to_owned(), "a bag")));
    assert_eq!(away.did_not_fit(), None);
}
