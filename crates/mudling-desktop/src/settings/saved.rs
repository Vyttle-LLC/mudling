//! Keeps [`Appearance`] across launches, as one row in the core's `meta`
//! table. Settings belong to the device, not a server, so the row lives in a
//! database of its own, outside the folder of server databases.
//!
//! This is a stopgap for `AppSettings`, which Phase 6's `repo` module will own.

use crate::hud::HudPlacement;
use crate::theme::{Appearance, ThemeChoice};
use mudling_core::persist::{self, Batch, Database, Meta, Timestamp, Writer};
use serde::{Deserialize, Serialize};
use std::path::Path;

const FOLDER: &str = "app";
const DATABASE: &str = "desktop";
const APPEARANCE_KEY: &str = "desktop.appearance";

/// The row's JSON. A field it lacks keeps its default.
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Row {
    theme: ThemeChoice,
    font: String,
    font_size: u8,
    hud: HudPlacement,
    show_prompts: bool,
    card: Option<(i32, i32)>,
}

impl Default for Row {
    fn default() -> Row {
        Row::from(Appearance::default())
    }
}

impl From<Appearance> for Row {
    fn from(appearance: Appearance) -> Row {
        Row {
            theme: appearance.theme,
            font: appearance.font.to_owned(),
            font_size: appearance.font_size,
            hud: appearance.hud,
            show_prompts: appearance.show_prompts,
            card: appearance.card,
        }
    }
}

impl From<Row> for Appearance {
    fn from(row: Row) -> Appearance {
        Appearance {
            theme: row.theme,
            hud: row.hud,
            show_prompts: row.show_prompts,
            card: row.card,
            ..Appearance::default()
        }
        .with_font_named(&row.font)
        .with_font_size(row.font_size)
    }
}

/// Where a choice goes to be kept. Dropping this waits for the disk, so drop
/// it off the UI thread.
pub struct SavedSettings {
    writer: Writer,
}

impl SavedSettings {
    /// Opens the database in `data_folder` and reads what was kept. This
    /// touches the disk: call it before the app runs.
    pub fn open(data_folder: &Path) -> Result<(SavedSettings, Appearance), persist::Error> {
        let database = Database::open(&data_folder.join(FOLDER), DATABASE)?;
        let kept = database.reader()?.meta()?;
        let appearance = kept
            .into_iter()
            .find(|row| row.key == APPEARANCE_KEY)
            .and_then(|row| serde_json::from_str::<Row>(&row.value).ok())
            .map_or_else(Appearance::default, Appearance::from);
        let writer = database.writer()?;
        Ok((SavedSettings { writer }, appearance))
    }

    /// Queues `appearance` for the writer thread, without waiting on the disk.
    pub fn save(&self, appearance: Appearance) {
        let value = serde_json::to_string(&Row::from(appearance)).expect("the row serializes");
        let mut batch = Batch::new();
        batch.upsert(Meta {
            key: APPEARANCE_KEY.to_owned(),
            value,
            updated: Timestamp::now(),
        });
        if let Err(error) = self.writer.submit(batch) {
            eprintln!("couldn't save the settings: {error}");
        }
        let failures = self.writer.take_failures();
        if !failures.is_empty() {
            eprintln!("{} settings saves failed", failures.count());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{CHAT_FONTS, LARGEST_CHAT_SIZE};
    use tempfile::TempDir;

    #[test]
    fn a_choice_comes_back_on_the_next_launch() {
        let folder = TempDir::new().unwrap();
        let (saved, first) = SavedSettings::open(folder.path()).unwrap();
        assert_eq!(first, Appearance::default());

        let chosen = Appearance {
            theme: ThemeChoice::Dark,
            font: CHAT_FONTS[1],
            font_size: 17,
            hud: HudPlacement::Floating,
            show_prompts: true,
            card: Some((120, 48)),
        };
        saved.save(chosen);
        drop(saved);

        let (_saved, next) = SavedSettings::open(folder.path()).unwrap();
        assert_eq!(next, chosen);
    }

    #[test]
    fn a_row_keeps_what_it_can_and_a_bad_one_is_refused() {
        let row: Row = serde_json::from_str(r#"{"theme":"light","font_size":200}"#).unwrap();
        let appearance = Appearance::from(row);
        assert_eq!(appearance.theme, ThemeChoice::Light);
        assert_eq!(appearance.font, CHAT_FONTS[0]);
        assert_eq!(appearance.font_size, LARGEST_CHAT_SIZE);
        assert!(serde_json::from_str::<Row>(r#"{"theme":"neon"}"#).is_err());
    }
}
