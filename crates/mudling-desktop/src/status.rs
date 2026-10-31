//! Quiet lines above the command bar: the game detection's suggestion and the
//! newest notice. The connection's words live in the title bar.

use crate::assets::UI_FONT;
use crate::theme::Theme;
use gpui::{Context, Pixels, Role, SharedString, Window, div, prelude::*, px};
use mudling_core::defaults::detect::Source;
use mudling_core::plugin::PluginId;
use mudling_core::store::SettingsState;

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
}

/// What the lines say.
pub fn lines_for(settings: &SettingsState, enabled: &[PluginId]) -> Vec<SharedString> {
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
    suggestion
        .into_iter()
        .chain(settings.notices.last().cloned())
        .map(SharedString::from)
        .collect()
}

impl Render for StatusLines {
    fn render(&mut self, _: &mut Window, cx: &mut Context<StatusLines>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        div()
            .w_full()
            .font_family(UI_FONT)
            .text_size(FONT_SIZE)
            .text_color(theme.text_muted)
            .children(self.lines.iter().enumerate().map(|(number, line)| {
                div()
                    .id(("status-line", number))
                    .role(Role::Label)
                    .aria_value(line.clone())
                    .pb_1()
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

    fn avatar() -> PluginId {
        PluginId::try_from("avatar".to_owned()).unwrap()
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
        assert!(lines_for(&settings, &[])[0].contains("--enable avatar"));
        assert!(lines_for(&settings, &[avatar()]).is_empty());
    }
}
