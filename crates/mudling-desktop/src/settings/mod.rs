//! The settings sheet: one modal sheet with eight sections, opened by the title
//! bar's sprocket or its shortcut.
//!
//! The sheet walks its own stops, because gpui's Tab order spans the window and
//! the sheet has to keep the keyboard inside it. Every control takes a stop
//! from [`Pane`], which remembers the row that holds it, so Tab can bring a
//! control scrolled out of view back in. The arrow and page keys scroll what
//! has no control to land on, like the list of shortcuts.

mod add_plugin;
mod appearance;
mod character;
mod data;
mod directory;
mod inspector;
mod logs;
mod plugins;
mod saved;
mod server_plugins;
mod servers;
mod shortcuts;
mod triggers;
mod updates;
mod vault;

pub use add_plugin::{Added, AddedPlugin, shown_path};
pub use directory::{Downloaded, Listing};
pub use plugins::{PluginRow, Sending};
pub use saved::{LockAfter, SavedSettings, SyncChoices, app_settings, with_app_settings};
pub use server_plugins::{ServerPlugins, SuggestedPlugin};
pub use servers::{CharacterOn, PasswordState, Passwords, SavedServer};
pub use updates::UpdateAsk;

use crate::assets::{HEADING_FONT, Icon, UI_FONT};
use crate::input;
use crate::launcher::Target;
use crate::shell::{FocusHotbar, PinCommand, ToggleLeftPanel, ToggleRightPanel};
use crate::theme::{Appearance, CHAT_FONTS, Theme};
use crate::title_bar::{FindInScrollback, OpenSettings, problems_text};
use crate::update::UpdateStatus;
use gpui::{
    AnyElement, App, Bounds, Context, Div, ElementId, EventEmitter, FocusHandle, FontWeight, Hsla,
    KeyBinding, MouseButton, Pixels, Role, ScrollHandle, SharedString, Size, Stateful, Task,
    Window, actions, div, point, prelude::*, px, size, transparent_black,
};
use inspector::InspectedRow;
use mudling_core::plugin::luau::PluginDisabled;
use mudling_core::plugin::{PluginCost, PluginId};
use mudling_core::registry::{Entry, Fetched, Index, Refused};
use mudling_core::session::RuleEdit;
use mudling_core::stats::Summary;
use mudling_core::store::{RulesState, SettingsState};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

const WIDTH: Pixels = px(1000.);
const HEIGHT: Pixels = px(716.);
/// The space kept around the sheet in a small window.
const MARGIN: Pixels = px(24.);
/// No less than the sheet's corner radius.
const CORNER: Pixels = px(16.);
const NAV_WIDTH: Pixels = px(264.);
/// The most of a small sheet the sections take.
const NAV_SHARE: f32 = 0.36;
const CONTEXT: &str = "SettingsSheet";
const MAKER: &str = "A Vyttle app";
/// How far an arrow key scrolls.
pub const SCROLL_STEP: Pixels = px(48.);

actions!(
    settings,
    [
        CloseSettings,
        NextStop,
        PreviousStop,
        ScrollUp,
        ScrollDown,
        PageUp,
        PageDown,
    ]
);

pub fn bind_keys(cx: &mut App) {
    let bound_here = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("escape", CloseSettings, bound_here),
        KeyBinding::new("tab", NextStop, bound_here),
        KeyBinding::new("shift-tab", PreviousStop, bound_here),
        KeyBinding::new("up", ScrollUp, bound_here),
        KeyBinding::new("down", ScrollDown, bound_here),
        KeyBinding::new("pageup", PageUp, bound_here),
        KeyBinding::new("pagedown", PageDown, bound_here),
    ]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Servers,
    Plugins,
    Triggers,
    Appearance,
    Shortcuts,
    Data,
    Logs,
    Vault,
}

impl Section {
    const ALL: [Section; 8] = [
        Section::Servers,
        Section::Plugins,
        Section::Triggers,
        Section::Appearance,
        Section::Shortcuts,
        Section::Data,
        Section::Logs,
        Section::Vault,
    ];

    fn title(self) -> &'static str {
        match self {
            Section::Servers => "Servers & characters",
            Section::Plugins => "Plugins",
            Section::Triggers => "Triggers & aliases",
            Section::Appearance => "Appearance",
            Section::Shortcuts => "Shortcuts",
            Section::Data => "Data & sync",
            Section::Logs => "Logs",
            Section::Vault => "Vault",
        }
    }
}

pub enum SheetEvent {
    Closed,
    Chose(Appearance),
    /// The player confirmed taking this host off their list.
    RemoveServer(String),
    /// Save the character `name` on `server`, and the server if that's new.
    AddCharacter {
        server: Target,
        name: String,
    },
    /// The player confirmed taking the character `id` off `host`'s list.
    RemoveCharacter {
        host: String,
        id: String,
    },
    /// What the vault says of the characters' passwords changed.
    PasswordsChanged,
    /// The player chose to log out and connect `Target` instead.
    Switch(Target),
    TurnOn(PluginId),
    /// The Inspector page opened or closed: the session records only between.
    Inspect(bool),
    /// Forget what the inspector recorded, and carry on recording.
    ClearInspector,
    /// The player asked to see this server's plugins.
    ServerPlugins(String),
    /// The player's switch for `plugin` on `host`, to save.
    SetPlugin {
        host: SharedString,
        plugin: PluginId,
        on: bool,
    },
    /// The directory opened: fetch and check its index.
    OpenDirectory,
    /// Download `entry` and check it against `index`.
    Fetch {
        index: Arc<Index>,
        entry: Entry,
    },
    /// Put a checked plugin in place, letting it send commands if `approve`.
    Install {
        fetched: Arc<Fetched>,
        approve: bool,
    },
    /// Whether `plugin` may send commands, to save and to apply at once.
    SetApproved {
        plugin: PluginId,
        approved: bool,
    },
    /// Take `plugin` out of the data folder, from `folder`, the one its row
    /// came from, as the player confirmed.
    Remove {
        plugin: PluginId,
        folder: SharedString,
    },
    /// The page that adds a plugin opened: show what was added.
    OpenAddPlugin,
    /// Fetch the plugin at this git link and check it.
    AddLink(String),
    /// Load the plugin in this folder on this device from now on.
    AddFolder(PathBuf),
    /// Stop loading the folder the player added, and `plugin`, the one it
    /// held, if it ever loaded. The folder stays.
    ForgetFolder {
        plugin: Option<PluginId>,
        path: PathBuf,
    },
    /// Check the copy in `folder`, shown with `hash`, against the directory,
    /// or else ask the player whether to trust it.
    ReviewArrival {
        folder: SharedString,
        hash: SharedString,
    },
    /// Load the copy in `folder`, as long as it's still the one shown with
    /// `hash`, as the player chose.
    Trust {
        folder: SharedString,
        hash: SharedString,
    },
    /// A change to the player's rules, for the session.
    Edit(RuleEdit),
    /// A sound was chosen: the player hears what they chose.
    TrySound(&'static str),
    /// New log settings are saved, for the session to follow.
    LoggingChanged,
    /// Sync into this folder from now on.
    SetRepo(PathBuf),
    /// The player confirmed going back to keeping everything local.
    ClearRepo,
    SyncNow,
    LockAfter(LockAfter),
    KeepKeyOnDevice(bool),
    VaultUnlocked,
    Update(UpdateAsk),
}

/// What the pane shows: its section, or a page opened from it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Page {
    Section,
    Inspector,
    Server(ServerPlugins),
    Directory,
    AddPlugin,
}

/// The pane's stops. Controls' handles are kept between frames, so a control
/// keeps its focus handle.
#[derive(Default)]
struct Stops {
    handles: Vec<FocusHandle>,
    /// How many of `handles` this frame has handed out.
    handed_out: usize,
    in_use: Vec<FocusHandle>,
    /// The row of the pane each stop in use sits in.
    rows: Vec<usize>,
}

impl Stops {
    fn in_use(&self) -> &[FocusHandle] {
        &self.in_use
    }

    fn start_frame(&mut self) {
        self.handed_out = 0;
        self.in_use.clear();
        self.rows.clear();
    }
}

/// A section's content being built: rows for the scrolling list, so the list
/// can scroll to any of them, and the stops inside those rows.
pub(crate) struct Pane {
    rows: Vec<AnyElement>,
    stops: Stops,
    /// What a page puts beside its title: Back, then any action.
    leading: Option<AnyElement>,
    trailing: Option<AnyElement>,
}

impl Pane {
    fn row(&mut self, row: impl IntoElement) {
        self.rows.push(row.into_any_element());
    }

    /// A stop for a control in the row about to be added.
    fn stop(&mut self, cx: &mut App) -> FocusHandle {
        let number = self.stops.handed_out;
        if number == self.stops.handles.len() {
            self.stops.handles.push(cx.focus_handle());
        }
        self.stops.handed_out += 1;
        let stop = self.stops.handles[number].clone();
        self.adopt(stop.clone());
        stop
    }

    /// Makes a stop of a handle that isn't the pane's, like a text field's,
    /// in the row about to be added.
    fn adopt(&mut self, handle: FocusHandle) {
        self.stops.in_use.push(handle);
        self.stops.rows.push(self.rows.len());
    }
}

/// Where the sheet sits in a window of this size.
fn sheet_bounds(room: Size<Pixels>) -> Bounds<Pixels> {
    let sheet = size(
        WIDTH.min(room.width - MARGIN * 2.),
        HEIGHT.min(room.height - MARGIN * 2.),
    );
    Bounds::centered_at(point(room.width / 2., room.height / 2.), sheet)
}

/// What the open sheet hides for certain: its corners are round.
pub fn hidden_under_sheet(room: Size<Pixels>) -> Bounds<Pixels> {
    sheet_bounds(room).dilate(-CORNER)
}

/// A heading inside a section.
fn section_heading(title: &'static str, theme: &Theme) -> Stateful<Div> {
    div()
        .id(title)
        .role(Role::Heading)
        .aria_label(title)
        .aria_level(3)
        .pt_4()
        .pb_2()
        .font_family(UI_FONT)
        .text_size(px(11.))
        .text_color(theme.text_muted)
        .child(title.to_uppercase())
}

/// One plain line, as a screen reader can read it.
fn muted_line(text: impl Into<SharedString>, theme: &Theme) -> Stateful<Div> {
    let text = text.into();
    div()
        .id(text.clone())
        .role(Role::Label)
        .aria_value(text.clone())
        .pb_2()
        .font_family(UI_FONT)
        .text_size(px(14.))
        .text_color(theme.text_muted)
        .child(text)
}

/// A line saying what went wrong, read out as soon as it shows.
fn problem_line(text: impl Into<SharedString>, theme: &Theme) -> Stateful<Div> {
    muted_line(text, theme)
        .role(Role::Alert)
        .text_color(theme.error)
}

/// A question to answer before going on, read out as soon as it shows.
fn question_line(text: impl Into<SharedString>, theme: &Theme) -> Stateful<Div> {
    muted_line(text, theme)
        .role(Role::Alert)
        .text_color(theme.text)
}

/// A control's shape: a stop of the sheet, marked when focus lands on it.
fn control(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    stop: &FocusHandle,
    theme: &Theme,
) -> Stateful<Div> {
    let id = id.into();
    let selector = id.to_string();
    let (hover, ring) = (theme.hover, theme.accent);
    div()
        .id(id)
        .debug_selector(move || selector)
        .role(Role::Button)
        .aria_label(label)
        .track_focus(stop)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .border_color(transparent_black())
        .hover(move |style| style.bg(hover))
        .focus_visible(move |style| style.border_color(ring))
}

/// An outlined button with its label as its text.
pub(crate) fn text_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    stop: &FocusHandle,
    theme: &Theme,
) -> Stateful<Div> {
    let label = label.into();
    control(id, label.clone(), stop, theme)
        .h(px(36.))
        .px_3()
        .border_color(theme.border)
        .font_family(UI_FONT)
        .font_weight(FontWeight::MEDIUM)
        .text_size(px(14.))
        .text_color(theme.text)
        .child(label)
}

fn count_badge(count: usize, theme: &Theme) -> Div {
    div()
        .flex_none()
        .min_w(px(20.))
        .h(px(20.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .bg(theme.warning)
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.on_warning)
        .child(count.to_string())
}

pub struct SettingsSheet {
    section: Section,
    page: Page,
    server: Option<SharedString>,
    servers: Vec<SavedServer>,
    /// What the last thing done to the saved servers ran into.
    servers_problem: Option<SharedString>,
    /// The host of the game running, which can't be removed.
    playing: Option<SharedString>,
    /// The host whose removal is being asked about.
    removing: Option<SharedString>,
    /// The character whose removal is being asked about.
    removing_character: Option<CharacterOn>,
    /// Add character or Edit, open alone over the window.
    character: Option<character::CharacterForm>,
    character_fields: character::Fields,
    /// Whether to log out and connect another character, asked alone over
    /// the window.
    switching: Option<character::Switch>,
    plugins: Vec<PluginRow>,
    /// The player installed, updated or removed a plugin since the session
    /// opened, which it won't follow.
    installs_changed: bool,
    /// Installed plugins that weren't loaded, and why.
    refused: Vec<Refused>,
    rules: RulesState,
    logs: logs::LogsPanel,
    form: triggers::Form,
    notices: Vec<String>,
    turned_off: Vec<PluginDisabled>,
    costs: Vec<PluginCost>,
    /// What all the plugins cost a line together.
    line_cost: Option<Summary>,
    /// The inspector's lines, newest last, while its page is open.
    inspector: VecDeque<InspectedRow>,
    /// Held while the inspector has drawn lately, so new lines wait their turn.
    inspector_cooldown: Option<Task<()>>,
    /// Lines arrived during the cooldown, so the page is out of date.
    inspector_late: bool,
    /// Why the last switch couldn't be saved.
    switch_problem: Option<SharedString>,
    /// The notice whose details are showing.
    details: Option<String>,
    /// The chat faces this machine has.
    fonts: Vec<&'static str>,
    data: data::DataPane,
    vault: vault::VaultPane,
    directory: directory::DirectoryPane,
    add: add_plugin::AddPane,
    /// The approval open over the sheet, which keeps the keyboard until it's
    /// answered.
    asking: Option<directory::Approval>,
    update: UpdateStatus,
    /// What restarting would drop, while Restart to update asks about it.
    asking_restart: Option<SharedString>,
    /// The version line was clicked since Settings opened, so it shows what
    /// the update status says of that check.
    checked_here: bool,
    dialog: FocusHandle,
    tabs: [FocusHandle; Section::ALL.len()],
    /// The version line's two stops: the check, and Restart to update.
    about: FocusHandle,
    about_restart: FocusHandle,
    close: FocusHandle,
    stops: Stops,
    /// A stop to bring into view at the next draw, once its row exists.
    reveal: Option<FocusHandle>,
    scroll: ScrollHandle,
    /// Keeps "minutes ago" true while Data & sync shows it.
    _ago_ticks: Option<Task<()>>,
    #[cfg(test)]
    renders: usize,
}

impl EventEmitter<SheetEvent> for SettingsSheet {}

impl SettingsSheet {
    pub fn new(window: &mut Window, cx: &mut Context<SettingsSheet>) -> SettingsSheet {
        let text = cx.text_system().clone();
        let installed = cx.background_spawn(async move { text.all_font_names() });
        cx.spawn(async move |sheet, cx| {
            let installed = installed.await;
            let _ = sheet.update(cx, |sheet, cx| sheet.keep_fonts(&installed, cx));
        })
        .detach();
        SettingsSheet {
            section: Section::Appearance,
            page: Page::Section,
            server: None,
            servers: Vec::new(),
            servers_problem: None,
            playing: None,
            removing: None,
            removing_character: None,
            character: None,
            character_fields: character::Fields::new(window, cx),
            switching: None,
            plugins: Vec::new(),
            installs_changed: false,
            refused: Vec::new(),
            rules: RulesState::default(),
            logs: logs::LogsPanel::default(),
            form: triggers::Form::new(window, cx),
            notices: Vec::new(),
            turned_off: Vec::new(),
            costs: Vec::new(),
            line_cost: None,
            inspector: VecDeque::new(),
            inspector_cooldown: None,
            inspector_late: false,
            switch_problem: None,
            details: None,
            fonts: vec![CHAT_FONTS[0]],
            data: data::DataPane::default(),
            vault: vault::VaultPane::new(window, cx),
            directory: directory::DirectoryPane::new(cx),
            add: add_plugin::AddPane::new(window, cx),
            asking: None,
            update: UpdateStatus::default(),
            asking_restart: None,
            checked_here: false,
            dialog: cx.focus_handle(),
            tabs: std::array::from_fn(|_| cx.focus_handle()),
            about: cx.focus_handle(),
            about_restart: cx.focus_handle(),
            close: cx.focus_handle(),
            stops: Stops::default(),
            reveal: None,
            scroll: ScrollHandle::new(),
            _ago_ticks: None,
            #[cfg(test)]
            renders: 0,
        }
    }

    fn keep_fonts(&mut self, installed: &[String], cx: &mut Context<SettingsSheet>) {
        self.fonts = CHAT_FONTS
            .into_iter()
            .enumerate()
            .filter(|(number, font)| *number == 0 || installed.iter().any(|name| name == font))
            .map(|(_, font)| font)
            .collect();
        cx.notify();
    }

    /// Takes the keyboard. With problems to show, opens on the section that
    /// shows them.
    pub fn opened(&mut self, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        self.checked_here = false;
        self.removing = None;
        self.removing_character = None;
        self.switching = None;
        self.close_character_form_quietly(window, cx);
        self.logs.stop_asking();
        let opening_on = if self.is_waited_on() {
            Section::Vault
        } else if self.problems() > 0 {
            Section::Plugins
        } else if self.sync_problems() > 0 {
            Section::Data
        } else {
            self.section
        };
        self.select(opening_on, cx);
        self.focus_tab(self.section, window, cx);
    }

    /// Moves focus to `section`'s tab, as a control that held it goes away.
    fn focus_tab(&self, section: Section, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        window.focus(&self.tabs[section as usize], cx);
    }

    pub fn select(&mut self, section: Section, cx: &mut Context<SettingsSheet>) {
        self.leave_page(cx);
        if self.section == Section::Vault && section != Section::Vault {
            self.forget_typed(cx);
        }
        self.section = section;
        self._ago_ticks = (section == Section::Data).then(|| self.tick_ago(cx));
        match section {
            Section::Data => self.read_sign_in(cx),
            Section::Vault => self.list_vault(cx),
            _ => {}
        }
        self.scroll.set_offset(point(px(0.), px(0.)));
        if section == Section::Logs {
            self.refresh_logs(cx);
        }
        cx.notify();
    }

    /// Back to the section. Leaving the inspector ends its recording, and
    /// forgets what it held.
    pub fn leave_page(&mut self, cx: &mut Context<SettingsSheet>) {
        self.drop_approval();
        if self.page == Page::Inspector {
            self.forget_inspected();
            cx.emit(SheetEvent::Inspect(false));
        }
        self.page = Page::Section;
        cx.notify();
    }

    fn go_to_page(&mut self, page: Page, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        self.page = page;
        self.scroll.set_offset(point(px(0.), px(0.)));
        // The first stop of any page is its Back.
        if let Some(back) = self.stops.handles.first() {
            window.focus(back, cx);
        }
        cx.notify();
    }

    /// The button that leaves a page for `section`.
    fn back_button(
        &self,
        section: Section,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) -> Stateful<Div> {
        text_button("page-back", "Back", &pane.stop(cx), theme).on_click(cx.listener(
            move |sheet, _, window, cx| {
                sheet.leave_page(cx);
                sheet.focus_tab(section, window, cx);
            },
        ))
    }

    fn page_title(&self) -> SharedString {
        match &self.page {
            Page::Section => self.section.title().into(),
            Page::Inspector => "Inspector".into(),
            Page::Server(page) => format!("Plugins for {}", page.host).into(),
            Page::Directory => directory::DIRECTORY_TITLE.into(),
            Page::AddPlugin => add_plugin::ADD_TITLE.into(),
        }
    }

    /// What Data & sync adds to the sprocket's count.
    pub fn sync_problems(&self) -> usize {
        self.data.problems()
    }

    pub fn show_servers(
        &mut self,
        servers: Vec<SavedServer>,
        problem: Option<String>,
        cx: &mut Context<SettingsSheet>,
    ) {
        self.servers = servers;
        self.servers_problem = problem.map(Into::into);
        cx.notify();
    }

    pub fn show_servers_problem(&mut self, problem: String, cx: &mut Context<SettingsSheet>) {
        self.servers_problem = Some(problem.into());
        cx.notify();
    }

    /// Whether the player changed a switch for the session running, which
    /// won't follow it.
    pub fn reconnect_to_apply(&self) -> bool {
        self.installs_changed || self.plugins.iter().any(PluginRow::is_switched)
    }

    #[cfg(test)]
    pub fn close_stop(&self) -> FocusHandle {
        self.close.clone()
    }

    #[cfg(test)]
    pub fn is_asking_to_remove(&self) -> bool {
        self.removing.is_some()
    }

    #[cfg(test)]
    pub fn on_a_page(&self) -> bool {
        self.page != Page::Section
    }

    #[cfg(test)]
    pub fn inspected_lines(&self) -> usize {
        self.inspector.len()
    }

    #[cfg(test)]
    pub fn server_hosts(&self) -> Vec<String> {
        (self.servers.iter())
            .map(|server| server.host.to_string())
            .collect()
    }

    pub fn show_playing(&mut self, host: Option<&str>, cx: &mut Context<SettingsSheet>) {
        if self.playing.as_deref() != host {
            self.playing = host.map(|host| host.to_owned().into());
            cx.notify();
        }
    }

    /// A new session's plugins, which run whatever was installed before it.
    pub fn show_plugins(&mut self, plugins: Vec<PluginRow>, cx: &mut Context<SettingsSheet>) {
        self.plugins = plugins;
        self.installs_changed = false;
        cx.notify();
    }

    #[cfg(test)]
    pub fn servers(&self) -> &[SavedServer] {
        &self.servers
    }

    #[cfg(test)]
    pub fn plugins(&self) -> &[PluginRow] {
        &self.plugins
    }

    #[cfg(test)]
    pub fn turned_off(&self) -> &[PluginDisabled] {
        &self.turned_off
    }

    #[cfg(test)]
    pub fn costs(&self) -> &[PluginCost] {
        &self.costs
    }

    /// What the session says about itself.
    pub fn show_session(&mut self, settings: &SettingsState, cx: &mut Context<SettingsSheet>) {
        let server = settings.server.as_deref();
        let changed = self.server.as_deref() != server
            || self.notices != settings.notices
            || self.turned_off != settings.turned_off
            || self.costs != settings.plugin_costs
            || self.line_cost != settings.plugin_line_cost;
        if changed {
            self.server = server.map(|server| server.to_owned().into());
            self.notices.clone_from(&settings.notices);
            self.turned_off.clone_from(&settings.turned_off);
            self.costs.clone_from(&settings.plugin_costs);
            self.line_cost = settings.plugin_line_cost;
            cx.notify();
        }
    }

    fn problems(&self) -> usize {
        self.notices.len() + self.turned_off.len()
    }

    fn choose(&mut self, appearance: Appearance, cx: &mut Context<SettingsSheet>) {
        cx.emit(SheetEvent::Chose(appearance));
    }

    fn close(&mut self, cx: &mut Context<SettingsSheet>) {
        cx.emit(SheetEvent::Closed);
    }

    /// The stops the sheet keeps outside its pane, in Tab order: the section
    /// tabs, the version line, then Close.
    fn sheet_stops(&self) -> Vec<&FocusHandle> {
        let restart = self.ready_to_restart().then_some(&self.about_restart);
        (self.tabs.iter())
            .chain(self.about_stop())
            .chain(restart)
            .chain([&self.close])
            .collect()
    }

    /// Moves `step` stops on, round the sheet, and brings the stop into view.
    fn walk(&mut self, step: isize, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        // An approval, a question or a dialog keeps the keyboard on its own
        // stops.
        let fixed = if self.asking.is_some() || self.stands_alone() {
            Vec::new()
        } else {
            self.sheet_stops()
        };
        let order: Vec<&FocusHandle> = (fixed.iter().copied()).chain(self.stops.in_use()).collect();
        let at = order.iter().position(|stop| stop.is_focused(window));
        let next = at.map_or(0, |at| {
            (at as isize + step).rem_euclid(order.len() as isize) as usize
        });
        window.focus(order[next], cx);
        if let Some(stop) = next
            .checked_sub(fixed.len())
            .filter(|_| self.asking.is_none())
        {
            self.scroll.scroll_to_item(self.stops.rows[stop]);
            cx.notify();
        }
    }

    fn scroll_by(&self, distance: Pixels, cx: &mut Context<SettingsSheet>) {
        let offset = self.scroll.offset();
        let lowest = -self.scroll.max_offset().y;
        self.scroll
            .set_offset(point(offset.x, (offset.y - distance).clamp(lowest, px(0.))));
        cx.notify();
    }

    fn render_tab(
        &self,
        section: Section,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) -> impl IntoElement + use<> {
        let chosen = section == self.section;
        let problems = match section {
            Section::Plugins => self.problems(),
            Section::Data => self.sync_problems(),
            _ => 0,
        };
        control(
            SharedString::from(format!("section-{}", section as usize)),
            section.title(),
            &self.tabs[section as usize],
            theme,
        )
        .role(Role::Tab)
        .aria_selected(chosen)
        .when(problems > 0, |tab| {
            tab.aria_description(problems_text(problems))
        })
        .w_full()
        .h(px(42.))
        .px_3()
        .justify_between()
        .rounded_lg()
        .when(chosen, |tab| tab.bg(theme.selected))
        .text_size(px(15.))
        .text_color(theme.text)
        .on_click(cx.listener(move |sheet, _, _, cx| sheet.select(section, cx)))
        .child(section.title())
        .when(problems > 0, |tab| tab.child(count_badge(problems, theme)))
    }

    fn render_nav(
        &self,
        width: Pixels,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) -> impl IntoElement + use<> {
        let tabs = Section::ALL.map(|section| self.render_tab(section, theme, cx));
        div()
            .flex_none()
            .w(width)
            .h_full()
            .p_3()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(theme.divider)
            .child(
                div()
                    .id("settings-heading")
                    .role(Role::Heading)
                    .aria_label("Settings")
                    .aria_level(1)
                    .px_3()
                    .pt_3()
                    .pb_5()
                    .font_family(HEADING_FONT)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_size(px(22.))
                    .text_color(theme.text)
                    .child("Settings"),
            )
            .child(
                div()
                    .id("settings-sections")
                    .role(Role::TabList)
                    .aria_label("Settings sections")
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .children(tabs),
            )
            .child(self.render_about(theme, cx))
    }

    fn render_pane(
        &mut self,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<SettingsSheet>,
    ) -> impl IntoElement + use<> {
        let mut stops = std::mem::take(&mut self.stops);
        stops.start_frame();
        let mut pane = Pane {
            rows: Vec::new(),
            stops,
            leading: None,
            trailing: None,
        };
        match (&self.page, self.section) {
            (Page::Inspector, _) => self.fill_inspector(&mut pane, theme, window, cx),
            (Page::Server(page), _) => self.fill_server_plugins(page, &mut pane, theme, cx),
            (Page::Directory, _) => self.fill_directory(&mut pane, theme, cx),
            (Page::AddPlugin, _) => self.fill_add_plugin(&mut pane, theme, cx),
            (Page::Section, section) => {
                self.fill_update_card(&mut pane, theme, cx);
                self.fill_section(section, &mut pane, theme, cx);
            }
        }

        if let Some(wanted) = self.reveal.take()
            && let Some(at) = pane.stops.in_use().iter().position(|stop| *stop == wanted)
        {
            self.scroll.scroll_to_item(pane.stops.rows[at]);
        }
        self.stops = pane.stops;
        let title = self.page_title();
        let header = div()
            .flex_none()
            .h(px(84.))
            .px_8()
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(theme.divider)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_3()
                    .children(pane.leading)
                    .child(
                        div()
                            .id("settings-title")
                            .role(Role::Heading)
                            .aria_label(title.clone())
                            .aria_level(2)
                            .min_w_0()
                            .truncate()
                            .font_family(HEADING_FONT)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_size(px(19.))
                            .text_color(theme.text)
                            .child(title.clone()),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_3()
                    .children(pane.trailing)
                    .child(
                        control("settings-close", "Close settings", &self.close, theme)
                            .size(px(32.))
                            .on_click(cx.listener(|sheet, _, _, cx| sheet.close(cx)))
                            .child(Icon::WindowClose.svg().size(px(16.)).text_color(theme.text)),
                    ),
            );
        div()
            .id("settings-pane")
            .role(Role::TabPanel)
            .aria_label(title)
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(header)
            .child(
                div()
                    .id("settings-rows")
                    .flex_1()
                    .min_h_0()
                    .px_8()
                    .py_5()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(pane.rows),
            )
    }
}

impl SettingsSheet {
    fn fill_section(
        &mut self,
        section: Section,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) {
        match section {
            Section::Servers => self.fill_servers(pane, theme, cx),
            Section::Plugins => self.fill_plugins(pane, theme, cx),
            Section::Triggers => self.fill_triggers_section(pane, theme, cx),
            Section::Appearance => self.fill_appearance(pane, theme, cx),
            Section::Shortcuts => shortcuts::fill(pane, theme),
            Section::Logs => self.fill_logs(pane, theme, cx),
            Section::Data => self.fill_data(pane, theme, cx),
            Section::Vault => self.fill_vault(pane, theme, cx),
        }
    }
}

impl gpui::Render for SettingsSheet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<SettingsSheet>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.renders += 1;
        }
        let theme = *cx.global::<Theme>();
        let room = window.viewport_size();
        let sheet = sheet_bounds(room).size;
        let width = sheet.width;
        let scrim_click = cx.listener(|sheet, _, window, cx| {
            window.prevent_default();
            sheet.step_back(window, cx);
        });
        let dialog = div()
            .id("settings-sheet")
            .debug_selector(|| "settings-sheet".to_owned())
            .key_context(CONTEXT)
            .a11y_synthetic_children(|builder| builder.parent_node().set_modal())
            .track_focus(&self.dialog)
            .occlude()
            .font_family(UI_FONT)
            .text_color(theme.text)
            // Tab in a text field is a stop like any other here.
            .capture_action(cx.listener(|sheet, _: &input::Complete, window, cx| {
                sheet.walk(1, window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|sheet, _: &CloseSettings, window, cx| {
                if sheet.asking.is_some() && !sheet.stands_alone() {
                    sheet.answer(false, false, window, cx);
                } else {
                    sheet.step_back(window, cx);
                }
            }))
            .on_action(cx.listener(|sheet, _: &NextStop, window, cx| sheet.walk(1, window, cx)))
            .on_action(
                cx.listener(|sheet, _: &PreviousStop, window, cx| sheet.walk(-1, window, cx)),
            )
            .on_action(cx.listener(|sheet, _: &ScrollUp, _, cx| sheet.scroll_by(-SCROLL_STEP, cx)))
            .on_action(cx.listener(|sheet, _: &ScrollDown, _, cx| sheet.scroll_by(SCROLL_STEP, cx)))
            .on_action(cx.listener(|sheet, _: &PageUp, _, cx| {
                sheet.scroll_by(-sheet.scroll.bounds().size.height, cx);
            }))
            .on_action(cx.listener(|sheet, _: &PageDown, _, cx| {
                sheet.scroll_by(sheet.scroll.bounds().size.height, cx);
            }))
            // A modal sheet: what's behind it stays as it is.
            .on_action(|_: &ToggleLeftPanel, _, _| {})
            .on_action(|_: &ToggleRightPanel, _, _| {})
            .on_action(|_: &FindInScrollback, _, _| {})
            .on_action(|_: &OpenSettings, _, _| {})
            .on_action(|_: &FocusHotbar, _, _| {})
            .on_action(|_: &PinCommand, _, _| {});
        let alone = (self.render_restart_question(&theme, cx))
            .map(|question| (updates::RESTART_TITLE, question.into_any_element()))
            .or_else(|| {
                let question = self.render_switch_question(&theme, cx)?;
                Some((character::SWITCH_TITLE, question.into_any_element()))
            });
        let dialog = match alone {
            Some((title, question)) => dialog
                .role(Role::AlertDialog)
                .aria_label(title)
                .child(question),
            None if self.character.is_some() => {
                let form = self.render_character_dialog(&theme, cx);
                let title = self.character_title();
                dialog.role(Role::Dialog).aria_label(title).children(form)
            }
            None => {
                let dialog = dialog
                    .role(Role::Dialog)
                    .aria_label("Settings")
                    .w(width)
                    .h(sheet.height)
                    .flex()
                    .overflow_hidden()
                    .rounded_xl()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.surface)
                    .shadow(theme.floating_shadow());
                match self.render_approval(&theme, cx) {
                    Some(approval) => dialog.child(approval),
                    None => dialog
                        .child(self.render_nav(NAV_WIDTH.min(width * NAV_SHARE), &theme, cx))
                        .child(self.render_pane(&theme, window, cx)),
                }
            }
        };
        div()
            .id("settings-scrim")
            .debug_selector(|| "settings-scrim".to_owned())
            .occlude()
            .absolute()
            .top_0()
            .left_0()
            .w(room.width)
            .h(room.height)
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.scrim)
            .on_mouse_down(MouseButton::Left, scrim_click)
            .child(dialog)
    }
}

impl SettingsSheet {
    /// Whether a question or a dialog stands alone over the window, with the
    /// sheet stepped aside for it.
    fn stands_alone(&self) -> bool {
        self.asking_to_restart() || self.switching.is_some() || self.character.is_some()
    }

    /// Esc or a click beside it: what stands alone goes, or else the sheet.
    fn step_back(&mut self, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        if self.asking_to_restart() {
            self.answer_restart(false, window, cx);
        } else if self.switching.is_some() {
            self.answer_switch(false, window, cx);
        } else if self.character.is_some() {
            self.close_character_form(window, cx);
        } else {
            self.close(cx);
        }
    }
}

/// How a chosen thing stands out from its neighbors.
fn chosen_ground(chosen: bool, theme: &Theme) -> Hsla {
    if chosen {
        theme.surface_raised
    } else {
        transparent_black()
    }
}

#[cfg(test)]
mod tests {
    use super::inspector::REFRESH_EVERY;
    use super::*;
    use crate::chat::{COLUMN_PADDING, min_column_width};
    use crate::hud::HudPlacement;
    use crate::shell::{Shell, tests::window_with_keys};
    use crate::theme::{LARGEST_CHAT_SIZE, ThemeChoice};
    use gpui::{
        Entity, Modifiers, ScrollDelta, ScrollEvent, TestAppContext, VisualTestContext,
        WindowAppearance,
    };
    use mudling_core::defaults::triggers::{Highlight, Scope, TriggerRule};
    use mudling_core::plugin::luau::DisabledReason;
    use mudling_core::store::InspectorChange;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;
    use tempfile::TempDir;

    fn open(shell: &Entity<Shell>, cx: &mut VisualTestContext) {
        shell.update_in(cx, |shell, window, cx| shell.open_settings(window, cx));
        cx.run_until_parked();
    }

    fn click(id: &'static str, cx: &mut VisualTestContext) {
        let bounds = cx.debug_bounds(id).expect("the control is drawn");
        cx.simulate_click(bounds.center(), Modifiers::none());
        cx.run_until_parked();
    }

    fn chosen(cx: &mut VisualTestContext) -> Appearance {
        cx.update(|_, cx| cx.global::<Theme>().choice)
    }

    fn section(number: Section, cx: &mut VisualTestContext) {
        click(format!("section-{}", number as usize).leak(), cx);
    }

    fn open_key() -> &'static str {
        if cfg!(target_os = "macos") {
            "cmd-,"
        } else {
            "ctrl-,"
        }
    }

    #[gpui::test]
    fn the_sprocket_and_the_shortcut_open_it_and_esc_gives_typing_back(cx: &mut TestAppContext) {
        let (shell, cx) = window_with_keys(cx);
        click("open-settings", cx);
        assert!(cx.debug_bounds("settings-sheet").is_some());
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(cx.debug_bounds("settings-sheet").is_none());

        cx.simulate_keystrokes(open_key());
        cx.run_until_parked();
        assert!(cx.debug_bounds("settings-sheet").is_some());
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        cx.simulate_input("look");
        assert_eq!(shell.read_with(cx, |shell, cx| shell.typed(cx)), "look");
    }

    fn renders(sheet: &Entity<SettingsSheet>, cx: &mut VisualTestContext) -> usize {
        sheet.read_with(cx, |sheet, _| sheet.renders)
    }

    fn line_arrives(shell: &Entity<Shell>, cx: &mut VisualTestContext) {
        let chat = shell.read_with(cx, |shell, _| shell.chat());
        chat.update(cx, |chat, cx| chat.echo_unsent("look", cx));
        cx.run_until_parked();
    }

    #[gpui::test]
    fn lines_arriving_leave_the_sheet_as_it_was_drawn(cx: &mut TestAppContext) {
        let (shell, cx) = window_with_keys(cx);
        open(&shell, cx);
        let sheet = shell.read_with(cx, |shell, _| shell.settings_sheet());
        let before = renders(&sheet, cx);
        (0..5).for_each(|_| line_arrives(&shell, cx));
        assert_eq!(renders(&sheet, cx), before);
        assert!(
            cx.debug_bounds("settings-sheet").is_some(),
            "it still shows"
        );
    }

    fn inspected(lines: usize) -> InspectorChange {
        let line = crate::measure::sample_inspected().swap_remove(0);
        InspectorChange {
            clear: false,
            lines: vec![line; lines],
        }
    }

    fn inspect(sheet: &Entity<SettingsSheet>, lines: usize, cx: &mut VisualTestContext) {
        sheet.update_in(cx, |sheet, window, cx| {
            sheet.show_inspected(&inspected(lines), window, cx);
        });
    }

    #[gpui::test]
    fn the_inspector_draws_on_a_schedule_however_many_lines_arrive(cx: &mut TestAppContext) {
        let (shell, cx) = window_with_keys(cx);
        open(&shell, cx);
        let sheet = shell.read_with(cx, |shell, _| shell.settings_sheet());
        sheet.update_in(cx, |sheet, window, cx| sheet.open_inspector(window, cx));
        cx.run_until_parked();
        let before = renders(&sheet, cx);

        let apart = Duration::from_millis(30);
        for _ in 0..100 {
            inspect(&sheet, 1, cx);
            cx.run_until_parked();
            cx.executor().advance_clock(apart);
        }
        cx.executor().advance_clock(REFRESH_EVERY);
        cx.run_until_parked();
        let drawn = renders(&sheet, cx) - before;
        let at_most = (apart * 100).div_duration_f64(REFRESH_EVERY) as usize + 2;
        assert!(
            (3..=at_most).contains(&drawn),
            "100 lines drew {drawn} times"
        );
        assert_eq!(sheet.read_with(cx, |sheet, _| sheet.inspected_lines()), 100);
        assert!(cx.debug_bounds("inspected-0").is_some(), "the newest shows");

        let settled = renders(&sheet, cx);
        inspect(&sheet, 1, cx);
        cx.run_until_parked();
        assert_eq!(
            renders(&sheet, cx),
            settled + 1,
            "the first line draws at once"
        );
        inspect(&sheet, 1, cx);
        cx.run_until_parked();
        assert_eq!(renders(&sheet, cx), settled + 1, "the next waits its turn");
        cx.executor().advance_clock(REFRESH_EVERY);
        cx.run_until_parked();
        assert_eq!(renders(&sheet, cx), settled + 2, "and draws once it's due");
    }

    #[gpui::test]
    fn leaving_the_inspector_cancels_the_draw_it_was_waiting_for(cx: &mut TestAppContext) {
        let (shell, cx) = window_with_keys(cx);
        open(&shell, cx);
        let sheet = shell.read_with(cx, |shell, _| shell.settings_sheet());
        sheet.update_in(cx, |sheet, window, cx| sheet.open_inspector(window, cx));
        (0..2).for_each(|_| inspect(&sheet, 1, cx));
        sheet.update(cx, |sheet, cx| sheet.leave_page(cx));
        cx.run_until_parked();
        let left = renders(&sheet, cx);
        cx.executor().advance_clock(REFRESH_EVERY * 2);
        cx.run_until_parked();
        assert_eq!(renders(&sheet, cx), left);
        assert_eq!(sheet.read_with(cx, |sheet, _| sheet.inspected_lines()), 0);
    }

    #[gpui::test]
    fn what_changes_in_the_sheet_still_draws_it(cx: &mut TestAppContext) {
        let (shell, cx, _edits) = open_rules(cx);
        let sheet = shell.read_with(cx, |shell, _| shell.settings_sheet());

        let before = renders(&sheet, cx);
        click("new-trigger", cx);
        assert!(renders(&sheet, cx) > before, "a click");

        let before = renders(&sheet, cx);
        cx.simulate_input("t");
        assert!(renders(&sheet, cx) > before, "typing in a field");

        let before = renders(&sheet, cx);
        let theme = Appearance {
            theme: ThemeChoice::Light,
            ..chosen(cx)
        };
        sheet.update(cx, |sheet, cx| sheet.choose(theme, cx));
        cx.run_until_parked();
        assert!(renders(&sheet, cx) > before, "the theme");

        let before = renders(&sheet, cx);
        cx.simulate_resize(size(px(1200.), px(760.)));
        cx.run_until_parked();
        assert!(renders(&sheet, cx) > before, "the window's size");
    }

    #[gpui::test]
    fn a_wheel_scrolls_a_long_section(cx: &mut TestAppContext) {
        let (shell, cx) = window_with_keys(cx);
        cx.simulate_resize(size(px(700.), px(420.)));
        open(&shell, cx);
        section(Section::Shortcuts, cx);
        let sheet = shell.read_with(cx, |shell, _| shell.settings_sheet());
        let before = renders(&sheet, cx);
        let over_rows = sheet.read_with(cx, |sheet, _| sheet.scroll.bounds().center());
        cx.simulate_event(ScrollEvent {
            position: over_rows,
            delta: ScrollDelta::Pixels(point(px(0.), px(-100.))),
            ..ScrollEvent::default()
        });
        cx.run_until_parked();
        assert!(sheet.read_with(cx, |sheet, _| sheet.scroll.offset().y < px(0.)));
        assert!(renders(&sheet, cx) > before);
    }

    #[gpui::test]
    fn minutes_ago_moves_on_while_data_shows_it(cx: &mut TestAppContext) {
        let (shell, cx) = window_with_keys(cx);
        open(&shell, cx);
        section(Section::Data, cx);
        let sheet = shell.read_with(cx, |shell, _| shell.settings_sheet());
        let before = renders(&sheet, cx);
        cx.executor().advance_clock(Duration::from_secs(61));
        cx.run_until_parked();
        assert!(renders(&sheet, cx) > before);

        section(Section::Appearance, cx);
        let before = renders(&sheet, cx);
        cx.executor().advance_clock(Duration::from_secs(61));
        cx.run_until_parked();
        assert_eq!(renders(&sheet, cx), before, "no section to keep current");
    }

    #[gpui::test]
    fn the_chat_draws_the_lines_the_sheet_leaves_in_view(cx: &mut TestAppContext) {
        let (shell, cx) = window_with_keys(cx);
        cx.simulate_resize(size(px(1300.), px(1400.)));
        cx.run_until_parked();
        (0..100).for_each(|_| line_arrives(&shell, cx));
        let chat = shell.read_with(cx, |shell, _| shell.chat());
        let in_view = chat.read_with(cx, |chat, _| chat.lines_drawn());
        assert!(in_view > 30, "the window is tall: {in_view}");

        open(&shell, cx);
        line_arrives(&shell, cx);
        let beside_the_sheet = chat.read_with(cx, |chat, _| chat.lines_drawn());
        assert!(beside_the_sheet > 0, "above and below it, lines still show");
        assert!(
            beside_the_sheet < in_view,
            "{beside_the_sheet} of {in_view}"
        );

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(chat.read_with(cx, |chat, _| chat.lines_drawn()), in_view);
    }

    #[gpui::test]
    fn the_chat_measures_only_what_it_draws_beside_the_sheet(cx: &mut TestAppContext) {
        let (shell, cx) = window_with_keys(cx);
        (0..100).for_each(|_| line_arrives(&shell, cx));
        open(&shell, cx);
        line_arrives(&shell, cx);
        let chat = shell.read_with(cx, |shell, _| shell.chat());
        let before = chat.read_with(cx, |chat, _| chat.lines_measured());
        line_arrives(&shell, cx);
        let (measured, drawn) = chat.read_with(cx, |chat, _| {
            (chat.lines_measured() - before, chat.lines_drawn())
        });
        assert!(measured <= drawn + 1, "{measured} measured, {drawn} drawn");
    }

    /// What's known of a line's height can't outlive the size it was
    /// measured at, drawn or not.
    #[gpui::test]
    fn a_new_chat_size_measures_the_lines_under_the_sheet_again(cx: &mut TestAppContext) {
        let (shell, cx) = window_with_keys(cx);
        cx.simulate_resize(size(px(1300.), px(1400.)));
        cx.run_until_parked();
        (0..100).for_each(|_| line_arrives(&shell, cx));
        open(&shell, cx);
        section(Section::Appearance, cx);
        line_arrives(&shell, cx);
        let chat = shell.read_with(cx, |shell, _| shell.chat());
        let (before, drawn) =
            chat.read_with(cx, |chat, _| (chat.lines_measured(), chat.lines_drawn()));

        click("chat-size-larger", cx);
        let measured = chat.read_with(cx, |chat, _| chat.lines_measured()) - before;
        assert!(
            measured > drawn,
            "{measured} measured, {drawn} drawn before"
        );
    }

    #[gpui::test]
    fn every_section_shows_something(cx: &mut TestAppContext) {
        let (shell, cx) = window_with_keys(cx);
        open(&shell, cx);
        for section_shown in Section::ALL {
            section(section_shown, cx);
            let rows = shell.read_with(cx, |shell, cx| {
                shell.settings_sheet().read(cx).scroll.children_count()
            });
            assert!(rows >= 1, "{section_shown:?} is empty");
        }
    }

    #[gpui::test]
    fn the_keyboard_stays_in_the_sheet_and_reaches_every_stop(cx: &mut TestAppContext) {
        let (shell, cx) = window_with_keys(cx);
        open(&shell, cx);
        let stops = shell.read_with(cx, |shell, cx| {
            let sheet = shell.settings_sheet().read(cx);
            sheet.sheet_stops().len() + sheet.stops.in_use().len()
        });
        assert!(stops > 8, "Appearance has controls: {stops}");
        let focused_in_sheet = |shell: &Entity<Shell>, cx: &mut VisualTestContext| {
            let sheet = shell.read_with(cx, |shell, _| shell.settings_sheet());
            cx.update(|window, cx| {
                let sheet = sheet.read(cx);
                (sheet.sheet_stops().into_iter())
                    .chain(sheet.stops.in_use())
                    .any(|stop| stop.is_focused(window))
            })
        };
        for _ in 0..stops * 2 {
            cx.simulate_keystrokes("tab");
            assert!(focused_in_sheet(&shell, cx));
        }
        cx.simulate_keystrokes("shift-tab");
        assert!(focused_in_sheet(&shell, cx));
    }

    #[gpui::test]
    fn a_stop_below_the_fold_scrolls_into_view(cx: &mut TestAppContext) {
        let (shell, cx) = window_with_keys(cx);
        cx.simulate_resize(size(px(700.), px(300.)));
        open(&shell, cx);
        cx.run_until_parked();
        let sheet = shell.read_with(cx, |shell, _| shell.settings_sheet());
        // Back from the Appearance tab, round the tabs, to the pane's last stop.
        cx.simulate_keystrokes("shift-tab shift-tab shift-tab shift-tab");
        cx.run_until_parked();
        assert!(sheet.read_with(cx, |sheet, _| sheet.scroll.offset().y < px(0.)));
    }

    #[gpui::test]
    fn the_arrow_keys_scroll_a_long_section_and_stop_at_its_ends(cx: &mut TestAppContext) {
        let (shell, cx) = window_with_keys(cx);
        cx.simulate_resize(size(px(700.), px(420.)));
        open(&shell, cx);
        section(Section::Shortcuts, cx);
        let sheet = shell.read_with(cx, |shell, _| shell.settings_sheet());
        let offset =
            |cx: &mut VisualTestContext| sheet.read_with(cx, |sheet, _| sheet.scroll.offset().y);

        cx.simulate_keystrokes("down");
        cx.run_until_parked();
        assert_eq!(offset(cx), -SCROLL_STEP);
        (0..40).for_each(|_| cx.simulate_keystrokes("pagedown"));
        cx.run_until_parked();
        let lowest = sheet.read_with(cx, |sheet, _| -sheet.scroll.max_offset().y);
        assert_eq!(offset(cx), lowest);
        (0..40).for_each(|_| cx.simulate_keystrokes("pageup"));
        cx.run_until_parked();
        assert_eq!(offset(cx), px(0.));
    }

    #[gpui::test]
    fn what_is_behind_the_sheet_stays_as_it_is(cx: &mut TestAppContext) {
        let (shell, cx) = window_with_keys(cx);
        open(&shell, cx);
        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-1"
        } else {
            "ctrl-1"
        });
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("left-dock")
                .is_none_or(|dock| dock.size.width == px(0.))
        );
        cx.simulate_click(point(px(4.), px(4.)), Modifiers::none());
        cx.run_until_parked();
        assert!(cx.debug_bounds("settings-sheet").is_none());
    }

    #[gpui::test]
    fn the_shortcut_to_the_command_bar_closes_the_sheet(cx: &mut TestAppContext) {
        let (shell, cx) = window_with_keys(cx);
        open(&shell, cx);
        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-l"
        } else {
            "ctrl-l"
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("settings-sheet").is_none());
        cx.simulate_input("look");
        assert_eq!(shell.read_with(cx, |shell, cx| shell.typed(cx)), "look");
    }

    #[gpui::test]
    fn a_theme_applies_at_once_and_holds_when_the_os_changes(cx: &mut TestAppContext) {
        let (shell, cx) = window_with_keys(cx);
        open(&shell, cx);
        section(Section::Appearance, cx);
        click("theme-Light", cx);
        assert_eq!(chosen(cx).theme, ThemeChoice::Light);
        let parchment = Theme::for_appearance(WindowAppearance::Light).background;
        cx.update(|_, cx| assert_eq!(cx.global::<Theme>().background, parchment));

        click("theme-System", cx);
        assert_eq!(chosen(cx).theme, ThemeChoice::System);
    }

    #[gpui::test]
    fn the_size_applies_at_once_and_the_80_column_floor_follows_it(cx: &mut TestAppContext) {
        let (shell, cx) = window_with_keys(cx);
        cx.simulate_resize(size(px(1300.), px(800.)));
        open(&shell, cx);
        section(Section::Appearance, cx);
        let floor = cx.update(|_, cx| min_column_width(cx));
        (0..LARGEST_CHAT_SIZE).for_each(|_| click("chat-size-larger", cx));
        assert_eq!(chosen(cx).font_size, LARGEST_CHAT_SIZE);
        let larger = cx.update(|_, cx| min_column_width(cx));
        assert!(larger > floor, "{larger:?} after {floor:?}");

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        let column = cx.debug_bounds("chat-area").unwrap().size.width;
        assert!(column >= larger, "{column:?} under {larger:?}");
        assert!(larger > COLUMN_PADDING * 2.);
    }

    #[gpui::test]
    fn the_hud_goes_where_it_is_put_and_hiding_it_turns_prompts_on(cx: &mut TestAppContext) {
        let (shell, cx) = window_with_keys(cx);
        cx.simulate_resize(size(px(1300.), px(1400.)));
        open(&shell, cx);
        section(Section::Appearance, cx);
        assert_eq!(chosen(cx).hud, HudPlacement::Attached);
        assert!(!chosen(cx).shows_prompts());

        click("hud-Top strip", cx);
        assert_eq!(chosen(cx).hud, HudPlacement::TopStrip);
        click("show-prompts", cx);
        assert!(chosen(cx).show_prompts && chosen(cx).shows_prompts());
        click("show-prompts", cx);
        assert!(!chosen(cx).shows_prompts());

        click("hud-Hidden", cx);
        assert!(chosen(cx).shows_prompts());
        click("show-prompts", cx);
        assert!(chosen(cx).shows_prompts(), "they stay on while it's hidden");
        assert!(!chosen(cx).show_prompts, "and the player's choice stands");
    }

    #[gpui::test]
    fn a_choice_made_in_the_sheet_is_there_on_the_next_launch(cx: &mut TestAppContext) {
        let folder = TempDir::new().unwrap();
        let (shell, cx) = window_with_keys(cx);
        let (saved, kept) = SavedSettings::open(folder.path()).unwrap();
        shell.update_in(cx, |shell, window, cx| {
            shell.restore_settings(saved, kept, window, cx);
        });
        open(&shell, cx);
        section(Section::Appearance, cx);
        click("theme-Dark", cx);
        click("chat-size-larger", cx);

        let left = shell.update(cx, |shell, cx| shell.leave(cx));
        left.expect("the first leave").detach();
        cx.run_until_parked();
        let (_saved, next) = SavedSettings::open(folder.path()).unwrap();
        assert_eq!(next, chosen(cx));
        assert_eq!(next.theme, ThemeChoice::Dark);
    }

    fn rules() -> RulesState {
        RulesState {
            server: "mud.example".to_owned(),
            character: "ana".to_owned(),
            triggers: vec![TriggerRule {
                id: "t1".to_owned(),
                ..TriggerRule::new("bat")
            }],
            hotbar: vec!["look".to_owned(), "score".to_owned()],
            ..RulesState::default()
        }
    }

    /// Opens Triggers & aliases, keeping what it asks the session to do.
    fn open_rules(
        cx: &mut TestAppContext,
    ) -> (
        Entity<Shell>,
        &mut VisualTestContext,
        Rc<RefCell<Vec<RuleEdit>>>,
    ) {
        let (shell, cx) = window_with_keys(cx);
        cx.simulate_resize(size(px(1300.), px(800.)));
        let edits = Rc::new(RefCell::new(Vec::new()));
        let kept = Rc::clone(&edits);
        let sheet = shell.read_with(cx, |shell, _| shell.settings_sheet());
        cx.update(|_, cx| {
            cx.subscribe(&sheet, move |_, event, _| {
                if let SheetEvent::Edit(edit) = event {
                    kept.borrow_mut().push(edit.clone());
                }
            })
            .detach();
        });
        shell.update(cx, |shell, cx| shell.show_sample_rules(rules(), cx));
        open(&shell, cx);
        section(Section::Triggers, cx);
        (shell, cx, edits)
    }

    #[gpui::test]
    fn with_no_game_the_section_says_what_it_needs(cx: &mut TestAppContext) {
        let (shell, cx) = window_with_keys(cx);
        open(&shell, cx);
        section(Section::Triggers, cx);
        assert!(cx.debug_bounds("new-trigger").is_none());
    }

    #[gpui::test]
    fn a_new_trigger_saves_what_the_form_says(cx: &mut TestAppContext) {
        let (_shell, cx, edits) = open_rules(cx);
        click("new-trigger", cx);
        cx.simulate_input("tells you");
        click("Highlight-Purple", cx);
        click("In the chat-Hide the line", cx);
        click("Applies to-This character", cx);
        click("form-save", cx);

        let edits = edits.borrow();
        let [RuleEdit::SaveTrigger(saved)] = edits.as_slice() else {
            panic!("one save, not {edits:?}");
        };
        assert_eq!(saved.pattern, "tells you");
        assert_eq!(saved.color, Some(Highlight::Purple));
        assert!(saved.hides);
        assert_eq!(saved.scope, Scope::Character("ana".to_owned()));
        assert!(cx.debug_bounds("form-save").is_none(), "the form closes");
    }

    /// A session on the desktop has no say in who is playing until the game
    /// names them, so a rule can't be for "this character" before that.
    #[gpui::test]
    fn this_character_is_offered_once_the_game_has_named_them(cx: &mut TestAppContext) {
        let (shell, cx, _edits) = open_rules(cx);
        let unnamed = RulesState {
            character: String::new(),
            ..rules()
        };
        shell.update(cx, |shell, cx| shell.show_sample_rules(unnamed, cx));
        click("new-trigger", cx);
        assert!(cx.debug_bounds("Applies to-All characters").is_some());
        assert!(cx.debug_bounds("Applies to-This server").is_some());
        assert!(cx.debug_bounds("Applies to-This character").is_none());

        shell.update(cx, |shell, cx| shell.show_sample_rules(rules(), cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("Applies to-This character").is_some());
    }

    #[gpui::test]
    fn a_bad_regular_expression_stays_in_the_form_and_says_why(cx: &mut TestAppContext) {
        let (_shell, cx, edits) = open_rules(cx);
        click("new-trigger", cx);
        cx.simulate_input("rat(");
        click("Matches-Regular expression", cx);
        click("form-save", cx);
        assert!(edits.borrow().is_empty());
        assert!(cx.debug_bounds("form-save").is_some());
    }

    #[gpui::test]
    fn return_in_a_field_saves_and_tab_walks_on_inside_the_sheet(cx: &mut TestAppContext) {
        let (shell, cx, edits) = open_rules(cx);
        click("new-alias", cx);
        cx.simulate_input("k");
        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        cx.simulate_input("kill $1");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        let edits = edits.borrow();
        let [RuleEdit::SaveAlias(saved)] = edits.as_slice() else {
            panic!("one save, not {edits:?}");
        };
        assert_eq!(
            (saved.name.as_str(), saved.expansion.as_str()),
            ("k", "kill $1")
        );
        assert_eq!(shell.read_with(cx, |shell, cx| shell.typed(cx)), "");
    }

    #[gpui::test]
    fn a_rule_turns_off_and_comes_out_from_its_row(cx: &mut TestAppContext) {
        let (_shell, cx, edits) = open_rules(cx);
        click("trigger-on-0", cx);
        click("trigger-remove-0", cx);
        let edits = edits.borrow();
        let [RuleEdit::SaveTrigger(off), RuleEdit::RemoveTrigger(id)] = edits.as_slice() else {
            panic!("a save and a remove, not {edits:?}");
        };
        assert!(!off.enabled);
        assert_eq!(id, "t1");
    }

    #[gpui::test]
    fn chips_reorder_and_come_out_here_too(cx: &mut TestAppContext) {
        let (_shell, cx, edits) = open_rules(cx);
        click("chip-Later-0", cx);
        click("chip-remove-1", cx);
        let chips = |edit: &RuleEdit| match edit {
            RuleEdit::SetHotbar(chips) => chips.clone(),
            other => panic!("{other:?}"),
        };
        let edits = edits.borrow();
        assert_eq!(chips(&edits[0]), ["score", "look"]);
        assert_eq!(chips(&edits[1]), ["look"]);
    }

    /// Applies what the sheet asked for, as the session's store would.
    fn echo_chips(
        shell: &Entity<Shell>,
        edits: &Rc<RefCell<Vec<RuleEdit>>>,
        cx: &mut VisualTestContext,
    ) {
        let Some(RuleEdit::SetHotbar(hotbar)) = edits.borrow().last().cloned() else {
            panic!("a hotbar edit");
        };
        let echoed = RulesState { hotbar, ..rules() };
        shell.update(cx, |shell, cx| shell.show_sample_rules(echoed, cx));
        cx.run_until_parked();
    }

    #[gpui::test]
    fn a_chip_moved_under_its_form_is_the_one_saved(cx: &mut TestAppContext) {
        let (shell, cx, edits) = open_rules(cx);
        click("chip-edit-1", cx);
        click("chip-Earlier-1", cx);
        echo_chips(&shell, &edits, cx);
        click("Hotbar command", cx);
        cx.simulate_input(" all");
        click("form-save", cx);
        echo_chips(&shell, &edits, cx);
        let chips = shell.read_with(cx, |shell, cx| {
            shell.settings_sheet().read(cx).rules.hotbar.clone()
        });
        assert_eq!(chips, ["score all", "look"]);
    }

    #[gpui::test]
    fn a_chip_removed_under_its_form_stays_removed(cx: &mut TestAppContext) {
        let (shell, cx, edits) = open_rules(cx);
        click("chip-edit-1", cx);
        click("chip-remove-1", cx);
        echo_chips(&shell, &edits, cx);
        let before = edits.borrow().len();
        click("form-save", cx);
        assert_eq!(edits.borrow().len(), before, "nothing came back");
        assert!(cx.debug_bounds("form-save").is_none(), "the form closes");
    }

    #[gpui::test]
    fn a_trigger_switched_under_its_form_stays_switched(cx: &mut TestAppContext) {
        let (shell, cx, edits) = open_rules(cx);
        click("trigger-edit-0", cx);
        let off = RulesState {
            triggers: vec![TriggerRule {
                enabled: false,
                ..rules().triggers[0].clone()
            }],
            ..rules()
        };
        shell.update(cx, |shell, cx| shell.show_sample_rules(off, cx));
        cx.run_until_parked();
        click("form-save", cx);
        let edits = edits.borrow();
        let [RuleEdit::SaveTrigger(saved)] = edits.as_slice() else {
            panic!("one save, not {edits:?}");
        };
        assert!(!saved.enabled);
    }

    #[gpui::test]
    fn opening_the_form_brings_it_into_view(cx: &mut TestAppContext) {
        let (shell, cx, _edits) = open_rules(cx);
        let many = RulesState {
            triggers: (0..40)
                .map(|number| TriggerRule {
                    id: number.to_string(),
                    ..TriggerRule::new(format!("line {number}"))
                })
                .collect(),
            ..rules()
        };
        shell.update(cx, |shell, cx| shell.show_sample_rules(many, cx));
        cx.run_until_parked();
        let sheet = shell.read_with(cx, |shell, _| shell.settings_sheet());
        assert_eq!(
            sheet.read_with(cx, |sheet, _| sheet.scroll.offset().y),
            px(0.)
        );

        click("trigger-edit-0", cx);
        cx.run_until_parked();
        let (offset, pane) = sheet.read_with(cx, |sheet, _| {
            (sheet.scroll.offset().y, sheet.scroll.bounds().size.height)
        });
        assert!(offset < -pane, "scrolled past the first page: {offset:?}");
    }

    #[gpui::test]
    fn the_keyboard_reaches_every_control_of_the_section(cx: &mut TestAppContext) {
        let (shell, cx, _edits) = open_rules(cx);
        click("new-trigger", cx);
        let sheet = shell.read_with(cx, |shell, _| shell.settings_sheet());
        let (stops, field) = sheet.read_with(cx, |sheet, cx| {
            (
                sheet.sheet_stops().len() + sheet.stops.in_use().len(),
                sheet.form_field_handle(cx),
            )
        });
        let mut reached_field = 0;
        for _ in 0..stops {
            cx.simulate_keystrokes("tab");
            cx.run_until_parked();
            if cx.update(|window, _| field.is_focused(window)) {
                reached_field += 1;
            }
        }
        assert_eq!(reached_field, 1, "the field is one stop of {stops}");
    }

    #[gpui::test]
    fn problems_show_on_the_sprocket_and_open_the_plugins_section(cx: &mut TestAppContext) {
        let (shell, cx) = window_with_keys(cx);
        assert!(cx.debug_bounds("settings-badge").is_none());
        let disabled = PluginDisabled {
            plugin: PluginId::try_from("a-plugin".to_owned()).unwrap(),
            reason: DisabledReason::Overran(3),
        };
        let settings = SettingsState {
            server: Some("mud.example".to_owned()),
            turned_off: vec![disabled.clone()],
            ..SettingsState::default()
        };
        shell.update(cx, |shell, cx| shell.show_sample_problems(&settings, cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("settings-badge").is_some());

        click("open-settings", cx);
        assert!(cx.debug_bounds("notice-0").is_some());
        click("notice-details-0", cx);
        let open = shell.read_with(cx, |shell, cx| {
            shell.settings_sheet().read(cx).details.clone()
        });
        assert_eq!(open, Some(disabled.to_string()));
    }

    #[gpui::test]
    fn keep_syncing_leaves_the_sheet_the_keyboard(cx: &mut TestAppContext) {
        let (shell, cx) = window_with_keys(cx);
        open(&shell, cx);
        section(Section::Data, cx);
        let sheet = shell.read_with(cx, |shell, _| shell.settings_sheet());
        sheet.update(cx, |sheet, cx| sheet.show_sample_data(cx));
        cx.run_until_parked();
        click("stop-sync", cx);
        click("keep-sync", cx);
        assert!(cx.debug_bounds("keep-sync").is_none());
        let on_tab =
            cx.update(|window, cx| sheet.read(cx).tabs[Section::Data as usize].is_focused(window));
        assert!(on_tab, "focus stays in the sheet");
    }

    #[gpui::test]
    fn a_server_waiting_on_the_vault_opens_the_vault_over_any_problems(cx: &mut TestAppContext) {
        let (shell, cx) = window_with_keys(cx);
        let settings = SettingsState {
            notices: vec!["A notice".to_owned()],
            ..SettingsState::default()
        };
        shell.update(cx, |shell, cx| shell.show_sample_problems(&settings, cx));
        let sheet = shell.read_with(cx, |shell, _| shell.settings_sheet());
        sheet.update(cx, |sheet, cx| sheet.wait_for_unlock("mud.example", cx));
        open(&shell, cx);
        assert_eq!(
            sheet.read_with(cx, |sheet, _| sheet.section),
            Section::Vault
        );
    }

    #[gpui::test]
    fn turn_on_again_asks_for_the_plugin_it_names(cx: &mut TestAppContext) {
        let (shell, cx) = window_with_keys(cx);
        let plugin = PluginId::try_from("a-plugin".to_owned()).unwrap();
        let settings = SettingsState {
            turned_off: vec![PluginDisabled {
                plugin: plugin.clone(),
                reason: DisabledReason::Overran(3),
            }],
            ..SettingsState::default()
        };
        shell.update(cx, |shell, cx| shell.show_sample_problems(&settings, cx));
        cx.run_until_parked();
        let asked = Rc::new(RefCell::new(None));
        let sheet = shell.read_with(cx, |shell, _| shell.settings_sheet());
        let keep = Rc::clone(&asked);
        let _watching = cx.update(|_, cx| {
            cx.subscribe(&sheet, move |_, event: &SheetEvent, _| {
                if let SheetEvent::TurnOn(plugin) = event {
                    *keep.borrow_mut() = Some(plugin.clone());
                }
            })
        });

        click("open-settings", cx);
        click("notice-turn-on-0", cx);
        assert_eq!(*asked.borrow(), Some(plugin));
    }

    #[gpui::test]
    fn tab_brings_the_last_switch_into_view_however_many_plugins_there_are(
        cx: &mut TestAppContext,
    ) {
        let (shell, cx) = window_with_keys(cx);
        cx.simulate_resize(size(px(1000.), px(500.)));
        let sheet = shell.read_with(cx, |shell, _| shell.settings_sheet());
        let plugins = crate::measure::sample_plugins();
        assert!(plugins.len() >= 7);
        sheet.update(cx, |sheet, cx| {
            sheet.show_plugins(plugins, cx);
            sheet.select(Section::Plugins, cx);
        });
        open(&shell, cx);

        // Back from the Plugins tab, round the tabs, to the pane's last stop.
        cx.simulate_keystrokes("shift-tab shift-tab");
        cx.run_until_parked();

        let switch = cx.debug_bounds("switch-sample-vitals").expect("drawn");
        let sheet_bounds = cx.debug_bounds("settings-sheet").expect("open");
        assert!(
            switch.bottom() <= sheet_bounds.bottom() && switch.top() >= sheet_bounds.top(),
            "{switch:?} is outside {sheet_bounds:?}"
        );
    }

    /// The approval-related events the sheet sends, said in a few words.
    fn approval_events(
        sheet: &Entity<SettingsSheet>,
        cx: &mut VisualTestContext,
    ) -> (Rc<RefCell<Vec<String>>>, gpui::Subscription) {
        let said = Rc::new(RefCell::new(Vec::new()));
        let keep = Rc::clone(&said);
        let watching = cx.update(|_, cx| {
            cx.subscribe(sheet, move |_, event: &SheetEvent, _| {
                let words = match event {
                    SheetEvent::SetPlugin { plugin, on, .. } => format!("{plugin} on: {on}"),
                    SheetEvent::SetApproved { plugin, approved } => {
                        format!("{plugin} approved: {approved}")
                    }
                    _ => return,
                };
                keep.borrow_mut().push(words);
            })
        });
        (said, watching)
    }

    /// The Plugins section of a session on mud.example, its waiting plugin off.
    fn plugins_with_one_waiting(
        cx: &mut TestAppContext,
    ) -> (Entity<SettingsSheet>, &mut VisualTestContext) {
        let (shell, cx) = window_with_keys(cx);
        let sheet = shell.read_with(cx, |shell, _| shell.settings_sheet());
        // Few enough that every switch is in view, so a click lands on it.
        let mut plugins = crate::measure::sample_plugins();
        plugins.retain(|row| row.sending != Sending::No);
        for row in &mut plugins {
            row.on &= row.sending != Sending::Waiting;
        }
        let session = SettingsState {
            server: Some("mud.example".to_owned()),
            ..SettingsState::default()
        };
        sheet.update(cx, |sheet, cx| {
            sheet.show_plugins(plugins, cx);
            sheet.show_session(&session, cx);
            sheet.select(Section::Plugins, cx);
        });
        open(&shell, cx);
        (sheet, cx)
    }

    #[gpui::test]
    fn turning_on_a_plugin_waiting_for_approval_asks_first(cx: &mut TestAppContext) {
        let (sheet, cx) = plugins_with_one_waiting(cx);
        let (said, _watching) = approval_events(&sheet, cx);

        click("switch-sample-chat", cx);
        assert!(cx.debug_bounds("approval").is_some());
        assert!(
            said.borrow().is_empty(),
            "nothing's saved until the player answers"
        );
        click("approval-without", cx);
        assert!(cx.debug_bounds("approval").is_none());
        assert_eq!(*said.borrow(), ["sample-chat on: true"]);

        click("switch-sample-chat", cx);
        click("switch-sample-chat", cx);
        click("approval-allow", cx);
        assert_eq!(
            said.borrow()[1..],
            [
                "sample-chat on: false",
                "sample-chat approved: true",
                "sample-chat on: true"
            ]
        );
    }

    #[gpui::test]
    fn an_approval_is_taken_back_from_its_row_and_cancel_changes_nothing(cx: &mut TestAppContext) {
        let (sheet, cx) = plugins_with_one_waiting(cx);
        let (said, _watching) = approval_events(&sheet, cx);

        click("approval-sample-helper", cx);
        assert_eq!(*said.borrow(), ["sample-helper approved: false"]);
        let sending = |cx: &mut VisualTestContext| {
            sheet.read_with(cx, |sheet, _| {
                let row = sheet
                    .plugins
                    .iter()
                    .find(|row| row.id.as_str() == "sample-helper");
                row.unwrap().sending
            })
        };
        assert_eq!(sending(cx), Sending::Waiting);

        click("approval-sample-helper", cx);
        assert!(cx.debug_bounds("approval").is_some(), "Review asks again");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(cx.debug_bounds("approval").is_none());
        assert!(
            cx.debug_bounds("settings-sheet").is_some(),
            "Esc leaves the sheet open"
        );
        assert_eq!(said.borrow().len(), 1);
        assert_eq!(sending(cx), Sending::Waiting);
    }

    #[gpui::test]
    fn remove_takes_a_plugin_out_and_says_it_applies_at_the_next_connect(cx: &mut TestAppContext) {
        let (sheet, cx) = plugins_with_one_waiting(cx);
        let removed = Rc::new(RefCell::new(None));
        let keep = Rc::clone(&removed);
        let _watching = cx.update(|_, cx| {
            cx.subscribe(&sheet, move |_, event: &SheetEvent, _| {
                if let SheetEvent::Remove { plugin, folder } = event {
                    *keep.borrow_mut() = Some(format!("{plugin} from {folder}"));
                }
            })
        });
        assert!(cx.debug_bounds("reconnect-note").is_none());

        click("remove-sample-helper", cx);
        assert!(removed.borrow().is_none(), "it asks first");
        click("approval-cancel", cx);
        assert!(removed.borrow().is_none());
        click("remove-sample-helper", cx);
        click("approval-allow", cx);
        assert_eq!(
            removed.borrow().as_deref(),
            Some("sample-helper from sample-helper")
        );
        sheet.update(cx, |sheet, cx| {
            let mut rows = sheet.plugins.clone();
            rows.retain(|row| row.id.as_str() != "sample-helper");
            sheet.refresh_plugins(rows, true, cx);
        });
        cx.run_until_parked();

        assert!(cx.debug_bounds("remove-sample-helper").is_none());
        assert!(cx.debug_bounds("reconnect-note").is_some());
        assert!(sheet.read_with(cx, |sheet, _| sheet.reconnect_to_apply()));
    }

    #[gpui::test]
    fn a_switch_flipped_and_flipped_back_leaves_nothing_to_reconnect_for(cx: &mut TestAppContext) {
        let (sheet, cx) = plugins_with_one_waiting(cx);
        let flip = |cx: &mut VisualTestContext| {
            sheet.update(cx, |sheet, cx| {
                let row = (sheet.plugins.iter())
                    .find(|row| row.sending == Sending::Approved)
                    .expect("one is let to send")
                    .clone();
                sheet.set_plugin("mud.example".into(), row.id, !row.on, cx);
            });
            sheet.read_with(cx, |sheet, _| sheet.reconnect_to_apply())
        };

        assert!(flip(cx), "a change waits for the next connect");
        assert!(!flip(cx), "flipped back, nothing changed");
    }

    #[gpui::test]
    fn a_copy_that_arrived_is_reviewed_and_trusted_only_as_the_player_chooses(
        cx: &mut TestAppContext,
    ) {
        let (sheet, cx) = plugins_with_one_waiting(cx);
        sheet.update(cx, |sheet, cx| {
            let mut rows = sheet.plugins.clone();
            let arrived = rows.iter_mut().find(|row| row.id.as_str() == "sample-chat");
            arrived.unwrap().held_back = Some("sha256:shown".into());
            sheet.refresh_plugins(rows, false, cx);
        });
        let said = Rc::new(RefCell::new(Vec::new()));
        let keep = Rc::clone(&said);
        let _watching = cx.update(|_, cx| {
            cx.subscribe(&sheet, move |_, event: &SheetEvent, _| match event {
                SheetEvent::ReviewArrival { folder, hash } => {
                    keep.borrow_mut().push(format!("review {folder} {hash}"))
                }
                SheetEvent::Trust { folder, hash } => {
                    keep.borrow_mut().push(format!("trust {folder} {hash}"))
                }
                _ => {}
            })
        });
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("switch-sample-chat").is_none(),
            "nothing to switch"
        );

        // A held-back copy's elements are told by its folder too.
        click("arrival-sample-chat-in-sample-chat", cx);
        assert_eq!(*said.borrow(), ["review sample-chat sha256:shown"]);
        // The directory didn't vouch for it, so the shell asks the player.
        sheet.update_in(cx, |sheet, window, cx| {
            let why = "The directory doesn't list this copy.".to_owned();
            sheet.ask_to_trust("sample-chat", "sha256:shown", why, window, cx);
        });
        cx.run_until_parked();
        click("approval-allow", cx);

        assert_eq!(
            *said.borrow(),
            [
                "review sample-chat sha256:shown",
                "trust sample-chat sha256:shown"
            ]
        );
    }

    #[gpui::test]
    fn a_link_check_ends_only_with_its_own_result(cx: &mut TestAppContext) {
        let (sheet, cx) = plugins_with_one_waiting(cx);
        sheet.update_in(cx, |sheet, window, cx| {
            sheet.open_add_plugin(window, cx);
            sheet.add.checking = true;
            // A directory install that ends meanwhile, either way.
            sheet.show_installed(std::collections::BTreeMap::new(), Vec::new(), cx);
            sheet.install_ended(Some("Other wasn't installed.".to_owned()), cx);
            assert!(sheet.add.checking, "the link is still being checked");

            let why = "The plugin at x wasn't added: it failed a check.".to_owned();
            sheet.link_checked(Err(why.clone()), window, cx);
            assert!(!sheet.add.checking);
            assert_eq!(sheet.add.problem.as_deref(), Some(why.as_str()));
        });
    }

    #[gpui::test]
    fn removing_an_added_folder_asks_then_forgets_only_that_folder(cx: &mut TestAppContext) {
        let (sheet, cx) = plugins_with_one_waiting(cx);
        sheet.update_in(cx, |sheet, window, cx| {
            sheet.open_add_plugin(window, cx);
            sheet.show_sample_added(cx);
        });
        let said = Rc::new(RefCell::new(Vec::new()));
        let keep = Rc::clone(&said);
        let _watching = cx.update(|_, cx| {
            cx.subscribe(&sheet, move |_, event: &SheetEvent, _| {
                if let SheetEvent::ForgetFolder { plugin, path } = event {
                    let plugin = plugin.as_ref().map(PluginId::as_str);
                    keep.borrow_mut()
                        .push(format!("forget {plugin:?} {}", path.display()));
                }
            })
        });
        cx.run_until_parked();

        click("remove-folder-/Users/sample/plugins/prompts", cx);
        assert!(said.borrow().is_empty(), "it asks first");
        click("approval-allow", cx);

        assert_eq!(
            *said.borrow(),
            ["forget Some(\"prompts\") /Users/sample/plugins/prompts"]
        );
    }

    #[gpui::test]
    fn a_copy_no_longer_shown_is_not_asked_about(cx: &mut TestAppContext) {
        let (sheet, cx) = plugins_with_one_waiting(cx);
        sheet.update(cx, |sheet, cx| {
            let mut rows = sheet.plugins.clone();
            let arrived = rows.iter_mut().find(|row| row.id.as_str() == "sample-chat");
            arrived.unwrap().held_back = Some("sha256:newer".into());
            sheet.refresh_plugins(rows, false, cx);
        });

        // The review was for the copy shown before a sync replaced it.
        sheet.update_in(cx, |sheet, window, cx| {
            let why = "The directory doesn't list this copy.".to_owned();
            sheet.ask_to_trust("sample-chat", "sha256:shown", why, window, cx);
        });
        cx.run_until_parked();

        assert!(cx.debug_bounds("approval").is_none());
    }

    #[gpui::test]
    fn a_plugin_refused_at_load_says_why_on_the_plugins_page(cx: &mut TestAppContext) {
        let (sheet, cx) = plugins_with_one_waiting(cx);
        sheet.update(cx, |sheet, cx| {
            let refused = Refused {
                id: "extras".to_owned(),
                why: "its files aren't the ones the directory signed for".to_owned(),
            };
            sheet.show_refused(vec![refused], cx);
        });
        cx.run_until_parked();

        assert!(cx.debug_bounds("plugin-refused").is_some());
    }

    #[gpui::test]
    fn a_directory_that_did_not_check_out_lists_nothing(cx: &mut TestAppContext) {
        let (shell, cx) = window_with_keys(cx);
        shell.update_in(cx, |shell, window, cx| {
            crate::measure::open_sample_directory(shell, window, cx);
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("directory-get-aardwolf").is_some());

        let sheet = shell.read_with(cx, |shell, _| shell.settings_sheet());
        sheet.update(cx, |sheet, cx| {
            let why = mudling_core::registry::IndexError::Forged
                .to_string()
                .into();
            sheet.show_directory(Listing::Refused(why), Default::default(), cx);
        });
        cx.run_until_parked();

        assert!(cx.debug_bounds("directory-refused").is_some());
        assert!(cx.debug_bounds("directory-get-aardwolf").is_none());
    }
}
