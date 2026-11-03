//! The HUD, wherever the player put it: the character and what it's doing, a
//! meter for each vital the server reports, and who it's fighting; nothing
//! until the server reports a vital. On a row too narrow for them all it
//! wraps, so no vital is ever clipped.
//!
//! - **Attached** to the command bar, it opens to show the opponent, affects
//!   and the experience to the next level, whichever of them the game gives.
//!   The bar grows and the chat ends where it begins, so nothing is covered.
//! - **Floating**, it's a card over the window, moved by the mouse or the
//!   arrow keys. Only where it was left is kept: it's drawn inside the window
//!   at whatever size that is, so it comes back when the window grows again.
//! - **On the top strip**, it's a row the width of the window.
//!
//! The shell mounts this view where the placement says. Hidden, it mounts it
//! nowhere and the chat shows the prompt lines instead.

use crate::assets::Icon;
use crate::shell::{FocusCommandBar, FocusNext};
use crate::theme::Theme;
use crate::title_bar;
use gpui::{
    App, Context, Div, DragMoveEvent, EventEmitter, FocusHandle, FontWeight, Hsla, KeyBinding,
    Length, MouseButton, Pixels, Point, Role, SharedString, Size, Stateful, Transformation, Window,
    actions, div, percentage, point, prelude::*, px, relative, size,
};
use mudling_core::defaults::hud::Vital;
use mudling_core::plugin::luau::Value;
use mudling_core::store::{
    CHARACTER_LEVEL, CHARACTER_NAME, CHARACTER_POSITION, CharactersState, TargetState, VitalsState,
};
use serde::{Deserialize, Serialize};

const ROW_HEIGHT: Pixels = px(40.);
const METER_WIDTH: Pixels = px(64.);
const METER_HEIGHT: Pixels = px(4.);
const TOGGLE_SIZE: Pixels = px(24.);
const CHEVRON_SIZE: Pixels = px(14.);
const CARD_WIDTH: Pixels = px(216.);
const CARD_METER_WIDTH: Pixels = px(96.);
const CARD_PADDING: Pixels = px(11.);
const CARD_GRIP_HEIGHT: Pixels = px(12.);
const CARD_ROW_HEIGHT: Pixels = px(22.);
const CARD_TARGET_HEIGHT: Pixels = px(32.);
/// Where the card starts, in from the top right of the window under the title
/// bar.
const CARD_MARGIN: Pixels = px(24.);
/// How far an arrow key moves the card.
const CARD_STEP: Pixels = px(16.);
const CONTEXT: &str = "Hud";
const CARD_CONTEXT: &str = "HudCard";

actions!(
    hud,
    [
        ToggleHud,
        MoveCardUp,
        MoveCardDown,
        MoveCardLeft,
        MoveCardRight
    ]
);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new(secondary!("j"), ToggleHud, None),
        KeyBinding::new("tab", FocusNext, Some(CONTEXT)),
        KeyBinding::new("escape", FocusCommandBar, Some(CONTEXT)),
        KeyBinding::new("up", MoveCardUp, Some(CARD_CONTEXT)),
        KeyBinding::new("down", MoveCardDown, Some(CARD_CONTEXT)),
        KeyBinding::new("left", MoveCardLeft, Some(CARD_CONTEXT)),
        KeyBinding::new("right", MoveCardRight, Some(CARD_CONTEXT)),
    ]);
}

/// Where the player keeps the HUD.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HudPlacement {
    #[default]
    Attached,
    Floating,
    TopStrip,
    Hidden,
}

impl HudPlacement {
    pub const ALL: [(HudPlacement, &str); 4] = [
        (HudPlacement::Attached, "On the command bar"),
        (HudPlacement::Floating, "Floating card"),
        (HudPlacement::TopStrip, "Top strip"),
        (HudPlacement::Hidden, "Hidden"),
    ];
}

/// Thousands get a comma, as the numbers grow large.
fn grouped(number: i64) -> String {
    let digits = number.unsigned_abs().to_string();
    let mut text = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            text.push(',');
        }
        text.push(digit);
    }
    if number < 0 {
        text.insert(0, '-');
    }
    text
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MeterKind {
    Hp,
    Mana,
    Moves,
}

impl MeterKind {
    /// The name on the row, and the one a screen reader reads.
    fn labels(self) -> (&'static str, &'static str) {
        match self {
            MeterKind::Hp => ("HP", "Hit points"),
            MeterKind::Mana => ("MP", "Mana"),
            MeterKind::Moves => ("MV", "Moves"),
        }
    }

    fn color(self, theme: &Theme) -> Hsla {
        match self {
            MeterKind::Hp => theme.hud_hp,
            MeterKind::Mana => theme.hud_mana,
            MeterKind::Moves => theme.hud_moves,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Meter {
    kind: MeterKind,
    current: i64,
    max: Option<i64>,
}

impl Meter {
    fn fraction(&self) -> Option<f32> {
        fraction_of(self.current, self.max?)
    }

    fn value(&self) -> SharedString {
        match self.max {
            Some(max) => format!("{}/{}", grouped(self.current), grouped(max)).into(),
            None => grouped(self.current).into(),
        }
    }

    /// The value as a screen reader says it: "2,410 of 2,940", not a slash.
    fn spoken(&self) -> SharedString {
        match self.max {
            Some(max) => format!("{} of {}", grouped(self.current), grouped(max)).into(),
            None => grouped(self.current).into(),
        }
    }
}

fn fraction_of(part: i64, whole: i64) -> Option<f32> {
    (whole > 0).then(|| (part as f32 / whole as f32).clamp(0., 1.))
}

/// The meters `vitals` can fill, in the canvas's order.
fn meters_for(vitals: &VitalsState) -> Vec<Meter> {
    [
        (MeterKind::Hp, Vital::Hp, Vital::MaxHp),
        (MeterKind::Mana, Vital::Mana, Vital::MaxMana),
        (MeterKind::Moves, Vital::Moves, Vital::MaxMoves),
    ]
    .into_iter()
    .filter_map(|(kind, current, max)| {
        Some(Meter {
            kind,
            current: *vitals.0.get(&current)?,
            max: vitals.0.get(&max).copied(),
        })
    })
    .collect()
}

/// "Tamsin", then "L22 · Stand", with what's known of it.
#[derive(Clone, Debug, Default, PartialEq)]
struct Who {
    name: Option<SharedString>,
    about: Option<SharedString>,
}

fn who_of(characters: &CharactersState) -> Who {
    let text = |key: &str| match characters.0.get(key) {
        Some(Value::Text(text)) if !text.is_empty() => Some(text.clone()),
        _ => None,
    };
    let level = match characters.0.get(CHARACTER_LEVEL) {
        Some(Value::Number(level)) => Some(format!("L{level}")),
        _ => None,
    };
    let about: Vec<String> = [level, text(CHARACTER_POSITION)]
        .into_iter()
        .flatten()
        .collect();
    Who {
        name: text(CHARACTER_NAME).map(Into::into),
        about: (!about.is_empty()).then(|| about.join(" · ").into()),
    }
}

/// Who the character is fighting.
#[derive(Clone, Debug, PartialEq)]
struct Opponent {
    name: SharedString,
    level: Option<i64>,
    /// "41%", or the bare number where the game gives no maximum.
    health: Option<SharedString>,
    fraction: Option<f32>,
}

impl Opponent {
    /// "A rat · 41%".
    fn text(&self) -> SharedString {
        match &self.health {
            Some(health) => format!("{} · {health}", self.name).into(),
            None => self.name.clone(),
        }
    }
}

fn opponent_of(target: &TargetState) -> Option<Opponent> {
    let target = target.0.as_ref()?;
    let fraction = target.health.zip(target.max_health);
    let fraction = fraction.and_then(|(health, max)| fraction_of(health, max));
    let health = match (fraction, target.health) {
        (Some(fraction), _) => Some(format!("{:.0}%", fraction * 100.).into()),
        (None, Some(health)) => Some(grouped(health).into()),
        (None, None) => None,
    };
    Some(Opponent {
        name: target.name.clone().into(),
        level: target.level,
        health,
        fraction,
    })
}

/// The experience left to earn this level.
#[derive(Clone, Debug, PartialEq)]
struct Experience {
    to_go: i64,
    /// How far through the level the character is.
    fraction: Option<f32>,
}

fn experience_of(vitals: &VitalsState) -> Option<Experience> {
    let to_go = *vitals.0.get(&Vital::ToNextLevel)?;
    let of = vitals.0.get(&Vital::MaxToNextLevel);
    Some(Experience {
        to_go,
        fraction: of.and_then(|&of| fraction_of(of - to_go, of)),
    })
}

/// "bless for 6 hours": each affect, as the game words it.
fn affects_of(characters: &CharactersState) -> Vec<SharedString> {
    characters
        .affects()
        .into_iter()
        .map(|affect| {
            let text = format!("{} {}", affect.name, affect.detail);
            text.trim_end().to_owned().into()
        })
        .collect()
}

/// Keeps a card of `card` size wholly inside `area`, or as far as it fits.
fn inside(at: Point<Pixels>, card: Size<Pixels>, area: Size<Pixels>) -> Point<Pixels> {
    point(
        at.x.min(area.width - card.width).max(px(0.)),
        at.y.min(area.height - card.height).max(px(0.)),
    )
}

pub enum HudEvent {
    /// The player left the card here, in whole pixels from the top left of
    /// the window under the title bar.
    CardMoved((i32, i32)),
}

/// Marks a drag of the card.
struct DraggingCard;

impl Render for DraggingCard {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

pub struct Hud {
    meters: Vec<Meter>,
    who: Who,
    opponent: Option<Opponent>,
    experience: Option<Experience>,
    affects: Vec<SharedString>,
    /// The attached row shows its details.
    open: bool,
    /// Where the mouse has the card, until it's let go.
    dragged_to: Option<Point<Pixels>>,
    /// Where in the card the mouse took hold.
    grabbed_at: Point<Pixels>,
    toggle: FocusHandle,
    card: FocusHandle,
}

impl EventEmitter<HudEvent> for Hud {}

impl Hud {
    pub fn new(cx: &mut Context<Hud>) -> Hud {
        Hud {
            meters: Vec::new(),
            who: Who::default(),
            opponent: None,
            experience: None,
            affects: Vec::new(),
            open: false,
            dragged_to: None,
            grabbed_at: Point::default(),
            toggle: cx.focus_handle().tab_stop(true),
            card: cx.focus_handle().tab_stop(true),
        }
    }

    pub fn show(
        &mut self,
        vitals: &VitalsState,
        characters: &CharactersState,
        target: &TargetState,
        cx: &mut Context<Hud>,
    ) {
        let meters = meters_for(vitals);
        let who = who_of(characters);
        let opponent = opponent_of(target);
        let experience = experience_of(vitals);
        let affects = affects_of(characters);
        if meters != self.meters
            || who != self.who
            || opponent != self.opponent
            || experience != self.experience
            || affects != self.affects
        {
            self.meters = meters;
            self.who = who;
            self.opponent = opponent;
            self.experience = experience;
            self.affects = affects;
            cx.notify();
        }
    }

    #[cfg(test)]
    pub fn values(&self) -> Vec<SharedString> {
        self.meters.iter().map(Meter::value).collect()
    }

    #[cfg(test)]
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn set_open(&mut self, open: bool, cx: &mut Context<Hud>) {
        self.open = open;
        cx.notify();
    }

    /// What the HUD's shortcut does: opens or closes the attached row, or
    /// brings the keyboard to the card to move it.
    pub fn use_shortcut(&mut self, window: &mut Window, cx: &mut Context<Hud>) {
        match cx.global::<Theme>().choice.hud {
            HudPlacement::Attached if self.has_details() => self.set_open(!self.open, cx),
            HudPlacement::Floating if !self.meters.is_empty() => window.focus(&self.card, cx),
            _ => {}
        }
    }

    fn has_details(&self) -> bool {
        self.opponent.is_some() || self.experience.is_some() || !self.affects.is_empty()
    }

    /// The window under the title bar, which is where the card goes.
    fn card_area(window: &Window) -> Size<Pixels> {
        let window = window.viewport_size();
        size(window.width, window.height - title_bar::HEIGHT)
    }

    fn card_size(&self) -> Size<Pixels> {
        let rows = usize::from(self.who != Who::default()) + self.meters.len();
        let target = self
            .opponent
            .as_ref()
            .map_or(px(0.), |_| CARD_TARGET_HEIGHT);
        let height = CARD_PADDING * 2. + CARD_GRIP_HEIGHT + CARD_ROW_HEIGHT * rows as f32 + target;
        size(CARD_WIDTH, height)
    }

    /// Where the card is drawn: where it was left, inside the window.
    fn card_at(&self, window: &Window, cx: &App) -> Point<Pixels> {
        let area = Self::card_area(window);
        let left = cx.global::<Theme>().choice.card;
        let left = left.map(|(x, y)| point(px(x as f32), px(y as f32)));
        let at = self
            .dragged_to
            .or(left)
            .unwrap_or_else(|| point(area.width - CARD_WIDTH - CARD_MARGIN, CARD_MARGIN));
        inside(at, self.card_size(), area)
    }

    /// Has the mouse hold the card at `at`. It's kept once it's let go.
    pub fn drag_card_to(&mut self, at: Point<Pixels>, window: &Window, cx: &mut Context<Hud>) {
        self.dragged_to = Some(inside(at, self.card_size(), Self::card_area(window)));
        cx.notify();
    }

    fn step_card(&mut self, by: Point<Pixels>, window: &Window, cx: &mut Context<Hud>) {
        let at = inside(
            self.card_at(window, cx) + by,
            self.card_size(),
            Self::card_area(window),
        );
        emit_card_moved(at, cx);
    }

    fn card_let_go(&mut self, cx: &mut Context<Hud>) {
        if let Some(at) = self.dragged_to.take() {
            emit_card_moved(at, cx);
        }
    }

    fn render_row(&self, theme: &Theme) -> Stateful<Div> {
        div()
            .id("hud")
            .debug_selector(|| "hud".to_owned())
            .role(Role::Group)
            .aria_label("Status")
            .key_context(CONTEXT)
            .min_h(ROW_HEIGHT)
            .px_4()
            .py_2()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x_6()
            .gap_y_1()
            .text_size(px(13.))
            .border_b_1()
            .border_color(theme.border)
            .child(render_who(&self.who, theme))
            .children(self.meters.iter().map(|meter| {
                render_meter(meter, METER_WIDTH, theme).child(render_value(meter, theme))
            }))
    }

    fn render_toggle(&self, theme: &Theme, cx: &mut Context<Hud>) -> impl IntoElement + use<> {
        let open = self.open;
        let ring = theme.accent;
        let chevron = Icon::ChevronDown
            .svg()
            .size(CHEVRON_SIZE)
            .text_color(theme.text_muted);
        let chevron = if open {
            chevron.with_transformation(Transformation::rotate(percentage(0.5)))
        } else {
            chevron
        };
        div()
            .id("hud-toggle")
            .debug_selector(|| "hud-toggle".to_owned())
            .role(Role::Button)
            .aria_label(if open {
                "Hide status details"
            } else {
                "Show status details"
            })
            .aria_expanded(open)
            .aria_keyshortcuts(title_bar::shortcut_name("j"))
            .track_focus(&self.toggle)
            .ml_auto()
            .flex_none()
            .size(TOGGLE_SIZE)
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .border_1()
            .border_color(gpui::transparent_black())
            .hover({
                let hover = theme.hover;
                move |style| style.bg(hover)
            })
            .focus_visible(move |style| style.border_color(ring))
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .on_click(cx.listener(move |hud, _, _, cx| hud.set_open(!open, cx)))
            .child(chevron)
    }

    /// What the attached row opens to show. Only what the game gives is here.
    fn render_details(&self, theme: &Theme) -> impl IntoElement + use<> {
        let opponent = self.opponent.as_ref().map(|opponent| {
            let level = opponent.level.map(|level| format!("level {level}"));
            detail(
                "opponent",
                opponent.name.clone(),
                level.map(Into::into),
                opponent.fraction,
                theme.hud_hp,
                theme,
            )
            .aria_value(opponent.health.clone().unwrap_or_default())
        });
        let experience = self.experience.as_ref().map(|experience| {
            let to_go: SharedString = format!("{} to go", grouped(experience.to_go)).into();
            detail(
                "next-level",
                "Next level".into(),
                Some(to_go.clone()),
                experience.fraction,
                theme.hud_xp,
                theme,
            )
            .aria_value(to_go)
        });
        let affects = (!self.affects.is_empty()).then(|| {
            div()
                .id("hud-affects")
                .role(Role::Group)
                .aria_label("Affects")
                .flex()
                .flex_wrap()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .mr_1()
                        .text_color(theme.text)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Affects"),
                )
                .children(
                    self.affects
                        .iter()
                        .map(|affect| chip(affect.clone(), theme)),
                )
        });
        div()
            .id("hud-details")
            .debug_selector(|| "hud-details".to_owned())
            .role(Role::Group)
            .aria_label("Status details")
            .px_4()
            .py_2()
            .flex()
            .flex_col()
            .gap_1p5()
            .text_size(px(13.))
            .text_color(theme.text_muted)
            .border_b_1()
            .border_color(theme.border)
            .when(opponent.is_some() || experience.is_some(), |details| {
                details.child(
                    div()
                        .flex()
                        .gap_x_5()
                        .children(opponent)
                        .children(experience),
                )
            })
            .children(affects)
    }

    fn render_card(
        &self,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Hud>,
    ) -> impl IntoElement + use<> {
        let at = self.card_at(window, cx);
        let (ring, hud) = (theme.accent, cx.entity());
        let target = self.opponent.as_ref().map(|opponent| {
            div()
                .flex_none()
                .h(CARD_TARGET_HEIGHT)
                .flex()
                .items_end()
                .border_t_1()
                .border_color(theme.border)
                .text_color(theme.text_muted)
                .child(elided(opponent.text()))
        });
        let card = div()
            .id("hud-card")
            .debug_selector(|| "hud-card".to_owned())
            .role(Role::Group)
            .aria_label("Status card. The arrow keys move it.")
            .key_context(CARD_CONTEXT)
            .track_focus(&self.card)
            .absolute()
            .left(at.x)
            .top(at.y)
            .w(CARD_WIDTH)
            .h(self.card_size().height)
            .p(CARD_PADDING - px(1.))
            .flex()
            .flex_col()
            .rounded_xl()
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_raised)
            .shadow(theme.floating_shadow())
            .text_size(px(12.))
            .cursor_grab()
            .block_mouse_except_scroll()
            .focus_visible(move |style| style.border_color(ring))
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .on_drag(DraggingCard, move |_, grabbed_at, _, cx| {
                cx.stop_propagation();
                let dragging = cx.new(|_| DraggingCard);
                hud.update(cx, |hud, cx| {
                    hud.grabbed_at = grabbed_at;
                    cx.observe_release(&dragging, |hud, _, cx| hud.card_let_go(cx))
                        .detach();
                });
                dragging
            })
            .on_action(cx.listener(|hud, _: &MoveCardUp, window, cx| {
                hud.step_card(point(px(0.), -CARD_STEP), window, cx);
            }))
            .on_action(cx.listener(|hud, _: &MoveCardDown, window, cx| {
                hud.step_card(point(px(0.), CARD_STEP), window, cx);
            }))
            .on_action(cx.listener(|hud, _: &MoveCardLeft, window, cx| {
                hud.step_card(point(-CARD_STEP, px(0.)), window, cx);
            }))
            .on_action(cx.listener(|hud, _: &MoveCardRight, window, cx| {
                hud.step_card(point(CARD_STEP, px(0.)), window, cx);
            }))
            .child(
                div()
                    .flex_none()
                    .h(CARD_GRIP_HEIGHT)
                    .flex()
                    .justify_center()
                    .text_color(theme.text_muted)
                    .line_height(px(6.))
                    .child("······"),
            )
            .when(self.who != Who::default(), |card| {
                card.child(
                    div()
                        .flex_none()
                        .h(CARD_ROW_HEIGHT)
                        .flex()
                        .items_center()
                        .child(render_who(&self.who, theme)),
                )
            })
            .children(self.meters.iter().map(|meter| {
                let row = render_meter(meter, CARD_METER_WIDTH, theme)
                    .h(CARD_ROW_HEIGHT)
                    .justify_between();
                // With no maximum there's no bar, so the number takes its place.
                row.when(meter.fraction().is_none(), |row| {
                    row.child(render_value(meter, theme))
                })
            }))
            .children(target);
        // The whole window, so a drag is followed wherever the mouse goes.
        div()
            .id("hud-card-area")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .key_context(CONTEXT)
            .on_drag_move(
                cx.listener(|hud, event: &DragMoveEvent<DraggingCard>, window, cx| {
                    let at = event.event.position - event.bounds.origin - hud.grabbed_at;
                    hud.drag_card_to(at, window, cx);
                }),
            )
            .child(card)
    }
}

fn emit_card_moved(at: Point<Pixels>, cx: &mut Context<Hud>) {
    let at = (
        f32::from(at.x).round() as i32,
        f32::from(at.y).round() as i32,
    );
    cx.emit(HudEvent::CardMoved(at));
}

/// Text that stops at the edge of its row. A screen reader gets it all.
fn elided(text: SharedString) -> Stateful<Div> {
    div()
        .id(text.clone())
        .role(Role::Label)
        .aria_value(text.clone())
        .min_w_0()
        .truncate()
        .child(text)
}

fn render_who(who: &Who, theme: &Theme) -> impl IntoElement + use<> {
    let text = [who.name.as_ref(), who.about.as_ref()]
        .into_iter()
        .flatten()
        .map(SharedString::as_ref)
        .collect::<Vec<&str>>()
        .join(" · ");
    div()
        .id("hud-who")
        .role(Role::Label)
        .aria_value(text)
        .flex_none()
        .flex()
        .items_baseline()
        .gap_1()
        .children(who.name.clone().map(|name| {
            div()
                .text_color(theme.text)
                .font_weight(FontWeight::SEMIBOLD)
                .child(name)
        }))
        .children(
            who.about
                .clone()
                .map(|about| div().text_color(theme.text_muted).child(about)),
        )
}

/// A bar of `width`, filled to `fraction`.
fn track(fraction: f32, width: impl Into<Length>, color: Hsla, theme: &Theme) -> Div {
    div()
        .w(width.into())
        .h(METER_HEIGHT)
        .flex_none()
        .rounded_full()
        .bg(theme.hud_track)
        .child(
            div()
                .w(relative(fraction))
                .h_full()
                .rounded_full()
                .bg(color),
        )
}

fn render_meter(meter: &Meter, width: Pixels, theme: &Theme) -> Stateful<Div> {
    let (label, name) = meter.kind.labels();
    div()
        .id(label)
        .debug_selector(move || format!("meter-{label}"))
        .role(Role::Meter)
        .aria_label(name)
        .aria_value(meter.spoken())
        .flex_none()
        .flex()
        .items_center()
        .gap_2()
        .child(div().text_color(theme.text_muted).child(label))
        .children(
            meter
                .fraction()
                .map(|fraction| track(fraction, width, meter.kind.color(theme), theme)),
        )
}

fn render_value(meter: &Meter, theme: &Theme) -> impl IntoElement + use<> {
    div().text_color(theme.text).child(meter.value())
}

/// One half of the opened row: a heading, a note beside it and a bar under it.
fn detail(
    id: &'static str,
    heading: SharedString,
    note: Option<SharedString>,
    fraction: Option<f32>,
    color: Hsla,
    theme: &Theme,
) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || format!("hud-{id}"))
        .role(Role::Meter)
        .aria_label(heading.clone())
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .gap_1()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(theme.text)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(heading),
                )
                .children(note.map(|note| div().flex_none().child(format!("· {note}")))),
        )
        .children(fraction.map(|fraction| track(fraction, relative(1.), color, theme)))
}

fn chip(text: SharedString, theme: &Theme) -> impl IntoElement + use<> {
    div()
        .id(text.clone())
        .role(Role::Label)
        .aria_value(text.clone())
        .px_1p5()
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .text_size(px(12.))
        .child(text)
}

impl Render for Hud {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Hud>) -> impl IntoElement {
        if self.meters.is_empty() {
            return div().into_any_element();
        }
        let theme = *cx.global::<Theme>();
        match theme.choice.hud {
            HudPlacement::Attached => div()
                .flex()
                .flex_col()
                .when(self.open && self.has_details(), |hud| {
                    hud.child(self.render_details(&theme))
                })
                .child(self.render_row(&theme).when(self.has_details(), |row| {
                    row.child(self.render_toggle(&theme, cx))
                }))
                .into_any_element(),
            HudPlacement::Floating => self.render_card(&theme, window, cx).into_any_element(),
            HudPlacement::TopStrip => self
                .render_row(&theme)
                .flex_none()
                .bg(theme.surface)
                .children(self.opponent.as_ref().map(|opponent| {
                    elided(opponent.text())
                        .flex_none()
                        .text_color(theme.text_muted)
                }))
                .into_any_element(),
            HudPlacement::Hidden => div().into_any_element(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mudling_core::store::{Affect, Target};
    use std::collections::BTreeMap;

    fn vitals(values: &[(Vital, i64)]) -> VitalsState {
        VitalsState(BTreeMap::from_iter(values.iter().cloned()))
    }

    #[test]
    fn a_meter_needs_its_current_value_and_fills_against_its_maximum() {
        let meters = meters_for(&vitals(&[
            (Vital::Moves, 110),
            (Vital::MaxMoves, 0),
            (Vital::Hp, 30),
            (Vital::MaxHp, 42),
            (Vital::MaxMana, 115),
        ]));
        let kinds: Vec<_> = meters.iter().map(|meter| meter.kind).collect();
        assert_eq!(kinds, [MeterKind::Hp, MeterKind::Moves]);
        assert_eq!(meters[0].value(), "30/42");
        assert_eq!(meters[0].spoken(), "30 of 42");
        assert_eq!(meters[0].fraction(), Some(30. / 42.));
        assert_eq!(meters[1].fraction(), None, "no bar without a maximum");
    }

    #[test]
    fn a_vital_past_its_maximum_fills_the_bar_and_no_more() {
        let meters = meters_for(&vitals(&[(Vital::Hp, 50), (Vital::MaxHp, 42)]));
        assert_eq!(meters[0].fraction(), Some(1.));
    }

    #[test]
    fn large_numbers_read_with_commas() {
        assert_eq!(grouped(812), "812");
        assert_eq!(grouped(3_120), "3,120");
        assert_eq!(grouped(-1_234_567), "-1,234,567");
    }

    #[test]
    fn the_character_reads_as_what_is_known_of_it() {
        assert_eq!(who_of(&CharactersState::default()), Who::default());
        let characters = CharactersState(BTreeMap::from([
            (CHARACTER_NAME.to_owned(), Value::Text("Tamsin".to_owned())),
            (CHARACTER_LEVEL.to_owned(), Value::Number(22.)),
            (CHARACTER_POSITION.to_owned(), Value::Text(String::new())),
        ]));
        let who = who_of(&characters);
        assert_eq!(who.name.unwrap(), "Tamsin");
        assert_eq!(who.about.unwrap(), "L22");
    }

    #[test]
    fn the_opponent_reads_as_its_name_and_share_of_health() {
        assert_eq!(opponent_of(&TargetState(None)), None);
        let target = |health, max_health| {
            TargetState(Some(Target {
                name: "A rat".to_owned(),
                health,
                max_health,
                level: None,
            }))
        };
        let text = |state: &TargetState| opponent_of(state).unwrap().text();
        assert_eq!(text(&target(Some(36), Some(120))), "A rat · 30%");
        assert_eq!(text(&target(Some(36), None)), "A rat · 36");
        assert_eq!(text(&target(None, None)), "A rat");
    }

    #[test]
    fn experience_fills_as_it_is_earned_and_a_game_without_it_shows_none() {
        assert_eq!(experience_of(&vitals(&[(Vital::Hp, 1)])), None);
        let earned = experience_of(&vitals(&[
            (Vital::ToNextLevel, 500),
            (Vital::MaxToNextLevel, 2_000),
        ]));
        assert_eq!(
            earned,
            Some(Experience {
                to_go: 500,
                fraction: Some(0.75)
            })
        );
    }

    #[test]
    fn an_affect_reads_with_how_long_it_lasts_if_the_game_says() {
        let listed = |name: &str, detail: &str| Affect {
            name: name.to_owned(),
            detail: detail.to_owned(),
        };
        let affects = [listed("bless", "for 6 hours"), listed("haste", "")];
        let characters = CharactersState::listing(&affects);
        assert_eq!(affects_of(&characters), ["bless for 6 hours", "haste"]);
    }

    #[test]
    fn the_card_stays_inside_the_area_and_fits_a_smaller_one_from_its_corner() {
        let card = size(px(200.), px(100.));
        let area = size(px(800.), px(600.));
        assert_eq!(
            inside(point(px(10.), px(20.)), card, area),
            point(px(10.), px(20.))
        );
        assert_eq!(
            inside(point(px(900.), px(-5.)), card, area),
            point(px(600.), px(0.))
        );
        assert_eq!(
            inside(point(px(50.), px(50.)), card, size(px(100.), px(50.))),
            point(px(0.), px(0.))
        );
    }
}
