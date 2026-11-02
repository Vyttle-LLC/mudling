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

mod directory;

use crate::assets::UI_FONT;
use crate::character_panel::{CharacterPanel, RefreshCharacter};
use crate::chat::{self, Chat, CloseFind, column_width};
use crate::dock::{self, Dock, DockState, Side, Slot, fit_docks};
use crate::empty_state::{EmptyState, EmptyStateEvent};
use crate::find_panel::{self, FindPanel};
use crate::gear_panel::GearPanel;
use crate::gear_panel::sets::SetAction;
use crate::hotbar::{Hotbar, HotbarEvent};
use crate::hud::{Hud, HudEvent, HudPlacement, ToggleHud};
use crate::input::{self, CommandInput, InputEvent};
use crate::launcher::{Launcher, Login, Target};
use crate::map_history::MapHistoryView;
#[cfg(any(test, target_os = "macos"))]
use crate::map_view::harness::versioned;
use crate::map_view::{MapLayout, MapView, PickRoom, Selection};
use crate::notes::{Host, NoteEdit, Notes, NotesEvent, NotesPanel, RoomCard};
use crate::room_panel::{MarkMob, RoomPanel, SendCommand, Verbs};
use crate::session_bar::{Offers, SessionBar, SessionBarEvent};
#[cfg(target_os = "macos")]
use crate::settings::Section;
use crate::settings::{
    LockAfter, PluginRow, SavedServer, SavedSettings, ServerPlugins, SettingsSheet, SheetEvent,
    SyncChoices, app_settings, hidden_under_sheet, with_app_settings,
};
use crate::skills_panel::{self, SkillsPanel};
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
    AnyView, App, Context, Entity, FocusHandle, Focusable, KeyBinding, SharedString,
    StyleRefinement, Subscription, Task, Window, actions, deferred, div, prelude::*,
};
use mudling_core::completion::Completion;
use mudling_core::defaults::hotbar;
use mudling_core::git::GitError;
#[cfg(target_os = "macos")]
use mudling_core::logs::{LogFile, LogSettings};
use mudling_core::map::FindStores;
#[cfg(any(test, target_os = "macos"))]
use mudling_core::map::RoomId;
use mudling_core::persist::{SavedServers, Timestamp};
use mudling_core::plugin::PluginId;
use mudling_core::repo::AppSettings;
use mudling_core::session::{MapEdit, RuleEdit, Session};
use mudling_core::store::{
    CharactersState, ConnectionStatus, CuesState, InspectorState, ItemsState, MapState, MobsState,
    RoomState, RulesState, SettingsState, SheetState, StoreHub, SyncState, TargetState,
    TerminalChange, Versioned, VitalsState,
};
use mudling_core::sync::{RepoSettings, Syncer, repo_holds_data_folder};
use mudling_core::telnet::Bytes;
use mudling_core::vault::{KeyStore, Vault};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
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
        FocusFindPanel,
        FocusNotes,
        PinCommand,
        FocusNext
    ]
);

/// Above everything else that floats.
const SHEET_PRIORITY: usize = 4;
/// How long ending a session may wait for the disk. A stuck hook or disk can
/// lose what's unsaved, but can't hold up the window or the quit.
pub const QUIT_WAIT: Duration = Duration::from_secs(5);
/// How long leaving may take in all: the session, then sync's last push, which
/// gives up after about 2 seconds.
pub const LEAVE_WAIT: Duration = Duration::from_secs(8);
const STUCK_SESSION: &str = "The last session didn't finish saving in time, so its newest \
                             changes may be lost.";

pub fn bind_keys(cx: &mut App) {
    chat::bind_keys(cx);
    crate::gear_panel::bind_keys(cx);
    crate::hotbar::bind_keys(cx);
    cx.bind_keys([
        KeyBinding::new(secondary!("1"), ToggleLeftPanel, None),
        KeyBinding::new(secondary!("2"), ToggleRightPanel, None),
        KeyBinding::new(secondary!("3"), FocusRoomPanel, None),
        KeyBinding::new(secondary!("4"), FocusHotbar, None),
        KeyBinding::new(secondary!("5"), FocusFindPanel, None),
        KeyBinding::new(secondary!("6"), FocusNotes, None),
        KeyBinding::new(secondary!("p"), PinCommand, None),
        KeyBinding::new(secondary!("l"), FocusCommandBar, None),
        KeyBinding::new("tab", FocusNext, Some(dock::CONTEXT)),
        KeyBinding::new("escape", FocusCommandBar, Some(dock::CONTEXT)),
        KeyBinding::new("tab", FocusNext, Some(find_panel::CONTEXT)),
        // Outranks the field's own Tab for being bound after it.
        KeyBinding::new("tab", FocusNext, Some(&find_panel::field_context())),
        KeyBinding::new("escape", FocusCommandBar, Some(find_panel::CONTEXT)),
        KeyBinding::new("tab", FocusNext, Some(skills_panel::CONTEXT)),
        KeyBinding::new("tab", FocusNext, Some(&skills_panel::field_context())),
        KeyBinding::new("escape", FocusCommandBar, Some(skills_panel::CONTEXT)),
    ]);
    crate::notes::bind_keys(cx);
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
    sheet: Versioned<SheetState>,
    target: Versioned<TargetState>,
    room: Versioned<RoomState>,
    mobs: Versioned<MobsState>,
    rules: Versioned<RulesState>,
    items: Versioned<ItemsState>,
}

impl Copies {
    fn of(stores: Option<&StoreHub>) -> Copies {
        // Sounds and the inspector's lines are only ever read from a batch, so
        // they need no copy.
        if let Some(stores) = stores {
            stores.subscribe::<CuesState>();
            stores.subscribe::<InspectorState>();
        }
        Copies {
            settings: stores.map(|stores| stores.subscribe()).unwrap_or_default(),
            vitals: stores.map(|stores| stores.subscribe()).unwrap_or_default(),
            characters: stores.map(|stores| stores.subscribe()).unwrap_or_default(),
            sheet: stores.map(|stores| stores.subscribe()).unwrap_or_default(),
            target: stores.map(|stores| stores.subscribe()).unwrap_or_default(),
            room: stores.map(|stores| stores.subscribe()).unwrap_or_default(),
            mobs: stores.map(|stores| stores.subscribe()).unwrap_or_default(),
            rules: stores.map(|stores| stores.subscribe()).unwrap_or_default(),
            items: stores.map(|stores| stores.subscribe()).unwrap_or_default(),
        }
    }
}

/// The window's end of sync, which runs whether or not a game does.
struct SyncLink {
    syncer: Syncer,
    copy: Versioned<SyncState>,
    choices: SyncChoices,
    /// The settings in force, which the newest `updated` wins.
    settings: AppSettings,
    key_store: Arc<dyn KeyStore>,
    /// Where the vault in use lives.
    vault_file: PathBuf,
    /// Stop syncing was pressed, and sync hasn't let go of the repo yet. It
    /// can refuse, so the saved choice waits for it.
    clearing: bool,
    /// The sync the saved servers were last listed after: one that finishes
    /// later may have brought servers in.
    listed_after: Option<Timestamp>,
}

/// A server to open once the vault has had its say.
struct WaitingForVault {
    target: Target,
    turn_on: Option<PluginId>,
}

/// What the player can ask for that takes a job off the UI thread.
enum Request {
    Open {
        target: Target,
        turn_on: Option<PluginId>,
        /// Whether to look for a saved login.
        login: bool,
    },
    ChooseServer,
    Save(Target),
    Remove(String),
    /// Read the saved servers again, as a sync may have changed them.
    ListServers,
    /// Read what Settings shows of this server's plugins.
    ServerPlugins(String),
    /// Save the player's switch for a plugin on a server.
    SetPlugin {
        host: String,
        plugin: PluginId,
        on: bool,
    },
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
    /// A server that can't log in until the player unlocks the vault.
    locked_out: Option<WaitingForVault>,
    /// Only the saved servers were read again: no game opened or ended, so
    /// nothing else on show changes.
    only_listing: bool,
    /// A server's plugins, read for Settings.
    server_plugins: Option<ServerPlugins>,
    /// The switch this tried to save, `problem` saying if it couldn't.
    switch: Option<SavedSwitch>,
}

struct SavedSwitch {
    host: SharedString,
    plugin: PluginId,
    on: bool,
}

pub struct Shell {
    game: Option<Game>,
    launcher: Option<Launcher>,
    sync: Option<SyncLink>,
    /// What opens when the vault unlocks, or the player closes the sheet.
    waiting_for_vault: Option<WaitingForVault>,
    /// The one job running off the UI thread: a session ending, another
    /// opening, a server being saved.
    job: Option<Task<()>>,
    /// The directory's index being fetched and checked.
    directory_read: Option<Task<()>>,
    /// A plugin being fetched or installed, or the installed ones read again.
    plugin_work: Option<Task<()>>,
    /// What the player asked for while a job ran, to run in turn.
    waiting: VecDeque<Request>,
    /// What the last job ran into, for a window on a game.
    trouble: Option<String>,
    /// The saved servers as last shown.
    listed: SavedServers,
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
    character_panel: Entity<CharacterPanel>,
    skills_panel: Entity<SkillsPanel>,
    /// What the session last heard about the command field.
    told_of_typing: bool,
    /// A sample fills the Character and Skills tabs, not the stores.
    sheet_held: bool,
    /// The map in the left panel, and the small one in the Room panel.
    map: Entity<MapView>,
    map_inset: Entity<MapView>,
    find_panel: Entity<FindPanel>,
    gear_panel: Entity<GearPanel>,
    map_history: Entity<MapHistoryView>,
    notes: Entity<Notes>,
    notes_panel: Entity<NotesPanel>,
    /// The selected room's note and features, under the map.
    map_card: Entity<RoomCard>,
    /// Follows the map in the left panel, which each session makes anew.
    map_selection: [Subscription; 2],
    settings: Entity<SettingsSheet>,
    settings_open: bool,
    /// A measured run draws its sample in the HUD, whatever the stores say.
    hud_held: bool,
    /// Where focus goes back to when the sheet closes.
    focus_before_settings: Option<FocusHandle>,
    /// Keeps the player's choices, if there's a folder to keep them in.
    saved_settings: Option<SavedSettings>,
    /// Opened with the first sound: most sessions may never ring one.
    speaker: Option<Speaker>,
    _subscriptions: [Subscription; 20],
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
        let edited = cx.observe(&input, |shell, _, cx| {
            shell.show_alias_preview(cx);
            shell.tell_of_typing(cx);
        });
        let hotbar = cx.new(Hotbar::new);
        let chips = cx.subscribe_in(&hotbar, window, |shell, _, event, window, cx| {
            shell.on_hotbar(event, window, cx);
        });
        let verbs = game.as_ref().map(|game| game.verbs.clone());
        let gear_verbs = verbs.clone().unwrap_or_default();
        let room_panel = cx.new(|cx| RoomPanel::new(verbs.unwrap_or_default(), cx));
        let verb_sent = cx.subscribe_in(&room_panel, window, |shell, _, sent, window, cx| {
            shell.on_panel_command(sent, window, cx);
        });
        let gear_panel = cx.new(|cx| GearPanel::new(gear_verbs, cx));
        let gear_sent = cx.subscribe_in(&gear_panel, window, |shell, _, sent, window, cx| {
            shell.on_panel_command(sent, window, cx);
        });
        let gear_set_acted = cx.subscribe_in(&gear_panel, window, Self::on_gear_set);
        let character_panel = cx.new(CharacterPanel::new);
        let refreshed = cx.subscribe_in(&character_panel, window, Self::on_refresh);
        let skills_panel = cx.new(|cx| {
            let verbs = game.as_ref().map(|game| game.verbs.clone());
            SkillsPanel::new(verbs.unwrap_or_default(), cx)
        });
        let practiced = cx.subscribe_in(&skills_panel, window, |shell, _, sent, window, cx| {
            shell.on_panel_command(sent, window, cx);
        });
        let find_stores = stores.map(FindStores::subscribed).unwrap_or_default();
        let find_panel = cx.new(|cx| FindPanel::new(find_stores, cx));
        let walked = cx.subscribe_in(&find_panel, window, |shell, _, sent, window, cx| {
            shell.on_panel_command(sent, window, cx);
        });
        let mob_marked = cx.subscribe(&room_panel, |shell, _, marked: &MarkMob, _| {
            shell.save_note(&NoteEdit::Map(MapEdit::Mark {
                title: marked.title.clone(),
                mark: marked.mark,
                set: marked.set,
            }));
        });
        let notes = cx.new(Notes::new);
        Self::use_notes_of(&notes, stores, cx);
        let note_events = cx.subscribe(&notes, |shell, _, event, cx| shell.on_notes(event, cx));
        let notes_panel = cx.new(|cx| NotesPanel::new(notes.clone(), cx));
        let room_card = cx.new(|cx| RoomCard::new(notes.clone(), Host::RoomPanel, cx));
        room_panel.update(cx, |panel, cx| panel.mount_card(room_card, cx));
        let map_card = cx.new(|cx| RoomCard::new(notes.clone(), Host::Map, cx));
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
                    SheetEvent::Inspect(open) => shell.inspect(*open),
                    SheetEvent::ClearInspector => {
                        shell.inspect(false);
                        shell.inspect(true);
                    }
                    SheetEvent::ServerPlugins(host) => {
                        shell.ask(Request::ServerPlugins(host.clone()), window, cx);
                    }
                    SheetEvent::SetPlugin { host, plugin, on } => {
                        let request = Request::SetPlugin {
                            host: host.to_string(),
                            plugin: plugin.clone(),
                            on: *on,
                        };
                        shell.ask(request, window, cx);
                    }
                    SheetEvent::OpenDirectory => shell.refresh_directory(cx),
                    SheetEvent::Fetch { index, entry } => {
                        shell.fetch_plugin(index.clone(), entry.clone(), window, cx);
                    }
                    SheetEvent::Install { fetched, approve } => {
                        shell.install_plugin(fetched.clone(), *approve, cx);
                    }
                    SheetEvent::SetApproved { plugin, approved } => {
                        shell.set_approved(plugin.clone(), *approved, cx);
                    }
                    SheetEvent::Remove(plugin) => shell.remove_plugin(plugin.clone(), cx),
                    SheetEvent::Edit(edit) => shell.edit_rules(edit.clone()),
                    SheetEvent::TrySound(sound) => shell.play_sound(sound),
                    SheetEvent::LoggingChanged => shell.logging_changed(),
                    SheetEvent::SetRepo(folder) => shell.set_repo(folder, cx),
                    SheetEvent::ClearRepo => shell.clear_repo(cx),
                    SheetEvent::SyncNow => {
                        if let Some(link) = &shell.sync {
                            link.syncer.sync_now();
                        }
                    }
                    SheetEvent::LockAfter(lock_after) => shell.keep_lock_after(*lock_after),
                    SheetEvent::VaultUnlocked => shell.go_on_after_unlock(window, cx),
                },
            );
        let front = cx.observe_window_activation(window, |shell, window, cx| {
            if window.is_window_active() {
                if let Some(link) = &shell.sync {
                    link.syncer.foreground();
                }
            } else {
                shell
                    .settings
                    .update(cx, |sheet, cx| sheet.hide_passwords(cx));
            }
        });
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
        let hud = cx.new(Hud::new);
        let card_moves = cx.subscribe_in(&hud, window, |shell, _, event, window, cx| {
            let HudEvent::CardMoved(at) = event;
            let choice = cx.global::<Theme>().choice;
            shell.apply_choice(
                Appearance {
                    card: Some(*at),
                    ..choice
                },
                window,
                cx,
            );
        });
        let (map, map_inset) = Self::new_maps(stores, cx);
        let map_history = Self::new_history(game.as_ref(), copies.settings.value(), cx);
        let map_selection = Self::follow_selection(&map, cx);
        let mut shell = Shell {
            game,
            launcher: None,
            sync: None,
            waiting_for_vault: None,
            job: None,
            directory_read: None,
            plugin_work: None,
            waiting: VecDeque::new(),
            trouble: None,
            listed: SavedServers::default(),
            switching: false,
            copies,
            left: false,
            chat,
            status: cx.new(|_| StatusLines::default()),
            session_bar,
            empty_state,
            title_bar,
            hud,
            hotbar,
            input,
            left_dock,
            right_dock,
            room_panel,
            character_panel,
            skills_panel,
            told_of_typing: false,
            sheet_held: false,
            map,
            map_inset,
            find_panel,
            gear_panel,
            map_history,
            notes,
            notes_panel,
            map_card,
            map_selection,
            settings,
            settings_open: false,
            hud_held: false,
            focus_before_settings: None,
            saved_settings: None,
            speaker: None,
            _subscriptions: [
                typing,
                edited,
                chips,
                verb_sent,
                refreshed,
                practiced,
                focus_lost,
                appearance,
                quitting,
                menu,
                offers,
                places,
                sheet_events,
                front,
                card_moves,
                walked,
                gear_sent,
                gear_set_acted,
                mob_marked,
                note_events,
            ],
            _woken: Self::apply_stores_when_woken(woken, window, cx),
        };
        shell.show_status(cx);
        shell.show_room(cx);
        shell.show_rules(cx);
        shell.show_gear(cx);
        shell.show_sheet(cx);
        shell.mount(Slot::Room, shell.room_panel.clone(), cx);
        shell.mount(Slot::Character, shell.character_panel.clone(), cx);
        shell.mount(Slot::Skills, shell.skills_panel.clone(), cx);
        shell.mount(Slot::Find, shell.find_panel.clone(), cx);
        shell.mount(Slot::Gear, shell.gear_panel.clone(), cx);
        shell.mount(Slot::Notes, shell.notes_panel.clone(), cx);
        shell.mount_maps(cx);
        shell
    }

    /// Maps that start from the map store as it is now.
    fn new_maps(
        stores: Option<&StoreHub>,
        cx: &mut Context<Shell>,
    ) -> (Entity<MapView>, Entity<MapView>) {
        let map = |layout, cx: &mut Context<Shell>| {
            let store: Versioned<MapState> =
                stores.map(|stores| stores.subscribe()).unwrap_or_default();
            cx.new(|cx| MapView::new(store, layout, cx))
        };
        (map(MapLayout::Panel, cx), map(MapLayout::Inset, cx))
    }

    /// Map history of `game`, or an empty one.
    fn new_history(
        game: Option<&Game>,
        settings: &SettingsState,
        cx: &mut Context<Shell>,
    ) -> Entity<MapHistoryView> {
        let history = game.map(|game| game.session.map_history());
        cx.new(|cx| MapHistoryView::new(history, settings, cx))
    }

    fn mount_maps(&self, cx: &mut Context<Shell>) {
        self.mount(Slot::Map, self.map.clone(), cx);
        let history = self.map_history.clone();
        self.map
            .update(cx, |map, cx| map.mount_below(history.into(), cx));
        let inset = self.map_inset.clone();
        self.room_panel
            .update(cx, |panel, cx| panel.mount_map(inset, cx));
        let card = self.map_card.clone();
        self.map.update(cx, |map, cx| map.mount_card(card, cx));
    }

    /// Keeps the card under the map on the room selected, and hands the
    /// session what the player tells the map about duplicates.
    fn follow_selection(map: &Entity<MapView>, cx: &mut Context<Shell>) -> [Subscription; 2] {
        [
            cx.subscribe(map, |shell, _, selection: &Selection, cx| {
                let room = selection.room;
                shell
                    .map_card
                    .update(cx, |card, cx| card.show(Some(room), cx));
            }),
            cx.subscribe(map, |shell, _, edit: &MapEdit, _| {
                shell.save_note(&NoteEdit::Map(edit.clone()));
            }),
        ]
    }

    /// Starts the maps and Find over on `stores`, which belong to another
    /// session or to none.
    fn use_maps_of(&mut self, stores: Option<&StoreHub>, cx: &mut Context<Shell>) {
        (self.map, self.map_inset) = Self::new_maps(stores, cx);
        self.map_selection = Self::follow_selection(&self.map, cx);
        self.mount_maps(cx);
        let find_stores = stores.map(FindStores::subscribed).unwrap_or_default();
        self.find_panel
            .update(cx, |panel, cx| panel.start_over(find_stores, cx));
        self.map_card.update(cx, |card, cx| card.show(None, cx));
        Self::use_notes_of(&self.notes, stores, cx);
    }

    fn use_notes_of(notes: &Entity<Notes>, stores: Option<&StoreHub>, cx: &mut Context<Shell>) {
        let store: Versioned<MapState> =
            stores.map(|stores| stores.subscribe()).unwrap_or_default();
        notes.update(cx, |notes, cx| notes.use_map(store, cx));
    }

    /// Hands the session a change to the notes, the features or the marks, or
    /// what the player told the map about duplicates.
    fn save_note(&self, edit: &NoteEdit) {
        let Some(game) = &self.game else { return };
        match edit {
            NoteEdit::Map(edit) => game.session.edit_map(edit.clone()),
            NoteEdit::Rules(edit) => game.session.edit(edit.clone()),
        }
    }

    fn on_notes(&mut self, event: &NotesEvent, cx: &mut Context<Shell>) {
        match event {
            NotesEvent::Edit(edit) => self.save_note(edit),
            NotesEvent::Walk(command) => self.send_button_command(command, cx),
            NotesEvent::GoTo(room) => {
                self.dock(Side::Left)
                    .update(cx, |dock, cx| dock.select(Slot::Map, cx));
                self.map.update(cx, |map, cx| map.go_to(*room, cx));
            }
        }
    }

    /// Lets go of the game, with what's typed in the notes saved first. The
    /// editor closes with it: words typed with no session to save them, while
    /// the next one opens, would be lost.
    fn take_game(&mut self, cx: &mut Context<Shell>) -> Option<Game> {
        if let Some(edit) = self.notes.update(cx, |notes, _| notes.unsaved()) {
            self.save_note(&edit);
        }
        self.notes.update(cx, |notes, cx| notes.finish(cx));
        self.game.take()
    }

    /// Lets the window open games. With no game running it shows the empty
    /// state. A replay has no use for this: it never calls it.
    pub fn use_launcher(
        &mut self,
        launcher: Launcher,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) {
        let logs = launcher.logs();
        let refused = launcher.refused().to_vec();
        self.settings.update(cx, |sheet, cx| {
            sheet.use_logs(logs, cx);
            sheet.show_refused(refused, cx);
        });
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

    /// Follows `syncer`, and opens the vault where it says the vault lives.
    /// Call it after [`Shell::restore_settings`], which holds what's kept.
    pub fn use_sync(
        &mut self,
        syncer: Syncer,
        key_store: Arc<dyn KeyStore>,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) {
        let saved = self.saved_settings.as_ref();
        let choices = saved.map(SavedSettings::sync_choices).unwrap_or_default();
        let settings = app_settings(cx.global::<Theme>().choice, choices.settings_updated);
        let copy = syncer.stores().subscribe();
        self.sync = Some(SyncLink {
            syncer,
            copy,
            choices,
            settings,
            key_store,
            vault_file: PathBuf::new(),
            clearing: false,
            listed_after: None,
        });
        self.show_sync(window, cx);
    }

    /// Acts on what sync published. Its waker counts as a wake like a
    /// session's, and comes with no game running too.
    fn apply_sync(&mut self, window: &mut Window, cx: &mut Context<Shell>) {
        let Some(link) = &mut self.sync else { return };
        let batch = link.syncer.stores().drain();
        let Some(diff) = batch.and_then(|batch| batch.sync) else {
            return;
        };
        link.copy.apply_diff(&diff);
        self.show_sync(window, cx);
    }

    fn show_sync(&mut self, window: &mut Window, cx: &mut Context<Shell>) {
        let saved_settings = &self.saved_settings;
        let Some(link) = &mut self.sync else { return };
        let state = link.copy.value();
        if link.clearing && state.repo.is_none() {
            link.clearing = false;
            link.choices.repo = None;
            if let Some(saved) = saved_settings {
                saved.save_sync(&link.choices);
            }
        }
        let moved = !state.vault_file.as_os_str().is_empty() && state.vault_file != link.vault_file;
        if moved {
            link.vault_file.clone_from(&state.vault_file);
            let lock_after = link.choices.lock_after;
            let vault = Vault::open(
                state.vault_file.clone(),
                Arc::clone(&link.key_store),
                lock_after.auto_lock(),
            );
            self.settings.update(cx, |sheet, cx| {
                sheet.use_vault(vault, lock_after, window, cx);
            });
        }
        let Some(link) = &self.sync else { return };
        let state = link.copy.value();
        let repo = link.choices.repo.as_deref();
        self.settings
            .update(cx, |sheet, cx| sheet.show_sync(state, repo, cx));
        let theirs = state.settings.clone();
        let synced = state.last_sync;
        self.adopt_settings(theirs, window, cx);
        self.show_problems(self.copies.settings.value(), cx);
        let Some(link) = &mut self.sync else { return };
        if synced.is_some() && synced != link.listed_after {
            link.listed_after = synced;
            // On the job path the launcher's reads always take: never here.
            self.ask(Request::ListServers, window, cx);
            self.reload_plugins(cx);
        }
    }

    /// Wears settings a merge brought in, if they're newer than what's worn.
    fn adopt_settings(
        &mut self,
        theirs: AppSettings,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) {
        let Some(link) = &mut self.sync else { return };
        if theirs.updated <= link.settings.updated {
            return;
        }
        let choice = with_app_settings(cx.global::<Theme>().choice, &theirs);
        link.choices.settings_updated = theirs.updated;
        link.settings = theirs;
        cx.set_global(Theme::chosen(choice, window.appearance()));
        window.refresh();
        if let Some(saved) = &self.saved_settings {
            saved.save(choice);
            saved.save_sync(&link.choices);
        }
    }

    /// Hands a choice of theme or font to sync, as the newest there is.
    fn tell_sync_of(&mut self, choice: Appearance) {
        let Some(link) = &mut self.sync else { return };
        let updated = Timestamp::now();
        link.settings = AppSettings {
            shortcuts: std::mem::take(&mut link.settings.shortcuts),
            panels: std::mem::take(&mut link.settings.panels),
            ..app_settings(choice, updated)
        };
        link.choices.settings_updated = updated;
        link.syncer.settings_changed(link.settings.clone());
        if let Some(saved) = &self.saved_settings {
            saved.save_sync(&link.choices);
        }
    }

    /// Takes the folder the player chose for sync, unless it is or holds the
    /// data folder: then it says so in Data & sync and changes nothing.
    fn set_repo(&mut self, folder: &Path, cx: &mut Context<Shell>) {
        let holds_data = self
            .launcher
            .as_ref()
            .is_some_and(|launcher| repo_holds_data_folder(folder, launcher.data_folder()));
        let refusal = holds_data.then(|| GitError::HoldsTheDataFolder.to_string());
        self.settings
            .update(cx, |sheet, cx| sheet.repo_refused(refusal, cx));
        if holds_data {
            return;
        }
        let Some(link) = &mut self.sync else { return };
        link.clearing = false;
        link.choices.repo = Some(folder.to_owned());
        link.syncer.set_repo(RepoSettings::new(folder));
        if let Some(saved) = &self.saved_settings {
            saved.save_sync(&link.choices);
        }
    }

    fn clear_repo(&mut self, cx: &mut Context<Shell>) {
        let Some(link) = &mut self.sync else { return };
        link.clearing = true;
        link.syncer.clear_repo();
        // A repo that never opened publishes nothing to wait for.
        if link.copy.value().repo.is_none() {
            link.clearing = false;
            link.choices.repo = None;
            if let Some(saved) = &self.saved_settings {
                saved.save_sync(&link.choices);
            }
            let (state, repo) = (link.copy.value(), None);
            self.settings
                .update(cx, |sheet, cx| sheet.show_sync(state, repo, cx));
        }
    }

    fn keep_lock_after(&mut self, lock_after: LockAfter) {
        let Some(link) = &mut self.sync else { return };
        link.choices.lock_after = lock_after;
        if let Some(saved) = &self.saved_settings {
            saved.save_sync(&link.choices);
        }
    }

    /// The vault opened, so what waited on it goes ahead and logs in.
    fn go_on_after_unlock(&mut self, window: &mut Window, cx: &mut Context<Shell>) {
        if let Some(WaitingForVault { target, turn_on }) = self.waiting_for_vault.take() {
            let login = true;
            self.ask(
                Request::Open {
                    target,
                    turn_on,
                    login,
                },
                window,
                cx,
            );
            self.close_settings(window, cx);
        }
    }

    /// The sheet is gone from the screen, so what it held goes too. A server
    /// still waiting on the vault connects without logging in.
    fn sheet_closed(&mut self, window: &mut Window, cx: &mut Context<Shell>) {
        self.settings.update(cx, |sheet, cx| {
            sheet.leave_page(cx);
            sheet.forget_typed(cx);
            sheet.stop_waiting_for_unlock(cx);
        });
        if let Some(WaitingForVault { target, turn_on }) = self.waiting_for_vault.take() {
            let login = false;
            self.ask(
                Request::Open {
                    target,
                    turn_on,
                    login,
                },
                window,
                cx,
            );
        }
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
        if outcome.only_listing {
            if !self.left {
                self.show_listing(&outcome.saved, cx);
                // A page that arrives after the sheet closed would take the
                // keyboard from the game.
                let sheet_open = self.settings_open;
                self.settings.update(cx, |sheet, cx| {
                    if let Some(plugins) = outcome.server_plugins.filter(|_| sheet_open) {
                        sheet.show_server_plugins(plugins, window, cx);
                    }
                    match (outcome.switch, outcome.problem) {
                        (Some(switch), problem) => {
                            let SavedSwitch { host, plugin, on } = switch;
                            sheet.plugin_saved(&host, &plugin, on, problem, cx);
                        }
                        (None, Some(problem)) => sheet.show_servers_problem(problem, cx),
                        (None, None) => {}
                    }
                });
                self.start_waiting(window, cx);
            }
            return;
        }
        self.switching = false;
        // A game opened as the window left goes unused: dropping it ends it.
        if self.left {
            return;
        }
        self.trouble = outcome.problem.clone();
        self.listed = outcome.saved.clone();
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
        if let Some(waiting) = outcome.locked_out {
            self.settings.update(cx, |sheet, cx| {
                sheet.wait_for_unlock(&waiting.target.host, cx)
            });
            self.waiting_for_vault = Some(waiting);
            self.open_settings(window, cx);
        }
        // A sheet that's open keeps the keyboard. What it goes back to may be
        // gone by the time it closes, as a removed server's card is.
        if self.settings_open {
            self.focus_before_settings = None;
        } else if self.on_empty_state() {
            self.empty_state
                .update(cx, |empty_state, cx| empty_state.focus(window, cx));
        }
        self.start_waiting(window, cx);
    }

    /// Shows what the saved servers are now and nothing else: a problem on
    /// show stays, and so does the keyboard, which a sync running behind the
    /// player's back must not move. A list that's the one on show changes
    /// nothing at all. When it did change, the title bar menu's rows are
    /// replaced, as with any change to the servers, so a keyboard on one of
    /// them loses it.
    fn show_listing(&mut self, saved: &SavedServers, cx: &mut Context<Shell>) {
        if *saved == self.listed {
            return;
        }
        self.listed = saved.clone();
        let trouble = self.trouble.clone();
        self.empty_state.update(cx, |empty_state, cx| {
            empty_state.show(saved, cx);
            if let Some(problem) = trouble.clone() {
                empty_state.show_problem(problem, cx);
            }
        });
        self.settings.update(cx, |sheet, cx| {
            let servers = saved.servers.iter().map(SavedServer::from);
            sheet.show_servers(servers.collect(), trouble, cx);
        });
        self.show_where_things_stand(cx);
    }

    fn start_waiting(&mut self, window: &mut Window, cx: &mut Context<Shell>) {
        while self.job.is_none()
            && let Some(request) = self.waiting.pop_front()
        {
            self.start(request, window, cx);
        }
    }

    fn ask_to_open(&mut self, target: &Target, window: &mut Window, cx: &mut Context<Shell>) {
        let request = Request::Open {
            target: target.clone(),
            turn_on: None,
            login: true,
        };
        self.ask(request, window, cx);
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
            Request::Open {
                target,
                turn_on,
                login,
            } => self.open(target, turn_on, login, window, cx),
            Request::ChooseServer => self.choose_server(window, cx),
            Request::Save(target) => self.add_server(target, window, cx),
            Request::Remove(host) => self.remove_server(host, window, cx),
            Request::ListServers => self.off_the_ui_thread(
                |launcher| Outcome {
                    saved: launcher.saved(),
                    only_listing: true,
                    ..Outcome::default()
                },
                window,
                cx,
            ),
            Request::ServerPlugins(host) => {
                let playing = self.game.as_ref().is_some_and(|g| g.target.host == host);
                let session = playing.then(|| self.copies.settings.value());
                let suggested = session.and_then(|settings| settings.suggestion.clone());
                let running: Vec<PluginId> = (session.iter())
                    .flat_map(|settings| &settings.plugin_costs)
                    .map(|cost| cost.plugin.clone())
                    .collect();
                self.off_the_ui_thread(
                    move |launcher| {
                        let read = launcher.server_plugins(&host, suggested, &running);
                        let (server_plugins, problem) = match read {
                            Ok(plugins) => (Some(plugins), None),
                            Err(problem) => (None, Some(problem)),
                        };
                        Outcome {
                            saved: launcher.saved(),
                            only_listing: true,
                            server_plugins,
                            problem,
                            ..Outcome::default()
                        }
                    },
                    window,
                    cx,
                );
            }
            Request::SetPlugin { host, plugin, on } => self.off_the_ui_thread(
                move |launcher| Outcome {
                    saved: launcher.saved(),
                    only_listing: true,
                    problem: launcher.set_plugin(&host, &plugin, on).err(),
                    switch: Some(SavedSwitch {
                        host: host.into(),
                        plugin,
                        on,
                    }),
                    ..Outcome::default()
                },
                window,
                cx,
            ),
        }
    }

    /// Moves the window onto `game`: a chat and copies of its own, so nothing
    /// of the last session carries over. A real server's connection opens
    /// here, once the stores are subscribed.
    fn play(&mut self, game: Game, window: &mut Window, cx: &mut Context<Shell>) {
        let stores = game.session.stores();
        let terminal = stores.subscribe();
        self.copies = Copies::of(Some(stores));
        self.map_history = Self::new_history(Some(&game), self.copies.settings.value(), cx);
        self.use_maps_of(Some(stores), cx);
        self.chat = cx.new(|_| Chat::new(terminal));
        self.room_panel
            .update(cx, |panel, cx| panel.use_verbs(game.verbs.clone(), cx));
        self.gear_panel
            .update(cx, |panel, cx| panel.use_verbs(game.verbs.clone(), cx));
        self.skills_panel
            .update(cx, |panel, cx| panel.use_verbs(game.verbs.clone(), cx));
        self.settings.update(cx, |sheet, cx| {
            sheet.leave_page(cx);
            sheet.show_plugins(game.plugins.clone(), cx);
        });
        self.show_room(cx);
        self.show_rules(cx);
        self.show_gear(cx);
        self.show_sheet(cx);
        self.told_of_typing = false;
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
    /// of what's already running, such as a double click, does nothing. With
    /// `login`, the vault is asked for the saved login, and a vault only the
    /// player can open sends them to it, with nothing connected yet.
    fn open(
        &mut self,
        target: Target,
        turn_on: Option<PluginId>,
        login: bool,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) {
        let running = self.game.as_ref().is_some_and(|game| {
            game.target == target
                && !game.session.has_stopped()
                && !self.plugins_changed_since_it_closed(cx)
                && turn_on
                    .as_ref()
                    .is_none_or(|plugin| game.enabled.contains(plugin))
        });
        if running || self.launcher.is_none() {
            return;
        }
        let leaving = self.take_game(cx);
        self.switching = leaving.is_some();
        let connecting = SessionLabel::new(target.host.clone(), None, "Connecting…");
        self.title_bar
            .update(cx, |bar, cx| bar.show(connecting, cx));
        let vault = self
            .settings
            .read(cx)
            .vault()
            .filter(|_| login && !target.host.is_empty());
        self.off_the_ui_thread(
            move |launcher| {
                let stuck = leaving.and_then(Game::finish);
                let credentials = match vault.map(|vault| launcher.login(&vault, &target.host)) {
                    Some(Login::Locked) => {
                        return Outcome {
                            saved: launcher.saved(),
                            problem: stuck,
                            locked_out: Some(WaitingForVault { target, turn_on }),
                            ..Outcome::default()
                        };
                    }
                    Some(Login::Ready(credentials)) => credentials,
                    None => None,
                };
                let opened = launcher.open(&target, turn_on.as_ref(), credentials);
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
        let leaving = self.take_game(cx);
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
    /// core thread stopped can't, and one whose plugins the player changed
    /// mustn't, as a session reads them when it opens: those are opened anew.
    /// The Reconnect button goes as the connection starts, so focus moves off
    /// it first.
    fn reconnect(&mut self, window: &mut Window, cx: &mut Context<Shell>) {
        let Some(game) = &self.game else { return };
        if game.session.has_stopped() || self.plugins_changed_since_it_closed(cx) {
            let target = game.target.clone();
            self.ask_to_open(&target, window, cx);
        } else {
            game.session.connect();
            window.focus(&self.input.focus_handle(cx), cx);
        }
    }

    /// The connection is closed, and the next one has to read new plugins.
    fn plugins_changed_since_it_closed(&self, cx: &App) -> bool {
        let connection = &self.copies.settings.value().connection;
        matches!(connection, ConnectionStatus::Closed(_))
            && self.settings.read(cx).reconnect_to_apply()
    }

    fn turn_on(&mut self, plugin: PluginId, window: &mut Window, cx: &mut Context<Shell>) {
        if let Some(game) = &self.game {
            let request = Request::Open {
                target: game.target.clone(),
                turn_on: Some(plugin),
                login: true,
            };
            self.ask(request, window, cx);
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
    fn apply_choice(&mut self, choice: Appearance, window: &mut Window, cx: &mut Context<Shell>) {
        let worn = cx.global::<Theme>().choice;
        if (worn.theme, worn.font) != (choice.theme, choice.font) {
            self.tell_sync_of(choice);
        }
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
        self.sheet_closed(window, cx);
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
        self.apply_sync(window, cx);
        let stores = self.game.as_ref().map(|game| game.session.stores());
        let Some(batch) = stores.and_then(|stores| stores.drain()) else {
            return;
        };
        if let Some(lines) = &batch.terminal {
            self.chat.update(cx, |chat, cx| chat.apply(lines, cx));
            let TerminalChange::Append(lines) = &lines.change;
            self.gear_panel.update(cx, |panel, cx| {
                lines.iter().for_each(|line| panel.hear(line.plain(), cx));
            });
        }
        if let Some(vitals) = &batch.vitals {
            self.copies.vitals.apply_diff(vitals);
        }
        if let Some(settings) = &batch.settings {
            self.copies.settings.apply_diff(settings);
            let saved = self.copies.settings.value();
            self.map_history
                .update(cx, |history, cx| history.show(saved, cx));
            let masked = self.copies.settings.value().echo_off;
            self.input
                .update(cx, |input, cx| input.set_masked(masked, window, cx));
            self.hotbar
                .update(cx, |hotbar, cx| hotbar.set_locked(masked, cx));
            self.find_panel
                .update(cx, |panel, cx| panel.set_locked(masked, cx));
            self.gear_panel
                .update(cx, |panel, cx| panel.set_locked(masked, cx));
            self.skills_panel
                .update(cx, |panel, cx| panel.set_locked(masked, cx));
        }
        if let Some(rules) = &batch.rules {
            self.copies.rules.apply_diff(rules);
            self.show_rules(cx);
        }
        if let Some(inspector) = &batch.inspector {
            self.settings
                .update(cx, |sheet, cx| sheet.show_inspected(&inspector.change, cx));
        }
        if let Some(cues) = batch.cues.as_ref().filter(|_| !self.is_replaying()) {
            cues.change.iter().for_each(|sound| self.play_sound(sound));
        }
        if let Some(characters) = &batch.characters {
            self.copies.characters.apply_diff(characters);
            let character = self.copies.characters.value();
            self.gear_panel
                .update(cx, |panel, cx| panel.show_character(character, cx));
        }
        if let Some(items) = &batch.items {
            self.copies.items.apply_diff(items);
            self.show_gear(cx);
        }
        if let Some(sheet) = &batch.sheet {
            self.copies.sheet.apply_diff(sheet);
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
        if batch.vitals.is_some()
            || batch.settings.is_some()
            || batch.characters.is_some()
            || batch.sheet.is_some()
        {
            self.show_sheet(cx);
        }
        if let Some(room) = &batch.room {
            self.copies.room.apply_diff(room);
        }
        if let Some(mobs) = &batch.mobs {
            self.copies.mobs.apply_diff(mobs);
        }
        if let Some(map) = &batch.map {
            for view in [&self.map, &self.map_inset] {
                view.update(cx, |view, cx| view.apply(map, cx));
            }
            self.notes
                .update(cx, |notes, cx| notes.apply(map, window, cx));
        }
        self.find_panel
            .update(cx, |panel, cx| panel.apply(&batch, cx));
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
        self.notes.update(cx, |notes, cx| {
            notes.show_server(&rules.server, &rules.notepad, cx);
        });
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

    /// Tells the session whether the command field holds text, when that
    /// changes: quiet reads wait while it does.
    fn tell_of_typing(&mut self, cx: &mut Context<Shell>) {
        let typing = !self.input.read(cx).content().is_empty();
        if self.told_of_typing != typing {
            self.told_of_typing = typing;
            if let Some(game) = &self.game {
                game.session.set_typing(typing);
            }
        }
    }

    fn on_refresh(
        &mut self,
        _: &Entity<CharacterPanel>,
        refresh: &RefreshCharacter,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) {
        if let Some(game) = &self.game {
            game.session.refresh_character();
        }
        if !refresh.by_keyboard {
            window.focus(&self.input.focus_handle(cx), cx);
        }
    }

    fn show_sheet(&self, cx: &mut Context<Shell>) {
        if self.sheet_held {
            return;
        }
        let copies = &self.copies;
        self.show_sheet_of(
            copies.sheet.value(),
            copies.characters.value(),
            copies.vitals.value(),
            copies.settings.value(),
            cx,
        );
    }

    /// Fills the Character and Skills tabs.
    pub fn show_sheet_of(
        &self,
        sheet: &SheetState,
        characters: &CharactersState,
        vitals: &VitalsState,
        settings: &SettingsState,
        cx: &mut Context<Shell>,
    ) {
        self.character_panel.update(cx, |panel, cx| {
            panel.show(sheet, characters, vitals, settings, cx);
        });
        self.skills_panel
            .update(cx, |panel, cx| panel.show(sheet, cx));
    }

    fn show_room(&mut self, cx: &mut Context<Shell>) {
        let (room, mobs) = (self.copies.room.value(), self.copies.mobs.value());
        self.room_panel.update(cx, |panel, cx| {
            panel.show_room(room, mobs, cx);
            panel.show_contents(room.contents.clone(), cx);
        });
    }

    fn show_gear(&mut self, cx: &mut Context<Shell>) {
        let items = self.copies.items.value().clone();
        let chat = items.clone();
        self.gear_panel
            .update(cx, |panel, cx| panel.show_items(items, cx));
        self.chat.update(cx, |view, cx| view.show_items(chat, cx));
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
        if !self.hud_held {
            self.hud.update(cx, |hud, cx| {
                let target = self.copies.target.value();
                hud.show(self.copies.vitals.value(), characters, target, cx);
            });
        }
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

    /// With no game, nothing is running: what the settings sheet, the title
    /// bar, the hotbar, the find bar and the command bar said about the last
    /// session goes with it.
    fn forget_the_session(&mut self, cx: &mut Context<Shell>) {
        self.map_history = Self::new_history(None, &SettingsState::default(), cx);
        self.use_maps_of(None, cx);
        self.show_sheet_of(
            &SheetState::default(),
            &CharactersState::default(),
            &VitalsState::default(),
            &SettingsState::default(),
            cx,
        );
        self.gear_panel.update(cx, |panel, cx| {
            panel.show_items(ItemsState::default(), cx);
            panel.show_character(&CharactersState::default(), cx);
        });
        self.show_problems(&SettingsState::default(), cx);
        self.settings.update(cx, |sheet, cx| {
            sheet.show_plugins(Vec::new(), cx);
            sheet.show_rules(&RulesState::default(), cx);
        });
        self.hotbar.update(cx, |hotbar, cx| hotbar.show(&[], cx));
        self.chat.update(cx, |chat, cx| chat.close_find(cx));
        self.title_bar
            .update(cx, |bar, cx| bar.show_find(false, cx));
        self.input
            .update(cx, |input, cx| input.set_alias_preview(None, cx));
    }

    fn show_problems(&self, settings: &SettingsState, cx: &mut Context<Shell>) {
        self.settings
            .update(cx, |sheet, cx| sheet.show_session(settings, cx));
        let session_problems = settings.notices.len() + settings.turned_off.len();
        let problems = session_problems + self.settings.read(cx).sync_problems();
        self.title_bar
            .update(cx, |bar, cx| bar.show_problems(problems, cx));
    }

    fn turn_plugin_on(&self, plugin: &PluginId) {
        if let Some(game) = &self.game {
            game.session.turn_plugin_on(plugin.clone());
        }
    }

    fn inspect(&self, open: bool) {
        if let Some(game) = &self.game {
            game.session.inspect(open);
        }
    }

    fn logging_changed(&self) {
        if let Some(game) = &self.game {
            game.session.logging_changed();
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

    /// A panel's button sent a command. With echo off the server is asking for
    /// a password, and a command sent now would go as one, so it's refused, as
    /// core refuses a hook's.
    fn on_panel_command(
        &mut self,
        sent: &SendCommand,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) {
        self.send_button_command(&sent.command, cx);
        if !sent.by_keyboard {
            window.focus(&self.input.focus_handle(cx), cx);
        }
    }

    /// The Gear panel acted on a gear set. Wearing one sends commands, so with
    /// echo off it's refused, as a panel's command is.
    fn on_gear_set(
        &mut self,
        _: &Entity<GearPanel>,
        action: &SetAction,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) {
        let Some(game) = &self.game else { return };
        match action {
            SetAction::Edit(edit) => game.session.edit_gear(edit.clone()),
            SetAction::Wear { .. } if self.copies.settings.value().echo_off => {}
            SetAction::Wear {
                character,
                id,
                by_keyboard,
            } => {
                game.session.wear_set(character.clone(), id.clone());
                if !by_keyboard {
                    window.focus(&self.input.focus_handle(cx), cx);
                }
            }
        }
    }

    /// Sends a panel's command, unless echo is off.
    fn send_button_command(&mut self, command: &str, cx: &mut Context<Shell>) {
        if !self.copies.settings.value().echo_off {
            self.send_with(
                command,
                |session, command| {
                    session.send_action(command);
                },
                cx,
            );
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

    pub fn focus_command_bar(&self, window: &mut Window, cx: &mut Context<Shell>) {
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

    /// Shows the Find panel, opening the left panel if it's closed, and moves
    /// the keyboard into its field, as [`Shell::focus_room_panel`] does.
    pub fn focus_find_panel(&self, window: &mut Window, cx: &mut Context<Shell>) {
        self.dock(Side::Left)
            .update(cx, |dock, cx| dock.select(Slot::Find, cx));
        cx.spawn_in(window, async move |shell, cx| {
            shell
                .update_in(cx, |shell, window, cx| {
                    shell
                        .find_panel
                        .update(cx, |panel, cx| panel.focus(window, cx));
                })
                .ok();
        })
        .detach();
    }

    /// [`Shell::focus_room_panel`], for the Notes tab.
    fn focus_notes(&self, window: &mut Window, cx: &mut Context<Shell>) {
        self.dock(Side::Right)
            .update(cx, |dock, cx| dock.select(Slot::Notes, cx));
        cx.spawn_in(window, async move |shell, cx| {
            shell
                .update_in(cx, |shell, window, cx| {
                    shell
                        .notes_panel
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

    pub fn settings_sheet(&self) -> Entity<SettingsSheet> {
        self.settings.clone()
    }

    /// Shows the sheet's Logs section with `log` on it, for `--ax-check`.
    #[cfg(target_os = "macos")]
    pub fn show_sample_logs(&self, host: &str, log: LogFile, cx: &mut Context<Shell>) {
        self.settings.update(cx, |sheet, cx| {
            sheet.show_playing(Some(host), cx);
            sheet.select(Section::Logs, cx);
            sheet.show_logs(LogSettings::default(), vec![log], cx);
        });
    }

    #[cfg(test)]
    pub fn show_sample_rules(&self, rules: RulesState, cx: &mut Context<Shell>) {
        self.settings
            .update(cx, |sheet, cx| sheet.show_rules(&rules, cx));
    }

    #[cfg(all(test, target_os = "macos"))]
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
        self.show_sample_hud(vitals, &characters, &TargetState::default(), cx);
    }

    /// Fills the HUD with no session behind it: how a measured run, whose
    /// recording holds no GMCP, draws one.
    pub fn show_sample_hud(
        &self,
        vitals: &VitalsState,
        characters: &CharactersState,
        target: &TargetState,
        cx: &mut App,
    ) {
        self.hud
            .update(cx, |hud, cx| hud.show(vitals, characters, target, cx));
    }

    pub fn hud(&self) -> Entity<Hud> {
        self.hud.clone()
    }

    /// Leaves the Character and Skills tabs to [`Shell::show_sheet_of`] from
    /// now on.
    pub fn hold_sheet(&mut self) {
        self.sheet_held = true;
    }

    /// Leaves the HUD to [`Shell::show_sample_hud`] from now on.
    pub fn hold_hud(&mut self) {
        self.hud_held = true;
    }

    /// Puts the HUD at `placement`, opened or not, as the sheet would.
    pub fn place_hud(
        &mut self,
        placement: HudPlacement,
        open: bool,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) {
        let choice = Appearance {
            hud: placement,
            ..cx.global::<Theme>().choice
        };
        self.apply_choice(choice, window, cx);
        self.hud.update(cx, |hud, cx| hud.set_open(open, cx));
    }

    #[cfg(test)]
    pub fn show_sample_session(&self, session: SessionLabel, cx: &mut Context<Shell>) {
        self.title_bar.update(cx, |bar, cx| bar.show(session, cx));
    }

    #[cfg(all(test, target_os = "macos"))]
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
    #[cfg(all(test, target_os = "macos"))]
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

    pub fn notes(&self) -> Entity<Notes> {
        self.notes.clone()
    }

    /// The left panel's map.
    #[cfg(target_os = "macos")]
    pub fn map(&self) -> Entity<MapView> {
        self.map.clone()
    }

    /// Shows `map` in the panels, with a room selected on it.
    #[cfg(any(test, target_os = "macos"))]
    pub fn show_sample_map(&mut self, map: &MapState, room: RoomId, cx: &mut Context<Shell>) {
        let (panel, inset) = [MapLayout::Panel, MapLayout::Inset]
            .map(|layout| cx.new(|cx| MapView::new(versioned(map.clone()), layout, cx)))
            .into();
        (self.map, self.map_inset) = (panel, inset);
        self.map_selection = Self::follow_selection(&self.map, cx);
        self.mount_maps(cx);
        self.notes
            .update(cx, |notes, cx| notes.use_map(versioned(map.clone()), cx));
        self.map.update(cx, |map, cx| map.go_to(room, cx));
    }

    pub fn room_panel(&self) -> Entity<RoomPanel> {
        self.room_panel.clone()
    }

    pub fn gear_panel(&self) -> Entity<GearPanel> {
        self.gear_panel.clone()
    }

    pub fn find_panel(&self) -> Entity<FindPanel> {
        self.find_panel.clone()
    }

    #[cfg(target_os = "macos")]
    pub fn map_history(&self) -> &Entity<MapHistoryView> {
        &self.map_history
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

    /// Ends the session, then sync, off this thread: sync's last push must
    /// see the session's last rows. The task finishes once everything reached
    /// the disk. `None` if the shell already left.
    pub fn leave(&mut self, cx: &mut Context<Shell>) -> Option<Task<()>> {
        if std::mem::replace(&mut self.left, true) {
            return None;
        }
        let job = self.job.take();
        let game = self.take_game(cx);
        let saved_settings = self.saved_settings.take();
        let syncer = self.sync.as_ref().map(|link| link.syncer.clone());
        Some(cx.background_spawn(async move {
            drop(saved_settings);
            if let Some(job) = job {
                job.await;
            }
            if let Some(problem) = game.and_then(Game::finish) {
                eprintln!("{problem}");
            }
            if let Some(syncer) = syncer {
                syncer.quit().wait();
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
        let reader_listens = window.is_a11y_active();
        // A screen reader reads the lines the chat draws, seen or not.
        let covered = (self.settings_open && !reader_listens)
            .then(|| hidden_under_sheet(window.viewport_size()));
        self.chat.update(cx, |chat, _| chat.set_covered(covered));
        let theme = *cx.global::<Theme>();
        let hud_at = |place: HudPlacement| (theme.choice.hud == place).then(|| self.hud.clone());
        let command_bar = div()
            .debug_selector(|| "command-bar".to_owned())
            .rounded_xl()
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_raised)
            .shadow(theme.floating_shadow())
            .children(hud_at(HudPlacement::Attached))
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
        let on_empty_state = self.on_empty_state();
        let body = if on_empty_state {
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
                .children(hud_at(HudPlacement::Floating))
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
                if std::mem::take(&mut shell.settings_open) {
                    shell.sheet_closed(window, cx);
                }
                shell.focus_before_settings = None;
                shell.focus_command_bar(window, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|shell, _: &OpenSettings, window, cx| {
                shell.open_settings(window, cx);
            }))
            .on_action(cx.listener(|shell, _: &PickRoom, window, cx| {
                shell
                    .map
                    .update(cx, |map, cx| map.pick_selected(window, cx));
            }))
            .on_action(cx.listener(|shell, _: &FocusRoomPanel, window, cx| {
                shell.focus_room_panel(window, cx);
            }))
            .on_action(cx.listener(|shell, _: &FocusNotes, window, cx| {
                shell.focus_notes(window, cx);
            }))
            .on_action(cx.listener(|shell, _: &ToggleHud, window, cx| {
                // The empty state draws no HUD, so there is nothing to open
                // or move, and focus sent to it would be lost.
                if !shell.settings_open && !shell.on_empty_state() {
                    shell.hud.update(cx, |hud, cx| hud.use_shortcut(window, cx));
                }
            }))
            .on_action(cx.listener(|shell, _: &FocusHotbar, window, cx| {
                shell.focus_hotbar(window, cx);
            }))
            .on_action(cx.listener(|shell, _: &FocusFindPanel, window, cx| {
                if !shell.on_empty_state() {
                    shell.focus_find_panel(window, cx);
                }
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
            .children(hud_at(HudPlacement::TopStrip).filter(|_| !on_empty_state))
            .child(body)
            .when(self.settings_open, |shell_view| {
                // The chat draws every frame, and the sheet only when it changes.
                let sheet = if reader_listens {
                    // A cached view leaves its nodes out of the accessibility
                    // tree, so a screen reader gets the sheet drawn afresh.
                    self.settings.clone().into_any_element()
                } else {
                    let over_the_window = StyleRefinement::default().absolute().size_full();
                    self.settings
                        .clone()
                        .cached(over_the_window)
                        .into_any_element()
                };
                shell_view.child(deferred(sheet).with_priority(SHEET_PRIORITY))
            })
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::chat::{COLUMN_WIDTH, LINE_HEIGHT, min_column_width};
    use crate::notes::NoteTarget;
    use crate::room_panel::press_key;
    use crate::settings::Section;
    use crate::theme::ThemeChoice;
    use futures::channel::mpsc::{UnboundedSender, unbounded};
    use gpui::{
        Modifiers, MouseButton, Pixels, ScrollDelta, ScrollEvent, TestAppContext,
        VisualTestContext, point, px, size,
    };
    use mudling_core::defaults::aliases::AliasRule;
    use mudling_core::defaults::hud::Vital;
    use mudling_core::map::MapChange;
    use mudling_core::persist::{self, GearPiece, GearSet, Timestamp};
    use mudling_core::plugin::Plugin;
    use mudling_core::plugin::luau::{DisabledReason, PluginDisabled};
    use mudling_core::session::{GearEdit, SessionConfig};
    use mudling_core::store::{Diff, Revision, TerminalState};
    use mudling_core::sync::SyncConfig;
    use mudling_core::telnet::command::{IAC, SB, SE, WILL, WONT};
    use mudling_core::telnet::option::{ECHO, GMCP};
    use mudling_core::vault::MemoryKeyStore;
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
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
    fn a_held_hud_keeps_its_sample_through_a_store_batch(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.shell.update(played.cx, |shell, cx| {
            let (vitals, characters, target) = crate::measure::sample_hud(0);
            shell.hold_hud();
            shell.show_sample_hud(&vitals, &characters, &target, cx);
        });
        played.gmcp(r#"Char.Vitals {"hp":11,"maxhp":30}"#);
        played.frames_until("the vitals", |shell, _| {
            shell.copies.vitals.value().0.get(&Vital::Hp) == Some(&11)
        });
        let shown = played
            .shell
            .read_with(played.cx, |shell, cx| shell.hud.read(cx).values());
        assert_eq!(shown[0], "3,120/4,300");
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

    /// A window with the sample HUD in it, at `placement`.
    fn hud_window(
        placement: HudPlacement,
        cx: &mut TestAppContext,
    ) -> (Entity<Shell>, &mut VisualTestContext) {
        let (shell, cx) = empty_window(cx);
        cx.update(|_, cx| {
            bind_keys(cx);
            crate::hud::bind_keys(cx);
        });
        shell.update_in(cx, |shell, window, cx| {
            let (vitals, characters, target) = crate::measure::sample_hud(0);
            shell.show_sample_hud(&vitals, &characters, &target, cx);
            shell.place_hud(placement, false, window, cx);
        });
        cx.run_until_parked();
        (shell, cx)
    }

    fn hud_is_open(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> bool {
        shell.read_with(cx, |shell, cx| shell.hud.read(cx).is_open())
    }

    #[gpui::test]
    fn the_arrow_and_the_shortcut_open_the_attached_hud_and_the_chat_moves_up(
        cx: &mut TestAppContext,
    ) {
        let (shell, cx) = hud_window(HudPlacement::Attached, cx);
        let closed = cx.debug_bounds("command-bar").unwrap();
        assert!(cx.debug_bounds("hud-details").is_none());

        let toggle = cx.debug_bounds("hud-toggle").unwrap();
        cx.simulate_click(toggle.center(), Modifiers::none());
        cx.run_until_parked();
        assert!(hud_is_open(&shell, cx));
        let opened = cx.debug_bounds("command-bar").unwrap();
        assert!(opened.size.height > closed.size.height);
        assert_eq!(opened.bottom(), closed.bottom());
        let chat = cx.debug_bounds("chat-area").unwrap();
        assert!(chat.bottom() <= opened.top(), "the bar covers the chat");

        cx.simulate_keystrokes(secondary!("j"));
        cx.run_until_parked();
        assert!(!hud_is_open(&shell, cx));
        assert!(cx.debug_bounds("hud-details").is_none());
    }

    #[gpui::test]
    fn a_game_with_nothing_more_to_show_has_no_arrow_and_no_empty_box(cx: &mut TestAppContext) {
        let (shell, cx) = hud_window(HudPlacement::Attached, cx);
        shell.update(cx, |shell, cx| {
            let vitals = VitalsState(BTreeMap::from([(Vital::Hp, 1)]));
            shell.show_sample_vitals(&vitals, cx);
        });
        cx.simulate_keystrokes(secondary!("j"));
        cx.run_until_parked();
        assert!(cx.debug_bounds("hud-toggle").is_none());
        assert!(cx.debug_bounds("hud-details").is_none());
        assert!(cx.debug_bounds("meter-HP").is_some());
    }

    #[gpui::test]
    fn the_details_show_only_what_the_game_gave(cx: &mut TestAppContext) {
        let (shell, cx) = hud_window(HudPlacement::Attached, cx);
        shell.update(cx, |shell, cx| {
            let vitals = VitalsState(BTreeMap::from([(Vital::Hp, 1), (Vital::ToNextLevel, 9)]));
            shell.show_sample_vitals(&vitals, cx);
            shell.hud.update(cx, |hud, cx| hud.set_open(true, cx));
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("hud-next-level").is_some());
        assert!(cx.debug_bounds("hud-opponent").is_none());
    }

    #[gpui::test]
    fn the_card_drags_stays_inside_the_window_and_is_kept(cx: &mut TestAppContext) {
        let (_shell, cx) = hud_window(HudPlacement::Floating, cx);
        cx.simulate_resize(size(px(1000.), px(700.)));
        cx.run_until_parked();
        let card = cx.debug_bounds("hud-card").unwrap();
        let by = point(px(-300.), px(150.));
        // A drag starts a few pixels on, and takes hold of the card there.
        let taken = card.center() + point(px(-4.), px(0.));
        let to = taken + by;

        cx.simulate_mouse_down(card.center(), MouseButton::Left, Modifiers::none());
        for step in [taken, to] {
            cx.simulate_mouse_move(step, MouseButton::Left, Modifiers::none());
        }
        cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        let moved = cx.debug_bounds("hud-card").unwrap();
        assert_eq!(moved.origin, card.origin + by);
        let area_y = moved.top() - crate::title_bar::HEIGHT;
        let kept = cx.update(|_, cx| cx.global::<Theme>().choice.card);
        assert_eq!(
            kept,
            Some((f32::from(moved.left()) as i32, f32::from(area_y) as i32))
        );

        cx.simulate_resize(size(px(500.), px(420.)));
        cx.run_until_parked();
        let small = cx.debug_bounds("hud-card").unwrap();
        assert!(small.right() <= px(500.) && small.bottom() <= px(420.));
        assert!(small.top() >= crate::title_bar::HEIGHT);

        cx.simulate_resize(size(px(1000.), px(700.)));
        cx.run_until_parked();
        let back = cx.debug_bounds("hud-card").unwrap();
        assert_eq!(
            back.origin, moved.origin,
            "it goes back to where it was left"
        );
    }

    #[gpui::test]
    fn the_card_moves_by_keyboard_and_esc_gives_typing_back(cx: &mut TestAppContext) {
        let (shell, cx) = hud_window(HudPlacement::Floating, cx);
        let card = cx.debug_bounds("hud-card").unwrap();

        cx.simulate_keystrokes(secondary!("j"));
        cx.run_until_parked();
        cx.simulate_keystrokes("left left down");
        cx.run_until_parked();
        let moved = cx.debug_bounds("hud-card").unwrap();
        assert_eq!(moved.origin, card.origin + point(px(-32.), px(16.)));
        let kept = cx.update(|_, cx| cx.global::<Theme>().choice.card);
        assert!(kept.is_some());

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        cx.simulate_input("look");
        assert_eq!(shell.read_with(cx, |shell, cx| shell.typed(cx)), "look");
    }

    #[gpui::test]
    fn a_click_on_the_card_leaves_typing_in_the_command_bar(cx: &mut TestAppContext) {
        let (shell, cx) = hud_window(HudPlacement::Floating, cx);
        let card = cx.debug_bounds("hud-card").unwrap();
        cx.simulate_click(card.center(), Modifiers::none());
        cx.simulate_input("look");
        assert_eq!(shell.read_with(cx, |shell, cx| shell.typed(cx)), "look");
    }

    #[gpui::test]
    fn the_top_strip_is_as_wide_as_the_window_under_the_title_bar(cx: &mut TestAppContext) {
        let (_shell, cx) = hud_window(HudPlacement::TopStrip, cx);
        let window = size(px(1100.), px(700.));
        cx.simulate_resize(window);
        cx.run_until_parked();
        let strip = cx.debug_bounds("hud").unwrap();
        assert_eq!(strip.size.width, window.width);
        assert_eq!(strip.top(), crate::title_bar::HEIGHT);
        assert!(cx.debug_bounds("chat-area").unwrap().top() >= strip.bottom());
    }

    #[gpui::test]
    fn hidden_leaves_no_hud_and_the_chat_shows_prompts(cx: &mut TestAppContext) {
        let (shell, cx) = hud_window(HudPlacement::Hidden, cx);
        assert!(cx.debug_bounds("hud").is_none());
        assert!(cx.update(|_, cx| cx.global::<Theme>().choice.shows_prompts()));
        shell.update_in(cx, |shell, window, cx| {
            shell.place_hud(HudPlacement::Attached, false, window, cx);
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("hud").is_some());
        assert!(!cx.update(|_, cx| cx.global::<Theme>().choice.shows_prompts()));
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
    fn a_room_card_and_map_history_stay_reachable_in_a_short_window(cx: &mut TestAppContext) {
        let (shell, cx) = empty_window(cx);
        cx.simulate_resize(size(px(1400.), px(420.)));
        shell.update(cx, |shell, cx| {
            shell.open_panels(cx);
            shell.show_sample_map(&crate::map_view::harness::noted_map(10), 24, cx);
        });
        cx.run_until_parked();

        let dock = cx.debug_bounds("left-dock").unwrap();
        let map = cx.debug_bounds("map-surface").unwrap();
        let below = cx.debug_bounds("map-below").unwrap();
        assert!(
            map.size.height >= px(120.),
            "the map keeps its share: {map:?}"
        );
        assert!(below.bottom() <= dock.bottom(), "{below:?} in {dock:?}");

        cx.simulate_event(ScrollEvent {
            position: below.center(),
            delta: ScrollDelta::Pixels(point(px(0.), px(-1000.))),
            ..ScrollEvent::default()
        });
        cx.run_until_parked();
        let history = cx.debug_bounds("map-history").unwrap();
        assert_eq!(
            history.bottom(),
            below.bottom(),
            "scrolled to the end, the history's last row is in view"
        );
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
    fn the_session_hears_when_the_command_field_fills_and_empties(cx: &mut TestAppContext) {
        let (_waker, woken) = unbounded();
        let (shell, cx) = cx.add_window_view(|window, cx| Shell::new(None, woken, window, cx));
        cx.run_until_parked();
        shell.update_in(cx, |shell, window, cx| {
            shell.input.update(cx, |input, cx| {
                input.show("lo".to_owned(), 2, window, cx);
            });
        });
        cx.run_until_parked();
        shell.read_with(cx, |shell, _| assert!(shell.told_of_typing));
        shell.update_in(cx, |shell, window, cx| {
            shell.input.update(cx, |input, cx| {
                input.show(String::new(), 0, window, cx);
            });
        });
        cx.run_until_parked();
        shell.read_with(cx, |shell, _| assert!(!shell.told_of_typing));
    }

    #[gpui::test]
    fn a_note_typed_in_the_room_panel_outlives_the_walk_and_never_reaches_the_server(
        cx: &mut TestAppContext,
    ) {
        let mut played = play(cx);
        played.enter_room();
        let notes = played.shell.read_with(played.cx, |shell, _| shell.notes());
        notes.update_in(played.cx, |notes, window, cx| {
            notes.edit(NoteTarget::Room(1), Host::RoomPanel, window, cx);
        });
        played.cx.run_until_parked();
        played.cx.simulate_input("quit");

        played.gmcp(r#"Room.Info {"num": 2, "name": "A yard", "exits": {"s": 1}}"#);
        played.frames_until("the next room", |shell, cx| {
            shell.room_panel.read(cx).room_name() == Some("A yard")
        });
        played.cx.executor().advance_clock(Duration::from_secs(1));
        played.frames_until("the note", |shell, cx| {
            !shell.notes.read(cx).rows().is_empty()
        });

        let rows = notes.read_with(played.cx, |notes, _| notes.rows().to_vec());
        assert_eq!((rows[0].room, rows[0].note.as_str()), (1, "quit"));
        played.shell.update(played.cx, |shell, cx| {
            shell.submit("look", cx);
        });
        assert_eq!(played.heard_command(), "look");
    }

    /// Takes the room out of the notes' copy of the map, as a map that folds
    /// a room away does.
    fn remove_room_under_the_note(played: &mut Played<'_>, notes: &Entity<Notes>) {
        let diff = Diff {
            revision: Revision(1_000),
            change: MapChange {
                removed: vec![1],
                ..MapChange::default()
            },
        };
        notes.update_in(played.cx, |notes, window, cx| {
            notes.apply(&diff, window, cx);
        });
        played.cx.run_until_parked();
    }

    fn typing_stays_in_a_note_when_its_room_leaves_the_map(host: Host, cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.enter_room();
        let notes = played.shell.read_with(played.cx, |shell, _| shell.notes());
        played.shell.update(played.cx, |shell, cx| {
            shell.map.update(cx, |map, cx| map.go_to(1, cx));
        });
        played.cx.run_until_parked();
        notes.update_in(played.cx, |notes, window, cx| {
            notes.edit(NoteTarget::Room(1), host, window, cx);
        });
        played.cx.run_until_parked();
        played.cx.simulate_input("a door");

        remove_room_under_the_note(&mut played, &notes);

        played.cx.simulate_input(" here");
        played.cx.simulate_keystrokes("enter");
        played.cx.simulate_input("quit");
        played.cx.run_until_parked();
        assert!(played.cx.debug_bounds("Done").is_some(), "still editing");
        played.shell.read_with(played.cx, |shell, cx| {
            assert_eq!(
                shell.input.read(cx).content(),
                "",
                "nothing went to the bar"
            );
            let typed = shell.notes.read(cx).editor().read(cx).content().to_owned();
            assert!(typed.ends_with(": a door here\nquit"), "{typed:?}");
        });
    }

    #[gpui::test]
    fn typing_stays_in_a_room_panel_note_when_its_room_leaves_the_map(cx: &mut TestAppContext) {
        typing_stays_in_a_note_when_its_room_leaves_the_map(Host::RoomPanel, cx);
    }

    #[gpui::test]
    fn typing_stays_in_a_map_note_when_its_room_leaves_the_map(cx: &mut TestAppContext) {
        typing_stays_in_a_note_when_its_room_leaves_the_map(Host::Map, cx);
    }

    fn a_click_on_the_editor_takes_typing_back_from_the_bar(host: Host, cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.enter_room();
        let notes = played.shell.read_with(played.cx, |shell, _| shell.notes());
        played.shell.update(played.cx, |shell, cx| {
            shell.map.update(cx, |map, cx| map.go_to(1, cx));
            if host == Host::NotesTab {
                shell
                    .dock(Side::Right)
                    .update(cx, |dock, cx| dock.select(Slot::Notes, cx));
            }
        });
        played.cx.run_until_parked();
        notes.update_in(played.cx, |notes, window, cx| {
            notes.edit(NoteTarget::Room(1), host, window, cx);
        });
        played.cx.run_until_parked();
        played.cx.simulate_input("one");
        played.shell.update_in(played.cx, |shell, window, cx| {
            window.focus(&shell.input.focus_handle(cx), cx);
        });
        played.cx.run_until_parked();

        played.click("Note");
        played.cx.run_until_parked();
        played.cx.simulate_input(" two");
        played.cx.simulate_keystrokes("enter");

        played.shell.read_with(played.cx, |shell, cx| {
            assert_eq!(
                shell.input.read(cx).content(),
                "",
                "nothing went to the bar"
            );
            let typed = shell.notes.read(cx).editor().read(cx).content().to_owned();
            assert_eq!(typed, "one two\n");
        });
    }

    #[gpui::test]
    fn a_click_on_the_editor_in_the_room_panel_takes_typing_back(cx: &mut TestAppContext) {
        a_click_on_the_editor_takes_typing_back_from_the_bar(Host::RoomPanel, cx);
    }

    #[gpui::test]
    fn a_click_on_the_editor_in_the_notes_tab_takes_typing_back(cx: &mut TestAppContext) {
        a_click_on_the_editor_takes_typing_back_from_the_bar(Host::NotesTab, cx);
    }

    #[gpui::test]
    fn a_click_on_the_editor_on_the_map_card_takes_typing_back(cx: &mut TestAppContext) {
        a_click_on_the_editor_takes_typing_back_from_the_bar(Host::Map, cx);
    }

    #[gpui::test]
    fn leaving_a_game_saves_an_open_note_and_closes_it(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.enter_room();
        let notes = played.shell.read_with(played.cx, |shell, _| shell.notes());
        notes.update_in(played.cx, |notes, window, cx| {
            notes.edit(NoteTarget::Room(1), Host::RoomPanel, window, cx);
        });
        played.cx.run_until_parked();
        played.cx.simulate_input("herbs");

        let game = played
            .shell
            .update(played.cx, |shell, cx| shell.take_game(cx));
        let game = game.expect("a game was running");

        let open = notes.read_with(played.cx, |notes, _| notes.draft_target());
        assert_eq!(
            open, None,
            "nothing can be typed with no session to save it"
        );
        let saved = || {
            game.session.stores().read(|map: &Versioned<MapState>| {
                map.value()
                    .room(1)
                    .is_some_and(|room| room.notes == "herbs")
            })
        };
        let started = Instant::now();
        while !saved() {
            assert!(started.elapsed() < WAIT, "timed out waiting for the note");
            sleep(Duration::from_millis(1));
        }
    }

    #[gpui::test]
    fn clicking_a_tab_or_the_cards_buttons_leaves_typing_with_the_bar(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.enter_room();
        played.shell.update(played.cx, |shell, cx| {
            shell.map.update(cx, |map, cx| map.go_to(1, cx));
        });
        played.cx.run_until_parked();

        played.click("tab-Notes");
        played.click("Walk to A hall");
        played.click("Set Shop");
        played.cx.run_until_parked();

        played.cx.simulate_input("say hi there");
        played.shell.read_with(played.cx, |shell, cx| {
            assert_eq!(shell.input.read(cx).content(), "say hi there");
        });
    }

    #[gpui::test]
    fn the_notes_shortcut_opens_the_tab_and_takes_the_keyboard_into_it(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.enter_room();
        played.shell.update_in(played.cx, |shell, window, cx| {
            shell.focus_notes(window, cx);
        });
        played.cx.run_until_parked();
        assert!(played.cx.debug_bounds("notes-panel").is_some());
        assert!(played.cx.debug_bounds("Add a note here").is_some());
        let in_the_bar = played.shell.update_in(played.cx, |shell, window, cx| {
            shell.input.focus_handle(cx).is_focused(window)
        });
        assert!(!in_the_bar);
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

    /// Walks north from the hall to a gate, then brings up Find and looks for
    /// the hall.
    fn find_the_hall(played: &mut Played) {
        played.enter_room();
        played.cx.simulate_input("north");
        played.cx.simulate_keystrokes("enter");
        assert_eq!(played.heard_command(), "north");
        played.gmcp(r#"Room.Info {"num": 2, "name": "A gate", "exits": {"s": 1}}"#);
        played.frames_until("the gate", |shell, cx| {
            shell.room_panel.read(cx).room_name() == Some("A gate")
        });
        played.cx.simulate_keystrokes(secondary!("5"));
        played.cx.run_until_parked();
        played.cx.simulate_input("hall");
        played.cx.run_until_parked();
    }

    fn click_walk_to_the_hall(played: &mut Played) {
        let walk = played.cx.debug_bounds("Walk to A hall").unwrap().center();
        played.cx.simulate_click(walk, Modifiers::none());
        played.cx.run_until_parked();
    }

    #[gpui::test]
    fn find_opens_by_its_shortcut_and_its_walk_reaches_the_server(cx: &mut TestAppContext) {
        let mut played = play(cx);
        find_the_hall(&mut played);
        played.shell.read_with(played.cx, |shell, cx| {
            assert_eq!(shell.dock(Side::Left).read(cx).state(), DockState::Open);
            assert_eq!(shell.find_panel.read(cx).rows_for_typed(cx), Some(1));
            assert_eq!(shell.typed(cx), "", "the field took the typing");
        });

        click_walk_to_the_hall(&mut played);
        assert_eq!(played.heard_command(), "south");
    }

    #[gpui::test]
    fn a_walk_is_refused_while_echo_is_off_even_once_under_way(cx: &mut TestAppContext) {
        let mut played = play(cx);
        find_the_hall(&mut played);
        played.say(&[IAC, WILL, ECHO]);
        played.frames_until("echo off", |shell, _| {
            shell.copies.settings.value().echo_off
        });
        click_walk_to_the_hall(&mut played);
        played.shell.update_in(played.cx, |shell, window, cx| {
            let sent = SendCommand {
                command: "1s".to_owned(),
                by_keyboard: false,
            };
            shell.on_panel_command(&sent, window, cx);
        });

        played.say(&[IAC, WONT, ECHO]);
        played.frames_until("echo on", |shell, _| {
            !shell.copies.settings.value().echo_off
        });
        played.shell.update(played.cx, |shell, cx| {
            shell.submit("look", cx);
        });
        assert_eq!(played.heard_command(), "look", "nothing went before it");
    }

    /// The session refuses too while it knows echo is off, so what's the
    /// shell's alone is the moment its copy says off and the session's says on:
    /// the server has given echo back, and no frame has told the shell.
    #[gpui::test]
    fn wearing_a_set_is_refused_while_the_shell_sees_echo_off(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.connected();
        played.gmcp(r#"Char.Status {"character_name": "Bren"}"#);
        played.gmcp(r#"Char.Items.List {"location": "inv", "items": [{"id": "7", "name": "a linen robe"}]}"#);
        played.frames_until("the robe", |shell, _| {
            !shell.copies.items.value().carried.is_empty()
        });
        played.shell.update(played.cx, |shell, _| {
            let game = shell.game.as_ref().expect("a game is on");
            game.session.edit_gear(GearEdit::SaveSet(GearSet {
                id: String::new(),
                character_id: "bren".to_owned(),
                name: "Robe".to_owned(),
                container: None,
                pieces: vec![GearPiece {
                    item: "a linen robe".to_owned(),
                    slot: None,
                    copy_id: None,
                }],
                updated: Timestamp(0),
            }));
        });
        played.frames_until("the set", |shell, _| {
            !shell.copies.items.value().sets.is_empty()
        });
        let (panel, wear) = played.shell.read_with(played.cx, |shell, _| {
            let id = shell.copies.items.value().sets[0].id.clone();
            let wear = SetAction::Wear {
                character: "bren".to_owned(),
                id,
                by_keyboard: true,
            };
            (shell.gear_panel.clone(), wear)
        });

        played.say(&[IAC, WILL, ECHO]);
        played.frames_until("echo off", |shell, _| {
            shell.copies.settings.value().echo_off
        });
        played.say(&[IAC, WONT, ECHO]);
        let started = Instant::now();
        while played.shell.read_with(played.cx, |shell, _| {
            let game = shell.game.as_ref().expect("a game is on");
            let stores = game.session.stores();
            stores.read(|settings: &Versioned<SettingsState>| settings.value().echo_off)
        }) {
            assert!(
                started.elapsed() < WAIT,
                "timed out waiting for the session"
            );
            sleep(Duration::from_millis(1));
        }
        panel.update(played.cx, |_, cx| cx.emit(wear.clone()));
        played.shell.read_with(played.cx, |shell, _| {
            assert!(shell.copies.settings.value().echo_off, "no frame went by");
        });

        played.frames_until("echo on", |shell, _| {
            !shell.copies.settings.value().echo_off
        });
        played.shell.update(played.cx, |shell, cx| {
            shell.submit("look", cx);
        });
        assert_eq!(played.heard_command(), "look", "nothing went before it");

        panel.update(played.cx, |_, cx| cx.emit(wear));
        assert_eq!(
            played.heard_command(),
            "wear 7",
            "the set could go all along"
        );
    }

    #[gpui::test]
    fn practice_is_refused_while_echo_is_off(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.connected();
        played.say(&[IAC, WILL, ECHO]);
        played.frames_until("echo off", |shell, _| {
            shell.copies.settings.value().echo_off
        });
        let panel = played
            .shell
            .read_with(played.cx, |shell, _| shell.skills_panel.clone());
        panel.update(played.cx, |_, cx| {
            cx.emit(SendCommand {
                command: "practice dodge".to_owned(),
                by_keyboard: true,
            });
        });
        played.cx.run_until_parked();

        played.say(&[IAC, WONT, ECHO]);
        played.frames_until("echo on", |shell, _| {
            !shell.copies.settings.value().echo_off
        });
        played.shell.update(played.cx, |shell, cx| {
            shell.submit("look", cx);
        });
        assert_eq!(played.heard_command(), "look", "nothing went before it");
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
    fn the_hud_shortcut_leaves_the_empty_states_keyboard_alone(cx: &mut TestAppContext) {
        let (server, port) = fake_server();
        let mut launched = launch(&[("127.0.0.1", port)], cx);
        launched.cx.update(|_, cx| crate::hud::bind_keys(cx));
        launched.shell.update_in(launched.cx, |shell, window, cx| {
            let (vitals, characters, target) = crate::measure::sample_hud(0);
            shell.show_sample_hud(&vitals, &characters, &target, cx);
            shell.place_hud(HudPlacement::Floating, false, window, cx);
        });
        assert!(launched.is_drawn("connect-0"));
        assert!(!launched.is_drawn("hud-card"), "no card on the empty state");

        launched.cx.simulate_keystrokes(secondary!("j"));
        press_enter(launched.cx);
        // The connection is made without an accept, so a dead keyboard times
        // out here instead of hanging on the listener.
        launched.wait_for_connection(ConnectionStatus::Connected);
        drop(server);
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
    fn a_sync_relisting_the_servers_keeps_the_trouble_and_where_the_keyboard_goes_back_to(
        cx: &mut TestAppContext,
    ) {
        let played = play(cx);
        played.shell.update_in(played.cx, |shell, window, cx| {
            let problem = Some(STUCK_SESSION.to_owned());
            shell.job_done(
                Outcome {
                    problem,
                    ..Outcome::default()
                },
                window,
                cx,
            );
            shell.open_settings(window, cx);
        });
        let kept = played
            .shell
            .read_with(played.cx, |shell, _| shell.focus_before_settings.is_some());
        assert!(kept, "the sheet has somewhere to give the keyboard back");

        played.shell.update_in(played.cx, |shell, window, cx| {
            let listing = Outcome {
                only_listing: true,
                ..Outcome::default()
            };
            shell.job_done(listing, window, cx);
        });

        played.shell.read_with(played.cx, |shell, cx| {
            assert_eq!(shell.status.read(cx).lines(), [STUCK_SESSION]);
            assert_eq!(shell.trouble.as_deref(), Some(STUCK_SESSION));
            assert!(shell.focus_before_settings.is_some(), "still remembered");
        });
    }

    fn relisting(folder: &Path) -> Outcome {
        Outcome {
            saved: persist::saved_servers(folder),
            only_listing: true,
            ..Outcome::default()
        }
    }

    #[gpui::test]
    fn a_relisting_of_the_same_servers_leaves_the_title_bar_menus_rows_alone(
        cx: &mut TestAppContext,
    ) {
        let mut launched = launch(&[("one.example", 1), ("two.example", 2)], cx);
        launched.frames_until("the cards", |shell, cx| {
            shell.empty_state.read(cx).cards().len() == 2
        });
        let rows = |launched: &Launched<'_>| {
            let bar = launched
                .shell
                .read_with(launched.cx, |shell, _| shell.title_bar.clone());
            bar.read_with(launched.cx, |bar, _| bar.saved_row_handles())
        };
        let before = rows(&launched);
        assert_eq!(before.len(), 2);

        let same = relisting(launched.folder.path());
        launched.shell.update_in(launched.cx, |shell, window, cx| {
            shell.job_done(same, window, cx);
        });
        assert_eq!(rows(&launched), before, "the rows kept their handles");

        persist::save_server(launched.folder.path(), "three.example", 3).unwrap();
        let more = relisting(launched.folder.path());
        launched.shell.update_in(launched.cx, |shell, window, cx| {
            shell.job_done(more, window, cx);
        });
        assert_eq!(rows(&launched).len(), 3, "a change still shows");
    }

    #[gpui::test]
    fn nothing_waiting_starts_behind_a_relisting_once_the_window_has_left(cx: &mut TestAppContext) {
        let launched = launch(&[], cx);
        launched.shell.update_in(launched.cx, |shell, window, cx| {
            shell.left = true;
            shell
                .waiting
                .push_back(Request::Save(target("late.example", 9)));
            let listing = Outcome {
                only_listing: true,
                ..Outcome::default()
            };
            shell.job_done(listing, window, cx);
            assert_eq!(shell.waiting.len(), 1, "it stayed queued");
            assert!(shell.job.is_none(), "nothing started");
        });
        assert!(saved_hosts(launched.folder.path()).is_empty());
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
    fn a_switch_on_the_server_in_play_is_saved_for_the_next_connect(cx: &mut TestAppContext) {
        let (server, port) = fake_server();
        let mut launched = launch(&[("127.0.0.1", port)], cx);
        press_enter(launched.cx);
        let socket = server.accept().unwrap();
        launched.wait_for_connection(ConnectionStatus::Connected);
        open_servers(&mut launched);
        launched.click("server-plugins-0");
        launched.frames_until("the server's plugins", |shell, cx| {
            shell.settings.read(cx).on_a_page()
        });
        launched.cx.run_until_parked();
        assert!(launched.is_drawn("suggested-plugin"));

        launched.click("suggested-turn-on");
        launched.frames_until("the switch to be saved", |shell, _| shell.job.is_none());
        launched.cx.run_until_parked();
        let saved = persist::enabled_plugins(launched.folder.path(), "127.0.0.1").unwrap();
        assert_eq!(saved, ["loopback"]);
        assert!(
            !launched.is_drawn("suggested-plugin"),
            "the suggestion goes"
        );
        assert!(launched.is_drawn("reconnect-note"));

        let enabled = |launched: &mut Launched<'_>| {
            launched.shell.read_with(launched.cx, |shell, _| {
                shell.game.as_ref().map_or(0, |game| game.enabled.len())
            })
        };
        assert_eq!(
            enabled(&mut launched),
            0,
            "the running session reads its plugins once"
        );

        drop(socket);
        launched.frames_until("the connection to end", |shell, _| {
            let connection = &shell.copies.settings.value().connection;
            matches!(connection, ConnectionStatus::Closed(_))
        });
        launched.shell.update_in(launched.cx, |shell, window, cx| {
            shell.reconnect(window, cx);
        });
        launched.cx.run_until_parked();
        let _next = server.accept().unwrap();
        launched.wait_for_connection(ConnectionStatus::Connected);
        assert_eq!(enabled(&mut launched), 1, "Reconnect brings it in");
        assert!(!launched.is_drawn("reconnect-note"));
    }

    #[gpui::test]
    fn a_switch_that_could_not_be_saved_goes_back_and_says_so_until_one_is(
        cx: &mut TestAppContext,
    ) {
        let (mut launched, _socket) = launch_playing(game_plugin(), cx);
        launched.shell.update_in(launched.cx, |shell, window, cx| {
            shell.open_settings(window, cx);
            let sheet = &shell.settings;
            sheet.update(cx, |sheet, cx| sheet.select(Section::Plugins, cx));
        });
        launched.cx.run_until_parked();
        let game_row_on = |launched: &mut Launched<'_>| {
            launched.shell.read_with(launched.cx, |shell, cx| {
                let rows = shell.settings.read(cx).plugins();
                rows.iter()
                    .find(|row| row.id.as_str() == "game")
                    .unwrap()
                    .on
            })
        };
        assert!(game_row_on(&mut launched));

        launched.click("switch-game");
        launched.frames_until("the save", |shell, _| shell.job.is_none());
        assert!(!game_row_on(&mut launched));
        launched.shell.update_in(launched.cx, |shell, window, cx| {
            let failed = Outcome {
                saved: shell.listed.clone(),
                only_listing: true,
                problem: Some("couldn't save 127.0.0.1: the disk is full".to_owned()),
                switch: Some(SavedSwitch {
                    host: "127.0.0.1".into(),
                    plugin: PluginId::try_from("game".to_owned()).unwrap(),
                    on: false,
                }),
                ..Outcome::default()
            };
            shell.job_done(failed, window, cx);
        });
        launched.cx.run_until_parked();
        assert!(game_row_on(&mut launched), "the switch goes back");
        assert!(launched.is_drawn("switch-problem"));
        assert!(
            !launched.is_drawn("reconnect-note"),
            "nothing is waiting to apply"
        );

        launched.click("switch-game");
        launched.frames_until("the save", |shell, _| shell.job.is_none());
        launched.cx.run_until_parked();
        assert!(
            !launched.is_drawn("switch-problem"),
            "a good save clears it"
        );
        assert!(launched.is_drawn("reconnect-note"));
    }

    #[gpui::test]
    fn an_override_for_another_server_has_no_switch_in_plugins(cx: &mut TestAppContext) {
        let mut launched = launch(&[("mud.example", 4000)], cx);
        let manifest = "id = \"mine\"\nname = \"Mine\"\nversion = \"1.0.0\"\napi = 1\n\
                        license = \"MIT\"\nlayer = \"override\"\n[servers]\n\
                        hosts = [\"b.example\"]\n";
        let mine = Plugin::from_files(|file| match file {
            "plugin.toml" => Ok(manifest.to_owned()),
            _ => Err(std::io::Error::other(file.to_owned())),
        })
        .unwrap();
        launched.shell.update_in(launched.cx, |shell, window, cx| {
            shell.open_settings(window, cx);
            let rows = PluginRow::for_session(&[mine], &[], &[], "a.example");
            shell.settings.update(cx, |sheet, cx| {
                sheet.show_plugins(rows, cx);
                sheet.select(Section::Plugins, cx);
            });
        });
        launched.cx.run_until_parked();
        assert!(launched.is_drawn("plugin-mine"));
        assert!(!launched.is_drawn("switch-mine"));
    }

    #[gpui::test]
    fn tab_reaches_a_servers_plugins_before_remove(cx: &mut TestAppContext) {
        let mut launched = launch(&[("mud.example", 4000)], cx);
        launched.cx.update(|_, cx| crate::settings::bind_keys(cx));
        open_servers(&mut launched);
        launched.shell.update_in(launched.cx, |shell, window, cx| {
            let close = shell.settings.read(cx).close_stop();
            window.focus(&close, cx);
        });
        launched.cx.simulate_keystrokes("tab");
        press_enter(launched.cx);
        launched.frames_until("the server's plugins", |shell, cx| {
            let sheet = shell.settings.read(cx);
            sheet.on_a_page() || sheet.is_asking_to_remove()
        });
        launched.shell.read_with(launched.cx, |shell, cx| {
            assert!(shell.settings.read(cx).on_a_page());
        });
    }

    #[gpui::test]
    fn a_servers_plugins_arriving_after_the_sheet_closed_leave_the_keyboard_alone(
        cx: &mut TestAppContext,
    ) {
        let mut launched = launch(&[("mud.example", 4000)], cx);
        launched.frames_until("the servers to load", |shell, _| shell.job.is_none());
        let after_closing = launched.shell.update_in(launched.cx, |shell, window, cx| {
            shell.open_settings(window, cx);
            shell
                .settings
                .update(cx, |sheet, cx| sheet.select(Section::Servers, cx));
            shell.ask(Request::ServerPlugins("mud.example".to_owned()), window, cx);
            shell.close_settings(window, cx);
            window.focused(cx)
        });
        assert!(after_closing.is_some());
        launched.frames_until("the server's plugins", |shell, _| shell.job.is_none());

        launched.shell.update_in(launched.cx, |shell, window, cx| {
            assert!(!shell.settings.read(cx).on_a_page());
            assert_eq!(window.focused(cx), after_closing);
        });
    }

    #[gpui::test]
    fn a_plugin_the_core_turned_off_here_is_a_plain_switch_on_another_server(
        cx: &mut TestAppContext,
    ) {
        let mut launched = launch(&[("other.example", 4000)], cx);
        let folder = launched.folder.path().to_owned();
        let plugin = loopback_plugin().id().clone();
        launched.shell.update_in(launched.cx, |shell, window, cx| {
            shell.open_settings(window, cx);
            let page = shell.launcher.as_ref().unwrap();
            let page = page.server_plugins("other.example", None, &[]).unwrap();
            shell.show_problems(
                &SettingsState {
                    server: Some("127.0.0.1".to_owned()),
                    turned_off: vec![PluginDisabled {
                        plugin: plugin.clone(),
                        reason: DisabledReason::Overran(3),
                    }],
                    ..SettingsState::default()
                },
                cx,
            );
            shell.settings.update(cx, |sheet, cx| {
                sheet.select(Section::Servers, cx);
                sheet.show_server_plugins(page, window, cx);
            });
        });
        launched.cx.run_until_parked();

        launched.click("switch-loopback");
        launched.frames_until("the switch to be saved", |shell, _| shell.job.is_none());
        let saved = persist::enabled_plugins(&folder, "other.example").unwrap();
        assert_eq!(saved, ["loopback"]);
    }

    #[gpui::test]
    fn the_inspector_records_only_while_its_page_is_open(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.connected();
        let recorded = |played: &mut Played<'_>| {
            played.shell.read_with(played.cx, |shell, _| {
                let stores = shell.game.as_ref().unwrap().session.stores();
                stores.read(|state: &Versioned<InspectorState>| state.value().lines.len())
            })
        };
        let open = |played: &mut Played<'_>| {
            played.shell.update_in(played.cx, |shell, window, cx| {
                shell.open_settings(window, cx);
                shell.settings.update(cx, |sheet, cx| {
                    sheet.show_plugins(crate::measure::sample_plugins(), cx);
                    sheet.select(Section::Plugins, cx);
                });
            });
            played.cx.run_until_parked();
            played.click("open-inspector");
            played.say(b"seen\r\n");
            played.frames_until("a line in the inspector", |shell, cx| {
                shell.settings.read(cx).inspected_lines() > 0
            });
        };

        played.say(b"before\r\n");
        played.wait_for_chat("before");
        assert_eq!(recorded(&mut played), 0, "closed, so nothing is recorded");

        open(&mut played);
        played.click("page-back");
        played.frames_until("the record to go", |shell, cx| {
            shell.settings.read(cx).inspected_lines() == 0
        });
        played.say(b"after back\r\n");
        played.wait_for_chat("after back");
        assert_eq!(recorded(&mut played), 0, "Back ends the recording");

        open(&mut played);
        played.shell.update_in(played.cx, |shell, window, cx| {
            shell.close_settings(window, cx);
        });
        played.say(b"after closing\r\n");
        played.wait_for_chat("after closing");
        assert_eq!(recorded(&mut played), 0, "closing the sheet ends it too");
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
    fn leaving_for_the_empty_state_empties_the_gear_panel(cx: &mut TestAppContext) {
        let (mut launched, _socket) = launch_playing(game_plugin(), cx);
        launched.shell.update(launched.cx, |shell, cx| {
            let items = crate::measure::sample_items_with_sets();
            shell
                .gear_panel
                .update(cx, |panel, cx| panel.show_items(items, cx));
        });
        launched.cx.run_until_parked();
        launched.shell.read_with(launched.cx, |shell, cx| {
            let items = shell.gear_panel.read(cx).items();
            assert!(!items.worn.is_empty() && !items.carried.is_empty());
            assert!(!items.sets.is_empty());
        });

        launched.click("session-switcher");
        launched.click("session-choose-server");
        launched.frames_until("the empty state", |shell, _| shell.on_empty_state());
        launched.shell.read_with(launched.cx, |shell, cx| {
            let items = shell.gear_panel.read(cx).items();
            assert!(items.worn.is_empty() && items.carried.is_empty());
            assert!(items.sets.is_empty());
        });
    }

    #[gpui::test]
    fn leaving_for_the_empty_state_forgets_the_sessions_chips_rules_and_find(
        cx: &mut TestAppContext,
    ) {
        let (mut launched, _socket) = launch_playing(game_plugin(), cx);
        let rules = RulesState {
            hotbar: vec!["look".to_owned()],
            ..RulesState::default()
        };
        launched.shell.update_in(launched.cx, |shell, window, cx| {
            shell.show_sample_rules(rules.clone(), cx);
            shell
                .hotbar
                .update(cx, |hotbar, cx| hotbar.show(&rules.hotbar, cx));
            shell.open_find(window, cx);
        });
        launched.cx.run_until_parked();
        launched.shell.read_with(launched.cx, |shell, cx| {
            assert!(shell.chat.read(cx).find_is_open());
            assert_eq!(shell.hotbar.read(cx).chips(), ["look"]);
        });

        launched.click("session-switcher");
        launched.click("session-choose-server");
        launched.frames_until("the empty state", |shell, _| shell.on_empty_state());
        launched.shell.read_with(launched.cx, |shell, cx| {
            assert!(!shell.chat.read(cx).find_is_open());
            assert!(shell.hotbar.read(cx).chips().is_empty());
            assert_eq!(shell.settings.read(cx).rules(), &RulesState::default());
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

    #[gpui::test]
    fn the_character_the_game_names_is_who_the_rules_are_for(cx: &mut TestAppContext) {
        let mut played = play(cx);
        played.connected();
        played.shell.read_with(played.cx, |shell, cx| {
            assert_eq!(shell.settings.read(cx).rules().character, "");
        });
        played.gmcp(r#"Char.Status {"character_name": "Bren"}"#);
        played.frames_until("the rules to know who plays", |shell, cx| {
            shell.settings.read(cx).rules().character == "bren"
        });
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

    /// A port nothing listens on: the listener goes with this call.
    fn unused_port() -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.local_addr().unwrap().port()
    }

    /// A window on sync with no game.
    fn synced_window<'a>(
        cx: &'a mut TestAppContext,
        folder: &Path,
        repo: Option<&Path>,
    ) -> (Entity<Shell>, &'a mut VisualTestContext, Syncer) {
        cx.update(|cx| {
            crate::bind_keys(cx);
            cx.set_reduce_motion(true);
        });
        let (_waker, woken) = unbounded();
        let config = SyncConfig {
            repo: repo.map(RepoSettings::new),
            ..SyncConfig::new(folder)
        };
        let syncer = Syncer::start(config, || {}).unwrap();
        let handle = syncer.clone();
        let (shell, cx) = cx.add_window_view(move |window, cx| {
            let mut shell = Shell::new(None, woken, window, cx);
            shell.use_sync(handle, Arc::new(MemoryKeyStore::default()), window, cx);
            shell
        });
        (shell, cx, syncer)
    }

    /// Applies the stores until `done`: sync answers from a real thread.
    fn applied_until(
        shell: &Entity<Shell>,
        cx: &mut VisualTestContext,
        what: &str,
        done: impl Fn(&Shell, &App) -> bool,
    ) {
        let began = Instant::now();
        while began.elapsed() < WAIT {
            shell.update_in(cx, |shell, window, cx| shell.apply_stores(window, cx));
            cx.run_until_parked();
            if shell.read_with(cx, |shell, cx| done(shell, cx)) {
                return;
            }
            sleep(Duration::from_millis(5));
        }
        panic!("never saw {what}");
    }

    #[gpui::test]
    fn the_vault_opens_where_sync_says_it_lives_with_no_game_running(cx: &mut TestAppContext) {
        let folder = TempDir::new().unwrap();
        let (shell, cx, syncer) = synced_window(cx, folder.path(), None);
        applied_until(&shell, cx, "the vault", |shell, cx| {
            shell.settings.read(cx).vault().is_some()
        });
        let path = shell.read_with(cx, |shell, _| {
            shell.sync.as_ref().unwrap().vault_file.clone()
        });
        assert_eq!(path, folder.path().join(mudling_core::vault::FILE_NAME));
        syncer.quit().wait();
    }

    #[gpui::test]
    fn setting_a_repo_moves_the_vault_there_and_the_choice_is_kept(cx: &mut TestAppContext) {
        let (folder, repo) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let (shell, cx, syncer) = synced_window(cx, folder.path(), None);
        applied_until(&shell, cx, "the first vault", |shell, cx| {
            shell.settings.read(cx).vault().is_some()
        });

        let chosen = repo.path().to_owned();
        shell.update(cx, |shell, cx| shell.set_repo(&chosen, cx));
        let wanted = repo.path().join(mudling_core::vault::FILE_NAME);
        applied_until(&shell, cx, "the vault moving", |shell, _| {
            shell.sync.as_ref().unwrap().vault_file == wanted
        });
        let kept = shell.read_with(cx, |shell, _| {
            shell.sync.as_ref().unwrap().choices.repo.clone()
        });
        assert_eq!(kept.as_deref(), Some(repo.path()));

        shell.update(cx, |shell, cx| shell.clear_repo(cx));
        applied_until(&shell, cx, "sync letting go", |shell, _| {
            shell.sync.as_ref().unwrap().choices.repo.is_none()
        });
        syncer.quit().wait();
    }

    #[gpui::test]
    fn a_folder_that_is_or_holds_the_data_folder_is_refused_in_words(cx: &mut TestAppContext) {
        let outer = TempDir::new().unwrap();
        let data = outer.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let (shell, cx, syncer) = synced_window(cx, &data, None);
        applied_until(&shell, cx, "the first vault", |shell, cx| {
            shell.settings.read(cx).vault().is_some()
        });
        let launcher = Launcher::new(data.clone(), Vec::new(), Vec::new(), || {});
        shell.update_in(cx, |shell, window, cx| {
            shell.use_launcher(launcher, window, cx);
        });

        for folder in [data.clone(), outer.path().to_owned()] {
            shell.update(cx, |shell, cx| shell.set_repo(&folder, cx));
            let kept = shell.read_with(cx, |shell, _| {
                shell.sync.as_ref().unwrap().choices.repo.clone()
            });
            assert_eq!(kept, None, "{} was taken", folder.display());
            let said = shell.read_with(cx, |shell, cx| shell.settings.read(cx).repo_refusal());
            assert!(said.is_some_and(|words| words.contains("MUDling's own data")));
        }
        assert!(!data.join(".git").exists(), "nothing was made in it");

        let beside = outer.path().join("repo");
        shell.update(cx, |shell, cx| shell.set_repo(&beside, cx));
        let kept = shell.read_with(cx, |shell, _| {
            shell.sync.as_ref().unwrap().choices.repo.clone()
        });
        assert_eq!(kept.as_deref(), Some(beside.as_path()));
        let said = shell.read_with(cx, |shell, cx| shell.settings.read(cx).repo_refusal());
        assert_eq!(said, None, "a good choice clears the refusal");
        syncer.quit().wait();
    }

    #[gpui::test]
    fn a_server_a_sync_brings_in_is_listed_without_a_relaunch(cx: &mut TestAppContext) {
        let folder = TempDir::new().unwrap();
        let (shell, cx, syncer) = synced_window(cx, folder.path(), None);
        let launcher = Launcher::new(folder.path().to_owned(), Vec::new(), Vec::new(), || {});
        shell.update_in(cx, |shell, window, cx| {
            shell.use_launcher(launcher, window, cx);
        });
        let hosts = |shell: &Entity<Shell>, cx: &mut VisualTestContext| -> Vec<String> {
            shell.read_with(cx, |shell, cx| {
                let cards = shell.empty_state.read(cx).cards().to_vec();
                cards.into_iter().map(|card| card.target.host).collect()
            })
        };
        applied_until(&shell, cx, "the first listing", |shell, _| {
            shell.job.is_none()
        });
        assert!(hosts(&shell, cx).is_empty());

        // What a merge does to this device: another's server arrives.
        persist::save_server(folder.path(), "brought.example.org", 4000).unwrap();
        shell.update_in(cx, |shell, window, cx| {
            let landed = SyncState {
                last_sync: Some(Timestamp::now()),
                ..SyncState::default()
            };
            shell.sync.as_mut().unwrap().copy.write(&landed);
            shell.show_sync(window, cx);
        });

        applied_until(&shell, cx, "the server listed", |shell, cx| {
            shell.job.is_none()
                && (shell.empty_state.read(cx).cards().iter())
                    .any(|card| card.target.host == "brought.example.org")
        });
        let listed = shell.read_with(cx, |shell, cx| shell.settings.read(cx).server_hosts());
        assert_eq!(listed, ["brought.example.org"], "Settings lists it too");
        syncer.quit().wait();
    }

    #[gpui::test]
    fn newer_settings_from_another_device_are_worn_and_older_ones_are_not(cx: &mut TestAppContext) {
        let folder = TempDir::new().unwrap();
        let (shell, cx, syncer) = synced_window(cx, folder.path(), None);
        let settings = |theme: &str, updated| AppSettings {
            theme: theme.to_owned(),
            updated: Timestamp(updated),
            ..AppSettings::default()
        };
        shell.update_in(cx, |shell, window, cx| {
            shell.adopt_settings(settings("dark", 20), window, cx);
        });
        let worn =
            |cx: &mut VisualTestContext| cx.update(|_, cx| cx.global::<Theme>().choice.theme);
        assert_eq!(worn(cx), ThemeChoice::Dark);

        shell.update_in(cx, |shell, window, cx| {
            shell.adopt_settings(settings("light", 10), window, cx);
        });
        assert_eq!(worn(cx), ThemeChoice::Dark);
        syncer.quit().wait();
    }

    #[gpui::test]
    fn choosing_a_theme_tells_sync_and_moving_the_card_does_not(cx: &mut TestAppContext) {
        let folder = TempDir::new().unwrap();
        let (shell, cx, syncer) = synced_window(cx, folder.path(), None);
        let updated = |shell: &Entity<Shell>, cx: &mut VisualTestContext| {
            shell.read_with(cx, |shell, _| shell.sync.as_ref().unwrap().settings.updated)
        };
        let choice = cx.update(|_, cx| cx.global::<Theme>().choice);
        shell.update_in(cx, |shell, window, cx| {
            let card = Appearance {
                card: Some((1, 2)),
                ..choice
            };
            shell.apply_choice(card, window, cx);
        });
        assert_eq!(updated(&shell, cx), Timestamp::default());

        shell.update_in(cx, |shell, window, cx| {
            let dark = Appearance {
                theme: ThemeChoice::Dark,
                ..choice
            };
            shell.apply_choice(dark, window, cx);
        });
        assert!(updated(&shell, cx) > Timestamp::default());
        syncer.quit().wait();
    }

    #[gpui::test]
    fn leaving_quits_sync_last_so_its_last_commit_lands(cx: &mut TestAppContext) {
        let (folder, repo) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let (shell, cx, syncer) = synced_window(cx, folder.path(), Some(repo.path()));
        applied_until(&shell, cx, "the repo opening", |shell, _| {
            shell.sync.as_ref().unwrap().copy.value().repo.is_some()
        });
        let left = shell.update(cx, |shell, cx| shell.leave(cx)).unwrap();
        left.detach();
        cx.run_until_parked();
        syncer.quit().wait();
        let branch = repo.path().join(".git/refs/heads/main");
        assert!(branch.exists(), "the last pass committed");
    }

    #[gpui::test]
    fn a_vault_only_the_player_can_open_stops_for_them_and_closing_connects(
        cx: &mut TestAppContext,
    ) {
        let (shell, cx) = window_with_keys(cx);
        let folder = TempDir::new().unwrap();
        let launcher = Launcher::new(folder.path().to_owned(), Vec::new(), Vec::new(), || {});
        let target = Target {
            host: "127.0.0.1".to_owned(),
            port: unused_port(),
        };
        shell.update_in(cx, |shell, window, cx| {
            shell.use_launcher(launcher, window, cx);
            let outcome = Outcome {
                locked_out: Some(WaitingForVault {
                    target,
                    turn_on: None,
                }),
                ..Outcome::default()
            };
            shell.job_done(outcome, window, cx);
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("settings-sheet").is_some());
        assert!(shell.read_with(cx, |shell, _| shell.game.is_none()));

        cx.simulate_keystrokes("escape");
        applied_until(&shell, cx, "the game opening", |shell, _| {
            shell.game.is_some()
        });
        let game = shell.update(cx, |shell, _| shell.game.take()).unwrap();
        game.session.quit().wait().unwrap();
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
