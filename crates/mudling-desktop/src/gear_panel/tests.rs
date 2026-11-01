use super::*;
use crate::measure::sample_items;
use crate::theme::Theme;
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, WindowAppearance};
use std::cell::RefCell;
use std::rc::Rc;

fn carried<'a>(rows: &'a [Row], name: &str) -> &'a Row {
    (rows.iter())
        .find(|row| row.name == name)
        .unwrap_or_else(|| panic!("no {name} in {rows:?}"))
}

#[test]
fn every_slot_shows_in_the_plugins_order_with_the_empty_ones() {
    let rows = worn_rows(&sample_items());
    let slots: Vec<&str> = rows.iter().filter_map(|row| row.slot.as_deref()).collect();
    assert_eq!(
        slots,
        ["light", "finger", "neck", "body", "head", "legs", "feet"]
    );
    assert_eq!(rows[0].name, "a bright ball of light");
    assert_eq!(rows[1].name, "Empty");
    assert!(rows[1].is_empty_slot());
    assert_eq!(rows[2].flags, ["Glowing"]);
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
    assert_eq!(rows[1].name, "a pink ring");
    assert_eq!(rows[2].name, "a jade ring");
    assert_eq!(rows[2].slot.as_deref(), Some("finger"));
}

#[test]
fn copies_are_one_row_with_a_count_and_what_of_them_is_in_use() {
    let rows = carried_rows(&sample_items(), Some(2));
    let lights = carried(&rows, "a bright ball of light");
    assert_eq!(lights.count, 5);
    assert_eq!(lights.counted_name(), "a bright ball of light ×5");
    assert_eq!(lights.flags, ["1 in use as your light"]);
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
        "a copper bracelet, Glowing, Humming, Needs level 7"
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
    assert_eq!(more, ["Wear", "Hold", "Examine"]);
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
