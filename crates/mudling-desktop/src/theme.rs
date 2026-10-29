//! Colors from the MUDling brand guidelines, chosen by the OS appearance.

use gpui::{Global, Hsla, WindowAppearance, rgb};
use mudling_core::Color;

const NIGHT: u32 = 0x111A16;
const PARCHMENT: u32 = 0xF8F7F2;
const STONE: u32 = 0xB9AB98;
const SLATE: u32 = 0x333C39;
const MOSS: u32 = 0x9BAF55;
const MOSS_DEEP: u32 = 0x5F7026;

/// The 16 basic ANSI colors, tuned to stay readable on each ground.
const ANSI_ON_NIGHT: [u32; 16] = [
    0x4A524E, 0xE06C75, 0x98C379, 0xE5C07B, 0x61AFEF, 0xC678DD, 0x56B6C2, 0xB9AB98, //
    0x6E7772, 0xF08A93, 0xB5C77D, 0xE5C04F, 0x82AAFF, 0xD7A6F0, 0x7FD4DE, 0xF8F7F2,
];
const ANSI_ON_PARCHMENT: [u32; 16] = [
    0x333C39, 0xA81E33, 0x1F6B45, 0x7A5A0B, 0x1F4F99, 0x7A2E8E, 0x12707A, 0x5E625F, //
    0x6E7772, 0xC8323F, 0x2F7D3B, 0x8A6A00, 0x2B63C4, 0x9A3FB0, 0x1C8A94, 0x111A16,
];

pub struct Theme {
    pub background: Hsla,
    pub text: Hsla,
    /// Echoed commands: told apart from server text by color alone.
    pub command: Hsla,
    pub accent: Hsla,
    pub input_background: Hsla,
    pub input_border: Hsla,
    pub placeholder: Hsla,
    ansi: [u32; 16],
}

impl Global for Theme {}

impl Theme {
    pub fn for_appearance(appearance: WindowAppearance) -> Theme {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Theme {
                background: rgb(NIGHT).into(),
                text: rgb(STONE).into(),
                command: rgb(MOSS).into(),
                accent: rgb(MOSS).into(),
                input_background: rgb(0x18231E).into(),
                input_border: rgb(0x2A3A32).into(),
                placeholder: rgb(0x6E7772).into(),
                ansi: ANSI_ON_NIGHT,
            },
            WindowAppearance::Light | WindowAppearance::VibrantLight => Theme {
                background: rgb(PARCHMENT).into(),
                text: rgb(SLATE).into(),
                command: rgb(MOSS_DEEP).into(),
                accent: rgb(MOSS_DEEP).into(),
                input_background: rgb(0xFFFFFF).into(),
                input_border: rgb(0xDAD6CB).into(),
                placeholder: rgb(0x8A8E8B).into(),
                ansi: ANSI_ON_PARCHMENT,
            },
        }
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
