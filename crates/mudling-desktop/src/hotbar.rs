//! The hotbar: chips above the command bar, each a command one press away.
//!
//! The keyboard gets here with its own shortcut. The arrows move between
//! chips, Shift and an arrow moves the chip itself, E edits it and Delete
//! removes it. The mouse drags chips to reorder them, and right-clicks to edit.
//! While the server asks for a password no chip does anything: what it sent
//! would go as the password.

use crate::assets::{Icon, UI_FONT};
use crate::input::{CommandInput, InputEvent};
use crate::shell::{FocusCommandBar, FocusNext};
use crate::theme::Theme;
use gpui::{
    AnyElement, App, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, KeyBinding,
    MouseButton, Pixels, Render, Role, SharedString, Stateful, Window, actions, div, prelude::*,
    px,
};
use mudling_core::defaults::hotbar;

const CONTEXT: &str = "Hotbar";
const EDITOR_CONTEXT: &str = "HotbarEditor";
const CHIP_HEIGHT: Pixels = px(30.);
const EDITOR_WIDTH: Pixels = px(220.);
const LOCKED_NOTE: &str = "Not while the server asks for a password.";
const CHIP_KEYS: &str = "Shift and an arrow moves it, E edits it, Delete removes it.";

actions!(
    hotbar,
    [
        PreviousChip,
        NextChip,
        MoveChipLeft,
        MoveChipRight,
        EditChip,
        RemoveChip,
        CancelEdit
    ]
);

pub fn bind_keys(cx: &mut App) {
    let bound_here = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("left", PreviousChip, bound_here),
        KeyBinding::new("right", NextChip, bound_here),
        KeyBinding::new("shift-left", MoveChipLeft, bound_here),
        KeyBinding::new("shift-right", MoveChipRight, bound_here),
        KeyBinding::new("e", EditChip, bound_here),
        KeyBinding::new("delete", RemoveChip, bound_here),
        KeyBinding::new("backspace", RemoveChip, bound_here),
        KeyBinding::new("tab", FocusNext, bound_here),
        KeyBinding::new("escape", FocusCommandBar, bound_here),
        KeyBinding::new("escape", CancelEdit, Some(EDITOR_CONTEXT)),
    ]);
}

/// What the hotbar asks of whoever runs the session.
pub enum HotbarEvent {
    Pressed {
        command: String,
        by_keyboard: bool,
    },
    Changed(Vec<String>),
    /// Done here: the keyboard goes back to the command bar.
    Left,
}

/// A chip on its way to a new place, by the mouse.
#[derive(Clone)]
struct DraggedChip {
    from: usize,
    command: SharedString,
}

impl Render for DraggedChip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<DraggedChip>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        chip_shape(&theme).child(self.command.clone())
    }
}

fn chip_shape(theme: &Theme) -> Div {
    div()
        .flex_none()
        .h(CHIP_HEIGHT)
        .px_3()
        .flex()
        .items_center()
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .bg(theme.surface)
        .font_family(UI_FONT)
        .text_size(px(14.))
        .text_color(theme.text_muted)
}

/// What the editor in the row is changing.
#[derive(Clone, Copy, PartialEq)]
enum Editing {
    Chip(usize),
    New,
}

pub struct Hotbar {
    chips: Vec<String>,
    /// The server asks for a password.
    locked: bool,
    /// One for each chip, kept between frames so a chip keeps its focus.
    handles: Vec<FocusHandle>,
    add: FocusHandle,
    editing: Option<Editing>,
    editor: Entity<CommandInput>,
}

impl EventEmitter<HotbarEvent> for Hotbar {}

impl Hotbar {
    pub fn new(cx: &mut Context<Hotbar>) -> Hotbar {
        let editor = cx.new(|cx| CommandInput::field("Hotbar command", "A command", cx));
        cx.subscribe(&editor, |hotbar, _, event, cx| {
            if let InputEvent::Submitted(command) = event {
                hotbar.finish_editing(command, cx);
            }
        })
        .detach();
        Hotbar {
            chips: Vec::new(),
            locked: false,
            handles: Vec::new(),
            add: cx.focus_handle(),
            editing: None,
            editor,
        }
    }

    pub fn show(&mut self, chips: &[String], cx: &mut Context<Hotbar>) {
        if self.chips != chips {
            self.chips = chips.to_vec();
            // An edit in progress points at a chip that may have moved.
            self.editing = None;
            self.keep_handles(cx);
            cx.notify();
        }
    }

    pub fn set_locked(&mut self, locked: bool, cx: &mut Context<Hotbar>) {
        if self.locked != locked {
            self.locked = locked;
            cx.notify();
        }
    }

    pub fn chips(&self) -> &[String] {
        &self.chips
    }

    fn keep_handles(&mut self, cx: &mut Context<Hotbar>) {
        while self.handles.len() < self.chips.len() {
            self.handles.push(cx.focus_handle());
        }
    }

    /// Takes the keyboard: the first chip, or the add button with none.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Hotbar>) {
        match self.handles.first().filter(|_| !self.chips.is_empty()) {
            Some(first) => window.focus(first, cx),
            None => window.focus(&self.add, cx),
        }
    }

    /// The stops, chips then the add button, and which of them has focus.
    fn focused_stop(&self, window: &Window) -> Option<usize> {
        let in_use = &self.handles[..self.chips.len()];
        let chip = in_use.iter().position(|chip| chip.is_focused(window));
        chip.or(self.add.is_focused(window).then_some(self.chips.len()))
    }

    fn step(&mut self, by: isize, window: &mut Window, cx: &mut Context<Hotbar>) {
        let Some(at) = self.focused_stop(window) else {
            return;
        };
        let stops = self.chips.len() as isize + 1;
        let next = (at as isize + by).rem_euclid(stops) as usize;
        match self.handles[..self.chips.len()].get(next) {
            Some(chip) => window.focus(chip, cx),
            None => window.focus(&self.add, cx),
        }
    }

    /// The chip the keyboard is on. None during an edit, which goes by a
    /// chip's place: Tab can leave the editor, and the place must hold.
    fn focused_chip(&self, window: &Window) -> Option<usize> {
        self.focused_stop(window)
            .filter(|&at| self.editing.is_none() && at < self.chips.len())
    }

    fn move_focused(&mut self, by: isize, window: &mut Window, cx: &mut Context<Hotbar>) {
        let Some(from) = self.focused_chip(window) else {
            return;
        };
        let Some(to) = from
            .checked_add_signed(by)
            .filter(|&to| to < self.chips.len())
        else {
            return;
        };
        self.move_chip(from, to, cx);
        window.focus(&self.handles[to], cx);
    }

    fn move_chip(&mut self, from: usize, to: usize, cx: &mut Context<Hotbar>) {
        let chip = self.chips.remove(from);
        self.chips.insert(to, chip);
        self.changed(cx);
    }

    fn changed(&mut self, cx: &mut Context<Hotbar>) {
        cx.emit(HotbarEvent::Changed(self.chips.clone()));
        cx.notify();
    }

    fn remove_focused(&mut self, window: &mut Window, cx: &mut Context<Hotbar>) {
        let Some(at) = self.focused_chip(window) else {
            return;
        };
        self.chips.remove(at);
        self.changed(cx);
        match self.handles[..self.chips.len()].get(at) {
            Some(next) => window.focus(next, cx),
            None => window.focus(&self.add, cx),
        }
    }

    fn start_editing(&mut self, editing: Editing, window: &mut Window, cx: &mut Context<Hotbar>) {
        let text = match editing {
            Editing::Chip(at) => self.chips[at].clone(),
            Editing::New => String::new(),
        };
        self.editing = Some(editing);
        self.editor.update(cx, |editor, cx| {
            let end = text.len();
            editor.show(text, end, window, cx);
        });
        window.focus(&self.editor.focus_handle(cx), cx);
        cx.notify();
    }

    /// Keeps what was typed. A chip emptied goes.
    fn finish_editing(&mut self, typed: &str, cx: &mut Context<Hotbar>) {
        let Some(editing) = self.editing.take() else {
            return;
        };
        match editing {
            Editing::Chip(at) if typed.trim().is_empty() => {
                self.chips.remove(at);
            }
            Editing::Chip(at) => typed.trim().clone_into(&mut self.chips[at]),
            Editing::New => {
                hotbar::pin(&mut self.chips, typed);
            }
        }
        self.keep_handles(cx);
        self.changed(cx);
        cx.emit(HotbarEvent::Left);
    }

    fn cancel(&mut self, cx: &mut Context<Hotbar>) {
        self.editing = None;
        cx.emit(HotbarEvent::Left);
        cx.notify();
    }

    fn press(&mut self, at: usize, by_keyboard: bool, cx: &mut Context<Hotbar>) {
        if !self.locked {
            cx.emit(HotbarEvent::Pressed {
                command: self.chips[at].clone(),
                by_keyboard,
            });
        }
    }

    fn render_chip(&self, at: usize, theme: &Theme, cx: &mut Context<Hotbar>) -> Stateful<Div> {
        let command: SharedString = self.chips[at].clone().into();
        let (ring, hover) = (theme.accent, theme.hover);
        let dragged = DraggedChip {
            from: at,
            command: command.clone(),
        };
        chip_shape(theme)
            .id(("hotbar-chip", at))
            .debug_selector(move || format!("hotbar-chip-{at}"))
            .key_context(CONTEXT)
            .role(Role::Button)
            .aria_label(command.clone())
            .aria_description(if self.locked { LOCKED_NOTE } else { CHIP_KEYS })
            .aria_position_in_set(at + 1)
            .aria_size_of_set(self.chips.len())
            .track_focus(&self.handles[at])
            .tab_stop(true)
            .when(self.locked, |chip| chip.opacity(0.5))
            .when(!self.locked, |chip| {
                chip.hover(move |style| style.bg(hover))
            })
            .focus_visible(move |style| style.border_color(ring))
            // The mouse never moves focus here: typing stays with the bar.
            .capture_any_mouse_down(|_, window, _| window.prevent_default())
            .on_click(cx.listener(move |hotbar, event: &gpui::ClickEvent, _, cx| {
                hotbar.press(at, event.is_keyboard(), cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |hotbar, _, window, cx| {
                    hotbar.start_editing(Editing::Chip(at), window, cx);
                }),
            )
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .drag_over::<DraggedChip>(move |style, _, _, _| style.border_color(ring))
            .on_drop(cx.listener(move |hotbar, dragged: &DraggedChip, _, cx| {
                if dragged.from != at && hotbar.editing.is_none() {
                    hotbar.move_chip(dragged.from, at, cx);
                }
            }))
            .child(command)
    }

    /// In a key context of its own: a letter typed here isn't a chip's key.
    fn render_editor(&self) -> AnyElement {
        div()
            .key_context(EDITOR_CONTEXT)
            .flex_none()
            .w(EDITOR_WIDTH)
            .child(self.editor.clone())
            .into_any_element()
    }

    fn render_add(&self, theme: &Theme, cx: &mut Context<Hotbar>) -> AnyElement {
        let (ring, hover) = (theme.accent, theme.hover);
        div()
            .id("hotbar-add")
            .debug_selector(|| "hotbar-add".to_owned())
            .key_context(CONTEXT)
            .role(Role::Button)
            .aria_label("Add a chip")
            .track_focus(&self.add)
            .tab_stop(true)
            .flex_none()
            .size(CHIP_HEIGHT)
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .border_1()
            .border_color(gpui::transparent_black())
            .hover(move |style| style.bg(hover))
            .focus_visible(move |style| style.border_color(ring))
            .on_click(cx.listener(|hotbar, _, window, cx| {
                hotbar.start_editing(Editing::New, window, cx);
            }))
            .child(Icon::Plus.svg().size(px(14.)).text_color(theme.text_muted))
            .into_any_element()
    }
}

impl Focusable for Hotbar {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.add.clone()
    }
}

impl Render for Hotbar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Hotbar>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let mut row: Vec<AnyElement> = (0..self.chips.len())
            .map(|at| match self.editing {
                Some(Editing::Chip(editing)) if editing == at => self.render_editor(),
                _ => self.render_chip(at, &theme, cx).into_any_element(),
            })
            .collect();
        row.push(match self.editing {
            Some(Editing::New) => self.render_editor(),
            _ => self.render_add(&theme, cx),
        });
        div()
            .id("hotbar")
            .debug_selector(|| "hotbar".to_owned())
            .role(Role::Toolbar)
            .aria_label("Hotbar")
            .w_full()
            .pb_2()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .on_action(cx.listener(|hotbar, _: &PreviousChip, window, cx| {
                hotbar.step(-1, window, cx);
            }))
            .on_action(cx.listener(|hotbar, _: &NextChip, window, cx| {
                hotbar.step(1, window, cx);
            }))
            .on_action(cx.listener(|hotbar, _: &MoveChipLeft, window, cx| {
                hotbar.move_focused(-1, window, cx);
            }))
            .on_action(cx.listener(|hotbar, _: &MoveChipRight, window, cx| {
                hotbar.move_focused(1, window, cx);
            }))
            .on_action(cx.listener(|hotbar, _: &EditChip, window, cx| {
                if let Some(at) = hotbar.focused_chip(window) {
                    hotbar.start_editing(Editing::Chip(at), window, cx);
                }
            }))
            .on_action(cx.listener(|hotbar, _: &RemoveChip, window, cx| {
                hotbar.remove_focused(window, cx);
            }))
            .on_action(cx.listener(|hotbar, _: &CancelEdit, _, cx| hotbar.cancel(cx)))
            .children(row)
    }
}
