//! The window's own title bar: the session switcher, how the connection
//! stands, the panel toggles, search and settings. What isn't a control drags
//! the window.
//!
//! A press on a control leaves focus where it was, so the player's typing
//! stays in the command bar. From the keyboard, Shift-Tab goes from the
//! command bar to the controls, Tab and Shift-Tab walk them, and Esc goes back.
//!
//! macOS draws the traffic lights over the bar's left end. Windows and Linux
//! get their controls from here, and Linux its resizing too: that's the one
//! desktop where a window with its own title bar has neither.

use crate::assets::{Icon, UI_FONT};
use crate::dock::{Dock, DockState, Side};
use crate::shell::{FocusCommandBar, ToggleLeftPanel, ToggleRightPanel};
use crate::theme::Theme;
use gpui::{
    Action, App, Context, Decorations, Div, Entity, FocusHandle, FontWeight, Hsla, KeyBinding,
    MouseButton, Pixels, Point, ResizeEdge, Role, SharedString, Size, Stateful, Window,
    WindowControlArea, actions, deferred, div, point, prelude::*, px, transparent_black,
};
use mudling_core::plugin::luau::Value;
use mudling_core::store::{CharactersState, ConnectionStatus, SettingsState};
use std::time::Duration;

/// Where the traffic lights sit, level with the middle of the bar.
pub const TRAFFIC_LIGHTS: Point<Pixels> = point(px(19.), px(19.));
const TRAFFIC_LIGHTS_WIDTH: Pixels = px(98.);
const HEIGHT: Pixels = px(52.);
const EDGE_PADDING: Pixels = px(12.);
const BUTTON_SIZE: Pixels = px(28.);
const ICON_SIZE: Pixels = px(16.);
const SWITCHER_HEIGHT: Pixels = px(34.);
const MARK_SIZE: Pixels = px(20.);
const MENU_MIN_WIDTH: Pixels = px(240.);
/// How far in from a client-drawn window's edge a press resizes it.
const RESIZE_MARGIN: Pixels = px(6.);
/// The key the character's name would be under: the `character` store, which
/// a game plugin writes. None writes this key yet.
const CHARACTER_NAME_KEY: &str = "character.name";
/// With no session the switcher has no server to name.
const NO_SERVER: &str = "MUDling";
const BADGE_SIZE: Pixels = px(15.);
const CONTEXT: &str = "TitleBar";
const MENU_CONTEXT: &str = "TitleBarMenu";
/// Above a panel that overlays the chat.
const MENU_PRIORITY: usize = 2;

actions!(
    title_bar,
    [
        /// Find in the scrollback: VYT-209 handles it.
        FindInScrollback,
        OpenSettings,
        FocusNextControl,
        FocusPreviousControl,
        CloseMenu,
    ]
);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new(secondary!("f"), FindInScrollback, None),
        KeyBinding::new(secondary!(","), OpenSettings, None),
        KeyBinding::new("shift-tab", FocusPreviousControl, None),
        KeyBinding::new("tab", FocusNextControl, Some(CONTEXT)),
        KeyBinding::new("escape", FocusCommandBar, Some(CONTEXT)),
        KeyBinding::new("escape", CloseMenu, Some(MENU_CONTEXT)),
        KeyBinding::new("tab", CloseMenu, Some(MENU_CONTEXT)),
        KeyBinding::new("shift-tab", CloseMenu, Some(MENU_CONTEXT)),
    ]);
}

/// What the bar says about the session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionLabel {
    server: SharedString,
    character: Option<SharedString>,
    connection: SharedString,
}

impl Default for SessionLabel {
    fn default() -> SessionLabel {
        SessionLabel::new(NO_SERVER, None, "Not connected")
    }
}

impl SessionLabel {
    pub fn new(
        server: impl Into<SharedString>,
        character: Option<SharedString>,
        connection: impl Into<SharedString>,
    ) -> SessionLabel {
        SessionLabel {
            server: server.into(),
            character,
            connection: connection.into(),
        }
    }

    /// `replaying` says so instead of naming a server: a paced replay opens
    /// and closes a connection every few frames.
    pub fn of(
        settings: &SettingsState,
        characters: &CharactersState,
        replaying: bool,
        latency: Option<Duration>,
    ) -> SessionLabel {
        let server = match &settings.server {
            _ if replaying => "Recording".to_owned(),
            Some(server) => server.clone(),
            None => NO_SERVER.to_owned(),
        };
        let character = match characters.0.get(CHARACTER_NAME_KEY) {
            Some(Value::Text(name)) => Some(name.clone().into()),
            _ => None,
        };
        let connection = connection_text(&settings.connection, replaying, latency);
        SessionLabel::new(server, character, connection)
    }

    fn name(&self) -> SharedString {
        match &self.character {
            Some(character) => format!("{} / {character}", self.server).into(),
            None => self.server.clone(),
        }
    }
}

fn connection_text(
    connection: &ConnectionStatus,
    replaying: bool,
    latency: Option<Duration>,
) -> String {
    if replaying {
        return "Replaying a recording".to_owned();
    }
    match (connection, latency) {
        (ConnectionStatus::Idle, _) => "Not connected".to_owned(),
        (ConnectionStatus::Connecting, _) => "Connecting…".to_owned(),
        (ConnectionStatus::Connected, None) => "Connected".to_owned(),
        (ConnectionStatus::Connected, Some(latency)) => {
            format!("Connected · {} ms", latency.as_millis())
        }
        (ConnectionStatus::Closed(reason), _) => format!("Disconnected: {reason}"),
    }
}

/// The edge of a window of `size` that `position` is on.
fn resize_edge(position: Point<Pixels>, size: Size<Pixels>) -> Option<ResizeEdge> {
    let left = position.x < RESIZE_MARGIN;
    let right = position.x >= size.width - RESIZE_MARGIN;
    let top = position.y < RESIZE_MARGIN;
    let bottom = position.y >= size.height - RESIZE_MARGIN;
    match (left, right, top, bottom) {
        (true, _, true, _) => Some(ResizeEdge::TopLeft),
        (_, true, true, _) => Some(ResizeEdge::TopRight),
        (true, _, _, true) => Some(ResizeEdge::BottomLeft),
        (_, true, _, true) => Some(ResizeEdge::BottomRight),
        (true, ..) => Some(ResizeEdge::Left),
        (_, true, ..) => Some(ResizeEdge::Right),
        (_, _, true, _) => Some(ResizeEdge::Top),
        (_, _, _, true) => Some(ResizeEdge::Bottom),
        _ => None,
    }
}

fn draws_its_own_frame(window: &Window) -> bool {
    matches!(window.window_decorations(), Decorations::Client { .. })
}

/// Starts resizing a window that draws its own frame, if a press landed on
/// its edge. Returns whether it did.
pub fn resize_from_edge(position: Point<Pixels>, window: &Window) -> bool {
    let edge = resize_edge(position, window.viewport_size());
    match edge.filter(|_| draws_its_own_frame(window)) {
        Some(edge) => {
            window.start_window_resize(edge);
            true
        }
        None => false,
    }
}

fn shortcut_name(key: &str) -> String {
    let modifier = if cfg!(target_os = "macos") {
        "Meta"
    } else {
        "Control"
    };
    format!("{modifier}+{key}")
}

/// A control's shape: reachable by Tab, and a press keeps focus where it is.
fn control(id: &'static str, label: impl Into<SharedString>, theme: &Theme) -> Stateful<Div> {
    let (hover, ring) = (theme.hover, theme.accent);
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .role(Role::Button)
        .aria_label(label)
        .focusable()
        .tab_stop(true)
        .occlude()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .border_color(transparent_black())
        .hover(move |style| style.bg(hover))
        .focus_visible(move |style| style.border_color(ring))
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
}

fn icon_button(
    id: &'static str,
    label: &'static str,
    icon: Icon,
    color: Hsla,
    theme: &Theme,
) -> Stateful<Div> {
    control(id, label, theme)
        .size(BUTTON_SIZE)
        .child(icon.svg().size(ICON_SIZE).text_color(color))
}

/// A button that does what `action`'s shortcut (on `key`) does.
fn action_button(
    (id, label, icon): (&'static str, &'static str, Icon),
    key: &str,
    action: impl Action + Clone,
    color: Hsla,
    theme: &Theme,
) -> Stateful<Div> {
    icon_button(id, label, icon, color, theme)
        .aria_keyshortcuts(shortcut_name(key))
        .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
}

/// The runestone, in the logomark's two colors.
fn runestone_mark(theme: &Theme) -> impl IntoElement {
    div()
        .relative()
        .flex_none()
        .size(MARK_SIZE)
        .child(
            Icon::Runestone
                .svg()
                .absolute()
                .size(MARK_SIZE)
                .text_color(theme.runestone),
        )
        .child(
            Icon::RunestoneCursor
                .svg()
                .absolute()
                .size(MARK_SIZE)
                .text_color(theme.accent),
        )
}

fn elided_text(text: SharedString) -> Div {
    div()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .child(text)
}

/// The minimize, maximize and close buttons, on the desktops that leave them
/// to the window. `window_control_area` is how Windows finds them, and the
/// clicks are how Linux acts on them.
fn window_controls(window: &Window, theme: &Theme) -> Option<Div> {
    if !cfg!(target_os = "windows") && !draws_its_own_frame(window) {
        return None;
    }
    let button = |id, label, icon, area, press: fn(&mut Window)| {
        icon_button(id, label, icon, theme.text_muted, theme)
            .window_control_area(area)
            .on_click(move |_, window, _| press(window))
    };
    let offered = window.window_controls();
    Some(
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .when(offered.minimize, |controls| {
                controls.child(button(
                    "window-minimize",
                    "Minimize",
                    Icon::WindowMinimize,
                    WindowControlArea::Min,
                    |window| window.minimize_window(),
                ))
            })
            .when(offered.maximize, |controls| {
                controls.child(button(
                    "window-maximize",
                    "Maximize",
                    Icon::WindowMaximize,
                    WindowControlArea::Max,
                    |window| window.zoom_window(),
                ))
            })
            .child(button(
                "window-close",
                "Close",
                Icon::WindowClose,
                WindowControlArea::Close,
                |window| window.remove_window(),
            )),
    )
}

pub struct TitleBar {
    session: SessionLabel,
    /// Problems for the sprocket's badge.
    problems: usize,
    left_dock: Entity<Dock>,
    right_dock: Entity<Dock>,
    /// The press that may turn into dragging the window.
    pressed: bool,
    menu_open: bool,
    menu_item: FocusHandle,
    focus_before_menu: Option<FocusHandle>,
}

impl TitleBar {
    pub fn new(
        left_dock: Entity<Dock>,
        right_dock: Entity<Dock>,
        cx: &mut Context<TitleBar>,
    ) -> TitleBar {
        TitleBar {
            session: SessionLabel::default(),
            problems: 0,
            left_dock,
            right_dock,
            pressed: false,
            menu_open: false,
            menu_item: cx.focus_handle(),
            focus_before_menu: None,
        }
    }

    pub fn show(&mut self, session: SessionLabel, cx: &mut Context<TitleBar>) {
        if session != self.session {
            self.session = session;
            cx.notify();
        }
    }

    pub fn show_problems(&mut self, problems: usize, cx: &mut Context<TitleBar>) {
        if problems != self.problems {
            self.problems = problems;
            cx.notify();
        }
    }

    #[cfg(test)]
    pub fn connection(&self) -> &str {
        &self.session.connection
    }

    pub fn open_menu(&mut self, window: &mut Window, cx: &mut Context<TitleBar>) {
        self.focus_before_menu = window.focused(cx);
        self.menu_open = true;
        window.focus(&self.menu_item, cx);
        cx.notify();
    }

    /// Closes the menu, if it's open, for what takes focus from it.
    pub fn dismiss_menu(&mut self, window: &mut Window, cx: &mut Context<TitleBar>) {
        if self.menu_open {
            self.close_menu(window, cx);
        }
    }

    fn close_menu(&mut self, window: &mut Window, cx: &mut Context<TitleBar>) {
        self.menu_open = false;
        if let Some(focused) = self.focus_before_menu.take() {
            window.focus(&focused, cx);
        }
        cx.notify();
    }

    fn toggle_menu(&mut self, window: &mut Window, cx: &mut Context<TitleBar>) {
        if self.menu_open {
            self.close_menu(window, cx);
        } else {
            self.open_menu(window, cx);
        }
    }

    fn render_panel_toggle(&self, side: Side, theme: &Theme, cx: &App) -> Stateful<Div> {
        let (dock, button, key) = match side {
            Side::Left => (
                &self.left_dock,
                ("toggle-left-panel", "Left panel", Icon::PanelLeft),
                "1",
            ),
            Side::Right => (
                &self.right_dock,
                ("toggle-right-panel", "Right panel", Icon::PanelRight),
                "2",
            ),
        };
        let open = dock.read(cx).state() == DockState::Open;
        let color = if open { theme.text } else { theme.text_muted };
        let button = match side {
            Side::Left => action_button(button, key, ToggleLeftPanel, color, theme),
            Side::Right => action_button(button, key, ToggleRightPanel, color, theme),
        };
        button
            .aria_toggled(open.into())
            .when(open, |button| button.bg(theme.selected))
    }

    /// The sprocket, with the count of problems on its corner.
    fn render_settings_button(&self, theme: &Theme) -> Stateful<Div> {
        let problems = self.problems;
        action_button(
            ("open-settings", "Settings", Icon::Sprocket),
            ",",
            OpenSettings,
            theme.text_muted,
            theme,
        )
        .relative()
        .when(problems > 0, |button| {
            button
                .aria_description(format!("{problems} problems"))
                .child(
                    div()
                        .debug_selector(|| "settings-badge".to_owned())
                        .absolute()
                        .top(px(-3.))
                        .right(px(-3.))
                        .min_w(BADGE_SIZE)
                        .h(BADGE_SIZE)
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(theme.warning)
                        .text_size(px(10.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.on_warning)
                        .child(problems.to_string()),
                )
        })
    }

    fn render_switcher(&self, theme: &Theme, cx: &mut Context<TitleBar>) -> Stateful<Div> {
        let session = &self.session;
        control(
            "session-switcher",
            format!("Session: {}", session.name()),
            theme,
        )
        .aria_expanded(self.menu_open)
        .flex_shrink(1.)
        .min_w_0()
        .w_full()
        .h(SWITCHER_HEIGHT)
        .px_2()
        .gap_2()
        .rounded_lg()
        .border_color(theme.border)
        .bg(theme.surface_raised)
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.text)
        .on_click(cx.listener(|bar, _, window, cx| bar.toggle_menu(window, cx)))
        .child(runestone_mark(theme))
        .child(elided_text(session.server.clone()))
        .when_some(session.character.clone(), |switcher, character| {
            switcher
                .child(div().flex_none().text_color(theme.text_muted).child("/"))
                .child(elided_text(character))
        })
        .child(
            Icon::ChevronDown
                .svg()
                .flex_none()
                .size(ICON_SIZE)
                .text_color(theme.text_muted),
        )
    }

    /// One game runs at a time, so the menu lists only that; VYT-205 adds the
    /// saved characters.
    fn render_menu(&self, theme: &Theme, cx: &mut Context<TitleBar>) -> impl IntoElement {
        let session = &self.session;
        let name = session.name();
        let (hover, ring) = (theme.hover, theme.accent);
        let item = div()
            .id("session-current")
            .debug_selector(|| "session-current".to_owned())
            .role(Role::MenuItem)
            .aria_label(name.clone())
            .aria_description(session.connection.clone())
            .aria_selected(true)
            .track_focus(&self.menu_item)
            .px_2()
            .py_1p5()
            .flex()
            .items_center()
            .gap_2()
            .rounded_md()
            .border_1()
            .border_color(transparent_black())
            .hover(move |style| style.bg(hover))
            .focus_visible(move |style| style.border_color(ring))
            .on_click(cx.listener(|bar, _, window, cx| bar.close_menu(window, cx)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        elided_text(name)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text),
                    )
                    .child(
                        elided_text(session.connection.clone())
                            .text_size(px(12.))
                            .text_color(theme.text_muted),
                    ),
            )
            .child(
                Icon::Check
                    .svg()
                    .flex_none()
                    .size(ICON_SIZE)
                    .text_color(theme.accent),
            );
        let menu = div()
            .id("session-menu")
            .debug_selector(|| "session-menu".to_owned())
            .key_context(MENU_CONTEXT)
            .role(Role::Menu)
            .aria_label("Sessions")
            .occlude()
            .absolute()
            .top_full()
            .left_0()
            .mt_1()
            .min_w(MENU_MIN_WIDTH)
            .p_1()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_raised)
            .shadow(theme.floating_shadow())
            .on_action(cx.listener(|bar, _: &CloseMenu, window, cx| bar.close_menu(window, cx)))
            .child(item);
        deferred(menu).with_priority(MENU_PRIORITY + 1)
    }

    /// Takes the click that closes the menu, so it lands on nothing else.
    fn render_menu_scrim(&self, window: &Window, cx: &mut Context<TitleBar>) -> impl IntoElement {
        deferred(
            div()
                .id("session-menu-scrim")
                .debug_selector(|| "session-menu-scrim".to_owned())
                .occlude()
                .absolute()
                .top_0()
                .left_0()
                .w(window.viewport_size().width)
                .h(window.viewport_size().height)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|bar, _, window, cx| {
                        window.prevent_default();
                        bar.close_menu(window, cx);
                    }),
                ),
        )
        .with_priority(MENU_PRIORITY)
    }
}

impl Render for TitleBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<TitleBar>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let beside_traffic_lights = cfg!(target_os = "macos") && !window.is_fullscreen();
        let switcher = div()
            .relative()
            .min_w_0()
            .child(self.render_switcher(&theme, cx))
            .when(self.menu_open, |switcher| {
                switcher.child(self.render_menu(&theme, cx))
            });
        let connection = div()
            .id("connection")
            .role(Role::Label)
            .aria_value(self.session.connection.clone())
            .min_w_0()
            .flex()
            .items_center()
            .gap_1p5()
            .text_size(px(12.))
            .text_color(theme.text_muted)
            .child(
                Icon::Signal
                    .svg()
                    .flex_none()
                    .size(px(14.))
                    .text_color(theme.text_muted),
            )
            .child(elided_text(self.session.connection.clone()));
        div()
            .id("title-bar")
            .debug_selector(|| "title-bar".to_owned())
            .key_context(CONTEXT)
            .role(Role::Group)
            .aria_label("Title bar")
            .window_control_area(WindowControlArea::Drag)
            .relative()
            .flex_none()
            .w_full()
            .h(HEIGHT)
            .pl(if beside_traffic_lights {
                TRAFFIC_LIGHTS_WIDTH
            } else {
                EDGE_PADDING
            })
            .pr(EDGE_PADDING)
            .flex()
            .items_center()
            .gap_3()
            .font_family(UI_FONT)
            .text_size(px(13.))
            .bg(theme.background)
            .border_b_1()
            .border_color(theme.divider)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|bar, event: &gpui::MouseDownEvent, window, _| {
                    bar.pressed = event.click_count == 1;
                    if event.click_count == 2 {
                        if cfg!(target_os = "macos") {
                            window.titlebar_double_click();
                        } else {
                            window.zoom_window();
                        }
                    }
                }),
            )
            .on_mouse_move(cx.listener(|bar, event: &gpui::MouseMoveEvent, window, _| {
                if std::mem::take(&mut bar.pressed) && event.dragging() {
                    window.start_window_move();
                }
            }))
            .on_action(|_: &FocusNextControl, window, cx| window.focus_next(cx))
            .child(self.render_panel_toggle(Side::Left, &theme, cx))
            .child(switcher)
            .child(connection)
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(action_button(
                        ("find-in-scrollback", "Search", Icon::Find),
                        "F",
                        FindInScrollback,
                        theme.placeholder,
                        &theme,
                    ))
                    .child(self.render_panel_toggle(Side::Right, &theme, cx))
                    .child(self.render_settings_button(&theme)),
            )
            .children(window_controls(window, &theme))
            .when(self.menu_open, |bar| {
                bar.child(self.render_menu_scrim(window, cx))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::Shell;
    use futures::channel::mpsc::unbounded;
    use gpui::{KeyUpEvent, Keystroke, Modifiers, TestAppContext, VisualTestContext, size};
    use mudling_core::net::DisconnectReason;
    use std::cell::Cell;
    use std::rc::Rc;

    fn connected() -> SettingsState {
        SettingsState {
            server: Some("mud.example".to_owned()),
            connection: ConnectionStatus::Connected,
            ..SettingsState::default()
        }
    }

    fn label(settings: &SettingsState, latency: Option<Duration>) -> SessionLabel {
        SessionLabel::of(settings, &CharactersState::default(), false, latency)
    }

    #[test]
    fn the_connection_shows_its_latency_once_there_is_one() {
        assert_eq!(label(&connected(), None).connection, "Connected");
        let latency = Some(Duration::from_millis(38));
        assert_eq!(label(&connected(), latency).connection, "Connected · 38 ms");

        let closed = SettingsState {
            connection: ConnectionStatus::Closed(DisconnectReason::ClosedByServer),
            ..connected()
        };
        assert_eq!(
            label(&closed, latency).connection,
            "Disconnected: closed by server"
        );
    }

    #[test]
    fn a_replay_says_so_rather_than_naming_a_server() {
        let replay = SessionLabel::of(&connected(), &CharactersState::default(), true, None);
        assert_eq!(replay.server, "Recording");
        assert_eq!(replay.connection, "Replaying a recording");
    }

    #[test]
    fn the_character_shows_once_a_store_has_it() {
        assert_eq!(label(&connected(), None).name(), "mud.example");

        let characters = CharactersState(
            [(
                CHARACTER_NAME_KEY.to_owned(),
                Value::Text("Altissima".to_owned()),
            )]
            .into(),
        );
        let named = SessionLabel::of(&connected(), &characters, false, None);
        assert_eq!(named.name(), "mud.example / Altissima");
    }

    #[test]
    fn a_press_at_the_window_edge_names_the_edge() {
        let window = size(px(800.), px(600.));
        let edge = |x, y| resize_edge(point(px(x), px(y)), window);
        assert!(matches!(edge(2., 2.), Some(ResizeEdge::TopLeft)));
        assert!(matches!(edge(798., 598.), Some(ResizeEdge::BottomRight)));
        assert!(matches!(edge(400., 2.), Some(ResizeEdge::Top)));
        assert!(matches!(edge(798., 300.), Some(ResizeEdge::Right)));
        assert!(edge(400., 300.).is_none());
    }

    fn window(cx: &mut TestAppContext) -> (Entity<Shell>, &mut VisualTestContext) {
        cx.update(|cx| {
            crate::input::bind_keys(cx);
            crate::shell::bind_keys(cx);
            bind_keys(cx);
            cx.set_reduce_motion(true);
        });
        let (_waker, woken) = unbounded();
        let opened = cx.add_window_view(|window, cx| Shell::new(None, woken, window, cx));
        opened.1.run_until_parked();
        opened
    }

    fn click(id: &'static str, cx: &mut VisualTestContext) {
        let bounds = cx.debug_bounds(id).expect("the control is drawn");
        cx.simulate_click(bounds.center(), Modifiers::none());
        cx.run_until_parked();
    }

    fn dock_state(shell: &Entity<Shell>, side: Side, cx: &mut VisualTestContext) -> DockState {
        shell.read_with(cx, |shell, cx| shell.dock(side).read(cx).state())
    }

    fn typed(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> String {
        shell.read_with(cx, |shell, cx| shell.typed(cx))
    }

    /// Enter activates a focused control when it's let go.
    fn press_enter(cx: &mut VisualTestContext) {
        cx.simulate_keystrokes("enter");
        cx.simulate_event(KeyUpEvent {
            keystroke: Keystroke::parse("enter").unwrap(),
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn the_toggles_open_and_close_the_panels_and_typing_stays_in_the_command_bar(
        cx: &mut TestAppContext,
    ) {
        let (shell, cx) = window(cx);
        click("toggle-left-panel", cx);
        assert_eq!(dock_state(&shell, Side::Left, cx), DockState::Open);
        click("toggle-right-panel", cx);
        assert_eq!(dock_state(&shell, Side::Right, cx), DockState::Open);
        click("toggle-left-panel", cx);
        assert_eq!(dock_state(&shell, Side::Left, cx), DockState::Hidden);

        cx.simulate_input("look");
        assert_eq!(typed(&shell, cx), "look");
    }

    #[gpui::test]
    fn a_press_on_the_bar_or_the_chat_leaves_focus_for_the_toggles(cx: &mut TestAppContext) {
        let (shell, cx) = window(cx);
        for place in ["title-bar", "chat-area"] {
            let bounds = cx.debug_bounds(place).unwrap();
            cx.simulate_click(bounds.center(), Modifiers::none());
            cx.run_until_parked();
        }
        click("toggle-right-panel", cx);
        assert_eq!(dock_state(&shell, Side::Right, cx), DockState::Open);
    }

    #[gpui::test]
    fn panels_open_below_the_bar_even_over_the_chat(cx: &mut TestAppContext) {
        let (shell, cx) = window(cx);
        cx.simulate_resize(size(px(760.), px(640.)));
        shell.update(cx, |shell, cx| shell.open_panels(cx));
        cx.run_until_parked();

        let bar = cx.debug_bounds("title-bar").unwrap();
        assert_eq!(bar.top(), px(0.));
        assert_eq!(bar.size.width, px(760.));
        for dock in ["left-dock", "right-dock"] {
            assert!(cx.debug_bounds(dock).unwrap().top() >= bar.bottom());
        }
    }

    #[gpui::test]
    fn search_dispatches_its_action_by_click_and_by_key(cx: &mut TestAppContext) {
        let (_shell, cx) = window(cx);
        let finds = Rc::new(Cell::new(0));
        cx.update(|_, cx| {
            let counted = finds.clone();
            cx.on_action(move |_: &FindInScrollback, _| counted.set(counted.get() + 1));
        });

        click("find-in-scrollback", cx);
        let modifier = if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        };
        cx.simulate_keystrokes(&format!("{modifier}-f"));
        assert_eq!(finds.get(), 2);
    }

    #[gpui::test]
    fn the_keyboard_walks_the_controls_and_esc_returns_to_the_command_bar(cx: &mut TestAppContext) {
        let (shell, cx) = window(cx);

        // Back from the command bar past Settings to the right toggle.
        cx.simulate_keystrokes("shift-tab shift-tab");
        press_enter(cx);
        assert_eq!(dock_state(&shell, Side::Right, cx), DockState::Open);
        press_enter(cx);
        assert_eq!(dock_state(&shell, Side::Right, cx), DockState::Hidden);

        cx.simulate_keystrokes("escape");
        cx.simulate_input("look");
        assert_eq!(typed(&shell, cx), "look");
    }

    #[gpui::test]
    fn the_last_control_tabs_on_to_the_command_bar(cx: &mut TestAppContext) {
        let (shell, cx) = window(cx);
        cx.simulate_keystrokes("shift-tab tab");
        cx.simulate_input("look");
        assert_eq!(typed(&shell, cx), "look");
    }

    #[gpui::test]
    fn the_menu_lists_the_current_session_and_gives_focus_back(cx: &mut TestAppContext) {
        let (shell, cx) = window(cx);
        click("session-switcher", cx);
        assert!(cx.debug_bounds("session-current").is_some());
        let menu = cx.debug_bounds("session-menu").unwrap();
        let switcher = cx.debug_bounds("session-switcher").unwrap();
        assert!(menu.top() >= switcher.bottom());

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(cx.debug_bounds("session-menu").is_none());
        cx.simulate_input("look");
        assert_eq!(typed(&shell, cx), "look");
    }

    #[gpui::test]
    fn the_click_that_closes_the_menu_lands_on_nothing_else(cx: &mut TestAppContext) {
        let (shell, cx) = window(cx);
        click("session-switcher", cx);
        click("toggle-right-panel", cx);
        assert!(cx.debug_bounds("session-menu").is_none());
        assert_eq!(dock_state(&shell, Side::Right, cx), DockState::Hidden);

        click("session-switcher", cx);
        click("session-switcher", cx);
        assert!(cx.debug_bounds("session-menu").is_none());
    }

    #[gpui::test]
    fn a_long_name_gives_way_and_the_controls_stay_in_a_narrow_window(cx: &mut TestAppContext) {
        let (shell, cx) = window(cx);
        cx.simulate_resize(size(px(480.), px(400.)));
        shell.update(cx, |shell, cx| {
            let session = SessionLabel::new(
                "a-server-name-long-enough-to-fill-the-whole-title-bar.example",
                Some("Altissima".into()),
                "Disconnected: the connection timed out",
            );
            shell.show_sample_session(session, cx);
        });
        cx.run_until_parked();

        let settings = cx.debug_bounds("open-settings").unwrap();
        assert!(settings.right() <= px(480.));
        let toggle = cx.debug_bounds("toggle-left-panel").unwrap();
        let switcher = cx.debug_bounds("session-switcher").unwrap();
        assert!(toggle.right() <= switcher.left());
        assert!(switcher.right() <= cx.debug_bounds("find-in-scrollback").unwrap().left());
    }

    #[gpui::test]
    fn only_windows_and_a_client_drawn_frame_get_window_controls(cx: &mut TestAppContext) {
        let (_shell, cx) = window(cx);
        cx.update(|window, _| assert!(!draws_its_own_frame(window)));
        assert_eq!(
            cx.debug_bounds("window-close").is_some(),
            cfg!(target_os = "windows")
        );
    }
}
