//! The Triggers & aliases section: a table of what a line matches, what to do
//! and who it applies to, and the same for aliases and the hotbar. One rule is
//! edited at a time, in a form below its table. Every change goes to the
//! session, which saves it and publishes the rules this section shows.

use super::{
    Pane, SettingsSheet, SheetEvent, chosen_ground, control, muted_line, section_heading,
    text_button,
};
use crate::assets::UI_FONT;
use crate::input::{CommandInput, InputEvent};
use crate::sound::{SOUNDS, label_of};
use crate::theme::Theme;
use gpui::{
    AnyElement, Context, Entity, FocusHandle, Focusable, Pixels, Role, SharedString, Window, div,
    prelude::*, px,
};
use mudling_core::defaults::aliases::AliasRule;
use mudling_core::defaults::commands::CHAIN_SEPARATOR;
use mudling_core::defaults::hotbar;
use mudling_core::defaults::triggers::{Highlight, Scope, TriggerRule};
use mudling_core::session::RuleEdit;
use mudling_core::store::RulesState;

const NO_SESSION: &str = "Connect to a server to set up its triggers and aliases.";
const NO_TRIGGERS: &str = "No triggers yet.";
const NO_ALIASES: &str = "No aliases yet.";
const NO_CHIPS: &str = "Nothing on the hotbar yet. Pin a command from the command bar.";
const WALK_NOTE: &str = "Type a walk like 3n2e. Repeated moves alternate their spelling, so a \
                         long walk isn't taken for spam.";
const WHAT_WIDTH: Pixels = px(150.);
const WHO_WIDTH: Pixels = px(120.);
const SWITCH_WIDTH: Pixels = px(46.);
const ROW_BUTTON_HEIGHT: Pixels = px(30.);
const MOVE_WIDTH: Pixels = px(58.);
const ALL_CHARACTERS: &str = "All characters";
const THIS_SERVER: &str = "This server";
const THIS_CHARACTER: &str = "This character";

/// What the form is changing.
#[derive(Clone, Debug, PartialEq)]
enum Draft {
    Trigger(TriggerRule),
    Alias(AliasRule),
    /// A chip of the hotbar, by its command: the table's buttons stay live
    /// under the form, so its place can change. `None` is a new one.
    Chip(Option<String>),
}

/// A choice made in the form.
#[derive(Clone)]
enum Choice {
    Scope(Scope),
    Color(Option<Highlight>),
    Sound(Option<&'static str>),
    Regex(bool),
    Hides(bool),
}

/// The form: its text fields live as long as the sheet, so typing survives
/// each frame.
pub(super) struct Form {
    draft: Option<Draft>,
    problem: Option<&'static str>,
    pattern: Entity<CommandInput>,
    name: Entity<CommandInput>,
    expansion: Entity<CommandInput>,
    chip: Entity<CommandInput>,
}

impl Form {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsSheet>) -> Form {
        let mut field = |label, placeholder| {
            let field = cx.new(|cx| CommandInput::field(label, placeholder, cx));
            cx.subscribe_in(&field, window, |sheet, _, event, window, cx| {
                if let InputEvent::Submitted(_) = event {
                    sheet.save_draft(window, cx);
                }
            })
            .detach();
            field
        };
        Form {
            draft: None,
            problem: None,
            pattern: field("What the line matches", "Text in the line"),
            name: field("Alias name", "A short word"),
            expansion: field("What it stands for", "kill $1;kick $1"),
            chip: field("Hotbar command", "A command"),
        }
    }
}

/// What a trigger does, in a few words.
fn what_it_does(rule: &TriggerRule) -> String {
    let color = rule.color.map(|color| color.name().to_owned());
    let hides = rule.hides.then(|| "Hide".to_owned());
    let sound = rule.sound.as_deref().map(|name| label_of(name).to_owned());
    let parts: Vec<String> = [color, hides, sound].into_iter().flatten().collect();
    if parts.is_empty() {
        "Nothing".to_owned()
    } else {
        parts.join(" · ")
    }
}

fn who_it_applies_to(scope: &Scope, character: &str) -> String {
    match scope {
        Scope::All => ALL_CHARACTERS.to_owned(),
        Scope::Server => THIS_SERVER.to_owned(),
        Scope::Character(id) if id == character => THIS_CHARACTER.to_owned(),
        Scope::Character(id) => id.clone(),
    }
}

/// How the game takes several commands, as this section says it.
fn chain_note(separator: Option<&str>) -> String {
    match separator {
        Some(separator) => format!(
            "Separate commands with {CHAIN_SEPARATOR}. MUDling sends them chained with {separator}."
        ),
        None => format!(
            "Separate commands with {CHAIN_SEPARATOR}. This game takes one command a line, so \
             MUDling sends them in turn."
        ),
    }
}

fn plain_cell(text: impl Into<SharedString>, width: Option<Pixels>, theme: &Theme) -> gpui::Div {
    let cell = div()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .text_color(theme.text_muted)
        .child(text.into());
    match width {
        Some(width) => cell.flex_none().w(width),
        None => cell.flex_1(),
    }
}

/// A short button in a table row.
fn row_button(
    id: String,
    label: &'static str,
    what: &str,
    stop: &FocusHandle,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    control(id, format!("{label} {what}"), stop, theme)
        .h(ROW_BUTTON_HEIGHT)
        .px_2()
        .text_size(px(13.))
        .text_color(theme.text_muted)
        .child(label)
}

/// Turns a rule on or off.
fn switch(
    id: String,
    what: &str,
    on: bool,
    stop: &FocusHandle,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    control(id, format!("{what}, on"), stop, theme)
        .role(Role::Switch)
        .aria_toggled(on.into())
        .w(SWITCH_WIDTH)
        .h(ROW_BUTTON_HEIGHT)
        .border_color(theme.border)
        .bg(chosen_ground(on, theme))
        .text_size(px(13.))
        .text_color(if on { theme.text } else { theme.text_muted })
        .child(if on { "On" } else { "Off" })
}

/// One row of a table, read by a screen reader as one list item.
fn table_row(id: String, label: String, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(id))
        .role(Role::ListItem)
        .aria_label(label)
        .flex()
        .items_center()
        .gap_2()
        .py_1()
        .border_b_1()
        .border_color(theme.divider)
        .font_family(UI_FONT)
        .text_size(px(14.))
        .text_color(theme.text)
}

impl SettingsSheet {
    /// What the session has now.
    pub fn show_rules(&mut self, rules: &RulesState, cx: &mut Context<SettingsSheet>) {
        if self.rules != *rules {
            self.rules = rules.clone();
            cx.notify();
        }
    }

    #[cfg(test)]
    pub fn rules(&self) -> &RulesState {
        &self.rules
    }

    fn edit(&mut self, edit: RuleEdit, cx: &mut Context<SettingsSheet>) {
        cx.emit(SheetEvent::Edit(edit));
    }

    fn set_chips(&mut self, chips: Vec<String>, cx: &mut Context<SettingsSheet>) {
        self.edit(RuleEdit::SetHotbar(chips), cx);
    }

    fn open_draft(&mut self, draft: Draft, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        let fill = |field: &Entity<CommandInput>,
                    text: String,
                    window: &mut Window,
                    cx: &mut Context<SettingsSheet>| {
            field.update(cx, |field, cx| {
                let end = text.len();
                field.show(text, end, window, cx);
            });
        };
        let form = &self.form;
        let (pattern, name, expansion, chip) = (
            form.pattern.clone(),
            form.name.clone(),
            form.expansion.clone(),
            form.chip.clone(),
        );
        let first = match &draft {
            Draft::Trigger(rule) => {
                fill(&pattern, rule.pattern.clone(), window, cx);
                pattern
            }
            Draft::Alias(rule) => {
                fill(&name, rule.name.clone(), window, cx);
                fill(&expansion, rule.expansion.clone(), window, cx);
                name
            }
            Draft::Chip(command) => {
                fill(&chip, command.clone().unwrap_or_default(), window, cx);
                chip
            }
        };
        self.form.draft = Some(draft);
        self.form.problem = None;
        let field = first.focus_handle(cx);
        window.focus(&field, cx);
        self.reveal = Some(field);
        cx.notify();
    }

    #[cfg(test)]
    pub(super) fn form_field_handle(&self, cx: &gpui::App) -> FocusHandle {
        self.form.pattern.focus_handle(cx)
    }

    /// Opens the form on the first trigger.
    #[cfg(all(test, target_os = "macos"))]
    pub fn edit_sample_trigger(&mut self, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        let draft = Draft::Trigger(self.rules.triggers[0].clone());
        self.open_draft(draft, window, cx);
    }

    fn close_draft(&mut self, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        self.form.draft = None;
        self.form.problem = None;
        window.focus(&self.tabs[self.section as usize], cx);
        cx.notify();
    }

    /// Saves the form, or says why it can't. The table's buttons work while
    /// the form is open: a rule switched there stays as it was switched, and
    /// one removed there stays removed.
    pub(super) fn save_draft(&mut self, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        let Some(draft) = self.form.draft.clone() else {
            return;
        };
        let (problem, edit) = match draft {
            Draft::Trigger(rule) => {
                let saved = self.rules.triggers.iter().find(|saved| saved.id == rule.id);
                if !rule.id.is_empty() && saved.is_none() {
                    return self.close_draft(window, cx);
                }
                let enabled = saved.map_or(rule.enabled, |saved| saved.enabled);
                let pattern = self.form.pattern.read(cx).content().to_owned();
                let rule = TriggerRule {
                    pattern,
                    enabled,
                    ..rule
                };
                (rule.problem(), RuleEdit::SaveTrigger(rule))
            }
            Draft::Alias(rule) => {
                let saved = self.rules.aliases.iter().find(|saved| saved.id == rule.id);
                if !rule.id.is_empty() && saved.is_none() {
                    return self.close_draft(window, cx);
                }
                let enabled = saved.map_or(rule.enabled, |saved| saved.enabled);
                let name = self.form.name.read(cx).content().trim().to_owned();
                let expansion = self.form.expansion.read(cx).content().to_owned();
                let rule = AliasRule {
                    name,
                    expansion,
                    enabled,
                    ..rule
                };
                (rule.problem(), RuleEdit::SaveAlias(rule))
            }
            Draft::Chip(command) => {
                let typed = self.form.chip.read(cx).content().trim().to_owned();
                let mut chips = self.rules.hotbar.clone();
                match command {
                    Some(command) => {
                        let Some(chip) = chips.iter_mut().find(|chip| **chip == command) else {
                            return self.close_draft(window, cx);
                        };
                        typed.clone_into(chip);
                    }
                    None => {
                        hotbar::pin(&mut chips, &typed);
                    }
                }
                let problem = typed.is_empty().then_some("Type the command.");
                (problem, RuleEdit::SetHotbar(chips))
            }
        };
        match problem {
            Some(problem) => {
                self.form.problem = Some(problem);
                cx.notify();
            }
            None => {
                self.edit(edit, cx);
                self.close_draft(window, cx);
            }
        }
    }

    fn choose_in_draft(&mut self, choice: Choice, cx: &mut Context<SettingsSheet>) {
        let Some(draft) = &mut self.form.draft else {
            return;
        };
        match (draft, choice) {
            (Draft::Trigger(rule), Choice::Scope(scope)) => rule.scope = scope,
            (Draft::Alias(rule), Choice::Scope(scope)) => rule.scope = scope,
            (Draft::Trigger(rule), Choice::Color(color)) => rule.color = color,
            (Draft::Trigger(rule), Choice::Regex(regex)) => rule.regex = regex,
            (Draft::Trigger(rule), Choice::Hides(hides)) => rule.hides = hides,
            (Draft::Trigger(rule), Choice::Sound(sound)) => {
                rule.sound = sound.map(str::to_owned);
                if let Some(sound) = sound {
                    cx.emit(SheetEvent::TrySound(sound));
                }
            }
            _ => {}
        }
        cx.notify();
    }

    /// A row of options, one chosen.
    fn fill_choices(
        &self,
        pane: &mut Pane,
        label: &'static str,
        options: Vec<(SharedString, bool, Choice)>,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) {
        let buttons: Vec<AnyElement> = options
            .into_iter()
            .map(|(name, chosen, choice)| {
                control(
                    format!("{label}-{name}"),
                    name.clone(),
                    &pane.stop(cx),
                    theme,
                )
                .role(Role::RadioButton)
                .aria_toggled(chosen.into())
                .h(ROW_BUTTON_HEIGHT)
                .px_2()
                .bg(chosen_ground(chosen, theme))
                .text_size(px(13.))
                .text_color(theme.text)
                .child(name)
                .on_click(cx.listener(move |sheet, _, _, cx| {
                    sheet.choose_in_draft(choice.clone(), cx);
                }))
                .into_any_element()
            })
            .collect();
        pane.row(
            div()
                .id(label)
                .role(Role::RadioGroup)
                .aria_label(label)
                .pb_2()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .w(px(84.))
                        .text_size(px(13.))
                        .text_color(theme.text_muted)
                        .child(label),
                )
                .children(buttons),
        );
    }

    fn fill_scope(
        &self,
        pane: &mut Pane,
        scope: &Scope,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) {
        // Who is playing is what the game says, so until it has said, there is
        // nobody to make a rule for. The rules already for one character wait
        // for the name too.
        let named = (!self.rules.character.is_empty()).then(|| {
            (
                THIS_CHARACTER,
                Scope::Character(self.rules.character.clone()),
            )
        });
        let options: Vec<_> = [(ALL_CHARACTERS, Scope::All), (THIS_SERVER, Scope::Server)]
            .into_iter()
            .chain(named)
            .map(|(name, option)| (name.into(), *scope == option, Choice::Scope(option)))
            .collect();
        self.fill_choices(pane, "Applies to", options, theme, cx);
    }

    fn fill_form_buttons(
        &self,
        pane: &mut Pane,
        save: &'static str,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) {
        if let Some(problem) = self.form.problem {
            pane.row(
                muted_line(problem, theme)
                    .role(Role::Alert)
                    .text_color(theme.warning),
            );
        }
        let (save_stop, cancel_stop) = (pane.stop(cx), pane.stop(cx));
        pane.row(
            div()
                .pb_4()
                .flex()
                .gap_2()
                .child(
                    text_button("form-save", save, &save_stop, theme)
                        .on_click(cx.listener(|sheet, _, window, cx| sheet.save_draft(window, cx))),
                )
                .child(
                    text_button("form-cancel", "Cancel", &cancel_stop, theme).on_click(
                        cx.listener(|sheet, _, window, cx| sheet.close_draft(window, cx)),
                    ),
                ),
        );
    }

    fn fill_field(
        &self,
        pane: &mut Pane,
        field: &Entity<CommandInput>,
        cx: &mut Context<SettingsSheet>,
    ) {
        pane.adopt(field.focus_handle(cx));
        pane.row(div().pb_2().child(field.clone()));
    }

    fn fill_trigger_form(
        &self,
        pane: &mut Pane,
        rule: &TriggerRule,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) {
        self.fill_field(pane, &self.form.pattern, cx);
        let matching = [("Text", false), ("Regular expression", true)]
            .map(|(name, regex)| (name.into(), rule.regex == regex, Choice::Regex(regex)));
        self.fill_choices(pane, "Matches", matching.into(), theme, cx);
        let colors = std::iter::once(("None".into(), rule.color.is_none(), Choice::Color(None)))
            .chain(Highlight::ALL.map(|color| {
                let choice = Choice::Color(Some(color));
                (color.name().into(), rule.color == Some(color), choice)
            }))
            .collect();
        self.fill_choices(pane, "Highlight", colors, theme, cx);
        let shown = [("Show the line", false), ("Hide the line", true)]
            .map(|(name, hides)| (name.into(), rule.hides == hides, Choice::Hides(hides)));
        self.fill_choices(pane, "In the chat", shown.into(), theme, cx);
        let sounds = std::iter::once(("None".into(), rule.sound.is_none(), Choice::Sound(None)))
            .chain(SOUNDS.iter().map(|sound| {
                let chosen = rule.sound.as_deref() == Some(sound.name);
                (sound.label.into(), chosen, Choice::Sound(Some(sound.name)))
            }))
            .collect();
        self.fill_choices(pane, "Sound", sounds, theme, cx);
        self.fill_scope(pane, &rule.scope, theme, cx);
        self.fill_form_buttons(pane, "Save trigger", theme, cx);
    }

    fn fill_triggers(&self, pane: &mut Pane, theme: &Theme, cx: &mut Context<SettingsSheet>) {
        pane.row(section_heading("Triggers", theme));
        if self.rules.triggers.is_empty() {
            pane.row(muted_line(NO_TRIGGERS, theme));
        }
        for (at, rule) in self.rules.triggers.iter().enumerate() {
            let what = &rule.pattern;
            let does = what_it_does(rule);
            let who = who_it_applies_to(&rule.scope, &self.rules.character);
            let flipped = TriggerRule {
                enabled: !rule.enabled,
                ..rule.clone()
            };
            let (on, edit, remove) = (pane.stop(cx), pane.stop(cx), pane.stop(cx));
            let open = Draft::Trigger(rule.clone());
            let id = rule.id.clone();
            pane.row(
                table_row(
                    format!("trigger-{at}"),
                    format!("{what}: {does}, {who}"),
                    theme,
                )
                .child(
                    switch(format!("trigger-on-{at}"), what, rule.enabled, &on, theme).on_click(
                        cx.listener(move |sheet, _, _, cx| {
                            sheet.edit(RuleEdit::SaveTrigger(flipped.clone()), cx);
                        }),
                    ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(what.clone()),
                )
                .child(plain_cell(does, Some(WHAT_WIDTH), theme))
                .child(plain_cell(who, Some(WHO_WIDTH), theme))
                .child(
                    row_button(format!("trigger-edit-{at}"), "Edit", what, &edit, theme).on_click(
                        cx.listener(move |sheet, _, window, cx| {
                            sheet.open_draft(open.clone(), window, cx);
                        }),
                    ),
                )
                .child(
                    row_button(
                        format!("trigger-remove-{at}"),
                        "Remove",
                        what,
                        &remove,
                        theme,
                    )
                    .on_click(cx.listener(move |sheet, _, _, cx| {
                        sheet.edit(RuleEdit::RemoveTrigger(id.clone()), cx);
                    })),
                ),
            );
        }
        if let Some(Draft::Trigger(rule)) = &self.form.draft {
            self.fill_trigger_form(pane, rule, theme, cx);
        } else {
            let stop = pane.stop(cx);
            pane.row(div().flex().py_2().child(
                text_button("new-trigger", "New trigger", &stop, theme).on_click(cx.listener(
                    |sheet, _, window, cx| {
                        let draft = Draft::Trigger(TriggerRule::new(""));
                        sheet.open_draft(draft, window, cx);
                    },
                )),
            ));
        }
    }

    fn fill_aliases(&self, pane: &mut Pane, theme: &Theme, cx: &mut Context<SettingsSheet>) {
        pane.row(section_heading("Aliases", theme));
        if self.rules.aliases.is_empty() {
            pane.row(muted_line(NO_ALIASES, theme));
        }
        for (at, rule) in self.rules.aliases.iter().enumerate() {
            let name = &rule.name;
            let who = who_it_applies_to(&rule.scope, &self.rules.character);
            let flipped = AliasRule {
                enabled: !rule.enabled,
                ..rule.clone()
            };
            let (on, edit, remove) = (pane.stop(cx), pane.stop(cx), pane.stop(cx));
            let open = Draft::Alias(rule.clone());
            let id = rule.id.clone();
            pane.row(
                table_row(
                    format!("alias-{at}"),
                    format!("{name}: {}, {who}", rule.expansion),
                    theme,
                )
                .child(
                    switch(format!("alias-on-{at}"), name, rule.enabled, &on, theme).on_click(
                        cx.listener(move |sheet, _, _, cx| {
                            sheet.edit(RuleEdit::SaveAlias(flipped.clone()), cx);
                        }),
                    ),
                )
                .child(
                    div()
                        .w(px(90.))
                        .flex_none()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(name.clone()),
                )
                .child(plain_cell(rule.expansion.clone(), None, theme))
                .child(plain_cell(who, Some(WHO_WIDTH), theme))
                .child(
                    row_button(format!("alias-edit-{at}"), "Edit", name, &edit, theme).on_click(
                        cx.listener(move |sheet, _, window, cx| {
                            sheet.open_draft(open.clone(), window, cx);
                        }),
                    ),
                )
                .child(
                    row_button(format!("alias-remove-{at}"), "Remove", name, &remove, theme)
                        .on_click(cx.listener(move |sheet, _, _, cx| {
                            sheet.edit(RuleEdit::RemoveAlias(id.clone()), cx);
                        })),
                ),
            );
        }
        if let Some(Draft::Alias(rule)) = &self.form.draft {
            self.fill_field(pane, &self.form.name, cx);
            self.fill_field(pane, &self.form.expansion, cx);
            self.fill_scope(pane, &rule.scope, theme, cx);
            self.fill_form_buttons(pane, "Save alias", theme, cx);
        } else {
            let stop = pane.stop(cx);
            pane.row(div().flex().py_2().child(
                text_button("new-alias", "New alias", &stop, theme).on_click(cx.listener(
                    |sheet, _, window, cx| {
                        let draft = Draft::Alias(AliasRule::new("", ""));
                        sheet.open_draft(draft, window, cx);
                    },
                )),
            ));
        }
    }

    fn fill_hotbar(&self, pane: &mut Pane, theme: &Theme, cx: &mut Context<SettingsSheet>) {
        pane.row(section_heading("Hotbar", theme));
        if self.rules.hotbar.is_empty() {
            pane.row(muted_line(NO_CHIPS, theme));
        }
        let chips = &self.rules.hotbar;
        for (at, command) in chips.iter().enumerate() {
            let mut row = table_row(format!("chip-{at}"), command.clone(), theme)
                .child(plain_cell(command.clone(), None, theme).text_color(theme.text));
            let swapped = |other: usize| {
                let mut moved = chips.clone();
                moved.swap(at, other);
                moved
            };
            let neighbors = [
                (at.checked_sub(1), "Earlier"),
                (Some(at + 1).filter(|&next| next < chips.len()), "Later"),
            ];
            for (other, label) in neighbors {
                // The end of the list has no move that way, so no stop.
                let Some(moved) = other.map(swapped) else {
                    row = row.child(div().flex_none().w(MOVE_WIDTH));
                    continue;
                };
                let stop = pane.stop(cx);
                row = row.child(
                    row_button(format!("chip-{label}-{at}"), label, command, &stop, theme)
                        .w(MOVE_WIDTH)
                        .on_click(cx.listener(move |sheet, _, _, cx| {
                            sheet.set_chips(moved.clone(), cx);
                        })),
                );
            }
            let (edit, remove) = (pane.stop(cx), pane.stop(cx));
            let edited = command.clone();
            let mut without = chips.clone();
            without.remove(at);
            pane.row(
                row.child(
                    row_button(format!("chip-edit-{at}"), "Edit", command, &edit, theme).on_click(
                        cx.listener(move |sheet, _, window, cx| {
                            sheet.open_draft(Draft::Chip(Some(edited.clone())), window, cx);
                        }),
                    ),
                )
                .child(
                    row_button(
                        format!("chip-remove-{at}"),
                        "Remove",
                        command,
                        &remove,
                        theme,
                    )
                    .on_click(cx.listener(move |sheet, _, _, cx| {
                        sheet.set_chips(without.clone(), cx);
                    })),
                ),
            );
        }
        if let Some(Draft::Chip(_)) = &self.form.draft {
            self.fill_field(pane, &self.form.chip, cx);
            self.fill_form_buttons(pane, "Save chip", theme, cx);
        } else {
            let stop = pane.stop(cx);
            pane.row(div().flex().py_2().child(
                text_button("new-chip", "New chip", &stop, theme).on_click(cx.listener(
                    |sheet, _, window, cx| sheet.open_draft(Draft::Chip(None), window, cx),
                )),
            ));
        }
    }

    pub(super) fn fill_triggers_section(
        &self,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) {
        if self.rules.server.is_empty() {
            pane.row(muted_line(NO_SESSION, theme));
            return;
        }
        self.fill_triggers(pane, theme, cx);
        self.fill_aliases(pane, theme, cx);
        self.fill_hotbar(pane, theme, cx);
        pane.row(section_heading("Commands", theme));
        pane.row(muted_line(
            chain_note(self.rules.separator.as_deref()),
            theme,
        ));
        pane.row(muted_line(WALK_NOTE, theme));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trigger_says_what_it_does_in_a_few_words() {
        let rule = TriggerRule {
            color: Some(Highlight::Teal),
            hides: true,
            sound: Some("chime".to_owned()),
            ..TriggerRule::new("bat")
        };
        assert_eq!(what_it_does(&rule), "Teal · Hide · Chime");
        assert_eq!(what_it_does(&TriggerRule::new("bat")), "Nothing");
    }

    #[test]
    fn the_note_names_how_this_game_takes_commands() {
        assert!(chain_note(None).contains("one command a line"));
        assert!(chain_note(Some("=")).contains("chained with ="));
    }
}
