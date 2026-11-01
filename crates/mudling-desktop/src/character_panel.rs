//! The Character panel: stats with their maximums, progress, combat numbers
//! and affects, and where they came from and how fresh they are.
//!
//! All of it comes from the sheet the core keeps for this connection's
//! character, the live vitals and the live affects. The panel never sends a
//! command: Refresh emits [`RefreshCharacter`], and the shell asks the session
//! for a quiet read.

use crate::assets::HEADING_FONT;
use crate::room_panel::{button, row_name, section_label};
use crate::theme::Theme;
use gpui::{
    AnyElement, Context, Div, EventEmitter, FocusHandle, FontWeight, Role, Stateful, Task, Window,
    div, prelude::*, px,
};
use mudling_core::defaults::hud::Vital;
use mudling_core::persist::Timestamp;
use mudling_core::plugin::luau::Value;
use mudling_core::store::{
    Affect, CHARACTER_POSITION, CharactersState, ConnectionStatus, SettingsState, SheetState,
    VitalsState,
};
use std::time::Duration;

/// How often "updated 2 min ago" is worked out again.
pub const CLOCK_TICK: Duration = Duration::from_secs(30);
const NAMELESS: &str = "Your character shows here once the game names them.";
const NO_AFFECTS: &str = "Nothing is affecting you.";
const UPDATING: &str = "Updating quietly…";
const NOT_READ: &str = "Not read from the game yet.";
const OFFLINE: &str = "Refresh needs a connection.";
/// The attributes, as the game's keys and the tile's heading.
const ATTRIBUTES: [(&str, &str, &str); 5] = [
    ("str", "STR", "Strength"),
    ("int", "INT", "Intelligence"),
    ("wis", "WIS", "Wisdom"),
    ("dex", "DEX", "Dexterity"),
    ("con", "CON", "Constitution"),
];
const MILLIS_PER_MINUTE: i64 = 60_000;
const MINUTES_PER_HOUR: i64 = 60;
const MINUTES_PER_DAY: i64 = 24 * 60;
const MINUS: char = '\u{2212}';

/// The player asked for the character's tables again.
pub struct RefreshCharacter {
    pub by_keyboard: bool,
}

impl EventEmitter<RefreshCharacter> for CharacterPanel {}

/// One of the five attributes.
#[derive(Clone, Debug, PartialEq)]
struct Attribute {
    heading: &'static str,
    /// What a screen reader says it is.
    name: &'static str,
    value: String,
    most: Option<String>,
}

/// A label and its value, in a card.
#[derive(Clone, Debug, PartialEq)]
struct Fact {
    label: &'static str,
    value: String,
}

/// How far through this level the character is.
#[derive(Clone, Debug, PartialEq)]
struct Progress {
    heading: String,
    to_go: String,
    /// 0 to 1.
    share: f32,
}

/// What the panel draws, worked out from the stores.
#[derive(Clone, Debug, Default, PartialEq)]
struct Sheet {
    name: String,
    about: String,
    attributes: Vec<Attribute>,
    progress: Option<Progress>,
    advancement: Vec<Fact>,
    combat: Vec<Fact>,
    affects: Vec<Affect>,
    reading: bool,
    sources: Vec<String>,
    read_at: Option<Timestamp>,
    connected: bool,
}

/// A whole number as the game gave it.
fn number(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(number) => Some(*number as i64),
        Value::Text(text) => text.trim().parse().ok(),
        Value::Bool(_) => None,
    }
}

fn words(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Text(text) if !text.is_empty() => Some(text.clone()),
        _ => None,
    }
}

/// `5001` as `5,001`, with a real minus sign.
fn grouped(number: i64) -> String {
    let digits = number.unsigned_abs().to_string();
    let mut text = String::new();
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            text.push(',');
        }
        text.push(digit);
    }
    if number < 0 {
        text.insert(0, MINUS);
    }
    text
}

/// A bonus or penalty, always with its sign.
fn signed(number: i64) -> String {
    if number > 0 {
        format!("+{}", grouped(number))
    } else {
        grouped(number)
    }
}

/// "just now", "2 min ago", "3 h ago", "5 d ago".
pub fn age_words(then: Timestamp, now: Timestamp) -> String {
    let minutes = (now.0 - then.0).max(0) / MILLIS_PER_MINUTE;
    match minutes {
        0 => "just now".to_owned(),
        1..MINUTES_PER_HOUR => format!("{minutes} min ago"),
        MINUTES_PER_HOUR..MINUTES_PER_DAY => format!("{} h ago", minutes / MINUTES_PER_HOUR),
        _ => format!("{} d ago", minutes / MINUTES_PER_DAY),
    }
}

/// "score", "score and aff", "score, practice and aff".
fn listed(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [only] => only.clone(),
        [first @ .., last] => format!("{} and {last}", first.join(", ")),
    }
}

impl Sheet {
    fn of(
        sheet: &SheetState,
        characters: &CharactersState,
        vitals: &VitalsState,
        settings: &SettingsState,
    ) -> Sheet {
        let stat = |key: &str| sheet.stats.get(key);
        let connected = settings.connection == ConnectionStatus::Connected;
        let level = number(stat("level"));
        let position = words(characters.0.get(CHARACTER_POSITION)).filter(|_| connected);
        let who = [words(stat("race")), words(stat("class"))]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        let about = [
            level.map(|level| format!("Level {level}")),
            Some(who).filter(|who| !who.is_empty()),
            position,
        ];
        let name = match words(stat("name")) {
            Some(name) => name,
            None => sheet.character.clone(),
        };
        let fact = |label, value: Option<String>| value.map(|value| Fact { label, value });
        let or_dash = |value: Option<String>| Some(value.unwrap_or_else(|| "–".to_owned()));
        let practices = sheet.practices.or(number(stat("practice")));
        let gold = number(stat("gold")).map(grouped);
        Sheet {
            name,
            about: about.into_iter().flatten().collect::<Vec<_>>().join(" · "),
            attributes: ATTRIBUTES
                .iter()
                .map(|&(key, heading, name)| Attribute {
                    heading,
                    name,
                    value: number(stat(key)).map_or_else(|| "–".to_owned(), |n| n.to_string()),
                    most: number(stat(&format!("{key}_perm"))).map(|most| format!("max {most}")),
                })
                .collect(),
            progress: Progress::of(level, vitals),
            advancement: [
                fact("Practices", or_dash(practices.map(grouped))),
                fact("Trains", number(stat("trains")).map(grouped)),
                fact("Gold", or_dash(gold)),
                fact("Quest points", number(stat("quest_points")).map(grouped)),
            ]
            .into_iter()
            .flatten()
            .collect(),
            combat: [
                fact("Hitroll", or_dash(number(stat("hitroll")).map(signed))),
                fact("Damroll", or_dash(number(stat("damroll")).map(signed))),
                fact("Armor", or_dash(number(stat("ac")).map(signed))),
                fact("Saves", number(stat("saves")).map(signed)),
                fact("Alignment", or_dash(number(stat("alignment")).map(grouped))),
                fact(
                    "Wimpy",
                    or_dash(number(stat("wimpy")).map(|hits| format!("{} hp", grouped(hits)))),
                ),
            ]
            .into_iter()
            .flatten()
            .collect(),
            affects: if connected {
                characters.affects()
            } else {
                sheet.affects.clone()
            },
            reading: sheet.reading,
            sources: sheet.sources.clone(),
            read_at: sheet.character_read,
            connected,
        }
    }

    /// Where the numbers came from and how fresh they are.
    fn freshness(&self, now: Timestamp) -> String {
        if self.reading {
            return UPDATING.to_owned();
        }
        match (self.read_at, self.sources.as_slice()) {
            (Some(at), []) => format!("Updated {}", age_words(at, now)),
            (Some(at), sources) => {
                format!("From {} · updated {}", listed(sources), age_words(at, now))
            }
            (None, _) => NOT_READ.to_owned(),
        }
    }
}

impl Progress {
    fn of(level: Option<i64>, vitals: &VitalsState) -> Option<Progress> {
        let level = level?;
        let to_go = *vitals.0.get(&Vital::ToNextLevel)?;
        let whole = vitals.0.get(&Vital::MaxToNextLevel).copied();
        Some(Progress {
            heading: format!("Level {level} → {}", level + 1),
            to_go: format!("{} xp to go", grouped(to_go)),
            share: whole
                .filter(|whole| *whole > 0)
                .map_or(0., |whole| 1. - (to_go as f32 / whole as f32).clamp(0., 1.)),
        })
    }
}

/// A rounded card holding rows.
fn card(theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
}

/// Facts in two columns, as the canvas sets them.
fn fact_rows(
    id: &'static str,
    label: &'static str,
    facts: &[Fact],
    theme: &Theme,
) -> Stateful<Div> {
    let fact = |(index, fact): (usize, &Fact)| {
        div()
            .id((id, index))
            .role(Role::ListItem)
            .aria_label(format!("{}: {}", fact.label, fact.value))
            .w_1_2()
            .pr_3()
            .flex()
            .justify_between()
            .child(
                div()
                    .text_color(theme.text_muted)
                    .child(fact.label.to_owned()),
            )
            .child(fact.value.clone())
    };
    div()
        .id(id)
        .role(Role::List)
        .aria_label(label)
        .flex()
        .flex_wrap()
        .gap_y_1()
        .children(facts.iter().enumerate().map(fact))
}

pub struct CharacterPanel {
    sheet: Sheet,
    focus_handle: FocusHandle,
    _clock: Task<()>,
}

impl CharacterPanel {
    pub fn new(cx: &mut Context<CharacterPanel>) -> CharacterPanel {
        CharacterPanel {
            sheet: Sheet::default(),
            focus_handle: cx.focus_handle(),
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

    /// Takes in the stores; only a real change redraws.
    pub fn show(
        &mut self,
        sheet: &SheetState,
        characters: &CharactersState,
        vitals: &VitalsState,
        settings: &SettingsState,
        cx: &mut Context<CharacterPanel>,
    ) {
        let drawn = Sheet::of(sheet, characters, vitals, settings);
        if self.sheet != drawn {
            self.sheet = drawn;
            cx.notify();
        }
    }

    fn render_heading(&self, theme: &Theme, cx: &mut Context<CharacterPanel>) -> impl IntoElement {
        let connected = self.sheet.connected;
        let refresh = button("character-refresh", "Refresh the character".into(), theme)
            .h(px(28.))
            .px_2()
            .text_size(px(12.))
            .text_color(theme.text_muted)
            .child("Refresh")
            .map(|refresh| {
                if connected {
                    refresh.on_click(cx.listener(|_, event: &gpui::ClickEvent, _, cx| {
                        cx.emit(RefreshCharacter {
                            by_keyboard: event.is_keyboard(),
                        });
                    }))
                } else {
                    refresh.opacity(0.5).aria_description(OFFLINE)
                }
            });
        div()
            .flex()
            .items_start()
            .justify_between()
            .gap_2()
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .id("character-name")
                            .role(Role::Heading)
                            .aria_label(self.sheet.name.clone())
                            .font_family(HEADING_FONT)
                            .text_size(px(22.))
                            .font_weight(FontWeight::BOLD)
                            .child(row_name(self.sheet.name.clone().into())),
                    )
                    .child(
                        div()
                            .id("character-about")
                            .role(Role::Label)
                            .aria_label(self.sheet.about.clone())
                            .text_size(px(13.))
                            .text_color(theme.text_muted)
                            .child(self.sheet.about.clone()),
                    ),
            )
            .child(refresh)
    }

    fn render_attributes(&self, theme: &Theme) -> impl IntoElement {
        let tile = |(index, attribute): (usize, &Attribute)| {
            let label = match &attribute.most {
                Some(most) => format!("{} {}, {most}", attribute.name, attribute.value),
                None => format!("{} {}", attribute.name, attribute.value),
            };
            div()
                .id(("character-attribute", index))
                .role(Role::ListItem)
                .aria_label(label)
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .items_center()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .child(section_label(attribute.heading, theme))
                .child(
                    div()
                        .text_size(px(20.))
                        .font_weight(FontWeight::BOLD)
                        .child(attribute.value.clone()),
                )
                .children(attribute.most.clone().map(|most| {
                    div()
                        .text_size(px(11.))
                        .text_color(theme.text_muted)
                        .child(most)
                }))
        };
        div()
            .id("character-attributes")
            .role(Role::List)
            .aria_label("Attributes")
            .flex()
            .gap_2()
            .children(self.sheet.attributes.iter().enumerate().map(tile))
    }

    fn render_progress(&self, theme: &Theme) -> impl IntoElement {
        let bar = self.sheet.progress.as_ref().map(|progress| {
            div()
                .id("character-progress")
                .role(Role::ProgressIndicator)
                .aria_label(format!("{}, {}", progress.heading, progress.to_go))
                .flex()
                .flex_col()
                .gap_2()
                .pb_2()
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .child(progress.heading.clone())
                        .child(progress.to_go.clone()),
                )
                .child(
                    div().h(px(6.)).rounded_full().bg(theme.segment).child(
                        div()
                            .h_full()
                            .w(gpui::relative(progress.share))
                            .rounded_full()
                            .bg(theme.accent),
                    ),
                )
        });
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(section_label("PROGRESS", theme))
            .child(card(theme).children(bar).child(fact_rows(
                "character-advancement",
                "Progress",
                &self.sheet.advancement,
                theme,
            )))
    }

    fn render_combat(&self, theme: &Theme) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(section_label("COMBAT", theme))
            .child(card(theme).child(fact_rows(
                "character-combat",
                "Combat",
                &self.sheet.combat,
                theme,
            )))
    }

    fn render_affects(&self, theme: &Theme) -> impl IntoElement {
        let row = |(index, affect): (usize, &Affect)| {
            let detail = affect.detail.replace('\n', "; ");
            let label = match detail.as_str() {
                "" => affect.name.clone(),
                detail => format!("{}: {detail}", affect.name),
            };
            div()
                .id(("character-affect", index))
                .role(Role::ListItem)
                .aria_label(label)
                .flex()
                .flex_col()
                .child(row_name(affect.name.clone().into()))
                .when(!detail.is_empty(), |row| {
                    row.child(
                        div()
                            .text_size(px(12.))
                            .text_color(theme.text_muted)
                            .child(detail),
                    )
                })
        };
        let list: AnyElement = if self.sheet.affects.is_empty() {
            card(theme)
                .border_dashed()
                .id("character-no-affects")
                .role(Role::Label)
                .aria_label(NO_AFFECTS)
                .text_color(theme.text_muted)
                .child(NO_AFFECTS)
                .into_any_element()
        } else {
            card(theme)
                .id("character-affects")
                .role(Role::List)
                .aria_label("Affects")
                .children(self.sheet.affects.iter().enumerate().map(row))
                .into_any_element()
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(section_label("AFFECTS", theme))
            .child(list)
    }
}

impl Render for CharacterPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<CharacterPanel>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let panel = div()
            .id("character-panel")
            .debug_selector(|| "character-panel".to_owned())
            .track_focus(&self.focus_handle)
            // The mouse never moves focus here: typing stays with the bar.
            .capture_any_mouse_down(|_, window, _| window.prevent_default())
            .role(Role::Group)
            .aria_label("Character")
            .size_full()
            .flex()
            .flex_col()
            .pt_2()
            .text_size(px(14.))
            .text_color(theme.text);
        if self.sheet.name.is_empty() {
            return panel
                .child(
                    div()
                        .id("character-nameless")
                        .role(Role::Label)
                        .aria_label(NAMELESS)
                        .text_color(theme.text_muted)
                        .child(NAMELESS),
                )
                .into_any_element();
        }
        let freshness = self.sheet.freshness(Timestamp::now());
        panel
            .child(
                div()
                    .id("character-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(self.render_heading(&theme, cx))
                    .child(self.render_attributes(&theme))
                    .child(self.render_progress(&theme))
                    .child(self.render_combat(&theme))
                    .child(self.render_affects(&theme)),
            )
            .child(
                div()
                    .id("character-freshness")
                    .role(Role::Status)
                    .aria_label(freshness.clone())
                    .flex_none()
                    .pt_2()
                    .text_size(px(12.))
                    .text_color(theme.text_muted)
                    .child(freshness),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measure::sample_sheet;
    use crate::room_panel::press_key;
    use gpui::{Entity, TestAppContext, VisualTestContext, WindowAppearance};
    use std::cell::Cell;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    fn connected() -> SettingsState {
        SettingsState {
            connection: ConnectionStatus::Connected,
            ..SettingsState::default()
        }
    }

    fn sample() -> Sheet {
        let (sheet, characters, vitals) = sample_sheet();
        Sheet::of(&sheet, &characters, &vitals, &connected())
    }

    fn panel_window(cx: &mut TestAppContext) -> (Entity<CharacterPanel>, &mut VisualTestContext) {
        cx.update(|cx| {
            crate::bind_keys(cx);
            cx.set_global(Theme::for_appearance(WindowAppearance::Dark));
        });
        let (panel, cx) = cx.add_window_view(|_, cx| CharacterPanel::new(cx));
        (panel, cx)
    }

    #[test]
    fn it_says_who_and_what_as_the_canvas_does() {
        let sheet = sample();
        assert_eq!(sheet.name, "Altissima");
        assert_eq!(sheet.about, "Level 2 · Human Mage · Standing");
        assert_eq!(sheet.attributes[0].value, "14");
        assert_eq!(sheet.attributes[0].most.as_deref(), Some("max 16"));
        let progress = sheet.progress.unwrap();
        assert_eq!(progress.heading, "Level 2 → 3");
        assert_eq!(progress.to_go, "5,001 xp to go");
    }

    #[test]
    fn a_number_the_game_never_gave_leaves_its_row_or_shows_a_dash() {
        let sheet = Sheet::of(
            &SheetState {
                character: "bren".to_owned(),
                ..SheetState::default()
            },
            &CharactersState::default(),
            &VitalsState::default(),
            &connected(),
        );
        assert_eq!(sheet.name, "bren");
        assert_eq!(sheet.progress, None);
        let labels: Vec<&str> = sheet.combat.iter().map(|fact| fact.label).collect();
        assert_eq!(
            labels,
            ["Hitroll", "Damroll", "Armor", "Alignment", "Wimpy"],
            "Avatar has no saves: the row goes"
        );
        assert_eq!(sheet.combat[0].value, "–");
    }

    #[test]
    fn the_signs_and_groups_read_as_the_canvas_writes_them() {
        assert_eq!(signed(2), "+2");
        assert_eq!(signed(-4), "−4");
        assert_eq!(signed(0), "0");
        assert_eq!(grouped(5001), "5,001");
        assert_eq!(grouped(-1234567), "−1,234,567");
        assert_eq!(grouped(999), "999");
    }

    #[test]
    fn freshness_says_where_and_how_long_ago() {
        let mut sheet = Sheet {
            sources: vec!["score".to_owned(), "aff".to_owned()],
            read_at: Some(Timestamp(0)),
            ..Sheet::default()
        };
        let two_minutes = Timestamp(2 * MILLIS_PER_MINUTE);
        assert_eq!(
            sheet.freshness(two_minutes),
            "From score and aff · updated 2 min ago"
        );
        sheet.reading = true;
        assert_eq!(sheet.freshness(two_minutes), UPDATING);
        sheet.reading = false;
        sheet.read_at = None;
        assert_eq!(sheet.freshness(two_minutes), NOT_READ);
    }

    #[test]
    fn ages_grow_from_minutes_to_days() {
        let minutes = |minutes: i64| Timestamp(minutes * MILLIS_PER_MINUTE);
        for (age, words) in [
            (0, "just now"),
            (1, "1 min ago"),
            (59, "59 min ago"),
            (60, "1 h ago"),
            (1_440, "1 d ago"),
        ] {
            assert_eq!(age_words(Timestamp(0), minutes(age)), words);
        }
    }

    #[test]
    fn off_a_connection_it_shows_the_saved_affects_and_no_position() {
        let (mut sheet, mut characters, vitals) = sample_sheet();
        sheet.affects = vec![Affect {
            name: "bless".to_owned(),
            detail: String::new(),
        }];
        characters.0.insert(
            CHARACTER_POSITION.to_owned(),
            Value::Text("Standing".to_owned()),
        );
        let closed = SettingsState::default();
        let drawn = Sheet::of(&sheet, &characters, &vitals, &closed);
        assert_eq!(drawn.affects, sheet.affects);
        assert!(!drawn.about.contains("Standing"));
    }

    #[gpui::test]
    fn refresh_answers_the_mouse_and_the_keyboard(cx: &mut TestAppContext) {
        let (panel, cx) = panel_window(cx);
        let refreshes = Rc::new(Cell::new(0));
        let count = refreshes.clone();
        cx.update(|_, cx| {
            cx.subscribe(&panel, move |_, _: &RefreshCharacter, _| {
                count.set(count.get() + 1);
            })
            .detach();
        });
        panel.update(cx, |panel, cx| {
            let (sheet, characters, vitals) = sample_sheet();
            panel.show(&sheet, &characters, &vitals, &connected(), cx);
        });
        cx.run_until_parked();
        let bounds = cx
            .debug_bounds("Refresh the character")
            .expect("the button shows");
        cx.simulate_click(bounds.center(), gpui::Modifiers::none());
        assert_eq!(refreshes.get(), 1);
        cx.update(|window, cx| window.focus_next(cx));
        cx.run_until_parked();
        press_key("enter", cx);
        assert_eq!(refreshes.get(), 2, "the keyboard reaches it");
    }

    #[gpui::test]
    fn off_a_connection_refresh_does_nothing(cx: &mut TestAppContext) {
        let (panel, cx) = panel_window(cx);
        let refreshes = Rc::new(Cell::new(0));
        let count = refreshes.clone();
        cx.update(|_, cx| {
            cx.subscribe(&panel, move |_, _: &RefreshCharacter, _| {
                count.set(count.get() + 1);
            })
            .detach();
        });
        panel.update(cx, |panel, cx| {
            let (sheet, characters, vitals) = sample_sheet();
            panel.show(&sheet, &characters, &vitals, &SettingsState::default(), cx);
        });
        cx.run_until_parked();
        let bounds = cx.debug_bounds("Refresh the character").unwrap();
        cx.simulate_click(bounds.center(), gpui::Modifiers::none());
        assert_eq!(refreshes.get(), 0);
    }

    #[test]
    fn a_character_with_no_rows_shows_their_id_until_the_game_says_more() {
        let stats = BTreeMap::from([("name".to_owned(), Value::Text("Bren".to_owned()))]);
        let sheet = SheetState {
            character: "bren".to_owned(),
            stats,
            ..SheetState::default()
        };
        let drawn = Sheet::of(
            &sheet,
            &CharactersState::default(),
            &VitalsState::default(),
            &connected(),
        );
        assert_eq!(drawn.name, "Bren");
    }
}
