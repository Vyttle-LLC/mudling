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
use crate::chat::{self, Chat, CloseFind, column_width};
use crate::dock::{Dock, DockState, Side, Slot, fit_docks};
use crate::empty_state::{EmptyState, EmptyStateEvent};
use crate::hotbar::{Hotbar, HotbarEvent};
use crate::hud::Hud;
use crate::input::{self, CommandInput, InputEvent};
use crate::launcher::{Launcher, Target};
use crate::room_panel::{self, RoomPanel, SendCommand, Verbs};
use crate::session_bar::{Offers, SessionBar, SessionBarEvent};
use crate::settings::{PluginRow, SavedServer, SavedSettings, SettingsSheet, SheetEvent};
use crate::sound::Speaker;
use crate::status::{StatusLines, lines_for};
use crate::theme::{Appearance, Theme};
use crate::title_bar::{
    self, FindInScrollback, FocusNextControl, FocusPreviousControl, OpenSettings, SessionLabel,
    TitleBar, TitleBarEvent,
};
use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use gpui::{
    AnyView, App, Context, Entity, FocusHandle, Focusable, KeyBinding, SharedString, Subscription,
    Task, Window, actions, deferred, div, prelude::*,
};
use mudling_core::completion::Completion;
use mudling_core::defaults::hotbar;
use mudling_core::persist::SavedServers;
use mudling_core::plugin::PluginId;
use mudling_core::session::{RuleEdit, Session};
use mudling_core::store::{
    CharactersState, ConnectionStatus, CuesState, MobsState, RoomState, RulesState, SettingsState,
    StoreHub, TargetState, Versioned, VitalsState,
};
use mudling_core::telnet::Bytes;
use std::collections::VecDeque;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

actions!(
    shell,
    [
        ToggleLeftPanel,
        ToggleRightPanel,
        FocusCommandBar,
        FocusRoomPanel,
        FocusHotbar,
        PinCommand,
        FocusNext
    ]
);

/// Above everything else that floats.
const SHEET_PRIORITY: usize = 4;
/// How long ending a session may wait for the disk. A stuck hook or disk can
/// lose what's unsaved, but can't hold up the window or the quit.
pub const QUIT_WAIT: Duration = Duration::from_secs(5);
const STUCK_SESSION: &str = "The last session didn't finish saving in time, so its newest \
                             changes may be lost.";

pub fn bind_keys(cx: &mut App) {
    chat::bind_keys(cx);
    crate::hotbar::bind_keys(cx);
    cx.bind_keys([
        KeyBinding::new(secondary!("1"), ToggleLeftPanel, None),
        KeyBinding::new(secondary!("2"), ToggleRightPanel, None),
        KeyBinding::new(secondary!("3"), FocusRoomPanel, None),
        KeyBinding::new(secondary!("4"), FocusHotbar, None),
        KeyBinding::new(secondary!("p"), PinCommand, None),
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

    /// Ends the session and says what the player should hear about it, if
    /// anything. This blocks until it's on the disk, up to [`QUIT_WAIT`]. One
    /// stuck past that finishes by itself, if it can, on a thread of its own.
    fn finish(self) -> Option<String> {
        let Game {
            session,
            replay_folder,
            ..
        } = self;
        let ended = within(QUIT_WAIT, move || {
            let ended = session.quit().wait();
            drop(replay_folder);
            ended
        });
        match ended {
            Some(Ok(())) => None,
            Some(Err(error)) => Some(format!("Couldn't finish saving the session: {error}")),
            None => Some(STUCK_SESSION.to_owned()),
        }
    }
}

/// Runs `work` on a thread of its own and waits up to `limit` for its result.
/// Past that, it's left to finish alone.
fn within<T: Send + 'static>(
    limit: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let _ = done.send(work());
    });
    finished.recv_timeout(limit).ok()
}

/// The shell's copies of the stores it shows, other than the terminal.
struct Copies {
    settings: Versioned<SettingsState>,
    vitals: Versioned<VitalsState>,
    characters: Versioned<CharactersState>,
    target: Versioned<TargetState>,
    room: Versioned<RoomState>,
    mobs: Versioned<MobsState>,
    rules: Versioned<RulesState>,
}

impl Copies {
    fn of(stores: Option<&StoreHub>) -> Copies {
        // Sounds are only ever read from a batch, so they need no copy.
        if let Some(stores) = stores {
            stores.subscribe::<CuesState>();
        }
        Copies {
            settings: stores.map(|stores| stores.subscribe()).unwrap_or_default(),
            vitals: stores.map(|stores| stores.subscribe()).unwrap_or_default(),
            characters: stores.map(|stores| stores.subscribe()).unwrap_or_default(),
            target: stores.map(|stores| stores.subscribe()).unwrap_or_default(),
            room: stores.map(|stores| stores.subscribe()).unwrap_or_default(),
            mobs: stores.map(|stores| stores.subscribe()).unwrap_or_default(),
            rules: stores.map(|stores| stores.subscribe()).unwrap_or_default(),
        }
    }
}

/// What the player can ask for that takes a job off the UI thread.
enum Request {
    Open {
        target: Target,
        turn_on: Option<PluginId>,
    },
    ChooseServer,
    Save(Target),
    Remove(String),
}

impl Request {
    /// Whether it says where the window goes. Only the newest such ask counts.
    fn moves_the_window(&self) -> bool {
        matches!(self, Request::Open { .. } | Request::ChooseServer)
    }
}

/// What a job off the UI thread comes back with.
#[derive(Default)]
struct Outcome {
    game: Option<Game>,
    saved: SavedServers,
    problem: Option<String>,
    /// A server the player added that couldn't be saved.
    unsaved: Option<Target>,
}

pub struct Shell {
    game: Option<Game>,
    launcher: Option<Launcher>,
    /// The one job running off the UI thread: a session ending, another
    /// opening, a server being saved.
    job: Option<Task<()>>,
    /// What the player asked for while a job ran, to run in turn.
    waiting: VecDeque<Request>,
    /// What the last job ran into, for a window on a game.
    trouble: Option<String>,
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
    hotbar: Entity<Hotbar>,
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
    /// Opened with the first sound: most sessions may never ring one.
    speaker: Option<Speaker>,
    _subscriptions: [Subscription; 11],
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
        let edited = cx.observe(&input, |shell, _, cx| shell.show_alias_preview(cx));
        let hotbar = cx.new(Hotbar::new);
        let chips = cx.subscribe_in(&hotbar, window, |shell, _, event, window, cx| {
            shell.on_hotbar(event, window, cx);
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
            let mut sheet = SettingsSheet::new(window, cx);
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
                    SheetEvent::RemoveServer(host) => {
                        shell.ask(Request::Remove(host.clone()), window, cx);
                    }
                    SheetEvent::TurnOn(plugin) => shell.turn_plugin_on(plugin),
                    SheetEvent::Edit(edit) => shell.edit_rules(edit.clone()),
                    SheetEvent::TrySound(sound) => shell.play_sound(sound),
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
                TitleBarEvent::Open(target) => shell.ask_to_open(target, window, cx),
                TitleBarEvent::ChooseServer => shell.ask(Request::ChooseServer, window, cx),
            },
        );
        let offers = cx.subscribe_in(
            &session_bar,
            window,
            |shell, _, event, window, cx| match event {
                SessionBarEvent::Reconnect => shell.reconnect(window, cx),
                SessionBarEvent::ChooseServer => shell.ask(Request::ChooseServer, window, cx),
                SessionBarEvent::TurnOn(plugin) => shell.turn_on(plugin.clone(), window, cx),
            },
        );
        let places = cx.subscribe_in(
            &empty_state,
            window,
            |shell, _, event, window, cx| match event {
                EmptyStateEvent::Connect(target) => shell.ask_to_open(target, window, cx),
                EmptyStateEvent::Add(target) => {
                    shell.ask(Request::Save(target.clone()), window, cx);
                }
            },
        );
        let mut shell = Shell {
            game,
            launcher: None,
            job: None,
            waiting: VecDeque::new(),
            trouble: None,
            switching: false,
            copies,
            left: false,
            chat,
            status: cx.new(|_| StatusLines::default()),
            session_bar,
            empty_state,
            title_bar,
            hud: cx.new(|_| Hud::default()),
            hotbar,
            input,
            left_dock,
            right_dock,
            room_panel,
            settings,
            settings_open: false,
            focus_before_settings: None,
            saved_settings: None,
            speaker: None,
            _subscriptions: [
                typing,
                edited,
                chips,
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
        shell.show_rules(cx);
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
                saved: launcher.saved(),
                ..Outcome::default()
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
        self.trouble = outcome.problem.clone();
        self.empty_state.update(cx, |empty_state, cx| {
            empty_state.show(&outcome.saved, cx);
            if let Some(problem) = outcome.problem.clone() {
                empty_state.show_problem(problem, cx);
            }
            if let Some(target) = outcome.unsaved {
                empty_state.keep_address(&target, window, cx);
            }
        });
        self.settings.update(cx, |sheet, cx| {
            let servers = outcome.saved.servers.iter().map(SavedServer::from);
            sheet.show_servers(servers.collect(), outcome.problem, cx);
        });
        if let Some(game) = outcome.game {
            self.play(game, window, cx);
        }
        self.show_where_things_stand(cx);
        // A sheet that's open keeps the keyboard. What it goes back to may be
        // gone by the time it closes, as a removed server's card is.
        if self.settings_open {
            self.focus_before_settings = None;
        } else if self.on_empty_state() {
            self.empty_state
                .update(cx, |empty_state, cx| empty_state.focus(window, cx));
        }
        while self.job.is_none()
            && let Some(request) = self.waiting.pop_front()
        {
            self.start(request, window, cx);
        }
    }

    fn ask_to_open(&mut self, target: &Target, window: &mut Window, cx: &mut Context<Shell>) {
        let target = target.clone();
        let turn_on = None;
        self.ask(Request::Open { target, turn_on }, window, cx);
    }

    /// Does what the player asked now, or once the running job is done.
    fn ask(&mut self, request: Request, window: &mut Window, cx: &mut Context<Shell>) {
        if self.job.is_none() {
            self.start(request, window, cx);
            return;
        }
        if request.moves_the_window() {
            self.waiting.retain(|waiting| !waiting.moves_the_window());
        }
        self.waiting.push_back(request);
    }

    fn start(&mut self, request: Request, window: &mut Window, cx: &mut Context<Shell>) {
        match request {
            Request::Open { target, turn_on } => self.open(target, turn_on, window, cx),
            Request::ChooseServer => self.choose_server(window, cx),
            Request::Save(target) => self.add_server(target, window, cx),
            Request::Remove(host) => self.remove_server(host, window, cx),
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
        self.show_rules(cx);
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
    /// plugins when it opens, so this is also how that takes effect. A repeat
    /// of what's already running, such as a double click, does nothing.
    fn open(
        &mut self,
        target: Target,
        turn_on: Option<PluginId>,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) {
        let running = self.game.as_ref().is_some_and(|game| {
            game.target == target
                && !game.session.has_stopped()
                && turn_on
                    .as_ref()
                    .is_none_or(|plugin| game.enabled.contains(plugin))
        });
        if running || self.launcher.is_none() {
            return;
        }
        let leaving = self.game.take();
        self.switching = leaving.is_some();
        let connecting = SessionLabel::new(target.host.clone(), None, "Connecting…");
        self.title_bar
            .update(cx, |bar, cx| bar.show(connecting, cx));
        self.off_the_ui_thread(
            move |launcher| {
                let stuck = leaving.and_then(Game::finish);
                let opened = launcher.open(&target, turn_on.as_ref());
                let (game, problem) = match opened {
                    Ok(game) => (Some(game), None),
                    Err(problem) => (None, Some(problem)),
                };
                Outcome {
                    game,
                    saved: launcher.saved(),
                    problem: problem.or(stuck),
                    ..Outcome::default()
                }
            },
            window,
            cx,
        );
    }

    /// Ends the session and goes back to the empty state.
    fn choose_server(&mut self, window: &mut Window, cx: &mut Context<Shell>) {
        let leaving = self.game.take();
        self.show_where_things_stand(cx);
        self.off_the_ui_thread(
            move |launcher| Outcome {
                problem: leaving.and_then(Game::finish),
                saved: launcher.saved(),
                ..Outcome::default()
            },
            window,
            cx,
        );
    }

    fn add_server(&mut self, target: Target, window: &mut Window, cx: &mut Context<Shell>) {
        self.off_the_ui_thread(
            move |launcher| {
                let problem = launcher.save(&target).err();
                Outcome {
                    unsaved: problem.is_some().then_some(target),
                    problem,
                    saved: launcher.saved(),
                    ..Outcome::default()
                }
            },
            window,
            cx,
        );
    }

    /// Takes a server off the list, unless the game running is on it: its
    /// session would write the server back.
    fn remove_server(&mut self, host: String, window: &mut Window, cx: &mut Context<Shell>) {
        let playing = self
            .game
            .as_ref()
            .is_some_and(|game| game.target.host == host);
        self.off_the_ui_thread(
            move |launcher| Outcome {
                problem: match playing {
                    true => Some(format!("{host} is in play, so it stays on the list.")),
                    false => launcher.remove(&host).err(),
                },
                saved: launcher.saved(),
                ..Outcome::default()
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
            let turn_on = None;
            self.ask(Request::Open { target, turn_on }, window, cx);
        } else {
            game.session.connect();
            window.focus(&self.input.focus_handle(cx), cx);
        }
    }

    fn turn_on(&mut self, plugin: PluginId, window: &mut Window, cx: &mut Context<Shell>) {
        if let Some(game) = &self.game {
            let target = game.target.clone();
            let turn_on = Some(plugin);
            self.ask(Request::Open { target, turn_on }, window, cx);
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
        match self.focus_before_settings.take() {
            Some(focused) => window.focus(&focused, cx),
            None => self.focus_command_bar(window, cx),
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
            self.hotbar
                .update(cx, |hotbar, cx| hotbar.set_locked(masked, cx));
        }
        if let Some(rules) = &batch.rules {
            self.copies.rules.apply_diff(rules);
            self.show_rules(cx);
        }
        if let Some(cues) = batch.cues.as_ref().filter(|_| !self.is_replaying()) {
            cues.change.iter().for_each(|sound| self.play_sound(sound));
        }
        if let Some(characters) = &batch.characters {
            self.copies.characters.apply_diff(characters);
        }
        if let Some(target) = &batch.target {
            self.copies.target.apply_diff(target);
        }
        if batch.vitals.is_some()
            || batch.settings.is_some()
            || batch.characters.is_some()
            || batch.target.is_some()
        {
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

    fn show_rules(&mut self, cx: &mut Context<Shell>) {
        let rules = self.copies.rules.value();
        self.hotbar
            .update(cx, |hotbar, cx| hotbar.show(&rules.hotbar, cx));
        self.settings
            .update(cx, |sheet, cx| sheet.show_rules(rules, cx));
        self.show_alias_preview(cx);
    }

    fn is_replaying(&self) -> bool {
        self.game.as_ref().is_some_and(Game::is_replay)
    }

    fn play_sound(&mut self, sound: &str) {
        self.speaker.get_or_insert_with(Speaker::start).play(sound);
    }

    fn edit_rules(&self, edit: RuleEdit) {
        if let Some(game) = &self.game {
            game.session.edit(edit);
        }
    }

    /// Fills the bar's alias slot from what's typed. A password shows nothing
    /// and asks nothing.
    fn show_alias_preview(&mut self, cx: &mut Context<Shell>) {
        let Some(game) = &self.game else { return };
        let input = self.input.read(cx);
        let typed = input.content();
        let one_line = !input.is_masked() && !typed.contains('\n');
        let preview = one_line
            .then(|| game.session.alias_preview(typed))
            .flatten()
            .map(SharedString::from);
        self.input
            .update(cx, |input, cx| input.set_alias_preview(preview, cx));
    }

    fn on_hotbar(&mut self, event: &HotbarEvent, window: &mut Window, cx: &mut Context<Shell>) {
        match event {
            HotbarEvent::Pressed {
                command,
                by_keyboard,
            } => {
                self.press(command, cx);
                if !by_keyboard {
                    window.focus(&self.input.focus_handle(cx), cx);
                }
            }
            HotbarEvent::Changed(chips) => self.edit_rules(RuleEdit::SetHotbar(chips.clone())),
            HotbarEvent::Left => window.focus(&self.input.focus_handle(cx), cx),
        }
    }

    /// What's typed goes on the hotbar, and stays typed.
    fn pin_typed(&mut self, typed: &str, cx: &mut Context<Shell>) {
        let mut chips = self.hotbar.read(cx).chips().to_vec();
        if hotbar::pin(&mut chips, typed) {
            self.hotbar.update(cx, |hotbar, cx| hotbar.show(&chips, cx));
            self.edit_rules(RuleEdit::SetHotbar(chips));
        }
    }

    /// Runs a command pressed rather than typed. The session refuses it while
    /// the server asks for a password; with no server connected the chat shows
    /// it here, as it does what's typed.
    fn press(&mut self, command: &str, cx: &mut Context<Shell>) {
        let settings = self.copies.settings.value();
        if settings.echo_off {
            return;
        }
        match &self.game {
            Some(game)
                if settings.connection == ConnectionStatus::Connected && !game.is_replay() =>
            {
                game.session.press(command);
            }
            _ => self
                .chat
                .update(cx, |chat, cx| chat.echo_unsent(command, cx)),
        }
    }

    fn show_room(&mut self, cx: &mut Context<Shell>) {
        let (room, mobs) = (self.copies.room.value(), self.copies.mobs.value());
        self.room_panel.update(cx, |panel, cx| {
            panel.show_room(room, mobs, cx);
            panel.show_contents(room.contents.clone(), cx);
        });
    }

    fn show_status(&mut self, cx: &mut Context<Shell>) {
        let Some(game) = &self.game else {
            self.forget_the_session(cx);
            return;
        };
        let settings = self.copies.settings.value();
        self.show_problems(settings, cx);
        let trouble = self.trouble.clone().map(SharedString::from);
        let lines = trouble.into_iter().chain(lines_for(settings)).collect();
        let characters = self.copies.characters.value();
        let session = SessionLabel::of(settings, characters, game.is_replay(), settings.latency);
        let launcher = self.launcher.as_ref();
        let offers = Offers::of(settings, &game.enabled, game.is_replay(), launcher);
        self.status.update(cx, |status, cx| status.show(lines, cx));
        self.session_bar.update(cx, |bar, cx| bar.show(offers, cx));
        self.title_bar.update(cx, |bar, cx| bar.show(session, cx));
        self.hud.update(cx, |hud, cx| {
            let target = self.copies.target.value();
            hud.show(self.copies.vitals.value(), characters, target, cx);
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
        let find_open = self.chat.read(cx).find_is_open();
        self.settings
            .update(cx, |sheet, cx| sheet.show_playing(current, cx));
        self.title_bar.update(cx, |bar, cx| {
            bar.show_saved(elsewhere, can_choose_server, cx);
            bar.set_bare(bare, cx);
            bar.show_find(find_open, cx);
            if bare {
                bar.show(SessionLabel::default(), cx);
            }
        });
        self.show_status(cx);
        cx.notify();
    }

    /// With no game, nothing is running: what the settings sheet and title bar
    /// said about the last session goes with it.
    fn forget_the_session(&self, cx: &mut Context<Shell>) {
        self.show_problems(&SettingsState::default(), cx);
        self.settings
            .update(cx, |sheet, cx| sheet.show_plugins(Vec::new(), cx));
    }

    fn show_problems(&self, settings: &SettingsState, cx: &mut Context<Shell>) {
        self.settings
            .update(cx, |sheet, cx| sheet.show_session(settings, cx));
        let problems = settings.notices.len() + settings.turned_off.len();
        self.title_bar
            .update(cx, |bar, cx| bar.show_problems(problems, cx));
    }

    fn turn_plugin_on(&self, plugin: &PluginId) {
        if let Some(game) = &self.game {
            game.session.turn_plugin_on(plugin.clone());
        }
    }

    fn on_typing(&mut self, event: &InputEvent, window: &mut Window, cx: &mut Context<Shell>) {
        match event {
            InputEvent::Submitted(command) => return self.submit(command, cx),
            InputEvent::Pin(typed) => return self.pin_typed(typed, cx),
            _ => {}
        }
        let Some(game) = &self.game else { return };
        let at_the_end = |line: String| Completion {
            cursor: line.len(),
            line,
        };
        let shown = match event {
            InputEvent::Submitted(_) | InputEvent::Pin(_) => None,
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
            self.send_with(
                &sent.command,
                |session, command| {
                    session.send_action(command);
                },
                cx,
            );
        }
        if !sent.by_keyboard {
            window.focus(&self.input.focus_handle(cx), cx);
        }
    }

    /// Sends what was typed, a command to each line; the session echoes them,
    /// unless it's a password. With no server connected they go nowhere, so
    /// the chat shows them here.
    fn submit(&mut self, typed: &str, cx: &mut Context<Shell>) {
        self.send_with(typed, |session, command| session.send(command), cx);
    }

    fn send_with(&mut self, typed: &str, send: impl Fn(&Session, &str), cx: &mut Context<Shell>) {
        let settings = self.copies.settings.value();
        let connected = settings.connection == ConnectionStatus::Connected;
        for command in typed.split('\n') {
            match &self.game {
                Some(game) if connected && !game.is_replay() => send(&game.session, command),
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

    /// Opens the find bar over the scrollback, or goes back to it.
    pub fn open_find(&self, window: &mut Window, cx: &mut Context<Shell>) {
        if self.on_empty_state() {
            return;
        }
        self.title_bar.update(cx, |bar, cx| {
            bar.dismiss_menu(window, cx);
            bar.show_find(true, cx);
        });
        self.chat.update(cx, |chat, cx| chat.open_find(window, cx));
    }

    /// Closes the find bar, and typing goes back to the command bar.
    pub fn close_find(&self, window: &mut Window, cx: &mut Context<Shell>) {
        self.chat.update(cx, |chat, cx| chat.close_find(cx));
        self.title_bar
            .update(cx, |bar, cx| bar.show_find(false, cx));
        self.focus_command_bar(window, cx);
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

    /// Moves the keyboard to the hotbar, opening nothing: it's always there.
    fn focus_hotbar(&self, window: &mut Window, cx: &mut Context<Shell>) {
        if !self.on_empty_state() {
            self.hotbar
                .update(cx, |hotbar, cx| hotbar.focus(window, cx));
        }
    }

    pub fn hotbar(&self) -> Entity<Hotbar> {
        self.hotbar.clone()
    }

    #[cfg(test)]
    pub fn title_bar_shows_find(&self, cx: &App) -> bool {
        self.title_bar.read(cx).find_open()
    }

    #[cfg(test)]
    pub fn settings_sheet(&self) -> Entity<SettingsSheet> {
        self.settings.clone()
    }

    #[cfg(test)]
    pub fn show_sample_rules(&self, rules: RulesState, cx: &mut Context<Shell>) {
        self.settings
            .update(cx, |sheet, cx| sheet.show_rules(&rules, cx));
    }

    #[cfg(test)]
    pub fn show_sample_alias_preview(&self, preview: &'static str, cx: &mut Context<Shell>) {
        self.input.update(cx, |input, cx| {
            input.set_alias_preview(Some(preview.into()), cx)
        });
    }

    #[cfg(test)]
    pub fn show_sample_problems(&self, settings: &SettingsState, cx: &mut Context<Shell>) {
        self.show_problems(settings, cx);
    }

    #[cfg(test)]
    pub fn show_sample_vitals(&self, vitals: &VitalsState, cx: &mut Context<Shell>) {
        let characters = CharactersState::default();
        self.hud.update(cx, |hud, cx| {
            hud.show(vitals, &characters, &TargetState::default(), cx);
        });
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

    #[cfg(target_os = "macos")]
    pub fn empty_state(&self) -> Entity<EmptyState> {
        self.empty_state.clone()
    }

    /// Goes from a replay to the empty state of `launcher`, for `--ax-check`.
    #[cfg(target_os = "macos")]
    pub fn leave_for_empty_state(
        &mut self,
        launcher: Launcher,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) {
        self.settings_open = false;
        self.use_launcher(launcher, window, cx);
        self.ask(Request::ChooseServer, window, cx);
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
            if let Some(problem) = game.and_then(Game::finish) {
                eprintln!("{problem}");
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
                    .child(self.hotbar.clone())
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
        chat::handle_keys(div(), &self.chat, cx)
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(theme.text)
            .font_family(UI_FONT)
            // End closes the split once the command bar has no caret left to
            // move. Other fields, like find, keep it for themselves.
            .capture_action(cx.listener(|shell, _: &input::End, window, cx| {
                let input = shell.input.read(cx);
                if input.focus_handle(cx).is_focused(window) && input.caret_at_end() {
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
            .on_action(cx.listener(|shell, _: &FocusHotbar, window, cx| {
                shell.focus_hotbar(window, cx);
            }))
            .on_action(cx.listener(|shell, _: &PinCommand, _, cx| {
                shell.input.update(cx, |input, cx| input.pin(cx));
            }))
            .on_action(cx.listener(|shell, _: &FindInScrollback, window, cx| {
                shell.open_find(window, cx);
            }))
            .on_action(cx.listener(|shell, _: &CloseFind, window, cx| {
                shell.close_find(window, cx);
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
    use crate::settings::Section;
    use futures::channel::mpsc::{UnboundedSender, unbounded};
    use gpui::{
        Modifiers, MouseButton, Pixels, TestAppContext, VisualTestContext, point, px, size,
    };
    use mudling_core::defaults::aliases::AliasRule;
    use mudling_core::defaults::hud::Vital;
    use mudling_core::persist;
    use mudling_core::plugin::Plugin;
    use mudling_core::plugin::luau::{DisabledReason, PluginDisabled};
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

        /// Like [`Self::frames_until`], with a line sent each time round, which
        /// is what keeps a session's clock ticking.
        fn frames_with_traffic_until(
            &mut self,
            what: &str,
            done: impl Fn(&Shell, &gpui::App) -> bool,
        ) {
            let started = Instant::now();
            while !self.shell.read_with(self.cx, |shell, cx| done(shell, cx)) {
                assert!(started.elapsed() < WAIT, "timed out waiting for {what}");
                self.say(b"\r\n");
                self.frame();
                sleep(Duration::from_millis(1));
            }
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

    fn press_find(cx: &mut VisualTestContext) {
        let modifier = if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        };
        cx.simulate_keystrokes(&format!("{modifier}-f"));
        cx.run_until_parked();
    }

    /// A window on no session with the app's keys, and a page of lines, every
    /// other one a goblin, found through the find bar.
    fn finding_window(cx: &mut TestAppContext) -> (Entity<Shell>, &mut VisualTestContext) {
        let (shell, cx) = empty_window(cx);
        cx.update(|_, cx| crate::bind_keys(cx));
        let chat = shell.read_with(cx, |shell, _| shell.chat());
        chat.update(cx, |chat, cx| {
            for number in 0..500 {
                chat.echo_unsent("a line", cx);
                chat.echo_unsent(&format!("a goblin {number}"), cx);
            }
        });
        cx.run_until_parked();
        press_find(cx);
        cx.simulate_input("goblin");
        cx.run_until_parked();
        (shell, cx)
    }

    fn top_line(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> u64 {
        shell.read_with(cx, |shell, cx| shell.chat.read(cx).scroll_offset().line)
    }

    fn find_is_open(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> bool {
        shell.read_with(cx, |shell, cx| shell.chat.read(cx).find_is_open())
    }

    #[gpui::test]
    fn the_find_key_takes_typing_and_esc_gives_it_back(cx: &mut TestAppContext) {
        let (shell, cx) = finding_window(cx);
        assert_eq!(shell.read_with(cx, |shell, cx| shell.typed(cx)), "");
        assert_eq!(
            shell.read_with(cx, |shell, cx| shell.chat.read(cx).find_count()),
            Some(500)
        );

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!find_is_open(&shell, cx));
        assert!(!shell.read_with(cx, |shell, cx| shell.title_bar_shows_find(cx)));
        assert!(is_split(&shell, cx), "the result stays open to read");

        cx.simulate_input("look");
        assert_eq!(shell.read_with(cx, |shell, cx| shell.typed(cx)), "look");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!is_split(&shell, cx));
    }

    #[gpui::test]
    fn return_walks_the_results_from_the_field(cx: &mut TestAppContext) {
        let (shell, cx) = finding_window(cx);
        assert_eq!(top_line(&shell, cx), 999);

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(top_line(&shell, cx), 997);
        cx.simulate_keystrokes("shift-enter");
        cx.run_until_parked();
        assert_eq!(top_line(&shell, cx), 999);
        assert_eq!(shell.read_with(cx, |shell, cx| shell.typed(cx)), "");
    }

    #[gpui::test]
    fn end_in_the_find_field_stays_in_the_field(cx: &mut TestAppContext) {
        let (shell, cx) = finding_window(cx);
        cx.simulate_keystrokes("end");
        cx.run_until_parked();
        assert!(is_split(&shell, cx));
    }

    #[gpui::test]
    fn esc_closes_find_from_the_command_bar_before_the_split(cx: &mut TestAppContext) {
        let (shell, cx) = finding_window(cx);
        shell.update_in(cx, |shell, window, cx| {
            shell.focus_command_bar(window, cx);
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!find_is_open(&shell, cx));
        assert!(is_split(&shell, cx));
    }

    #[gpui::test]
    fn the_find_key_does_nothing_on_the_empty_state(cx: &mut TestAppContext) {
        let (shell, cx) = empty_window(cx);
        cx.update(|_, cx| crate::bind_keys(cx));
        shell.update_in(cx, |shell, window, cx| {
            shell.show_sample_empty_state(&SavedServers::default(), window, cx);
        });
        cx.run_until_parked();
        press_find(cx);
        assert!(!find_is_open(&shell, cx));
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

        fn cards(&self) -> usize {
            self.shell.read_with(self.cx, |shell, cx| {
                shell.empty_state.read(cx).cards().len()
            })
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

    fn saved_hosts(folder: &Path) -> Vec<String> {
        let servers = persist::saved_servers(folder).servers;
        servers.into_iter().map(|server| server.host).collect()
    }

    fn target(host: &str, port: u16) -> Target {
        let host = host.to_owned();
        Target { host, port }
    }

    #[test]
    fn work_that_sticks_is_left_behind_at_the_limit() {
        assert_eq!(within(WAIT, || 7), Some(7));

        let (_held_open, never) = mpsc::channel::<()>();
        let started = Instant::now();
        let stuck = within(Duration::from_millis(20), move || never.recv().ok());
        assert_eq!(stuck, None);
        assert!(started.elapsed() < WAIT);
    }

    #[gpui::test]
    fn trouble_ending_a_session_reaches_the_player_in_the_next_one(cx: &mut TestAppContext) {
        let played = play(cx);
        played.shell.update_in(played.cx, |shell, window, cx| {
            let problem = Some(STUCK_SESSION.to_owned());
            let outcome = Outcome {
                problem,
                ..Outcome::default()
            };
            shell.job_done(outcome, window, cx);
        });
        played.shell.read_with(played.cx, |shell, cx| {
            assert_eq!(shell.status.read(cx).lines(), [STUCK_SESSION]);
        });
    }

    #[gpui::test]
    fn servers_added_while_a_job_runs_are_saved_in_turn(cx: &mut TestAppContext) {
        let mut launched = launch(&[], cx);
        launched.shell.update_in(launched.cx, |shell, window, cx| {
            shell.ask(Request::Save(target("one.example", 1)), window, cx);
            shell.ask(Request::Save(target("two.example", 2)), window, cx);
            assert_eq!(shell.waiting.len(), 1);
        });
        launched.frames_until("both cards", |shell, cx| {
            shell.empty_state.read(cx).cards().len() == 2
        });
        let hosts = saved_hosts(launched.folder.path());
        assert_eq!(hosts, ["one.example", "two.example"]);
    }

    #[gpui::test]
    fn only_the_newest_ask_to_go_somewhere_waits(cx: &mut TestAppContext) {
        let launched = launch(&[], cx);
        launched.shell.update_in(launched.cx, |shell, window, cx| {
            shell.ask(Request::Save(target("one.example", 1)), window, cx);
            shell.ask_to_open(&target("first.example", 1), window, cx);
            shell.ask(Request::Save(target("two.example", 2)), window, cx);
            shell.ask_to_open(&target("second.example", 2), window, cx);

            let waiting: Vec<_> = shell
                .waiting
                .iter()
                .map(Request::moves_the_window)
                .collect();
            assert_eq!(waiting, [false, true]);
            shell.waiting.clear();
        });
    }

    #[gpui::test]
    fn connecting_twice_in_a_row_opens_one_session(cx: &mut TestAppContext) {
        let (server, port) = fake_server();
        let mut launched = launch(&[("127.0.0.1", port)], cx);
        launched.shell.update_in(launched.cx, |shell, window, cx| {
            shell.ask_to_open(&target("127.0.0.1", port), window, cx);
            shell.ask_to_open(&target("127.0.0.1", port), window, cx);
        });
        launched.cx.run_until_parked();
        let (_socket, _) = server.accept().unwrap();
        launched.wait_for_connection(ConnectionStatus::Connected);

        launched.cx.run_until_parked();
        server.set_nonblocking(true).unwrap();
        assert!(server.accept().is_err(), "a second connection");
    }

    #[gpui::test]
    fn an_address_that_could_not_be_saved_goes_back_in_the_field(cx: &mut TestAppContext) {
        let launched = launch(&[], cx);
        std::fs::write(launched.folder.path().join(persist::SERVERS_FOLDER), "").unwrap();
        launched.shell.update_in(launched.cx, |shell, window, cx| {
            shell.ask(Request::Save(target("mud.example", 4000)), window, cx);
        });
        launched.cx.run_until_parked();
        launched.shell.read_with(launched.cx, |shell, cx| {
            let kept = shell.empty_state.read(cx).address_being_added(cx);
            assert_eq!(kept.as_deref(), Some("mud.example:4000"));
        });
    }

    fn open_servers(launched: &mut Launched<'_>) {
        launched.shell.update_in(launched.cx, |shell, window, cx| {
            shell.open_settings(window, cx);
            shell
                .settings
                .update(cx, |sheet, cx| sheet.select(Section::Servers, cx));
        });
        launched.cx.run_until_parked();
    }

    #[gpui::test]
    fn removing_a_server_asks_first_and_keeps_its_database(cx: &mut TestAppContext) {
        let mut launched = launch(&[("mud.example", 4000), ("typo.example", 23)], cx);
        open_servers(&mut launched);

        launched.click("remove-server-1");
        launched.click("keep-server-1");
        assert_eq!(launched.cards(), 2);

        launched.click("remove-server-1");
        launched.click("confirm-remove-server-1");
        launched.frames_until("one card", |shell, cx| {
            shell.empty_state.read(cx).cards().len() == 1
        });
        assert_eq!(saved_hosts(launched.folder.path()), ["mud.example"]);
        let servers = launched.folder.path().join(persist::SERVERS_FOLDER);
        assert!(servers.join("typo.example.sqlite").exists());
    }

    #[gpui::test]
    fn the_sheet_keeps_the_keyboard_after_a_removal(cx: &mut TestAppContext) {
        let (server, port) = fake_server();
        let saved = [("127.0.0.1", port), ("typo.example", 23)];
        let mut launched = launch(&saved, cx);
        launched.cx.update(|_, cx| crate::settings::bind_keys(cx));
        open_servers(&mut launched);

        launched.click("remove-server-1");
        launched.click("confirm-remove-server-1");
        launched.frames_until("one card", |shell, cx| {
            shell.empty_state.read(cx).cards().len() == 1
        });

        // Behind the sheet, Return would connect to the card that's left.
        press_enter(launched.cx);
        launched.cx.run_until_parked();
        server.set_nonblocking(true).unwrap();
        assert!(server.accept().is_err(), "connected from behind the sheet");

        launched.cx.simulate_keystrokes("escape");
        launched.cx.run_until_parked();
        assert!(!launched.is_drawn("settings-sheet"));
        press_enter(launched.cx);
        launched.frames_until("the game", |shell, _| shell.game.is_some());
    }

    #[gpui::test]
    fn the_last_server_removed_leaves_the_keyboard_on_quick_connect(cx: &mut TestAppContext) {
        let mut launched = launch(&[("typo.example", 23)], cx);
        launched.cx.update(|_, cx| crate::settings::bind_keys(cx));
        open_servers(&mut launched);
        launched.click("remove-server-0");
        launched.click("confirm-remove-server-0");
        launched.frames_until("no cards", |shell, cx| {
            shell.empty_state.read(cx).cards().is_empty()
        });

        launched.cx.simulate_keystrokes("escape");
        launched.cx.run_until_parked();
        launched.cx.simulate_input("mud.example:4000");
        launched.shell.read_with(launched.cx, |shell, cx| {
            assert_eq!(
                shell.empty_state.read(cx).quick_typed(cx),
                "mud.example:4000"
            );
        });
    }

    #[gpui::test]
    fn playing_on_a_removed_server_lists_it_again(cx: &mut TestAppContext) {
        let (server, port) = fake_server();
        let mut launched = launch(&[("127.0.0.1", port)], cx);
        persist::remove_server(launched.folder.path(), "127.0.0.1").unwrap();
        launched.shell.update_in(launched.cx, |shell, window, cx| {
            shell.ask_to_open(&target("127.0.0.1", port), window, cx);
        });
        launched.cx.run_until_parked();
        let (_socket, _) = server.accept().unwrap();
        launched.wait_for_connection(ConnectionStatus::Connected);
        assert_eq!(saved_hosts(launched.folder.path()), ["127.0.0.1"]);
    }

    #[gpui::test]
    fn the_server_in_play_offers_no_remove(cx: &mut TestAppContext) {
        let (mut launched, _socket) = launch_playing(game_plugin(), cx);
        open_servers(&mut launched);
        assert!(launched.cx.debug_bounds("remove-server-0").is_none());
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
    fn leaving_for_the_empty_state_forgets_the_sessions_problems(cx: &mut TestAppContext) {
        let (mut launched, _socket) = launch_playing(game_plugin(), cx);
        let settings = SettingsState {
            notices: vec!["a problem".to_owned()],
            turned_off: vec![PluginDisabled {
                plugin: PluginId::try_from("game".to_owned()).unwrap(),
                reason: DisabledReason::Overran(3),
            }],
            ..SettingsState::default()
        };
        launched.shell.update(launched.cx, |shell, cx| {
            shell.show_sample_problems(&settings, cx);
        });
        launched.cx.run_until_parked();
        assert!(launched.is_drawn("settings-badge"));

        launched.click("session-switcher");
        launched.click("session-choose-server");
        launched.frames_until("the empty state", |shell, _| shell.on_empty_state());
        assert!(!launched.is_drawn("settings-badge"));
        launched.shell.read_with(launched.cx, |shell, cx| {
            let sheet = shell.settings.read(cx);
            assert!(sheet.turned_off().is_empty());
            assert!(sheet.costs().is_empty());
            assert!(sheet.plugins().is_empty());
        });
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

    /// A game plugin whose `go` hook never ends.
    fn slow_plugin() -> Plugin {
        Plugin::from_files(|file| {
            Ok(match file {
                "plugin.toml" => "id = \"slow\"\nname = \"Slow\"\nversion = \"1.0.0\"\napi = 1\n\
                                  license = \"MIT\"\nlayer = \"game\"\n\
                                  [servers]\nhosts = [\"mud.example\"]\n\
                                  [points]\ntext = \"text.toml\"\n[hooks]\nfiles = [\"hooks.luau\"]\n"
                    .to_owned(),
                "text.toml" => "[[pattern]]\nname = \"go\"\npattern = '''^go$'''\nhook = \"on_go\"\n"
                    .to_owned(),
                "hooks.luau" => "function on_go(event) while true do end end".to_owned(),
                _ => return Err(std::io::ErrorKind::NotFound.into()),
            })
        })
        .expect("the plugin loads")
    }

    #[gpui::test]
    fn a_plugin_turned_off_shows_in_settings_and_turns_on_again_through_the_session(
        cx: &mut TestAppContext,
    ) {
        let mut played = play_with(cx, vec![slow_plugin()]);
        played.connected();
        played.say(b"go\r\ngo\r\ngo\r\n");
        played.frames_with_traffic_until("the plugin to be turned off", |shell, cx| {
            !shell.settings.read(cx).turned_off().is_empty()
        });
        played.shell.read_with(played.cx, |shell, cx| {
            let sheet = shell.settings.read(cx);
            assert_eq!(sheet.turned_off()[0].plugin.as_str(), "slow");
            assert!(
                sheet
                    .costs()
                    .iter()
                    .any(|cost| cost.plugin.as_str() == "slow")
            );
        });

        played.shell.update_in(played.cx, |shell, window, cx| {
            shell.open_settings(window, cx);
            shell
                .settings
                .update(cx, |sheet, cx| sheet.select(Section::Plugins, cx));
        });
        played.cx.run_until_parked();
        assert!(
            played.cx.debug_bounds("notice-0").is_some(),
            "the notice is drawn"
        );

        played.click("notice-turn-on-0");
        played.frames_with_traffic_until("the plugin to come back", |shell, cx| {
            shell.settings.read(cx).turned_off().is_empty()
        });
        played.cx.run_until_parked();
        assert!(
            played.cx.debug_bounds("notice-0").is_none(),
            "the notice went"
        );
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

    impl Played<'_> {
        fn pin(&mut self, typed: &str) {
            self.cx.simulate_input(typed);
            self.cx.simulate_keystrokes(secondary!("p"));
            self.shell.update_in(self.cx, |shell, window, cx| {
                shell.type_sample("", window, cx);
            });
            let typed = typed.to_owned();
            self.frames_until("the chip", move |shell, cx| {
                shell.hotbar.read(cx).chips().contains(&typed)
            });
        }

        fn chips(&mut self) -> Vec<String> {
            self.shell
                .read_with(self.cx, |shell, cx| shell.hotbar.read(cx).chips().to_vec())
        }

        fn saved_chips(&mut self) -> Vec<String> {
            self.shell.read_with(self.cx, |shell, _| {
                shell.copies.rules.value().hotbar.clone()
            })
        }
    }

    #[gpui::test]
    fn a_typed_command_pins_and_its_chip_sends_it(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.connected();
        played.pin("look");
        played.click("hotbar-chip-0");
        assert_eq!(played.heard_command(), "look");
        played.cx.simulate_input("score");
        played.shell.read_with(played.cx, |shell, cx| {
            assert_eq!(shell.input.read(cx).content(), "score", "typing stays");
        });
    }

    #[gpui::test]
    fn the_keyboard_reaches_the_chips_and_moves_them(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.connected();
        played.pin("look");
        played.pin("score");
        played.cx.run_until_parked();

        played.cx.simulate_keystrokes(secondary!("4"));
        played.cx.simulate_keystrokes("shift-right");
        played.cx.run_until_parked();
        assert_eq!(played.chips(), ["score", "look"]);
        played.frames_until("the saved order", |shell, _| {
            shell
                .copies
                .rules
                .value()
                .hotbar
                .first()
                .is_some_and(|chip| chip == "score")
        });
        assert_eq!(played.saved_chips(), ["score", "look"]);

        press_key("enter", played.cx);
        assert_eq!(played.heard_command(), "look", "focus went with the chip");

        played.cx.simulate_keystrokes("left");
        press_key("enter", played.cx);
        assert_eq!(played.heard_command(), "score");

        played.cx.simulate_keystrokes("delete");
        played.cx.run_until_parked();
        assert_eq!(played.chips(), ["look"]);

        played.cx.simulate_keystrokes("escape");
        played.cx.simulate_input("look");
        played.shell.read_with(played.cx, |shell, cx| {
            assert_eq!(shell.input.read(cx).content(), "look");
        });
    }

    #[gpui::test]
    fn a_chip_edits_in_place_and_letters_stay_letters(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.connected();
        played.pin("look");
        played.cx.run_until_parked();

        played.cx.simulate_keystrokes(secondary!("4"));
        played.cx.simulate_keystrokes("e");
        played.cx.run_until_parked();
        played.cx.simulate_input(" here");
        played.cx.simulate_keystrokes("enter");
        played.cx.run_until_parked();
        assert_eq!(played.chips(), ["look here"]);

        played.cx.simulate_input("score");
        played.shell.read_with(played.cx, |shell, cx| {
            assert_eq!(shell.input.read(cx).content(), "score", "back at the bar");
        });
    }

    #[gpui::test]
    fn chips_hold_their_places_while_one_is_edited(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.connected();
        played.pin("look");
        played.pin("inv");
        played.pin("score");
        played.cx.run_until_parked();

        // Right-click inv; its editor takes the place the others' bounds keep.
        let inv = played.cx.debug_bounds("hotbar-chip-1").unwrap().center();
        let mods = Modifiers::none();
        played.cx.simulate_mouse_down(inv, MouseButton::Right, mods);
        played.cx.simulate_mouse_up(inv, MouseButton::Right, mods);
        played.cx.run_until_parked();

        let from = played.cx.debug_bounds("hotbar-chip-0").unwrap().center();
        let to = played.cx.debug_bounds("hotbar-chip-2").unwrap().center();
        played.cx.simulate_mouse_down(from, MouseButton::Left, mods);
        played.cx.simulate_mouse_move(to, MouseButton::Left, mods);
        played.cx.run_until_parked();
        played.cx.simulate_mouse_up(to, MouseButton::Left, mods);
        played.cx.run_until_parked();
        assert_eq!(played.chips(), ["look", "inv", "score"], "the drag waits");

        // Tab leaves the editor for a chip, whose keys wait too.
        played.cx.simulate_keystrokes("tab");
        played.cx.simulate_keystrokes("shift-right");
        played.cx.simulate_keystrokes("delete");
        played.cx.run_until_parked();
        assert_eq!(played.chips(), ["look", "inv", "score"], "so do the keys");
    }

    #[gpui::test]
    fn esc_leaves_an_edit_without_changing_the_chip(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.connected();
        played.pin("look");
        played.cx.run_until_parked();

        played.cx.simulate_keystrokes(secondary!("4"));
        played.cx.simulate_keystrokes("e");
        played.cx.run_until_parked();
        played.cx.simulate_input(" here");
        played.cx.simulate_keystrokes("escape");
        played.cx.run_until_parked();
        assert_eq!(played.chips(), ["look"]);
    }

    #[gpui::test]
    fn dragging_a_chip_puts_it_where_it_lands(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.connected();
        played.pin("look");
        played.pin("score");
        played.cx.run_until_parked();

        let from = played.cx.debug_bounds("hotbar-chip-0").unwrap().center();
        let to = played.cx.debug_bounds("hotbar-chip-1").unwrap().center();
        let button = MouseButton::Left;
        let mods = Modifiers::none();
        played.cx.simulate_mouse_down(from, button, mods);
        played.cx.simulate_mouse_move(to, button, mods);
        played.cx.run_until_parked();
        played.cx.simulate_mouse_up(to, button, mods);
        played.cx.run_until_parked();
        assert_eq!(played.chips(), ["score", "look"]);
    }

    #[gpui::test]
    fn no_chip_and_no_pin_work_at_a_password_prompt(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.connected();
        played.pin("look");
        played.say(&[IAC, WILL, ECHO]);
        played.frames_until("the mask", |shell, cx| shell.input.read(cx).is_masked());
        played.cx.simulate_input("hunter2");

        played.click("hotbar-chip-0");
        played.cx.simulate_keystrokes(secondary!("p"));
        played.cx.run_until_parked();
        played.shell.read_with(played.cx, |shell, cx| {
            assert_eq!(shell.input.read(cx).content(), "hunter2");
        });
        assert_eq!(played.chips(), ["look"], "the password wasn't pinned");

        played.cx.simulate_keystrokes("enter");
        assert_eq!(played.heard_command(), "hunter2", "the chip went nowhere");
    }

    #[gpui::test]
    fn the_bar_previews_an_alias_as_it_is_typed_but_never_a_password(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.connected();
        played.shell.read_with(played.cx, |shell, _| {
            let alias = AliasRule::new("k", "kill $1;kick $1");
            shell.edit_rules(RuleEdit::SaveAlias(alias));
        });
        played.frames_until("the alias", |shell, _| {
            !shell.copies.rules.value().aliases.is_empty()
        });
        let hint = |played: &mut Played<'_>| {
            played
                .shell
                .read_with(played.cx, |shell, cx| shell.input.read(cx).hint())
        };

        played.cx.simulate_input("k rat");
        played.cx.run_until_parked();
        assert_eq!(hint(&mut played).as_deref(), Some("kill rat; kick rat"));

        played.say(&[IAC, WILL, ECHO]);
        played.frames_until("the mask", |shell, cx| shell.input.read(cx).is_masked());
        played.cx.simulate_input("k rat");
        played.cx.run_until_parked();
        assert_eq!(hint(&mut played), None);
    }
}
