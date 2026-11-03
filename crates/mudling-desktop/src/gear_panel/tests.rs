use super::sets::SetAction;
use super::*;
use crate::input::CommandInput;
use crate::measure::{sample_items, sample_items_with_sets};
use crate::room_panel::press_key;
use crate::theme::Theme;
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, WindowAppearance, size};
use mudling_core::gear::{Estimate, Reading};
use mudling_core::persist::{Batch, Identification, Timestamp};
use mudling_core::plugin::luau::Value;
use mudling_core::session::GearEdit;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

fn carried<'a>(rows: &'a [Row], name: &str) -> &'a Row {
    (rows.iter())
        .find(|row| row.name == name)
        .unwrap_or_else(|| panic!("no {name} in {rows:?}"))
}

#[test]
fn whats_worn_shows_in_the_plugins_order_and_the_empty_slots_fold_into_one_line() {
    let items = sample_items();
    let rows = worn_rows(&items);
    let slots: Vec<&str> = rows.iter().filter_map(|row| row.slot.as_deref()).collect();
    assert_eq!(slots, ["light", "neck", "body"]);
    assert_eq!(rows[0].name, "a bright ball of light");
    assert_eq!(rows[1].tags, ["Glowing"]);
    assert_eq!(empty_slots(&items), ["finger", "head", "legs", "feet"]);
}

#[test]
fn a_slot_listed_with_nothing_in_it_counts_as_empty() {
    let mut items = sample_items();
    let mut worn = items.worn.to_vec();
    worn.push(WornItem {
        slot: "head".to_owned(),
        item: String::new(),
        copy: None,
    });
    items.worn = worn.into();
    assert_eq!(worn_rows(&items).len(), 3);
    assert_eq!(empty_slots(&items), ["finger", "head", "legs", "feet"]);
}

#[test]
fn a_worn_row_says_its_stats_without_where_theyre_from() {
    let rows = worn_rows(&sample_items());
    assert_eq!(rows[0].line.text, "Stats unknown");
    assert_eq!(rows[1].line.text, "AC −1");
    assert_eq!(
        rows[1].label(),
        "neck, an amulet of the moon, Glowing, AC −1"
    );
}

fn reading(base: &[(&str, i64)], added: &[(&str, i64)]) -> ItemStats {
    let stats = |pairs: &[(&str, i64)]| pairs.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect();
    ItemStats::Identified(Box::new(Identification {
        id: "1".to_owned(),
        item_name: "a ring".to_owned(),
        copy_id: None,
        keywords: "ring".to_owned(),
        item_type: "armor".to_owned(),
        flags: Vec::new(),
        level: None,
        quality: None,
        base: stats(base),
        added: [("enchant".to_owned(), stats(added))].into(),
        details: Vec::new(),
        updated: Timestamp(0),
    }))
}

#[test]
fn the_worn_total_adds_up_what_is_known_and_counts_what_isnt() {
    let identified = reading(&[("str", 1), ("ac", -2)], &[("ac", -1)]);
    let measured = ItemStats::Measured([("hitroll".to_owned(), 2)].into());
    let estimated = ItemStats::Estimated(Estimate {
        ranges: [
            ("damroll".to_owned(), 4..=6),
            ("str".to_owned(), 2..=2),
            ("ac".to_owned(), -1..=-1),
        ]
        .into(),
        copies: 2,
        is_measured: false,
        any_magic: false,
        likely_enchanted: false,
    });
    let worn = [
        Some(identified),
        Some(measured),
        Some(estimated),
        None,
        None,
    ];
    assert_eq!(
        worn_total(worn).unwrap(),
        "Worn total · Str +3 · Hit +2 · Dam +4 to +6 · AC −4 · 2 unknown"
    );
    let cancelled = [Some(reading(&[("str", 1)], &[("str", -1)]))];
    assert_eq!(
        worn_total(cancelled).unwrap(),
        "Worn total · No stat changes"
    );
    assert_eq!(worn_total([]), None, "nothing worn, no line");
}

#[test]
fn a_slot_the_plugin_doesnt_name_still_shows_what_it_holds() {
    let mut items = sample_items();
    let mut worn = items.worn.to_vec();
    worn.push(WornItem {
        slot: "tail".to_owned(),
        item: "a bow".to_owned(),
        copy: None,
    });
    items.worn = worn.into();
    let rows = worn_rows(&items);
    assert_eq!(rows.last().unwrap().name, "a bow");
}

#[test]
fn a_second_ring_shows_as_a_second_row_of_its_slot() {
    let mut items = sample_items();
    let ring = |item: &str| WornItem {
        slot: "finger".to_owned(),
        item: item.to_owned(),
        copy: None,
    };
    items.worn = vec![ring("a pink ring"), ring("a jade ring")].into();
    let rows = worn_rows(&items);
    assert_eq!(rows[0].name, "a pink ring");
    assert_eq!(rows[1].name, "a jade ring");
    assert_eq!(rows[1].slot.as_deref(), Some("finger"));
}

#[test]
fn copies_are_one_row_with_a_count_and_what_of_them_is_in_use() {
    let rows = carried_rows(&sample_items(), Some(2));
    let lights = carried(&rows, "a bright ball of light");
    assert_eq!(lights.count, 5);
    assert_eq!(lights.counted_name(), "a bright ball of light ×5");
    assert_eq!(lights.line.text, "Stats unknown · 1 worn");
}

#[test]
fn a_level_lock_shows_and_takes_the_first_action_away() {
    let rows = carried_rows(&sample_items(), Some(2));
    let bracelet = carried(&rows, "a copper bracelet");
    assert_eq!(bracelet.needs_level, Some(7));
    assert_eq!(bracelet.primary, None);
    assert_eq!(bracelet.more[0].name, "Wear");
    assert_eq!(
        bracelet.label(),
        "a copper bracelet, Glowing, Humming, Needs level 7, AC −2 → wrist empty"
    );

    let rows = carried_rows(&sample_items(), Some(7));
    let bracelet = carried(&rows, "a copper bracelet");
    assert_eq!(bracelet.needs_level, None);
    assert_eq!(
        bracelet.primary.as_ref().map(|action| action.name),
        Some("Wear")
    );
}

#[test]
fn an_unknown_level_locks_nothing() {
    let rows = carried_rows(&sample_items(), None);
    assert_eq!(carried(&rows, "a copper bracelet").needs_level, None);
}

#[test]
fn the_action_follows_the_slot_and_an_unknown_one_offers_none_at_first() {
    let rows = carried_rows(&sample_items(), Some(2));
    let primary = |name: &str| {
        carried(&rows, name)
            .primary
            .as_ref()
            .map(|action| action.name)
    };
    assert_eq!(primary("a small necklace"), Some("Wear"));
    assert_eq!(primary("an alligator tooth"), Some("Hold"));
    assert_eq!(primary("a small boat"), None);
    let boat = carried(&rows, "a small boat");
    let more: Vec<&str> = boat.more.iter().map(|action| action.name).collect();
    assert_eq!(more, ["Wear", "Wield", "Hold", "Examine"]);
}

#[test]
fn what_is_carried_compares_with_what_is_worn_in_its_slot() {
    let rows = carried_rows(&sample_items(), Some(2));
    let necklace = carried(&rows, "a small necklace");
    assert_eq!(
        necklace.known[1],
        "Instead of an amulet of the moon: Wis +1"
    );
    let tooth = carried(&rows, "an alligator tooth");
    assert_eq!(tooth.known.len(), 1, "nothing's held to compare with");
}

/// The marked words of `row`'s second line, each with whether it's a gain.
fn marks(row: &Row) -> Vec<(&str, bool)> {
    (row.line.marks.iter())
        .map(|(range, better)| (&row.line.text[range.clone()], *better))
        .collect()
}

#[test]
fn a_carried_row_says_what_wearing_it_would_change_or_that_its_slot_is_empty() {
    let mut items = sample_items();
    let rows = carried_rows(&items, Some(2));
    let necklace = carried(&rows, "a small necklace");
    assert_eq!(necklace.line.text, "Wis +1 · AC −1 → Wis +1");
    assert_eq!(marks(necklace), [("Wis +1", true)]);
    let tooth = carried(&rows, "an alligator tooth");
    assert_eq!(tooth.line.text, "Hit +1 → held empty");
    assert_eq!(carried(&rows, "a small boat").line.text, "Stats unknown");

    let mut worn = items.worn.to_vec();
    worn.push(WornItem {
        slot: "held".to_owned(),
        item: "(Glowing) an alligator tooth".to_owned(),
        copy: None,
    });
    items.worn = worn.into();
    let rows = carried_rows(&items, Some(2));
    let orb = carried(&rows, "orb of knowledge");
    assert_eq!(orb.line.text, "Int +2 → Int +2 Hit −1");
    assert_eq!(marks(orb), [("Int +2", true), ("Hit −1", false)]);
    assert!(orb.label().ends_with("Int +2 → Int +2 Hit −1"));
    let tooth = carried(&rows, "an alligator tooth");
    assert_eq!(tooth.line.text, "Hit +1 · 1 worn");
}

/// A stone charm, AC −3, against the sample's amulet, AC −1, on the neck.
#[test]
fn armor_going_down_is_a_gain() {
    let mut items = sample_items();
    let (now, mut batch) = (Timestamp(0), Batch::new());
    let mut book = (*items.book).clone();
    let charm = ItemRef {
        name: "a stone charm",
        copy: None,
    };
    let reading = Reading {
        keywords: "charm".to_owned(),
        base: [("ac".to_owned(), -3)].into(),
        ..Reading::default()
    };
    book.learn(reading, charm, now, &mut batch);
    book.learn_slot("neck", charm.name, now, &mut batch);
    items.book = Arc::new(book);
    items.carried = vec![CarriedItem {
        id: "30".to_owned(),
        name: charm.name.to_owned(),
    }]
    .into();
    let rows = carried_rows(&items, Some(2));
    assert_eq!(rows[0].line.text, "AC −3 → AC −2 (better)");
    assert_eq!(marks(&rows[0]), [("AC −2 (better)", true)]);
}

#[test]
fn actions_name_the_copy_so_two_of_a_name_can_be_told_apart() {
    let rows = carried_rows(&sample_items(), Some(2));
    let necklace = carried(&rows, "a small necklace");
    assert_eq!(necklace.primary.as_ref().unwrap().target, "2");
}

fn panel(cx: &mut TestAppContext) -> (Entity<GearPanel>, &mut VisualTestContext) {
    cx.update(|cx| cx.set_global(Theme::for_appearance(WindowAppearance::Dark)));
    let (panel, cx) = cx.add_window_view(|_, cx| {
        let mut panel = GearPanel::new(Verbs::default(), cx);
        panel.show_items(sample_items(), cx);
        panel.show_character(&CharactersState::default(), cx);
        panel.view = View::Carrying;
        panel
    });
    cx.run_until_parked();
    (panel, cx)
}

fn sent(panel: &Entity<GearPanel>, cx: &mut VisualTestContext) -> Rc<RefCell<Vec<String>>> {
    let sent = Rc::new(RefCell::new(Vec::new()));
    let hears = Rc::clone(&sent);
    cx.update(|_, cx| {
        cx.subscribe(panel, move |_, event: &SendCommand, _| {
            hears.borrow_mut().push(event.command.clone());
        })
        .detach();
    });
    sent
}

fn click_on(name: &str, cx: &mut VisualTestContext) {
    let bounds = cx
        .debug_bounds(Box::leak(name.to_owned().into_boxed_str()))
        .unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
}

#[gpui::test]
fn a_button_sends_through_the_shell_and_never_while_echo_is_off(cx: &mut TestAppContext) {
    let (panel, cx) = panel(cx);
    let sent = sent(&panel, cx);
    click_on("Wear a small necklace", cx);
    assert_eq!(*sent.borrow(), ["wear 2"]);

    panel.update(cx, |panel, cx| panel.set_locked(true, cx));
    cx.run_until_parked();
    click_on("Wear a small necklace", cx);
    assert_eq!(sent.borrow().len(), 1);
    assert!(panel.read_with(cx, |panel, _| panel.notes().contains(&LOCKED_NOTE)));
}

fn at_level(level: f64) -> CharactersState {
    CharactersState(BTreeMap::from([(
        CHARACTER_LEVEL.to_owned(),
        Value::Number(level),
    )]))
}

#[gpui::test]
fn a_locked_row_shows_its_level_and_no_first_action(cx: &mut TestAppContext) {
    let (panel, cx) = panel(cx);
    panel.update(cx, |panel, cx| panel.show_character(&at_level(2.), cx));
    cx.run_until_parked();
    let index = panel.read_with(cx, |panel, _| {
        (panel.carried.iter())
            .position(|row| row.name == "a copper bracelet")
            .unwrap()
    });
    let lock = format!("gear-lock-{index}");
    assert!(
        cx.debug_bounds(Box::leak(lock.clone().into_boxed_str()))
            .is_some()
    );
    assert!(cx.debug_bounds("Wear a copper bracelet").is_none());

    panel.update(cx, |panel, cx| panel.show_character(&at_level(7.), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds(Box::leak(lock.into_boxed_str())).is_none());
    assert!(cx.debug_bounds("Wear a copper bracelet").is_some());
}

#[gpui::test]
fn the_worn_view_adds_up_whats_worn_and_lists_the_empty_slots(cx: &mut TestAppContext) {
    let (panel, cx) = panel(cx);
    panel.update(cx, |panel, cx| panel.show_worn(cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("gear-total").is_some());
    assert!(cx.debug_bounds("gear-empty-slots").is_some());
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.total.clone()).unwrap(),
        "Worn total · AC −1 · 2 unknown"
    );

    panel.update(cx, |panel, cx| panel.show_carrying(cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("gear-total").is_none());
    assert!(cx.debug_bounds("gear-empty-slots").is_none());
}

/// The panel on its sets, in the Worn view.
fn panel_with_sets(cx: &mut TestAppContext) -> (Entity<GearPanel>, &mut VisualTestContext) {
    cx.update(|cx| {
        cx.set_global(Theme::for_appearance(WindowAppearance::Dark));
        bind_keys(cx);
    });
    let (panel, cx) = cx.add_window_view(|_, cx| {
        let mut panel = GearPanel::new(Verbs::default(), cx);
        panel.show_items(sample_items_with_sets(), cx);
        panel
    });
    // Tall enough for a whole editor: a click needs its button in view.
    cx.simulate_resize(size(px(800.), px(2000.)));
    cx.run_until_parked();
    (panel, cx)
}

fn acted(panel: &Entity<GearPanel>, cx: &mut VisualTestContext) -> Rc<RefCell<Vec<SetAction>>> {
    let acted = Rc::new(RefCell::new(Vec::new()));
    let hears = Rc::clone(&acted);
    cx.update(|_, cx| {
        cx.subscribe(panel, move |_, event: &SetAction, _| {
            hears.borrow_mut().push(event.clone());
        })
        .detach();
    });
    acted
}

fn type_in(field: &Entity<CommandInput>, text: &str, cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        field.update(cx, |field, cx| {
            field.show(text.to_owned(), text.len(), window, cx);
        });
    });
    cx.run_until_parked();
}

fn is_editing(panel: &Entity<GearPanel>, cx: &mut VisualTestContext) -> bool {
    panel.read_with(cx, |panel, _| panel.editor.is_some())
}

#[gpui::test]
fn wear_goes_to_the_shell_by_set_and_never_while_echo_is_off(cx: &mut TestAppContext) {
    let (panel, cx) = panel_with_sets(cx);
    let acted = acted(&panel, cx);
    click_on("Wear Levelling", cx);
    assert_eq!(
        *acted.borrow(),
        [SetAction::Wear {
            character: "tamsin".to_owned(),
            id: "1".to_owned(),
            by_keyboard: false,
        }]
    );

    panel.update(cx, |panel, cx| panel.set_locked(true, cx));
    cx.run_until_parked();
    click_on("Wear Levelling", cx);
    assert_eq!(acted.borrow().len(), 1);
}

#[gpui::test]
fn the_sets_stay_in_the_worn_view(cx: &mut TestAppContext) {
    let (panel, cx) = panel_with_sets(cx);
    assert!(cx.debug_bounds("New set").is_some());
    panel.update(cx, |panel, cx| panel.show_carrying(cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("New set").is_none());
}

#[gpui::test]
fn no_set_can_be_made_until_the_game_says_who_plays(cx: &mut TestAppContext) {
    let (panel, cx) = panel_with_sets(cx);
    panel.update(cx, |panel, cx| {
        panel.show_items(sample_items(), cx);
    });
    cx.run_until_parked();
    click_on("New set", cx);
    assert!(!is_editing(&panel, cx));
}

#[gpui::test]
fn a_new_set_starts_as_whats_worn_and_saves_for_its_character(cx: &mut TestAppContext) {
    let (panel, cx) = panel_with_sets(cx);
    let acted = acted(&panel, cx);
    click_on("New set", cx);
    assert!(is_editing(&panel, cx));
    click_on("Save", cx);
    assert!(is_editing(&panel, cx), "a set needs a name");
    assert!(acted.borrow().is_empty());

    let (name, _) = panel.read_with(cx, |panel, _| panel.editor.as_ref().unwrap().fields());
    type_in(&name, "Hit", cx);
    click_on("Save", cx);
    assert!(!is_editing(&panel, cx));
    let acted = acted.borrow();
    let [SetAction::Edit(GearEdit::SaveSet(set))] = acted.as_slice() else {
        panic!("{acted:?}");
    };
    assert_eq!(set.id, "", "the core gives it an id");
    assert_eq!(set.name, "Hit");
    assert_eq!(set.character_id, "tamsin");
    assert_eq!(set.container.as_deref(), Some("a canvas backpack"));
    let robe = set.pieces.iter().find(|piece| piece.item == "a linen robe");
    let robe = robe.unwrap();
    assert_eq!(robe.slot.as_deref(), Some("body"));
    assert_eq!(robe.copy_id.as_deref(), Some("1204883"));
}

#[gpui::test]
fn the_editor_lists_a_piece_not_seen_with_drop_and_previews_the_plan(cx: &mut TestAppContext) {
    let (panel, cx) = panel_with_sets(cx);
    click_on("Edit Tank", cx);
    let tooth = |panel: &Entity<GearPanel>, cx: &mut VisualTestContext| {
        panel.read_with(cx, |panel, _| {
            let pieces = &panel.editor.as_ref().unwrap().pieces();
            pieces
                .iter()
                .any(|piece| piece.item == "a cracked alligator tooth")
        })
    };
    assert!(tooth(&panel, cx));
    assert!(cx.debug_bounds("gear-preview-line-0").is_some());
    click_on("Drop a cracked alligator tooth", cx);
    assert!(!tooth(&panel, cx));
}

#[gpui::test]
fn the_preview_is_what_wear_would_send(cx: &mut TestAppContext) {
    let (panel, cx) = panel_with_sets(cx);
    click_on("Edit Levelling", cx);
    let (shown, planned) = panel.read_with(cx, |panel, _| {
        let set = panel.items.sets[0].clone();
        (
            panel.editor.as_ref().unwrap().preview().clone(),
            panel.items.plan(&set),
        )
    });
    assert_eq!(shown, planned);
    assert_eq!(
        shown.commands,
        [
            "remove 1204883",
            "get 998127 20",
            "get 21 20",
            "get 22 20",
            "put 1204883 20",
            "wear 998127",
            "wear 21",
            "wear 22",
        ]
    );
    assert_eq!(shown.left_alone, 1);
}

#[gpui::test]
fn an_exact_copy_can_be_pinned_and_let_go(cx: &mut TestAppContext) {
    let (panel, cx) = panel_with_sets(cx);
    click_on("Edit Levelling", cx);
    let copy = |panel: &Entity<GearPanel>, cx: &mut VisualTestContext| {
        panel.read_with(cx, |panel, _| {
            panel.editor.as_ref().unwrap().pieces()[0].copy_id.clone()
        })
    };
    assert_eq!(copy(&panel, cx).as_deref(), Some("998127"));
    click_on("Exact copy of a black hematite ring", cx);
    assert_eq!(copy(&panel, cx), None);
    click_on("Exact copy of a black hematite ring", cx);
    assert_eq!(copy(&panel, cx).as_deref(), Some("998127"));
}

#[gpui::test]
fn words_for_an_item_go_to_the_shell(cx: &mut TestAppContext) {
    let (panel, cx) = panel_with_sets(cx);
    let acted = acted(&panel, cx);
    click_on("Edit Levelling", cx);
    click_on("Words for a black hematite ring", cx);
    let (_, words) = panel.read_with(cx, |panel, _| panel.editor.as_ref().unwrap().fields());
    type_in(&words, "ring hematite", cx);
    click_on("Save words", cx);
    assert_eq!(
        *acted.borrow(),
        [SetAction::Edit(GearEdit::SetKeywords {
            item: "a black hematite ring".to_owned(),
            words: "ring hematite".to_owned(),
        })]
    );
}

#[gpui::test]
fn the_container_can_be_chosen_or_kept_carried(cx: &mut TestAppContext) {
    let (panel, cx) = panel_with_sets(cx);
    click_on("Edit Levelling", cx);
    click_on("What comes off goes in: a canvas backpack", cx);
    click_on("Nothing: keep it carried", cx);
    let chosen = panel.read_with(cx, |panel, _| {
        panel
            .editor
            .as_ref()
            .unwrap()
            .container()
            .map(str::to_owned)
    });
    assert_eq!(chosen, None);
    assert!(
        cx.debug_bounds("What comes off goes in: Nothing: keep it carried")
            .is_some()
    );
}

#[gpui::test]
fn use_what_im_wearing_now_replaces_the_pieces(cx: &mut TestAppContext) {
    let (panel, cx) = panel_with_sets(cx);
    click_on("Edit Tank", cx);
    click_on("Use what I'm wearing now", cx);
    let names = panel.read_with(cx, |panel, _| {
        let pieces = panel.editor.as_ref().unwrap().pieces();
        pieces
            .iter()
            .map(|piece| piece.item.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        names,
        [
            "a bright ball of light",
            "an amulet of the moon",
            "a linen robe"
        ]
    );
}

#[gpui::test]
fn delete_asks_first(cx: &mut TestAppContext) {
    let (panel, cx) = panel_with_sets(cx);
    let acted = acted(&panel, cx);
    click_on("Edit Tank", cx);
    click_on("Delete set", cx);
    assert!(acted.borrow().is_empty());
    click_on("Keep it", cx);
    assert!(is_editing(&panel, cx));
    assert!(acted.borrow().is_empty());

    click_on("Delete set", cx);
    click_on("Delete", cx);
    assert!(!is_editing(&panel, cx));
    assert_eq!(
        *acted.borrow(),
        [SetAction::Edit(GearEdit::RemoveSet {
            character: "tamsin".to_owned(),
            id: "2".to_owned(),
        })]
    );
}

#[gpui::test]
fn a_new_set_has_nothing_to_delete(cx: &mut TestAppContext) {
    let (_panel, cx) = panel_with_sets(cx);
    click_on("New set", cx);
    assert!(cx.debug_bounds("Delete set").is_none());
}

#[gpui::test]
fn cancel_and_escape_leave_without_saving(cx: &mut TestAppContext) {
    let (panel, cx) = panel_with_sets(cx);
    let acted = acted(&panel, cx);
    click_on("Edit Tank", cx);
    click_on("Cancel", cx);
    assert!(!is_editing(&panel, cx));
    click_on("Edit Tank", cx);
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!is_editing(&panel, cx));
    assert!(acted.borrow().is_empty());
}

#[gpui::test]
fn the_editor_closes_when_its_character_goes(cx: &mut TestAppContext) {
    let (panel, cx) = panel_with_sets(cx);
    click_on("Edit Tank", cx);
    panel.update(cx, |panel, cx| {
        let items = ItemsState {
            character: "ash".into(),
            sets: Vec::new().into(),
            ..sample_items_with_sets()
        };
        panel.show_items(items, cx);
    });
    assert!(!is_editing(&panel, cx));
}

/// Tabs on from the container's button to the first bag choice, and presses it.
#[gpui::test]
fn choosing_a_bag_by_keyboard_leaves_focus_on_the_container_button(cx: &mut TestAppContext) {
    let (panel, cx) = panel_with_sets(cx);
    click_on("Edit Levelling", cx);
    click_on("What comes off goes in: a canvas backpack", cx);
    let button = panel.read_with(cx, |panel, _| {
        panel.editor.as_ref().unwrap().container_stop().clone()
    });
    cx.update(|window, cx| {
        window.focus(&button, cx);
        window.focus_next(cx);
    });
    cx.run_until_parked();
    press_key("enter", cx);
    cx.run_until_parked();

    let chosen = panel.read_with(cx, |panel, _| {
        let editor = panel.editor.as_ref().unwrap();
        editor.container().map(str::to_owned)
    });
    assert_eq!(chosen, None, "the first choice keeps things carried");
    assert!(cx.update(|window, _| button.is_focused(window)));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!is_editing(&panel, cx), "focus is still in the editor");
}

/// Words open on the amulet, the last piece. The robe takes the vest's place
/// in the body slot, which moves the amulet up and leaves the robe last.
#[gpui::test]
fn adding_a_piece_closes_the_words_field_so_it_cant_land_on_another(cx: &mut TestAppContext) {
    let (panel, cx) = panel_with_sets(cx);
    let acted = acted(&panel, cx);
    click_on("Edit Levelling", cx);
    click_on("Words for an amulet of the moon", cx);
    click_on("Add a linen robe, worn: body", cx);

    let names = panel.read_with(cx, |panel, _| {
        let pieces = panel.editor.as_ref().unwrap().pieces();
        (pieces.iter())
            .map(|piece| piece.item.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        names,
        [
            "a black hematite ring",
            "a Tiamat mask",
            "an amulet of the moon",
            "a linen robe"
        ]
    );
    assert!(cx.debug_bounds("Save words").is_none());
    assert!(acted.borrow().is_empty());
}

#[gpui::test]
fn escape_in_the_words_field_leaves_it_and_keeps_the_edits(cx: &mut TestAppContext) {
    let (panel, cx) = panel_with_sets(cx);
    let acted = acted(&panel, cx);
    click_on("Edit Levelling", cx);
    click_on("Drop a Tiamat mask", cx);
    click_on("Words for a black hematite ring", cx);
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(is_editing(&panel, cx));
    assert!(cx.debug_bounds("Save words").is_none());
    let pieces = panel.read_with(cx, |panel, _| panel.editor.as_ref().unwrap().pieces().len());
    assert_eq!(pieces, 3, "the drop stands");
    assert!(acted.borrow().is_empty(), "no words were saved");

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(
        !is_editing(&panel, cx),
        "anywhere else it leaves the editor"
    );
}

#[gpui::test]
fn the_keyboard_reaches_the_preview_and_the_buttons_around_it(cx: &mut TestAppContext) {
    let (panel, cx) = panel_with_sets(cx);
    click_on("Edit Levelling", cx);
    let preview = panel.read_with(cx, |panel, _| {
        panel.editor.as_ref().unwrap().preview_stop().clone()
    });
    cx.update(|window, cx| {
        assert!(window.focused(cx).is_some(), "the name field takes focus");
    });
    let mut reached_preview = false;
    for _ in 0..30 {
        cx.update(|window, cx| window.focus_next(cx));
        cx.run_until_parked();
        reached_preview |= cx.update(|window, _| preview.is_focused(window));
    }
    assert!(reached_preview);
}

#[gpui::test]
fn more_opens_what_is_known_and_the_other_actions(cx: &mut TestAppContext) {
    let (panel, cx) = panel(cx);
    click_on("More about a small necklace", cx);
    assert_eq!(
        panel
            .read_with(cx, |panel, _| panel.open.clone())
            .as_deref(),
        Some("a small necklace")
    );
    let sent = sent(&panel, cx);
    click_on("Examine a small necklace", cx);
    assert_eq!(*sent.borrow(), ["examine 2"]);
    click_on("More about a small necklace", cx);
    assert_eq!(panel.read_with(cx, |panel, _| panel.open.clone()), None);
}

#[gpui::test]
fn items_the_chat_names_are_kept_newest_first_and_only_once(cx: &mut TestAppContext) {
    let (panel, cx) = panel(cx);
    panel.update(cx, |panel, cx| {
        panel.hear("You see a small necklace.", cx);
        panel.hear("And an alligator tooth, and a small boat.", cx);
        panel.hear("Your a small necklace glows.", cx);
    });
    let named = panel.read_with(cx, |panel, _| panel.named.clone());
    assert_eq!(
        named,
        ["a small necklace", "an alligator tooth"],
        "only the book's names count"
    );
}
