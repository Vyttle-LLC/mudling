//! The Servers & characters section, drawn from a small view model that the
//! shell fills from the saved servers: each server with its characters
//! under it, and Add character at the top.
//!
//! Removing a server or a character asks first, in its row. The question's
//! first stop is Keep, which takes the place of the Remove button, so focus
//! lands on the choice that loses nothing.

use super::{
    Pane, Section, SettingsSheet, SheetEvent, muted_line, problem_line, question_line, text_button,
};
use crate::assets::UI_FONT;
use crate::theme::Theme;
use gpui::{Context, FontWeight, Role, SharedString, div, prelude::*, px};
use mudling_core::persist;
use mudling_core::vault::EntryKey;

const NOTHING_SAVED: &str = "No saved servers yet.";
const PLAYING: &str = "Playing now";

/// A saved server and its characters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedServer {
    pub name: SharedString,
    pub host: SharedString,
    pub port: u16,
    pub characters: Vec<SavedCharacter>,
}

/// A character the game or the player has named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedCharacter {
    /// The lowercased name its rows are kept by.
    pub id: SharedString,
    pub name: SharedString,
}

impl From<&persist::SavedServer> for SavedServer {
    /// A server has no name of its own, so its host stands for it. A character
    /// the game hasn't named yet isn't listed.
    fn from(server: &persist::SavedServer) -> SavedServer {
        SavedServer {
            name: server.host.clone().into(),
            host: server.host.clone().into(),
            port: server.port,
            characters: (server.characters.iter())
                .filter_map(|character| {
                    Some(SavedCharacter {
                        id: character.id.clone().into(),
                        name: character.name.clone()?.into(),
                    })
                })
                .collect(),
        }
    }
}

impl SavedServer {
    /// Says what goes and what stays: see [`persist::remove_server`].
    fn removal_question(&self) -> String {
        format!(
            "Remove {} from your list? Its map, notes and characters stay on this device, \
             and come back if you add the server again.",
            self.name
        )
    }
}

/// What the vault holds for a character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasswordState {
    Saved,
    /// Nothing saved: the name is sent and the player types the password.
    Typed,
    /// The vault is locked, so nothing can say.
    Unknown,
}

/// What the vault says of every character's password, as far as it can.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Passwords {
    /// No vault, so nothing is saved.
    #[default]
    NoVault,
    Locked,
    Listed(Vec<EntryKey>),
}

impl Passwords {
    /// What's saved for `character` on `host`, matched as login matches it.
    pub fn of(&self, host: &str, character: &str) -> PasswordState {
        match self {
            Passwords::NoVault => PasswordState::Typed,
            Passwords::Locked => PasswordState::Unknown,
            Passwords::Listed(entries) => {
                match entries.iter().any(|entry| entry.is_for(host, character)) {
                    true => PasswordState::Saved,
                    false => PasswordState::Typed,
                }
            }
        }
    }
}

/// How a character's row says what logging in does.
fn login_words(state: PasswordState) -> &'static str {
    match state {
        PasswordState::Saved => "Password saved in the vault · logs in automatically",
        PasswordState::Typed => "Name sent, you type the password",
        PasswordState::Unknown => "The vault is locked, so it can't say if a password is saved",
    }
}

/// A saved character and the server it's on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CharacterOn {
    pub host: SharedString,
    pub character: SavedCharacter,
}

impl SettingsSheet {
    /// Asks whether to remove `host`, as its Remove does, for `--ax-check`.
    #[cfg(target_os = "macos")]
    pub fn ask_to_remove_server(&mut self, host: &str, cx: &mut Context<SettingsSheet>) {
        self.show_playing(None, cx);
        self.removing = Some(host.to_owned().into());
        cx.notify();
    }

    pub(super) fn fill_servers(
        &mut self,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) {
        let add = text_button("add-character", "Add character", &pane.stop(cx), theme)
            .border_color(theme.accent)
            .text_color(theme.accent)
            .on_click(cx.listener(|sheet, _, window, cx| {
                sheet.open_character_form(None, false, window, cx);
            }));
        pane.trailing = Some(add.into_any_element());
        if self.servers.is_empty() {
            pane.row(muted_line(NOTHING_SAVED, theme));
        }
        if let Some(problem) = self.servers_problem.clone() {
            pane.row(problem_line(problem, theme).debug_selector(|| "servers-problem".to_owned()));
        }
        let passwords = self.passwords();
        for (number, server) in self.servers.clone().iter().enumerate() {
            let header = self.server_header(number, server, pane, theme, cx);
            pane.row(header);
            for (index, character) in server.characters.iter().enumerate() {
                let state = passwords.of(&server.host, &character.name);
                let row =
                    self.character_row((number, index), server, character, state, pane, theme, cx);
                pane.row(row);
            }
        }
    }

    fn server_header(
        &self,
        number: usize,
        server: &SavedServer,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) -> impl IntoElement + use<> {
        let playing = self.playing.as_ref() == Some(&server.host);
        let asking = !playing && self.removing.as_ref() == Some(&server.host);
        let port = format!("port {}", server.port);
        let about = div()
            .id(("saved-server", number))
            .role(Role::Heading)
            .aria_level(3)
            .aria_label(format!("{}, {port}", server.name))
            .flex_1()
            .min_w_0()
            .flex()
            .items_baseline()
            .gap_2()
            .font_family(UI_FONT)
            .child(
                div()
                    .text_size(px(15.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(server.name.clone()),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(theme.text_muted)
                    .child(port),
            );
        // Stops go in the order they're drawn: Tab follows them.
        let plugins = {
            let id = SharedString::from(format!("server-plugins-{number}"));
            let host = server.host.to_string();
            text_button(id, "Plugins", &pane.stop(cx), theme)
                .aria_label(format!("Plugins for {}", server.name))
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(SheetEvent::ServerPlugins(host.clone()));
                }))
        };
        let remove = (!playing && !asking).then(|| {
            let id = SharedString::from(format!("remove-server-{number}"));
            let host = server.host.clone();
            text_button(id, "Remove server", &pane.stop(cx), theme)
                .aria_label(format!("Remove {}", server.name))
                .on_click(cx.listener(move |sheet, _, _, cx| {
                    sheet.removing = Some(host.clone());
                    cx.notify();
                }))
        });
        let in_play = playing.then(|| {
            div()
                .flex_none()
                .font_family(UI_FONT)
                .text_size(px(13.))
                .text_color(theme.text_muted)
                .child(PLAYING)
        });
        let question = asking.then(|| {
            let keep = SharedString::from(format!("keep-server-{number}"));
            let confirm = SharedString::from(format!("confirm-remove-server-{number}"));
            let host = server.host.clone();
            div()
                .pt_2()
                .child(question_line(server.removal_question(), theme))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            text_button(keep, "Keep", &pane.stop(cx), theme)
                                .aria_label(format!("Keep {}", server.name))
                                .on_click(cx.listener(|sheet, _, _, cx| {
                                    sheet.removing = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            text_button(confirm, "Remove", &pane.stop(cx), theme)
                                .aria_label(format!("Remove {}", server.name))
                                .on_click(cx.listener(move |sheet, _, window, cx| {
                                    sheet.removing = None;
                                    // This stop goes with the question.
                                    let tab = &sheet.tabs[Section::Servers as usize];
                                    window.focus(tab, cx);
                                    cx.emit(SheetEvent::RemoveServer(host.to_string()));
                                    cx.notify();
                                })),
                        ),
                )
        });
        div()
            .pt_4()
            .pb_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(about)
                    .child(plugins)
                    .children(remove)
                    .children(in_play),
            )
            .children(question)
    }

    #[allow(clippy::too_many_arguments)]
    fn character_row(
        &self,
        (number, index): (usize, usize),
        server: &SavedServer,
        character: &SavedCharacter,
        state: PasswordState,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) -> impl IntoElement + use<> {
        let words = login_words(state);
        let removing = CharacterOn {
            host: server.host.clone(),
            character: character.clone(),
        };
        let asking = self.removing_character.as_ref() == Some(&removing);
        let name = character.name.clone();
        let about = div()
            .id(SharedString::from(format!("character-{number}-{index}")))
            .role(Role::Label)
            .aria_value(format!("{name}: {words}"))
            .flex_1()
            .min_w_0()
            .font_family(UI_FONT)
            .child(
                div()
                    .text_size(px(14.))
                    .text_color(theme.text)
                    .child(name.clone()),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(theme.text_muted)
                    .child(words),
            );
        let id = |what: &str| SharedString::from(format!("{what}-character-{number}-{index}"));
        let edit = {
            let editing = removing.clone();
            text_button(id("edit"), "Edit", &pane.stop(cx), theme)
                .aria_label(format!("Edit {name} on {}", server.name))
                .on_click(cx.listener(move |sheet, _, window, cx| {
                    sheet.open_character_form(Some(editing.clone()), false, window, cx);
                }))
        };
        let remove = (!asking).then(|| {
            let asked = removing.clone();
            text_button(id("remove"), "Remove", &pane.stop(cx), theme)
                .aria_label(format!("Remove {name} from {}", server.name))
                .on_click(cx.listener(move |sheet, _, _, cx| {
                    sheet.removing_character = Some(asked.clone());
                    cx.notify();
                }))
        });
        let question = asking.then(|| {
            let keep = text_button(id("keep"), "Keep", &pane.stop(cx), theme)
                .aria_label(format!("Keep {name}"))
                .on_click(cx.listener(|sheet, _, _, cx| {
                    sheet.removing_character = None;
                    cx.notify();
                }));
            let confirm = text_button(id("confirm-remove"), "Remove", &pane.stop(cx), theme)
                .aria_label(format!("Remove {name}"))
                .on_click(cx.listener(move |sheet, _, window, cx| {
                    sheet.remove_character(removing.clone(), window, cx);
                }));
            div()
                .pt_2()
                .child(question_line(
                    format!(
                        "Remove {name} from your list on every device you sync? Its stats and \
                         gear history stay, and its saved password is deleted."
                    ),
                    theme,
                ))
                .child(div().flex().gap_2().child(keep).child(confirm))
        });
        div()
            .ml_4()
            .py_2()
            .border_b_1()
            .border_color(theme.divider)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(about)
                    .child(edit)
                    .children(remove),
            )
            .children(question)
    }

    /// Takes `removing` off the list, and its saved password out of the vault.
    fn remove_character(
        &mut self,
        removing: CharacterOn,
        window: &mut gpui::Window,
        cx: &mut Context<SettingsSheet>,
    ) {
        self.removing_character = None;
        // This stop goes with the question.
        self.focus_tab(Section::Servers, window, cx);
        let CharacterOn { host, character } = removing;
        cx.emit(SheetEvent::RemoveCharacter {
            host: host.to_string(),
            id: character.id.to_string(),
        });
        self.keep_password(&host, &character.name, None, window, cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_server_lists_the_characters_the_game_has_named() {
        let character = |id: &str, name: Option<&str>| persist::SavedCharacter {
            id: id.to_owned(),
            name: name.map(str::to_owned),
            level: None,
            last_played: persist::Timestamp::now(),
        };
        let saved = persist::SavedServer {
            host: "mud.example".to_owned(),
            port: 4000,
            updated: persist::Timestamp::now(),
            last_room: None,
            characters: vec![
                character("sample", Some("Sample")),
                character("player", None),
            ],
        };
        let shown = SavedServer::from(&saved);
        assert_eq!(shown.host, "mud.example");
        assert_eq!(shown.port, 4000);
        let names: Vec<_> = shown.characters.iter().map(|c| c.name.clone()).collect();
        assert_eq!(names, ["Sample"]);
        assert_eq!(shown.characters[0].id, "sample");
    }

    #[test]
    fn a_password_is_matched_however_it_was_capitalized() {
        let listed = Passwords::Listed(vec![EntryKey::new("mud.example", "Bren")]);
        assert_eq!(listed.of("mud.example", "bren"), PasswordState::Saved);
        assert_eq!(listed.of("mud.example", "Tamsin"), PasswordState::Typed);
        assert_eq!(listed.of("other.example", "Bren"), PasswordState::Typed);
        assert_eq!(
            Passwords::Locked.of("mud.example", "Bren"),
            PasswordState::Unknown
        );
    }
}
