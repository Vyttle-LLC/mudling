//! Plain lines about the session, under the chat: connection and vitals,
//! the game detection's suggestion, and the newest notice. No HUD design yet.

use crate::theme::Theme;
use gpui::{Context, Pixels, Role, SharedString, Window, div, prelude::*, px};
use mudling_core::defaults::detect::Source;
use mudling_core::plugin::PluginId;
use mudling_core::store::{ConnectionStatus, SettingsState, VitalsState};

const FONT_SIZE: Pixels = px(12.);

#[derive(Default)]
pub struct StatusLines {
    lines: Vec<SharedString>,
}

impl StatusLines {
    pub fn show(&mut self, lines: Vec<SharedString>, cx: &mut Context<StatusLines>) {
        if lines != self.lines {
            self.lines = lines;
            cx.notify();
        }
    }

    #[cfg(test)]
    pub fn lines(&self) -> &[SharedString] {
        &self.lines
    }
}

/// What the lines say. `replaying` says so in place of the connection: a
/// paced replay opens and closes one every few frames.
pub fn lines_for(
    settings: &SettingsState,
    vitals: &VitalsState,
    enabled: &[PluginId],
    replaying: bool,
) -> Vec<SharedString> {
    let mut status = if replaying {
        "Replaying a recording".to_owned()
    } else {
        connection_text(settings)
    };
    for (vital, value) in &vitals.0 {
        status.push_str(&format!("  {} {value}", vital.name()));
    }
    let suggestion = settings
        .suggestion
        .as_ref()
        .filter(|suggestion| !enabled.contains(&suggestion.plugin))
        .map(|suggestion| {
            let named_by = match suggestion.source {
                Source::Mssp => "the server's MSSP name",
                Source::GmcpHello => "the server's GMCP hello",
                Source::Host => "the server's host",
            };
            format!(
                "Game plugin suggested by {named_by}: {0}. Run with --enable {0} to turn it on.",
                suggestion.plugin
            )
        });
    std::iter::once(status)
        .chain(suggestion)
        .chain(settings.notices.last().cloned())
        .map(SharedString::from)
        .collect()
}

fn connection_text(settings: &SettingsState) -> String {
    let server = settings.server.as_deref().unwrap_or("the server");
    match &settings.connection {
        ConnectionStatus::Idle => format!("Not connected to {server}"),
        ConnectionStatus::Connecting => format!("Connecting to {server}"),
        ConnectionStatus::Connected => format!("Connected to {server}"),
        ConnectionStatus::Closed(reason) => format!("Disconnected from {server}: {reason}"),
    }
}

impl Render for StatusLines {
    fn render(&mut self, _: &mut Window, cx: &mut Context<StatusLines>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        div()
            .w_full()
            .text_size(FONT_SIZE)
            .text_color(theme.placeholder)
            .children(self.lines.iter().enumerate().map(|(number, line)| {
                div()
                    .id(("status-line", number))
                    .role(Role::Label)
                    .aria_value(line.clone())
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(line.clone())
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mudling_core::defaults::detect::Suggestion;
    use mudling_core::defaults::hud::Vital;
    use mudling_core::net::DisconnectReason;

    fn avatar() -> PluginId {
        PluginId::try_from("avatar".to_owned()).unwrap()
    }

    #[test]
    fn the_status_line_says_how_the_connection_stands_and_the_vitals() {
        let settings = SettingsState {
            server: Some("mud.example".to_owned()),
            connection: ConnectionStatus::Closed(DisconnectReason::ClosedByServer),
            ..SettingsState::default()
        };
        let vitals = VitalsState([(Vital::Hp, 11), (Vital::MaxHp, 30)].into());
        assert_eq!(
            lines_for(&settings, &vitals, &[], false),
            ["Disconnected from mud.example: closed by server  hp 11  maxhp 30"]
        );
    }

    #[test]
    fn a_suggestion_shows_until_the_plugin_is_on() {
        let settings = SettingsState {
            suggestion: Some(Suggestion {
                plugin: avatar(),
                source: Source::Mssp,
            }),
            ..SettingsState::default()
        };
        let vitals = VitalsState::default();
        let lines = lines_for(&settings, &vitals, &[], true);
        assert_eq!(lines[0], "Replaying a recording");
        assert!(lines[1].contains("--enable avatar"));
        assert_eq!(lines_for(&settings, &vitals, &[avatar()], true).len(), 1);
    }
}
