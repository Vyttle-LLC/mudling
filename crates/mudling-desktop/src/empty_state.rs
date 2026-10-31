//! "Where to today?": what the window shows with no game. A card for each
//! saved character, adding a server, and Quick connect.
//!
//! It only draws and validates. What it asks for, it emits, and the shell does
//! off the UI thread. Nothing here runs while the player reads it.

use crate::assets::{HEADING_FONT, Icon, UI_FONT};
use crate::input::{CommandInput, InputEvent};
use crate::launcher::Target;
use crate::theme::Theme;
use crate::title_bar::{FocusNextControl, runestone_mark};
use gpui::{
    App, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, KeyBinding,
    MouseButton, Pixels, Role, SharedString, Stateful, Subscription, Window, actions, div,
    prelude::*, px, relative, transparent_black,
};
use mudling_core::persist::{Database, SavedServer, SavedServers, Timestamp};

const CONTEXT: &str = "EmptyState";
const ADD_CONTEXT: &str = "AddServer";
/// The cards are 560 wide on the canvas, inside this column's padding.
const COLUMN_WIDTH: Pixels = px(592.);
const MARK_SIZE: Pixels = px(84.);
const CARD_HEIGHT: Pixels = px(74.);
const AVATAR_SIZE: Pixels = px(40.);
const BUTTON_HEIGHT: Pixels = px(36.);
const HOST_RULE: &str = "A host can hold letters, digits, dots, dashes and underscores.";
const PORT_RULE: &str = "The port must be a number from 1 to 65535.";
const NEEDS_PORT: &str = "Add the port after the host, as in mud.example:4000.";
const NEEDS_ADDRESS: &str = "Enter a server as host:port.";
const DAY_MILLIS: i64 = 24 * 60 * 60 * 1000;
/// Keeps its gaps by hand: gpui text has no letter spacing.
const QUICK_CONNECT: &str = "Q U I C K   C O N N E C T";

actions!(empty_state, [CancelAdd]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", FocusNextControl, Some(CONTEXT)),
        KeyBinding::new("escape", CancelAdd, Some(ADD_CONTEXT)),
    ]);
}

/// What the player typed as a server, or why it can't be one.
pub fn parse_address(typed: &str) -> Result<Target, &'static str> {
    let typed = typed.trim();
    if typed.is_empty() {
        return Err(NEEDS_ADDRESS);
    }
    let Some((host, port)) = typed.rsplit_once(':') else {
        return Err(if Database::can_name(typed) {
            NEEDS_PORT
        } else {
            HOST_RULE
        });
    };
    if !Database::can_name(host) {
        return Err(if host.is_empty() {
            NEEDS_ADDRESS
        } else {
            HOST_RULE
        });
    }
    match port.parse() {
        Ok(port) if port > 0 => Ok(Target {
            host: host.to_ascii_lowercase(),
            port,
        }),
        _ => Err(PORT_RULE),
    }
}

/// One place to go: a saved character, or a server nobody has played on.
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    pub target: Target,
    pub character: Option<SharedString>,
    pub detail: SharedString,
    last_played: Timestamp,
}

impl Card {
    pub fn server(&self) -> SharedString {
        self.target.host.clone().into()
    }

    pub fn title(&self) -> SharedString {
        match &self.character {
            Some(character) => format!("{character} on {}", self.target.host).into(),
            None => self.server(),
        }
    }

    fn initial(&self) -> SharedString {
        let name = self.character.clone().unwrap_or_else(|| self.server());
        let initial = name.chars().next().unwrap_or('?');
        initial.to_uppercase().collect::<String>().into()
    }
}

/// The cards for what's saved, the latest played first. A session with no
/// credentials plays as an unnamed character, which only earns a card when
/// the server has no named one.
pub fn cards(servers: &[SavedServer], now: Timestamp) -> Vec<Card> {
    let mut cards: Vec<Card> = servers
        .iter()
        .flat_map(|server| server_cards(server, now))
        .collect();
    cards.sort_by_key(|card| std::cmp::Reverse(card.last_played));
    cards
}

fn server_cards(server: &SavedServer, now: Timestamp) -> Vec<Card> {
    let target = Target {
        host: server.host.clone(),
        port: server.port,
    };
    let any_named = server.characters.iter().any(|c| c.name.is_some());
    let shown = server
        .characters
        .iter()
        .filter(|character| character.name.is_some() || !any_named);
    let cards: Vec<Card> = shown
        .map(|character| {
            let facts = [
                character.level.map(|level| format!("Level {level}")),
                server
                    .last_room
                    .as_ref()
                    .map(|room| format!("last in {room}")),
                Some(days_ago(character.last_played, now)),
            ];
            Card {
                target: target.clone(),
                character: character.name.clone().map(Into::into),
                detail: facts
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · ")
                    .into(),
                last_played: character.last_played,
            }
        })
        .collect();
    if cards.is_empty() {
        return vec![Card {
            detail: target.to_string().into(),
            target,
            character: None,
            last_played: server.updated,
        }];
    }
    cards
}

fn days_ago(then: Timestamp, now: Timestamp) -> String {
    match (now.0 - then.0) / DAY_MILLIS {
        ..=0 => "today".to_owned(),
        1 => "yesterday".to_owned(),
        days => format!("{days} days ago"),
    }
}

/// A button with a text label: filled with the accent, or outlined.
pub fn text_button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    primary: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let id = id.into();
    let selector = id.clone();
    let (fill, text, border) = if primary {
        (theme.accent, theme.on_accent, transparent_black())
    } else {
        (transparent_black(), theme.text, theme.border)
    };
    let (hover, ring) = (
        if primary {
            theme.accent_hover
        } else {
            theme.hover
        },
        theme.text,
    );
    let label = label.into();
    div()
        .id(id)
        .debug_selector(move || selector.to_string())
        .role(Role::Button)
        .aria_label(label.clone())
        .focusable()
        .tab_stop(true)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .gap_2()
        .h(BUTTON_HEIGHT)
        .px_4()
        .rounded_md()
        .border_1()
        .border_color(border)
        .bg(fill)
        .text_color(text)
        .text_size(px(14.))
        .font_weight(FontWeight::MEDIUM)
        .hover(move |style| style.bg(hover))
        .focus_visible(move |style| style.border_color(ring))
        .child(label)
}

fn quiet_line(id: &'static str, role: Role, text: impl Into<SharedString>) -> Stateful<Div> {
    let text = text.into();
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .role(role)
        .aria_value(text.clone())
        .child(text)
}

pub enum EmptyStateEvent {
    Connect(Target),
    Add(Target),
}

pub struct EmptyState {
    cards: Vec<Card>,
    first_connect: FocusHandle,
    add_button: FocusHandle,
    quick: Entity<CommandInput>,
    quick_error: Option<&'static str>,
    address: Entity<CommandInput>,
    adding: bool,
    add_error: Option<&'static str>,
    /// What went wrong that isn't about what's typed.
    problem: Option<SharedString>,
    _subscriptions: [Subscription; 2],
}

impl EventEmitter<EmptyStateEvent> for EmptyState {}

impl EmptyState {
    pub fn new(window: &mut Window, cx: &mut Context<EmptyState>) -> EmptyState {
        let quick = cx.new(|cx| CommandInput::field("Quick connect", "host:port", cx));
        let address = cx.new(|cx| CommandInput::field("Server address", "host:port", cx));
        let quick_typed = cx.subscribe_in(&quick, window, |landing, _, event, _, cx| {
            if let InputEvent::Submitted(typed) = event {
                landing.quick_connect(&typed.clone(), cx);
            }
        });
        let address_typed = cx.subscribe_in(&address, window, |landing, _, event, window, cx| {
            if let InputEvent::Submitted(typed) = event {
                landing.add_server(&typed.clone(), window, cx);
            }
        });
        EmptyState {
            cards: Vec::new(),
            first_connect: cx.focus_handle().tab_stop(true),
            add_button: cx.focus_handle().tab_stop(true),
            quick,
            quick_error: None,
            address,
            adding: false,
            add_error: None,
            problem: None,
            _subscriptions: [quick_typed, address_typed],
        }
    }

    pub fn show(&mut self, saved: &SavedServers, cx: &mut Context<EmptyState>) {
        self.cards = cards(&saved.servers, Timestamp::now());
        self.problem = (!saved.problems.is_empty()).then(|| {
            format!(
                "Some saved servers can't be read. {}",
                saved.problems.join("; ")
            )
            .into()
        });
        cx.notify();
    }

    pub fn cards(&self) -> &[Card] {
        &self.cards
    }

    pub fn show_problem(&mut self, problem: String, cx: &mut Context<EmptyState>) {
        self.problem = Some(problem.into());
        cx.notify();
    }

    /// Focuses the first card's Connect, or Quick connect with none, unless
    /// the player has started typing.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<EmptyState>) {
        let typing = [&self.quick, &self.address]
            .iter()
            .any(|field| field.focus_handle(cx).is_focused(window));
        if typing {
            return;
        }
        if self.cards.is_empty() {
            window.focus(&self.quick.focus_handle(cx), cx);
        } else {
            window.focus(&self.first_connect, cx);
        }
    }

    fn quick_connect(&mut self, typed: &str, cx: &mut Context<EmptyState>) {
        let parsed = parse_address(typed);
        self.quick_error = parsed.as_ref().err().copied();
        cx.notify();
        if let Ok(target) = parsed {
            cx.emit(EmptyStateEvent::Connect(target));
        }
    }

    fn add_server(&mut self, typed: &str, window: &mut Window, cx: &mut Context<EmptyState>) {
        let parsed = parse_address(typed);
        self.add_error = parsed.as_ref().err().copied();
        cx.notify();
        if let Ok(target) = parsed {
            self.stop_adding(window, cx);
            cx.emit(EmptyStateEvent::Add(target));
        }
    }

    fn start_adding(&mut self, window: &mut Window, cx: &mut Context<EmptyState>) {
        self.adding = true;
        window.focus(&self.address.focus_handle(cx), cx);
        cx.notify();
    }

    fn stop_adding(&mut self, window: &mut Window, cx: &mut Context<EmptyState>) {
        self.adding = false;
        self.add_error = None;
        self.address
            .update(cx, |address, cx| address.show(String::new(), 0, window, cx));
        window.focus(&self.add_button, cx);
        cx.notify();
    }

    fn render_card(
        &self,
        index: usize,
        card: &Card,
        theme: &Theme,
        cx: &mut Context<EmptyState>,
    ) -> impl IntoElement + use<> {
        let first = index == 0;
        let target = card.target.clone();
        let world = card.character.as_ref().map(|_| card.server());
        let name = card.character.clone().unwrap_or_else(|| card.server());
        let connect = text_button(format!("connect-{index}"), "Connect", first, theme)
            .aria_label(format!("Connect to {}", card.title()))
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.emit(EmptyStateEvent::Connect(target.clone()));
            }));
        let connect = match first {
            true => connect
                .track_focus(&self.first_connect)
                .aria_keyshortcuts("Enter")
                .child(Icon::Return.svg().size(px(12.)).text_color(theme.on_accent)),
            false => connect,
        };
        div()
            .id(("card", index))
            .debug_selector(move || format!("card-{index}"))
            .role(Role::Group)
            .aria_label(card.title())
            .aria_description(card.detail.clone())
            .w_full()
            .h(CARD_HEIGHT)
            .px_4()
            .flex()
            .items_center()
            .gap_4()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_raised)
            .child(
                div()
                    .flex_none()
                    .size(AVATAR_SIZE)
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .bg(theme.segment)
                    .font_family(HEADING_FONT)
                    .text_color(theme.accent)
                    .child(card.initial()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .child(div().font_weight(FontWeight::MEDIUM).child(name))
                            .children(world.map(|world| {
                                div()
                                    .text_color(theme.text_muted)
                                    .child(format!("on {world}"))
                            })),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(theme.text_muted)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(card.detail.clone()),
                    ),
            )
            .child(connect)
    }

    fn render_add_prompt(&self, theme: &Theme, cx: &mut Context<EmptyState>) -> Div {
        div()
            .w_full()
            .flex()
            .items_center()
            .gap_4()
            .child(
                div()
                    .flex_none()
                    .size(AVATAR_SIZE)
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .border_1()
                    .border_dashed()
                    .border_color(theme.border)
                    .child(Icon::Plus.svg().size(px(14.)).text_color(theme.text_muted)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(div().font_weight(FontWeight::MEDIUM).child("Add a server"))
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(theme.text_muted)
                            .child("MUDling suggests a game plugin when it recognizes the server."),
                    ),
            )
            .child(
                text_button("add-server", "Add", false, theme)
                    .aria_label("Add a server")
                    .track_focus(&self.add_button)
                    .on_click(cx.listener(|landing, _, window, cx| {
                        landing.start_adding(window, cx);
                    })),
            )
    }

    fn render_add_form(&self, theme: &Theme, cx: &mut Context<EmptyState>) -> Div {
        div()
            .w_full()
            .key_context(ADD_CONTEXT)
            .on_action(cx.listener(|landing, _: &CancelAdd, window, cx| {
                landing.stop_adding(window, cx);
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(self.address.clone()))
                    .child(
                        text_button("save-server", "Save", true, theme).on_click(cx.listener(
                            |landing, _, window, cx| {
                                let typed = landing.address.read(cx).content().to_owned();
                                landing.add_server(&typed, window, cx);
                            },
                        )),
                    )
                    .child(text_button("cancel-add", "Cancel", false, theme).on_click(
                        cx.listener(|landing, _, window, cx| landing.stop_adding(window, cx)),
                    )),
            )
            .children(self.add_error.map(|error| {
                quiet_line("add-error", Role::Alert, error)
                    .pt_1()
                    .text_size(px(13.))
                    .text_color(theme.error)
            }))
    }

    fn render_quick_connect(&self, theme: &Theme, cx: &mut Context<EmptyState>) -> Div {
        div()
            .w_full()
            .pt_8()
            .child(
                div()
                    .id("quick-connect-label")
                    .role(Role::Label)
                    .aria_value("Quick connect")
                    .pb_2()
                    .text_size(px(11.))
                    .text_color(theme.text_muted)
                    .child(QUICK_CONNECT),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(self.quick.clone()))
                    .child(
                        text_button("quick-connect", "Connect", false, theme).on_click(
                            cx.listener(|landing, _, _, cx| {
                                let typed = landing.quick.read(cx).content().to_owned();
                                landing.quick_connect(&typed, cx);
                            }),
                        ),
                    ),
            )
            .children(self.quick_error.map(|error| {
                quiet_line("quick-error", Role::Alert, error)
                    .pt_1()
                    .text_size(px(13.))
                    .text_color(theme.error)
            }))
    }
}

/// Where the data stays, in the words for this machine.
fn privacy_line() -> String {
    let device = if cfg!(target_os = "macos") {
        "Mac"
    } else {
        "device"
    };
    format!(
        "Your maps, triggers and notes stay on this {device} until you turn on sync in Settings."
    )
}

impl Render for EmptyState {
    fn render(&mut self, _: &mut Window, cx: &mut Context<EmptyState>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let cards: Vec<_> = self
            .cards
            .clone()
            .iter()
            .enumerate()
            .map(|(index, card)| self.render_card(index, card, &theme, cx).into_any_element())
            .collect();
        let add_body = if self.adding {
            self.render_add_form(&theme, cx)
        } else {
            self.render_add_prompt(&theme, cx)
        };
        let column = div()
            .w_full()
            .max_w(COLUMN_WIDTH)
            .px_4()
            .flex()
            .flex_col()
            .items_center()
            .child(runestone_mark(MARK_SIZE, &theme))
            .child(
                quiet_line("empty-state-title", Role::Heading, "Where to today?")
                    .pt_6()
                    .font_family(HEADING_FONT)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_size(px(28.)),
            )
            .child(
                quiet_line(
                    "empty-state-intro",
                    Role::Label,
                    "Pick a character to log back in, or connect to a new world.",
                )
                .pt_2()
                .pb_8()
                .text_size(px(15.))
                .text_color(theme.text_muted),
            )
            .child(div().w_full().flex().flex_col().gap_2().children(cards))
            .child(
                div()
                    .id("add-server-card")
                    .debug_selector(|| "add-server-card".to_owned())
                    .w_full()
                    .mt_2()
                    .min_h(CARD_HEIGHT)
                    .px_4()
                    .py_3()
                    .flex()
                    .items_center()
                    .rounded_lg()
                    .border_1()
                    .border_dashed()
                    .border_color(theme.border)
                    .child(add_body),
            )
            .child(self.render_quick_connect(&theme, cx))
            .children(self.problem.clone().map(|problem| {
                quiet_line("empty-state-problem", Role::Alert, problem)
                    .pt_4()
                    .text_size(px(13.))
                    .text_color(theme.error)
            }))
            .child(
                quiet_line("privacy-line", Role::Label, privacy_line())
                    .pt_8()
                    .text_size(px(13.))
                    .text_color(theme.text_muted),
            );
        div()
            .id("empty-state")
            .debug_selector(|| "empty-state".to_owned())
            .key_context(CONTEXT)
            .on_action(|_: &FocusNextControl, window, cx| window.focus_next(cx))
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .size_full()
            .overflow_y_scroll()
            .font_family(UI_FONT)
            .text_color(theme.text)
            .child(
                div()
                    .min_h(relative(1.))
                    .w_full()
                    .py_6()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .child(column),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mudling_core::persist::SavedCharacter;

    const NOW: Timestamp = Timestamp(100 * DAY_MILLIS);

    fn server(characters: Vec<SavedCharacter>, last_room: Option<&str>) -> SavedServer {
        SavedServer {
            host: "mud.example".to_owned(),
            port: 4000,
            updated: NOW,
            last_room: last_room.map(str::to_owned),
            characters,
        }
    }

    fn character(name: Option<&str>, level: Option<i64>, days: i64) -> SavedCharacter {
        SavedCharacter {
            name: name.map(str::to_owned),
            level,
            last_played: Timestamp(NOW.0 - days * DAY_MILLIS),
        }
    }

    #[test]
    fn an_address_needs_a_host_and_a_port() {
        let target = parse_address(" Mud.Example:4000 ").unwrap();
        assert_eq!(target.to_string(), "mud.example:4000");
        assert_eq!(parse_address(""), Err(NEEDS_ADDRESS));
        assert_eq!(parse_address(":4000"), Err(NEEDS_ADDRESS));
        assert_eq!(parse_address("mud.example"), Err(NEEDS_PORT));
        assert_eq!(parse_address("mud.example:"), Err(PORT_RULE));
        assert_eq!(parse_address("mud.example:0"), Err(PORT_RULE));
        assert_eq!(parse_address("mud.example:70000"), Err(PORT_RULE));
        assert_eq!(parse_address("mud example:4000"), Err(HOST_RULE));
        assert_eq!(parse_address("[::1]:4000"), Err(HOST_RULE));
        assert_eq!(parse_address("../etc:4000"), Err(HOST_RULE));
    }

    #[test]
    fn a_card_shows_only_what_is_known() {
        let known = server(
            vec![character(Some("Altissima"), Some(2), 0)],
            Some("the Donation Room"),
        );
        let card = &cards(&[known], NOW)[0];
        assert_eq!(card.title(), "Altissima on mud.example");
        assert_eq!(card.detail, "Level 2 · last in the Donation Room · today");

        let unknown = server(vec![character(None, None, 3)], None);
        let card = &cards(&[unknown], NOW)[0];
        assert_eq!(card.title(), "mud.example");
        assert_eq!(card.detail, "3 days ago");
    }

    #[test]
    fn a_server_nobody_has_played_on_gets_a_card() {
        let card = &cards(&[server(Vec::new(), None)], NOW)[0];
        assert_eq!(card.detail, "mud.example:4000");
    }

    #[test]
    fn the_unnamed_character_steps_aside_for_a_named_one_and_the_latest_is_first() {
        let saved = server(
            vec![
                character(None, None, 0),
                character(Some("Older"), None, 2),
                character(Some("Newer"), None, 1),
            ],
            None,
        );
        let titles: Vec<_> = cards(&[saved], NOW).iter().map(Card::title).collect();
        assert_eq!(titles, ["Newer on mud.example", "Older on mud.example"]);
    }
}
