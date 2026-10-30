//! The HUD row on the command bar: how the connection stands, then a meter
//! for each vital the server reports.

use crate::theme::Theme;
use gpui::{Context, FontWeight, Hsla, Pixels, Role, SharedString, Window, div, prelude::*, px};
use mudling_core::defaults::hud::Vital;
use mudling_core::store::VitalsState;

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

#[derive(Default)]
pub struct Hud {
    connection: SharedString,
    meters: Vec<Meter>,
}

impl Hud {
    pub fn show(&mut self, connection: SharedString, vitals: &VitalsState, cx: &mut Context<Hud>) {
        let meters = meters_for(vitals);
        if connection != self.connection || meters != self.meters {
            self.connection = connection;
            self.meters = meters;
            cx.notify();
        }
    }

    #[cfg(test)]
    pub fn connection(&self) -> &str {
        &self.connection
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
        let theme = cx.global::<Theme>();
        div()
            .id("hud")
            .role(Role::Group)
            .aria_label("Status")
            .h(ROW_HEIGHT)
            .px_4()
            .flex()
            .items_center()
            .gap_6()
            .overflow_hidden()
            .text_size(px(13.))
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .id("connection")
                    .role(Role::Label)
                    .aria_value(self.connection.clone())
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(self.connection.clone()),
            )
            .children(self.meters.iter().map(|meter| render_meter(meter, theme)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
