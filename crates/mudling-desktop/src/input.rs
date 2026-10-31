//! The command input: text through the platform's input handler, so IME
//! composition (marked text) works as in any native field. Return sends,
//! Shift+Return starts a new line, and long lines wrap; the field grows with
//! them, up to [`MOST_ROWS`] rows, then scrolls to keep the caret in view.
//!
//! The platform talks in UTF-16 offsets (`NSTextInputClient` on macOS, TSF on
//! Windows, text-input on Wayland); the content is stored as UTF-8. Adapted
//! from gpui's `input` example, without mouse selection for now.
//!
//! While the server has echo off the content is a password: the field draws a
//! mask, tells the platform and accessibility tools only the mask, and can't
//! be copied. History, completion and hints stay away from it.

use crate::assets::Icon;
use crate::chat::{FONT_SIZE, LINE_HEIGHT, MONO_FONT};
use crate::theme::Theme;
use gpui::{
    App, AvailableSpace, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable,
    GlobalElementId, KeyBinding, LayoutId, PaintQuad, Pixels, Point, Role, SharedString, Size,
    Style, TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window, WrappedLine, actions, div,
    fill, point, prelude::*, px, relative, size,
};
use mudling_core::defaults::commands::parse_speedwalk;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

actions!(
    command_input,
    [
        Backspace, Delete, Left, Right, Up, Down, Home, End, Paste, Copy, Submit, Newline, Complete
    ]
);

const CONTEXT: &str = "CommandInput";
/// Drawn for each character of a password.
const MASK: &str = "•";
const PLACEHOLDER: &str = "Type a command — ↑ for history, Tab to complete";
/// The field stops growing here and scrolls instead.
const MOST_ROWS: usize = 8;
const SEND_BUTTON_SIZE: Pixels = px(36.);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some(CONTEXT)),
        KeyBinding::new("delete", Delete, Some(CONTEXT)),
        KeyBinding::new("left", Left, Some(CONTEXT)),
        KeyBinding::new("right", Right, Some(CONTEXT)),
        KeyBinding::new("up", Up, Some(CONTEXT)),
        KeyBinding::new("down", Down, Some(CONTEXT)),
        KeyBinding::new("home", Home, Some(CONTEXT)),
        KeyBinding::new("end", End, Some(CONTEXT)),
        KeyBinding::new(secondary!("v"), Paste, Some(CONTEXT)),
        KeyBinding::new(secondary!("c"), Copy, Some(CONTEXT)),
        KeyBinding::new("enter", Submit, Some(CONTEXT)),
        KeyBinding::new("shift-enter", Newline, Some(CONTEXT)),
        KeyBinding::new("tab", Complete, Some(CONTEXT)),
    ]);
}

/// What the field asks of whoever holds the command history: it knows only
/// its own text.
pub enum InputEvent {
    /// Return was pressed.
    Submitted(String),
    /// Up on the top row. `prefix` is the text before the caret.
    Older { line: String, prefix: String },
    /// Down on the bottom row.
    Newer { line: String },
    /// Tab, with the caret at byte offset `cursor`.
    Complete { line: String, cursor: usize },
}

/// The field's text as laid out, in the field's own coordinates: one entry for
/// each line the player broke, wrapped at the field's width.
#[derive(Default)]
struct InputLayout {
    text: SharedString,
    lines: Vec<WrappedLine>,
    line_height: Pixels,
}

impl InputLayout {
    fn shape(
        text: SharedString,
        runs: &[TextRun],
        font_size: Pixels,
        width: Option<Pixels>,
        line_height: Pixels,
        window: &Window,
    ) -> InputLayout {
        let lines = window
            .text_system()
            .shape_text(text.clone(), font_size, runs, width, None)
            .map(|lines| lines.into_vec())
            .unwrap_or_default();
        InputLayout {
            text,
            lines,
            line_height,
        }
    }

    fn rows(&self) -> usize {
        self.lines
            .iter()
            .map(|line| line.wrap_boundaries().len() + 1)
            .sum()
    }

    /// Where the caret before `offset` of the shown text sits.
    fn position_of(&self, offset: usize) -> Point<Pixels> {
        let mut start = 0;
        let mut top = px(0.);
        for line in &self.lines {
            if offset <= start + line.len() {
                let inside = line
                    .position_for_index(offset - start, self.line_height)
                    .unwrap_or_default();
                return point(inside.x, top + inside.y);
            }
            start += line.len() + 1;
            top += line.size(self.line_height).height;
        }
        point(px(0.), top)
    }

    /// The offset in the shown text closest to `position`.
    fn offset_at(&self, position: Point<Pixels>) -> usize {
        let mut start = 0;
        let mut top = px(0.);
        let last = self.lines.len().saturating_sub(1);
        for (number, line) in self.lines.iter().enumerate() {
            let bottom = top + line.size(self.line_height).height;
            if position.y < bottom || number == last {
                let inside = point(position.x, position.y - top);
                return start
                    + line
                        .closest_index_for_position(inside, self.line_height)
                        .unwrap_or_else(|closest| closest);
            }
            start += line.len() + 1;
            top = bottom;
        }
        start
    }
}

pub struct CommandInput {
    focus_handle: FocusHandle,
    content: String,
    masked: bool,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    /// The first row in view, once the field has stopped growing.
    first_row: usize,
    alias_preview: Option<SharedString>,
    last_layout: Option<InputLayout>,
    last_bounds: Option<Bounds<Pixels>>,
}

impl EventEmitter<InputEvent> for CommandInput {}

impl Focusable for CommandInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl CommandInput {
    pub fn new(cx: &mut Context<CommandInput>) -> CommandInput {
        CommandInput {
            focus_handle: cx.focus_handle().tab_stop(true),
            content: String::new(),
            masked: false,
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            first_row: 0,
            alias_preview: None,
            last_layout: None,
            last_bounds: None,
        }
    }

    /// Fills the alias preview slot: what the typed alias would send.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_alias_preview(&mut self, preview: Option<SharedString>, cx: &mut Context<Self>) {
        self.alias_preview = preview;
        cx.notify();
    }

    /// Replaces what's in the field: a recalled command or a completion, with
    /// the caret at byte offset `cursor`.
    pub fn show(
        &mut self,
        line: String,
        cursor: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.content = line;
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.marked_range = None;
        window.notify_text_input_changed(&self.focus_handle);
        cx.notify();
    }

    /// What the row shows beside the text: the alias preview, else the
    /// speedwalk the text would walk.
    fn hint(&self) -> Option<SharedString> {
        if self.masked {
            return None;
        }
        self.alias_preview.clone().or_else(|| {
            let typed = self.content.trim();
            let one_line = !typed.contains('\n');
            (one_line && parse_speedwalk(typed).is_some())
                .then(|| format!("speedwalk: {typed}").into())
        })
    }

    /// The layout, if it still describes the text: a key can land between
    /// frames.
    fn layout(&self) -> Option<&InputLayout> {
        let shown = self.shown();
        self.last_layout
            .as_ref()
            .filter(|layout| layout.text == shown)
    }

    /// The offset one row above or below the caret, or `None` past the ends.
    fn offset_rows_away(&self, rows: f32) -> Option<usize> {
        let layout = self.layout()?;
        let caret = layout.position_of(self.shown_offset(self.cursor_offset()));
        let row = caret.y / layout.line_height + rows;
        (row >= 0. && row < layout.rows() as f32).then(|| {
            let target = point(caret.x, layout.line_height * row);
            self.content_offset(layout.offset_at(target))
        })
    }

    /// The text to lay out, placeholder included, and its runs.
    fn styled_text(&self, window: &Window, cx: &App) -> (SharedString, Vec<TextRun>) {
        let theme = cx.global::<Theme>();
        let style = window.text_style();
        let (text, color) = if self.content.is_empty() {
            (SharedString::from(PLACEHOLDER), theme.placeholder)
        } else {
            (self.shown(), style.color)
        };
        let run = TextRun {
            len: text.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        // The composition is underlined, as native fields show it.
        let runs = match &self.marked_range {
            Some(marked) => {
                let marked = self.shown_offset(marked.start)..self.shown_offset(marked.end);
                [
                    TextRun {
                        len: marked.start,
                        ..run.clone()
                    },
                    TextRun {
                        len: marked.end - marked.start,
                        underline: Some(UnderlineStyle {
                            color: Some(color),
                            thickness: px(1.),
                            wavy: false,
                        }),
                        ..run.clone()
                    },
                    TextRun {
                        len: text.len() - marked.end,
                        ..run
                    },
                ]
                .into_iter()
                .filter(|run| run.len > 0)
                .collect()
            }
            None => vec![run],
        };
        (text, runs)
    }

    fn accepts_commands(&self) -> bool {
        !self.masked && self.marked_range.is_none()
    }

    fn up(&mut self, _: &Up, window: &mut Window, cx: &mut Context<Self>) {
        if !self.accepts_commands() {
            return;
        }
        match self.offset_rows_away(-1.) {
            Some(offset) => self.move_to(offset, window, cx),
            None => cx.emit(InputEvent::Older {
                line: self.content.clone(),
                prefix: self.content[..self.cursor_offset()].to_owned(),
            }),
        }
    }

    fn down(&mut self, _: &Down, window: &mut Window, cx: &mut Context<Self>) {
        if !self.accepts_commands() {
            return;
        }
        match self.offset_rows_away(1.) {
            Some(offset) => self.move_to(offset, window, cx),
            None => cx.emit(InputEvent::Newer {
                line: self.content.clone(),
            }),
        }
    }

    fn complete(&mut self, _: &Complete, _: &mut Window, cx: &mut Context<Self>) {
        if self.accepts_commands() {
            cx.emit(InputEvent::Complete {
                line: self.content.clone(),
                cursor: self.cursor_offset(),
            });
        }
    }

    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        if self.accepts_commands() {
            self.replace_text_in_range(None, "\n", window, cx);
        }
    }

    /// Follows the server's echo. What's in the field is thrown away when the
    /// mask changes either way: a password must never show, and a half-typed
    /// command must never go out as a password.
    pub fn set_masked(&mut self, masked: bool, window: &mut Window, cx: &mut Context<Self>) {
        if masked == self.masked {
            return;
        }
        self.masked = masked;
        self.content.clear();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        window.notify_text_input_changed(&self.focus_handle);
        cx.notify();
    }

    #[cfg(test)]
    pub fn is_masked(&self) -> bool {
        self.masked
    }

    #[cfg(test)]
    pub fn content(&self) -> &str {
        &self.content
    }

    fn shown(&self) -> SharedString {
        if self.masked {
            MASK.repeat(self.content.chars().count()).into()
        } else {
            self.content.clone().into()
        }
    }

    /// Where a content offset falls in [`Self::shown`].
    fn shown_offset(&self, offset: usize) -> usize {
        if self.masked {
            self.content[..offset].chars().count() * MASK.len()
        } else {
            offset
        }
    }

    /// Where an offset in [`Self::shown`] falls in the content.
    fn content_offset(&self, shown: usize) -> usize {
        if self.masked {
            self.content
                .char_indices()
                .nth(shown / MASK.len())
                .map_or(self.content.len(), |(index, _)| index)
        } else {
            shown.min(self.content.len())
        }
    }

    fn submit(&mut self, _: &Submit, window: &mut Window, cx: &mut Context<Self>) {
        // Return while composing confirms the composition; it doesn't send.
        if self.marked_range.is_some() {
            return;
        }
        let command = std::mem::take(&mut self.content);
        self.selected_range = 0..0;
        window.notify_text_input_changed(&self.focus_handle);
        cx.emit(InputEvent::Submitted(command));
        cx.notify();
    }

    fn left(&mut self, _: &Left, window: &mut Window, cx: &mut Context<Self>) {
        let target = if self.selected_range.is_empty() {
            self.previous_boundary(self.cursor_offset())
        } else {
            self.selected_range.start
        };
        self.move_to(target, window, cx);
    }

    fn right(&mut self, _: &Right, window: &mut Window, cx: &mut Context<Self>) {
        let target = if self.selected_range.is_empty() {
            self.next_boundary(self.selected_range.end)
        } else {
            self.selected_range.end
        };
        self.move_to(target, window, cx);
    }

    fn home(&mut self, _: &Home, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, window, cx);
    }

    fn end(&mut self, _: &End, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), window, cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.selected_range =
                self.previous_boundary(self.cursor_offset())..self.selected_range.end;
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.selected_range =
                self.selected_range.start..self.next_boundary(self.cursor_offset());
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let text = text.replace("\r\n", "\n");
            let text = if self.masked {
                text.replace('\n', " ")
            } else {
                text
            };
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() && !self.masked {
            let selected = self.content[self.selected_range.clone()].to_string();
            cx.write_to_clipboard(ClipboardItem::new_string(selected));
        }
    }

    fn move_to(&mut self, offset: usize, window: &Window, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    /// Whether End would leave the caret where it is.
    pub fn caret_at_end(&self) -> bool {
        self.cursor_offset() == self.content.len()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(index, _)| (index < offset).then_some(index))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(index, _)| (index > offset).then_some(index))
            .unwrap_or(self.content.len())
    }

    fn offset_from_utf16(&self, offset_utf16: usize) -> usize {
        let mut utf16 = 0;
        for (index, ch) in self.content.char_indices() {
            if utf16 >= offset_utf16 {
                return index;
            }
            utf16 += ch.len_utf16();
        }
        self.content.len()
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        self.content[..offset].encode_utf16().count()
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    /// The range an edit replaces: the one given, else the composition, else
    /// the selection.
    fn edit_range(&self, range_utf16: Option<Range<usize>>) -> Range<usize> {
        range_utf16
            .map(|range| self.range_from_utf16(&range))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone())
    }
}

impl EntityInputHandler for CommandInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        if self.masked {
            return None;
        }
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected_range = self.range_from_utf16(&range_utf16);
        self.selection_reversed = false;
        window.notify_text_selection_changed(&self.focus_handle);
        cx.notify();
    }

    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.content.encode_utf16().count())
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.marked_range = None;
        window.notify_text_input_changed(&self.focus_handle);
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.edit_range(range_utf16);
        self.content.replace_range(range.clone(), new_text);
        let cursor = range.start + new_text.len();
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.marked_range = None;
        window.notify_text_input_changed(&self.focus_handle);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.edit_range(range_utf16);
        self.content.replace_range(range.clone(), new_text);
        self.marked_range =
            (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        // The new selection is relative to the inserted text.
        let start_utf16 = self.offset_to_utf16(range.start);
        let inserted_utf16 = new_text.encode_utf16().count();
        self.selected_range = match new_selected_range_utf16 {
            Some(selected) => self.range_from_utf16(
                &(start_utf16 + selected.start.min(inserted_utf16)
                    ..start_utf16 + selected.end.min(inserted_utf16)),
            ),
            None => range.start + new_text.len()..range.start + new_text.len(),
        };
        self.selection_reversed = false;
        window.notify_text_input_changed(&self.focus_handle);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // With nothing typed yet there's no layout; the IME's candidate
        // window then sits at the start of the field.
        let Some(layout) = self.layout() else {
            return Some(Bounds::new(bounds.origin, size(px(0.), LINE_HEIGHT)));
        };
        let range = self.range_from_utf16(&range_utf16);
        let start = layout.position_of(self.shown_offset(range.start));
        let end = layout.position_of(self.shown_offset(range.end));
        let scrolled = point(px(0.), layout.line_height * self.first_row as f32);
        let origin = bounds.origin + start - scrolled;
        let width = if end.y == start.y {
            end.x - start.x
        } else {
            px(0.)
        };
        Some(Bounds::new(origin, size(width, layout.line_height)))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let local = self.last_bounds?.localize(&point)?;
        let layout = self.layout()?;
        let scrolled = gpui::point(px(0.), layout.line_height * self.first_row as f32);
        let index = layout.offset_at(local + scrolled);
        Some(self.offset_to_utf16(self.content_offset(index)))
    }
}

impl CommandInput {
    fn render_send_button(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("send")
            .role(Role::Button)
            .aria_label("Send")
            .flex_none()
            .size(SEND_BUTTON_SIZE)
            .flex()
            .items_center()
            .justify_center()
            .rounded_lg()
            .bg(theme.accent)
            .hover(|style| style.bg(theme.accent_hover))
            .on_click(cx.listener(|input, _, window, cx| {
                input.submit(&Submit, window, cx);
                window.focus(&input.focus_handle, cx);
            }))
            .child(Icon::Send.svg().size(px(16.)).text_color(theme.on_accent))
    }
}

impl Render for CommandInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let hint = self.hint().map(|hint| {
            div()
                .id("hint")
                .role(Role::Label)
                .aria_value(hint.clone())
                .flex_none()
                .max_w(relative(0.4))
                .h(SEND_BUTTON_SIZE)
                .flex()
                .items_center()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_size(px(12.))
                .text_color(theme.text_muted)
                .child(hint)
        });
        div()
            .id("command-row")
            .flex()
            .items_end()
            .gap_2()
            .pl_4()
            .pr_2()
            .py_2()
            .font_family(MONO_FONT)
            .text_size(FONT_SIZE)
            .line_height(LINE_HEIGHT)
            .text_color(theme.text)
            .on_click(cx.listener(|input, _, window, cx| window.focus(&input.focus_handle, cx)))
            .child(
                div()
                    .flex_none()
                    .self_start()
                    .h(SEND_BUTTON_SIZE)
                    .flex()
                    .items_center()
                    .text_color(theme.accent)
                    .child("›"),
            )
            .child(
                div()
                    .id("command-input")
                    .role(if self.masked {
                        Role::PasswordInput
                    } else {
                        Role::TextInput
                    })
                    .aria_label("Command")
                    .aria_value(self.shown())
                    .aria_placeholder(PLACEHOLDER)
                    .key_context(CONTEXT)
                    .track_focus(&self.focus_handle)
                    .tab_stop(true)
                    .cursor(CursorStyle::IBeam)
                    .on_action(cx.listener(Self::submit))
                    .on_action(cx.listener(Self::newline))
                    .on_action(cx.listener(Self::complete))
                    .on_action(cx.listener(Self::backspace))
                    .on_action(cx.listener(Self::delete))
                    .on_action(cx.listener(Self::left))
                    .on_action(cx.listener(Self::right))
                    .on_action(cx.listener(Self::up))
                    .on_action(cx.listener(Self::down))
                    .on_action(cx.listener(Self::home))
                    .on_action(cx.listener(Self::end))
                    .on_action(cx.listener(Self::paste))
                    .on_action(cx.listener(Self::copy))
                    .flex_1()
                    .min_w_0()
                    .py(px(8.))
                    .child(InputText { input: cx.entity() }),
            )
            .children(hint)
            .child(self.render_send_button(&theme, cx))
    }
}

/// Draws the input's text, and registers it as the window's text input target
/// while it has focus.
struct InputText {
    input: Entity<CommandInput>,
}

struct InputTextPrepaint {
    layout: InputLayout,
    origin: Point<Pixels>,
    cursor: Option<PaintQuad>,
    selection: Vec<PaintQuad>,
}

/// One rectangle for each row a selection touches.
fn selection_rows(
    start: Point<Pixels>,
    end: Point<Pixels>,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
) -> Vec<Bounds<Pixels>> {
    let row = |top: Pixels, left: Pixels, right: Pixels| {
        Bounds::from_corners(point(left, top), point(right, top + line_height))
    };
    if start.y == end.y {
        return vec![row(start.y, start.x, end.x)];
    }
    let mut rows = vec![row(start.y, start.x, bounds.right())];
    let mut top = start.y + line_height;
    while top < end.y {
        rows.push(row(top, bounds.left(), bounds.right()));
        top += line_height;
    }
    rows.push(row(end.y, bounds.left(), end.x));
    rows
}

impl IntoElement for InputText {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for InputText {
    type RequestLayoutState = ();
    type PrepaintState = InputTextPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let (text, runs) = self.input.read(cx).styled_text(window, cx);
        let font_size = window.text_style().font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let measure = move |known: Size<Option<Pixels>>,
                            available: Size<AvailableSpace>,
                            window: &mut Window,
                            _: &mut App| {
            let width = known.width.or(match available.width {
                AvailableSpace::Definite(width) => Some(width),
                _ => None,
            });
            let layout =
                InputLayout::shape(text.clone(), &runs, font_size, width, line_height, window);
            let rows = layout.rows().clamp(1, MOST_ROWS);
            size(width.unwrap_or_default(), line_height * rows as f32)
        };
        (window.request_measured_layout(style, measure), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> InputTextPrepaint {
        let (text, runs) = self.input.read(cx).styled_text(window, cx);
        let font_size = window.text_style().font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let layout = InputLayout::shape(
            text,
            &runs,
            font_size,
            Some(bounds.size.width),
            line_height,
            window,
        );

        let input = self.input.read(cx);
        let caret = layout.position_of(input.shown_offset(input.cursor_offset()));
        let first_row = row_in_view(
            input.first_row,
            (caret.y / line_height) as usize,
            layout.rows(),
        );
        self.input
            .update(cx, |input, _| input.first_row = first_row);

        let input = self.input.read(cx);
        let theme = cx.global::<Theme>();
        let origin = bounds.origin - point(px(0.), line_height * first_row as f32);
        let (cursor, selection) = if input.selected_range.is_empty() {
            let caret = Bounds::new(origin + caret, size(px(2.), line_height));
            (Some(fill(caret, theme.accent)), Vec::new())
        } else {
            let start = layout.position_of(input.shown_offset(input.selected_range.start));
            let end = layout.position_of(input.shown_offset(input.selected_range.end));
            let field = Bounds::new(point(px(0.), px(0.)), bounds.size);
            let rows = selection_rows(start, end, field, line_height)
                .into_iter()
                .map(|row| fill(row + origin, theme.accent.opacity(0.3)))
                .collect();
            (None, rows)
        };
        InputTextPrepaint {
            layout,
            origin,
            cursor,
            selection,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut InputTextPrepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        let line_height = prepaint.layout.line_height;
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for row in prepaint.selection.drain(..) {
                window.paint_quad(row);
            }
            let mut top = prepaint.origin.y;
            for line in &prepaint.layout.lines {
                let origin = point(prepaint.origin.x, top);
                line.paint(origin, line_height, TextAlign::Left, None, window, cx)
                    .ok();
                top += line.size(line_height).height;
            }
            if focus_handle.is_focused(window)
                && let Some(cursor) = prepaint.cursor.take()
            {
                window.paint_quad(cursor);
            }
        });
        let has_content = !self.input.read(cx).content.is_empty();
        let layout = has_content.then(|| std::mem::take(&mut prepaint.layout));
        self.input.update(cx, |input, _| {
            // The placeholder's layout doesn't describe the content.
            input.last_layout = layout;
            input.last_bounds = Some(bounds);
        });
    }
}

/// The first row to show, moved as little as it takes to keep `caret_row` in
/// view.
fn row_in_view(first_row: usize, caret_row: usize, rows: usize) -> usize {
    first_row
        .min(caret_row)
        .max((caret_row + 1).saturating_sub(MOST_ROWS))
        .min(rows.saturating_sub(MOST_ROWS))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext, WindowAppearance};

    fn input_window(cx: &mut TestAppContext) -> (Entity<CommandInput>, &mut VisualTestContext) {
        cx.update(|cx| cx.set_global(Theme::for_appearance(WindowAppearance::Dark)));
        cx.add_window_view(|_, cx| CommandInput::new(cx))
    }

    // These drive the same EntityInputHandler calls the platform makes for an
    // IME: on macOS, setMarkedText and insertText from NSTextInputClient.
    #[gpui::test]
    fn japanese_composition_marks_then_commits(cx: &mut TestAppContext) {
        let (input, cx) = input_window(cx);
        input.update_in(cx, |input, window, cx| {
            input.replace_text_in_range(None, "go ", window, cx);
            input.replace_and_mark_text_in_range(None, "k", Some(1..1), window, cx);
            input.replace_and_mark_text_in_range(None, "き", Some(1..1), window, cx);
            assert_eq!(input.content, "go き");
            assert_eq!(input.marked_text_range(window, cx), Some(3..4));

            input.replace_text_in_range(None, "北", window, cx);
            assert_eq!(input.content, "go 北");
            assert_eq!(input.marked_text_range(window, cx), None);
            assert_eq!(
                input.selected_text_range(false, window, cx).unwrap().range,
                4..4
            );
        });
    }

    #[gpui::test]
    fn return_while_composing_confirms_instead_of_sending(cx: &mut TestAppContext) {
        let (input, cx) = input_window(cx);
        input.update_in(cx, |input, window, cx| {
            input.replace_and_mark_text_in_range(None, "にほん", None, window, cx);
            input.submit(&Submit, window, cx);
            assert_eq!(input.content, "にほん");
        });
    }

    #[gpui::test]
    fn a_password_stays_off_the_screen_and_away_from_the_platform(cx: &mut TestAppContext) {
        let (input, cx) = input_window(cx);
        input.update_in(cx, |input, window, cx| {
            input.set_masked(true, window, cx);
            input.replace_text_in_range(None, "pä🙂", window, cx);
            assert_eq!(input.shown(), MASK.repeat(3));
            assert_eq!(input.shown_offset(input.content.len()), 3 * MASK.len());
            assert_eq!(input.content_offset(2 * MASK.len()), "pä".len());
            assert_eq!(input.text_for_range(0..1, &mut None, window, cx), None);

            input.selected_range = 0..input.content.len();
            input.copy(&Copy, window, cx);
        });
        assert_eq!(cx.read_from_clipboard(), None);
    }

    #[gpui::test]
    fn the_field_empties_when_the_mask_changes(cx: &mut TestAppContext) {
        let (input, cx) = input_window(cx);
        input.update_in(cx, |input, window, cx| {
            input.replace_text_in_range(None, "half a comma", window, cx);
            input.set_masked(true, window, cx);
            assert_eq!(input.content, "");

            input.replace_text_in_range(None, "secret", window, cx);
            input.set_masked(false, window, cx);
            assert_eq!(input.content, "");
            assert_eq!(input.selected_range, 0..0);
        });
    }

    #[gpui::test]
    fn the_hint_names_a_speedwalk_or_the_alias_preview(cx: &mut TestAppContext) {
        let (input, cx) = input_window(cx);
        input.update_in(cx, |input, window, cx| {
            input.replace_text_in_range(None, "3n2e", window, cx);
            assert_eq!(input.hint().as_deref(), Some("speedwalk: 3n2e"));

            input.set_alias_preview(Some("get all corpse".into()), cx);
            assert_eq!(input.hint().as_deref(), Some("get all corpse"));
            input.set_alias_preview(None, cx);

            input.show("look".to_owned(), 4, window, cx);
            assert_eq!(input.hint(), None);
        });
    }

    #[gpui::test]
    fn a_password_never_gets_a_hint(cx: &mut TestAppContext) {
        let (input, cx) = input_window(cx);
        input.update_in(cx, |input, window, cx| {
            input.set_masked(true, window, cx);
            input.replace_text_in_range(None, "3n2e", window, cx);
            assert_eq!(input.hint(), None);
        });
    }

    #[gpui::test]
    fn pasted_lines_stay_lines_except_in_a_password(cx: &mut TestAppContext) {
        let (input, cx) = input_window(cx);
        cx.write_to_clipboard(ClipboardItem::new_string("look\r\ninv".to_owned()));
        input.update_in(cx, |input, window, cx| {
            input.paste(&Paste, window, cx);
            assert_eq!(input.content, "look\ninv");
            input.set_masked(true, window, cx);
            input.paste(&Paste, window, cx);
            assert_eq!(input.content, "look inv");
        });
    }

    #[gpui::test]
    fn up_moves_between_rows_before_it_recalls_history(cx: &mut TestAppContext) {
        let (input, cx) = input_window(cx);
        let recalled = std::rc::Rc::new(std::cell::Cell::new(0));
        let _counting = cx.update(|_, cx| {
            let recalled = recalled.clone();
            cx.subscribe(&input, move |_, event: &InputEvent, _| {
                if matches!(event, InputEvent::Older { .. }) {
                    recalled.set(recalled.get() + 1);
                }
            })
        });
        input.update_in(cx, |input, window, cx| {
            input.replace_text_in_range(None, "look\ninv", window, cx);
        });
        cx.run_until_parked();

        input.update_in(cx, |input, window, cx| input.up(&Up, window, cx));
        assert_eq!(
            input.read_with(cx, |input, _| input.cursor_offset()),
            3,
            "the same column on the row above"
        );
        assert_eq!(recalled.get(), 0);

        input.update_in(cx, |input, window, cx| input.up(&Up, window, cx));
        assert_eq!(recalled.get(), 1);
    }

    #[test]
    fn the_view_follows_the_caret_past_the_rows_the_field_shows() {
        assert_eq!(row_in_view(0, 3, 4), 0, "everything fits");
        assert_eq!(row_in_view(0, 9, 10), 2, "down to the caret");
        assert_eq!(row_in_view(2, 0, 10), 0, "back up to the caret");
        assert_eq!(row_in_view(5, 6, 9), 1, "no empty rows below the text");
    }

    #[gpui::test]
    fn platform_offsets_are_utf16(cx: &mut TestAppContext) {
        let (input, cx) = input_window(cx);
        input.update_in(cx, |input, window, cx| {
            input.replace_text_in_range(None, "a🙂b", window, cx);
            assert_eq!(input.text_length_utf16(window, cx), Some(4));
            input.set_selected_text_range(1..3, window, cx);
            assert_eq!(input.selected_range, 1..5);
            let mut actual = None;
            assert_eq!(
                input
                    .text_for_range(3..4, &mut actual, window, cx)
                    .as_deref(),
                Some("b")
            );
        });
    }
}
