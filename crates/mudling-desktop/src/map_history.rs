//! Map history, under the map: the versions the session saved, newest first,
//! and a way back to any of them.
//!
//! Listing goes to the disk off the UI thread and off the session's path,
//! and runs when the settings store says the list changed. A restore asks
//! first, then goes through the session, whose answer comes back the same
//! way: the live map itself follows through the map store.

use crate::assets::UI_FONT;
use crate::room_panel::{button, section_label};
use crate::theme::Theme;
use crate::title_bar::FocusNextControl;
use chrono::{DateTime, Datelike, Local};
use gpui::{
    App, Context, FocusHandle, KeyBinding, KeyDownEvent, Pixels, Role, ScrollHandle, SharedString,
    Task, Window, div, prelude::*, px,
};
use mudling_core::persist::{MapVersion, Timestamp};
use mudling_core::session::MapHistory;
use mudling_core::store::SettingsState;

/// The key context while focus is in the section.
const CONTEXT: &str = "MapHistory";
/// The whole section's height, which never changes: the list scrolls.
pub const HEIGHT: Pixels = px(164.);
const NOTHING_SAVED: &str =
    "Nothing saved yet. A version is saved when you connect, and every few minutes as you map.";
const NOT_READ: &str = "Couldn't read map history.";
const NOT_RESTORED: &str = "Couldn't restore that version. Your map is as it was.";
const RESTORING: &str = "Restoring…";

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("tab", FocusNextControl, Some(CONTEXT))]);
}

/// A version as the list shows it.
struct Row {
    id: i64,
    when: SharedString,
    rooms: SharedString,
    restore: FocusHandle,
}

impl Row {
    fn new(version: &MapVersion, now: DateTime<Local>, cx: &mut Context<MapHistoryView>) -> Row {
        Row {
            id: version.id,
            when: when(version.created, now).into(),
            rooms: rooms(version.room_count).into(),
            restore: cx.focus_handle().tab_stop(true),
        }
    }

    /// What it's called aloud, and in the question.
    fn title(&self) -> String {
        format!("{}, {}", self.when, self.rooms)
    }
}

/// What the last ask came to, for the line above the list.
#[derive(Clone, Debug, PartialEq)]
enum Line {
    Restoring,
    Restored(SharedString),
    Failed,
}

pub struct MapHistoryView {
    history: Option<MapHistory>,
    rows: Vec<Row>,
    /// The list couldn't be read.
    unread: bool,
    /// The `map_versions` last listed for.
    listed: u64,
    listing: Option<Task<()>>,
    /// The version the player is asked about.
    asking: Option<i64>,
    /// The restore last answered.
    answered: u64,
    line: Option<Line>,
    keep: FocusHandle,
    scroll: ScrollHandle,
    /// The row last scrolled to for its Restore taking focus.
    in_view: Option<usize>,
}

impl MapHistoryView {
    pub fn new(
        history: Option<MapHistory>,
        settings: &SettingsState,
        cx: &mut Context<MapHistoryView>,
    ) -> MapHistoryView {
        let mut view = MapHistoryView {
            history,
            rows: Vec::new(),
            unread: false,
            listed: 0,
            listing: None,
            asking: None,
            answered: settings.map_restore.as_ref().map_or(0, |last| last.attempt),
            line: None,
            keep: cx.focus_handle().tab_stop(true),
            scroll: ScrollHandle::new(),
            in_view: None,
        };
        view.show(settings, cx);
        view
    }

    /// Takes the settings store: lists again if history changed, and shows
    /// how a restore went.
    pub fn show(&mut self, settings: &SettingsState, cx: &mut Context<MapHistoryView>) {
        if let Some(restore) = &settings.map_restore
            && restore.attempt != self.answered
        {
            self.answered = restore.attempt;
            self.line = Some(match restore.outcome {
                Ok(version) => Line::Restored(self.row_title(version).into()),
                Err(_) => Line::Failed,
            });
            cx.notify();
        }
        if settings.map_versions != self.listed {
            self.listed = settings.map_versions;
            self.list(cx);
        }
    }

    fn list(&mut self, cx: &mut Context<MapHistoryView>) {
        let Some(history) = self.history.clone() else {
            return;
        };
        let versions = cx.background_spawn(async move { history.versions() });
        // A newer list replaces this one, and dropping a task cancels it.
        self.listing = Some(cx.spawn(async move |view, cx| {
            let versions = versions.await;
            let _ = view.update(cx, |view, cx| view.take_versions(versions.ok(), cx));
        }));
    }

    fn take_versions(
        &mut self,
        versions: Option<Vec<MapVersion>>,
        cx: &mut Context<MapHistoryView>,
    ) {
        self.unread = versions.is_none();
        let now = Local::now();
        self.rows = (versions.iter().flatten())
            .map(|version| Row::new(version, now, cx))
            .collect();
        self.in_view = None;
        if self.asking.is_some_and(|id| self.row(id).is_none()) {
            self.asking = None;
        }
        cx.notify();
    }

    fn row(&self, id: i64) -> Option<&Row> {
        self.rows.iter().find(|row| row.id == id)
    }

    fn row_title(&self, id: i64) -> String {
        self.row(id).map(Row::title).unwrap_or_default()
    }

    fn ask(&mut self, id: i64, window: &mut Window, cx: &mut Context<MapHistoryView>) {
        self.asking = Some(id);
        window.focus(&self.keep, cx);
        cx.notify();
    }

    /// Takes the question back, and focus to the Restore that raised it.
    pub fn keep_the_map(&mut self, window: &mut Window, cx: &mut Context<MapHistoryView>) {
        if let Some(row) = self.asking.take().and_then(|id| self.row(id)) {
            window.focus(&row.restore, cx);
        }
        cx.notify();
    }

    fn restore(&mut self, window: &mut Window, cx: &mut Context<MapHistoryView>) {
        let Some(id) = self.asking else { return };
        self.keep_the_map(window, cx);
        let sent = self.history.as_ref().is_some_and(|h| h.restore(id));
        self.line = Some(if sent { Line::Restoring } else { Line::Failed });
    }

    fn line_shown(&self, theme: &Theme) -> Option<impl IntoElement + use<>> {
        let (text, role, color): (SharedString, _, _) = match self.line.as_ref()? {
            Line::Restoring => (RESTORING.into(), Role::Status, theme.text_muted),
            Line::Failed => (NOT_RESTORED.into(), Role::Alert, theme.error),
            Line::Restored(title) => (
                format!("Restored {title}. The map from before is now first in this list.").into(),
                Role::Status,
                theme.text_muted,
            ),
        };
        Some(
            div()
                .id("map-history-line")
                .role(role)
                .aria_label(text.clone())
                .flex_none()
                .pb_1()
                .text_size(px(12.))
                .text_color(color)
                .child(text),
        )
    }

    /// The question before a restore: what it does to changes since, and that
    /// it can be undone.
    fn question(
        &self,
        id: i64,
        theme: &Theme,
        cx: &mut Context<MapHistoryView>,
    ) -> impl IntoElement {
        let title = self.row_title(id);
        let text = format!(
            "Put the map back to {title}? Changes to rooms, notes and features since then \
             go, but the map as it is now stays in this list."
        );
        div()
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    view.keep_the_map(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .id("map-history-question")
                    .role(Role::Alert)
                    .aria_label(text.clone())
                    .text_size(px(13.))
                    .text_color(theme.text)
                    .child(text),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .pt_2()
                    .child(
                        button("map-history-keep", "Keep this map".into(), theme)
                            .track_focus(&self.keep)
                            .px_2()
                            .py_0p5()
                            .text_size(px(13.))
                            .text_color(theme.text)
                            .child("Keep this map")
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.keep_the_map(window, cx);
                            })),
                    )
                    .child(
                        button(
                            "map-history-restore",
                            format!("Restore the map from {title}").into(),
                            theme,
                        )
                        .px_2()
                        .py_0p5()
                        .text_size(px(13.))
                        .text_color(theme.accent)
                        .child("Restore")
                        .on_click(cx.listener(|view, _, window, cx| view.restore(window, cx))),
                    ),
            )
    }

    fn row_shown(
        &self,
        number: usize,
        row: &Row,
        theme: &Theme,
        cx: &mut Context<MapHistoryView>,
    ) -> impl IntoElement + use<> {
        let id = row.id;
        div()
            .id(("map-version", number))
            .role(Role::ListItem)
            .aria_label(row.title())
            .flex()
            .items_center()
            .gap_3()
            .h(px(28.))
            .text_size(px(13.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(theme.text)
                    .child(row.when.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(theme.text_muted)
                    .child(row.rooms.clone()),
            )
            .child(
                button(
                    ("map-version-restore", number),
                    format!("Restore the map from {}", row.title()).into(),
                    theme,
                )
                .track_focus(&row.restore)
                .px_2()
                .border_color(gpui::transparent_black())
                .text_color(theme.accent)
                .child("Restore")
                .on_click(cx.listener(move |view, _, window, cx| view.ask(id, window, cx))),
            )
    }

    fn list_shown(&self, theme: &Theme, cx: &mut Context<MapHistoryView>) -> impl IntoElement {
        let rows = self.rows.iter().enumerate();
        div()
            .id("map-history-list")
            .role(Role::List)
            .aria_label("Saved versions")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .children(rows.map(|(number, row)| self.row_shown(number, row, theme, cx)))
    }

    /// Tab moves focus, not the list: this brings the focused row into view.
    fn follow_focus(&mut self, window: &Window) {
        let focused = (self.rows.iter()).position(|row| row.restore.is_focused(window));
        if focused.is_some() && focused != self.in_view {
            self.scroll.scroll_to_item(focused.unwrap_or_default());
        }
        self.in_view = focused.or(self.in_view);
    }

    #[cfg(any(test, target_os = "macos"))]
    pub fn show_sample(&mut self, cx: &mut Context<MapHistoryView>) {
        self.rows = [("Today, 11:42", 14), ("Yesterday, 21:07", 9)]
            .into_iter()
            .enumerate()
            .map(|(id, (when, count))| Row {
                id: id as i64,
                when: when.into(),
                rooms: rooms(count).into(),
                restore: cx.focus_handle().tab_stop(true),
            })
            .collect();
        cx.notify();
    }

    #[cfg(any(test, target_os = "macos"))]
    pub fn ask_about_sample(&mut self, window: &mut Window, cx: &mut Context<MapHistoryView>) {
        if let Some(id) = self.rows.first().map(|row| row.id) {
            self.ask(id, window, cx);
        }
    }
}

impl Render for MapHistoryView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.follow_focus(window);
        let theme = *cx.global::<Theme>();
        let empty = (self.rows.is_empty() || self.unread).then(|| {
            let text = if self.unread { NOT_READ } else { NOTHING_SAVED };
            div()
                .text_size(px(13.))
                .text_color(theme.text_muted)
                .child(text)
        });
        div()
            .id("map-history")
            .key_context(CONTEXT)
            .on_action(|_: &FocusNextControl, window, cx| window.focus_next(cx))
            .role(Role::Group)
            .aria_label("Map history")
            .font_family(UI_FONT)
            .h(HEIGHT)
            .w_full()
            .flex()
            .flex_col()
            .pt_4()
            .child(
                div()
                    .flex_none()
                    .pb_2()
                    .child(section_label("MAP HISTORY", &theme)),
            )
            .children(self.line_shown(&theme).filter(|_| self.asking.is_none()))
            .children(empty)
            .map(|section| match self.asking {
                Some(id) => section.child(self.question(id, &theme, cx)),
                None => section.child(self.list_shown(&theme, cx)),
            })
    }
}

/// A version's time as the canvas writes it.
fn when(created: Timestamp, now: DateTime<Local>) -> String {
    let Some(created) = DateTime::from_timestamp_millis(created.0) else {
        return String::new();
    };
    let created = created.with_timezone(&Local);
    let days = (now.date_naive() - created.date_naive()).num_days();
    let time = created.format("%H:%M");
    match days {
        0 => format!("Today, {time}"),
        1 => format!("Yesterday, {time}"),
        _ if created.year() == now.year() => created.format("%b %-d, %H:%M").to_string(),
        _ => created.format("%b %-d %Y, %H:%M").to_string(),
    }
}

fn rooms(count: i64) -> String {
    match count {
        1 => "1 room".to_owned(),
        count => format!("{count} rooms"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};
    use gpui::{KeyUpEvent, Keystroke, TestAppContext, VisualTestContext, WindowAppearance};
    use mudling_core::map::LiveMap;
    use mudling_core::persist::{Batch, Database, Room, SERVERS_FOLDER};
    use mudling_core::session::{Session, SessionConfig};
    use mudling_core::store::{MapRestore, MapState, Versioned};
    use std::time::Instant;

    const HOST: &str = "history.example";

    /// A session whose server has one saved version: one room, and a second
    /// room since.
    fn played_session(folder: &std::path::Path) -> Session {
        let room = |id: i64| Room {
            id,
            name: "A plain".into(),
            area: "Vale".into(),
            environment: String::new(),
            description: String::new(),
            x: id,
            y: 0,
            z: 0,
            building: None,
            tangled: false,
            maze: false,
            updated: Timestamp(0),
        };
        let database = Database::open(&folder.join(SERVERS_FOLDER), HOST).unwrap();
        let writer = database.writer().unwrap();
        let mut batch = Batch::new();
        batch.upsert(room(1));
        writer.commit(batch).unwrap();
        writer
            .commit(LiveMap::save_version("safety", "A test's", Timestamp(0)))
            .unwrap();
        let mut batch = Batch::new();
        batch.upsert(room(2));
        writer.commit(batch).unwrap();
        Session::open(SessionConfig::new(folder, HOST, 4000), || {}).unwrap()
    }

    fn settings_of(session: &Session) -> SettingsState {
        session
            .stores()
            .read(|settings: &Versioned<SettingsState>| settings.value().clone())
    }

    fn wait_for(session: &Session, done: impl Fn(&SettingsState) -> bool) -> SettingsState {
        let started = Instant::now();
        loop {
            let settings = settings_of(session);
            if done(&settings) {
                return settings;
            }
            assert!(
                started.elapsed().as_secs() < 5,
                "the session never answered"
            );
            std::thread::yield_now();
        }
    }

    #[gpui::test]
    fn a_confirmed_restore_reaches_the_session_and_the_list_follows(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_global(Theme::for_appearance(WindowAppearance::Dark)));
        let folder = tempfile::tempdir().unwrap();
        let session = played_session(folder.path());
        let settings = wait_for(&session, |settings| settings.map_versions > 0);
        let history = session.map_history();
        let (view, cx) =
            cx.add_window_view(|_, cx| MapHistoryView::new(Some(history), &settings, cx));
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |view, _| view.rows.len()), 1);

        view.update_in(cx, |view, window, cx| view.ask_about_sample(window, cx));
        cx.run_until_parked();
        view.update_in(cx, |view, window, cx| {
            assert!(view.keep.is_focused(window));
            view.keep_the_map(window, cx);
            assert_eq!(view.asking, None);
            view.ask_about_sample(window, cx);
            view.restore(window, cx);
            assert_eq!(view.line, Some(Line::Restoring));
        });

        let settings = wait_for(&session, |settings| settings.map_restore.is_some());
        view.update(cx, |view, cx| view.show(&settings, cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(matches!(view.line, Some(Line::Restored(_))));
            assert_eq!(view.rows.len(), 2, "what the restore replaced is listed");
        });
        let rooms = session
            .stores()
            .read(|map: &Versioned<MapState>| map.value().rooms().len());
        assert_eq!(rooms, 1);
        session.quit().wait().unwrap();
    }

    /// Enter activates a focused control when it's let go.
    fn press_enter(cx: &mut VisualTestContext) {
        cx.simulate_keystrokes("enter");
        cx.simulate_event(KeyUpEvent {
            keystroke: Keystroke::parse("enter").unwrap(),
        });
        cx.run_until_parked();
    }

    /// Tab, as a player presses it: from a row to the next, from the question
    /// to its Restore, and Enter there confirms.
    #[gpui::test]
    fn the_keyboard_reaches_each_restore_and_confirms(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_global(Theme::for_appearance(WindowAppearance::Dark));
            crate::bind_keys(cx);
        });
        let (view, cx) =
            cx.add_window_view(|_, cx| MapHistoryView::new(None, &SettingsState::default(), cx));
        view.update(cx, |view, cx| view.show_sample(cx));
        cx.run_until_parked();
        view.update_in(cx, |view, window, cx| {
            window.focus(&view.rows[0].restore, cx);
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        view.update_in(cx, |view, window, _| {
            assert!(view.rows[1].restore.is_focused(window));
        });

        press_enter(cx);
        view.update_in(cx, |view, window, _| {
            assert_eq!(view.asking, Some(view.rows[1].id));
            assert!(view.keep.is_focused(window));
        });

        // No session to restore through, so confirming reads as a failure.
        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        press_enter(cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.asking, None);
            assert_eq!(view.line, Some(Line::Failed));
        });
    }

    #[gpui::test]
    fn a_failed_restore_is_said_once_answered(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_global(Theme::for_appearance(WindowAppearance::Dark)));
        let (view, cx) =
            cx.add_window_view(|_, cx| MapHistoryView::new(None, &SettingsState::default(), cx));
        view.update(cx, |view, cx| {
            let failed = SettingsState {
                map_restore: Some(MapRestore {
                    attempt: 1,
                    outcome: Err("the disk is full".to_owned()),
                }),
                ..SettingsState::default()
            };
            view.show(&failed, cx);
            assert_eq!(view.line, Some(Line::Failed));
        });
    }

    fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(year, month, day, hour, minute, 0)
            .unwrap()
    }

    fn stamp(time: DateTime<Local>) -> Timestamp {
        Timestamp(time.timestamp_millis())
    }

    #[test]
    fn times_read_as_the_canvas_writes_them() {
        let now = at(2026, 10, 31, 12, 0);
        assert_eq!(when(stamp(at(2026, 10, 31, 11, 42)), now), "Today, 11:42");
        let yesterday = now - Duration::days(1) + Duration::hours(9);
        assert_eq!(when(stamp(yesterday), now), "Yesterday, 21:00");
        assert_eq!(when(stamp(at(2026, 3, 4, 9, 5)), now), "Mar 4, 09:05");
        assert_eq!(when(stamp(at(2025, 3, 4, 9, 5)), now), "Mar 4 2025, 09:05");
    }

    #[test]
    fn one_room_is_one_room() {
        assert_eq!(rooms(1), "1 room");
        assert_eq!(rooms(14), "14 rooms");
    }
}
