//! Keeps [`Appearance`] and the player's sync and vault choices across
//! launches, as rows in the core's `meta` table. Settings belong to the
//! device, not a server, so the rows live in a database of their own, outside
//! the folder of server databases.
//!
//! The theme and font also travel through the repo as `AppSettings`. Whatever
//! else `Appearance` holds stays on this device.

use crate::hud::HudPlacement;
use crate::theme::{Appearance, CHAT_FONTS, ThemeChoice};
use mudling_core::persist::{self, Batch, Database, Meta, Timestamp, Writer};
use mudling_core::repo::AppSettings;
use mudling_core::vault::AutoLock;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const FOLDER: &str = "app";
const DATABASE: &str = "desktop";
const APPEARANCE_KEY: &str = "desktop.appearance";
const SYNC_KEY: &str = "desktop.sync";

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

/// How long the vault stays open unused, as the player chose it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LockAfter {
    Five,
    #[default]
    Fifteen,
    Hour,
    Never,
}

impl LockAfter {
    pub const ALL: [LockAfter; 4] = [
        LockAfter::Five,
        LockAfter::Fifteen,
        LockAfter::Hour,
        LockAfter::Never,
    ];

    pub fn label(self) -> &'static str {
        match self {
            LockAfter::Five => "5 minutes",
            LockAfter::Fifteen => "15 minutes",
            LockAfter::Hour => "1 hour",
            LockAfter::Never => "Never",
        }
    }

    pub fn auto_lock(self) -> AutoLock {
        match self {
            LockAfter::Five => AutoLock::FIVE_MINUTES,
            LockAfter::Fifteen => AutoLock::FIFTEEN_MINUTES,
            LockAfter::Hour => AutoLock::ONE_HOUR,
            LockAfter::Never => AutoLock::NEVER,
        }
    }
}

/// What sync and the vault need from the player, kept across launches. Sync's
/// [`AppSettings`] come from these and [`Appearance`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncChoices {
    pub repo: Option<PathBuf>,
    pub lock_after: LockAfter,
    /// When the theme or font last changed, here or on another device.
    pub settings_updated: Timestamp,
}

/// `appearance` as sync carries it, as of `updated`.
pub fn app_settings(appearance: Appearance, updated: Timestamp) -> AppSettings {
    AppSettings {
        theme: theme_name(appearance.theme).to_owned(),
        font: appearance.font.to_owned(),
        updated,
        ..AppSettings::default()
    }
}

/// `appearance` wearing what another device chose. A theme or font this
/// version doesn't know leaves that choice as it is.
pub fn with_app_settings(appearance: Appearance, settings: &AppSettings) -> Appearance {
    let theme = [ThemeChoice::System, ThemeChoice::Light, ThemeChoice::Dark]
        .into_iter()
        .find(|theme| theme_name(*theme) == settings.theme)
        .unwrap_or(appearance.theme);
    let font = CHAT_FONTS
        .into_iter()
        .find(|font| *font == settings.font)
        .unwrap_or(appearance.font);
    Appearance {
        theme,
        font,
        ..appearance
    }
}

fn theme_name(theme: ThemeChoice) -> &'static str {
    match theme {
        ThemeChoice::System => "system",
        ThemeChoice::Light => "light",
        ThemeChoice::Dark => "dark",
    }
}

/// Where a choice goes to be kept. Dropping this waits for the disk, so drop
/// it off the UI thread.
pub struct SavedSettings {
    writer: Writer,
    /// What was kept when this opened.
    sync_choices: SyncChoices,
}

impl SavedSettings {
    /// Opens the database in `data_folder` and reads what was kept. This
    /// touches the disk: call it before the app runs.
    pub fn open(data_folder: &Path) -> Result<(SavedSettings, Appearance), persist::Error> {
        let database = Database::open(&data_folder.join(FOLDER), DATABASE)?;
        let kept = database.reader()?.meta()?;
        let row = |key: &str| kept.iter().find(|row| row.key == key);
        let appearance = row(APPEARANCE_KEY)
            .and_then(|row| serde_json::from_str::<Row>(&row.value).ok())
            .map_or_else(Appearance::default, Appearance::from);
        let sync_choices = row(SYNC_KEY)
            .and_then(|row| serde_json::from_str(&row.value).ok())
            .unwrap_or_default();
        let writer = database.writer()?;
        let saved = SavedSettings {
            writer,
            sync_choices,
        };
        Ok((saved, appearance))
    }

    pub fn sync_choices(&self) -> SyncChoices {
        self.sync_choices.clone()
    }

    /// Queues `appearance` for the writer thread, without waiting on the disk.
    pub fn save(&self, appearance: Appearance) {
        let value = serde_json::to_string(&Row::from(appearance)).expect("the row serializes");
        self.save_row(APPEARANCE_KEY, value);
    }

    /// Queues `choices`, the same way.
    pub fn save_sync(&self, choices: &SyncChoices) {
        let value = serde_json::to_string(choices).expect("the row serializes");
        self.save_row(SYNC_KEY, value);
    }

    fn save_row(&self, key: &str, value: String) {
        let mut batch = Batch::new();
        batch.upsert(Meta {
            key: key.to_owned(),
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
    use crate::theme::LARGEST_CHAT_SIZE;
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

    #[test]
    fn the_repo_and_the_auto_lock_choice_come_back_on_the_next_launch() {
        let folder = TempDir::new().unwrap();
        let (saved, _) = SavedSettings::open(folder.path()).unwrap();
        assert_eq!(saved.sync_choices(), SyncChoices::default());

        let chosen = SyncChoices {
            repo: Some("/some/repo".into()),
            lock_after: LockAfter::Hour,
            settings_updated: Timestamp(7),
        };
        saved.save_sync(&chosen);
        drop(saved);

        let (next, _) = SavedSettings::open(folder.path()).unwrap();
        assert_eq!(next.sync_choices(), chosen);
    }

    #[test]
    fn settings_from_another_device_change_only_what_this_one_knows() {
        let mine = Appearance {
            font: CHAT_FONTS[0],
            font_size: 17,
            ..Appearance::default()
        };
        let theirs = AppSettings {
            theme: "dark".to_owned(),
            font: CHAT_FONTS[1].to_owned(),
            ..AppSettings::default()
        };
        let worn = with_app_settings(mine, &theirs);
        assert_eq!((worn.theme, worn.font), (ThemeChoice::Dark, CHAT_FONTS[1]));
        assert_eq!(worn.font_size, 17, "size stays on this device");

        let unknown = AppSettings {
            theme: "neon".to_owned(),
            font: "A font from later".to_owned(),
            ..AppSettings::default()
        };
        assert_eq!(with_app_settings(mine, &unknown), mine);
    }
}
