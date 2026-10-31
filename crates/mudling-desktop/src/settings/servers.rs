//! The Servers & characters section, drawn from a small view model that the
//! shell fills from the saved servers.

use super::{Pane, muted_line};
use crate::assets::UI_FONT;
use crate::theme::Theme;
use gpui::{FontWeight, Role, SharedString, div, prelude::*, px};
use mudling_core::persist;

const NOTHING_SAVED: &str = "No saved servers yet.";

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
}

pub(super) fn fill(pane: &mut Pane, servers: &[SavedServer], theme: &Theme) {
    if servers.is_empty() {
        pane.row(muted_line(NOTHING_SAVED, theme));
    }
    for (number, server) in servers.iter().enumerate() {
        let summary = server.summary();
        pane.row(
            div()
                .id(("saved-server", number))
                .role(Role::Label)
                .aria_value(format!("{}: {summary}", server.name))
                .py_3()
                .border_b_1()
                .border_color(theme.divider)
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
                ),
        );
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
