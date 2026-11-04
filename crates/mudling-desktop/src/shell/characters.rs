//! Characters, from the window's side: choosing another while one is
//! connected asks first, and adding or removing one runs off the UI thread.

use super::{Outcome, Shell};
use crate::launcher::Target;
use gpui::{Context, Window};
use mudling_core::persist;
use mudling_core::plugin::luau::Value;
use mudling_core::store::{CHARACTER_NAME, ConnectionStatus};

impl Shell {
    /// The session menu's choice. Another character while one is connected
    /// asks before logging out; the one in play just comes forward.
    pub(super) fn choose_character(
        &mut self,
        target: &Target,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) {
        let Some(game) = self.game.as_ref().filter(|game| !game.is_replay()) else {
            return self.ask_to_open(target, window, cx);
        };
        if self.is_in_play(target) {
            return self.focus_command_bar(window, cx);
        }
        let connected = matches!(
            self.copies.settings.value().connection,
            ConnectionStatus::Connected | ConnectionStatus::Connecting
        );
        if !connected {
            return self.ask_to_open(target, window, cx);
        }
        let playing = (self.playing_name()).unwrap_or_else(|| game.target.host.clone());
        self.open_settings(window, cx);
        self.settings.update(cx, |sheet, cx| {
            sheet.ask_switch(&playing, target.clone(), window, cx);
        });
    }

    /// Who the game in play plays: the name the game gave, else the one it
    /// logged in as.
    fn playing_name(&self) -> Option<String> {
        let game = self.game.as_ref()?;
        match self.copies.characters.value().0.get(CHARACTER_NAME) {
            Some(Value::Text(name)) => Some(name.clone()),
            _ => game.target.character.clone(),
        }
    }

    /// Whether `target` is the game in play: its server, and its character
    /// as the game names it, so a game opened from a server card that has
    /// since named Bren is Bren's.
    pub(super) fn is_in_play(&self, target: &Target) -> bool {
        let Some(game) = &self.game else {
            return false;
        };
        let same_server = (&game.target.host, game.target.port) == (&target.host, target.port);
        same_server
            && match (&target.character, self.playing_name()) {
                (None, _) => game.target.character.is_none(),
                (Some(wanted), Some(playing)) => {
                    persist::character_id(wanted) == persist::character_id(&playing)
                }
                (Some(_), None) => false,
            }
    }

    /// Add character, from Where to today?.
    pub fn open_add_character(&mut self, window: &mut Window, cx: &mut Context<Shell>) {
        self.open_settings(window, cx);
        self.settings.update(cx, |sheet, cx| {
            sheet.open_character_form(None, true, window, cx);
        });
    }

    pub(super) fn add_character(
        &mut self,
        server: Target,
        name: String,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) {
        self.off_the_ui_thread(
            move |launcher| Outcome {
                problem: launcher.add_character(&server, &name).err(),
                saved: launcher.saved(),
                ..Outcome::default()
            },
            window,
            cx,
        );
    }

    pub(super) fn remove_character(
        &mut self,
        host: String,
        id: String,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) {
        self.off_the_ui_thread(
            move |launcher| Outcome {
                problem: launcher.remove_character(&host, &id).err(),
                saved: launcher.saved(),
                ..Outcome::default()
            },
            window,
            cx,
        );
    }

    /// Tags each card with what the vault now says of its password.
    pub(super) fn show_passwords(&mut self, cx: &mut Context<Shell>) {
        let passwords = self.settings.read(cx).passwords();
        self.empty_state.update(cx, |empty_state, cx| {
            empty_state.show_passwords(passwords, cx)
        });
    }
}
