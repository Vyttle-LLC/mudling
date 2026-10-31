//! The Plugins section: what's installed for this session, by layer, and the
//! session's notices.
//!
//! It shows what the core publishes today, and that's less than the canvas:
//! no cost per line, no description or source for a plugin, and no way to turn
//! one on or off from here. A plugin turned off for overrunning is known only
//! by its notice's wording.

use super::{Pane, SettingsSheet, muted_line, section_heading, text_button};
use crate::assets::{Icon, UI_FONT};
use crate::theme::Theme;
use gpui::{
    Context, Div, FontWeight, HighlightStyle, Pixels, Role, SharedString, Stateful, StyledText,
    div, prelude::*, px,
};
use mudling_core::defaults;
use mudling_core::plugin::luau::OVERRUNS_TO_TURN_OFF;
use mudling_core::plugin::{Layer, Plugin, PluginId, ServerInfo};

const SWITCH_WIDTH: Pixels = px(40.);
const SWITCH_HEIGHT: Pixels = px(22.);
const SWITCH_KNOB: Pixels = px(16.);
const SWITCH_NOTE: &str = "The switches show what's running. Changing them isn't here yet.";
const NO_SESSION: &str = "Connect to a server to see its plugins.";
const LAYERS_NOTE: &str = "Your overrides win over the game plugin, which wins over the defaults.";
/// How the core words the notice for a plugin it turned off.
const TURNED_OFF: &str = " was turned off: ";
const OVERRAN: &str = " One of its hooks ran past its time limit three times this session.";
/// Under this, a banner's buttons wrap below its text.
const BANNER_TEXT_WIDTH: Pixels = px(220.);
const TURN_ON_AGAIN_NOTE: &str = "Turning it on from here isn't ready yet. For now, restart \
                                  MUDling to turn it on again.";

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

    fn switched_on(&self, turned_off: &[&str]) -> bool {
        self.on && !turned_off.contains(&self.id.as_str())
    }
}

/// The id in a notice saying a plugin was turned off.
fn turned_off(notice: &str) -> Option<&str> {
    notice.split_once(TURNED_OFF).map(|(plugin, _)| plugin)
}

fn layer_heading(layer: Layer) -> &'static str {
    match layer {
        Layer::Override => "Your overrides",
        Layer::Game => "Game",
        Layer::Default => "Defaults · work on any GMCP server",
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
    theme: &Theme,
) -> impl IntoElement + use<> {
    let state = match (switched_on, overran) {
        (true, _) => "On",
        (false, true) => "Turned off · over budget",
        (false, false) => "Off",
    };
    let id = row.id.clone();
    div()
        .id(SharedString::from(format!("plugin-{}", row.id)))
        .debug_selector(move || format!("plugin-{id}"))
        .role(Role::Label)
        .aria_value(format!("{}, {}: {state}", row.name, layer_badge(row.layer)))
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
        .child(switch(switched_on, theme))
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

    /// A notice's banner. One about a plugin turned off has the canvas's
    /// words and its two buttons.
    fn notice_banner(
        &self,
        number: usize,
        notice: &str,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) -> Stateful<Div> {
        let open = self.details.as_deref() == Some(notice);
        let (lead, rest) = match turned_off(notice) {
            Some(plugin) => (
                format!("{} was turned off.", self.display_name(plugin)),
                OVERRAN,
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
        let buttons = turned_off(notice).map(|_| {
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
            .aria_description(TURN_ON_AGAIN_NOTE)
            .on_click(cx.listener({
                let notice = notice.to_owned();
                move |sheet, _, _, cx| {
                    sheet.details = Some(notice.clone());
                    cx.notify();
                }
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
                    div()
                        .mt_2()
                        .text_size(px(13.))
                        .text_color(theme.text_muted)
                        .child(format!("{notice}. {TURN_ON_AGAIN_NOTE}")),
                )
            })
    }

    pub(super) fn fill_plugins(
        &self,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) {
        for (number, notice) in self.notices.iter().enumerate() {
            let banner = self.notice_banner(number, notice, pane, theme, cx);
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
        let off: Vec<&str> = self.notices.iter().filter_map(|n| turned_off(n)).collect();
        let mut card = div()
            .id("plugin-layers")
            .mt_3()
            .rounded_lg()
            .border_1()
            .border_color(theme.border);
        for layer in [Layer::Override, Layer::Game, Layer::Default] {
            let in_layer: Vec<&PluginRow> =
                self.plugins.iter().filter(|p| p.layer == layer).collect();
            if in_layer.is_empty() {
                continue;
            }
            let rows = in_layer.into_iter().map(|row| {
                plugin_row(
                    row,
                    row.switched_on(&off),
                    off.contains(&row.id.as_str()),
                    theme,
                )
            });
            card = card
                .child(
                    div()
                        .px_5()
                        .child(section_heading(layer_heading(layer), theme)),
                )
                .children(rows);
        }
        pane.row(card);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_core_notice_names_the_plugin_it_turned_off() {
        // The words of `Engine::hook_output`'s `PluginDisabled` arm.
        assert_eq!(
            turned_off("a-game was turned off: Overran(3)"),
            Some("a-game")
        );
        assert_eq!(turned_off("couldn't save the map: disk full"), None);
    }
}
