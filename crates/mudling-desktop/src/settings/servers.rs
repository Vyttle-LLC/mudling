//! The Servers & characters section, drawn from a small view model. Nothing
//! fills it yet: once saved servers have a store, passing them to
//! [`super::SettingsSheet::show_servers`] is the one step left.

use super::{Pane, muted_line};
use crate::assets::UI_FONT;
use crate::theme::Theme;
use gpui::{FontWeight, Role, SharedString, div, prelude::*, px};

const NOTHING_SAVED: &str = "Saving servers isn't here yet, so this list is empty.";

/// A saved server and its characters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedServer {
    pub name: SharedString,
    pub host: SharedString,
    pub port: u16,
    pub characters: Vec<SharedString>,
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
}
