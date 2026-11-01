//! The settings sheet: one modal sheet with seven sections, opened by the title
//! bar's sprocket or its shortcut.
//!
//! The sheet walks its own stops, because gpui's Tab order spans the window and
//! the sheet has to keep the keyboard inside it. Every control takes a stop
//! from [`Pane`], which remembers the row that holds it, so Tab can bring a
//! control scrolled out of view back in. The arrow and page keys scroll what
//! has no control to land on, like the list of shortcuts.

mod appearance;
mod plugins;
mod saved;
mod servers;
mod shortcuts;
mod triggers;

pub use plugins::PluginRow;
pub use saved::SavedSettings;
pub use servers::SavedServer;

use crate::assets::{HEADING_FONT, Icon, UI_FONT};
use crate::input;
use crate::shell::{FocusHotbar, PinCommand, ToggleLeftPanel, ToggleRightPanel};
use crate::theme::{Appearance, CHAT_FONTS, Theme};
use crate::title_bar::{FindInScrollback, OpenSettings, problems_text};
use gpui::{
    AnyElement, App, Context, Div, ElementId, EventEmitter, FocusHandle, FontWeight, Hsla,
    KeyBinding, MouseButton, Pixels, Role, ScrollHandle, SharedString, Stateful, Window, actions,
    div, point, prelude::*, px, transparent_black,
};
use mudling_core::plugin::luau::PluginDisabled;
use mudling_core::plugin::{PluginCost, PluginId};
use mudling_core::session::RuleEdit;
use mudling_core::store::RulesState;
use mudling_core::store::SettingsState;

const WIDTH: Pixels = px(1000.);
const HEIGHT: Pixels = px(716.);
/// The space kept around the sheet in a small window.
const MARGIN: Pixels = px(24.);
const NAV_WIDTH: Pixels = px(264.);
/// The most of a small sheet the sections take.
const NAV_SHARE: f32 = 0.36;
const CONTEXT: &str = "SettingsSheet";
const MAKER: &str = "A Vyttle app";
/// How far an arrow key scrolls.
const SCROLL_STEP: Pixels = px(48.);

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
    Vault,
}

impl Section {
    const ALL: [Section; 7] = [
        Section::Servers,
        Section::Plugins,
        Section::Triggers,
        Section::Appearance,
        Section::Shortcuts,
        Section::Data,
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
            Section::Vault => "Vault",
        }
    }

    /// The line for a section whose phase hasn't arrived.
    fn not_yet(self) -> Option<&'static str> {
        match self {
            Section::Data => Some("Sync isn't here yet. Your data stays on this device."),
            Section::Vault => Some("The password vault isn't here yet."),
            _ => None,
        }
    }
}

pub enum SheetEvent {
    Closed,
    Chose(Appearance),
    /// The player confirmed taking this host off their list.
    RemoveServer(String),
    TurnOn(PluginId),
    /// A change to the player's rules, for the session.
    Edit(RuleEdit),
    /// A sound was chosen: the player hears what they chose.
    TrySound(&'static str),
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
    label: &'static str,
    stop: &FocusHandle,
    theme: &Theme,
) -> Stateful<Div> {
    control(id, label, stop, theme)
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
    server: Option<SharedString>,
    servers: Vec<SavedServer>,
    /// What the last thing done to the saved servers ran into.
    servers_problem: Option<SharedString>,
    /// The host of the game running, which can't be removed.
    playing: Option<SharedString>,
    /// The host whose removal is being asked about.
    removing: Option<SharedString>,
    plugins: Vec<PluginRow>,
    rules: RulesState,
    form: triggers::Form,
    notices: Vec<String>,
    turned_off: Vec<PluginDisabled>,
    costs: Vec<PluginCost>,
    /// The notice whose details are showing.
    details: Option<String>,
    /// The chat faces this machine has.
    fonts: Vec<&'static str>,
    dialog: FocusHandle,
    tabs: [FocusHandle; Section::ALL.len()],
    close: FocusHandle,
    stops: Stops,
    /// A stop to bring into view at the next draw, once its row exists.
    reveal: Option<FocusHandle>,
    scroll: ScrollHandle,
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
            server: None,
            servers: Vec::new(),
            servers_problem: None,
            playing: None,
            removing: None,
            plugins: Vec::new(),
            rules: RulesState::default(),
            form: triggers::Form::new(window, cx),
            notices: Vec::new(),
            turned_off: Vec::new(),
            costs: Vec::new(),
            details: None,
            fonts: vec![CHAT_FONTS[0]],
            dialog: cx.focus_handle(),
            tabs: std::array::from_fn(|_| cx.focus_handle()),
            close: cx.focus_handle(),
            stops: Stops::default(),
            reveal: None,
            scroll: ScrollHandle::new(),
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
        self.removing = None;
        if self.problems() > 0 {
            self.select(Section::Plugins, cx);
        }
        window.focus(&self.tabs[self.section as usize], cx);
    }

    pub fn select(&mut self, section: Section, cx: &mut Context<SettingsSheet>) {
        self.section = section;
        self.scroll.set_offset(point(px(0.), px(0.)));
        cx.notify();
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

    pub fn show_playing(&mut self, host: Option<&str>, cx: &mut Context<SettingsSheet>) {
        if self.playing.as_deref() != host {
            self.playing = host.map(|host| host.to_owned().into());
            cx.notify();
        }
    }

    pub fn show_plugins(&mut self, plugins: Vec<PluginRow>, cx: &mut Context<SettingsSheet>) {
        self.plugins = plugins;
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
            || self.costs != settings.plugin_costs;
        if changed {
            self.server = server.map(|server| server.to_owned().into());
            self.notices.clone_from(&settings.notices);
            self.turned_off.clone_from(&settings.turned_off);
            self.costs.clone_from(&settings.plugin_costs);
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

    /// Moves `step` stops on, round the sheet, and brings the stop into view.
    fn walk(&mut self, step: isize, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        let fixed = self.tabs.len() + 1;
        let order: Vec<&FocusHandle> = (self.tabs.iter())
            .chain([&self.close])
            .chain(self.stops.in_use())
            .collect();
        let at = order.iter().position(|stop| stop.is_focused(window));
        let next = at.map_or(0, |at| {
            (at as isize + step).rem_euclid(order.len() as isize) as usize
        });
        window.focus(order[next], cx);
        if let Some(stop) = next.checked_sub(fixed) {
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
        let problems = self.problems();
        control(
            SharedString::from(format!("section-{}", section as usize)),
            section.title(),
            &self.tabs[section as usize],
            theme,
        )
        .role(Role::Tab)
        .aria_selected(chosen)
        .when(section == Section::Plugins && problems > 0, |tab| {
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
        .when(section == Section::Plugins && problems > 0, |tab| {
            tab.child(count_badge(problems, theme))
        })
    }

    fn render_nav(
        &self,
        width: Pixels,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) -> impl IntoElement + use<> {
        let tabs = Section::ALL.map(|section| self.render_tab(section, theme, cx));
        let version = format!(
            "MUDling {} · {}",
            env!("CARGO_PKG_VERSION"),
            env!("CARGO_PKG_LICENSE")
        );
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
            .child(
                div()
                    .id("settings-about")
                    .role(Role::Label)
                    .aria_value(format!("{version} · {MAKER}"))
                    .px_3()
                    .pb_2()
                    .text_size(px(13.))
                    .text_color(theme.text_muted)
                    .child(version)
                    .child(MAKER),
            )
    }

    fn render_pane(
        &mut self,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) -> impl IntoElement + use<> {
        let mut stops = std::mem::take(&mut self.stops);
        stops.start_frame();
        let mut pane = Pane {
            rows: Vec::new(),
            stops,
        };
        match self.section {
            Section::Servers => self.fill_servers(&mut pane, theme, cx),
            Section::Plugins => self.fill_plugins(&mut pane, theme, cx),
            Section::Triggers => self.fill_triggers_section(&mut pane, theme, cx),
            Section::Appearance => self.fill_appearance(&mut pane, theme, cx),
            Section::Shortcuts => shortcuts::fill(&mut pane, theme),
            section => pane.row(muted_line(section.not_yet().unwrap_or_default(), theme)),
        }
        if let Some(wanted) = self.reveal.take()
            && let Some(at) = pane.stops.in_use().iter().position(|stop| *stop == wanted)
        {
            self.scroll.scroll_to_item(pane.stops.rows[at]);
        }
        self.stops = pane.stops;
        let title = self.section.title();
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
                    .id("settings-title")
                    .role(Role::Heading)
                    .aria_label(title)
                    .aria_level(2)
                    .font_family(HEADING_FONT)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_size(px(19.))
                    .text_color(theme.text)
                    .child(title),
            )
            .child(
                control("settings-close", "Close settings", &self.close, theme)
                    .size(px(32.))
                    .on_click(cx.listener(|sheet, _, _, cx| sheet.close(cx)))
                    .child(Icon::WindowClose.svg().size(px(16.)).text_color(theme.text)),
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

impl gpui::Render for SettingsSheet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<SettingsSheet>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let room = window.viewport_size();
        let width = WIDTH.min(room.width - MARGIN * 2.);
        let scrim_click = cx.listener(|sheet, _, window, cx| {
            window.prevent_default();
            sheet.close(cx);
        });
        let dialog = div()
            .id("settings-sheet")
            .debug_selector(|| "settings-sheet".to_owned())
            .key_context(CONTEXT)
            .role(Role::Dialog)
            .aria_label("Settings")
            .a11y_synthetic_children(|builder| builder.parent_node().set_modal())
            .track_focus(&self.dialog)
            .occlude()
            .w(width)
            .h(HEIGHT.min(room.height - MARGIN * 2.))
            .flex()
            .overflow_hidden()
            .rounded_xl()
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface)
            .shadow(theme.floating_shadow())
            .font_family(UI_FONT)
            .text_color(theme.text)
            // Tab in a text field is a stop like any other here.
            .capture_action(cx.listener(|sheet, _: &input::Complete, window, cx| {
                sheet.walk(1, window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|sheet, _: &CloseSettings, _, cx| sheet.close(cx)))
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
            .on_action(|_: &PinCommand, _, _| {})
            .child(self.render_nav(NAV_WIDTH.min(width * NAV_SHARE), &theme, cx))
            .child(self.render_pane(&theme, cx));
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
    use super::*;
    use crate::chat::{COLUMN_PADDING, min_column_width};
    use crate::hud::HudPlacement;
    use crate::shell::{Shell, tests::window_with_keys};
    use crate::theme::{LARGEST_CHAT_SIZE, ThemeChoice};
    use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, WindowAppearance, size};
    use mudling_core::defaults::triggers::{Highlight, Scope, TriggerRule};
    use mudling_core::plugin::luau::DisabledReason;
    use std::cell::RefCell;
    use std::rc::Rc;
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

    #[gpui::test]
    fn seven_sections_each_show_something(cx: &mut TestAppContext) {
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
            sheet.tabs.len() + 1 + sheet.stops.in_use().len()
        });
        assert!(stops > 8, "Appearance has controls: {stops}");
        let focused_in_sheet = |shell: &Entity<Shell>, cx: &mut VisualTestContext| {
            let sheet = shell.read_with(cx, |shell, _| shell.settings_sheet());
            cx.update(|window, cx| {
                let sheet = sheet.read(cx);
                sheet
                    .tabs
                    .iter()
                    .chain([&sheet.close])
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
                sheet.tabs.len() + 1 + sheet.stops.in_use().len(),
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
}
