//! A server's plugins: a page of Servers & characters, with the suggested game
//! plugin, a switch for each plugin and the player's overrides (PRD §11).
//!
//! A switch is saved for that server at once. A session reads its plugins when
//! it opens, so on the server being played it takes effect at the next connect.

use super::plugins::{PluginRow, cost_label, rule_counts};
use super::{Page, Pane, Section, SettingsSheet, muted_line, section_heading, text_button};
use crate::assets::UI_FONT;
use crate::theme::Theme;
use gpui::{Context, FontWeight, Role, SharedString, Stateful, div, prelude::*, px};
use gpui::{Div, Pixels, Window};
use mudling_core::defaults::detect::Source;
use mudling_core::plugin::{Layer, PluginId};

const LAYERS_NOTE: &str =
    "Your overrides win over the game plugin, and the game plugin wins over the defaults.";
const RULES_LIVE_ELSEWHERE: &str = "Edit them in Triggers and aliases.";
const ALWAYS_ON: &str = "always on";
const SUGGESTION_BUTTON_ROOM: Pixels = px(96.);

/// The game plugin detection suggests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuggestedPlugin {
    pub plugin: PluginId,
    pub name: SharedString,
    pub source: Source,
}

impl SuggestedPlugin {
    fn reason(&self) -> &'static str {
        match self.source {
            Source::Mssp | Source::GmcpHello => "MUDling recognized it from the game's greeting.",
            Source::Host => "MUDling recognized it from the server's address.",
        }
    }
}

/// What the page shows of one server, read off the UI thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerPlugins {
    pub host: SharedString,
    pub plugins: Vec<PluginRow>,
    pub suggested: Option<SuggestedPlugin>,
    /// The triggers and aliases the player keeps for this server.
    pub triggers: usize,
    pub aliases: usize,
}

impl ServerPlugins {
    /// The suggestion, until the plugin it names is on.
    fn offer(&self) -> Option<&SuggestedPlugin> {
        let on = |suggested: &&SuggestedPlugin| {
            (self.plugins.iter()).any(|row| row.id == suggested.plugin && row.on)
        };
        self.suggested.as_ref().filter(|s| !on(s))
    }
}

fn card(id: &'static str, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .mb_3()
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
}

/// A row's words: what it is, and a smaller line about it.
fn row_words(title: SharedString, about: SharedString, theme: &Theme) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .child(
            div()
                .text_size(px(15.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.text)
                .child(title),
        )
        .child(
            div()
                .text_size(px(13.))
                .text_color(theme.text_muted)
                .child(about),
        )
}

fn row_shape(first: bool, theme: &Theme) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .py_2()
        .font_family(UI_FONT)
        .when(!first, |row| row.border_t_1())
        .border_color(theme.divider)
}

impl SettingsSheet {
    /// Shows `plugins`, moving onto the page if the player's still waiting.
    pub fn show_server_plugins(
        &mut self,
        plugins: ServerPlugins,
        window: &mut Window,
        cx: &mut Context<SettingsSheet>,
    ) {
        if self.section == Section::Servers {
            self.go_to_page(Page::Server(plugins), window, cx);
        }
    }

    fn plugin_row_for_server(
        &self,
        row: &PluginRow,
        first: bool,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) -> impl IntoElement + use<> {
        let about: SharedString = match row.layer {
            _ if row.always_on => format!("{} · {ALWAYS_ON}", row.version).into(),
            Layer::Override => format!("{} · {}", row.holds, row.version).into(),
            _ => row.version.clone(),
        };
        let cost = self.page_cost(row);
        let said = match cost {
            Some(cost) => format!("{}, {about}, {cost:.1} µs a line", row.name),
            None => format!("{}, {about}", row.name),
        };
        let words = div()
            .id(SharedString::from(format!("server-plugin-{}", row.id)))
            .role(Role::Label)
            .aria_value(said)
            .flex_1()
            .min_w_0()
            .child(row_words(row.name.clone(), about, theme));
        row_shape(first, theme)
            .child(words)
            .child(cost_label(cost, theme))
            .when(!row.always_on, |shape| {
                shape.child(self.plugin_switch(row, row.on, pane, theme, cx))
            })
    }

    /// What a plugin costs here: only the server being played has been timed.
    fn page_cost(&self, row: &PluginRow) -> Option<f64> {
        let playing = |page: &ServerPlugins| self.server.as_ref() == Some(&page.host);
        match &self.page {
            Page::Server(page) if playing(page) => row.cost(&self.costs),
            _ => None,
        }
    }

    pub(super) fn fill_server_plugins(
        &self,
        page: &ServerPlugins,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) {
        let back = self.back_button(Section::Servers, pane, theme, cx);
        pane.leading = Some(back.into_any_element());
        pane.row(muted_line(LAYERS_NOTE, theme));
        if let Some(problem) = self.servers_problem.clone() {
            pane.row(
                muted_line(problem, theme)
                    .role(Role::Alert)
                    .text_color(theme.error),
            );
        }
        if self.reconnect_to_apply {
            pane.row(self.reconnect_note(theme));
        }
        if let Some(offer) = page.offer() {
            let name = format!("{} is suggested for this server", offer.name);
            let words = div()
                .id("suggested-plugin")
                .debug_selector(|| "suggested-plugin".to_owned())
                .role(Role::Label)
                .aria_value(format!("{name}. {}", offer.reason()))
                .flex_1()
                .min_w_0()
                .child(row_words(name.into(), offer.reason().into(), theme));
            let turn_on = {
                let plugin = offer.plugin.clone();
                let host = page.host.clone();
                text_button("suggested-turn-on", "Turn on", &pane.stop(cx), theme)
                    .aria_label(format!("Turn on {}", offer.name))
                    .min_w(SUGGESTION_BUTTON_ROOM)
                    .text_color(theme.accent)
                    .border_color(theme.accent)
                    .on_click(cx.listener(move |sheet, _, _, cx| {
                        sheet.set_plugin(host.clone(), plugin.clone(), true, cx);
                    }))
            };
            pane.row(
                card("plugin-suggestion", theme)
                    .mt_1()
                    .border_color(theme.accent)
                    .child(row_shape(true, theme).child(words).child(turn_on)),
            );
        }
        pane.row(section_heading("Installed", theme));
        let installed = page.plugins.iter().filter(|p| p.layer != Layer::Override);
        let mut rows = card("installed-plugins", theme);
        for (number, row) in installed.enumerate() {
            rows = rows.child(self.plugin_row_for_server(row, number == 0, pane, theme, cx));
        }
        pane.row(rows);

        pane.row(section_heading("Your overrides", theme));
        let rules = rule_counts(page.triggers, page.aliases);
        let words = div()
            .id("server-rules")
            .role(Role::Label)
            .aria_value(format!("{rules}. {RULES_LIVE_ELSEWHERE}"))
            .flex_1()
            .min_w_0()
            .child(row_words(rules.into(), RULES_LIVE_ELSEWHERE.into(), theme));
        let mut overrides =
            card("server-overrides", theme).child(row_shape(true, theme).child(words));
        for row in page.plugins.iter().filter(|p| p.layer == Layer::Override) {
            overrides = overrides.child(self.plugin_row_for_server(row, false, pane, theme, cx));
        }
        pane.row(overrides);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mudling_core::plugin::Layer;

    fn row(id: &str, on: bool) -> PluginRow {
        PluginRow {
            id: PluginId::try_from(id.to_owned()).unwrap(),
            name: id.to_owned().into(),
            layer: Layer::Game,
            version: "1.0.0 · MIT".into(),
            holds: "".into(),
            always_on: false,
            on,
        }
    }

    #[test]
    fn the_suggestion_goes_once_its_plugin_is_on() {
        let suggested = |source| SuggestedPlugin {
            plugin: row("game", false).id,
            name: "Game".into(),
            source,
        };
        let mut page = ServerPlugins {
            host: "mud.example".into(),
            plugins: vec![row("game", false)],
            suggested: Some(suggested(Source::Host)),
            triggers: 0,
            aliases: 0,
        };
        assert!(page.offer().is_some());
        page.plugins[0].on = true;
        assert!(page.offer().is_none());

        assert!(suggested(Source::Host).reason().contains("address"));
        assert!(suggested(Source::Mssp).reason().contains("greeting"));
        assert!(suggested(Source::GmcpHello).reason().contains("greeting"));
    }
}
