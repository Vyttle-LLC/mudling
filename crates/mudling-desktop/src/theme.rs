//! The Runestone tokens: every color the window draws, for each appearance,
//! and what the player chose in Settings › Appearance: the theme and the chat
//! font. Values come from the MUDling brand guidelines and the design canvas.

use crate::hud::HudPlacement;
use crate::map_view::Pip;
use gpui::{BoxShadow, Global, Hsla, Pixels, WindowAppearance, point, px, rgb, rgba};
use mudling_core::Color;
use serde::{Deserialize, Serialize};

/// The monospaced faces offered for the chat, the platform's default first.
/// Brand type never styles game text, so the bundled faces aren't here.
#[cfg(target_os = "macos")]
pub const CHAT_FONTS: [&str; 4] = ["Menlo", "Monaco", "Courier New", "Andale Mono"];
#[cfg(target_os = "windows")]
pub const CHAT_FONTS: [&str; 4] = ["Consolas", "Cascadia Mono", "Courier New", "Lucida Console"];
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub const CHAT_FONTS: [&str; 4] = [
    "DejaVu Sans Mono",
    "Liberation Mono",
    "Noto Sans Mono",
    "Ubuntu Mono",
];

pub const SMALLEST_CHAT_SIZE: u8 = 11;
pub const LARGEST_CHAT_SIZE: u8 = 22;
const DEFAULT_CHAT_SIZE: u8 = 14;
/// A chat row's height for its text: 20 px at 14 px, the default.
const CHAT_LINE_SPACING: f32 = 10. / 7.;

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

/// The map's colors, sampled from the design canvas in dark and built from
/// the same brand neutrals in light.
#[derive(Clone, Copy)]
pub struct MapColors {
    pub ground: Hsla,
    pub edge: Hsla,
    pub room: Hsla,
    pub room_border: Hsla,
    pub link: Hsla,
    pub you: Hsla,
    pub aggressive: Hsla,
    pub building: Hsla,
    pub maze: Hsla,
    pub selected: Hsla,
    pub heading: Hsla,
    pub muted: Hsla,
}

impl MapColors {
    /// What a room's small mark is drawn in.
    pub fn pip(&self, pip: Pip) -> Hsla {
        match pip {
            Pip::Note => self.you,
            Pip::Gear => self.maze,
            Pip::Important => self.selected,
            Pip::Danger => self.aggressive,
            Pip::Shop | Pip::Trainer | Pip::Fountain => self.building,
        }
    }
}

/// Which appearance the window wears.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

/// What the player chose in Settings › Appearance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Appearance {
    pub theme: ThemeChoice,
    pub font: &'static str,
    pub font_size: u8,
    pub hud: HudPlacement,
    /// Show prompt lines even with a HUD repeating them.
    pub show_prompts: bool,
    /// Where the floating HUD was left; `None` until it's moved.
    pub card: Option<(i32, i32)>,
}

impl Default for Appearance {
    fn default() -> Appearance {
        Appearance {
            theme: ThemeChoice::default(),
            font: CHAT_FONTS[0],
            font_size: DEFAULT_CHAT_SIZE,
            hud: HudPlacement::default(),
            show_prompts: false,
            card: None,
        }
    }
}

impl Appearance {
    /// Whether the chat shows prompt lines: with the HUD hidden, nothing else
    /// has their numbers.
    pub fn shows_prompts(&self) -> bool {
        self.show_prompts || self.hud == HudPlacement::Hidden
    }

    pub fn with_font_size(self, font_size: u8) -> Appearance {
        Appearance {
            font_size: font_size.clamp(SMALLEST_CHAT_SIZE, LARGEST_CHAT_SIZE),
            ..self
        }
    }

    /// The offered face called `name`, or the default for one this platform
    /// doesn't offer.
    pub fn with_font_named(self, name: &str) -> Appearance {
        let font = CHAT_FONTS.into_iter().find(|font| *font == name);
        Appearance {
            font: font.unwrap_or(CHAT_FONTS[0]),
            ..self
        }
    }

    pub fn chat_size(&self) -> Pixels {
        px(self.font_size as f32)
    }

    pub fn chat_line_height(&self) -> Pixels {
        (self.chat_size() * CHAT_LINE_SPACING).round()
    }
}

#[derive(Clone, Copy)]
pub struct Theme {
    pub map: MapColors,
    /// What the player chose, kept here so everything that draws reads it
    /// from one place.
    pub choice: Appearance,
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
    /// What can't be taken back, such as Kill.
    pub danger: Hsla,
    /// A change for the better; armor counts down.
    pub better: Hsla,
    /// A change for the worse.
    pub worse: Hsla,
    pub placeholder: Hsla,
    /// Words that say what went wrong.
    pub error: Hsla,
    pub warning: Hsla,
    /// Behind a warning's text.
    pub warning_ground: Hsla,
    /// Text on a filled warning.
    pub on_warning: Hsla,
    /// Dims the window behind a sheet.
    pub scrim: Hsla,
    pub hud_hp: Hsla,
    pub hud_mana: Hsla,
    pub hud_moves: Hsla,
    pub hud_xp: Hsla,
    pub hud_track: Hsla,
    ansi: [u32; 16],
}

impl Global for Theme {}

impl Theme {
    /// The theme for what the player chose, on an OS that's in `os`.
    pub fn chosen(choice: Appearance, os: WindowAppearance) -> Theme {
        let appearance = match choice.theme {
            ThemeChoice::System => os,
            ThemeChoice::Light => WindowAppearance::Light,
            ThemeChoice::Dark => WindowAppearance::Dark,
        };
        Theme {
            choice,
            ..Theme::for_appearance(appearance)
        }
    }

    pub fn for_appearance(appearance: WindowAppearance) -> Theme {
        let choice = Appearance::default();
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Theme {
                map: MapColors {
                    ground: rgb(0x0C1210).into(),
                    edge: rgb(0x1C241F).into(),
                    room: rgb(0x1B2620).into(),
                    room_border: rgb(0x363B36).into(),
                    link: rgb(0x333731).into(),
                    you: rgb(MOSS).into(),
                    aggressive: rgb(0xF08A93).into(),
                    building: rgb(STONE).into(),
                    maze: rgb(0xE5C04F).into(),
                    selected: rgb(PARCHMENT).into(),
                    heading: rgb(PARCHMENT).into(),
                    muted: rgb(0x8A918C).into(),
                },
                choice,
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
                danger: rgb(ANSI_ON_NIGHT[9]).into(),
                better: rgb(MOSS_BRIGHT).into(),
                worse: rgb(ANSI_ON_NIGHT[9]).into(),
                placeholder: rgb(0x8A928D).into(),
                error: rgb(ANSI_ON_NIGHT[9]).into(),
                warning: rgb(0xE5C04F).into(),
                warning_ground: rgba(0xE5C04F14).into(),
                on_warning: rgb(NIGHT).into(),
                scrim: rgba(0x0A100DB8).into(),
                hud_hp: rgb(ANSI_ON_NIGHT[9]).into(),
                hud_mana: rgb(ANSI_ON_NIGHT[12]).into(),
                hud_moves: rgb(0x4FBF87).into(),
                hud_xp: rgb(ANSI_ON_NIGHT[11]).into(),
                hud_track: rgb(0x2F3A34).into(),
                ansi: ANSI_ON_NIGHT,
            },
            WindowAppearance::Light | WindowAppearance::VibrantLight => Theme {
                map: MapColors {
                    ground: rgb(0xEFEDE5).into(),
                    edge: rgb(0xDAD6CB).into(),
                    room: rgb(0xFFFFFF).into(),
                    room_border: rgb(0xC9C5B9).into(),
                    link: rgb(0xB4B0A5).into(),
                    you: rgb(MOSS_DEEP).into(),
                    aggressive: rgb(0xA81E33).into(),
                    building: rgb(0x5E625F).into(),
                    maze: rgb(0x7A5A0B).into(),
                    selected: rgb(0x111A16).into(),
                    heading: rgb(0x111A16).into(),
                    muted: rgb(0x5E625F).into(),
                },
                choice,
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
                danger: rgb(ANSI_ON_PARCHMENT[1]).into(),
                better: rgb(ANSI_ON_PARCHMENT[2]).into(),
                worse: rgb(ANSI_ON_PARCHMENT[1]).into(),
                placeholder: rgb(0x636C67).into(),
                error: rgb(ANSI_ON_PARCHMENT[1]).into(),
                warning: rgb(0x7A5A0B).into(),
                warning_ground: rgba(0x7A5A0B14).into(),
                on_warning: rgb(PARCHMENT).into(),
                scrim: rgba(0x333C3973).into(),
                hud_hp: rgb(ANSI_ON_PARCHMENT[1]).into(),
                hud_mana: rgb(ANSI_ON_PARCHMENT[12]).into(),
                hud_moves: rgb(0x1F6B45).into(),
                hud_xp: rgb(ANSI_ON_PARCHMENT[11]).into(),
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
                    theme.danger,
                    theme.better,
                    theme.worse,
                    theme.placeholder,
                    theme.warning,
                ] {
                    assert!(
                        contrast(text, ground) >= 4.5,
                        "{appearance:?}: {text:?} on {ground:?}"
                    );
                }
            }
            assert!(contrast(theme.on_accent, theme.accent) >= 4.5);
            assert!(contrast(theme.on_warning, theme.warning) >= 4.5);
        }
    }

    #[test]
    fn a_held_theme_ignores_the_os_and_system_follows_it() {
        let dark_on = |theme: ThemeChoice, os| {
            let choice = Appearance {
                theme,
                ..Appearance::default()
            };
            Theme::chosen(choice, os).background
                == Theme::for_appearance(WindowAppearance::Dark).background
        };
        assert!(dark_on(ThemeChoice::Dark, WindowAppearance::Light));
        assert!(!dark_on(ThemeChoice::Light, WindowAppearance::Dark));
        assert!(dark_on(ThemeChoice::System, WindowAppearance::VibrantDark));
        assert!(!dark_on(ThemeChoice::System, WindowAppearance::Light));
    }

    #[test]
    fn the_row_height_follows_the_chat_size_and_the_size_stays_in_range() {
        let chosen = Appearance::default();
        assert_eq!(chosen.chat_line_height(), px(20.));
        assert_eq!(chosen.with_font_size(21).chat_line_height(), px(30.));
        assert_eq!(chosen.with_font_size(200).font_size, LARGEST_CHAT_SIZE);
        assert_eq!(chosen.with_font_size(1).font_size, SMALLEST_CHAT_SIZE);
    }

    #[test]
    fn a_face_this_platform_does_not_offer_falls_back_to_the_default() {
        let chosen = Appearance::default();
        assert_eq!(chosen.with_font_named(CHAT_FONTS[2]).font, CHAT_FONTS[2]);
        assert_eq!(chosen.with_font_named("No Such Face").font, CHAT_FONTS[0]);
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
