//! The Appearance section: the theme, the chat's font and size, where the HUD
//! sits and whether prompt lines show.

use super::{Pane, SettingsSheet, chosen_ground, control, muted_line, section_heading};
use crate::assets::{Icon, UI_FONT};
use crate::hud::HudPlacement;
use crate::theme::{Appearance, LARGEST_CHAT_SIZE, SMALLEST_CHAT_SIZE, Theme, ThemeChoice};
use gpui::{Context, FontWeight, Role, accesskit, div, prelude::*, px};

const THEMES: [(ThemeChoice, &str); 3] = [
    (ThemeChoice::System, "System"),
    (ThemeChoice::Light, "Light"),
    (ThemeChoice::Dark, "Dark"),
];
const LATER_NOTE: &str = "Line width, panels and motion aren't here yet.";
const PROMPTS_ON_NOTE: &str = "On while the HUD is hidden: nothing else shows their numbers.";
const SAMPLE: &str = "The chat will read like this, with 80 columns to itself.";

pub(super) fn toggled(on: bool) -> accesskit::Toggled {
    on.into()
}

impl SettingsSheet {
    pub(super) fn fill_appearance(
        &self,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) {
        let choice = theme.choice;
        pane.row(section_heading("Theme", theme));
        let segments: Vec<_> = THEMES
            .into_iter()
            .map(|(option, label)| {
                let chosen = choice.theme == option;
                control(format!("theme-{label}"), label, &pane.stop(cx), theme)
                    .role(Role::RadioButton)
                    .aria_toggled(toggled(chosen))
                    .flex_1()
                    .h(px(34.))
                    .bg(chosen_ground(chosen, theme))
                    .text_size(px(14.))
                    .text_color(theme.text)
                    .child(label)
                    .on_click(cx.listener(move |sheet, _, _, cx| {
                        let theme = option;
                        sheet.choose(Appearance { theme, ..choice }, cx);
                    }))
            })
            .collect();
        pane.row(
            div()
                .id("theme-choice")
                .role(Role::RadioGroup)
                .aria_label("Theme")
                .w(px(330.))
                .p_0p5()
                .flex()
                .gap_0p5()
                .rounded_lg()
                .bg(theme.segment)
                .children(segments),
        );

        pane.row(section_heading("Chat font", theme));
        let faces: Vec<_> = self
            .fonts
            .iter()
            .map(|&font| {
                let chosen = choice.font == font;
                control(format!("chat-font-{font}"), font, &pane.stop(cx), theme)
                    .role(Role::RadioButton)
                    .aria_toggled(toggled(chosen))
                    .w_full()
                    .h(px(36.))
                    .px_3()
                    .justify_start()
                    .bg(chosen_ground(chosen, theme))
                    .font_family(font)
                    .text_size(px(14.))
                    .text_color(theme.text)
                    .child(font)
                    .on_click(cx.listener(move |sheet, _, _, cx| {
                        sheet.choose(Appearance { font, ..choice }, cx);
                    }))
            })
            .collect();
        pane.row(
            div()
                .id("chat-font-choice")
                .role(Role::RadioGroup)
                .aria_label("Chat font")
                .flex()
                .flex_col()
                .gap_0p5()
                .children(faces),
        );

        pane.row(section_heading("Chat size", theme));
        let size = choice.font_size;
        let mut stepper =
            |id: &'static str, label: &'static str, glyph, to: u8, pane: &mut Pane| {
                control(id, label, &pane.stop(cx), theme)
                    .size(px(34.))
                    .border_color(theme.border)
                    .text_size(px(16.))
                    .text_color(theme.text)
                    .child(glyph)
                    .on_click(cx.listener(move |sheet, _, _, cx| {
                        sheet.choose(choice.with_font_size(to), cx);
                    }))
            };
        let smaller = stepper(
            "chat-size-smaller",
            "Smaller chat text",
            "−",
            size.saturating_sub(1).max(SMALLEST_CHAT_SIZE),
            pane,
        );
        let larger = stepper(
            "chat-size-larger",
            "Larger chat text",
            "+",
            size.saturating_add(1).min(LARGEST_CHAT_SIZE),
            pane,
        );
        pane.row(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(smaller)
                .child(
                    div()
                        .id("chat-size")
                        .role(Role::Label)
                        .aria_label("Chat size")
                        .aria_value(format!("{size} points"))
                        .min_w(px(48.))
                        .flex()
                        .justify_center()
                        .font_family(UI_FONT)
                        .font_weight(FontWeight::MEDIUM)
                        .text_size(px(14.))
                        .child(format!("{size} pt")),
                )
                .child(larger),
        );
        pane.row(
            muted_line(SAMPLE, theme)
                .pt_4()
                .pb_3()
                .font_family(choice.font)
                .text_size(choice.chat_size())
                .line_height(choice.chat_line_height())
                .text_color(theme.text),
        );

        pane.row(section_heading("HUD placement", theme));
        let placements: Vec<_> = HudPlacement::ALL
            .into_iter()
            .map(|(hud, label)| {
                let chosen = choice.hud == hud;
                control(format!("hud-{label}"), label, &pane.stop(cx), theme)
                    .role(Role::RadioButton)
                    .aria_toggled(toggled(chosen))
                    .flex_1()
                    .h(px(34.))
                    .bg(chosen_ground(chosen, theme))
                    .text_size(px(14.))
                    .text_color(theme.text)
                    .child(label)
                    .on_click(cx.listener(move |sheet, _, _, cx| {
                        sheet.choose(Appearance { hud, ..choice }, cx);
                    }))
            })
            .collect();
        pane.row(
            div()
                .id("hud-placement")
                .role(Role::RadioGroup)
                .aria_label("HUD placement")
                .p_0p5()
                .flex()
                .gap_0p5()
                .rounded_lg()
                .bg(theme.segment)
                .children(placements),
        );

        pane.row(section_heading("Prompt lines", theme));
        let shown = choice.shows_prompts();
        let forced = shown && !choice.show_prompts;
        let show_prompts = !choice.show_prompts;
        let stop = pane.stop(cx);
        pane.row(
            control("show-prompts", "Show prompt lines", &stop, theme)
                .role(Role::CheckBox)
                .aria_toggled(toggled(shown))
                .h(px(36.))
                .px_3()
                .gap_2()
                .justify_start()
                .font_family(UI_FONT)
                .text_size(px(14.))
                .text_color(theme.text)
                .child(
                    div()
                        .size(px(16.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_sm()
                        .border_1()
                        .border_color(theme.border)
                        .when(shown, |box_| {
                            box_.child(Icon::Check.svg().size(px(12.)).text_color(theme.accent))
                        }),
                )
                .child("Show prompt lines")
                .when(!forced, |checkbox| {
                    checkbox.on_click(cx.listener(move |sheet, _, _, cx| {
                        sheet.choose(
                            Appearance {
                                show_prompts,
                                ..choice
                            },
                            cx,
                        );
                    }))
                }),
        );
        if forced {
            pane.row(muted_line(PROMPTS_ON_NOTE, theme));
        }
        pane.row(muted_line(LATER_NOTE, theme));
    }
}
