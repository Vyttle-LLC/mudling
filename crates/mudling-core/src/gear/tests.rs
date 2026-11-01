use super::*;

const NOW: Timestamp = Timestamp(100);

fn stats(pairs: &[(&str, i64)]) -> StatMap {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect()
}

fn reading(keywords: &str, base: &[(&str, i64)]) -> Reading {
    Reading {
        keywords: keywords.to_owned(),
        base: stats(base),
        ..Reading::default()
    }
}

fn named(name: &str) -> ItemRef<'_> {
    ItemRef { name, copy: None }
}

fn copy<'a>(name: &'a str, id: &'a str) -> ItemRef<'a> {
    ItemRef {
        name,
        copy: Some(id),
    }
}

fn learn(book: &mut GearBook, reading: Reading, item: ItemRef<'_>) -> bool {
    book.learn(reading, item, NOW, &mut Batch::new())
}

#[test]
fn names_lose_their_tags_and_their_case() {
    assert_eq!(
        plain_name("(Glowing)  (Humming) A Tiamat   Mask"),
        "a tiamat mask"
    );
    assert_eq!(shown_name("(Glowing) a Tiamat mask"), "a Tiamat mask");
    assert_eq!(
        tags("(Glowing) (Humming) a Tiamat mask"),
        ["Glowing", "Humming"]
    );
}

#[test]
fn two_copies_keep_a_reading_each() {
    let mut book = GearBook::default();
    assert!(learn(
        &mut book,
        reading("mask tiamat", &[("wis", 1)]),
        copy("a Tiamat mask", "1")
    ));
    assert!(learn(
        &mut book,
        reading("mask tiamat", &[("wis", 3)]),
        copy("a Tiamat mask", "2")
    ));
    assert_eq!(book.item("a tiamat mask").unwrap().readings.len(), 2);

    let first = book.stats(copy("a Tiamat mask", "1")).unwrap();
    assert_eq!(first.effects(), stats(&[("wis", 1)]));
    let second = book.stats(copy("a Tiamat mask", "2")).unwrap();
    assert_eq!(second.effects(), stats(&[("wis", 3)]));
}

#[test]
fn a_reading_read_again_is_no_news() {
    let mut book = GearBook::default();
    let same = reading("mask tiamat", &[("wis", 1)]);
    assert!(learn(&mut book, same.clone(), named("a Tiamat mask")));
    assert!(!learn(&mut book, same, named("a Tiamat mask")));
}

#[test]
fn a_new_copy_gets_an_estimate_of_what_copies_showed() {
    let mut book = GearBook::default();
    let bare = "an amulet of Gorn";
    learn(&mut book, reading("gorn", &[("str", 1)]), copy(bare, "1"));
    let mut strong = reading("gorn", &[("str", 3), ("con", 1)]);
    strong.flags = vec!["magic".to_owned()];
    learn(&mut book, strong, copy(bare, "2"));

    let Some(ItemStats::Estimated(estimate)) = book.stats(copy(bare, "3")) else {
        panic!("an estimate");
    };
    assert_eq!(estimate.ranges["str"], 1..=3);
    // The copy without the stat has it at 0.
    assert_eq!(estimate.ranges["con"], 0..=1);
    assert_eq!(estimate.copies, 2);
    assert!(estimate.any_magic);
    assert!(!estimate.is_measured);
}

#[test]
fn a_magical_tag_hints_at_an_enchant_only_where_no_copy_was_magic() {
    let mut book = GearBook::default();
    learn(
        &mut book,
        reading("gorn", &[("str", 1)]),
        named("an amulet of Gorn"),
    );
    let Some(ItemStats::Estimated(estimate)) = book.stats(named("(Magical) an amulet of Gorn"))
    else {
        panic!("an estimate");
    };
    assert!(estimate.likely_enchanted);
}

#[test]
fn enchants_count_for_their_copy_and_never_in_an_estimate() {
    let mut book = GearBook::default();
    let mut enchanted = reading("gorn", &[("ac", -1)]);
    enchanted.added = BTreeMap::from([("enchant armor".to_owned(), stats(&[("ac", -4)]))]);
    learn(&mut book, enchanted, copy("a shield", "1"));

    let exact = book.stats(copy("a shield", "1")).unwrap();
    assert_eq!(exact.effects(), stats(&[("ac", -5)]));
    let other = book.stats(copy("a shield", "2")).unwrap();
    assert_eq!(other.effects(), stats(&[("ac", -1)]));
}

#[test]
fn a_measurement_never_replaces_an_identify_and_nothing_never_erases_it() {
    let mut book = GearBook::default();
    let mut batch = Batch::new();
    let item = copy("a ring", "1");
    assert!(book.learn_measured(stats(&[("hitroll", 2)]), item, NOW, &mut batch));
    assert!(!book.learn_measured(StatMap::new(), item, NOW, &mut batch));
    assert_eq!(
        book.stats(item),
        Some(ItemStats::Measured(stats(&[("hitroll", 2)])))
    );

    learn(&mut book, reading("ring", &[("hitroll", 3)]), item);
    assert!(!book.learn_measured(stats(&[("hitroll", 9)]), item, NOW, &mut batch));
    assert_eq!(
        book.stats(item).unwrap().effects(),
        stats(&[("hitroll", 3)])
    );
}

#[test]
fn a_measurement_with_no_copy_is_kept_for_the_item() {
    let mut book = GearBook::default();
    let mut batch = Batch::new();
    assert!(book.learn_measured(stats(&[("str", 1)]), named("a ring"), NOW, &mut batch));
    let Some(ItemStats::Estimated(estimate)) = book.stats(copy("a ring", "9")) else {
        panic!("an estimate");
    };
    assert!(estimate.is_measured);
    assert_eq!(estimate.ranges["str"], 1..=1);
}

#[test]
fn the_comparison_is_against_each_item_worn_and_leaves_out_the_same_item() {
    let mut book = GearBook::default();
    learn(
        &mut book,
        reading("orb", &[("int", 3), ("ac", -1)]),
        named("an orb"),
    );
    learn(&mut book, reading("tooth", &[("int", 1)]), named("a tooth"));

    let worn = [named("a tooth"), named("a stranger"), named("an orb")];
    let found = book.compare(named("an orb"), worn);
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].changes, stats(&[("ac", -1), ("int", 2)]));
    assert!(found[0].known);
    assert_eq!(found[1].changes, stats(&[("ac", -1), ("int", 3)]));
    assert!(!found[1].known);
    assert!(book.compare(named("an unknown"), worn).is_empty());
}

#[test]
fn the_book_reads_back_from_its_rows() {
    let mut book = GearBook::default();
    let mut batch = Batch::new();
    book.learn(
        reading("mask tiamat", &[("wis", 1)]),
        copy("a Tiamat mask", "1"),
        NOW,
        &mut batch,
    );
    book.learn_slot("head", "a Tiamat mask", NOW, &mut batch);
    let item = book.item("a tiamat mask").unwrap();
    let rows = GearRows {
        copies: item.copies.clone(),
        facts: vec![item.facts.clone()],
        identifications: item.readings.clone(),
    };
    let loaded = GearBook::from_rows(rows);
    assert_eq!(loaded.len(), 1);
    assert_eq!(
        loaded.stats(copy("a Tiamat mask", "1")),
        book.stats(copy("a Tiamat mask", "1"))
    );
    assert_eq!(loaded.item("a tiamat mask").unwrap().name, "a Tiamat mask");
    assert_eq!(
        loaded.item("a tiamat mask").unwrap().facts.slot.as_deref(),
        Some("head")
    );
}

#[test]
fn a_command_prefers_the_games_first_keyword() {
    let mut book = GearBook::default();
    book.note_word("mask", "a Tiamat mask");
    let word = |book: &GearBook| {
        let item = book.item("a tiamat mask").unwrap();
        item.command_word().map(str::to_owned)
    };
    assert_eq!(word(&book).as_deref(), Some("mask"));
    book.learn_keywords("tiamat mask", "a Tiamat mask", NOW, &mut Batch::new());
    assert_eq!(word(&book).as_deref(), Some("tiamat"));
}
