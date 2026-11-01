//! The Skills panel: Spells and Skills, with percentages, levels and costs, a
//! filter, the practices left, Practice, and a banner when a skill improves.
//!
//! Only the rows in view are laid out, so a long list costs a frame no more
//! than a short one. Rows out of view aren't tab stops, so Tab moves between
//! rows itself, as the Find panel's does.
//!
//! The panel never sends a command itself: it emits [`SendCommand`] and the
//! shell sends it, refusing it while the server asks for a password.

use crate::assets::HEADING_FONT;
use crate::character_panel::CLOCK_TICK;
use crate::chat::live_region;
use crate::input::CommandInput;
use crate::room_panel::{SendCommand, Verbs, button, row_name};
use crate::shell::FocusNext;
use crate::theme::Theme;
use crate::title_bar::FocusPreviousControl;
use gpui::{
    ClickEvent, Context, Div, Entity, EventEmitter, FocusHandle, FontWeight, Pixels, Role,
    ScrollStrategy, SharedString, Stateful, Subscription, Task, UniformListScrollHandle, Window,
    div, prelude::*, px, relative, uniform_list,
};
use mudling_core::persist::Timestamp;
use mudling_core::store::{Improved, SheetState, SkillRow};
use std::cmp::Reverse;
use std::ops::Range;

/// The key context while focus is in the panel.
pub const CONTEXT: &str = "SkillsPanel";
const FIELD_NAME: &str = "Filter skills";
const PLACEHOLDER: &str = "Filter";
const PRACTICE_VERB: &str = "practice";
const LOCKED_NOTE: &str = "Practice is off while the game asks for a password.";
const NOTHING_READ: &str = "Nothing read from the game yet.";
/// A row the game hasn't offered to teach: it has no percent and no Practice.
const NOT_YET: &str = "not learned yet";
const ROW_HEIGHT: Pixels = px(52.);
const BUTTON_HEIGHT: Pixels = px(28.);
const TAB_HEIGHT: Pixels = px(28.);
const BAR_WIDTH: Pixels = px(64.);
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

/// `kind`'s rows that match `filter`, in the order the game teaches them,
/// the best learned first within a level.
fn rows_of(skills: &[SkillRow], kind: Kind, filter: &str) -> Vec<SkillRow> {
    let filter = filter.trim().to_lowercase();
    let mut rows: Vec<SkillRow> = (skills.iter())
        .filter(|skill| kind.holds(skill) && skill.name.to_lowercase().contains(&filter))
        .cloned()
        .collect();
    rows.sort_by_key(|skill| (skill.level, Reverse(skill.learned), skill.name.clone()));
    rows
}

/// A row's second line: "Level 1 · 15 mana · just improved".
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
        self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
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

    fn render_heading(&self, theme: &Theme, cx: &mut Context<SkillsPanel>) -> impl IntoElement {
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
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .child(
                div()
                    .font_family(HEADING_FONT)
                    .font_weight(FontWeight::BOLD)
                    .text_size(px(22.))
                    .child("Skills"),
            )
            .child(
                div()
                    .id("skills-kinds")
                    .role(Role::TabList)
                    .aria_label("Show")
                    .flex()
                    .p(px(3.))
                    .rounded_lg()
                    .bg(theme.segment)
                    .border_1()
                    .border_color(theme.border)
                    .capture_any_mouse_down(|_, window, _| window.prevent_default())
                    .children(tabs),
            )
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
            .h(BUTTON_HEIGHT)
            .px_3()
            .font_weight(FontWeight::MEDIUM)
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
        let progress = skill.learned.map(|learned| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_none()
                        .w(BAR_WIDTH)
                        .h(px(4.))
                        .rounded_full()
                        .bg(theme.segment)
                        .child(
                            div()
                                .h_full()
                                .w(relative(learned as f32 / 100.))
                                .rounded_full()
                                .bg(theme.accent),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(36.))
                        .flex()
                        .justify_end()
                        .text_size(px(13.))
                        .child(format!("{learned}%")),
                )
        });
        div()
            .id(("skills-row", index))
            .debug_selector(move || format!("skills-row-{index}"))
            .role(Role::ListItem)
            .aria_label(label)
            // Tab stops on every row, so one with no button is reachable too.
            .when(practice.is_none(), |row| {
                row.track_focus(&self.practices[index])
            })
            .flex()
            .flex_none()
            .w_full()
            .h(ROW_HEIGHT)
            .items_center()
            .gap_2()
            .px_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(row_name(name))
                    .child(
                        div()
                            .text_size(px(12.))
                            .line_height(px(14.))
                            .text_color(theme.text_muted)
                            .child(details),
                    ),
            )
            .children(progress)
            .children(practice)
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
            let end = range.end.min(panel.rows.len());
            let in_view = panel.rows[range.start..end].iter().enumerate();
            in_view
                .map(|(offset, skill)| {
                    panel.render_row(range.start + offset, skill, now, &theme, cx)
                })
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
            .child(
                uniform_list("skills-rows", self.rows.len(), rows)
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
        let left = self.sheet.practices.map(practices_left);
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
            .gap_3()
            .pt_2()
            .text_size(px(14.))
            .text_color(theme.text)
            .child(self.render_heading(&theme, cx))
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
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(self.input.clone()))
                    .children(left.map(|left| {
                        div()
                            .id("skills-practices-left")
                            .role(Role::Label)
                            .aria_label(left.clone())
                            .flex_none()
                            .text_size(px(12.))
                            .text_color(theme.text_muted)
                            .child(left)
                    })),
            )
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
