//! The character's tables: score, practice (across the pager), slist
//! (wrapped rows) and aff, from scrubbed fixtures.

use mudling_core::plugin::luau::Value;
use mudling_plugin_avatar::Transcript;
use std::collections::HashMap;

fn read(fixture: &str) -> Transcript {
    let transcript = Transcript::of(fixture);
    assert!(
        transcript.problems().is_empty(),
        "{:?}",
        transcript.problems()
    );
    transcript
}

fn number(value: f64) -> Value {
    Value::Number(value)
}

fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

fn get<'a>(store: &'a HashMap<String, Value>, key: &str) -> &'a Value {
    store
        .get(key)
        .unwrap_or_else(|| panic!("no `{key}` in {store:?}"))
}

#[test]
fn score_fills_the_character() {
    let score = read(include_str!("fixtures/score.txt")).store("character");
    let expected = [
        ("title", text("the Quiet Attendant")),
        ("level", number(12.0)),
        ("race", text("Harpy")),
        ("class", text("Monk")),
        ("age", number(19.0)),
        ("str", number(15.0)),
        ("str_perm", number(18.0)),
        ("con", number(19.0)),
        ("con_perm", number(21.0)),
        ("gold", number(262.0)),
        ("bank", number(500.0)),
        ("killed", number(95.0)),
        ("died", number(3.0)),
        ("arrests", number(0.0)),
        ("wimpy", number(0.0)),
        ("practice", number(4.0)),
        ("incarnation", number(1.0)),
    ];
    for (key, value) in expected {
        assert_eq!(get(&score, key), &value, "{key}");
    }
    assert_eq!(get(&score, "score.read"), &number(12.0), "the rows read");
}

#[test]
fn a_score_line_outside_score_writes_nothing() {
    let transcript = read("(1h-1m-1v-1)(-/-)\nLevel 3 Elf Mage\nGold   :   10\n(1h-1m-1v-1)(-/-)");
    assert_eq!(transcript.writes().count(), 0);
}

#[test]
fn practice_reads_on_past_the_pager() {
    let transcript = read(include_str!("fixtures/practice-long.txt"));
    // The second arrives with the next page glued on, as in a log.
    assert_eq!(
        transcript.pager_prompts, 1,
        "the pager prompt recognized, for the client to answer"
    );
    let skills = transcript.store("skills");
    assert_eq!(get(&skills, "practice.sessions"), &number(12.0));
    assert_eq!(
        get(&skills, "practice.read"),
        &number(16.0),
        "every cell, both pages"
    );
    for (skill, learned, level, mana) in [
        ("anticipate", 0.0, 51.0, 15.0),
        ("armor lore", 0.0, 5.0, 0.0),
        ("endurance", 0.0, 21.0, 100.0),
        ("haggle", 0.0, 51.0, 0.0),
        ("heighten senses", 90.0, 19.0, 0.0),
        ("iron monk", 90.0, 30.0, 75.0),
        // Glued after the second pager prompt.
        ("weaken", 0.0, 51.0, 20.0),
        ("weapon lore", 90.0, 35.0, 0.0),
    ] {
        assert_eq!(
            get(&skills, &format!("{skill}.learned")),
            &number(learned),
            "{skill}"
        );
        assert_eq!(
            get(&skills, &format!("{skill}.level")),
            &number(level),
            "{skill}"
        );
        assert_eq!(
            get(&skills, &format!("{skill}.mana")),
            &number(mana),
            "{skill}"
        );
    }
}

#[test]
fn practice_left_at_its_pager_is_not_sent() {
    let transcript = read(include_str!("fixtures/practice-cut.txt"));
    let skills = transcript.store("skills");
    assert!(
        skills.is_empty(),
        "a table cut short sends nothing: {skills:?}"
    );
    assert_eq!(
        transcript.events("gear.equipment.start", &[]).len(),
        1,
        "the next table still starts"
    );
}

#[test]
fn a_last_page_without_rows_still_answers_the_pager() {
    let transcript = read(include_str!("fixtures/practice-last-page-empty.txt"));
    assert_eq!(transcript.pager_prompts, 1);
    let skills = transcript.store("skills");
    assert_eq!(get(&skills, "practice.read"), &number(4.0));
    assert_eq!(get(&skills, "concentrate.learned"), &number(90.0));

    // Score's facts all on its first page, only its tail on the second.
    let score = read(include_str!("fixtures/score-paged.txt")).store("character");
    assert_eq!(get(&score, "level"), &number(12.0));
    assert_eq!(get(&score, "incarnation"), &number(1.0));
    assert_eq!(get(&score, "score.read"), &number(12.0));
}

#[test]
fn the_next_table_ends_one_whose_prompt_is_unknown() {
    let transcript = read(
        "You are using:\n<worn on head>      a Tiamat mask\nhp:46 >\n\
         You have 1 practice sessions left.\ndodge               95   4   -    0\nhp:46 >",
    );
    assert_eq!(get(&transcript.store("equipment"), "count"), &number(1.0));
    assert!(
        transcript.store("skills").is_empty(),
        "the last waits for a prompt or the next table"
    );
}

#[test]
fn practicing_at_a_trainer_sets_the_skill() {
    let transcript = read("You practice dodge with Ulric. Your current skill level is now 45.");
    assert_eq!(
        get(&transcript.store("skills"), "dodge.learned"),
        &number(45.0)
    );
}

#[test]
fn slist_cuts_fixed_columns_and_reads_wrapped_rows() {
    let skills = read(include_str!("fixtures/slist.txt")).store("skills");
    let level = |skill: &str| get(&skills, &format!("{skill}.level")).clone();
    for (skill, expected) in [
        ("open hand", 1.0),
        ("fast healing", 3.0),
        ("meditation", 3.0),
        ("inner balance", 10.0),
        ("embalm", 10.0),
        ("iron monk", 30.0),
        // A name that fills its column, after the pager.
        ("move hidden", 35.0),
        ("serpent technique", 35.0),
        ("invigorate", 35.0),
        ("rejuvenate", 50.0),
        // The wrapped row, under level 50.
        ("demonstrate", 50.0),
        ("transmogrify", 50.0),
    ] {
        assert_eq!(level(skill), number(expected), "{skill}");
    }
    assert!(
        !skills.keys().any(|key| key.contains("path")),
        "room text isn't a skill: {skills:?}"
    );
    assert_eq!(get(&skills, "slist.read"), &number(17.0));
}

#[test]
fn a_blank_line_ends_the_wrapped_rows() {
    let fixture = include_str!("fixtures/slist-blank.txt");
    let skills = read(fixture).store("skills");
    assert_eq!(
        get(&skills, "transmogrify.level"),
        &number(50.0),
        "the wrapped row before the blank"
    );
    assert!(
        !skills.contains_key("Ancient Ruin.level"),
        "column-aligned room text after a blank: {skills:?}"
    );
    assert_eq!(get(&skills, "slist.read"), &number(4.0));

    // The same text with no blank line before it is a wrapped row.
    // By lines, so a checkout with CRLF endings reads the same.
    let without_blank: Vec<&str> = fixture.lines().filter(|line| !line.is_empty()).collect();
    let skills = read(&without_blank.join("\n")).store("skills");
    assert_eq!(get(&skills, "Ancient Ruin.level"), &number(50.0));
}

#[test]
fn affects_lists_each_spell_once() {
    let affects = read(include_str!("fixtures/affects.txt")).store("affects");
    assert_eq!(get(&affects, "count"), &number(3.0));
    assert_eq!(get(&affects, "1.name"), &text("armor"));
    assert_eq!(get(&affects, "2.name"), &text("bless"));
    assert_eq!(get(&affects, "3.name"), &text("sanctuary"));
    assert_eq!(
        get(&affects, "3.detail"),
        &text("modifies none by 0 for 4 hours")
    );
    assert!(!affects.contains_key("1.detail"));
}

#[test]
fn affects_with_nothing_on_is_an_empty_list() {
    for none in [
        "(53h-114m-102v-1448)(-/-)You are not under the affects of any spells",
        "You are not affected by any spells.",
    ] {
        let affects = read(none).store("affects");
        assert_eq!(
            affects,
            HashMap::from([("count".to_owned(), number(0.0))]),
            "{none}"
        );
    }
}

#[test]
fn messages_about_skills_and_affects_are_events() {
    let transcript = read(
        "You have become better at kick!\n(1h-1m-1v-1)(-/-)You feel more frail.\nYour armor spell wears off.",
    );
    assert_eq!(transcript.events("character.skill_improved", &[]).len(), 1);
    assert_eq!(transcript.events("character.affects_changed", &[]).len(), 2);
}
