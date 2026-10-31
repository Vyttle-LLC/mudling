//! The window's root view, a thin shell over a [`Session`]: the title bar over
//! a centered chat column with the floating command bar at its foot, between
//! two [`Dock`]s. With no game running, the [`EmptyState`] stands in for them.
//!
//! The bar looks like it floats, but the chat ends where it begins, so
//! however tall the bar grows it can't cover the newest line.
//!
//! The UI thread only subscribes, applies and draws. The session's waker
//! comes from a core thread; each wake asks for the next frame, and that
//! frame drains the stores once, so updates apply at most once per frame.

use crate::assets::UI_FONT;
use crate::chat::{self, Chat, column_width};
use crate::dock::{Dock, DockState, Side, Slot, fit_docks};
use crate::empty_state::{EmptyState, EmptyStateEvent};
use crate::hud::Hud;
use crate::input::{self, CommandInput, InputEvent};
use crate::launcher::{Launcher, Target};
use crate::room_panel::{self, RoomPanel, SendCommand, Verbs};
use crate::session_bar::{Offers, SessionBar, SessionBarEvent};
use crate::settings::{PluginRow, SavedServer, SavedSettings, SettingsSheet, SheetEvent};
use crate::status::{StatusLines, lines_for};
use crate::theme::{Appearance, Theme};
use crate::title_bar::{
    self, FocusNextControl, FocusPreviousControl, OpenSettings, SessionLabel, TitleBar,
    TitleBarEvent,
};
use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use gpui::{
    AnyView, App, Context, Entity, FocusHandle, Focusable, KeyBinding, Subscription, Task, Window,
    actions, deferred, div, prelude::*,
};
use mudling_core::completion::Completion;
use mudling_core::persist::SavedServers;
use mudling_core::plugin::PluginId;
use mudling_core::session::Session;
use mudling_core::store::{
    CharactersState, ConnectionStatus, MobsState, RoomState, SettingsState, StoreHub, Versioned,
    VitalsState,
};
use mudling_core::telnet::Bytes;
use tempfile::TempDir;

actions!(
    shell,
    [
        ToggleLeftPanel,
        ToggleRightPanel,
        FocusCommandBar,
        FocusRoomPanel,
        FocusNext
    ]
);

/// Above everything else that floats.
const SHEET_PRIORITY: usize = 4;

pub fn bind_keys(cx: &mut App) {
    chat::bind_keys(cx);
    cx.bind_keys([
        KeyBinding::new(secondary!("1"), ToggleLeftPanel, None),
        KeyBinding::new(secondary!("2"), ToggleRightPanel, None),
        KeyBinding::new(secondary!("3"), FocusRoomPanel, None),
        KeyBinding::new(secondary!("l"), FocusCommandBar, None),
        KeyBinding::new("tab", FocusNext, Some(room_panel::CONTEXT)),
        KeyBinding::new("escape", FocusCommandBar, Some(room_panel::CONTEXT)),
    ]);
}

/// What the window runs on. Without one, and with a [`Launcher`], it shows the
/// empty state; with neither it only echoes what's typed.
pub struct Game {
    pub session: Session,
    /// A recording's throwaway data folder, or `None` for a real server.
    pub replay_folder: Option<TempDir>,
    pub enabled: Vec<PluginId>,
    /// The game's words for what the panels send.
    pub verbs: Verbs,
    pub target: Target,
    /// What Settings › Plugins lists.
    pub plugins: Vec<PluginRow>,
}

impl Game {
    fn is_replay(&self) -> bool {
        self.replay_folder.is_some()
    }

    /// Ends the session. This blocks until it's on the disk.
    fn finish(self) {
        if let Err(error) = self.session.quit().wait() {
            eprintln!("couldn't finish saving the session: {error}");
        }
        drop(self.replay_folder);
    }
}

/// The shell's copies of the stores it shows, other than the terminal.
struct Copies {
    settings: Versioned<SettingsState>,
    vitals: Versioned<VitalsState>,
    characters: Versioned<CharactersState>,
    room: Versioned<RoomState>,
    mobs: Versioned<MobsState>,
}

impl Copies {
    fn of(stores: Option<&StoreHub>) -> Copies {
        Copies {
            settings: stores.map(|stores| stores.subscribe()).unwrap_or_default(),
            vitals: stores.map(|stores| stores.subscribe()).unwrap_or_default(),
            characters: stores.map(|stores| stores.subscribe()).unwrap_or_default(),
            room: stores.map(|stores| stores.subscribe()).unwrap_or_default(),
            mobs: stores.map(|stores| stores.subscribe()).unwrap_or_default(),
        }
    }
}

/// What a job off the UI thread comes back with.
struct Outcome {
    game: Option<Game>,
    saved: SavedServers,
    problem: Option<String>,
}

pub struct Shell {
    game: Option<Game>,
    launcher: Option<Launcher>,
    /// The one job running off the UI thread: a session ending, another
    /// opening, a server being saved.
    job: Option<Task<()>>,
    /// One session is ending and the next opening, so the window stays on the
    /// chat rather than flash the empty state.
    switching: bool,
    copies: Copies,
    left: bool,
    chat: Entity<Chat>,
    status: Entity<StatusLines>,
    session_bar: Entity<SessionBar>,
    empty_state: Entity<EmptyState>,
    title_bar: Entity<TitleBar>,
    hud: Entity<Hud>,
    input: Entity<CommandInput>,
    left_dock: Entity<Dock>,
    right_dock: Entity<Dock>,
    room_panel: Entity<RoomPanel>,
    settings: Entity<SettingsSheet>,
    settings_open: bool,
    /// Where focus goes back to when the sheet closes.
    focus_before_settings: Option<FocusHandle>,
    /// Keeps the player's choices, if there's a folder to keep them in.
    saved_settings: Option<SavedSettings>,
    _subscriptions: [Subscription; 9],
    _woken: Task<()>,
}

impl Shell {
    /// `woken` hears from the session's waker. A real server's connection
    /// opens here, once the stores are subscribed.
    pub fn new(
        game: Option<Game>,
        woken: UnboundedReceiver<()>,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) -> Shell {
        cx.set_global(Theme::for_appearance(window.appearance()));
        let stores = game.as_ref().map(|game| game.session.stores());
        let terminal = stores.map(|stores| stores.subscribe()).unwrap_or_default();
        let copies = Copies::of(stores);
        let chat = cx.new(|_| Chat::new(terminal));
        let input = cx.new(CommandInput::new);
        let typing = cx.subscribe_in(&input, window, |shell, _, event, window, cx| {
            shell.on_typing(event, window, cx);
        });
        let verbs = game.as_ref().map(|game| game.verbs.clone());
        let room_panel = cx.new(|cx| RoomPanel::new(verbs.unwrap_or_default(), cx));
        let verb_sent = cx.subscribe_in(&room_panel, window, |shell, _, sent, window, cx| {
            shell.on_room_verb(sent, window, cx);
        });
        // A panel that hides or changes tab takes its focus with it. Deferred
        // past the draw this runs in, which wouldn't redraw for the new focus,
        // and the bar only takes text once it's drawn focused.
        let focus_lost = cx.on_focus_lost(window, |_, window, cx| {
            cx.defer_in(window, |shell, window, cx| {
                window.focus(&shell.input.focus_handle(cx), cx);
            });
        });
        let appearance = cx.observe_window_appearance(window, |_, window, cx| {
            let choice = cx.global::<Theme>().choice;
            cx.set_global(Theme::chosen(choice, window.appearance()));
            window.refresh();
        });
        let plugins = game.as_ref().map(|game| game.plugins.clone());
        let settings = cx.new(|cx| {
            let mut sheet = SettingsSheet::new(cx);
            sheet.show_plugins(plugins.unwrap_or_default(), cx);
            sheet
        });
        let sheet_events =
            cx.subscribe_in(
                &settings,
                window,
                |shell, _, event, window, cx| match event {
                    SheetEvent::Closed => shell.close_settings(window, cx),
                    SheetEvent::Chose(choice) => shell.apply_choice(*choice, window, cx),
                },
            );
        let quitting = cx.on_app_quit(|shell, cx| {
            let saved = shell.leave(cx);
            async move {
                if let Some(saved) = saved {
                    saved.await;
                }
            }
        });
        window.focus(&input.focus_handle(cx), cx);
        if let Some(game) = game.as_ref().filter(|game| !game.is_replay()) {
            game.session.connect();
        }
        let left_dock = cx.new(|_| Dock::new(Side::Left));
        let right_dock = cx.new(|_| Dock::new(Side::Right));
        let title_bar = cx.new(|cx| TitleBar::new(left_dock.clone(), right_dock.clone(), cx));
        let session_bar = cx.new(|_| SessionBar::default());
        let empty_state = cx.new(|cx| EmptyState::new(window, cx));
        let menu = cx.subscribe_in(
            &title_bar,
            window,
            |shell, _, event, window, cx| match event {
                TitleBarEvent::Open(target) => shell.open(target.clone(), None, window, cx),
                TitleBarEvent::ChooseServer => shell.choose_server(window, cx),
            },
        );
        let offers = cx.subscribe_in(
            &session_bar,
            window,
            |shell, _, event, window, cx| match event {
                SessionBarEvent::Reconnect => shell.reconnect(window, cx),
                SessionBarEvent::ChooseServer => shell.choose_server(window, cx),
                SessionBarEvent::TurnOn(plugin) => shell.turn_on(plugin.clone(), window, cx),
            },
        );
        let places = cx.subscribe_in(
            &empty_state,
            window,
            |shell, _, event, window, cx| match event {
                EmptyStateEvent::Connect(target) => shell.open(target.clone(), None, window, cx),
                EmptyStateEvent::Add(target) => shell.add_server(target.clone(), window, cx),
            },
        );
        let mut shell = Shell {
            game,
            launcher: None,
            job: None,
            switching: false,
            copies,
            left: false,
            chat,
            status: cx.new(|_| StatusLines::default()),
            session_bar,
            empty_state,
            title_bar,
            hud: cx.new(|_| Hud::default()),
            input,
            left_dock,
            right_dock,
            room_panel,
            settings,
            settings_open: false,
            focus_before_settings: None,
            saved_settings: None,
            _subscriptions: [
                typing,
                verb_sent,
                focus_lost,
                appearance,
                quitting,
                menu,
                offers,
                places,
                sheet_events,
            ],
            _woken: Self::apply_stores_when_woken(woken, window, cx),
        };
        shell.show_status(cx);
        shell.show_room(cx);
        shell.mount(Slot::Room, shell.room_panel.clone(), cx);
        shell
    }

    /// Lets the window open games. With no game running it shows the empty
    /// state. A replay has no use for this: it never calls it.
    pub fn use_launcher(
        &mut self,
        launcher: Launcher,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) {
        self.launcher = Some(launcher);
        self.show_where_things_stand(cx);
        self.off_the_ui_thread(
            |launcher| Outcome {
                game: None,
                saved: launcher.saved(),
                problem: None,
            },
            window,
            cx,
        );
    }

    /// Runs `job` away from the UI thread, one job at a time.
    fn off_the_ui_thread(
        &mut self,
        job: impl FnOnce(&Launcher) -> Outcome + Send + 'static,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) {
        let Some(launcher) = self.launcher.clone() else {
            return;
        };
        let done = cx.background_spawn(async move { job(&launcher) });
        self.job = Some(cx.spawn_in(window, async move |shell, cx| {
            let outcome = done.await;
            let _ = shell.update_in(cx, |shell, window, cx| {
                shell.job_done(outcome, window, cx);
            });
        }));
    }

    fn job_done(&mut self, outcome: Outcome, window: &mut Window, cx: &mut Context<Shell>) {
        self.job = None;
        self.switching = false;
        // A game opened as the window left goes unused: dropping it ends it.
        if self.left {
            return;
        }
        self.empty_state.update(cx, |empty_state, cx| {
            empty_state.show(&outcome.saved, cx);
            if let Some(problem) = outcome.problem {
                empty_state.show_problem(problem, cx);
            }
        });
        self.settings.update(cx, |sheet, cx| {
            let servers = outcome.saved.servers.iter().map(SavedServer::from);
            sheet.show_servers(servers.collect(), cx);
        });
        if let Some(game) = outcome.game {
            self.play(game, window, cx);
        }
        self.show_where_things_stand(cx);
        if self.on_empty_state() {
            self.empty_state
                .update(cx, |empty_state, cx| empty_state.focus(window, cx));
        }
    }

    /// Moves the window onto `game`: a chat and copies of its own, so nothing
    /// of the last session carries over. A real server's connection opens
    /// here, once the stores are subscribed.
    fn play(&mut self, game: Game, window: &mut Window, cx: &mut Context<Shell>) {
        let stores = game.session.stores();
        let terminal = stores.subscribe();
        self.copies = Copies::of(Some(stores));
        self.chat = cx.new(|_| Chat::new(terminal));
        self.room_panel
            .update(cx, |panel, cx| panel.use_verbs(game.verbs.clone(), cx));
        self.settings
            .update(cx, |sheet, cx| sheet.show_plugins(game.plugins.clone(), cx));
        self.show_room(cx);
        game.session.connect();
        self.game = Some(game);
        self.input.update(cx, |input, cx| {
            input.set_masked(false, window, cx);
            input.show(String::new(), 0, window, cx);
        });
        window.focus(&self.input.focus_handle(cx), cx);
    }

    /// Whether the window shows "Where to today?" rather than a game.
    fn on_empty_state(&self) -> bool {
        self.game.is_none() && self.launcher.is_some() && !self.switching
    }

    /// Ends the session running, if any, and opens one on `target`. `turn_on`
    /// is a game plugin the player just turned on: a session reads its
    /// plugins when it opens, so this is also how that takes effect.
    fn open(
        &mut self,
        target: Target,
        turn_on: Option<PluginId>,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) {
        if self.job.is_some() || self.launcher.is_none() {
            return;
        }
        let leaving = self.game.take();
        self.switching = leaving.is_some();
        let connecting = SessionLabel::new(target.host.clone(), None, "Connecting…");
        self.title_bar
            .update(cx, |bar, cx| bar.show(connecting, cx));
        self.off_the_ui_thread(
            move |launcher| {
                if let Some(game) = leaving {
                    game.finish();
                }
                let opened = launcher.open(&target, turn_on.as_ref());
                let (game, problem) = match opened {
                    Ok(game) => (Some(game), None),
                    Err(problem) => (None, Some(problem)),
                };
                Outcome {
                    game,
                    saved: launcher.saved(),
                    problem,
                }
            },
            window,
            cx,
        );
    }

    /// Ends the session and goes back to the empty state.
    fn choose_server(&mut self, window: &mut Window, cx: &mut Context<Shell>) {
        if self.job.is_some() {
            return;
        }
        let leaving = self.game.take();
        self.show_where_things_stand(cx);
        self.off_the_ui_thread(
            move |launcher| {
                if let Some(game) = leaving {
                    game.finish();
                }
                Outcome {
                    game: None,
                    saved: launcher.saved(),
                    problem: None,
                }
            },
            window,
            cx,
        );
    }

    fn add_server(&mut self, target: Target, window: &mut Window, cx: &mut Context<Shell>) {
        if self.job.is_some() {
            return;
        }
        self.off_the_ui_thread(
            move |launcher| Outcome {
                game: None,
                problem: launcher.save(&target).err(),
                saved: launcher.saved(),
            },
            window,
            cx,
        );
    }

    /// Connects again on the same session, keeping the chat. A session whose
    /// core thread stopped can't, so that one is opened anew. The Reconnect
    /// button goes as the connection starts, so focus moves off it first.
    fn reconnect(&mut self, window: &mut Window, cx: &mut Context<Shell>) {
        let Some(game) = &self.game else { return };
        if game.session.has_stopped() {
            let target = game.target.clone();
            self.open(target, None, window, cx);
        } else {
            game.session.connect();
            window.focus(&self.input.focus_handle(cx), cx);
        }
    }

    fn turn_on(&mut self, plugin: PluginId, window: &mut Window, cx: &mut Context<Shell>) {
        if let Some(game) = &self.game {
            let target = game.target.clone();
            self.open(target, Some(plugin), window, cx);
        }
    }

    /// Wears what the player kept, and keeps what they choose from now on.
    pub fn restore_settings(
        &mut self,
        saved: SavedSettings,
        choice: Appearance,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) {
        cx.set_global(Theme::chosen(choice, window.appearance()));
        self.saved_settings = Some(saved);
    }

    /// Takes effect at once, in every view.
    fn apply_choice(&self, choice: Appearance, window: &mut Window, cx: &mut Context<Shell>) {
        cx.set_global(Theme::chosen(choice, window.appearance()));
        window.refresh();
        if let Some(saved) = &self.saved_settings {
            saved.save(choice);
        }
    }

    pub fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Shell>) {
        if self.settings_open {
            return;
        }
        self.title_bar
            .update(cx, |bar, cx| bar.dismiss_menu(window, cx));
        self.focus_before_settings = window.focused(cx);
        self.settings_open = true;
        self.settings
            .update(cx, |sheet, cx| sheet.opened(window, cx));
        cx.notify();
    }

    fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Shell>) {
        self.settings_open = false;
        if let Some(focused) = self.focus_before_settings.take() {
            window.focus(&focused, cx);
        }
        cx.notify();
    }

    /// Each wake is one batch waiting, and no other wake comes until a frame
    /// drains it.
    fn apply_stores_when_woken(
        mut woken: UnboundedReceiver<()>,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) -> Task<()> {
        cx.spawn_in(window, async move |shell, cx| {
            while woken.next().await.is_some() {
                let asked = shell.update_in(cx, |_, window, cx| {
                    cx.on_next_frame(window, |shell, window, cx| shell.apply_stores(window, cx));
                });
                if asked.is_err() {
                    return;
                }
            }
        })
    }

    /// Applies what changed since the last frame.
    fn apply_stores(&mut self, window: &mut Window, cx: &mut Context<Shell>) {
        let stores = self.game.as_ref().map(|game| game.session.stores());
        let Some(batch) = stores.and_then(|stores| stores.drain()) else {
            return;
        };
        if let Some(lines) = &batch.terminal {
            self.chat.update(cx, |chat, cx| chat.apply(lines, cx));
        }
        if let Some(vitals) = &batch.vitals {
            self.copies.vitals.apply_diff(vitals);
        }
        if let Some(settings) = &batch.settings {
            self.copies.settings.apply_diff(settings);
            let masked = self.copies.settings.value().echo_off;
            self.input
                .update(cx, |input, cx| input.set_masked(masked, window, cx));
        }
        if let Some(characters) = &batch.characters {
            self.copies.characters.apply_diff(characters);
        }
        if batch.vitals.is_some() || batch.settings.is_some() || batch.characters.is_some() {
            self.show_status(cx);
        }
        if let Some(room) = &batch.room {
            self.copies.room.apply_diff(room);
        }
        if let Some(mobs) = &batch.mobs {
            self.copies.mobs.apply_diff(mobs);
        }
        if batch.room.is_some() || batch.mobs.is_some() {
            self.show_room(cx);
        }
    }

    fn show_room(&mut self, cx: &mut Context<Shell>) {
        let (room, mobs) = (self.copies.room.value(), self.copies.mobs.value());
        self.room_panel
            .update(cx, |panel, cx| panel.show_room(room, mobs, cx));
    }

    fn show_status(&mut self, cx: &mut Context<Shell>) {
        let Some(game) = &self.game else { return };
        let settings = self.copies.settings.value();
        self.show_problems(settings.server.as_deref(), &settings.notices, cx);
        let lines = lines_for(settings);
        // The core publishes no latency yet.
        let latency = None;
        let characters = self.copies.characters.value();
        let session = SessionLabel::of(settings, characters, game.is_replay(), latency);
        let launcher = self.launcher.as_ref();
        let offers = Offers::of(settings, &game.enabled, game.is_replay(), launcher);
        self.status.update(cx, |status, cx| status.show(lines, cx));
        self.session_bar.update(cx, |bar, cx| bar.show(offers, cx));
        self.title_bar.update(cx, |bar, cx| bar.show(session, cx));
        self.hud.update(cx, |hud, cx| {
            hud.show(self.copies.vitals.value(), cx);
        });
    }

    /// Brings the title bar and what's beside the chat in line with whether
    /// a game is running, once that changes.
    fn show_where_things_stand(&mut self, cx: &mut Context<Shell>) {
        let current = self.game.as_ref().map(|game| game.target.host.as_str());
        let elsewhere = self
            .empty_state
            .read(cx)
            .cards()
            .iter()
            .filter(|card| Some(card.target.host.as_str()) != current)
            .cloned()
            .collect();
        let can_choose_server = self.game.is_some() && self.launcher.is_some();
        let bare = self.on_empty_state();
        self.title_bar.update(cx, |bar, cx| {
            bar.show_saved(elsewhere, can_choose_server, cx);
            bar.set_bare(bare, cx);
            if bare {
                bar.show(SessionLabel::default(), cx);
            }
        });
        self.show_status(cx);
        cx.notify();
    }

    fn show_problems(&self, server: Option<&str>, notices: &[String], cx: &mut Context<Shell>) {
        self.settings
            .update(cx, |sheet, cx| sheet.show_session(server, notices, cx));
        self.title_bar
            .update(cx, |bar, cx| bar.show_problems(notices.len(), cx));
    }

    fn on_typing(&mut self, event: &InputEvent, window: &mut Window, cx: &mut Context<Shell>) {
        if let InputEvent::Submitted(command) = event {
            self.submit(command, cx);
            return;
        }
        let Some(game) = &self.game else { return };
        let at_the_end = |line: String| Completion {
            cursor: line.len(),
            line,
        };
        let shown = match event {
            InputEvent::Submitted(_) => None,
            InputEvent::Older { line, prefix } => game.session.older(line, prefix).map(at_the_end),
            InputEvent::Newer { line } => game.session.newer(line).map(at_the_end),
            InputEvent::Complete { line, cursor } => game.session.complete(line, *cursor),
        };
        if let Some(Completion { line, cursor }) = shown {
            self.input
                .update(cx, |input, cx| input.show(line, cursor, window, cx));
        }
    }

    /// With echo off the server is asking for a password, and a command sent
    /// now would go as one, so it's refused, as core refuses a hook's.
    fn on_room_verb(&mut self, sent: &SendCommand, window: &mut Window, cx: &mut Context<Shell>) {
        if !self.copies.settings.value().echo_off {
            self.submit(&sent.command, cx);
        }
        if !sent.by_keyboard {
            window.focus(&self.input.focus_handle(cx), cx);
        }
    }

    /// Sends what was typed, a command to each line; the session echoes them,
    /// unless it's a password. With no server connected they go nowhere, so
    /// the chat shows them here.
    fn submit(&mut self, typed: &str, cx: &mut Context<Shell>) {
        let settings = self.copies.settings.value();
        let connected = settings.connection == ConnectionStatus::Connected;
        for command in typed.split('\n') {
            match &self.game {
                Some(game) if connected && !game.is_replay() => game.session.send(command),
                _ if settings.echo_off => {}
                _ => self
                    .chat
                    .update(cx, |chat, cx| chat.echo_unsent(command, cx)),
            }
        }
    }

    /// Puts `view` in a panel slot: how later tickets fill the panels.
    pub fn mount(&self, slot: Slot, view: impl Into<AnyView>, cx: &mut Context<Shell>) {
        let view = view.into();
        self.dock(slot.side())
            .update(cx, |dock, cx| dock.mount(slot, view, cx));
    }

    /// The dock on `side`, for what opens and closes it, such as the title
    /// bar's toggles.
    pub fn dock(&self, side: Side) -> &Entity<Dock> {
        match side {
            Side::Left => &self.left_dock,
            Side::Right => &self.right_dock,
        }
    }

    /// Opens both panels: the frame budget holds with everything open.
    pub fn open_panels(&self, cx: &mut Context<Shell>) {
        for side in [Side::Left, Side::Right] {
            self.dock(side)
                .update(cx, |dock, cx| dock.move_to(DockState::Open, cx));
        }
    }

    fn toggle_dock(&self, side: Side, cx: &mut Context<Shell>) {
        if !self.on_empty_state() {
            self.dock(side).update(cx, |dock, cx| dock.toggle(cx));
        }
    }

    fn focus_command_bar(&self, window: &mut Window, cx: &mut Context<Shell>) {
        if self.on_empty_state() {
            self.empty_state
                .update(cx, |empty_state, cx| empty_state.focus(window, cx));
        } else {
            window.focus(&self.input.focus_handle(cx), cx);
        }
    }

    /// Shows the Room panel, opening the right panel if it's closed, and
    /// moves the keyboard into it. A task, which runs after the draw that
    /// shows the panel: it can't take focus before it's drawn.
    fn focus_room_panel(&self, window: &mut Window, cx: &mut Context<Shell>) {
        self.dock(Side::Right)
            .update(cx, |dock, cx| dock.select(Slot::Room, cx));
        cx.spawn_in(window, async move |shell, cx| {
            shell
                .update_in(cx, |shell, window, cx| {
                    shell
                        .room_panel
                        .update(cx, |panel, cx| panel.focus(window, cx));
                })
                .ok();
        })
        .detach();
    }

    #[cfg(test)]
    pub fn settings_sheet(&self) -> Entity<SettingsSheet> {
        self.settings.clone()
    }

    #[cfg(test)]
    pub fn show_sample_notices(&self, notices: &[String], cx: &mut Context<Shell>) {
        self.show_problems(Some("mud.example"), notices, cx);
    }

    #[cfg(test)]
    pub fn show_sample_vitals(&self, vitals: &VitalsState, cx: &mut Context<Shell>) {
        self.hud.update(cx, |hud, cx| hud.show(vitals, cx));
    }

    #[cfg(test)]
    pub fn show_sample_session(&self, session: SessionLabel, cx: &mut Context<Shell>) {
        self.title_bar.update(cx, |bar, cx| bar.show(session, cx));
    }

    #[cfg(test)]
    pub fn open_session_menu(&self, window: &mut Window, cx: &mut Context<Shell>) {
        self.title_bar
            .update(cx, |bar, cx| bar.open_menu(window, cx));
    }

    /// Shows the empty state with `saved` on it, with no disk involved.
    #[cfg(test)]
    pub fn show_sample_empty_state(
        &mut self,
        saved: &SavedServers,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) {
        self.launcher = Some(Launcher::new(
            "unused".into(),
            Vec::new(),
            Vec::new(),
            || {},
        ));
        self.empty_state.update(cx, |empty_state, cx| {
            empty_state.show(saved, cx);
            empty_state.focus(window, cx);
        });
        self.show_where_things_stand(cx);
    }

    /// Shows the row a closed connection brings, with no session behind it.
    #[cfg(test)]
    pub fn show_sample_offers(&self, settings: &SettingsState, cx: &mut Context<Shell>) {
        let launcher = Launcher::new("unused".into(), Vec::new(), Vec::new(), || {});
        let offers = Offers::of(settings, &[], false, Some(&launcher));
        self.session_bar.update(cx, |bar, cx| bar.show(offers, cx));
    }

    #[cfg(test)]
    pub fn typed(&self, cx: &App) -> String {
        self.input.read(cx).content().to_owned()
    }

    #[cfg(test)]
    pub fn type_sample(&self, text: &str, window: &mut Window, cx: &mut Context<Shell>) {
        self.input.update(cx, |input, cx| {
            input.show(text.to_owned(), text.len(), window, cx)
        });
    }

    /// Feeds a recording through the session, as one connection.
    pub fn replay(&self, reads: Vec<Bytes>) {
        if let Some(game) = &self.game {
            game.session.replay(reads);
        }
    }

    /// Whether the first recording has reached the chat, or there is none.
    pub fn replay_finished(&self) -> bool {
        let replaying = self.game.as_ref().is_some_and(Game::is_replay);
        !replaying
            || matches!(
                self.copies.settings.value().connection,
                ConnectionStatus::Closed(_)
            )
    }

    pub fn chat(&self) -> Entity<Chat> {
        self.chat.clone()
    }

    pub fn room_panel(&self) -> Entity<RoomPanel> {
        self.room_panel.clone()
    }

    /// Ends the session, off this thread. The task finishes once everything
    /// reached the disk. `None` if the shell already left.
    pub fn leave(&mut self, cx: &mut Context<Shell>) -> Option<Task<()>> {
        if std::mem::replace(&mut self.left, true) {
            return None;
        }
        let job = self.job.take();
        let game = self.game.take();
        let saved_settings = self.saved_settings.take();
        Some(cx.background_spawn(async move {
            drop(saved_settings);
            if let Some(job) = job {
                job.await;
            }
            if let Some(game) = game {
                game.finish();
            }
        }))
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Shell>) -> impl IntoElement {
        fit_docks(
            &self.left_dock,
            &self.right_dock,
            window.viewport_size().width,
            cx,
        );
        let theme = *cx.global::<Theme>();
        let command_bar = div()
            .debug_selector(|| "command-bar".to_owned())
            .rounded_xl()
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_raised)
            .shadow(theme.floating_shadow())
            .child(self.hud.clone())
            .child(self.input.clone());
        let column = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .child(div().flex_1().min_h_0().w_full().child(self.chat.clone()))
            .child(
                div()
                    .w_full()
                    .max_w(column_width(cx))
                    .px_4()
                    .pt_3()
                    .pb_4()
                    .child(self.session_bar.clone())
                    .child(self.status.clone())
                    .child(command_bar),
            );
        let body = if self.on_empty_state() {
            div().flex_1().min_h_0().child(self.empty_state.clone())
        } else {
            div()
                .flex_1()
                .min_h_0()
                .relative()
                .flex()
                .child(self.left_dock.clone())
                .child(column)
                .child(self.right_dock.clone())
        };
        chat::handle_keys(div(), &self.chat)
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(theme.text)
            .font_family(UI_FONT)
            // End closes the split once it has no caret left to move.
            .capture_action(cx.listener(|shell, _: &input::End, _, cx| {
                if shell.input.read(cx).caret_at_end() {
                    shell.chat.update(cx, |chat, cx| chat.back_to_live(cx));
                }
            }))
            .on_action(cx.listener(|shell, _: &ToggleLeftPanel, _, cx| {
                shell.toggle_dock(Side::Left, cx);
            }))
            .on_action(cx.listener(|shell, _: &ToggleRightPanel, _, cx| {
                shell.toggle_dock(Side::Right, cx);
            }))
            .on_action(cx.listener(|shell, _: &FocusCommandBar, window, cx| {
                shell.settings_open = false;
                shell.focus_before_settings = None;
                shell.focus_command_bar(window, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|shell, _: &OpenSettings, window, cx| {
                shell.open_settings(window, cx);
            }))
            .on_action(cx.listener(|shell, _: &FocusRoomPanel, window, cx| {
                shell.focus_room_panel(window, cx);
            }))
            .on_action(|_: &FocusNext, window, cx| window.focus_next(cx))
            .on_action(|_: &FocusPreviousControl, window, cx| window.focus_prev(cx))
            .on_action(|_: &FocusNextControl, window, cx| window.focus_next(cx))
            .capture_any_mouse_down(|event, window, cx| {
                if title_bar::resize_from_edge(event.position, window) {
                    cx.stop_propagation();
                }
            })
            .child(self.title_bar.clone())
            .child(body)
            .when(self.settings_open, |shell_view| {
                shell_view.child(deferred(self.settings.clone()).with_priority(SHEET_PRIORITY))
            })
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::chat::{COLUMN_WIDTH, LINE_HEIGHT, min_column_width};
    use crate::room_panel::press_key;
    use futures::channel::mpsc::{UnboundedSender, unbounded};
    use gpui::{
        Modifiers, MouseButton, Pixels, TestAppContext, VisualTestContext, point, px, size,
    };
    use mudling_core::defaults::hud::Vital;
    use mudling_core::persist;
    use mudling_core::plugin::Plugin;
    use mudling_core::session::SessionConfig;
    use mudling_core::store::TerminalState;
    use mudling_core::telnet::command::{IAC, SB, SE, WILL, WONT};
    use mudling_core::telnet::option::{ECHO, GMCP};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::Path;
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    const WAIT: Duration = Duration::from_secs(5);
    /// What `main` opens the window at.
    const DEFAULT_WINDOW_WIDTH: Pixels = px(1300.);

    /// gpui's test scheduler fails on a wake from another thread, so the
    /// tests wake the shell themselves.
    fn open_game(folder: &Path, port: u16, plugins: Vec<Plugin>) -> Game {
        let enabled: Vec<PluginId> = plugins.iter().map(|plugin| plugin.id().clone()).collect();
        let verbs = Verbs::of(&plugins, &enabled);
        let config = SessionConfig {
            plugins,
            enabled: enabled.clone(),
            ..SessionConfig::new(folder, "127.0.0.1", port)
        };
        let session = Session::open(config, || {}).expect("the session starts");
        Game {
            session,
            replay_folder: None,
            enabled,
            verbs,
            target: Target {
                host: "127.0.0.1".to_owned(),
                port,
            },
            plugins: Vec::new(),
        }
    }

    /// The next line `server` reads, past any telnet the client put ahead of
    /// it, which is the only part that isn't printable.
    fn read_command(server: &mut TcpStream) -> String {
        let mut heard = Vec::new();
        let mut byte = [0];
        while byte != *b"\n" {
            server.read_exact(&mut byte).unwrap();
            heard.push(byte[0]);
        }
        let printable = |byte: &u8| byte.is_ascii() && !byte.is_ascii_control();
        let command = heard.trim_ascii_end().rsplit(|b| !printable(b)).next();
        String::from_utf8_lossy(command.unwrap()).into_owned()
    }

    /// Sends a GMCP message from `server`, offering GMCP first.
    fn send_gmcp(server: &mut TcpStream, message: &str) {
        for bytes in [
            &[IAC, WILL, GMCP][..],
            &[IAC, SB, GMCP],
            message.as_bytes(),
            &[IAC, SE],
        ] {
            server.write_all(bytes).unwrap();
        }
    }

    /// A window on a session, with its folder and a real server on loopback.
    struct Played<'a> {
        shell: Entity<Shell>,
        cx: &'a mut VisualTestContext,
        server: TcpStream,
        wake: UnboundedSender<()>,
        _folder: TempDir,
    }

    fn play(cx: &mut TestAppContext) -> Played<'_> {
        play_with(cx, Vec::new())
    }

    fn play_with(cx: &mut TestAppContext, plugins: Vec<Plugin>) -> Played<'_> {
        cx.update(|cx| {
            crate::input::bind_keys(cx);
            bind_keys(cx);
            cx.set_reduce_motion(true);
        });
        let folder = TempDir::new().unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (wake, woken) = unbounded();
        let game = open_game(folder.path(), port, plugins);
        let (shell, cx) =
            cx.add_window_view(|window, cx| Shell::new(Some(game), woken, window, cx));
        let (server, _) = listener.accept().unwrap();
        server.set_read_timeout(Some(WAIT)).unwrap();
        Played {
            shell,
            cx,
            server,
            wake,
            _folder: folder,
        }
    }

    impl Played<'_> {
        /// Runs frames until `done`, as the display would.
        fn frames_until(&mut self, what: &str, done: impl Fn(&Shell, &gpui::App) -> bool) {
            let started = Instant::now();
            while !self.shell.read_with(self.cx, |shell, cx| done(shell, cx)) {
                assert!(started.elapsed() < WAIT, "timed out waiting for {what}");
                self.frame();
                sleep(Duration::from_millis(1));
            }
        }

        /// What a display frame does. Test windows run no frames of their
        /// own, so what a wake asked for waits here.
        fn frame(&mut self) {
            self.cx.run_until_parked();
            self.shell
                .update_in(self.cx, |shell, window, cx| shell.apply_stores(window, cx));
            self.cx.run_until_parked();
        }

        fn connected(&mut self) {
            self.frames_until("the connection", |shell, _| {
                shell.copies.settings.value().connection == ConnectionStatus::Connected
            });
        }

        fn say(&mut self, bytes: &[u8]) {
            self.server.write_all(bytes).unwrap();
        }

        fn wait_for_chat(&mut self, text: &'static str) {
            self.frames_until(text, move |shell, cx| {
                shell.chat.read(cx).newest_text().as_deref() == Some(text)
            });
        }

        /// What the server reads, up to the end of a line.
        fn heard(&mut self) -> String {
            let mut heard = Vec::new();
            let mut byte = [0];
            while byte != *b"\n" {
                self.server.read_exact(&mut byte).unwrap();
                if byte == [IAC] {
                    self.server.read_exact(&mut [0, 0]).unwrap();
                } else {
                    heard.push(byte[0]);
                }
            }
            String::from_utf8_lossy(&heard).trim().to_owned()
        }

        fn gmcp(&mut self, message: &str) {
            send_gmcp(&mut self.server, message);
        }

        /// Walks into a room with an open exit north and a door east, with
        /// the Room panel open.
        fn enter_room(&mut self) {
            self.connected();
            self.gmcp(
                r#"Room.Info {"num": 1, "name": "A hall", "exits": {"n": 2, "e": "closed"}}"#,
            );
            self.frames_until("the room", |shell, cx| {
                shell.room_panel.read(cx).room_name() == Some("A hall")
            });
            self.shell
                .update(self.cx, |shell, cx| shell.open_panels(cx));
            self.cx.run_until_parked();
        }

        fn fill_room(&mut self) {
            self.shell.update(self.cx, |shell, cx| {
                let (_, _, contents) = crate::measure::sample_room();
                shell
                    .room_panel
                    .update(cx, |panel, cx| panel.show_contents(contents, cx));
            });
            self.cx.run_until_parked();
        }

        fn click(&mut self, selector: &'static str) {
            let bounds = self.cx.debug_bounds(selector).expect(selector);
            self.cx.simulate_click(bounds.center(), Modifiers::none());
        }

        /// The next line the server reads, past any telnet the client put
        /// ahead of it, which is the only part that isn't printable.
        fn heard_command(&mut self) -> String {
            read_command(&mut self.server)
        }

        fn chat_lines(&mut self) -> Vec<String> {
            self.shell.read_with(self.cx, |shell, cx| {
                let chat = shell.chat.read(cx);
                (0..chat.len()).filter_map(|n| chat.line_text(n)).collect()
            })
        }
    }

    #[gpui::test]
    fn typing_goes_to_the_input_and_return_echoes_it(cx: &mut TestAppContext) {
        cx.update(crate::input::bind_keys);
        let (_waker, woken) = unbounded();
        let (shell, cx) = cx.add_window_view(|window, cx| Shell::new(None, woken, window, cx));
        let chat = shell.read_with(cx, |shell, _| shell.chat());
        chat.update(cx, |chat, cx| {
            (0..1_000).for_each(|_| chat.echo_unsent("a line", cx));
        });
        cx.run_until_parked();
        chat.update(cx, |chat, cx| chat.scroll_back(px(400.), cx));
        cx.run_until_parked();
        let reading = chat.read_with(cx, |chat, _| chat.scroll_offset());

        cx.simulate_input("look north");
        assert_eq!(
            chat.read_with(cx, |chat, _| chat.scroll_offset()),
            reading,
            "typing must not move the chat"
        );

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(
            chat.read_with(cx, |chat, _| chat.newest_text()).as_deref(),
            Some("look north")
        );
        assert_eq!(
            chat.read_with(cx, |chat, _| chat.scroll_offset()),
            reading,
            "sending must not move the history pane"
        );
    }

    /// A window on no session, split, with a page of lines to read back over.
    fn split_window(cx: &mut TestAppContext) -> (Entity<Shell>, &mut VisualTestContext) {
        let (shell, cx) = empty_window(cx);
        cx.update(|_, cx| bind_keys(cx));
        let chat = shell.read_with(cx, |shell, _| shell.chat());
        chat.update(cx, |chat, cx| {
            (0..1_000).for_each(|_| chat.echo_unsent("a line", cx));
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("pageup");
        cx.run_until_parked();
        (shell, cx)
    }

    fn is_split(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> bool {
        shell.read_with(cx, |shell, cx| !shell.chat.read(cx).follows_newest())
    }

    #[gpui::test]
    fn page_up_splits_the_chat_and_goes_further_back_with_each_press(cx: &mut TestAppContext) {
        let (shell, cx) = split_window(cx);
        assert!(is_split(&shell, cx));
        let chat = shell.read_with(cx, |shell, _| shell.chat());
        let first_page = chat.read_with(cx, |chat, _| chat.scroll_offset());

        cx.simulate_keystrokes("pageup");
        cx.run_until_parked();
        assert!(chat.read_with(cx, |chat, _| chat.scroll_offset()).line < first_page.line);

        cx.simulate_keystrokes("pagedown");
        cx.run_until_parked();
        assert_eq!(
            chat.read_with(cx, |chat, _| chat.scroll_offset()),
            first_page
        );
    }

    #[gpui::test]
    fn escape_closes_the_split(cx: &mut TestAppContext) {
        let (shell, cx) = split_window(cx);
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!is_split(&shell, cx));
    }

    #[gpui::test]
    fn end_closes_the_split_once_the_caret_has_nowhere_to_go(cx: &mut TestAppContext) {
        let (shell, cx) = split_window(cx);
        cx.simulate_input("look");
        cx.simulate_keystrokes("home end");
        cx.run_until_parked();
        assert!(is_split(&shell, cx), "End moved the caret");

        cx.simulate_keystrokes("end");
        cx.run_until_parked();
        assert!(!is_split(&shell, cx));
        assert_eq!(shell.read_with(cx, |shell, cx| shell.typed(cx)), "look");
    }

    #[gpui::test]
    fn typing_after_a_click_in_the_chat_still_reaches_the_command_bar(cx: &mut TestAppContext) {
        let (shell, cx) = split_window(cx);
        let live = cx.debug_bounds("chat-area").unwrap();
        cx.simulate_click(live.center(), Modifiers::none());
        cx.simulate_input("look");
        cx.run_until_parked();
        assert_eq!(shell.read_with(cx, |shell, cx| shell.typed(cx)), "look");
        assert!(is_split(&shell, cx));
    }

    #[gpui::test]
    fn the_column_stays_capped_and_centered_on_a_wide_window(cx: &mut TestAppContext) {
        let (_shell, cx) = split_window(cx);
        let window = size(px(2_400.), px(900.));
        cx.simulate_resize(window);
        cx.run_until_parked();

        let column = cx.debug_bounds("chat-area").unwrap();
        assert_eq!(column.size.width, COLUMN_WIDTH);
        assert_eq!(column.center().x, window.width / 2.);
        let bar = cx.debug_bounds("command-bar").unwrap();
        assert_eq!(bar.center().x, window.width / 2.);
    }

    #[gpui::test]
    fn the_divider_crosses_the_whole_chat_beyond_the_column(cx: &mut TestAppContext) {
        let (_shell, cx) = split_window(cx);
        let window = size(px(2_400.), px(900.));
        cx.simulate_resize(window);
        cx.run_until_parked();
        assert_eq!(
            cx.debug_bounds("chat-divider").unwrap().size.width,
            window.width
        );
    }

    #[gpui::test]
    fn a_player_reads_the_server_and_sends_commands(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.say(b"Welcome to the fake server.\r\n");
        played.wait_for_chat("Welcome to the fake server.");

        played.cx.simulate_input("look");
        played.cx.simulate_keystrokes("enter");
        assert_eq!(played.heard(), "look");
        played.wait_for_chat("look");
    }

    #[gpui::test]
    fn a_wake_waits_for_the_frame_which_applies_one_batch(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.connected();
        played.say(b"one\r\ntwo\r\nthree\r\n");
        let lines_in_core = |shell: &Shell| {
            let stores = shell.game.as_ref().unwrap().session.stores();
            stores.read(|terminal: &Versioned<TerminalState>| terminal.value().lines().len())
        };
        let started = Instant::now();
        while played.shell.read_with(played.cx, |s, _| lines_in_core(s)) < 3 {
            assert!(started.elapsed() < WAIT, "timed out waiting for the lines");
            sleep(Duration::from_millis(1));
        }

        played.wake.unbounded_send(()).unwrap();
        played.cx.run_until_parked();
        assert_eq!(played.chat_lines().len(), 0, "no frame has run");
        played.frame();
        assert_eq!(played.chat_lines(), ["one", "two", "three"]);
    }

    #[gpui::test]
    fn vitals_and_the_connection_reach_the_hud_and_the_title_bar(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.say(&[IAC, WILL, GMCP]);
        played.say(&[IAC, SB, GMCP]);
        played.say(br#"Char.Vitals {"hp":11,"maxhp":30}"#);
        played.say(&[IAC, SE]);
        played.frames_until("the vitals", |shell, cx| {
            shell.title_bar.read(cx).connection() == "Connected"
                && shell.hud.read(cx).values() == ["11/30"]
        });
    }

    #[gpui::test]
    fn up_recalls_a_sent_command_and_tab_completes_a_word(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.connected();
        played.cx.simulate_input("get lantern");
        played.cx.simulate_keystrokes("enter");
        assert_eq!(played.heard(), "get lantern");
        let typed = |played: &mut Played| {
            played.shell.read_with(played.cx, |shell, cx| {
                shell.input.read(cx).content().to_owned()
            })
        };

        played.cx.simulate_keystrokes("up");
        assert_eq!(typed(&mut played), "get lantern");
        played.cx.simulate_keystrokes("down");
        assert_eq!(typed(&mut played), "");

        played.cx.simulate_input("drop la");
        played.cx.simulate_keystrokes("tab");
        assert_eq!(typed(&mut played), "drop lantern");
    }

    #[gpui::test]
    fn each_line_of_a_multi_line_input_goes_out_as_a_command(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.connected();
        played.cx.simulate_input("look");
        played.cx.simulate_keystrokes("shift-enter");
        played.cx.simulate_input("inv");
        played.cx.simulate_keystrokes("enter");
        assert_eq!(played.heard(), "look");
        assert_eq!(played.heard(), "inv");
    }

    #[gpui::test]
    fn history_and_completion_stay_out_of_a_password(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.connected();
        played.cx.simulate_input("look");
        played.cx.simulate_keystrokes("enter");
        assert_eq!(played.heard(), "look");
        played.say(&[IAC, WILL, ECHO]);
        played.frames_until("echo off", |shell, _| {
            shell.copies.settings.value().echo_off
        });

        played.cx.simulate_keystrokes("up");
        played.cx.simulate_input("lo");
        played.cx.simulate_keystrokes("tab shift-enter");
        played.shell.read_with(played.cx, |shell, cx| {
            assert_eq!(shell.input.read(cx).content(), "lo");
        });
    }

    #[gpui::test]
    fn panel_shortcuts_toggle_the_docks_and_leave_typing_alone(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::input::bind_keys(cx);
            bind_keys(cx);
            cx.set_reduce_motion(true);
        });
        let (_waker, woken) = unbounded();
        let (shell, cx) = cx.add_window_view(|window, cx| Shell::new(None, woken, window, cx));
        let state = |shell: &Entity<Shell>, side, cx: &mut VisualTestContext| {
            shell.read_with(cx, |shell, cx| shell.dock(side).read(cx).state())
        };

        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-1"
        } else {
            "ctrl-1"
        });
        assert_eq!(state(&shell, Side::Left, cx), DockState::Open);
        assert_eq!(state(&shell, Side::Right, cx), DockState::Hidden);

        cx.simulate_input("look");
        cx.simulate_keystrokes("enter");
        let chat = shell.read_with(cx, |shell, _| shell.chat());
        assert_eq!(
            chat.read_with(cx, |chat, _| chat.newest_text()).as_deref(),
            Some("look")
        );
    }

    #[gpui::test]
    fn the_chat_stops_above_the_command_bar_as_it_grows(cx: &mut TestAppContext) {
        cx.update(crate::input::bind_keys);
        let (_waker, woken) = unbounded();
        let (shell, cx) = cx.add_window_view(|window, cx| Shell::new(None, woken, window, cx));
        let chat = shell.read_with(cx, |shell, _| shell.chat());
        chat.update(cx, |chat, cx| {
            (0..200).for_each(|_| chat.echo_unsent("a line", cx));
        });
        cx.run_until_parked();

        cx.simulate_input("one");
        cx.simulate_keystrokes("shift-enter");
        cx.simulate_input("two");
        cx.simulate_keystrokes("shift-enter");
        cx.simulate_input("three");
        cx.run_until_parked();

        let chat_bottom = cx.debug_bounds("chat-area").unwrap().bottom();
        let bar = cx.debug_bounds("command-bar").unwrap();
        assert!(chat_bottom <= bar.top(), "{chat_bottom:?} over {bar:?}");
        assert!(
            bar.size.height >= LINE_HEIGHT * 3.,
            "the bar grows: {:?}",
            bar.size
        );
        assert!(
            chat.read_with(cx, |chat, _| chat.follows_newest()),
            "the newest line stays in view as the bar grows"
        );
    }

    /// A window on no session, with every key bound.
    pub fn window_with_keys(cx: &mut TestAppContext) -> (Entity<Shell>, &mut VisualTestContext) {
        cx.update(|cx| {
            crate::bind_keys(cx);
            cx.set_reduce_motion(true);
        });
        let (_waker, woken) = unbounded();
        let opened = cx.add_window_view(|window, cx| Shell::new(None, woken, window, cx));
        opened.1.run_until_parked();
        opened
    }

    /// A window on no session, with panels that open at once.
    fn empty_window(cx: &mut TestAppContext) -> (Entity<Shell>, &mut VisualTestContext) {
        cx.update(|cx| {
            crate::input::bind_keys(cx);
            cx.set_reduce_motion(true);
        });
        let (_waker, woken) = unbounded();
        cx.add_window_view(|window, cx| Shell::new(None, woken, window, cx))
    }

    /// Drags both panels as wide as they go.
    fn drag_panels_wide(shell: &Entity<Shell>, window_width: Pixels, cx: &mut VisualTestContext) {
        shell.update(cx, |shell, cx| {
            shell.open_panels(cx);
            shell.dock(Side::Left).update(cx, |dock, cx| {
                dock.resize_to(window_width, window_width, cx)
            });
            shell
                .dock(Side::Right)
                .update(cx, |dock, cx| dock.resize_to(px(0.), window_width, cx));
        });
    }

    #[gpui::test]
    fn a_click_on_an_overlaid_panel_stops_at_the_panel(cx: &mut TestAppContext) {
        let (shell, cx) = empty_window(cx);
        cx.simulate_resize(size(px(760.), px(640.)));
        shell.update(cx, |shell, cx| {
            shell
                .dock(Side::Right)
                .update(cx, |dock, cx| dock.toggle(cx));
        });
        cx.simulate_input("kill the king");
        cx.run_until_parked();

        let bar = cx.debug_bounds("command-bar").unwrap();
        cx.simulate_click(
            point(bar.right() - px(27.), bar.center().y),
            Modifiers::none(),
        );

        let chat = shell.read_with(cx, |shell, _| shell.chat());
        assert_eq!(chat.read_with(cx, |chat, _| chat.len()), 0);
        shell.read_with(cx, |shell, cx| {
            assert_eq!(shell.input.read(cx).content(), "kill the king");
        });
    }

    #[gpui::test]
    fn panels_dragged_wide_leave_the_command_bar_its_width(cx: &mut TestAppContext) {
        let (shell, cx) = empty_window(cx);
        cx.simulate_resize(size(px(1400.), px(800.)));
        drag_panels_wide(&shell, px(1400.), cx);
        cx.run_until_parked();

        assert!(cx.debug_bounds("command-bar").unwrap().size.width >= px(460.));
        assert!(cx.debug_bounds("right-dock").unwrap().right() <= px(1400.));
    }

    #[gpui::test]
    fn shrinking_the_window_narrows_a_dragged_panel_and_growing_restores_it(
        cx: &mut TestAppContext,
    ) {
        let (shell, cx) = empty_window(cx);
        cx.simulate_resize(size(px(1400.), px(800.)));
        shell.update(cx, |shell, cx| {
            shell.dock(Side::Left).update(cx, |dock, cx| {
                dock.toggle(cx);
                dock.resize_to(px(840.), px(1400.), cx);
            });
        });
        cx.run_until_parked();
        let dragged = cx.debug_bounds("left-dock").unwrap().size.width;

        cx.simulate_resize(size(px(950.), px(800.)));
        cx.run_until_parked();
        assert!(cx.debug_bounds("command-bar").unwrap().size.width >= px(460.));

        cx.simulate_resize(size(px(1400.), px(800.)));
        cx.run_until_parked();
        assert_eq!(cx.debug_bounds("left-dock").unwrap().size.width, dragged);
    }

    #[gpui::test]
    fn panels_with_no_room_beside_the_column_overlay_it(cx: &mut TestAppContext) {
        let (shell, cx) = empty_window(cx);
        cx.simulate_resize(size(px(1000.), px(800.)));
        shell.update(cx, |shell, cx| shell.open_panels(cx));
        cx.run_until_parked();

        let bar = cx.debug_bounds("command-bar").unwrap();
        assert!(bar.size.width >= px(700.), "the docks don't push: {bar:?}");
        let left = cx.debug_bounds("left-dock").unwrap();
        let right = cx.debug_bounds("right-dock").unwrap();
        assert!(left.right() <= right.left(), "{left:?} over {right:?}");
    }

    #[gpui::test]
    fn every_vital_shows_at_the_default_window_with_both_panels_open(cx: &mut TestAppContext) {
        let (shell, cx) = empty_window(cx);
        cx.simulate_resize(size(DEFAULT_WINDOW_WIDTH, px(800.)));
        let vitals = VitalsState(
            [
                (Vital::Hp, 30),
                (Vital::MaxHp, 42),
                (Vital::Mana, 110),
                (Vital::MaxMana, 115),
                (Vital::Moves, 110),
                (Vital::MaxMoves, 110),
            ]
            .into(),
        );
        shell.update(cx, |shell, cx| {
            shell.show_sample_vitals(&vitals, cx);
            shell.open_panels(cx);
        });
        cx.run_until_parked();

        let bar = cx.debug_bounds("command-bar").unwrap();
        for meter in ["meter-HP", "meter-MP", "meter-MV"] {
            let meter = cx.debug_bounds(meter).unwrap();
            assert!(
                meter.left() >= bar.left() && meter.right() <= bar.right(),
                "{meter:?} outside {bar:?}"
            );
            assert!(meter.top() >= bar.top() && meter.bottom() <= bar.bottom());
        }
    }

    #[gpui::test]
    fn the_default_window_docks_both_panels_beside_80_columns(cx: &mut TestAppContext) {
        let (shell, cx) = empty_window(cx);
        cx.simulate_resize(size(DEFAULT_WINDOW_WIDTH, px(800.)));
        drag_panels_wide(&shell, DEFAULT_WINDOW_WIDTH, cx);
        cx.run_until_parked();

        let left = cx.debug_bounds("left-dock").unwrap();
        let right = cx.debug_bounds("right-dock").unwrap();
        let area = cx.debug_bounds("chat-area").unwrap();
        assert!(left.right() <= area.left() && area.right() <= right.left());
        cx.update(|_, cx| assert!(area.size.width >= min_column_width(cx)));
    }

    #[gpui::test]
    fn panels_never_narrow_the_column_below_80_columns(cx: &mut TestAppContext) {
        let (shell, cx) = empty_window(cx);
        shell.update(cx, |shell, cx| shell.open_panels(cx));
        for width in [1_250., 1_000., 800.] {
            cx.simulate_resize(size(px(width), px(800.)));
            cx.run_until_parked();
            let column = cx.debug_bounds("chat-area").unwrap().size.width;
            cx.update(|_, cx| assert!(column >= min_column_width(cx), "{column:?} at {width}"));
        }
    }

    #[gpui::test]
    fn a_later_ticket_fills_a_slot_through_the_shell(cx: &mut TestAppContext) {
        struct Filled;
        impl Render for Filled {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div()
                    .debug_selector(|| "filled-slot".to_owned())
                    .child("filled")
            }
        }
        cx.update(|cx| cx.set_reduce_motion(true));
        let (_waker, woken) = unbounded();
        let (shell, cx) = cx.add_window_view(|window, cx| Shell::new(None, woken, window, cx));
        shell.update(cx, |shell, cx| {
            let filled = cx.new(|_| Filled);
            shell.mount(Slot::Room, filled, cx);
            shell.open_panels(cx);
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("filled-slot").is_some());
    }

    #[gpui::test]
    fn a_password_is_masked_sent_and_not_kept(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.cx.simulate_input("half a comma");
        played.say(&[IAC, WILL, ECHO]);
        played.say(b"Password:\r\n");
        played.frames_until("the mask", |shell, cx| shell.input.read(cx).is_masked());
        played.shell.read_with(played.cx, |shell, cx| {
            assert_eq!(shell.input.read(cx).content(), "");
        });

        played.cx.simulate_input("hunter2");
        played.cx.simulate_keystrokes("enter");
        assert_eq!(played.heard(), "hunter2");

        played.cx.simulate_input("half a pass");
        played.say(&[IAC, WONT, ECHO]);
        played.frames_until("the mask off", |shell, cx| {
            !shell.input.read(cx).is_masked()
        });
        played.shell.read_with(played.cx, |shell, cx| {
            assert_eq!(shell.input.read(cx).content(), "");
        });
        played.cx.simulate_input("look");
        played.cx.simulate_keystrokes("enter");
        assert_eq!(played.heard(), "look");
        played.wait_for_chat("look");
        assert!(!played.chat_lines().iter().any(|l| l.contains("hunter2")));
        played.shell.read_with(played.cx, |shell, _| {
            let session = &shell.game.as_ref().unwrap().session;
            assert_eq!(session.older("", "").as_deref(), Some("look"));
            assert_eq!(session.older("look", ""), None, "only look is in history");
        });
    }

    /// A game with its own words for the panel's commands.
    fn game_plugin() -> Plugin {
        Plugin::from_files(|file| {
            Ok(match file {
                "plugin.toml" => "id = \"game\"\nname = \"Game\"\nversion = \"0.1.0\"\n\
                                  api = 1\nlicense = \"MIT\"\nlayer = \"game\"\n\
                                  [servers]\nhosts = [\"game.example\"]\n[points]\ncommands = \"commands.toml\"\n"
                    .to_owned(),
                _ => "[verbs]\nget = \"take\"\n[doors]\nopen = \"pry {dir}\"\n".to_owned(),
            })
        })
        .expect("the plugin loads")
    }

    #[gpui::test]
    fn the_room_store_reaches_the_panel_only_on_a_frame(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.connected();
        played.gmcp(r#"Room.Info {"num": 1, "name": "A hall", "exits": {"n": 2}}"#);
        let room_in_core = |shell: &Shell| {
            let stores = shell.game.as_ref().unwrap().session.stores();
            stores.read(|room: &Versioned<RoomState>| room.value().room.is_some())
        };
        let started = Instant::now();
        while !played.shell.read_with(played.cx, |s, _| room_in_core(s)) {
            assert!(started.elapsed() < WAIT, "timed out waiting for the room");
            sleep(Duration::from_millis(1));
        }

        played.wake.unbounded_send(()).unwrap();
        played.cx.run_until_parked();
        let shown = |played: &mut Played| {
            played.shell.read_with(played.cx, |shell, cx| {
                shell.room_panel.read(cx).room_name().map(str::to_owned)
            })
        };
        assert_eq!(shown(&mut played), None, "no frame has run");
        played.frame();
        assert_eq!(shown(&mut played).as_deref(), Some("A hall"));
    }

    #[gpui::test]
    fn exits_send_moves_and_a_closed_door_uses_the_games_command(cx: &mut TestAppContext) {
        let mut played = play_with(cx, vec![game_plugin()]);
        played.enter_room();

        played.click("Go north");
        assert_eq!(played.heard_command(), "north");
        played.click("dpad-east");
        assert_eq!(played.heard_command(), "pry east");
        assert_eq!(played.heard_command(), "east");
    }

    #[gpui::test]
    fn a_verb_goes_out_in_the_games_word_and_typing_stays_with_the_bar(cx: &mut TestAppContext) {
        let mut played = play_with(cx, vec![game_plugin()]);
        played.enter_room();
        played.fill_room();

        played.click("Get a stone");
        assert_eq!(played.heard_command(), "take stone");
        played.cx.simulate_input("look");
        played.shell.read_with(played.cx, |shell, cx| {
            assert_eq!(shell.input.read(cx).content(), "look");
        });
    }

    #[gpui::test]
    fn the_panel_sends_nothing_to_a_password_prompt(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.enter_room();
        played.fill_room();
        played.say(&[IAC, WILL, ECHO]);
        played.frames_until("the mask", |shell, cx| shell.input.read(cx).is_masked());
        played.cx.simulate_input("hunter2");

        played.click("Go north");
        played.click("Get a stone");
        played.shell.read_with(played.cx, |shell, cx| {
            assert_eq!(shell.input.read(cx).content(), "hunter2");
        });

        played.cx.simulate_keystrokes("enter");
        assert_eq!(played.heard_command(), "hunter2", "the verbs went nowhere");
    }

    #[gpui::test]
    fn a_mouse_press_in_the_panel_leaves_typing_with_the_bar(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.enter_room();

        played.click("room-map");
        let north = played.cx.debug_bounds("Go north").unwrap().center();
        let map = played.cx.debug_bounds("room-map").unwrap().center();
        played
            .cx
            .simulate_mouse_down(north, MouseButton::Left, Modifiers::none());
        played
            .cx
            .simulate_mouse_up(map, MouseButton::Left, Modifiers::none());

        played.cx.simulate_input("look");
        played.shell.read_with(played.cx, |shell, cx| {
            assert_eq!(shell.input.read(cx).content(), "look");
        });
    }

    #[gpui::test]
    fn hiding_the_panel_with_focus_in_it_hands_typing_to_the_bar(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.enter_room();
        played.cx.simulate_keystrokes(secondary!("3"));

        played.shell.update(played.cx, |shell, cx| {
            shell.toggle_dock(Side::Right, cx);
        });
        played.cx.run_until_parked();

        played.cx.simulate_input("look");
        played.shell.read_with(played.cx, |shell, cx| {
            assert_eq!(shell.input.read(cx).content(), "look");
        });
    }

    #[gpui::test]
    fn the_keyboard_reaches_the_panel_from_the_command_bar_and_comes_back(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.enter_room();
        played.cx.simulate_input("get lantern");
        played.cx.simulate_keystrokes("enter");
        assert_eq!(played.heard_command(), "get lantern");

        played.cx.simulate_keystrokes(secondary!("3"));
        press_key("enter", played.cx);
        assert_eq!(played.heard_command(), "north");

        played.cx.simulate_keystrokes("tab tab");
        press_key("enter", played.cx);
        assert_eq!(played.heard_command(), "open east");
        assert_eq!(played.heard_command(), "east");

        played.cx.simulate_keystrokes("escape");
        played.cx.simulate_input("get la");
        played.cx.simulate_keystrokes("tab");
        played.shell.read_with(played.cx, |shell, cx| {
            assert_eq!(shell.input.read(cx).content(), "get lantern");
        });
    }

    #[gpui::test]
    fn the_room_shortcut_opens_a_closed_panel_and_takes_the_keyboard_into_it(
        cx: &mut TestAppContext,
    ) {
        let mut played = play(cx);
        played.enter_room();
        played.shell.update(played.cx, |shell, cx| {
            shell.toggle_dock(Side::Right, cx);
        });
        played.cx.run_until_parked();

        played.cx.simulate_keystrokes(secondary!("3"));
        played.shell.read_with(played.cx, |shell, cx| {
            assert_eq!(shell.dock(Side::Right).read(cx).state(), DockState::Open);
        });
        press_key("enter", played.cx);
        assert_eq!(played.heard_command(), "north");
    }

    /// A window with a launcher on a folder of its own, and what it's listening
    /// for: the fake servers are real sockets on loopback.
    struct Launched<'a> {
        shell: Entity<Shell>,
        cx: &'a mut VisualTestContext,
        folder: TempDir,
    }

    const OTHER_HOST: &str = "localhost";

    fn fake_server() -> (TcpListener, u16) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        (listener, port)
    }

    /// A game plugin that claims the fake server's host.
    fn loopback_plugin() -> Plugin {
        Plugin::from_files(|file| match file {
            "plugin.toml" => Ok(r#"
                id = "loopback"
                name = "Loopback"
                version = "1.0.0"
                api = 1
                license = "MIT"
                layer = "game"
                [servers]
                hosts = ["127.0.0.1"]
            "#
            .to_owned()),
            _ => Err(std::io::ErrorKind::NotFound.into()),
        })
        .expect("the manifest is valid")
    }

    fn launch<'a>(saved: &[(&str, u16)], cx: &'a mut TestAppContext) -> Launched<'a> {
        launch_with(saved, vec![loopback_plugin()], Vec::new(), cx)
    }

    /// `enabled` is what the command line turns on.
    fn launch_with<'a>(
        saved: &[(&str, u16)],
        plugins: Vec<Plugin>,
        enabled: Vec<PluginId>,
        cx: &'a mut TestAppContext,
    ) -> Launched<'a> {
        cx.update(|cx| {
            crate::input::bind_keys(cx);
            bind_keys(cx);
            title_bar::bind_keys(cx);
            crate::empty_state::bind_keys(cx);
            crate::session_bar::bind_keys(cx);
            cx.set_reduce_motion(true);
        });
        let folder = TempDir::new().unwrap();
        for (host, port) in saved {
            persist::save_server(folder.path(), host, *port).unwrap();
        }
        let launcher = Launcher::new(folder.path().to_owned(), plugins, enabled, || {});
        let (_wake, woken) = unbounded();
        let (shell, cx) = cx.add_window_view(|window, cx| {
            let mut shell = Shell::new(None, woken, window, cx);
            shell.use_launcher(launcher, window, cx);
            shell
        });
        cx.run_until_parked();
        Launched { shell, cx, folder }
    }

    impl Launched<'_> {
        fn frames_until(&mut self, what: &str, done: impl Fn(&Shell, &App) -> bool) {
            let started = Instant::now();
            while !self.shell.read_with(self.cx, |shell, cx| done(shell, cx)) {
                assert!(started.elapsed() < WAIT, "timed out waiting for {what}");
                self.cx.run_until_parked();
                self.shell
                    .update_in(self.cx, |shell, window, cx| shell.apply_stores(window, cx));
                self.cx.run_until_parked();
                sleep(Duration::from_millis(1));
            }
        }

        fn click(&mut self, id: &'static str) {
            let bounds = self.cx.debug_bounds(id).expect("the control is drawn");
            self.cx.simulate_click(bounds.center(), Modifiers::none());
            self.cx.run_until_parked();
        }

        fn is_drawn(&mut self, id: &'static str) -> bool {
            self.cx.run_until_parked();
            self.cx.debug_bounds(id).is_some()
        }

        fn wait_for_connection(&mut self, status: ConnectionStatus) {
            self.frames_until("the connection", move |shell, _| {
                shell.copies.settings.value().connection == status
            });
        }

        fn wait_for_closed(&mut self) -> Offers {
            self.frames_until("the connection to end", |shell, cx| {
                shell.session_bar.read(cx).offers().ended().is_some()
            });
            self.shell.read_with(self.cx, |shell, cx| {
                shell.session_bar.read(cx).offers().clone()
            })
        }

        fn host(&mut self) -> Option<String> {
            self.shell.read_with(self.cx, |shell, _| {
                shell.game.as_ref().map(|game| game.target.host.clone())
            })
        }
    }

    /// Enter activates a focused control when it's let go.
    fn press_enter(cx: &mut VisualTestContext) {
        cx.simulate_keystrokes("enter");
        cx.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse("enter").unwrap(),
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn with_no_game_the_window_offers_what_is_saved(cx: &mut TestAppContext) {
        let mut launched = launch(&[("mud.example", 4000)], cx);
        assert!(launched.is_drawn("empty-state"));
        assert!(launched.is_drawn("privacy-line"));
        assert!(launched.is_drawn("connect-0"));
        assert!(!launched.is_drawn("chat-area"));
        assert!(!launched.is_drawn("toggle-left-panel"));
    }

    #[gpui::test]
    fn the_saved_servers_reach_settings_with_the_empty_state(cx: &mut TestAppContext) {
        let mut launched = launch(&[("mud.example", 4000)], cx);
        launched.frames_until("the servers in settings", |shell, cx| {
            !shell.settings.read(cx).servers().is_empty()
        });
        launched.shell.read_with(launched.cx, |shell, cx| {
            let servers = shell.settings.read(cx).servers();
            assert_eq!(servers.len(), 1);
            assert_eq!(servers[0].host, "mud.example");
            assert_eq!(servers[0].port, 4000);
        });
    }

    #[gpui::test]
    fn return_on_the_first_card_connects_and_the_chat_takes_over(cx: &mut TestAppContext) {
        let (server, port) = fake_server();
        let mut launched = launch(&[("127.0.0.1", port)], cx);
        press_enter(launched.cx);
        let (mut socket, _) = server.accept().unwrap();
        launched.wait_for_connection(ConnectionStatus::Connected);
        assert!(launched.is_drawn("chat-area"));
        assert!(!launched.is_drawn("empty-state"));

        socket.write_all(b"Hello.\r\n").unwrap();
        launched.frames_until("the line", |shell, cx| {
            shell.chat.read(cx).newest_text().as_deref() == Some("Hello.")
        });
        launched.cx.simulate_input("look");
        launched.cx.simulate_keystrokes("enter");
        let mut heard = [0; 4];
        socket.set_read_timeout(Some(WAIT)).unwrap();
        let mut next = [0];
        while next != *b"l" {
            socket.read_exact(&mut next).unwrap();
        }
        socket.read_exact(&mut heard[1..]).unwrap();
        assert_eq!(&heard[1..], b"ook");
    }

    /// Launches the first saved server with `plugin` turned on, and plays.
    fn launch_playing(plugin: Plugin, cx: &mut TestAppContext) -> (Launched<'_>, TcpStream) {
        let (server, port) = fake_server();
        let enabled = vec![plugin.id().clone()];
        let mut launched = launch_with(&[("127.0.0.1", port)], vec![plugin], enabled, cx);
        press_enter(launched.cx);
        let (socket, _) = server.accept().unwrap();
        launched.wait_for_connection(ConnectionStatus::Connected);
        (launched, socket)
    }

    #[gpui::test]
    fn a_launched_games_words_reach_the_room_panel(cx: &mut TestAppContext) {
        let (mut launched, mut socket) = launch_playing(game_plugin(), cx);
        send_gmcp(
            &mut socket,
            r#"Room.Info {"num": 1, "name": "A hall", "exits": {"n": 2}}"#,
        );
        launched.frames_until("the room", |shell, cx| {
            shell.room_panel.read(cx).room_name() == Some("A hall")
        });
        launched.shell.update(launched.cx, |shell, cx| {
            let (_, _, contents) = crate::measure::sample_room();
            shell
                .room_panel
                .update(cx, |panel, cx| panel.show_contents(contents, cx));
            shell.open_panels(cx);
        });
        launched.cx.run_until_parked();

        launched.click("Get a stone");
        assert_eq!(read_command(&mut socket), "take stone");
    }

    #[gpui::test]
    fn a_launched_games_plugins_reach_settings(cx: &mut TestAppContext) {
        let (launched, _socket) = launch_playing(game_plugin(), cx);
        launched.shell.read_with(launched.cx, |shell, cx| {
            let rows = shell.settings.read(cx).plugins();
            let game = rows.iter().find(|row| row.id.as_str() == "game");
            assert!(game.is_some_and(|row| row.on), "{rows:?}");
        });
    }

    #[gpui::test]
    fn quick_connect_says_why_a_bad_value_is_bad_and_connects_a_good_one(cx: &mut TestAppContext) {
        let (server, port) = fake_server();
        let mut launched = launch(&[], cx);
        launched.cx.simulate_input("127.0.0.1");
        launched.cx.simulate_keystrokes("enter");
        assert!(launched.is_drawn("quick-error"));
        assert_eq!(launched.host(), None);

        launched.cx.simulate_input(&format!(":{port}"));
        launched.cx.simulate_keystrokes("enter");
        launched.cx.run_until_parked();
        let _socket = server.accept().unwrap();
        launched.wait_for_connection(ConnectionStatus::Connected);
    }

    #[gpui::test]
    fn a_server_added_is_saved_and_gets_a_card(cx: &mut TestAppContext) {
        let mut launched = launch(&[], cx);
        launched.click("add-server");
        launched.cx.simulate_input("mud.example");
        launched.cx.simulate_keystrokes("enter");
        assert!(launched.is_drawn("add-error"));
        launched.cx.simulate_input(":4000");
        launched.cx.simulate_keystrokes("enter");
        assert!(launched.is_drawn("connect-0"));
        assert!(!launched.is_drawn("add-error"));

        let saved = persist::saved_servers(launched.folder.path());
        assert_eq!(saved.servers[0].host, "mud.example");
        assert_eq!(saved.servers[0].port, 4000);
    }

    #[gpui::test]
    fn esc_cancels_adding_a_server(cx: &mut TestAppContext) {
        let mut launched = launch(&[], cx);
        launched.click("add-server");
        assert!(launched.is_drawn("save-server"));
        launched.cx.simulate_keystrokes("escape");
        assert!(!launched.is_drawn("save-server"));
        assert!(launched.is_drawn("add-server"));
    }

    #[gpui::test]
    fn the_keyboard_reaches_every_control(cx: &mut TestAppContext) {
        let mut launched = launch(&[("mud.example", 4000)], cx);
        // From the first card: Add, Quick connect's field, then its button.
        launched.cx.simulate_keystrokes("tab");
        press_enter(launched.cx);
        assert!(launched.is_drawn("save-server"), "Add opened the form");
        launched.cx.simulate_keystrokes("escape");
        launched.cx.simulate_keystrokes("tab");
        launched.cx.simulate_input("bad");
        launched.cx.simulate_keystrokes("tab");
        press_enter(launched.cx);
        assert!(launched.is_drawn("quick-error"));
    }

    #[gpui::test]
    fn a_failure_says_what_went_wrong_and_reconnect_needs_no_restart(cx: &mut TestAppContext) {
        let (server, port) = fake_server();
        let (_, closed_port) = fake_server();
        let mut launched = launch(&[("127.0.0.1", closed_port)], cx);
        press_enter(launched.cx);
        let offers = launched.wait_for_closed();
        assert!(
            offers
                .ended()
                .unwrap()
                .starts_with("The connection failed:"),
            "{offers:?}"
        );

        // The server comes back on a port of its own: the card's session is
        // for the closed one, so a new session goes there.
        launched.click("choose-server");
        launched.frames_until("the empty state", |shell, _| shell.on_empty_state());
        launched.cx.simulate_keystrokes("tab tab");
        launched.cx.simulate_input(&format!("127.0.0.1:{port}"));
        launched.cx.simulate_keystrokes("enter");
        launched.cx.run_until_parked();
        let (first, _) = server.accept().unwrap();
        launched.wait_for_connection(ConnectionStatus::Connected);

        drop(first);
        let offers = launched.wait_for_closed();
        assert_eq!(offers.ended(), Some("The server closed the connection."));
        launched.click("reconnect");
        let _second = server.accept().unwrap();
        launched.wait_for_connection(ConnectionStatus::Connected);

        launched.cx.simulate_input("look");
        let typed = launched
            .shell
            .read_with(launched.cx, |shell, cx| shell.typed(cx));
        assert_eq!(typed, "look", "typing goes on in the command bar");
    }

    #[gpui::test]
    fn the_switcher_lists_saved_characters_and_moves_between_them(cx: &mut TestAppContext) {
        let (server, port) = fake_server();
        let mut launched = launch(&[("127.0.0.1", port), (OTHER_HOST, port)], cx);
        launched.click("connect-1");
        let (first, _) = server.accept().unwrap();
        launched.wait_for_connection(ConnectionStatus::Connected);
        let playing = launched.host().unwrap();

        launched.click("session-switcher");
        assert!(launched.is_drawn("session-current"));
        launched.cx.simulate_keystrokes("down");
        press_enter(launched.cx);
        let (_second, _) = server.accept().unwrap();
        launched.wait_for_connection(ConnectionStatus::Connected);
        assert_ne!(launched.host().unwrap(), playing);

        // The first session ended: one runs at a time.
        first.set_read_timeout(Some(WAIT)).unwrap();
        assert_eq!(
            std::io::copy(&mut &first, &mut std::io::sink()).ok(),
            Some(0)
        );
    }

    #[gpui::test]
    fn leaving_for_the_empty_state_ends_the_session(cx: &mut TestAppContext) {
        let (server, port) = fake_server();
        let mut launched = launch(&[("127.0.0.1", port)], cx);
        press_enter(launched.cx);
        let (first, _) = server.accept().unwrap();
        launched.wait_for_connection(ConnectionStatus::Connected);

        launched.click("session-switcher");
        launched.click("session-choose-server");
        launched.frames_until("the empty state", |shell, _| shell.on_empty_state());
        assert!(launched.is_drawn("connect-0"));
        first.set_read_timeout(Some(WAIT)).unwrap();
        assert_eq!(
            std::io::copy(&mut &first, &mut std::io::sink()).ok(),
            Some(0)
        );
    }

    #[gpui::test]
    fn turning_a_suggested_plugin_on_opens_a_new_session_with_it(cx: &mut TestAppContext) {
        let (server, port) = fake_server();
        let mut launched = launch(&[("127.0.0.1", port)], cx);
        press_enter(launched.cx);
        let _first = server.accept().unwrap();
        launched.frames_until("the suggestion", |shell, cx| {
            shell.session_bar.read(cx).offers().turn_on().is_some()
        });
        let enabled = |launched: &mut Launched| {
            launched.shell.read_with(launched.cx, |shell, _| {
                shell.game.as_ref().map_or(0, |game| game.enabled.len())
            })
        };
        assert_eq!(enabled(&mut launched), 0, "detection only suggests");

        launched.click("turn-on-plugin");
        let _second = server.accept().unwrap();
        launched.wait_for_connection(ConnectionStatus::Connected);
        assert_eq!(enabled(&mut launched), 1);
        launched.frames_until("the offer to go", |shell, cx| {
            shell.session_bar.read(cx).offers().turn_on().is_none()
        });
        let saved = persist::enabled_plugins(launched.folder.path(), "127.0.0.1").unwrap();
        assert_eq!(saved, ["loopback"]);
    }

    #[gpui::test]
    fn leaving_closes_the_connection_once(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.connected();
        let saved = played.shell.update(played.cx, |shell, cx| shell.leave(cx));
        played.cx.run_until_parked();
        saved.expect("the first leave").detach();
        played.cx.run_until_parked();

        let mut rest = Vec::new();
        played.server.read_to_end(&mut rest).unwrap();
        let again = played.shell.update(played.cx, |shell, cx| shell.leave(cx));
        assert!(again.is_none());
    }
}
