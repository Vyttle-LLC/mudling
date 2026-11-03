//! The Skills panel: Spells and Skills, with percentages, levels and costs, a
//! filter, the practices left, Practice, and a banner when a skill improves.
//!
//! A skill is one line of columns, under a heading for what's learned and
//! one for what isn't yet. Only the lines in view are laid out, so a long
//! list costs a frame no more than a short one. Rows out of view aren't tab
//! stops, so Tab moves between rows itself, as the Find panel's does, and
//! passes over the headings.
//!
//! The panel never sends a command itself: it emits [`SendCommand`] and the
//! shell sends it, refusing it while the server asks for a password.

use crate::character_panel::CLOCK_TICK;
use crate::chat::live_region;
use crate::input::CommandInput;
use crate::room_panel::{SendCommand, Verbs, button, row_name, section_label};
use crate::shell::FocusNext;
use crate::theme::Theme;
use crate::title_bar::FocusPreviousControl;
use gpui::{
    AnyElement, ClickEvent, Context, Div, Entity, EventEmitter, FocusHandle, FontFeatures,
    FontWeight, Hsla, Pixels, Role, ScrollStrategy, SharedString, Stateful, Subscription, Task,
    UniformListScrollHandle, Window, div, prelude::*, px, relative, uniform_list,
};
use mudling_core::persist::Timestamp;
use mudling_core::store::{Improved, SheetState, SkillRow};
use std::cmp::Reverse;
use std::ops::Range;
use std::sync::Arc;

/// The key context while focus is in the panel.
pub const CONTEXT: &str = "SkillsPanel";
const FIELD_NAME: &str = "Filter skills";
const PLACEHOLDER: &str = "Filter";
const PRACTICE_VERB: &str = "practice";
const LOCKED_NOTE: &str = "Practice is off while the game asks for a password.";
const NOTHING_READ: &str = "Nothing read from the game yet.";
/// A row the game hasn't offered to teach: it has no percent and no Practice.
const NOT_YET: &str = "not learned yet";
/// Every line is this tall, headings too: the list is a uniform one.
const ROW_HEIGHT: Pixels = px(24.);
const BUTTON_HEIGHT: Pixels = px(18.);
const TAB_HEIGHT: Pixels = px(28.);
const COLUMN_GAP: Pixels = px(8.);
const MANA_WIDTH: Pixels = px(34.);
const BAR_WIDTH: Pixels = px(44.);
const PERCENT_WIDTH: Pixels = px(30.);
const PRACTICE_WIDTH: Pixels = px(56.);
const LEVEL_WIDTH: Pixels = px(26.);
/// How long an improvement stays "just" improved.
const IMPROVED_LASTS_MS: i64 = 10 * 60 * 1000;

/// The key context of the panel's field.
pub fn field_context() -> String {
    format!("{CONTEXT} > CommandInput")
}

/// Which list shows. A spell is what costs mana.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    Spells,
    Skills,
}

impl Kind {
    const ALL: [Kind; 2] = [Kind::Spells, Kind::Skills];

    fn name(self) -> &'static str {
        match self {
            Kind::Spells => "Spells",
            Kind::Skills => "Skills",
        }
    }

    /// A class list names what's taught later without saying what it costs,
    /// so until the game lists it to practice, it could be either: it shows
    /// in both.
    fn holds(self, skill: &SkillRow) -> bool {
        if skill.mana.is_none() && skill.learned.is_none() {
            return true;
        }
        let spell = skill.mana.is_some_and(|mana| mana > 0);
        spell == (self == Kind::Spells)
    }
}

/// What's learned lists first, then what the game hasn't offered to teach.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Group {
    Learned,
    NotYet,
}

impl Group {
    fn of(skill: &SkillRow) -> Group {
        match skill.learned {
            Some(_) => Group::Learned,
            None => Group::NotYet,
        }
    }

    fn heading(self) -> &'static str {
        match self {
            Group::Learned => "LEARNED",
            Group::NotYet => "NOT LEARNED YET",
        }
    }
}

/// A line of the list: a group's heading, or the row at that index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Line {
    Heading(Group),
    Skill(usize),
}

/// `kind`'s rows that match `filter`, learned first, each group in the order
/// the game teaches them, the best learned first within a level.
fn rows_of(skills: &[SkillRow], kind: Kind, filter: &str) -> Vec<SkillRow> {
    let filter = filter.trim().to_lowercase();
    let mut rows: Vec<SkillRow> = (skills.iter())
        .filter(|skill| kind.holds(skill) && skill.name.to_lowercase().contains(&filter))
        .cloned()
        .collect();
    rows.sort_by_key(|skill| {
        let group = Group::of(skill);
        (
            group,
            skill.level,
            Reverse(skill.learned),
            skill.name.clone(),
        )
    });
    rows
}

/// The rows, as `rows_of` orders them, with a heading where each group
/// starts. A group with no rows has no heading.
fn lines_of(rows: &[SkillRow]) -> Vec<Line> {
    let mut lines = Vec::with_capacity(rows.len() + 2);
    let mut group = None;
    for (index, skill) in rows.iter().enumerate() {
        let of = Group::of(skill);
        if group != Some(of) {
            lines.push(Line::Heading(of));
            group = Some(of);
        }
        lines.push(Line::Skill(index));
    }
    lines
}

/// What a row says after its name, for a screen reader:
/// "Level 1 · 15 mana · just improved".
fn details(skill: &SkillRow, just_improved: bool) -> String {
    let parts = [
        skill.level.map(|level| format!("Level {level}")),
        (skill.mana)
            .filter(|mana| *mana > 0)
            .map(|mana| format!("{mana} mana")),
        skill.learned.is_none().then(|| NOT_YET.to_owned()),
        just_improved.then(|| "just improved".to_owned()),
    ];
    let parts: Vec<String> = parts.into_iter().flatten().collect();
    parts.join(" · ")
}

fn practices_left(practices: i64) -> String {
    match practices {
        1 => "1 practice left".to_owned(),
        practices => format!("{practices} practices left"),
    }
}

/// A right-aligned column of a row, blank when there's nothing to show.
fn number_column(width: Pixels, text: Option<String>, color: Hsla) -> Div {
    div()
        .flex_none()
        .w(width)
        .flex()
        .justify_end()
        .text_size(px(12.))
        .text_color(color)
        .children(text)
}

fn banner_words(improved: &Improved) -> String {
    format!(
        "You got better at {} — now {}%.",
        improved.skill, improved.learned
    )
}

pub struct SkillsPanel {
    sheet: SheetState,
    verbs: Verbs,
    kind: Kind,
    locked: bool,
    input: Entity<CommandInput>,
    /// What's listed, and what it was made from.
    rows: Vec<SkillRow>,
    lines: Vec<Line>,
    listed_for: Option<(Kind, String)>,
    /// A handle for each row's Practice, so Tab can land on a row out of view.
    practices: Vec<FocusHandle>,
    scroll: UniformListScrollHandle,
    /// Counts improvements, so a screen reader hears each.
    announced: u64,
    focus_handle: FocusHandle,
    _typing: Subscription,
    _clock: Task<()>,
}

impl EventEmitter<SendCommand> for SkillsPanel {}

impl SkillsPanel {
    pub fn new(verbs: Verbs, cx: &mut Context<SkillsPanel>) -> SkillsPanel {
        let input = cx.new(|cx| CommandInput::field(FIELD_NAME, PLACEHOLDER, cx));
        let typing = cx.observe(&input, |_, _, cx| cx.notify());
        SkillsPanel {
            sheet: SheetState::default(),
            verbs,
            kind: Kind::default(),
            locked: false,
            input,
            rows: Vec::new(),
            lines: Vec::new(),
            listed_for: None,
            practices: Vec::new(),
            scroll: UniformListScrollHandle::new(),
            announced: 0,
            focus_handle: cx.focus_handle(),
            _typing: typing,
            _clock: cx.spawn(async move |panel, cx| {
                loop {
                    cx.background_executor().timer(CLOCK_TICK).await;
                    if panel.update(cx, |_, cx| cx.notify()).is_err() {
                        return;
                    }
                }
            }),
        }
    }

    pub fn use_verbs(&mut self, verbs: Verbs, cx: &mut Context<SkillsPanel>) {
        self.verbs = verbs;
        cx.notify();
    }

    pub fn show(&mut self, sheet: &SheetState, cx: &mut Context<SkillsPanel>) {
        if self.sheet == *sheet {
            return;
        }
        if self.sheet.improved != sheet.improved {
            self.announced += 1;
        }
        self.listed_for = None;
        self.sheet.clone_from(sheet);
        cx.notify();
    }

    /// Shows that Practice is off, as the hotbar does its chips.
    pub fn set_locked(&mut self, locked: bool, cx: &mut Context<SkillsPanel>) {
        if self.locked != locked {
            self.locked = locked;
            cx.notify();
        }
    }

    pub fn choose(&mut self, kind: Kind, cx: &mut Context<SkillsPanel>) {
        self.kind = kind;
        cx.notify();
    }

    /// Lists what the kind and the filter leave, if either moved.
    fn keep_rows_current(&mut self, cx: &mut Context<SkillsPanel>) {
        let wanted = (self.kind, self.input.read(cx).content().to_owned());
        if self.listed_for.as_ref() == Some(&wanted) {
            return;
        }
        self.rows = rows_of(&self.sheet.skills, wanted.0, &wanted.1);
        self.lines = lines_of(&self.rows);
        self.listed_for = Some(wanted);
        while self.practices.len() < self.rows.len() {
            self.practices.push(cx.focus_handle().tab_stop(true));
        }
    }

    /// The improvement to mention, while it's news.
    fn fresh_improvement(&self, now: Timestamp) -> Option<&Improved> {
        let improved = self.sheet.improved.as_ref()?;
        (now.0 - improved.at.0 < IMPROVED_LASTS_MS).then_some(improved)
    }

    fn practice(&mut self, row: usize, by_keyboard: bool, cx: &mut Context<SkillsPanel>) {
        if !self.locked
            && let Some(skill) = self.rows.get(row)
        {
            let command = self.verbs.command(PRACTICE_VERB, &skill.name);
            cx.emit(SendCommand {
                command,
                by_keyboard,
            });
        }
    }

    /// The row whose Practice has the keyboard.
    fn focused_row(&self, window: &Window) -> Option<usize> {
        let shown = &self.practices[..self.rows.len()];
        shown.iter().position(|handle| handle.is_focused(window))
    }

    fn focus_row(&mut self, row: usize, window: &mut Window, cx: &mut Context<SkillsPanel>) {
        if let Some(line) = self.lines.iter().position(|line| *line == Line::Skill(row)) {
            self.scroll.scroll_to_item(line, ScrollStrategy::Nearest);
        }
        window.focus(&self.practices[row], cx);
        cx.notify();
    }

    fn tab(&mut self, window: &mut Window, cx: &mut Context<SkillsPanel>) {
        match self.focused_row(window).map(|row| row + 1) {
            Some(next) if next < self.rows.len() => self.focus_row(next, window, cx),
            _ => window.focus_next(cx),
        }
    }

    fn shift_tab(&mut self, window: &mut Window, cx: &mut Context<SkillsPanel>) {
        match self.focused_row(window).and_then(|row| row.checked_sub(1)) {
            Some(previous) => self.focus_row(previous, window, cx),
            None => window.focus_prev(cx),
        }
    }

    /// The Spells and Skills switch, the filter and the practices left.
    fn render_toolbar(&self, theme: &Theme, cx: &mut Context<SkillsPanel>) -> impl IntoElement {
        let tabs = Kind::ALL.map(|kind| {
            let selected = kind == self.kind;
            let name = kind.name();
            button(("skills-kind", kind as usize), name.into(), theme)
                .role(Role::Tab)
                .aria_selected(selected)
                .h(TAB_HEIGHT)
                .px_3()
                .border_color(theme.segment)
                .text_size(px(12.))
                .text_color(theme.text_muted)
                .when(selected, |tab| {
                    tab.bg(theme.selected)
                        .text_color(theme.text)
                        .font_weight(FontWeight::MEDIUM)
                })
                .on_click(cx.listener(move |panel, _, _, cx| panel.choose(kind, cx)))
                .child(name)
        });
        let left = self.sheet.practices.map(practices_left);
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .child(
                div()
                    .id("skills-kinds")
                    .role(Role::TabList)
                    .aria_label("Show")
                    .flex()
                    .flex_none()
                    .p(px(3.))
                    .rounded_lg()
                    .bg(theme.segment)
                    .border_1()
                    .border_color(theme.border)
                    .capture_any_mouse_down(|_, window, _| window.prevent_default())
                    .children(tabs),
            )
            .child(div().flex_1().min_w_0().child(self.input.clone()))
            .children(left.map(|left| {
                div()
                    .id("skills-practices-left")
                    .role(Role::Label)
                    .aria_label(left.clone())
                    .flex_none()
                    .whitespace_nowrap()
                    .text_size(px(12.))
                    .text_color(theme.text_muted)
                    .child(left)
            }))
    }

    /// Names the columns over the rows: "Mana · Learned · Lv". A screen
    /// reader hears each row's own sentence instead.
    fn render_columns(theme: &Theme) -> impl IntoElement {
        let learned_width = BAR_WIDTH + PERCENT_WIDTH + PRACTICE_WIDTH + COLUMN_GAP * 2.;
        div()
            .flex()
            .flex_none()
            .gap(COLUMN_GAP)
            .px_1()
            .pb(px(3.))
            .border_b_1()
            .border_color(theme.border)
            .text_size(px(10.5))
            .text_color(theme.text_muted)
            .child(div().flex_1())
            .child(div().w(MANA_WIDTH).flex().justify_end().child("Mana"))
            .child(
                div()
                    .w(learned_width)
                    .flex()
                    .justify_center()
                    .child("Learned"),
            )
            .child(div().w(LEVEL_WIDTH).flex().justify_end().child("Lv"))
    }

    fn render_line(
        &self,
        index: usize,
        now: Timestamp,
        theme: &Theme,
        cx: &mut Context<SkillsPanel>,
    ) -> AnyElement {
        match self.lines[index] {
            Line::Heading(group) => div()
                .id(("skills-heading", index))
                .debug_selector(move || format!("skills-heading-{}", group.heading()))
                .role(Role::Heading)
                .aria_label(group.heading())
                .flex()
                .items_end()
                .h(ROW_HEIGHT)
                .px_1()
                .pb(px(2.))
                .child(section_label(group.heading(), theme))
                .into_any_element(),
            Line::Skill(row) => self
                .render_row(row, &self.rows[row], now, theme, cx)
                .into_any_element(),
        }
    }

    fn render_row(
        &self,
        index: usize,
        skill: &SkillRow,
        now: Timestamp,
        theme: &Theme,
        cx: &mut Context<SkillsPanel>,
    ) -> Stateful<Div> {
        let name: SharedString = skill.name.clone().into();
        let just_improved = self
            .fresh_improvement(now)
            .is_some_and(|improved| improved.skill == skill.name);
        let details = details(skill, just_improved);
        let practice = skill.learned.map(|_| {
            button(
                ("skills-practice", index),
                format!("Practice {name}").into(),
                theme,
            )
            .track_focus(&self.practices[index])
            .w(PRACTICE_WIDTH)
            .h(BUTTON_HEIGHT)
            .rounded(px(5.))
            .text_size(px(12.))
            .text_color(theme.accent)
            .child("Practice")
            .map(|practice| {
                if self.locked {
                    practice.opacity(0.5).aria_description(LOCKED_NOTE)
                } else {
                    practice.on_click(cx.listener(move |panel, event, _, cx| {
                        panel.practice(index, ClickEvent::is_keyboard(event), cx);
                    }))
                }
            })
        });
        let label = match skill.learned {
            Some(learned) => format!("{name}, {learned}%, {details}"),
            None => format!("{name}, {details}"),
        };
        let name_color = match (just_improved, skill.learned) {
            (true, _) => theme.accent,
            (false, Some(_)) => theme.text,
            (false, None) => theme.text_muted,
        };
        let bar = skill.learned.map(|learned| {
            div().h(px(4.)).rounded_full().bg(theme.segment).child(
                div()
                    .h_full()
                    .w(relative(learned as f32 / 100.))
                    .rounded_full()
                    .bg(theme.accent),
            )
        });
        let mana = (skill.mana)
            .filter(|mana| *mana > 0)
            .map(|mana| mana.to_string());
        let percent = skill.learned.map(|learned| format!("{learned}%"));
        let level = skill.level.map(|level| level.to_string());
        let blank = practice
            .is_none()
            .then(|| div().flex_none().w(PRACTICE_WIDTH));
        div()
            .id(("skills-row", index))
            .debug_selector(move || format!("skills-row-{index}"))
            .role(Role::ListItem)
            .aria_label(label)
            // Tab stops on every row, so one with no button is reachable too.
            .when(blank.is_some(), |row| {
                row.track_focus(&self.practices[index])
            })
            .flex()
            .flex_none()
            .w_full()
            .h(ROW_HEIGHT)
            .items_center()
            .gap(COLUMN_GAP)
            .px_1()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_color(name_color)
                    .child(row_name(name))
                    .when(just_improved, |name| {
                        name.child(div().flex_none().text_size(px(12.)).child("↑"))
                    }),
            )
            .child(number_column(MANA_WIDTH, mana, theme.text_muted))
            .child(div().flex_none().w(BAR_WIDTH).children(bar))
            .child(number_column(PERCENT_WIDTH, percent, theme.text))
            .children(practice)
            .children(blank)
            .child(number_column(LEVEL_WIDTH, level, theme.text_muted))
    }

    fn render_list(&self, theme: &Theme, cx: &mut Context<SkillsPanel>) -> Stateful<Div> {
        if self.rows.is_empty() {
            let typed = self.input.read(cx).content().trim().to_owned();
            let empty = if typed.is_empty() {
                NOTHING_READ.to_owned()
            } else {
                format!("Nothing matches \"{typed}\".")
            };
            return div()
                .id("skills-empty")
                .role(Role::Label)
                .aria_label(empty.clone())
                .text_color(theme.text_muted)
                .child(empty);
        }
        let rows = cx.processor(|panel, range: Range<usize>, _, cx| {
            let theme = *cx.global::<Theme>();
            let now = Timestamp::now();
            let end = range.end.min(panel.lines.len());
            (range.start..end)
                .map(|line| panel.render_line(line, now, &theme, cx))
                .collect()
        });
        div()
            .id("skills-list")
            .debug_selector(|| "skills-list".to_owned())
            .role(Role::List)
            .aria_label(self.kind.name())
            .flex_1()
            .min_h_0()
            .capture_any_mouse_down(|_, window, _| window.prevent_default())
            .font_features(FontFeatures(Arc::new(vec![("tnum".to_owned(), 1)])))
            .child(
                uniform_list("skills-rows", self.lines.len(), rows)
                    .track_scroll(&self.scroll)
                    .size_full(),
            )
    }
}

impl Render for SkillsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<SkillsPanel>) -> impl IntoElement {
        self.keep_rows_current(cx);
        let theme = *cx.global::<Theme>();
        let improved = self.fresh_improvement(Timestamp::now());
        let banner = improved.map(banner_words);
        let drawn_banner = improved.map(|improved| {
            let (skill, learned) = (improved.skill.clone(), improved.learned);
            div()
                .flex()
                .flex_wrap()
                .gap_x_1()
                .child(div().text_color(theme.accent).pr_2().child("↑"))
                .child("You got better at")
                .child(div().font_weight(FontWeight::BOLD).child(skill))
                .child(format!("— now {learned}%."))
        });
        div()
            .id("skills-panel")
            .debug_selector(|| "skills-panel".to_owned())
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .role(Role::Group)
            .aria_label("Skills")
            .on_action(cx.listener(|panel, _: &FocusNext, window, cx| panel.tab(window, cx)))
            .on_action(cx.listener(|panel, _: &FocusPreviousControl, window, cx| {
                panel.shift_tab(window, cx);
            }))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .gap_2()
            .pt_2()
            .text_size(px(14.))
            .text_color(theme.text)
            .child(self.render_toolbar(&theme, cx))
            .children(banner.clone().zip(drawn_banner).map(|(banner, drawn)| {
                div()
                    .id("skills-banner")
                    .debug_selector(|| "skills-banner".to_owned())
                    .role(Role::Label)
                    .aria_label(banner)
                    .flex_none()
                    .py_2()
                    .px_3()
                    .rounded_lg()
                    .bg(theme.accent.opacity(0.12))
                    .text_size(px(13.))
                    .child(drawn)
            }))
            .when(!self.rows.is_empty(), |panel| {
                panel.child(Self::render_columns(&theme))
            })
            .child(self.render_list(&theme, cx))
            .children(
                banner
                    .map(|banner| live_region("skills-panel-said", self.announced, banner.into())),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measure::sample_sheet;
    use crate::room_panel::press_key;
    use gpui::{TestAppContext, VisualTestContext, WindowAppearance};
    use std::cell::RefCell;
    use std::rc::Rc;

    type Sent = Rc<RefCell<Vec<(String, bool)>>>;

    fn skill(name: &str, level: i64, mana: Option<i64>) -> SkillRow {
        SkillRow {
            level: Some(level),
            learned: Some(40),
            mana,
            ..SkillRow::named(name)
        }
    }

    fn panel_window(
        cx: &mut TestAppContext,
    ) -> (Entity<SkillsPanel>, &mut VisualTestContext, Sent) {
        cx.update(|cx| {
            crate::bind_keys(cx);
            cx.set_global(Theme::for_appearance(WindowAppearance::Dark));
        });
        let sent = Sent::default();
        let log = sent.clone();
        let (panel, cx) = cx.add_window_view(|_, cx| SkillsPanel::new(Verbs::default(), cx));
        cx.update(|_, cx| {
            cx.subscribe(&panel, move |_, event: &SendCommand, _| {
                log.borrow_mut()
                    .push((event.command.clone(), event.by_keyboard));
            })
            .detach();
        });
        panel.update(cx, |panel, cx| panel.show(&sample_sheet().0, cx));
        cx.run_until_parked();
        (panel, cx, sent)
    }

    #[test]
    fn a_spell_costs_mana_and_the_rest_are_skills() {
        let list = [
            skill("kick", 7, Some(0)),
            skill("armor", 1, Some(5)),
            skill("dodge", 4, None),
        ];
        let names = |kind| -> Vec<String> {
            let rows = rows_of(&list, kind, "");
            rows.into_iter().map(|skill| skill.name).collect()
        };
        assert_eq!(names(Kind::Spells), ["armor"]);
        assert_eq!(names(Kind::Skills), ["dodge", "kick"], "by level");
    }

    #[test]
    fn a_name_only_the_class_list_gave_shows_in_both_lists_as_not_learned() {
        let later = SkillRow {
            level: Some(50),
            ..SkillRow::named("mass invigorate")
        };
        let list = [later.clone()];
        assert_eq!(rows_of(&list, Kind::Spells, "").len(), 1);
        assert_eq!(rows_of(&list, Kind::Skills, "").len(), 1);
        assert_eq!(details(&later, false), "Level 50 · not learned yet");
    }

    #[gpui::test]
    fn a_row_not_learned_yet_has_no_practice_but_the_keyboard_reaches_it(cx: &mut TestAppContext) {
        let (panel, cx, sent) = panel_window(cx);
        let last = panel.read_with(cx, |panel, _| panel.rows.len() - 1);
        panel.update_in(cx, |panel, window, cx| panel.focus_row(last, window, cx));
        cx.run_until_parked();
        let (name, learned) = panel.read_with(cx, |panel, _| {
            (panel.rows[last].name.clone(), panel.rows[last].learned)
        });
        assert_eq!((name.as_str(), learned), ("mass invigorate", None));
        assert!(cx.debug_bounds("Practice mass invigorate").is_none());
        press_key("enter", cx);
        assert!(sent.borrow().is_empty());
        panel.update_in(cx, |panel, window, _| {
            assert_eq!(panel.focused_row(window), Some(last));
        });
    }

    #[test]
    fn learned_rows_list_first_under_headings_and_an_empty_group_has_none() {
        let later = SkillRow {
            level: Some(2),
            ..SkillRow::named("weapon lore")
        };
        let list = [later, skill("kick", 7, None), skill("dodge", 4, None)];
        let rows = rows_of(&list, Kind::Skills, "");
        let names: Vec<&str> = rows.iter().map(|skill| skill.name.as_str()).collect();
        assert_eq!(names, ["dodge", "kick", "weapon lore"], "learned first");
        assert_eq!(
            lines_of(&rows),
            [
                Line::Heading(Group::Learned),
                Line::Skill(0),
                Line::Skill(1),
                Line::Heading(Group::NotYet),
                Line::Skill(2),
            ]
        );
        let learned_only = rows_of(&list, Kind::Skills, "d");
        assert_eq!(
            lines_of(&learned_only),
            [Line::Heading(Group::Learned), Line::Skill(0)]
        );
        let not_yet_only = rows_of(&list, Kind::Skills, "lore");
        assert_eq!(
            lines_of(&not_yet_only),
            [Line::Heading(Group::NotYet), Line::Skill(0)]
        );
        assert!(lines_of(&[]).is_empty());
    }

    #[gpui::test]
    fn tab_reaches_every_row_in_order_but_no_heading(cx: &mut TestAppContext) {
        let (panel, cx, _) = panel_window(cx);
        let rows = panel.read_with(cx, |panel, _| {
            assert!(panel.lines.contains(&Line::Heading(Group::NotYet)));
            panel.rows.len()
        });
        panel.update_in(cx, |panel, window, cx| panel.focus_row(0, window, cx));
        cx.run_until_parked();
        let mut reached = vec![0];
        for _ in 1..rows {
            cx.simulate_keystrokes("tab");
            cx.run_until_parked();
            let focused = panel.update_in(cx, |panel, window, _| panel.focused_row(window));
            reached.extend(focused);
        }
        assert_eq!(reached, (0..rows).collect::<Vec<_>>());
    }

    #[test]
    fn the_filter_ignores_case_and_outer_spaces() {
        let list = [
            skill("Magic Missile", 1, Some(15)),
            skill("armor", 1, Some(5)),
        ];
        let rows = rows_of(&list, Kind::Spells, " magic ");
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn a_row_says_what_the_canvas_does() {
        let spell = skill("magic missile", 1, Some(15));
        assert_eq!(details(&spell, true), "Level 1 · 15 mana · just improved");
        assert_eq!(details(&skill("kick", 7, Some(0)), false), "Level 7");
        assert_eq!(practices_left(1), "1 practice left");
        assert_eq!(practices_left(4), "4 practices left");
    }

    #[gpui::test]
    fn practice_sends_the_skill_by_mouse_and_keyboard(cx: &mut TestAppContext) {
        let (_panel, cx, sent) = panel_window(cx);
        let bounds = cx
            .debug_bounds("Practice magic missile")
            .expect("the first row shows");
        cx.simulate_click(bounds.center(), gpui::Modifiers::none());
        assert_eq!(
            sent.borrow().last().unwrap(),
            &("practice magic missile".to_owned(), false)
        );
    }

    #[gpui::test]
    fn tab_walks_the_rows_past_the_ones_in_view(cx: &mut TestAppContext) {
        let (panel, cx, sent) = panel_window(cx);
        panel.update_in(cx, |panel, window, cx| panel.focus_row(0, window, cx));
        cx.run_until_parked();
        // The very last row isn't learned yet, so it has no Practice.
        let learned = panel.read_with(cx, |panel, _| panel.rows.len()) - 1;
        for _ in 1..learned {
            cx.simulate_keystrokes("tab");
            cx.run_until_parked();
        }
        press_key("enter", cx);
        let (command, by_keyboard) = sent.borrow().last().cloned().expect("it was pressed");
        assert!(by_keyboard);
        let last = panel.read_with(cx, |panel, _| panel.rows[learned - 1].name.clone());
        assert_eq!(command, format!("practice {last}"));
    }

    #[gpui::test]
    fn practice_does_nothing_while_the_game_asks_for_a_password(cx: &mut TestAppContext) {
        let (panel, cx, sent) = panel_window(cx);
        panel.update(cx, |panel, cx| panel.set_locked(true, cx));
        cx.run_until_parked();
        let bounds = cx.debug_bounds("Practice magic missile").unwrap();
        cx.simulate_click(bounds.center(), gpui::Modifiers::none());
        assert!(sent.borrow().is_empty(), "by mouse");
        panel.update_in(cx, |panel, window, cx| panel.focus_row(0, window, cx));
        cx.run_until_parked();
        press_key("enter", cx);
        assert!(sent.borrow().is_empty(), "by keyboard");

        panel.update(cx, |panel, cx| panel.set_locked(false, cx));
        cx.run_until_parked();
        press_key("enter", cx);
        assert_eq!(sent.borrow().len(), 1, "back once the password's done");
    }

    #[gpui::test]
    fn the_banner_shows_a_fresh_improvement_only(cx: &mut TestAppContext) {
        let (panel, cx, _) = panel_window(cx);
        assert!(cx.debug_bounds("skills-banner").is_some(), "the sample's");
        panel.update(cx, |panel, cx| {
            let mut sheet = sample_sheet().0;
            sheet.improved.as_mut().unwrap().at = Timestamp(0);
            panel.show(&sheet, cx);
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("skills-banner").is_none());
    }

    #[gpui::test]
    fn switching_lists_starts_from_the_other_kind(cx: &mut TestAppContext) {
        let (panel, cx, _) = panel_window(cx);
        panel.update(cx, |panel, cx| panel.choose(Kind::Skills, cx));
        cx.run_until_parked();
        let names: Vec<String> = panel.read_with(cx, |panel, _| {
            panel.rows.iter().map(|r| r.name.clone()).collect()
        });
        assert!(
            names.iter().all(|name| name != "magic missile"),
            "{names:?}"
        );
        assert!(!names.is_empty());
    }
}
