//! The row above the command bar that offers what a session can do next:
//! reconnect once the connection has ended, and turn on the game plugin
//! detection suggests. Detection only suggests: the player's click turns the
//! plugin on (PRD §11).

use crate::assets::UI_FONT;
use crate::empty_state::text_button;
use crate::launcher::Launcher;
use crate::shell::FocusCommandBar;
use crate::theme::Theme;
use crate::title_bar::FocusNextControl;
use gpui::{
    App, Context, EventEmitter, KeyBinding, Pixels, Role, SharedString, Window, div, prelude::*, px,
};
use mudling_core::defaults::detect::{Source, Suggestion};
use mudling_core::net::DisconnectReason;
use mudling_core::plugin::PluginId;
use mudling_core::store::{ConnectionStatus, SettingsState};

const CONTEXT: &str = "SessionBar";
const FONT_SIZE: Pixels = px(12.);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", FocusNextControl, Some(CONTEXT)),
        KeyBinding::new("escape", FocusCommandBar, Some(CONTEXT)),
    ]);
}

pub enum SessionBarEvent {
    Reconnect,
    ChooseServer,
    TurnOn(PluginId),
}

/// What's on offer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Offers {
    /// How the connection ended.
    ended: Option<SharedString>,
    suggestion: Option<SharedString>,
    /// The plugin one click turns on; a replay can only say what's suggested.
    turn_on: Option<PluginId>,
    /// Whether there's anywhere else to go: a launcher to choose from.
    can_choose: bool,
}

impl Offers {
    /// With no `launcher` to open a new session, a suggestion is only words.
    pub fn of(
        settings: &SettingsState,
        enabled: &[PluginId],
        replaying: bool,
        launcher: Option<&Launcher>,
    ) -> Offers {
        let ended = match &settings.connection {
            ConnectionStatus::Closed(reason) if !replaying => Some(ended_text(reason).into()),
            _ => None,
        };
        let suggested = settings
            .suggestion
            .as_ref()
            .filter(|suggestion| !enabled.contains(&suggestion.plugin));
        let can_click = launcher.is_some() && !replaying;
        let name = |plugin| launcher.and_then(|launcher| launcher.plugin_name(plugin));
        Offers {
            ended,
            suggestion: suggested.map(|suggestion| {
                let words = suggestion_text(suggestion, name(&suggestion.plugin));
                if can_click {
                    words.into()
                } else {
                    let plugin = &suggestion.plugin;
                    format!("{words} Run with --enable {plugin} to turn it on.").into()
                }
            }),
            turn_on: suggested
                .filter(|_| can_click)
                .map(|suggestion| suggestion.plugin.clone()),
            can_choose: launcher.is_some(),
        }
    }
}

fn suggestion_text(suggestion: &Suggestion, name: Option<&str>) -> String {
    let named_by = match suggestion.source {
        Source::Mssp => "the server's MSSP name",
        Source::GmcpHello => "the server's GMCP hello",
        Source::Host => "the server's host",
    };
    let plugin = name.unwrap_or(suggestion.plugin.as_str());
    format!("Game plugin suggested by {named_by}: {plugin}.")
}

fn ended_text(reason: &DisconnectReason) -> String {
    match reason {
        DisconnectReason::ClosedByClient => "Disconnected.".to_owned(),
        DisconnectReason::ClosedByServer => "The server closed the connection.".to_owned(),
        DisconnectReason::Failed(message) => format!("The connection failed: {message}"),
    }
}

#[derive(Default)]
pub struct SessionBar {
    offers: Offers,
}

impl EventEmitter<SessionBarEvent> for SessionBar {}

impl SessionBar {
    pub fn show(&mut self, offers: Offers, cx: &mut Context<SessionBar>) {
        if offers != self.offers {
            self.offers = offers;
            cx.notify();
        }
    }

    #[cfg(test)]
    pub fn offers(&self) -> &Offers {
        &self.offers
    }
}

fn row(id: &'static str, role: Role, text: SharedString) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .flex()
        .items_center()
        .gap_2()
        .pb_1()
        .child(
            div()
                .id("words")
                .role(role)
                .aria_value(text.clone())
                .flex_1()
                .min_w_0()
                .child(text),
        )
}

impl Render for SessionBar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<SessionBar>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let suggestion = self.offers.suggestion.clone().map(|words| {
            let turn_on = self.offers.turn_on.clone().map(|plugin| {
                text_button("turn-on-plugin", "Turn on and reconnect", false, &theme).on_click(
                    cx.listener(move |_, _, _, cx| {
                        cx.emit(SessionBarEvent::TurnOn(plugin.clone()));
                    }),
                )
            });
            row("plugin-suggestion", Role::Label, words).children(turn_on)
        });
        let ended = self.offers.ended.clone().map(|words| {
            let choose = self.offers.can_choose.then(|| {
                text_button("choose-server", "All servers", false, &theme)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SessionBarEvent::ChooseServer)))
            });
            row("connection-ended", Role::Alert, words)
                .child(
                    text_button("reconnect", "Reconnect", true, &theme)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(SessionBarEvent::Reconnect))),
                )
                .children(choose)
        });
        div()
            .key_context(CONTEXT)
            .w_full()
            .font_family(UI_FONT)
            .text_size(FONT_SIZE)
            .text_color(theme.text_muted)
            .children(suggestion)
            .children(ended)
    }
}

#[cfg(test)]
impl Offers {
    pub fn ended(&self) -> Option<&str> {
        self.ended.as_deref()
    }

    pub fn turn_on(&self) -> Option<&PluginId> {
        self.turn_on.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn avatar() -> PluginId {
        PluginId::try_from("avatar".to_owned()).unwrap()
    }

    fn suggesting() -> SettingsState {
        SettingsState {
            suggestion: Some(Suggestion {
                plugin: avatar(),
                source: Source::Mssp,
            }),
            ..SettingsState::default()
        }
    }

    #[test]
    fn a_suggestion_offers_its_plugin_until_it_is_on() {
        let launcher = Launcher::new("unused".into(), Vec::new(), Vec::new(), || {});
        let offers = Offers::of(&suggesting(), &[], false, Some(&launcher));
        assert_eq!(offers.turn_on(), Some(&avatar()));
        assert!(offers.suggestion.as_deref().unwrap().contains("avatar"));

        let on = Offers::of(&suggesting(), &[avatar()], false, Some(&launcher));
        assert_eq!(on.turn_on(), None);
        assert_eq!(on.suggestion, None);
    }

    #[test]
    fn with_no_launcher_a_suggestion_is_only_words() {
        let offers = Offers::of(&suggesting(), &[], true, None);
        assert_eq!(offers.turn_on(), None);
        assert!(
            offers
                .suggestion
                .as_deref()
                .unwrap()
                .contains("--enable avatar")
        );
    }

    #[test]
    fn a_failure_says_what_went_wrong_and_a_replay_offers_no_reconnect() {
        let failed = SettingsState {
            connection: ConnectionStatus::Closed(DisconnectReason::Failed("refused".to_owned())),
            ..SettingsState::default()
        };
        let offers = Offers::of(&failed, &[], false, None);
        assert_eq!(offers.ended(), Some("The connection failed: refused"));
        assert_eq!(Offers::of(&failed, &[], true, None).ended(), None);
    }
}
