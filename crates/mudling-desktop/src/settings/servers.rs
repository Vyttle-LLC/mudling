//! The Servers & characters section, drawn from a small view model that the
//! shell fills from the saved servers.
//!
//! Removing a server asks first, in its row. The question's first stop is
//! Keep, which takes the place of the Remove button, so focus lands on the
//! choice that loses nothing.

use super::{Pane, SettingsSheet, SheetEvent, muted_line, text_button};
use crate::assets::UI_FONT;
use crate::theme::Theme;
use gpui::{Context, FontWeight, Role, SharedString, div, prelude::*, px};
use mudling_core::persist;

const NOTHING_SAVED: &str = "No saved servers yet.";
const PLAYING: &str = "Playing now";

/// A saved server and its characters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedServer {
    pub name: SharedString,
    pub host: SharedString,
    pub port: u16,
    pub characters: Vec<SharedString>,
}

impl From<&persist::SavedServer> for SavedServer {
    /// A server has no name of its own, so its host stands for it. A character
    /// the game hasn't named yet isn't listed.
    fn from(server: &persist::SavedServer) -> SavedServer {
        SavedServer {
            name: server.host.clone().into(),
            host: server.host.clone().into(),
            port: server.port,
            characters: server
                .characters
                .iter()
                .filter_map(|character| character.name.clone())
                .map(SharedString::from)
                .collect(),
        }
    }
}

impl SavedServer {
    fn summary(&self) -> String {
        let address = format!("{}:{}", self.host, self.port);
        let characters = self.characters.join(", ");
        match characters.is_empty() {
            true => address,
            false => format!("{address} · {characters}"),
        }
    }

    /// Says what goes and what stays: see [`persist::remove_server`].
    fn removal_question(&self) -> String {
        format!(
            "Remove {} from your list? Its map, notes and characters stay on this device, \
             and come back if you add the server again.",
            self.name
        )
    }
}

impl SettingsSheet {
    pub(super) fn fill_servers(
        &mut self,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) {
        if self.servers.is_empty() {
            pane.row(muted_line(NOTHING_SAVED, theme));
        }
        if let Some(problem) = self.servers_problem.clone() {
            pane.row(
                muted_line(problem, theme)
                    .debug_selector(|| "servers-problem".to_owned())
                    .role(Role::Alert)
                    .text_color(theme.error),
            );
        }
        for (number, server) in self.servers.clone().iter().enumerate() {
            let row = self.server_row(number, server, pane, theme, cx);
            pane.row(row);
        }
    }

    fn server_row(
        &self,
        number: usize,
        server: &SavedServer,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) -> impl IntoElement + use<> {
        let summary = server.summary();
        let playing = self.playing.as_ref() == Some(&server.host);
        let asking = !playing && self.removing.as_ref() == Some(&server.host);
        let about = div()
            .id(("saved-server", number))
            .role(Role::Label)
            .aria_value(format!("{}: {summary}", server.name))
            .flex_1()
            .min_w_0()
            .font_family(UI_FONT)
            .child(
                div()
                    .text_size(px(15.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(server.name.clone()),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(theme.text_muted)
                    .child(summary),
            );
        let remove = (!playing && !asking).then(|| {
            let id = SharedString::from(format!("remove-server-{number}"));
            let host = server.host.clone();
            text_button(id, "Remove", &pane.stop(cx), theme)
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
                .child(
                    muted_line(server.removal_question(), theme)
                        .role(Role::Alert)
                        .text_color(theme.text),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(text_button(keep, "Keep", &pane.stop(cx), theme).on_click(
                            cx.listener(|sheet, _, _, cx| {
                                sheet.removing = None;
                                cx.notify();
                            }),
                        ))
                        .child(
                            text_button(confirm, "Remove", &pane.stop(cx), theme)
                                .aria_label(format!("Remove {}", server.name))
                                .on_click(cx.listener(move |sheet, _, _, cx| {
                                    sheet.removing = None;
                                    cx.emit(SheetEvent::RemoveServer(host.to_string()));
                                    cx.notify();
                                })),
                        ),
                )
        });
        div()
            .py_3()
            .border_b_1()
            .border_color(theme.divider)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(about)
                    .children(remove)
                    .children(in_play),
            )
            .children(question)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_reads_as_its_address_and_characters() {
        let mut server = SavedServer {
            name: "A game".into(),
            host: "mud.example".into(),
            port: 4000,
            characters: Vec::new(),
        };
        assert_eq!(server.summary(), "mud.example:4000");
        server.characters = vec!["Sample".into(), "Other".into()];
        assert_eq!(server.summary(), "mud.example:4000 · Sample, Other");
    }

    #[test]
    fn a_saved_server_lists_the_characters_the_game_has_named() {
        let character = |name: Option<&str>| persist::SavedCharacter {
            name: name.map(str::to_owned),
            level: None,
            last_played: persist::Timestamp::now(),
        };
        let saved = persist::SavedServer {
            host: "mud.example".to_owned(),
            port: 4000,
            updated: persist::Timestamp::now(),
            last_room: None,
            characters: vec![character(Some("Sample")), character(None)],
        };
        let shown = SavedServer::from(&saved);
        assert_eq!(shown.host, "mud.example");
        assert_eq!(shown.port, 4000);
        assert_eq!(shown.characters, ["Sample"]);
    }
}
