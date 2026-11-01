//! The Shortcuts section. [`SHORTCUTS`] is the one list of what the keys do. A
//! test holds it to the keymap both ways, so a binding added anywhere fails
//! until it has a row here, and a row can't outlive its binding.

use super::{Pane, muted_line, section_heading};
use crate::assets::UI_FONT;
use crate::theme::Theme;
use gpui::{FontWeight, Role, SharedString, div, prelude::*, px};

const REBINDING_NOTE: &str = "Changing these keys isn't here yet.";

/// A row of the section. Each action is bound to one or more of the `keys`.
pub struct Shortcut {
    pub what: &'static str,
    /// In the keymap's notation, with `main` for ⌘ on macOS and Ctrl elsewhere.
    pub keys: &'static [&'static str],
    /// Only the test against the keymap reads these.
    #[cfg_attr(not(test), allow(dead_code))]
    pub actions: &'static [&'static str],
}

const fn row(
    what: &'static str,
    keys: &'static [&'static str],
    actions: &'static [&'static str],
) -> Shortcut {
    Shortcut {
        what,
        keys,
        actions,
    }
}

pub const SHORTCUTS: [(&str, &[Shortcut]); 5] = [
    (
        "Window",
        &[
            row(
                "Focus the command bar",
                &["main-l"],
                &["shell::FocusCommandBar"],
            ),
            row(
                "Map and Find panel",
                &["main-1"],
                &["shell::ToggleLeftPanel"],
            ),
            row("Right panel", &["main-2"], &["shell::ToggleRightPanel"]),
            row(
                "Go to the Room panel",
                &["main-3"],
                &["shell::FocusRoomPanel"],
            ),
            row("Go to the hotbar", &["main-4"], &["shell::FocusHotbar"]),
            row("Go to the Notes tab", &["main-6"], &["shell::FocusNotes"]),
            row("Finish a note", &["escape"], &["notes::FinishNote"]),
            row(
                "Leave a gear set without saving",
                &["escape"],
                &["gear_panel::CloseEditor"],
            ),
            row(
                "Keep the map when asked to restore one",
                &["escape"],
                &["map_history::KeepTheMap"],
            ),
            row(
                "Move along the notes",
                &["up", "down"],
                &["notes::PreviousNote", "notes::NextNote"],
            ),
            row("Go to a note's room", &["enter"], &["notes::GoToNote"]),
            row("Walk to a note's room", &["w"], &["notes::WalkToNote"]),
            row(
                "Go to the Find panel",
                &["main-5"],
                &["shell::FocusFindPanel"],
            ),
            row(
                "Search the scrollback",
                &["main-f"],
                &["title_bar::FindInScrollback"],
            ),
            row(
                "Open the HUD, or go to its floating card",
                &["main-j"],
                &["hud::ToggleHud"],
            ),
            row(
                "Move the floating card",
                &["up", "down", "left", "right"],
                &[
                    "hud::MoveCardUp",
                    "hud::MoveCardDown",
                    "hud::MoveCardLeft",
                    "hud::MoveCardRight",
                ],
            ),
            row("Open settings", &["main-,"], &["title_bar::OpenSettings"]),
            row("Quit", &["main-q"], &["mudling::Quit"]),
        ],
    ),
    (
        "Chat",
        &[
            row("Read back a page", &["pageup"], &["chat::PageBack"]),
            row("Forward a page", &["pagedown"], &["chat::PageForward"]),
            row("Back to live", &["end", "escape"], &["chat::BackToLive"]),
            row("Close find", &["escape"], &["find::CloseFind"]),
            row("Next find result", &["enter"], &["find::NextResult"]),
            row(
                "Previous find result",
                &["shift-enter"],
                &["find::PreviousResult"],
            ),
        ],
    ),
    (
        "Command bar",
        &[
            row("Send", &["enter"], &["command_input::Submit"]),
            row("New line", &["shift-enter"], &["command_input::Newline"]),
            row("Previous command", &["up"], &["command_input::Up"]),
            row("Next command", &["down"], &["command_input::Down"]),
            row("Complete a word", &["tab"], &["command_input::Complete"]),
            row("Copy", &["main-c"], &["command_input::Copy"]),
            row("Paste", &["main-v"], &["command_input::Paste"]),
            row("Pin to the hotbar", &["main-p"], &["shell::PinCommand"]),
            row(
                "Move the caret",
                &["left", "right", "home", "end"],
                &[
                    "command_input::Left",
                    "command_input::Right",
                    "command_input::Home",
                    "command_input::End",
                ],
            ),
            row(
                "Delete",
                &["backspace", "delete"],
                &["command_input::Backspace", "command_input::Delete"],
            ),
        ],
    ),
    (
        "Hotbar",
        &[
            row(
                "Move between chips",
                &["left", "right"],
                &["hotbar::PreviousChip", "hotbar::NextChip"],
            ),
            row(
                "Move a chip",
                &["shift-left", "shift-right"],
                &["hotbar::MoveChipLeft", "hotbar::MoveChipRight"],
            ),
            row("Edit a chip", &["e"], &["hotbar::EditChip"]),
            row(
                "Remove a chip",
                &["delete", "backspace"],
                &["hotbar::RemoveChip"],
            ),
            row("Stop editing a chip", &["escape"], &["hotbar::CancelEdit"]),
        ],
    ),
    (
        "Controls",
        &[
            row(
                "Next control",
                &["tab"],
                &[
                    "title_bar::FocusNextControl",
                    "shell::FocusNext",
                    "title_bar::CloseMenu",
                    "settings::NextStop",
                ],
            ),
            row(
                "Previous control",
                &["shift-tab"],
                &[
                    "title_bar::FocusPreviousControl",
                    "title_bar::CloseMenu",
                    "settings::PreviousStop",
                ],
            ),
            row(
                "Back to the command bar",
                &["escape"],
                &["shell::FocusCommandBar"],
            ),
            row(
                "Close a menu, this sheet or the add-server form",
                &["escape"],
                &[
                    "title_bar::CloseMenu",
                    "settings::CloseSettings",
                    "empty_state::CancelAdd",
                ],
            ),
            row(
                "Move through a menu",
                &["up", "down"],
                &["title_bar::MenuPrevious", "title_bar::MenuNext"],
            ),
            row(
                "Scroll this sheet",
                &["up", "down", "pageup", "pagedown"],
                &[
                    "settings::ScrollUp",
                    "settings::ScrollDown",
                    "settings::PageUp",
                    "settings::PageDown",
                ],
            ),
        ],
    ),
];

/// One keystroke as the player reads it: `⌘,` on a Mac, `Ctrl+,` elsewhere.
fn keystroke_label(keys: &str, mac: bool) -> String {
    keys.split('-')
        .map(|part| match (part, mac) {
            ("main", true) => "⌘".to_owned(),
            ("main", false) => "Ctrl+".to_owned(),
            ("shift", true) => "⇧".to_owned(),
            ("shift", false) => "Shift+".to_owned(),
            ("enter", true) => "Return".to_owned(),
            ("enter", false) => "Enter".to_owned(),
            ("escape", _) => "Esc".to_owned(),
            ("up", _) => "↑".to_owned(),
            ("down", _) => "↓".to_owned(),
            ("left", _) => "←".to_owned(),
            ("right", _) => "→".to_owned(),
            ("pageup", _) => "Page Up".to_owned(),
            ("pagedown", _) => "Page Down".to_owned(),
            (key, _) if key.chars().count() > 1 => {
                let mut letters = key.chars();
                letters.next().map_or_else(String::new, |first| {
                    first.to_uppercase().chain(letters).collect()
                })
            }
            (key, _) => key.to_uppercase(),
        })
        .collect()
}

fn keys_label(keys: &[&str]) -> String {
    let labels = keys
        .iter()
        .map(|keys| keystroke_label(keys, cfg!(target_os = "macos")));
    labels.collect::<Vec<_>>().join("  ")
}

pub(super) fn fill(pane: &mut Pane, theme: &Theme) {
    pane.row(muted_line(REBINDING_NOTE, theme));
    for (group, shortcuts) in SHORTCUTS {
        pane.row(section_heading(group, theme));
        for (number, shortcut) in shortcuts.iter().enumerate() {
            let keys: SharedString = keys_label(shortcut.keys).into();
            pane.row(
                div()
                    .id((group, number))
                    .debug_selector(move || format!("shortcut-{}", shortcut.what))
                    .role(Role::Label)
                    .aria_value(format!("{}: {keys}", shortcut.what))
                    .flex()
                    .items_center()
                    .justify_between()
                    .py_2()
                    .border_b_1()
                    .border_color(theme.divider)
                    .font_family(UI_FONT)
                    .text_size(px(14.))
                    .text_color(theme.text)
                    .child(shortcut.what)
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text_muted)
                            .child(keys),
                    ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestApp;
    use std::collections::BTreeSet;

    /// A row's keys as this platform's keymap has them.
    fn platform_keys(row: &Shortcut) -> Vec<String> {
        let main = if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        };
        row.keys
            .iter()
            .map(|keys| keys.replace("main", main))
            .collect()
    }

    fn rows() -> impl Iterator<Item = &'static Shortcut> {
        SHORTCUTS.iter().flat_map(|(_, shortcuts)| shortcuts.iter())
    }

    /// Every (action, keystroke) the app binds.
    fn bound() -> BTreeSet<(String, String)> {
        let mut app = TestApp::new();
        app.update(|cx| {
            crate::bind_keys(cx);
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            keymap
                .bindings()
                .map(|binding| {
                    let keys = binding.keystrokes().iter().map(|key| {
                        let key = key.inner();
                        let mut text = String::new();
                        for (held, name) in [
                            (key.modifiers.platform, "cmd-"),
                            (key.modifiers.control, "ctrl-"),
                            (key.modifiers.shift, "shift-"),
                        ] {
                            if held {
                                text.push_str(name);
                            }
                        }
                        text + &key.key
                    });
                    (binding.action().name().to_owned(), keys.collect::<String>())
                })
                .collect()
        })
    }

    #[test]
    fn every_binding_has_a_row() {
        let unlisted: Vec<_> = bound()
            .into_iter()
            .filter(|(action, keys)| {
                !rows().any(|row| {
                    row.actions.contains(&action.as_str()) && platform_keys(row).contains(keys)
                })
            })
            .collect();
        assert!(unlisted.is_empty(), "add rows for {unlisted:?}");
    }

    #[test]
    fn every_row_is_bound() {
        let bound = bound();
        for row in rows() {
            for action in row.actions {
                assert!(
                    bound.iter().any(|(bound, _)| bound == action),
                    "{action} isn't bound"
                );
            }
            for keys in platform_keys(row) {
                assert!(
                    bound
                        .iter()
                        .any(|(action, bound)| *bound == keys
                            && row.actions.contains(&action.as_str())),
                    "{}: {keys} isn't bound",
                    row.what
                );
            }
        }
    }

    #[test]
    fn keys_map_between_the_command_key_and_ctrl() {
        assert_eq!(keystroke_label("main-,", true), "⌘,");
        assert_eq!(keystroke_label("main-,", false), "Ctrl+,");
        assert_eq!(keystroke_label("main-l", true), "⌘L");
        assert_eq!(keystroke_label("main-l", false), "Ctrl+L");
        assert_eq!(keystroke_label("shift-enter", true), "⇧Return");
        assert_eq!(keystroke_label("shift-enter", false), "Shift+Enter");
        assert_eq!(keystroke_label("escape", true), "Esc");
        assert_eq!(keystroke_label("up", false), "↑");
        assert_eq!(keystroke_label("backspace", false), "Backspace");
    }
}
