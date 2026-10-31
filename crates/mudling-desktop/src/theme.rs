//! The Runestone tokens: every color the window draws, for each appearance.
//! Values come from the MUDling brand guidelines and the design canvas.

use gpui::{BoxShadow, Global, Hsla, WindowAppearance, point, px, rgb, rgba};
use mudling_core::Color;

const NIGHT: u32 = 0x111A16;
const PARCHMENT: u32 = 0xF8F7F2;
const STONE: u32 = 0xB9AB98;
const SLATE: u32 = 0x333C39;
const MOSS: u32 = 0x9BAF55;
const MOSS_BRIGHT: u32 = 0xB5C77D;
const MOSS_DEEP: u32 = 0x5F7026;
const MOSS_DIM: u32 = 0x49571C;

/// The 16 basic ANSI colors, tuned to stay readable on each ground.
const ANSI_ON_NIGHT: [u32; 16] = [
    0x4A524E, 0xE06C75, 0x98C379, 0xE5C07B, 0x61AFEF, 0xC678DD, 0x56B6C2, 0xB9AB98, //
    0x6E7772, 0xF08A93, 0xB5C77D, 0xE5C04F, 0x82AAFF, 0xD7A6F0, 0x7FD4DE, 0xF8F7F2,
];
const ANSI_ON_PARCHMENT: [u32; 16] = [
    0x333C39, 0xA81E33, 0x1F6B45, 0x7A5A0B, 0x1F4F99, 0x7A2E8E, 0x12707A, 0x5E625F, //
    0x6E7772, 0xC8323F, 0x2F7D3B, 0x8A6A00, 0x2B63C4, 0x9A3FB0, 0x1C8A94, 0x111A16,
];

#[derive(Clone, Copy)]
pub struct Theme {
    pub background: Hsla,
    /// Panels, a step up from the background.
    pub surface: Hsla,
    /// What floats: the command bar, cards.
    pub surface_raised: Hsla,
    /// The groove behind a segmented control.
    pub segment: Hsla,
    /// The chosen segment, or the row under selection.
    pub selected: Hsla,
    pub hover: Hsla,
    /// Lines between regions of the same surface.
    pub divider: Hsla,
    /// The outline of a control or a floating surface.
    pub border: Hsla,
    pub shadow: Hsla,
    pub text: Hsla,
    /// Labels and hints.
    pub text_muted: Hsla,
    /// The prompt before an echoed command, which tells it from server text.
    pub command: Hsla,
    /// The runestone in the logomark; its cursor is the accent.
    pub runestone: Hsla,
    pub accent: Hsla,
    pub accent_hover: Hsla,
    /// Text and icons on a filled accent.
    pub on_accent: Hsla,
    pub placeholder: Hsla,
    /// Words that say what went wrong.
    pub error: Hsla,
    pub hud_hp: Hsla,
    pub hud_mana: Hsla,
    pub hud_moves: Hsla,
    pub hud_track: Hsla,
    ansi: [u32; 16],
}

impl Global for Theme {}

impl Theme {
    pub fn for_appearance(appearance: WindowAppearance) -> Theme {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Theme {
                background: rgb(NIGHT).into(),
                surface: rgb(0x151E1B).into(),
                surface_raised: rgb(0x1B2620).into(),
                segment: rgb(0x222D27).into(),
                selected: rgb(0x1B2620).into(),
                hover: rgba(0x9BAF551F).into(),
                divider: rgb(0x1F2823).into(),
                border: rgb(0x2A3A32).into(),
                shadow: rgba(0x00000066).into(),
                text: rgb(PARCHMENT).into(),
                text_muted: rgb(STONE).into(),
                command: rgb(MOSS).into(),
                runestone: rgb(STONE).into(),
                accent: rgb(MOSS).into(),
                accent_hover: rgb(MOSS_BRIGHT).into(),
                on_accent: rgb(NIGHT).into(),
                placeholder: rgb(0x8A928D).into(),
                error: rgb(ANSI_ON_NIGHT[9]).into(),
                hud_hp: rgb(ANSI_ON_NIGHT[9]).into(),
                hud_mana: rgb(ANSI_ON_NIGHT[12]).into(),
                hud_moves: rgb(0x4FBF87).into(),
                hud_track: rgb(0x2F3A34).into(),
                ansi: ANSI_ON_NIGHT,
            },
            WindowAppearance::Light | WindowAppearance::VibrantLight => Theme {
                background: rgb(PARCHMENT).into(),
                surface: rgb(0xF1EFE8).into(),
                surface_raised: rgb(0xFFFFFF).into(),
                segment: rgb(0xE6E3DA).into(),
                selected: rgb(0xFFFFFF).into(),
                hover: rgba(0x5F70261F).into(),
                divider: rgb(0xE6E3DA).into(),
                border: rgb(0xDAD6CB).into(),
                shadow: rgba(0x333C3926).into(),
                text: rgb(SLATE).into(),
                text_muted: rgb(0x5E625F).into(),
                command: rgb(MOSS_DEEP).into(),
                runestone: rgb(SLATE).into(),
                accent: rgb(MOSS_DEEP).into(),
                accent_hover: rgb(MOSS_DIM).into(),
                on_accent: rgb(PARCHMENT).into(),
                placeholder: rgb(0x636C67).into(),
                error: rgb(ANSI_ON_PARCHMENT[1]).into(),
                hud_hp: rgb(ANSI_ON_PARCHMENT[1]).into(),
                hud_mana: rgb(ANSI_ON_PARCHMENT[12]).into(),
                hud_moves: rgb(0x1F6B45).into(),
                hud_track: rgb(0xE6E3DA).into(),
                ansi: ANSI_ON_PARCHMENT,
            },
        }
    }

    /// The soft shadow under whatever floats.
    pub fn floating_shadow(&self) -> Vec<BoxShadow> {
        vec![BoxShadow {
            color: self.shadow,
            offset: point(px(0.), px(6.)),
            blur_radius: px(24.),
            spread_radius: px(0.),
            inset: false,
        }]
    }

    /// Maps a palette index to a color: the theme's 16, then the standard
    /// 6×6×6 cube and the 24 grays.
    pub fn ansi(&self, color: Color) -> Hsla {
        let index = color.0 as usize;
        let hex = match index {
            0..=15 => self.ansi[index],
            16..=231 => {
                let level = |n: usize| if n == 0 { 0 } else { 55 + 40 * n as u32 };
                let i = index - 16;
                (level(i / 36) << 16) | (level(i / 6 % 6) << 8) | level(i % 6)
            }
            _ => {
                let gray = 8 + 10 * (index as u32 - 232);
                (gray << 16) | (gray << 8) | gray
            }
        };
        rgb(hex).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Rgba;

    fn relative_luminance(color: Hsla) -> f32 {
        let linear = |channel: f32| {
            if channel <= 0.03928 {
                channel / 12.92
            } else {
                ((channel + 0.055) / 1.055).powf(2.4)
            }
        };
        let color = Rgba::from(color);
        0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
    }

    fn contrast(a: Hsla, b: Hsla) -> f32 {
        let (a, b) = (relative_luminance(a), relative_luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn text_tokens_stay_readable_on_every_surface() {
        for appearance in [WindowAppearance::Dark, WindowAppearance::Light] {
            let theme = Theme::for_appearance(appearance);
            for ground in [theme.background, theme.surface, theme.surface_raised] {
                for text in [
                    theme.text,
                    theme.text_muted,
                    theme.accent,
                    theme.placeholder,
                ] {
                    assert!(
                        contrast(text, ground) >= 4.5,
                        "{appearance:?}: {text:?} on {ground:?}"
                    );
                }
            }
            assert!(contrast(theme.on_accent, theme.accent) >= 4.5);
        }
    }

    #[test]
    fn palette_indexes_map_to_the_cube_and_grays() {
        let theme = Theme::for_appearance(WindowAppearance::Dark);
        let hex = |index: u8| {
            let c = Rgba::from(theme.ansi(Color(index)));
            ((c.r * 255.0).round() as u32) << 16
                | ((c.g * 255.0).round() as u32) << 8
                | (c.b * 255.0).round() as u32
        };
        assert_eq!(hex(1), 0xE06C75);
        assert_eq!(hex(16), 0x000000);
        assert_eq!(hex(196), 0xFF0000);
        assert_eq!(hex(231), 0xFFFFFF);
        assert_eq!(hex(232), 0x080808);
        assert_eq!(hex(255), 0xEEEEEE);
    }
}
