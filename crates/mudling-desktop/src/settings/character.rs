//! Add character and Edit: one dialog, alone over the window as the restart
//! question is, opened from Where to today? or from Servers & characters.
//!
//! A character's server is a pull-down of the saved servers, with New server…
//! last for an address. Its password goes in the vault only if the box says
//! so: ticked as soon as a password is typed, unless the player chose. With no
//! vault, or a locked one, the section's create or unlock step comes next and
//! the password is saved once the vault opens.

use super::servers::{CharacterOn, PasswordState};
use super::{Pane, Section, SettingsSheet, SheetEvent, control, muted_line, text_button};
use crate::assets::{Icon, UI_FONT};
use crate::empty_state::parse_address;
use crate::input::{CommandInput, InputEvent};
use crate::launcher::Target;
use crate::theme::Theme;
use gpui::{
    Context, Div, Entity, FocusHandle, Focusable, FontWeight, Role, SharedString, Stateful, Window,
    div, prelude::*, px,
};
use mudling_core::persist;
use mudling_core::vault::VaultState;

pub(super) const ADD_TITLE: &str = "Add character";
const NEW_SERVER: &str = "New server…";
const SAVE_IN_VAULT: &str = "Save the password in MUDling's vault";
const VAULT_HELP: &str =
    "Encrypted with your master password. It syncs only if you've turned on Data & sync.";
const NO_VAULT_HELP: &str = "No vault yet: ticking this sets one up next, with a master password.";
const NEEDS_NAME: &str = "Type the character's name.";
const NEEDS_PASSWORD: &str = "Type the password to keep in the vault, or untick the box.";

/// The dialog's fields, kept for the sheet's life so each keeps its focus.
pub(super) struct Fields {
    pub(super) name: Entity<CommandInput>,
    address: Entity<CommandInput>,
    pub(super) password: Entity<CommandInput>,
    server_choice: FocusHandle,
}

impl Fields {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsSheet>) -> Fields {
        let mut watched = |field: fn(&mut Context<CommandInput>) -> CommandInput| {
            let field = cx.new(field);
            cx.subscribe_in(&field, window, |sheet, _, event, window, cx| {
                if let InputEvent::Submitted(_) = event {
                    sheet.submit_character(window, cx);
                }
            })
            .detach();
            // The box follows what's typed, so the dialog draws again.
            cx.observe(&field, |_, _, cx| cx.notify()).detach();
            field
        };
        Fields {
            name: watched(|cx| CommandInput::field("Name", "Name", cx)),
            address: watched(|cx| CommandInput::field("Address", "host:port", cx)),
            password: watched(|cx| CommandInput::secret_field("Password", "Optional", cx)),
            server_choice: cx.focus_handle(),
        }
    }
}

/// The dialog while it's open.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct CharacterForm {
    /// The character being edited, whose server is fixed. `None` adds one.
    editing: Option<CharacterOn>,
    /// The saved server chosen, or `None` for a new one.
    server: Option<SharedString>,
    /// The server pull-down is open.
    choosing: bool,
    /// What the player chose for the box. `None` follows the password: ticked
    /// once one is typed.
    in_vault: Option<bool>,
    problem: Option<SharedString>,
    /// A character already on the chosen server, with the name typed.
    taken: Option<CharacterOn>,
    /// Opened from Where to today?, so closing it closes the sheet.
    alone: bool,
}

impl CharacterForm {
    fn ticked(&self, password_typed: bool) -> bool {
        self.in_vault.unwrap_or(password_typed)
    }
}

impl SettingsSheet {
    /// Opens Add character, or Edit for `editing`. `alone` is for Where to
    /// today?: the sheet behind it isn't shown, and closing the dialog closes
    /// it.
    pub fn open_character_form(
        &mut self,
        editing: Option<CharacterOn>,
        alone: bool,
        window: &mut Window,
        cx: &mut Context<SettingsSheet>,
    ) {
        self.forget_character_typed(window, cx);
        let server = match &editing {
            Some(editing) => Some(editing.host.clone()),
            None => (self.playing.clone())
                .filter(|host| self.servers.iter().any(|server| server.host == *host))
                .or_else(|| self.servers.first().map(|server| server.host.clone())),
        };
        let saved = editing.as_ref().is_some_and(|editing| {
            self.passwords().of(&editing.host, &editing.character.name) == PasswordState::Saved
        });
        let first = match &editing {
            Some(_) => self.character_fields.password.focus_handle(cx),
            None => self.character_fields.name.focus_handle(cx),
        };
        self.character = Some(CharacterForm {
            editing,
            server,
            choosing: false,
            in_vault: saved.then_some(true),
            problem: None,
            taken: None,
            alone,
        });
        window.focus(&first, cx);
        cx.notify();
    }

    /// Empties the dialog's fields, the password wiped.
    fn forget_character_typed(&mut self, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        let fields = &self.character_fields;
        for field in [&fields.name, &fields.address] {
            field.update(cx, |field, cx| field.show(String::new(), 0, window, cx));
        }
        fields
            .password
            .update(cx, |field, cx| drop(field.take_secret(cx)));
    }

    /// Whether the vault box shows ticked, while the dialog is open.
    #[cfg(test)]
    pub(super) fn vault_box_ticked(&self, cx: &gpui::App) -> Option<bool> {
        let typed = !self.character_fields.password.read(cx).content().is_empty();
        Some(self.character.as_ref()?.ticked(typed))
    }

    /// Drops a dialog left open when the sheet last closed, its password
    /// wiped.
    pub(super) fn close_character_form_quietly(
        &mut self,
        window: &mut Window,
        cx: &mut Context<SettingsSheet>,
    ) {
        if self.character.take().is_some() {
            self.forget_character_typed(window, cx);
        }
    }

    /// Cancel, Esc or a click beside it.
    pub fn close_character_form(&mut self, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        let Some(form) = self.character.take() else {
            return;
        };
        self.forget_character_typed(window, cx);
        if form.alone {
            cx.emit(SheetEvent::Closed);
        } else {
            self.focus_tab(Section::Servers, window, cx);
        }
        cx.notify();
    }

    /// Add or Save: saves the character, and what the box says of its password.
    pub(super) fn submit_character(
        &mut self,
        window: &mut Window,
        cx: &mut Context<SettingsSheet>,
    ) {
        let Some(form) = self.character.clone() else {
            return;
        };
        let fields = &self.character_fields;
        let typed = |field: &Entity<CommandInput>| field.read(cx).content().trim().to_owned();
        let password_typed = !fields.password.read(cx).content().is_empty();
        let ticked = form.ticked(password_typed);
        let (target, adding) = match &form.editing {
            Some(editing) => {
                let port = (self.servers.iter())
                    .find(|server| server.host == editing.host)
                    .map_or(0, |server| server.port);
                let target = Target::server(editing.host.to_string(), port)
                    .with_character(editing.character.name.to_string());
                (target, false)
            }
            None => {
                let name = typed(&fields.name);
                let server = match &form.server {
                    Some(host) => (self.servers.iter())
                        .find(|server| server.host == *host)
                        .map(|server| Target::server(server.host.to_string(), server.port))
                        .ok_or(NEW_SERVER),
                    None => parse_address(&typed(&fields.address)),
                };
                let problem = match (&server, name.is_empty()) {
                    (_, true) => Some(NEEDS_NAME),
                    (Err(problem), _) => Some(*problem),
                    (Ok(_), false) if ticked && !password_typed => Some(NEEDS_PASSWORD),
                    _ => None,
                };
                if let Some(problem) = problem {
                    return self.show_character_problem(problem.into(), None, cx);
                }
                let target = server.expect("checked above").with_character(name);
                if let Some(taken) = self.character_on(&target) {
                    let said = format!("{} is already on {}.", taken.character.name, taken.host);
                    return self.show_character_problem(said.into(), Some(taken), cx);
                }
                (target, true)
            }
        };
        let password = fields
            .password
            .update(cx, |field, cx| field.take_secret(cx));
        let host = target.host.clone();
        let name = target.character.clone().unwrap_or_default();
        if adding {
            let server = Target::server(target.host, target.port);
            let name = name.clone();
            cx.emit(SheetEvent::AddCharacter { server, name });
        }
        let leaves = form.alone;
        self.character = None;
        self.forget_character_typed(window, cx);
        let vault_change = match (ticked, password_typed) {
            (true, true) => Some(Some(password)),
            // Unticking a saved password takes it out.
            (false, _) if !adding && form.in_vault == Some(false) => Some(None),
            _ => None,
        };
        let waits = match vault_change {
            Some(password) => self.keep_password(&host, &name, password, window, cx),
            None => false,
        };
        // A vault that must open first has the section's step to show.
        if leaves && !waits {
            cx.emit(SheetEvent::Closed);
        } else if !waits {
            self.focus_tab(Section::Servers, window, cx);
        }
        cx.notify();
    }

    fn show_character_problem(
        &mut self,
        problem: SharedString,
        taken: Option<CharacterOn>,
        cx: &mut Context<SettingsSheet>,
    ) {
        if let Some(form) = &mut self.character {
            form.problem = Some(problem);
            form.taken = taken;
        }
        cx.notify();
    }

    /// The saved character `target` names, if its server has it.
    fn character_on(&self, target: &Target) -> Option<CharacterOn> {
        let name = target.character.as_deref()?;
        let server = self
            .servers
            .iter()
            .find(|server| server.host == target.host)?;
        let id = persist::character_id(name);
        let character = server.characters.iter().find(|c| c.id == id)?;
        Some(CharacterOn {
            host: server.host.clone(),
            character: character.clone(),
        })
    }

    fn choose_server(
        &mut self,
        server: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<SettingsSheet>,
    ) {
        if let Some(form) = &mut self.character {
            form.server = server.clone();
            form.choosing = false;
            form.taken = None;
            form.problem = None;
        }
        let next = match server {
            Some(_) => self.character_fields.server_choice.clone(),
            None => self.character_fields.address.focus_handle(cx),
        };
        window.focus(&next, cx);
        cx.notify();
    }

    /// The dialog, alone over the window: the sheet steps aside for it.
    pub(super) fn render_character_dialog(
        &mut self,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) -> Option<Stateful<Div>> {
        let form = self.character.clone()?;
        let mut stops = std::mem::take(&mut self.stops);
        stops.start_frame();
        let mut pane = Pane {
            rows: Vec::new(),
            stops,
            leading: None,
            trailing: None,
        };
        let fields = &self.character_fields;
        let (name_field, address, password) = (
            fields.name.clone(),
            fields.address.clone(),
            fields.password.clone(),
        );
        let title: SharedString = match &form.editing {
            Some(editing) => format!("Edit {}", editing.character.name).into(),
            None => ADD_TITLE.into(),
        };
        let mut body = div().flex().flex_col();
        body = body.child(label("Name", theme));
        body = match &form.editing {
            Some(editing) => body.child(fixed(
                "character-name",
                editing.character.name.clone(),
                theme,
            )),
            None => {
                pane.adopt(name_field.focus_handle(cx));
                body.child(name_field)
            }
        };
        body = body.child(label("Server", theme));
        body = match &form.editing {
            Some(editing) => {
                let port = (self.servers.iter())
                    .find(|server| server.host == editing.host)
                    .map(|server| server.port);
                let shown = match port {
                    Some(port) => server_words(&editing.host, port),
                    None => editing.host.clone(),
                };
                body.child(fixed("character-server", shown, theme))
            }
            None => self.server_choice(&form, body, &mut pane, theme, cx),
        };
        if form.editing.is_none() && form.server.is_none() {
            pane.adopt(address.focus_handle(cx));
            body = body.child(label("Address", theme)).child(address);
        }
        pane.adopt(password.focus_handle(cx));
        body = body.child(label("Password", theme)).child(password.clone());
        let ticked = form.ticked(!password.read(cx).content().is_empty());
        body = body.child(self.vault_box(ticked, &mut pane, theme, cx));
        if let Some(problem) = form.problem.clone() {
            body = body.child(
                muted_line(problem, theme)
                    .debug_selector(|| "character-problem".to_owned())
                    .role(Role::Alert)
                    .pt_3()
                    .pb_0()
                    .text_color(theme.error),
            );
        }
        if let Some(taken) = form.taken.clone() {
            let words = format!("Edit {}", taken.character.name);
            let alone = form.alone;
            body = body.child(div().pt_2().flex().child(
                text_button("edit-taken-character", words, &pane.stop(cx), theme).on_click(
                    cx.listener(move |sheet, _, window, cx| {
                        sheet.open_character_form(Some(taken.clone()), alone, window, cx);
                    }),
                ),
            ));
        }
        let cancel = text_button("character-cancel", "Cancel", &pane.stop(cx), theme)
            .on_click(cx.listener(|sheet, _, window, cx| sheet.close_character_form(window, cx)));
        let done = if form.editing.is_some() {
            "Save"
        } else {
            "Add"
        };
        let submit = text_button("character-submit", done, &pane.stop(cx), theme)
            .border_color(theme.accent)
            .text_color(theme.accent)
            .on_click(cx.listener(|sheet, _, window, cx| sheet.submit_character(window, cx)));
        self.stops = pane.stops;
        Some(
            div()
                .id("character-dialog")
                .debug_selector(|| "character-dialog".to_owned())
                .w(px(420.))
                .p_5()
                .rounded_xl()
                .border_1()
                .border_color(theme.border)
                .bg(theme.surface)
                .shadow(theme.floating_shadow())
                .font_family(UI_FONT)
                .child(
                    div()
                        .id("character-title")
                        .role(Role::Heading)
                        .aria_label(title.clone())
                        .aria_level(2)
                        .pb_1()
                        .text_size(px(16.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child(title),
                )
                .child(body)
                .child(
                    div()
                        .pt_4()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(cancel)
                        .child(submit),
                ),
        )
    }

    /// The pull-down of saved servers, with New server… last.
    fn server_choice(
        &self,
        form: &CharacterForm,
        body: Div,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) -> Div {
        let chosen = |host: &SharedString| self.servers.iter().find(|server| server.host == *host);
        let shown = match form.server.as_ref().and_then(chosen) {
            Some(server) => server_words(&server.host, server.port),
            None => NEW_SERVER.into(),
        };
        let choice = &self.character_fields.server_choice;
        pane.adopt(choice.clone());
        let button = control("character-server", "Server", choice, theme)
            .role(Role::ComboBox)
            .aria_value(shown.clone())
            .aria_expanded(form.choosing)
            .h(px(36.))
            .px_3()
            .justify_between()
            .border_color(theme.border)
            .bg(theme.background)
            .text_size(px(14.))
            .text_color(theme.text)
            .child(shown)
            .child(
                Icon::ChevronDown
                    .svg()
                    .size(px(14.))
                    .text_color(theme.text_muted),
            )
            .on_click(cx.listener(|sheet, _, _, cx| {
                if let Some(form) = &mut sheet.character {
                    form.choosing = !form.choosing;
                }
                cx.notify();
            }));
        let body = body.child(button);
        if !form.choosing {
            return body;
        }
        let options = (self.servers.iter().map(|server| Some(server.host.clone())))
            .chain([None])
            .enumerate()
            .map(|(number, host)| {
                let words = match host.as_ref().and_then(chosen) {
                    Some(server) => server_words(&server.host, server.port),
                    None => NEW_SERVER.into(),
                };
                let selected = host == form.server;
                let id = SharedString::from(format!("character-server-{number}"));
                control(id, words.clone(), &pane.stop(cx), theme)
                    .role(Role::MenuItem)
                    .aria_selected(selected)
                    .w_full()
                    .h(px(32.))
                    .px_3()
                    .justify_start()
                    .when(selected, |option| option.bg(theme.selected))
                    .text_size(px(14.))
                    .text_color(if host.is_none() {
                        theme.accent
                    } else {
                        theme.text
                    })
                    .child(words)
                    .on_click(cx.listener(move |sheet, _, window, cx| {
                        sheet.choose_server(host.clone(), window, cx);
                    }))
            });
        body.child(
            div()
                .id("character-servers")
                .role(Role::Menu)
                .aria_label("Servers")
                .mt_1()
                .p_1()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.surface_raised)
                .children(options),
        )
    }

    fn vault_box(
        &self,
        ticked: bool,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) -> Div {
        let help = match self.vault_state() {
            Some(VaultState::NotCreated) | None => NO_VAULT_HELP,
            Some(_) => VAULT_HELP,
        };
        let checkbox = control("character-in-vault", SAVE_IN_VAULT, &pane.stop(cx), theme)
            .role(Role::CheckBox)
            .aria_toggled(super::appearance::toggled(ticked))
            .aria_description(help)
            .h(px(32.))
            .px_1()
            .gap_2()
            .justify_start()
            .text_size(px(14.))
            .text_color(theme.text)
            .child(
                div()
                    .size(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .border_1()
                    .border_color(if ticked { theme.accent } else { theme.border })
                    .when(ticked, |box_| {
                        box_.child(Icon::Check.svg().size(px(12.)).text_color(theme.accent))
                    }),
            )
            .child(SAVE_IN_VAULT)
            .on_click(cx.listener(move |sheet, _, _, cx| {
                if let Some(form) = &mut sheet.character {
                    form.in_vault = Some(!ticked);
                }
                cx.notify();
            }));
        div().pt_3().child(checkbox).child(
            div()
                .pl(px(28.))
                .text_size(px(13.))
                .text_color(theme.text_muted)
                .child(help),
        )
    }

    /// Opens Add character on a new server with `name` and `password` typed,
    /// for the accessibility check.
    #[cfg(target_os = "macos")]
    pub fn type_sample_character(
        &mut self,
        name: &str,
        password: &str,
        new_server: bool,
        window: &mut Window,
        cx: &mut Context<SettingsSheet>,
    ) {
        self.open_character_form(None, true, window, cx);
        if new_server {
            self.choose_server(None, window, cx);
        } else if let Some(form) = &mut self.character {
            form.choosing = true;
        }
        let fields = &self.character_fields;
        for (field, text) in [(&fields.name, name), (&fields.password, password)] {
            let text = text.to_owned();
            let end = text.len();
            field.update(cx, |field, cx| field.show(text, end, window, cx));
        }
    }
}

pub(super) const SWITCH_TITLE: &str = "Switch character";

/// Whether to log out of the game in play and connect `target` instead.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Switch {
    question: SharedString,
    target: Target,
}

impl SettingsSheet {
    /// What the open dialog is called, for its frame.
    pub(super) fn character_title(&self) -> SharedString {
        match self
            .character
            .as_ref()
            .and_then(|form| form.editing.as_ref())
        {
            Some(editing) => format!("Edit {}", editing.character.name).into(),
            None => ADD_TITLE.into(),
        }
    }

    /// Asks "Log out `playing` and connect `target`?", over everything else.
    pub fn ask_switch(
        &mut self,
        playing: &str,
        target: Target,
        window: &mut Window,
        cx: &mut Context<SettingsSheet>,
    ) {
        let next = target.character.as_deref().unwrap_or(&target.host);
        self.switching = Some(Switch {
            question: format!("Log out {playing} and connect {next}?").into(),
            target,
        });
        // Cancel takes the pane's first stop when the question draws.
        if self.stops.handles.is_empty() {
            self.stops.handles.push(cx.focus_handle());
        }
        window.focus(&self.stops.handles[0], cx);
        cx.notify();
    }

    pub fn answer_switch(
        &mut self,
        switch: bool,
        window: &mut Window,
        cx: &mut Context<SettingsSheet>,
    ) {
        let Some(asked) = self.switching.take() else {
            return;
        };
        window.focus(&self.tabs[self.section as usize], cx);
        match switch {
            true => cx.emit(SheetEvent::Switch(asked.target)),
            false => cx.emit(SheetEvent::Closed),
        }
        cx.notify();
    }

    #[cfg(test)]
    pub fn switch_question(&self) -> Option<&str> {
        self.switching.as_ref().map(|asked| asked.question.as_ref())
    }

    pub(super) fn render_switch_question(
        &mut self,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) -> Option<Stateful<Div>> {
        let question = self.switching.as_ref()?.question.clone();
        let mut stops = std::mem::take(&mut self.stops);
        stops.start_frame();
        let mut pane = Pane {
            rows: Vec::new(),
            stops,
            leading: None,
            trailing: None,
        };
        let cancel = text_button("switch-cancel", "Cancel", &pane.stop(cx), theme)
            .on_click(cx.listener(|sheet, _, window, cx| sheet.answer_switch(false, window, cx)));
        let switch = text_button("switch-now", "Log out and connect", &pane.stop(cx), theme)
            .border_color(theme.accent)
            .text_color(theme.accent)
            .on_click(cx.listener(|sheet, _, window, cx| sheet.answer_switch(true, window, cx)));
        self.stops = pane.stops;
        Some(
            div()
                .id("switch-question")
                .debug_selector(|| "switch-question".to_owned())
                .w(px(420.))
                .p_5()
                .rounded_xl()
                .border_1()
                .border_color(theme.border)
                .bg(theme.surface)
                .shadow(theme.floating_shadow())
                .child(
                    div()
                        .id("switch-title")
                        .role(Role::Heading)
                        .aria_label(SWITCH_TITLE)
                        .aria_level(2)
                        .pb_2()
                        .text_size(px(16.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child(SWITCH_TITLE),
                )
                .child(muted_line(question, theme).role(Role::Alert).pb_0())
                .child(
                    div()
                        .pt_4()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(cancel)
                        .child(switch),
                ),
        )
    }
}

/// A saved server as the pull-down names it.
fn server_words(host: &str, port: u16) -> SharedString {
    format!("{host} : {port}").into()
}

fn label(text: &'static str, theme: &Theme) -> Div {
    div()
        .pt_3()
        .pb_1()
        .text_size(px(12.))
        .text_color(theme.text_muted)
        .child(text)
}

/// What Edit doesn't change, said as a screen reader reads a label.
fn fixed(id: &'static str, text: SharedString, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .role(Role::Label)
        .aria_value(text.clone())
        .h(px(36.))
        .flex()
        .items_center()
        .text_size(px(14.))
        .text_color(theme.text)
        .child(text)
}
