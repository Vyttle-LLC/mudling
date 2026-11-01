//! The Room panel: where the player is, the ways out, who's here and what's
//! on the ground.
//!
//! All of it comes from the room and mob stores. Who's here and what's on the
//! ground stay hidden until the server lists something.
//!
//! The panel never sends a command itself: it emits [`SendCommand`] and the
//! shell sends it, the same way as a typed line.

use crate::assets::HEADING_FONT;
use crate::dpad;
use crate::find_panel::marked_label;
use crate::theme::Theme;
use gpui::{
    AnyView, ClickEvent, Context, Div, ElementId, EventEmitter, FocusHandle, FontWeight, Hsla,
    Pixels, Role, SharedString, Stateful, Window, div, prelude::*, px,
};
use mudling_core::defaults::commands::CHAIN_SEPARATOR;
use mudling_core::map::{Direction, MapRoom, Mark};
use mudling_core::plugin::{Plugin, PluginId};
use mudling_core::store::{MobsState, RoomContents, RoomState};
use std::cmp::Reverse;
use std::collections::BTreeMap;

/// The key context while focus is in the panel.
pub const CONTEXT: &str = "RoomPanel";
const MAP_HEIGHT: Pixels = px(176.);
const BUTTON_HEIGHT: Pixels = px(28.);
const MARK_HEIGHT: Pixels = px(24.);
const ITEM_ROW_HEIGHT: Pixels = px(48.);
/// A room's memory can grow; the panel shows the mobs seen most recently.
const MOST_SIGHTINGS: usize = 8;

/// A way out of the room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exit {
    pub direction: Direction,
    pub closed: bool,
}

impl Exit {
    /// What moves the player. A bracketed step is speedwalk for "open the
    /// door, then go", which the session writes with the plugin's door
    /// command.
    pub fn command(self) -> String {
        if self.closed {
            format!("[{}]", self.direction.letter())
        } else {
            self.direction.name().to_owned()
        }
    }

    /// The name a screen reader reads.
    pub fn action(self) -> SharedString {
        let word = self.direction.name();
        if self.closed {
            format!("Open the door and go {word}").into()
        } else {
            format!("Go {word}").into()
        }
    }
}

/// The game's own words for the panel's verbs, from the enabled plugins'
/// Commands points. A verb no plugin names is sent as it is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Verbs(BTreeMap<String, String>);

impl Verbs {
    /// A higher layer's word for a verb wins.
    pub fn of(plugins: &[Plugin], enabled: &[PluginId]) -> Verbs {
        let mut on: Vec<&Plugin> = plugins
            .iter()
            .filter(|plugin| enabled.contains(plugin.id()))
            .collect();
        on.sort_by_key(|plugin| plugin.layer());
        Verbs(
            on.iter()
                .filter_map(|plugin| plugin.points.commands.as_ref())
                .flat_map(|commands| commands.verbs.clone())
                .collect(),
        )
    }

    /// `target` comes from the server, so it can't be trusted to be one word
    /// of one command: anything past a line or chain break is dropped.
    pub fn command(&self, verb: &str, target: &str) -> String {
        let word = self.0.get(verb).map_or(verb, String::as_str);
        let target = target
            .split(['\n', '\r', CHAIN_SEPARATOR])
            .next()
            .unwrap_or_default();
        format!("{word} {target}")
    }
}

/// The room as the panel draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Place {
    name: SharedString,
    area: SharedString,
    exits: Vec<Exit>,
    seen: Vec<Sighting>,
}

impl Place {
    fn of(room: &MapRoom, mobs: &MobsState) -> Place {
        Place {
            name: room.name.clone().into(),
            area: room.zone.clone().into(),
            exits: room
                .exits
                .iter()
                .map(|(direction, exit)| Exit {
                    direction,
                    closed: exit.closed,
                })
                .collect(),
            seen: sightings(room, mobs),
        }
    }
}

/// A mob the map remembers in this room, and how the player's marked it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Sighting {
    name: SharedString,
    marks: Vec<Mark>,
}

impl Sighting {
    fn label(&self) -> String {
        marked_label(&self.name, &self.marks)
    }
}

fn sightings(room: &MapRoom, mobs: &MobsState) -> Vec<Sighting> {
    let mut seen: Vec<_> = room.sightings.iter().collect();
    seen.sort_by_key(|(_, when)| Reverse(**when));
    seen.into_iter()
        .take(MOST_SIGHTINGS)
        .map(|(title, _)| Sighting {
            name: title.clone().into(),
            marks: mobs.marks_on(title),
        })
        .collect()
}

/// A note when the map isn't sure where the player is.
fn doubt(room: &RoomState) -> Option<&'static str> {
    if room.whereabouts.dark {
        Some("Too dark to see. Following your moves.")
    } else if room.whereabouts.unsure {
        Some("Still working out which room this is.")
    } else {
        None
    }
}

/// A command the panel wants sent. A mouse click also hands typing back to
/// the command bar; the keyboard stays in the panel.
pub struct SendCommand {
    pub command: String,
    pub by_keyboard: bool,
}

impl EventEmitter<SendCommand> for RoomPanel {}

/// The player set or cleared a mark on a mob, by the line the game shows.
pub struct MarkMob {
    pub title: String,
    pub mark: Mark,
    pub set: bool,
}

impl EventEmitter<MarkMob> for RoomPanel {}

pub struct RoomPanel {
    focus_handle: FocusHandle,
    verbs: Verbs,
    place: Option<Place>,
    doubt: Option<&'static str>,
    contents: RoomContents,
    mobs: MobsState,
    /// Each occupant's marks, worked out when the room or the marks change.
    occupant_marks: Vec<Vec<Mark>>,
    map: Option<AnyView>,
    card: Option<AnyView>,
}

impl RoomPanel {
    pub fn new(verbs: Verbs, cx: &mut Context<RoomPanel>) -> RoomPanel {
        RoomPanel {
            focus_handle: cx.focus_handle(),
            verbs,
            place: None,
            doubt: None,
            contents: RoomContents::default(),
            mobs: MobsState::default(),
            occupant_marks: Vec::new(),
            map: None,
            card: None,
        }
    }

    /// Switches to a new game's words for the verbs.
    pub fn use_verbs(&mut self, verbs: Verbs, cx: &mut Context<RoomPanel>) {
        self.verbs = verbs;
        cx.notify();
    }

    /// Takes the stores' new state. Redraws only if what's shown changed.
    pub fn show_room(&mut self, room: &RoomState, mobs: &MobsState, cx: &mut Context<RoomPanel>) {
        let place = room.room.as_deref().map(|room| Place::of(room, mobs));
        let doubt = doubt(room);
        if place != self.place || doubt != self.doubt {
            self.place = place;
            self.doubt = doubt;
            cx.notify();
        }
        if *mobs != self.mobs {
            self.mobs = mobs.clone();
            self.mark_occupants();
            cx.notify();
        }
    }

    /// Who and what is in the room, from the room store.
    pub fn show_contents(&mut self, contents: RoomContents, cx: &mut Context<RoomPanel>) {
        if contents != self.contents {
            self.contents = contents;
            self.mark_occupants();
            cx.notify();
        }
    }

    fn mark_occupants(&mut self) {
        let occupants = self.contents.occupants.iter();
        self.occupant_marks = occupants
            .map(|occupant| self.mobs.marks_on(&occupant.name))
            .collect();
    }

    /// Puts the small map in its place above the exits.
    pub fn mount_map(&mut self, map: impl Into<AnyView>, cx: &mut Context<RoomPanel>) {
        self.map = Some(map.into());
        cx.notify();
    }

    /// Puts the room's note and features in their place under the exits.
    pub fn mount_card(&mut self, card: impl Into<AnyView>, cx: &mut Context<RoomPanel>) {
        self.card = Some(card.into());
        cx.notify();
    }

    /// Moves the keyboard to the panel's first button, or to the panel itself
    /// when it has none.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<RoomPanel>) {
        window.focus(&self.focus_handle, cx);
        window.focus_next(cx);
        if !self.focus_handle.contains_focused(window, cx) {
            window.focus(&self.focus_handle, cx);
        }
    }

    #[cfg(test)]
    pub fn room_name(&self) -> Option<&str> {
        self.place.as_ref().map(|place| place.name.as_str())
    }

    /// The button pressed may be gone once the room changes, which would
    /// leave the keyboard nowhere, so focus goes back to the panel.
    pub fn send(
        &mut self,
        command: String,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<RoomPanel>,
    ) {
        let by_keyboard = event.is_keyboard();
        if by_keyboard {
            window.focus(&self.focus_handle, cx);
        }
        cx.emit(SendCommand {
            command,
            by_keyboard,
        });
    }

    fn render_heading(&self, theme: &Theme) -> impl IntoElement {
        let name = match &self.place {
            Some(place) => place.name.clone(),
            None => "Nowhere yet".into(),
        };
        let area = self
            .place
            .as_ref()
            .map(|place| place.area.clone())
            .filter(|area| !area.is_empty());
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .id("room-name")
                    .role(Role::Heading)
                    .aria_level(2)
                    .aria_label(name.clone())
                    .font_family(HEADING_FONT)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_size(px(21.))
                    .text_color(theme.text)
                    .child(name),
            )
            .children(area.map(|area| {
                div()
                    .id("room-area")
                    .role(Role::Label)
                    .aria_value(area.clone())
                    .text_color(theme.text_muted)
                    .child(area)
            }))
            .children(self.doubt.map(|doubt| {
                div()
                    .id("room-doubt")
                    .role(Role::Status)
                    .aria_label(doubt)
                    .text_size(px(12.))
                    .text_color(theme.text_muted)
                    .child(doubt)
            }))
    }

    /// The map's one place: the shell mounts its inset view here through
    /// [`RoomPanel::mount_map`].
    fn render_map(&self, theme: &Theme) -> impl IntoElement {
        div()
            .debug_selector(|| "room-map".to_owned())
            .h(MAP_HEIGHT)
            .flex_none()
            .overflow_hidden()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .children(self.map.clone())
    }

    fn render_exits(
        &self,
        exits: &[Exit],
        theme: &Theme,
        cx: &mut Context<RoomPanel>,
    ) -> impl IntoElement {
        let chips = exits.iter().map(|exit| {
            let exit = *exit;
            let word = exit.direction.name();
            button(("exit", exit.direction as usize), exit.action(), theme)
                .h(BUTTON_HEIGHT)
                .px_3()
                .gap_1()
                .text_color(theme.text)
                .on_click(cx.listener(move |panel, event, window, cx| {
                    panel.send(exit.command(), event, window, cx);
                }))
                .child(word)
                .when(exit.closed, |chip| {
                    chip.child(div().text_color(theme.text_muted).child("· closed"))
                })
        });
        div()
            .id("exits")
            .role(Role::Group)
            .aria_label("Exits")
            .flex()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .content_start()
                    .gap_2()
                    .child(section_label("EXITS", theme))
                    .children(chips.collect::<Vec<_>>()),
            )
            .child(dpad::render(exits, theme, cx))
    }

    fn render_occupants(&self, theme: &Theme, cx: &mut Context<RoomPanel>) -> impl IntoElement {
        let rows: Vec<_> = self
            .contents
            .occupants
            .iter()
            .enumerate()
            .map(|(index, occupant)| {
                let name = SharedString::from(occupant.name.clone());
                let marks = self.occupant_marks.get(index);
                let marks = marks.map_or(&[][..], Vec::as_slice);
                let verbs = div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(row_name(name.clone()))
                    .when_some(occupant.target.as_ref(), |row, target| {
                        row.child(verb_button(
                            ("look", index),
                            "Look",
                            format!("Look at {name}"),
                            self.verbs.command("look", target),
                            theme.accent,
                            theme,
                            cx,
                        ))
                        .child(verb_button(
                            ("kill", index),
                            "Kill",
                            format!("Kill {name}"),
                            self.verbs.command("kill", target),
                            theme.danger,
                            theme,
                            cx,
                        ))
                    });
                div()
                    .id(("occupant", index))
                    .debug_selector(move || format!("occupant-{index}"))
                    .role(Role::ListItem)
                    .aria_label(name.clone())
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_3()
                    .py(px(8.))
                    .child(verbs)
                    .child(mark_toggles(
                        "occupant-mark",
                        index,
                        &name,
                        marks,
                        theme,
                        cx,
                    ))
            })
            .collect();
        section(
            "IN THE ROOM".to_owned(),
            "In the room",
            div()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.surface_raised)
                .children(rows),
            theme,
        )
    }

    fn render_items(&self, theme: &Theme, cx: &mut Context<RoomPanel>) -> impl IntoElement {
        let total: u32 = self.contents.items.iter().map(|item| item.count).sum();
        let rows: Vec<_> = self
            .contents
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let name = SharedString::from(item.name.clone());
                let count = (item.count > 1).then(|| {
                    let count = format!("×{}", item.count);
                    div()
                        .id(("count", index))
                        .role(Role::Label)
                        .aria_value(count.clone())
                        .flex_none()
                        .text_size(px(12.))
                        .text_color(theme.text_muted)
                        .child(count)
                });
                div()
                    .id(("item", index))
                    .role(Role::ListItem)
                    .aria_label(name.clone())
                    .h(ITEM_ROW_HEIGHT)
                    .flex()
                    .items_center()
                    .gap_2()
                    .pl_2()
                    .pr_1()
                    .child(row_name(name.clone()))
                    .children(count)
                    .child(verb_button(
                        ("get", index),
                        "Get",
                        format!("Get {name}"),
                        self.verbs.command("get", &item.target),
                        theme.accent,
                        theme,
                        cx,
                    ))
            })
            .collect();
        section(
            format!("ON THE GROUND · {total}"),
            "On the ground",
            div().children(rows),
            theme,
        )
    }

    /// What the map remembers about the room's mobs: all a store holds on
    /// targets so far.
    fn render_seen(
        &self,
        seen: &[Sighting],
        theme: &Theme,
        cx: &mut Context<RoomPanel>,
    ) -> impl IntoElement {
        let rows: Vec<_> = seen
            .iter()
            .enumerate()
            .map(|(index, mob)| {
                div()
                    .id(("seen", index))
                    .role(Role::ListItem)
                    .aria_label(mob.label())
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .child(row_name(mob.name.clone()))
                    .child(mark_toggles(
                        "seen-mark",
                        index,
                        &mob.name,
                        &mob.marks,
                        theme,
                        cx,
                    ))
            })
            .collect();
        section(
            "SEEN HERE".to_owned(),
            "Seen here",
            div().children(rows),
            theme,
        )
    }
}

/// A toggle for each mark a mob can have, lit for the ones it has. The player
/// sees what's marked in the same place they mark it.
fn mark_toggles(
    id: &'static str,
    index: usize,
    name: &SharedString,
    marks: &[Mark],
    theme: &Theme,
    cx: &mut Context<RoomPanel>,
) -> impl IntoElement {
    let toggles = Mark::ALL.into_iter().map(|mark| {
        let on = marks.contains(&mark);
        let word = mark.name();
        let action = if on {
            format!("Clear {word} on {name}")
        } else {
            format!("Mark {name} {word}")
        };
        let color = match mark {
            Mark::Aggressive if on => theme.danger,
            _ if on => theme.accent,
            _ => theme.text_muted,
        };
        let title = name.to_string();
        button(
            (id, index * Mark::ALL.len() + mark as usize),
            action.into(),
            theme,
        )
        .aria_toggled(on.into())
        .h(MARK_HEIGHT)
        .px_2()
        .text_size(px(12.))
        .text_color(color)
        .when(on, |toggle| toggle.bg(theme.selected))
        .on_click(cx.listener(move |_, _, _, cx| {
            cx.emit(MarkMob {
                title: title.clone(),
                mark,
                set: !on,
            });
        }))
        .child(word)
    });
    div().flex().gap_1().children(toggles.collect::<Vec<_>>())
}

fn verb_button(
    id: impl Into<ElementId>,
    label: &'static str,
    name: String,
    command: String,
    color: Hsla,
    theme: &Theme,
    cx: &mut Context<RoomPanel>,
) -> Stateful<Div> {
    button(id, name.into(), theme)
        .h(BUTTON_HEIGHT)
        .px_3()
        .font_weight(FontWeight::MEDIUM)
        .text_color(color)
        .on_click(cx.listener(move |panel, event, window, cx| {
            panel.send(command.clone(), event, window, cx);
        }))
        .child(label)
}

/// A button the keyboard can reach and press. It shows its focus only when
/// the keyboard moved it there.
pub fn button(id: impl Into<ElementId>, name: SharedString, theme: &Theme) -> Stateful<Div> {
    let (hover, focused) = (theme.hover, theme.accent_hover);
    div()
        .id(id)
        .debug_selector({
            let name = name.clone();
            move || name.to_string()
        })
        .role(Role::Button)
        .aria_label(name)
        .focusable()
        .tab_stop(true)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .focus_visible(move |style| style.bg(hover).border_color(focused))
}

/// gpui has no letter spacing, so the canvas's wide-set labels use thin spaces.
pub fn section_label(text: &str, theme: &Theme) -> impl IntoElement {
    let spaced: Vec<String> = text.chars().map(String::from).collect();
    div()
        .flex_none()
        .text_size(px(11.))
        .text_color(theme.text_muted)
        .child(spaced.join("\u{2009}"))
}

fn section(label: String, name: &'static str, list: Div, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(section_label(&label, theme))
        .child(div().id(name).role(Role::List).aria_label(name).child(list))
}

pub fn row_name(name: SharedString) -> impl IntoElement {
    div()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .child(name)
}

impl Render for RoomPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<RoomPanel>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let exits = self.place.as_ref().map(|place| place.exits.as_slice());
        let seen = self.place.as_ref().map(|place| place.seen.as_slice());
        div()
            .id("room-panel")
            .debug_selector(|| "room-panel".to_owned())
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            // The mouse never moves focus here: typing stays with the bar.
            .capture_any_mouse_down(|_, window, _| window.prevent_default())
            .role(Role::Group)
            .aria_label("Room")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_4()
            .pt_2()
            .text_size(px(14.))
            .text_color(theme.text)
            .child(self.render_heading(&theme))
            .child(self.render_map(&theme))
            .children(
                exits
                    .filter(|exits| !exits.is_empty())
                    .map(|exits| self.render_exits(exits, &theme, cx)),
            )
            .children(self.card.clone())
            .when(!self.contents.occupants.is_empty(), |panel| {
                panel.child(self.render_occupants(&theme, cx))
            })
            .when(!self.contents.items.is_empty(), |panel| {
                panel.child(self.render_items(&theme, cx))
            })
            .children(
                seen.filter(|seen| !seen.is_empty())
                    .map(|seen| self.render_seen(seen, &theme, cx)),
            )
    }
}

/// Presses and lets go of `key`: a keyboard click needs both.
#[cfg(test)]
pub fn press_key(key: &'static str, cx: &mut gpui::VisualTestContext) {
    cx.simulate_keystrokes(key);
    cx.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse(key).expect("a key"),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measure::sample_room;
    use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, WindowAppearance, point, px};
    use mudling_core::map::{LiveMap, MarkChange, PlainRules};
    use mudling_core::persist::Timestamp;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    fn plugin(id: &str, layer: &str, verbs: &str) -> Plugin {
        Plugin::from_files(|file| {
            Ok(match file {
                "plugin.toml" => format!(
                    "id = \"{id}\"\nname = \"{id}\"\nversion = \"0.1.0\"\napi = 1\n\
                     license = \"MIT\"\nlayer = \"{layer}\"\n[servers]\nhosts = [\"game.example\"]\n[points]\ncommands = \"commands.toml\"\n"
                ),
                _ => format!("[verbs]\n{verbs}\n"),
            })
        })
        .expect("the plugin loads")
    }

    fn room_with(sightings: &[(&str, i64)], info: &str) -> Arc<MapRoom> {
        let mut map = LiveMap::new(Box::new(PlainRules));
        map.room_info(info, Timestamp(0));
        for (title, when) in sightings {
            map.note_sightings(&[(*title).to_owned()], Timestamp(*when));
        }
        let commit = map.commit(Timestamp(0)).expect("a room was mapped");
        Arc::clone(&commit.change.rooms[0])
    }

    #[test]
    fn a_closed_door_goes_as_a_door_step_and_an_open_exit_as_a_word() {
        let open = Exit {
            direction: Direction::North,
            closed: false,
        };
        let shut = Exit {
            direction: Direction::Up,
            closed: true,
        };
        assert_eq!(open.command(), "north");
        assert_eq!(shut.command(), "[u]");
    }

    #[test]
    fn the_highest_layers_word_wins_and_a_disabled_plugin_has_no_say() {
        let plugins = [
            plugin("the-override", "override", "kill = \"slay\""),
            plugin("the-game", "game", "kill = \"murder\"\nget = \"take\""),
            plugin("off", "override", "look = \"peer\""),
        ];
        let enabled = [plugins[0].id().clone(), plugins[1].id().clone()];
        let verbs = Verbs::of(&plugins, &enabled);
        assert_eq!(verbs.command("kill", "wolf"), "slay wolf");
        assert_eq!(verbs.command("get", "stone"), "take stone");
        assert_eq!(verbs.command("look", "wolf"), "look wolf");
    }

    #[test]
    fn a_name_from_the_server_cant_add_a_command() {
        let verbs = Verbs::default();
        assert_eq!(verbs.command("get", "stone;drop all"), "get stone");
        assert_eq!(verbs.command("get", "stone\nquit"), "get stone");
    }

    #[test]
    fn the_newest_sightings_show_with_their_marks() {
        let mut titles: Vec<(String, i64)> = (0..MOST_SIGHTINGS as i64)
            .map(|n| (format!("mob {n}"), n + 1))
            .collect();
        titles.push(("(Marked) a grey Wolf".to_owned(), 100));
        titles.push(("forgotten".to_owned(), 0));
        let borrowed: Vec<(&str, i64)> = titles.iter().map(|(t, w)| (t.as_str(), *w)).collect();
        let room = room_with(&borrowed, r#"{"num": 1, "name": "A road"}"#);
        let mark = |name: &str, mark| MarkChange {
            name: name.to_owned(),
            mark,
            set: true,
        };
        let mobs = MobsState::default().with_changes(&[
            mark("wolf", Mark::Aggressive),
            mark("a grey wolf", Mark::Gear),
            mark("gre", Mark::Aggressive),
        ]);

        let seen = sightings(&room, &mobs);
        assert_eq!(seen.len(), MOST_SIGHTINGS);
        assert_eq!(seen[0].name, "(Marked) a grey Wolf");
        assert_eq!(seen[0].marks, [Mark::Aggressive, Mark::Gear]);
        assert!(!seen.iter().any(|mob| mob.name == "forgotten"));
    }

    #[test]
    fn a_sightings_marks_are_read_with_its_name() {
        let sighting = |marks| Sighting {
            name: "a grey wolf".into(),
            marks,
        };
        assert_eq!(sighting(Vec::new()).label(), "a grey wolf");
        assert_eq!(
            sighting(vec![Mark::Aggressive, Mark::Gear]).label(),
            "a grey wolf, aggressive, gear"
        );
    }

    type Sent = Rc<RefCell<Vec<(String, bool)>>>;

    fn panel_window(cx: &mut TestAppContext) -> (Entity<RoomPanel>, &mut VisualTestContext, Sent) {
        cx.update(|cx| {
            crate::shell::bind_keys(cx);
            cx.set_global(Theme::for_appearance(WindowAppearance::Dark));
        });
        let sent = Sent::default();
        let log = sent.clone();
        let (panel, cx) = cx.add_window_view(|_, cx| RoomPanel::new(Verbs::default(), cx));
        cx.update(|_, cx| {
            cx.subscribe(&panel, move |_, event: &SendCommand, _| {
                log.borrow_mut()
                    .push((event.command.clone(), event.by_keyboard));
            })
            .detach();
        });
        (panel, cx, sent)
    }

    fn fill(panel: &Entity<RoomPanel>, cx: &mut VisualTestContext) {
        let (room, mobs, contents) = sample_room();
        panel.update(cx, |panel, cx| {
            panel.show_room(&room, &mobs, cx);
            panel.show_contents(contents, cx);
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn an_occupant_with_no_word_to_target_shows_with_no_verbs(cx: &mut TestAppContext) {
        let (panel, cx, _) = panel_window(cx);
        let (room, mobs, mut contents) = sample_room();
        contents.occupants[0].target = None;
        panel.update(cx, |panel, cx| {
            panel.show_room(&room, &mobs, cx);
            panel.show_contents(contents, cx);
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("occupant-0").is_some());
        assert!(
            cx.debug_bounds("Kill A traveller resting by the wall")
                .is_none()
        );
    }

    #[gpui::test]
    fn sections_stay_hidden_until_something_feeds_them(cx: &mut TestAppContext) {
        let (panel, cx, _) = panel_window(cx);
        let (room, mobs, contents) = sample_room();
        panel.update(cx, |panel, cx| panel.show_room(&room, &mobs, cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("Go north").is_some());
        assert!(
            cx.debug_bounds("Kill A traveller resting by the wall")
                .is_none()
        );
        assert!(cx.debug_bounds("Get a stone").is_none());

        panel.update(cx, |panel, cx| panel.show_contents(contents, cx));
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("Kill A traveller resting by the wall")
                .is_some()
        );
        assert!(cx.debug_bounds("Get a stone").is_some());
    }

    #[gpui::test]
    fn the_dpad_lights_only_the_real_exits(cx: &mut TestAppContext) {
        let (panel, cx, sent) = panel_window(cx);
        fill(&panel, cx);
        for lit in ["dpad-north", "dpad-south", "dpad-east"] {
            assert!(cx.debug_bounds(lit).is_some(), "{lit}");
        }
        for dark in ["dpad-west", "dpad-up", "dpad-down"] {
            assert!(cx.debug_bounds(dark).is_none(), "{dark}");
        }

        let east = cx.debug_bounds("dpad-east").unwrap().center();
        cx.simulate_click(east, Modifiers::none());
        assert_eq!(*sent.borrow(), [("[e]".to_owned(), false)]);
    }

    #[gpui::test]
    fn the_verbs_send_their_commands(cx: &mut TestAppContext) {
        let (panel, cx, sent) = panel_window(cx);
        fill(&panel, cx);
        for name in [
            "Look at A traveller resting by the wall",
            "Kill A traveller resting by the wall",
            "Get some coins",
        ] {
            let button = cx.debug_bounds(name).unwrap().center();
            cx.simulate_click(button, Modifiers::none());
        }
        let commands: Vec<_> = sent.borrow().iter().map(|(c, _)| c.clone()).collect();
        assert_eq!(commands, ["look traveller", "kill traveller", "get coins"]);
    }

    #[gpui::test]
    fn a_toggle_marks_a_mob_and_clears_a_mark(cx: &mut TestAppContext) {
        let (panel, cx, _) = panel_window(cx);
        fill(&panel, cx);
        let marked = Rc::new(RefCell::new(Vec::new()));
        let log = marked.clone();
        cx.update(|_, cx| {
            cx.subscribe(&panel, move |_, event: &MarkMob, _| {
                log.borrow_mut()
                    .push((event.title.clone(), event.mark, event.set));
            })
            .detach();
        });

        let toggle = "Mark A traveller resting by the wall gear";
        let toggle = cx.debug_bounds(toggle).unwrap().center();
        cx.simulate_click(toggle, Modifiers::none());
        let traveller = "A traveller resting by the wall".to_owned();
        assert_eq!(*marked.borrow(), [(traveller, Mark::Gear, true)]);

        // The wolf came marked aggressive, so its toggle clears.
        let clear = "Clear aggressive on (Marked) a grey wolf";
        let toggle = cx.debug_bounds(clear).unwrap().center();
        cx.simulate_click(toggle, Modifiers::none());
        assert_eq!(
            marked.borrow().last(),
            Some(&("(Marked) a grey wolf".to_owned(), Mark::Aggressive, false))
        );
    }

    #[gpui::test]
    fn a_mounted_map_takes_the_maps_place(cx: &mut TestAppContext) {
        struct Map;
        impl Render for Map {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div().debug_selector(|| "the-map".to_owned()).size_full()
            }
        }
        let (panel, cx, _) = panel_window(cx);
        panel.update(cx, |panel, cx| {
            let map = cx.new(|_| Map);
            panel.mount_map(map, cx);
        });
        cx.run_until_parked();
        let map = cx.debug_bounds("the-map").unwrap();
        let slot = cx.debug_bounds("room-map").unwrap();
        assert!(slot.contains(&point(map.center().x, map.center().y)));
        assert!(map.size.height > px(100.));
    }
}
