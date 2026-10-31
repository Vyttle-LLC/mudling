//! The HUD row on the command bar: the character and what it's doing, a meter
//! for each vital the server reports, and who it's fighting; nothing until
//! the server reports a vital. When the bar is too narrow for them all the
//! row wraps, so no vital is ever clipped.

use crate::theme::Theme;
use gpui::{Context, Hsla, Pixels, Role, SharedString, Window, div, prelude::*, px};
use mudling_core::defaults::hud::Vital;
use mudling_core::plugin::luau::Value;
use mudling_core::store::{
    CHARACTER_LEVEL, CHARACTER_NAME, CHARACTER_POSITION, CharactersState, TargetState, VitalsState,
};

const ROW_HEIGHT: Pixels = px(40.);
const METER_WIDTH: Pixels = px(64.);
const METER_HEIGHT: Pixels = px(4.);

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
        let max = self.max.filter(|max| *max > 0)?;
        Some((self.current as f32 / max as f32).clamp(0., 1.))
    }

    fn value(&self) -> SharedString {
        match self.max {
            Some(max) => format!("{}/{max}", self.current).into(),
            None => self.current.to_string().into(),
        }
    }
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

/// "Tamsin · Level 22 · Stand", with what's known of it.
fn character_text(characters: &CharactersState) -> Option<SharedString> {
    let text = |key: &str| match characters.0.get(key) {
        Some(Value::Text(text)) if !text.is_empty() => Some(text.clone()),
        _ => None,
    };
    let level = match characters.0.get(CHARACTER_LEVEL) {
        Some(Value::Number(level)) => Some(format!("Level {level}")),
        _ => None,
    };
    let parts: Vec<String> = [text(CHARACTER_NAME), level, text(CHARACTER_POSITION)]
        .into_iter()
        .flatten()
        .collect();
    (!parts.is_empty()).then(|| parts.join(" · ").into())
}

/// "A rat · 36/100", or that there's no one.
fn target_text(target: &TargetState) -> SharedString {
    let Some(target) = &target.0 else {
        return "No target".into();
    };
    match (target.health, target.max_health) {
        (Some(health), Some(max)) => format!("{} · {health}/{max}", target.name).into(),
        (Some(health), None) => format!("{} · {health}", target.name).into(),
        _ => target.name.clone().into(),
    }
}

#[derive(Default)]
pub struct Hud {
    meters: Vec<Meter>,
    character: Option<SharedString>,
    target: SharedString,
}

impl Hud {
    pub fn show(
        &mut self,
        vitals: &VitalsState,
        characters: &CharactersState,
        target: &TargetState,
        cx: &mut Context<Hud>,
    ) {
        let meters = meters_for(vitals);
        let character = character_text(characters);
        let target = target_text(target);
        if meters != self.meters || character != self.character || target != self.target {
            self.meters = meters;
            self.character = character;
            self.target = target;
            cx.notify();
        }
    }

    #[cfg(test)]
    pub fn values(&self) -> Vec<SharedString> {
        self.meters.iter().map(Meter::value).collect()
    }
}

fn render_meter(meter: &Meter, theme: &Theme) -> impl IntoElement {
    let (label, name) = meter.kind.labels();
    let value = meter.value();
    let track = meter.fraction().map(|fraction| {
        div()
            .w(METER_WIDTH)
            .h(METER_HEIGHT)
            .rounded_full()
            .bg(theme.hud_track)
            .child(
                div()
                    .w(METER_WIDTH * fraction)
                    .h_full()
                    .rounded_full()
                    .bg(meter.kind.color(theme)),
            )
    });
    div()
        .id(label)
        .debug_selector(move || format!("meter-{label}"))
        .role(Role::Meter)
        .aria_label(name)
        .aria_value(value.clone())
        .flex_none()
        .flex()
        .items_center()
        .gap_2()
        .child(div().text_color(theme.text_muted).child(label))
        .children(track)
        .child(div().text_color(theme.text).child(value))
}

impl Render for Hud {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Hud>) -> impl IntoElement {
        if self.meters.is_empty() {
            return div().into_any_element();
        }
        let theme = cx.global::<Theme>();
        div()
            .id("hud")
            .role(Role::Group)
            .aria_label("Status")
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
            .children(
                self.character
                    .clone()
                    .map(|character| div().flex_none().text_color(theme.text).child(character)),
            )
            .children(self.meters.iter().map(|meter| render_meter(meter, theme)))
            .child(
                div()
                    .flex_none()
                    .text_color(theme.text_muted)
                    .child(self.target.clone()),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mudling_core::store::Target;
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
        assert_eq!(meters[0].fraction(), Some(30. / 42.));
        assert_eq!(meters[1].fraction(), None, "no bar without a maximum");
    }

    #[test]
    fn a_vital_past_its_maximum_fills_the_bar_and_no_more() {
        let meters = meters_for(&vitals(&[(Vital::Hp, 50), (Vital::MaxHp, 42)]));
        assert_eq!(meters[0].fraction(), Some(1.));
    }

    #[test]
    fn the_character_reads_as_what_is_known_of_it() {
        assert_eq!(character_text(&CharactersState::default()), None);
        let characters = CharactersState(BTreeMap::from([
            (CHARACTER_NAME.to_owned(), Value::Text("Tamsin".to_owned())),
            (CHARACTER_LEVEL.to_owned(), Value::Number(22.)),
            (CHARACTER_POSITION.to_owned(), Value::Text(String::new())),
        ]));
        assert_eq!(character_text(&characters).unwrap(), "Tamsin · Level 22");
    }

    #[test]
    fn the_target_reads_as_its_name_and_health() {
        assert_eq!(target_text(&TargetState(None)), "No target");
        let target = Target {
            name: "A rat".to_owned(),
            health: Some(36),
            max_health: Some(100),
            level: None,
        };
        assert_eq!(target_text(&TargetState(Some(target))), "A rat · 36/100");
    }
}
