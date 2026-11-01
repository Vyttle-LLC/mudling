//! The Plugins section: what's installed for this session, by layer, and the
//! session's notices.
//!
//! It shows what the core publishes today, and that's less than the canvas:
//! no description or source for a plugin, and the switches only report. Each
//! plugin's cost per line is sampled, so it shows after a few hundred lines. A
//! plugin the core turned off has a banner that can turn it on again.

use super::{Pane, SettingsSheet, SheetEvent, muted_line, section_heading, text_button};
use crate::assets::{Icon, UI_FONT};
use crate::theme::Theme;
use gpui::{
    Context, Div, FontWeight, HighlightStyle, Pixels, Role, SharedString, Stateful, StyledText,
    div, prelude::*, px,
};
use mudling_core::defaults;
use mudling_core::plugin::luau::{DisabledReason, OVERRUNS_TO_TURN_OFF, PluginDisabled};
use mudling_core::plugin::{Layer, Plugin, PluginCost, PluginId, ServerInfo};
use mudling_core::stats::Summary;

const SWITCH_WIDTH: Pixels = px(40.);
const SWITCH_HEIGHT: Pixels = px(22.);
const SWITCH_KNOB: Pixels = px(16.);
const SWITCH_NOTE: &str = "The switches show what's running. Changing them isn't here yet.";
const NO_SESSION: &str = "Connect to a server to see its plugins.";
const NOT_MEASURED: &str = "Costs show here once a few hundred lines have been read.";
/// PRD §6: one line with all its plugins, and what any one of them may add.
const LINE_BUDGET_US: f64 = 20.;
const PLUGIN_BUDGET_US: f64 = 5.;
/// Room for a cost like "12.3 µs".
const COST_WIDTH: Pixels = px(64.);
const LAYERS_NOTE: &str = "Your overrides win over the game plugin, which wins over the defaults.";
const OVERRAN: &str = " One of its hooks ran past its time limit three times this session.";
/// Under this, a banner's buttons wrap below its text.
const BANNER_TEXT_WIDTH: Pixels = px(220.);

// The banner spells the count out, as the canvas does.
const _: () = assert!(OVERRUNS_TO_TURN_OFF == 3);

/// A plugin as this section shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginRow {
    pub id: PluginId,
    pub name: SharedString,
    pub layer: Layer,
    /// The canvas's description, until manifests carry one.
    pub detail: SharedString,
    pub on: bool,
}

impl PluginRow {
    fn of(plugin: &Plugin, on: bool) -> PluginRow {
        PluginRow {
            id: plugin.id().clone(),
            name: plugin.manifest.name.clone().into(),
            layer: plugin.layer(),
            detail: format!(
                "Version {} · {}",
                plugin.manifest.version, plugin.manifest.license
            )
            .into(),
            on,
        }
    }

    /// The session's plugins on `host`: the built-in defaults, then what's
    /// installed, on as the session would run them.
    pub fn for_session(plugins: &[Plugin], enabled: &[PluginId], host: &str) -> Vec<PluginRow> {
        let server = ServerInfo {
            host,
            mssp_name: None,
        };
        let installed = plugins.iter().map(|plugin| {
            let on = match plugin.layer() {
                Layer::Override => plugin.manifest.matches_server(server),
                _ => enabled.contains(plugin.id()),
            };
            PluginRow::of(plugin, on)
        });
        let built_in = PluginRow::of(&defaults::plugin(), true);
        std::iter::once(built_in).chain(installed).collect()
    }

    /// Whether it's running. The core's costs list what a connection runs;
    /// before one has, this is how the session would run it.
    fn switched_on(&self, costs: &[PluginCost], turned_off: &[PluginDisabled]) -> bool {
        let running = match costs {
            [] => self.on,
            costs => costs.iter().any(|cost| cost.plugin == self.id),
        };
        running && !turned_off.iter().any(|off| off.plugin == self.id)
    }

    /// What it costs a line in µs at p99, once sampled.
    fn cost(&self, costs: &[PluginCost]) -> Option<f64> {
        let sampled = costs.iter().find(|cost| cost.plugin == self.id)?;
        sampled.per_line.as_ref().map(|cost| cost.p99_us)
    }
}

fn micros(cost: f64) -> String {
    format!("{cost:.1} µs")
}

/// What each layer's plugins that run cost a line, top layer first, and what
/// they cost together against the budget. The whole comes from the one pass,
/// so it's not the layers added up: each of those times its rules alone.
fn cost_lines(
    rows: &[PluginRow],
    costs: &[PluginCost],
    turned_off: &[PluginDisabled],
    whole: &Summary,
) -> (String, String) {
    let layers = [Layer::Override, Layer::Game, Layer::Default].map(|layer| {
        let running = rows.iter().filter(|row| row.layer == layer);
        let cost = running
            .filter(|row| row.switched_on(costs, turned_off))
            .filter_map(|row| row.cost(costs))
            .fold(0., |total, cost| total + cost);
        format!("{} {}", layer_name(layer), micros(cost))
    });
    (
        layers.join(" · "),
        format!(
            "All plugins {} of {LINE_BUDGET_US} µs a line. One plugin may add \
             {PLUGIN_BUDGET_US} µs.",
            micros(whole.p99_us),
        ),
    )
}

fn layer_heading(layer: Layer) -> &'static str {
    match layer {
        Layer::Override => "Your overrides",
        Layer::Game => "Game",
        Layer::Default => "Defaults · work on any GMCP server",
    }
}

fn layer_name(layer: Layer) -> &'static str {
    match layer {
        Layer::Override => "Overrides",
        Layer::Game => "Game",
        Layer::Default => "Defaults",
    }
}

fn layer_badge(layer: Layer) -> &'static str {
    match layer {
        Layer::Override => "Local",
        Layer::Game => "Game plugin",
        Layer::Default => "Built in",
    }
}

/// The switch's look. It only reports, so it's no control yet.
fn switch(on: bool, theme: &Theme) -> Div {
    let knob =
        div()
            .size(SWITCH_KNOB)
            .rounded_sm()
            .bg(if on { theme.on_accent } else { theme.text });
    div()
        .flex_none()
        .w(SWITCH_WIDTH)
        .h(SWITCH_HEIGHT)
        .p(px(3.))
        .flex()
        .when(on, |track| track.justify_end())
        .rounded_md()
        .bg(if on { theme.accent } else { theme.segment })
        .child(knob)
}

fn plugin_row(
    row: &PluginRow,
    switched_on: bool,
    overran: bool,
    cost: Option<f64>,
    theme: &Theme,
) -> impl IntoElement + use<> {
    let state = match (switched_on, overran) {
        (true, _) => "On",
        (false, true) => "Turned off · over budget",
        (false, false) => "Off",
    };
    let over_limit = cost.is_some_and(|cost| cost > PLUGIN_BUDGET_US);
    let cost_said = match cost {
        Some(cost) if over_limit => format!(
            " {} a line, over the {PLUGIN_BUDGET_US} µs limit.",
            micros(cost)
        ),
        Some(cost) => format!(" {} a line.", micros(cost)),
        None => String::new(),
    };
    let id = row.id.clone();
    div()
        .id(SharedString::from(format!("plugin-{}", row.id)))
        .debug_selector(move || format!("plugin-{id}"))
        .role(Role::Label)
        .aria_value(format!(
            "{}, {}: {state}. {}.{cost_said}",
            row.name,
            layer_badge(row.layer),
            row.detail,
        ))
        .flex()
        .items_center()
        .gap_3()
        .px_5()
        .py_3()
        .border_t_1()
        .border_color(theme.divider)
        .font_family(UI_FONT)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_x_2()
                        .text_size(px(15.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(row.name.clone())
                        .when(overran, |name| {
                            name.child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(theme.warning)
                                    .child("Turned off · over budget"),
                            )
                        }),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(theme.text_muted)
                        .child(row.detail.clone()),
                ),
        )
        .child(
            div()
                .flex_none()
                .px_2()
                .rounded_sm()
                .border_1()
                .border_color(theme.border)
                .text_size(px(12.))
                .text_color(theme.text_muted)
                .child(layer_badge(row.layer)),
        )
        .child(
            div()
                .flex_none()
                .w(COST_WIDTH)
                .flex()
                .justify_end()
                .text_size(px(14.))
                .text_color(if over_limit {
                    theme.warning
                } else {
                    theme.text_muted
                })
                .child(cost.map_or_else(String::new, micros)),
        )
        .child(switch(switched_on, theme))
}

/// A layer's heading. The first has the costs' column heading too.
fn layer_heading_row(layer: Layer, first: bool, theme: &Theme) -> impl IntoElement + use<> {
    div()
        .px_5()
        .flex()
        .items_end()
        .justify_between()
        .child(section_heading(layer_heading(layer), theme))
        .when(first, |row| {
            row.child(
                div()
                    .pb_2()
                    .mr(SWITCH_WIDTH + px(12.))
                    .font_family(UI_FONT)
                    .text_size(px(13.))
                    .text_color(theme.text_muted)
                    .child("per line"),
            )
        })
}

impl SettingsSheet {
    fn display_name(&self, id: &str) -> SharedString {
        let row = self.plugins.iter().find(|row| row.id.as_str() == id);
        row.map_or_else(|| id.to_owned().into(), |row| row.name.clone())
    }

    fn toggle_details(&mut self, notice: &str, cx: &mut Context<SettingsSheet>) {
        self.details = (self.details.as_deref() != Some(notice)).then(|| notice.to_owned());
        cx.notify();
    }

    /// A notice's banner. One about a plugin turned off, in `off`, has the
    /// canvas's words and its two buttons.
    fn notice_banner(
        &self,
        number: usize,
        notice: &str,
        off: Option<&PluginDisabled>,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) -> Stateful<Div> {
        let open = self.details.as_deref() == Some(notice);
        let (lead, rest) = match off {
            Some(off) => (
                format!("{} was turned off.", self.display_name(off.plugin.as_str())),
                match off.reason {
                    DisabledReason::Overran(_) => OVERRAN,
                },
            ),
            None => (notice.to_owned(), ""),
        };
        let bold = HighlightStyle {
            font_weight: Some(FontWeight::MEDIUM),
            ..HighlightStyle::default()
        };
        let text = SharedString::from(format!("{lead}{rest}"));
        let summary = div()
            .flex_1()
            .min_w(BANNER_TEXT_WIDTH)
            .text_size(px(14.))
            .text_color(theme.text)
            .child(StyledText::new(text.clone()).with_highlights([(0..lead.len(), bold)]));
        let buttons = off.map(|off| {
            let details = text_button(("notice-details", number), "Details", &pane.stop(cx), theme)
                .aria_expanded(open)
                .on_click(cx.listener({
                    let notice = notice.to_owned();
                    move |sheet, _, _, cx| sheet.toggle_details(&notice, cx)
                }));
            let turn_on = text_button(
                ("notice-turn-on", number),
                "Turn on again",
                &pane.stop(cx),
                theme,
            )
            .on_click(cx.listener({
                let plugin = off.plugin.clone();
                move |_, _, _, cx| cx.emit(SheetEvent::TurnOn(plugin.clone()))
            }));
            div()
                .flex_none()
                .flex()
                .gap_2()
                .child(details)
                .child(turn_on)
        });
        div()
            .id(("notice", number))
            .debug_selector(move || format!("notice-{number}"))
            .role(Role::Label)
            .aria_value(text)
            .mb_3()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(theme.warning)
            .bg(theme.warning_ground)
            .font_family(UI_FONT)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_3()
                    .child(
                        Icon::Warning
                            .svg()
                            .flex_none()
                            .size(px(20.))
                            .text_color(theme.warning),
                    )
                    .child(summary)
                    .children(buttons),
            )
            .when(open, |banner| {
                banner.child(
                    muted_line(format!("{notice}."), theme)
                        .mt_2()
                        .pb_0()
                        .text_size(px(13.)),
                )
            })
    }

    pub(super) fn fill_plugins(
        &self,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) {
        for (number, off) in self.turned_off.iter().enumerate() {
            let notice = off.to_string();
            let banner = self.notice_banner(number, &notice, Some(off), pane, theme, cx);
            pane.row(banner);
        }
        let first = self.turned_off.len();
        for (number, notice) in self.notices.iter().enumerate() {
            let banner = self.notice_banner(first + number, notice, None, pane, theme, cx);
            pane.row(banner);
        }
        if self.plugins.is_empty() {
            pane.row(muted_line(NO_SESSION, theme));
            return;
        }
        let scope = match &self.server {
            Some(server) => format!("Plugins for {server}. {LAYERS_NOTE}"),
            None => LAYERS_NOTE.to_owned(),
        };
        pane.row(muted_line(scope, theme));
        pane.row(muted_line(SWITCH_NOTE, theme));
        let mut card = div()
            .id("plugin-layers")
            .mt_3()
            .rounded_lg()
            .border_1()
            .border_color(theme.border);
        let mut first = true;
        for layer in [Layer::Override, Layer::Game, Layer::Default] {
            let in_layer: Vec<&PluginRow> =
                self.plugins.iter().filter(|p| p.layer == layer).collect();
            if in_layer.is_empty() {
                continue;
            }
            let rows = in_layer.into_iter().map(|row| {
                plugin_row(
                    row,
                    row.switched_on(&self.costs, &self.turned_off),
                    self.turned_off.iter().any(|off| off.plugin == row.id),
                    row.cost(&self.costs),
                    theme,
                )
            });
            card = card
                .child(layer_heading_row(layer, first, theme))
                .children(rows);
            first = false;
        }
        pane.row(card.child(self.cost_footer(theme)));
    }

    /// The layers' costs and the total against the budgets, closing the card.
    fn cost_footer(&self, theme: &Theme) -> Stateful<Div> {
        let (layers, total) = match &self.line_cost {
            Some(whole) => cost_lines(&self.plugins, &self.costs, &self.turned_off, whole),
            None => (String::new(), NOT_MEASURED.to_owned()),
        };
        let over_budget = (self.line_cost.as_ref()).is_some_and(|w| w.p99_us > LINE_BUDGET_US);
        div()
            .id("plugin-costs")
            .debug_selector(|| "plugin-costs".to_owned())
            .role(Role::Label)
            .aria_value(format!("{layers}. {total}"))
            .px_5()
            .py_3()
            .border_t_1()
            .border_color(theme.divider)
            .font_family(UI_FONT)
            .text_size(px(13.))
            .text_color(theme.text_muted)
            .when(!layers.is_empty(), |footer| footer.child(layers))
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if over_budget {
                        theme.warning
                    } else {
                        theme.text
                    })
                    .child(total),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mudling_core::stats::Summary;
    use std::time::Duration;

    fn plugin(toml: &'static str) -> Plugin {
        Plugin::from_files(move |file| match file {
            "plugin.toml" => Ok(toml.to_owned()),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                file.to_owned(),
            )),
        })
        .unwrap()
    }

    const A_GAME: &str = "id = \"a-game\"\nname = \"A game\"\nversion = \"1.0.0\"\napi = 1\n\
                          license = \"MIT\"\nlayer = \"game\"\n\n[servers]\nhosts = [\"a.example\"]\n";

    #[test]
    fn the_session_lists_the_defaults_then_what_is_installed_and_on() {
        let game = plugin(A_GAME);
        let off = PluginRow::for_session(std::slice::from_ref(&game), &[], "a.example");
        assert_eq!(off.len(), 2);
        assert_eq!(off[0].layer, Layer::Default);
        assert!(off[0].on);
        assert_eq!((off[1].name.as_str(), off[1].on), ("A game", false));

        let on = PluginRow::for_session(
            std::slice::from_ref(&game),
            &[game.id().clone()],
            "a.example",
        );
        assert!(on[1].on);
    }

    #[test]
    fn a_switch_follows_what_a_connection_runs() {
        let game = plugin(A_GAME);
        let id = game.id().clone();
        let row = &PluginRow::for_session(std::slice::from_ref(&game), &[], "a.example")[1];
        let cost = |per_line| PluginCost {
            plugin: id.clone(),
            sampled_lines: 1,
            per_line,
        };
        assert!(
            !row.switched_on(&[], &[]),
            "before a connection, the config"
        );
        assert!(row.switched_on(&[cost(None)], &[]), "in the host, it runs");
        let off = PluginDisabled {
            plugin: id.clone(),
            reason: DisabledReason::Overran(3),
        };
        assert!(!row.switched_on(&[cost(None)], &[off]));

        assert_eq!(row.cost(&[cost(None)]), None);
        let summary = Summary::of(&[Duration::from_micros(2)]);
        assert_eq!(row.cost(&[cost(summary)]), Some(2.));
    }

    #[test]
    fn layers_count_what_runs_and_the_total_is_the_whole_read() {
        let game = plugin(A_GAME);
        let id = game.id().clone();
        let rows = PluginRow::for_session(
            std::slice::from_ref(&game),
            std::slice::from_ref(&id),
            "a.example",
        );
        let of = |plugin: &PluginId, micros| PluginCost {
            plugin: plugin.clone(),
            sampled_lines: 1,
            per_line: Summary::of(&[Duration::from_micros(micros)]),
        };
        let costs = [of(&rows[0].id, 1), of(&id, 9)];
        let whole = Summary::of(&[Duration::from_micros(7)]).unwrap();

        let (layers, total) = cost_lines(&rows, &costs, &[], &whole);
        assert_eq!(layers, "Overrides 0.0 µs · Game 9.0 µs · Defaults 1.0 µs");
        assert_eq!(
            total,
            "All plugins 7.0 µs of 20 µs a line. One plugin may add 5 µs."
        );

        let off = PluginDisabled {
            plugin: id.clone(),
            reason: DisabledReason::Overran(3),
        };
        let (layers, _) = cost_lines(&rows, &costs, &[off], &whole);
        assert!(layers.contains("Game 0.0 µs"), "{layers}");
    }
}
