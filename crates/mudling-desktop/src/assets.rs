//! What's bundled into the binary: the brand fonts and the interface icons.

use gpui::{App, AssetSource, SharedString, Svg, svg};
use std::borrow::Cow;

/// The interface's face, from the brand guidelines.
pub const UI_FONT: &str = "DM Sans";
/// Panel and sheet headings. Brand type never styles game text.
pub const HEADING_FONT: &str = "JetBrains Mono";

const FONTS: [&[u8]; 3] = [
    include_bytes!("../../../assets/fonts/DMSans-Regular.ttf"),
    include_bytes!("../../../assets/fonts/DMSans-Medium.ttf"),
    include_bytes!("../../../assets/fonts/JetBrainsMono-SemiBold.ttf"),
];

pub fn load_fonts(cx: &App) {
    let fonts = FONTS.iter().map(|font| Cow::Borrowed(*font)).collect();
    cx.text_system()
        .add_fonts(fonts)
        .expect("the bundled fonts are valid");
}

macro_rules! icons {
    ($($variant:ident => $file:literal),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Icon {
            $($variant),+
        }

        impl Icon {
            fn path(self) -> &'static str {
                match self {
                    $(Icon::$variant => $file),+
                }
            }
        }

        const ICON_FILES: &[(&str, &[u8])] = &[
            $(($file, include_bytes!(concat!("../../../assets/icons/", $file)))),+
        ];
    };
}

icons! {
    Map => "map.svg",
    Find => "find.svg",
    Room => "room.svg",
    Character => "character.svg",
    Gear => "gear.svg",
    Skills => "skills.svg",
    Notes => "notes.svg",
    CollapseLeft => "collapse-left.svg",
    CollapseRight => "collapse-right.svg",
    Send => "send.svg",
    PanelLeft => "panel-left.svg",
    PanelRight => "panel-right.svg",
    Sprocket => "sprocket.svg",
    ChevronDown => "chevron-down.svg",
    Signal => "signal.svg",
    Check => "check.svg",
    Runestone => "runestone.svg",
    RunestoneCursor => "runestone-cursor.svg",
    WindowMinimize => "window-minimize.svg",
    WindowMaximize => "window-maximize.svg",
    WindowClose => "window-close.svg",
}

impl Icon {
    /// The icon as an element, drawn in the text color set on it.
    pub fn svg(self) -> Svg {
        svg().path(self.path())
    }
}

/// Serves the icons to gpui.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let file = ICON_FILES.iter().find(|(file, _)| *file == path);
        Ok(file.map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, _: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(ICON_FILES
            .iter()
            .map(|(file, _)| SharedString::from(*file))
            .collect())
    }
}
