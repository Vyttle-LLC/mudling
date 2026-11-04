//! Map history, under the map: the versions the session saved, newest first,
//! and a way back to any of them.
//!
//! Listing goes to the disk off the UI thread and off the session's path,
//! and runs when the settings store says the list changed. A restore asks
//! first, then goes through the session, whose answer comes back the same
//! way: the live map itself follows through the map store.
//!
//! A join sits in the list as a change of its own, with Undo in place of
//! Restore. Undoing takes back that join and nothing else, and the note over
//! the map says how it went. A restore leaves joined rooms joined.

use crate::assets::UI_FONT;
use crate::room_panel::{button, section_label};
use crate::shell::FocusCommandBar;
use crate::theme::Theme;
use crate::title_bar::FocusNextControl;
use chrono::{DateTime, Datelike, Local};
use gpui::{
    App, Context, FocusHandle, KeyBinding, Pixels, Role, ScrollHandle, SharedString, Task, Window,
    actions, div, prelude::*, px,
};
use mudling_core::map::VERSION_JOIN;
use mudling_core::persist::{MapVersion, Timestamp};
use mudling_core::session::MapHistory;
use mudling_core::store::SettingsState;
use std::collections::HashMap;

/// The key context while focus is in the section.
const CONTEXT: &str = "MapHistory";
/// While the question is up, deeper than the dock's own Escape.
const QUESTION_CONTEXT: &str = "MapHistoryQuestion";
/// The whole section's height, which never changes: the list scrolls.
pub const HEIGHT: Pixels = px(164.);
const NOTHING_SAVED: &str =
    "Nothing saved yet. A version is saved when you connect, and every few minutes as you map.";
const NOT_READ: &str = "Couldn't read map history.";
const NOT_RESTORED: &str = "Couldn't restore that version. Your map is as it was.";
const RESTORING: &str = "Restoring…";

actions!(map_history, [KeepTheMap]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", FocusNextControl, Some(CONTEXT)),
        KeyBinding::new("escape", KeepTheMap, Some(QUESTION_CONTEXT)),
    ]);
}

/// A version as the list shows it.
struct Row {
    id: i64,
    when: SharedString,
    /// How many rooms, or what the join was called.
    rooms: SharedString,
    /// For a join, what names it to undo.
    joined: Option<Timestamp>,
    restore: FocusHandle,
}

impl Row {
    /// A row for `version`, on `restore` if the list had one for it already.
    fn new(
        version: &MapVersion,
        now: DateTime<Local>,
        restore: Option<FocusHandle>,
        cx: &mut Context<MapHistoryView>,
    ) -> Row {
        let joined = (version.kind == VERSION_JOIN).then_some(version.created);
        Row {
            id: version.id,
            when: when(version.created, now).into(),
            rooms: match joined {
                Some(_) => version.label.clone().into(),
                None => rooms(version.room_count).into(),
            },
            joined,
            restore: restore.unwrap_or_else(|| cx.focus_handle().tab_stop(true)),
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
    /// A join being undone with focus on its row, and that row's place, for
    /// focus to land on what's there once the row goes.
    undoing: Option<(i64, usize)>,
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
            undoing: None,
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
        // A list again must not take the keyboard from a Restore it's on.
        let mut handles: HashMap<i64, FocusHandle> = (self.rows.drain(..))
            .map(|row| (row.id, row.restore))
            .collect();
        self.rows = (versions.iter().flatten())
            .map(|version| Row::new(version, now, handles.remove(&version.id), cx))
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

    fn undo_join(&mut self, (id, number): (i64, usize), created: Timestamp, window: &Window) {
        if self
            .row(id)
            .is_some_and(|row| row.restore.is_focused(window))
        {
            self.undoing = Some((id, number));
        }
        if let Some(history) = &self.history {
            history.undo_join(created);
        }
    }

    /// Once an undone join's row has gone, takes focus to the row in its place
    /// or the last one, or to the command bar if the list is empty.
    fn land_after_undo(&mut self, window: &mut Window, cx: &mut Context<MapHistoryView>) {
        let Some((_, number)) = self.undoing.filter(|(id, _)| self.row(*id).is_none()) else {
            return;
        };
        self.undoing = None;
        cx.defer_in(window, move |view, window, cx| {
            match view.rows.get(number).or(view.rows.last()) {
                Some(row) => window.focus(&row.restore, cx),
                None => window.dispatch_action(Box::new(FocusCommandBar), cx),
            }
        });
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
            "Put the map back to {title}? Changes since then go, but today's map stays in \
             this list. Joined rooms stay joined, unless this map lacks the room kept: that \
             join is cancelled."
        );
        div()
            .key_context(QUESTION_CONTEXT)
            .on_action(cx.listener(|view, _: &KeepTheMap, window, cx| {
                view.keep_the_map(window, cx);
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
        let (verb, spoken) = match row.joined {
            Some(_) => ("Undo", format!("Undo {}", row.title())),
            None => ("Restore", format!("Restore the map from {}", row.title())),
        };
        let joined = row.joined;
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
                button(("map-version-restore", number), spoken.into(), theme)
                    .track_focus(&row.restore)
                    .px_2()
                    .border_color(gpui::transparent_black())
                    .text_color(theme.accent)
                    .child(verb)
                    .on_click(cx.listener(move |view, _, window, cx| match joined {
                        Some(created) => view.undo_join((id, number), created, window),
                        None => view.ask(id, window, cx),
                    })),
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
                joined: None,
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
        self.land_after_undo(window, cx);
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
            .debug_selector(|| "map-history".to_owned())
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
    use gpui::{
        Entity, KeyUpEvent, Keystroke, TestAppContext, VisualTestContext, WindowAppearance,
    };
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
                started.elapsed().as_secs() < 60,
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

    #[gpui::test]
    fn a_new_list_leaves_the_keyboard_on_the_restore_it_was_on(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_global(Theme::for_appearance(WindowAppearance::Dark)));
        let (view, cx) =
            cx.add_window_view(|_, cx| MapHistoryView::new(None, &SettingsState::default(), cx));
        let version = |id| MapVersion {
            id,
            created: Timestamp(id),
            kind: "autosave".into(),
            label: String::new(),
            room_count: id,
            areas: Default::default(),
            updated: Timestamp(id),
        };
        view.update(cx, |view, cx| {
            view.take_versions(Some(vec![version(2), version(1)]), cx)
        });
        cx.run_until_parked();
        view.update_in(cx, |view, window, cx| {
            window.focus(&view.rows[1].restore, cx)
        });
        cx.run_until_parked();

        // An autosave lists a newer version in front of both.
        view.update(cx, |view, cx| {
            view.take_versions(Some(vec![version(3), version(2), version(1)]), cx);
        });
        cx.run_until_parked();
        view.update_in(cx, |view, window, _| {
            assert_eq!(view.rows[2].id, 1);
            assert!(view.rows[2].restore.is_focused(window));
        });
    }

    /// The history as the dock holds it: Escape there goes to the command bar,
    /// as the shell's handler sends it.
    struct InDock(Entity<MapHistoryView>);

    impl Render for InDock {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .key_context(crate::dock::CONTEXT)
                .on_action(|_: &crate::shell::FocusCommandBar, _, _| {})
                .child(self.0.clone())
        }
    }

    #[gpui::test]
    fn escape_on_the_question_keeps_the_map_even_in_the_dock(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_global(Theme::for_appearance(WindowAppearance::Dark));
            crate::bind_keys(cx);
        });
        let (dock, cx) = cx.add_window_view(|_, cx| {
            InDock(cx.new(|cx| MapHistoryView::new(None, &SettingsState::default(), cx)))
        });
        let view = dock.read_with(cx, |dock, _| dock.0.clone());
        view.update(cx, |view, cx| view.show_sample(cx));
        view.update_in(cx, |view, window, cx| view.ask_about_sample(window, cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| assert!(view.asking.is_some()));

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        view.update_in(cx, |view, window, _| {
            assert_eq!(view.asking, None, "the question is gone");
            assert!(
                view.rows[0].restore.is_focused(window),
                "back on its Restore"
            );
        });
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

    /// A join's row, and an autosave's. Undoing the join takes its row away.
    #[gpui::test]
    fn undoing_a_join_by_keyboard_lands_on_the_row_in_its_place(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_global(Theme::for_appearance(WindowAppearance::Dark));
            crate::bind_keys(cx);
        });
        let (view, cx) =
            cx.add_window_view(|_, cx| MapHistoryView::new(None, &SettingsState::default(), cx));
        let version = |id, kind: &str| MapVersion {
            id,
            created: Timestamp(id),
            kind: kind.into(),
            label: "Joined two copies of a square".into(),
            room_count: 2,
            areas: Default::default(),
            updated: Timestamp(id),
        };
        view.update(cx, |view, cx| {
            let versions = vec![version(2, VERSION_JOIN), version(1, "autosave")];
            view.take_versions(Some(versions), cx);
        });
        cx.run_until_parked();
        view.update_in(cx, |view, window, cx| {
            window.focus(&view.rows[0].restore, cx);
        });
        cx.run_until_parked();

        press_enter(cx);
        view.update(cx, |view, cx| {
            view.take_versions(Some(vec![version(1, "autosave")]), cx);
        });
        cx.run_until_parked();

        view.update_in(cx, |view, window, _| {
            assert!(view.rows[0].restore.is_focused(window));
            assert_eq!(view.undoing, None);
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
