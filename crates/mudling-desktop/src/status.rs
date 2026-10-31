//! A quiet line above the command bar: the newest notice. The connection's
//! words live in the title bar, and what the player can do about them in the
//! session bar.

use crate::assets::UI_FONT;
use crate::theme::Theme;
use gpui::{Context, Pixels, Role, SharedString, Window, div, prelude::*, px};
use mudling_core::store::SettingsState;

const FONT_SIZE: Pixels = px(12.);

#[derive(Default)]
pub struct StatusLines {
    lines: Vec<SharedString>,
}

impl StatusLines {
    pub fn show(&mut self, lines: Vec<SharedString>, cx: &mut Context<StatusLines>) {
        if lines != self.lines {
            self.lines = lines;
            cx.notify();
        }
    }

    #[cfg(test)]
    pub fn lines(&self) -> &[SharedString] {
        &self.lines
    }
}

/// What the lines say.
pub fn lines_for(settings: &SettingsState) -> Vec<SharedString> {
    settings
        .notices
        .last()
        .cloned()
        .into_iter()
        .map(SharedString::from)
        .collect()
}

impl Render for StatusLines {
    fn render(&mut self, _: &mut Window, cx: &mut Context<StatusLines>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        div()
            .w_full()
            .font_family(UI_FONT)
            .text_size(FONT_SIZE)
            .text_color(theme.text_muted)
            .children(self.lines.iter().enumerate().map(|(number, line)| {
                div()
                    .id(("status-line", number))
                    .role(Role::Label)
                    .aria_value(line.clone())
                    .pb_1()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(line.clone())
            }))
    }
}
